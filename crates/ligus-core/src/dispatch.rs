//! The device-keyed dispatcher (architecture-review 4.9; repos review F8;
//! design: `docs/design/v2/repos-and-checkouts.md` section 11).
//!
//! **Invariant.** Each turn (a claim key: a user message, or a hand-off's
//! `(trigger, receiver)` key, [`crate::session::turn_claim_key`]) has at most
//! one *committed* answer in every device's fold, whatever arrives in what
//! order. Exactly-once execution isn't achievable with devices that go
//! offline mid-turn; what holds is that a second execution never commits.
//!
//! Two pure halves:
//!
//! * **The fold** ([`fold_claim`], [`note_answer`], [`note_removed`]): run
//!   in canonical `(lamport, event_id)` order by `events::fold_pass`, so the
//!   result is a function of the event set. Per key a [`TurnSlot`] holds the
//!   holder (also mirrored in `ChatSession::turn_claims`, which every older
//!   reader uses), its epoch, whether it covers the chat's repo, and the
//!   committed answer.
//! * **The decision** ([`decide`]): what one device does next, from its own
//!   fold plus device-local facts (eligibility from `turn_runs_here`,
//!   coverage from the host's own checkout check, how long it waited, the
//!   holder's presence and the current roster).
//!
//! Signed coverage: a claim's `repo_id` is the claiming device's own
//! statement that it has a confirmed checkout of the chat's repo. The claim
//! is signed by the device it names (`authz::lease_signer_ok`), so nobody
//! can attest for another host. Coverage on the presence beat is unsigned
//! and advisory and never decides anything here.
//!
//! Revoked devices and removed members need nothing special in the fold:
//! their events that a revocation's or removal's watermark doesn't cover
//! never fold (`watermark`, `lamport`), so a backdated claim or answer never
//! becomes holder or commits. A claim made before a revocation stays folded;
//! [`decide`] treats its holder as gone ([`holder_gone`]) and hands the turn
//! over at once.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::chat::{ChatMessage, MessageRole};
use crate::session::{turn_claim_key, ChatSession};

/// How long a claimed turn may go without an answer before an eligible
/// device hands it over (when the holder isn't live). Also the desktop's
/// deferred-claim timeout: long enough that a healthy peer's claim syncs
/// first, short enough that a chat isn't left dead.
pub const CLAIM_TIMEOUT: Duration = Duration::from_secs(25);
/// How long a holder that beats on the presence channel may still go
/// without committing an answer before an eligible device hands the turn
/// over anyway (a wedged holder, or a member's device that claims turns and
/// never answers). A holder commits when it writes its placeholder, at the
/// start of its run.
pub const HOLDER_COMMIT_TIMEOUT: Duration = Duration::from_secs(120);
/// The clock skew the fold tolerates between a holder's claim and a
/// hand-over: a hand-over signed less than `CLAIM_TIMEOUT - HANDOVER_SKEW`
/// after the claim it displaces (by the envelopes' signed timestamps) is
/// ignored, unless the holder is gone at the hand-over's position.
pub const HANDOVER_SKEW: Duration = Duration::from_secs(10);
/// How long a device that doesn't cover a repo chat's repo waits before it
/// claims, so a covering host claims first.
pub const UNCOVERED_BACKOFF: Duration = Duration::from_secs(8);
/// No hand-over of a turn whose trigger is older than this: a device that
/// syncs a backlog doesn't answer yesterday's questions.
pub const TAKEOVER_MAX_AGE: Duration = Duration::from_secs(15 * 60);
/// At most this many claim generations per turn (a hand-over from a holder
/// that is gone at its position may go past it).
pub const MAX_TURN_EPOCH: u32 = 64;
/// A device holds one turn at most this many times (no hand-over to itself,
/// and no ping-pong between a member's own devices up to the epoch cap).
pub const MAX_HOLDS_PER_DEVICE: usize = 2;

/// A device that held a turn and was handed over from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Displaced {
    pub device: Uuid,
    /// The signed time (unix ms) of the hand-over that displaced it (0:
    /// unknown). An answer it signed before then still commits.
    #[serde(default)]
    pub until: i64,
}

/// One turn's dispatch state in a fold.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSlot {
    /// The device that holds the turn (`None`: answered without a claim).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<Uuid>,
    /// The holder's claim generation (1 for a first claim).
    #[serde(default)]
    pub epoch: u32,
    /// The holder's claim named the chat's repo (signed coverage).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub covers: bool,
    /// The committed answer (a message id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Uuid>,
    /// The device whose answer committed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_by: Option<Uuid>,
    /// When the holder's claim was signed (unix ms; 0: unknown).
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub since: i64,
    /// The devices handed over from, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub before: Vec<Displaced>,
}

fn is_zero_i64(n: &i64) -> bool {
    *n == 0
}

impl TurnSlot {
    /// Where `device` stands in the turn's chain of holders (the oldest
    /// first; the current holder last), its latest position when it held
    /// more than once.
    fn position(&self, device: Uuid) -> Option<usize> {
        if self.holder == Some(device) {
            return Some(self.before.len());
        }
        self.before.iter().rposition(|d| d.device == device)
    }

    /// How many times `device` held the turn (the current hold included).
    pub fn holds_of(&self, device: Uuid) -> usize {
        self.before.iter().filter(|d| d.device == device).count()
            + usize::from(self.holder == Some(device))
    }
}

/// What the fold did with a claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// The first claim of the turn.
    Held,
    /// The same holder again (idempotent; may add its coverage).
    Again,
    /// A covering claim displaced a non-covering holder at the same epoch.
    Preempted,
    /// A hand-over to the next epoch.
    HandedOver,
    /// The turn is answered already.
    Answered,
    /// Another device holds it (same epoch, no displacement).
    Lost,
    /// A stale or skipped epoch, a hand-over from someone who isn't the
    /// holder, too early or to a device that held the turn enough, or a
    /// hand-over whose predecessor isn't folded (yet).
    Stale,
}

/// A claim's optional fields as the fold reads them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Claim {
    pub device: Uuid,
    /// 0 or 1: a first claim.
    pub epoch: u32,
    pub supersedes: Option<Uuid>,
    pub repo_id: Option<Uuid>,
}

impl Claim {
    pub fn first(device: Uuid) -> Self {
        Self {
            device,
            epoch: 1,
            supersedes: None,
            repo_id: None,
        }
    }

    fn epoch(&self) -> u32 {
        self.epoch.max(1)
    }
}

/// What the checked fold knows about a claim beyond its payload.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClaimAt {
    /// The claim's signed timestamp (unix ms; `None`: an unplaced `apply`,
    /// which skips the hand-over time check).
    pub ms: Option<i64>,
    /// The current holder is revoked or no longer a member's at the claim's
    /// position ([`holder_gone`] over the workspace roster there).
    pub holder_gone: bool,
}

/// The chat's repo at this point of the fold.
fn chat_repo(session: &ChatSession) -> Option<Uuid> {
    session.repo.as_ref().and_then(|r| r.repo_id)
}

/// Fold one `TurnClaimed` (already authorized: signed by the device it
/// names) for `key`, with no placement facts (see [`fold_claim_at`]).
pub fn fold_claim(session: &mut ChatSession, key: Uuid, claim: Claim) -> ClaimOutcome {
    fold_claim_at(session, key, claim, ClaimAt::default())
}

/// Fold one `TurnClaimed` for `key`. Deterministic: reads only the fold so
/// far and the claim's own signed facts.
pub fn fold_claim_at(
    session: &mut ChatSession,
    key: Uuid,
    claim: Claim,
    at: ClaimAt,
) -> ClaimOutcome {
    let covers = claim.repo_id.is_some() && claim.repo_id == chat_repo(session);
    let epoch = claim.epoch();
    let ms = at.ms.unwrap_or(0);
    let outcome = match session.turn_dispatch.get_mut(&key) {
        Some(slot) if slot.answer.is_some() => ClaimOutcome::Answered,
        Some(slot) => {
            let holder = slot.holder;
            if epoch == slot.epoch {
                if holder == Some(claim.device) {
                    slot.covers |= covers;
                    ClaimOutcome::Again
                } else if covers
                    && !slot.covers
                    && claim.supersedes.is_none_or(|s| Some(s) == holder)
                {
                    slot.holder = Some(claim.device);
                    slot.covers = true;
                    slot.since = ms;
                    ClaimOutcome::Preempted
                } else {
                    ClaimOutcome::Lost
                }
            } else if epoch == slot.epoch + 1
                && (epoch <= MAX_TURN_EPOCH || at.holder_gone)
                && claim.supersedes.is_some()
                && claim.supersedes == holder
                && holder != Some(claim.device)
                && slot.holds_of(claim.device) < MAX_HOLDS_PER_DEVICE
                && handover_not_early(slot, at)
            {
                if let Some(h) = holder {
                    slot.before.push(Displaced {
                        device: h,
                        until: ms,
                    });
                }
                slot.holder = Some(claim.device);
                slot.epoch = epoch;
                slot.covers = covers;
                slot.since = ms;
                ClaimOutcome::HandedOver
            } else {
                ClaimOutcome::Stale
            }
        }
        None if epoch == 1 && claim.supersedes.is_none() => {
            session.turn_dispatch.insert(
                key,
                TurnSlot {
                    holder: Some(claim.device),
                    epoch: 1,
                    covers,
                    since: ms,
                    ..Default::default()
                },
            );
            ClaimOutcome::Held
        }
        None => ClaimOutcome::Stale,
    };
    if matches!(
        outcome,
        ClaimOutcome::Held | ClaimOutcome::Preempted | ClaimOutcome::HandedOver
    ) {
        session.turn_claims.insert(key, claim.device);
    }
    outcome
}

/// A hand-over from a live-looking holder is signed at least
/// `CLAIM_TIMEOUT - HANDOVER_SKEW` after that holder's claim: a member's
/// device can't slip one in (by lamport) between a holder's claim and the
/// answer it wrote right away.
fn handover_not_early(slot: &TurnSlot, at: ClaimAt) -> bool {
    let Some(ms) = at.ms else { return true };
    if at.holder_gone || slot.since == 0 {
        return true;
    }
    let min = (CLAIM_TIMEOUT - HANDOVER_SKEW).as_millis() as i64;
    ms >= slot.since.saturating_add(min)
}

/// The turn an answer belongs to: its `reply_to_message_id`, or, when that
/// is a System row, the nearest earlier non-System message (a device stamps
/// its placeholder with the last non-streaming row, which is a System row
/// when one trails the user's turn).
pub fn trigger_of(session: &ChatSession, reply_to: Uuid) -> Uuid {
    let Some(at) = session.message_index(reply_to) else {
        return reply_to;
    };
    if session.messages[at].role != MessageRole::System {
        return reply_to;
    }
    session.messages[..at]
        .iter()
        .rev()
        .find(|m| m.role != MessageRole::System)
        .map(|m| m.id)
        .unwrap_or(reply_to)
}

/// Whether `m` is an answer the dispatcher arbitrates: an assistant or agent
/// reply naming the message it answers.
pub fn is_answer(m: &ChatMessage) -> bool {
    matches!(m.role, MessageRole::Assistant | MessageRole::Agent) && m.reply_to_message_id.is_some()
}

/// Fold an answer whose origin device is verified, with no signed time (see
/// [`note_answer_at`]).
pub fn note_answer(session: &mut ChatSession, message_id: Uuid, origin: Uuid) {
    note_answer_at(session, message_id, origin, None)
}

/// Fold an answer whose origin device is verified (`origin`: the device
/// that signed it, or that certified the agent key that did), signed at
/// `ms` (unix ms). Marks it `superseded` when it isn't the turn's committed
/// answer. The message must already be in `session.messages`.
///
/// An answer from a holder that was handed over from, signed before that
/// hand-over, still commits, and displaces an answer a later holder
/// committed: the hand-over came after it in time, whatever lamport it
/// took.
pub fn note_answer_at(session: &mut ChatSession, message_id: Uuid, origin: Uuid, ms: Option<i64>) {
    let Some(idx) = session.message_index(message_id) else {
        return;
    };
    let m = &session.messages[idx];
    if !is_answer(m) {
        return;
    }
    let responder = m.responder_id;
    let trigger = trigger_of(session, m.reply_to_message_id.unwrap_or_default());
    let per_receiver = responder.map(|a| turn_claim_key(trigger, Some(a)));
    let mut key = per_receiver
        .filter(|k| session.turn_dispatch.contains_key(k))
        .or_else(|| {
            // A reply to a System row may have been claimed under that row.
            let direct = m
                .reply_to_message_id
                .filter(|r| *r != trigger && session.turn_dispatch.contains_key(r));
            direct.or(session
                .turn_dispatch
                .contains_key(&trigger)
                .then_some(trigger))
        });
    // Another agent's answer to the same trigger (a message that mentions
    // several, each run where it lives) is its own turn: it commits under
    // its receiver's key instead of being fenced by the first agent's.
    if let (Some(k), Some(pr)) = (key, per_receiver) {
        if k != pr {
            let committed_responder = session.turn_dispatch[&k]
                .answer
                .and_then(|a| session.messages.iter().find(|x| x.id == a))
                .map(|x| x.responder_id);
            if committed_responder.is_some_and(|r| r != responder) {
                key = None;
            }
        }
    }
    let superseded = match key {
        Some(k) => {
            let slot = session.turn_dispatch.get_mut(&k).expect("key is present");
            match slot.answer {
                Some(a) if a == message_id => false,
                None if slot.holder == Some(origin) => {
                    slot.answer = Some(message_id);
                    slot.answered_by = Some(origin);
                    false
                }
                committed => {
                    // A displaced holder's answer signed before its hand-over.
                    let restore = slot
                        .before
                        .iter()
                        .rposition(|d| d.device == origin)
                        .is_some_and(|i| {
                            let d = slot.before[i];
                            let in_time = ms.is_some_and(|t| d.until > 0 && t < d.until);
                            let later = match slot.answered_by {
                                None => true,
                                Some(by) => slot.position(by).is_none_or(|p| p > i),
                            };
                            in_time && (committed.is_none() || later)
                        });
                    if restore {
                        slot.answer = Some(message_id);
                        slot.answered_by = Some(origin);
                        if let Some(prev) = committed {
                            if let Some(p) = session.messages.iter_mut().find(|x| x.id == prev) {
                                p.superseded = true;
                            }
                        }
                        false
                    } else {
                        true
                    }
                }
            }
        }
        None => {
            // Nobody claimed it (a local cascade, a relay-less device): it
            // commits under the trigger and its receiver's key, so a late
            // claim can't run it again.
            let slot = TurnSlot {
                answer: Some(message_id),
                answered_by: Some(origin),
                ..Default::default()
            };
            match per_receiver {
                Some(k) => {
                    session
                        .turn_dispatch
                        .entry(trigger)
                        .or_insert_with(|| slot.clone());
                    session.turn_dispatch.entry(k).or_insert(slot);
                }
                None => {
                    session.turn_dispatch.entry(trigger).or_insert(slot);
                }
            }
            false
        }
    };
    if let Some(i) = session.message_index(message_id) {
        session.messages[i].superseded = superseded;
    }
}

/// A message was removed: a turn whose committed answer is gone starts
/// over (regenerate, on any device: the next answer commits whoever writes
/// it, and a fresh claim may take it).
pub fn note_removed(session: &mut ChatSession, message_id: Uuid) {
    let freed: Vec<Uuid> = session
        .turn_dispatch
        .iter()
        .filter(|(_, s)| s.answer == Some(message_id))
        .map(|(k, _)| *k)
        .collect();
    for k in freed {
        session.turn_dispatch.remove(&k);
        session.turn_claims.remove(&k);
    }
}

/// Whether the turn under `key` has a committed answer.
pub fn answered(session: &ChatSession, key: Uuid) -> bool {
    session
        .turn_dispatch
        .get(&key)
        .is_some_and(|s| s.answer.is_some())
}

/// Whether this device covers the chat's repo, as its host gate sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    /// The chat has no repo: every device is equal.
    NoRepo,
    /// A confirmed checkout of the chat's repo that still is that repo.
    Present,
    /// This host is cloning it: claims at once, waits for the clone.
    Cloning,
    /// No usable checkout here: the host gate would refuse.
    Missing,
}

/// One device's view of a turn when it decides.
#[derive(Debug, Clone, Copy)]
pub struct View {
    /// This device.
    pub me: Uuid,
    /// `turn_runs_here` (local-only and solo devices always are; the
    /// deferred fallback asks as an eligible device).
    pub eligible: bool,
    pub coverage: Coverage,
    /// How long this device has seen the turn (or the current claim).
    pub waited: Duration,
    /// How old the trigger is.
    pub age: Duration,
    /// The holder beats on the presence channel now.
    pub holder_live: bool,
    /// The holder is revoked, or no longer acts for a member, in the
    /// current roster.
    pub holder_gone: bool,
}

/// What a device does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Write this claim, re-fold, and run only if it is then the holder.
    Claim(Claim),
    /// This device holds the turn: run it.
    Run,
    /// Look again after this long.
    Wait(Duration),
    /// Nothing for this device to do.
    StandDown,
}

/// The decision (design section 11.4). Pure: the same fold and view give
/// the same step.
pub fn decide(session: &ChatSession, key: Uuid, chat_repo_id: Option<Uuid>, v: &View) -> Step {
    let slot = session.turn_dispatch.get(&key);
    if slot.is_some_and(|s| s.answer.is_some()) {
        return Step::StandDown;
    }
    let attested = (v.coverage == Coverage::Present)
        .then_some(chat_repo_id)
        .flatten();
    let holder = session.turn_claims.get(&key).copied();
    match holder {
        Some(h) if h == v.me => Step::Run,
        Some(h) => {
            if !v.eligible {
                return Step::StandDown;
            }
            let (epoch, covers) = slot
                .map(|s| (s.epoch.max(1), s.covers))
                .unwrap_or((1, false));
            if attested.is_some() && !covers {
                return Step::Claim(Claim {
                    device: v.me,
                    epoch,
                    supersedes: None,
                    repo_id: attested,
                });
            }
            let held = slot.map(|s| s.holds_of(v.me)).unwrap_or(0);
            if (epoch >= MAX_TURN_EPOCH && !v.holder_gone)
                || v.age > TAKEOVER_MAX_AGE
                || held >= MAX_HOLDS_PER_DEVICE
            {
                return Step::StandDown;
            }
            let handover = Step::Claim(Claim {
                device: v.me,
                epoch: epoch + 1,
                supersedes: Some(h),
                repo_id: attested,
            });
            if v.holder_gone {
                return handover;
            }
            if !v.holder_live {
                if v.waited >= CLAIM_TIMEOUT {
                    return handover;
                }
                return Step::Wait(
                    CLAIM_TIMEOUT
                        .saturating_sub(v.waited)
                        .max(Duration::from_secs(1)),
                );
            }
            // A live holder that still hasn't committed (wedged, or a
            // member's device that claims and never answers): handed over
            // too, but a covering holder only to another covering device
            // (a slow clone or checkout isn't replaced by a refusal).
            let may = !covers || v.coverage == Coverage::Present;
            if may && v.waited >= HOLDER_COMMIT_TIMEOUT {
                return handover;
            }
            let until = if may {
                HOLDER_COMMIT_TIMEOUT
            } else {
                TAKEOVER_MAX_AGE
            };
            Step::Wait(
                until
                    .saturating_sub(v.waited)
                    .clamp(Duration::from_secs(1), CLAIM_TIMEOUT),
            )
        }
        None => {
            if !v.eligible {
                return Step::StandDown;
            }
            if v.coverage == Coverage::Missing && v.waited < UNCOVERED_BACKOFF {
                return Step::Wait(UNCOVERED_BACKOFF - v.waited);
            }
            Step::Claim(Claim {
                device: v.me,
                epoch: 1,
                supersedes: None,
                repo_id: attested,
            })
        }
    }
}

/// Whether `device` no longer counts as a holder in `roster` (the current
/// one, not the claim's position): revoked, or certified only by accounts
/// that are no longer members. A device the roster doesn't know is not
/// gone (its turn waits out the timeout instead).
pub fn holder_gone(roster: &crate::authz::RosterState, device: Uuid) -> bool {
    let Some(d) = roster.devices.get(&device) else {
        return false;
    };
    if d.revoked {
        return true;
    }
    let members: std::collections::BTreeSet<Uuid> = roster
        .members
        .values()
        .filter_map(|m| m.account_id)
        .collect();
    !d.accounts.is_empty() && !d.accounts.iter().any(|a| members.contains(a))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::ChatMessage;

    fn chat() -> (ChatSession, Uuid) {
        let mut s = ChatSession::new("t", Uuid::new_v4(), "x");
        let m = ChatMessage::new(MessageRole::User, "ana", "hi");
        let id = m.id;
        s.messages.push(m);
        (s, id)
    }

    fn answer(s: &mut ChatSession, trigger: Uuid, origin: Uuid) -> Uuid {
        let mut m = ChatMessage::new(MessageRole::Assistant, "ligus", "...");
        m.reply_to_message_id = Some(trigger);
        m.origin_device_id = Some(origin);
        let id = m.id;
        s.messages.push(m);
        note_answer(s, id, origin);
        id
    }

    fn superseded(s: &ChatSession, id: Uuid) -> bool {
        s.messages.iter().find(|m| m.id == id).unwrap().superseded
    }

    #[test]
    fn first_claim_holds_and_a_second_loses() {
        let (mut s, t) = chat();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        assert_eq!(fold_claim(&mut s, t, Claim::first(a)), ClaimOutcome::Held);
        assert_eq!(fold_claim(&mut s, t, Claim::first(b)), ClaimOutcome::Lost);
        assert_eq!(fold_claim(&mut s, t, Claim::first(a)), ClaimOutcome::Again);
        assert_eq!(s.turn_claims[&t], a);
    }

    #[test]
    fn only_the_holders_answer_commits() {
        let (mut s, t) = chat();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        fold_claim(&mut s, t, Claim::first(a));
        let by_b = answer(&mut s, t, b);
        let by_a = answer(&mut s, t, a);
        let again = answer(&mut s, t, a);
        assert!(superseded(&s, by_b));
        assert!(!superseded(&s, by_a));
        assert!(superseded(&s, again), "one committed answer per turn");
        assert_eq!(
            fold_claim(
                &mut s,
                t,
                Claim {
                    device: b,
                    epoch: 2,
                    supersedes: Some(a),
                    repo_id: None
                }
            ),
            ClaimOutcome::Answered
        );
    }

    #[test]
    fn a_covering_claim_displaces_a_non_covering_holder_once() {
        let (mut s, t) = chat();
        let repo = Uuid::new_v4();
        s.repo = Some(crate::workspace_repo::ChatRepo {
            repo_id: Some(repo),
            ..Default::default()
        });
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        fold_claim(&mut s, t, Claim::first(a));
        let wrong_repo = Claim {
            device: c,
            epoch: 1,
            supersedes: None,
            repo_id: Some(Uuid::new_v4()),
        };
        assert_eq!(
            fold_claim(&mut s, t, wrong_repo),
            ClaimOutcome::Lost,
            "coverage of another repo is no coverage"
        );
        assert_eq!(
            fold_claim(
                &mut s,
                t,
                Claim {
                    device: b,
                    epoch: 1,
                    supersedes: None,
                    repo_id: Some(repo)
                }
            ),
            ClaimOutcome::Preempted
        );
        assert_eq!(
            fold_claim(
                &mut s,
                t,
                Claim {
                    device: c,
                    epoch: 1,
                    supersedes: None,
                    repo_id: Some(repo)
                }
            ),
            ClaimOutcome::Lost
        );
        assert_eq!(s.turn_claims[&t], b);
    }

    #[test]
    fn hand_over_needs_the_next_epoch_and_the_current_holder() {
        let (mut s, t) = chat();
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        fold_claim(&mut s, t, Claim::first(a));
        assert_eq!(
            fold_claim(
                &mut s,
                t,
                Claim {
                    device: b,
                    epoch: 3,
                    supersedes: Some(a),
                    repo_id: None
                }
            ),
            ClaimOutcome::Stale
        );
        assert_eq!(
            fold_claim(
                &mut s,
                t,
                Claim {
                    device: b,
                    epoch: 2,
                    supersedes: Some(c),
                    repo_id: None
                }
            ),
            ClaimOutcome::Stale
        );
        assert_eq!(
            fold_claim(
                &mut s,
                t,
                Claim {
                    device: b,
                    epoch: 2,
                    supersedes: Some(a),
                    repo_id: None
                }
            ),
            ClaimOutcome::HandedOver
        );
        // A concurrent hand-over from the same holder loses.
        assert_eq!(
            fold_claim(
                &mut s,
                t,
                Claim {
                    device: c,
                    epoch: 2,
                    supersedes: Some(a),
                    repo_id: None
                }
            ),
            ClaimOutcome::Lost
        );
        assert_eq!(
            fold_claim(
                &mut s,
                t,
                Claim {
                    device: c,
                    epoch: 3,
                    supersedes: Some(b),
                    repo_id: None
                }
            ),
            ClaimOutcome::HandedOver
        );
        let late_a = answer(&mut s, t, a);
        assert!(
            superseded(&s, late_a),
            "the old holder's late answer is fenced"
        );
        // A hand-over without a folded predecessor does nothing.
        let (mut s2, t2) = chat();
        assert_eq!(
            fold_claim(
                &mut s2,
                t2,
                Claim {
                    device: b,
                    epoch: 2,
                    supersedes: Some(a),
                    repo_id: None
                }
            ),
            ClaimOutcome::Stale
        );
        assert!(s2.turn_claims.is_empty());
    }

    #[test]
    fn an_unclaimed_answer_commits_and_fences_late_claims() {
        let (mut s, t) = chat();
        let a = Uuid::new_v4();
        let id = answer(&mut s, t, a);
        assert!(!superseded(&s, id));
        assert_eq!(
            fold_claim(&mut s, t, Claim::first(Uuid::new_v4())),
            ClaimOutcome::Answered
        );
        // Regenerate: removing the answer frees the turn.
        s.messages.retain(|m| m.id != id);
        note_removed(&mut s, id);
        assert_eq!(fold_claim(&mut s, t, Claim::first(a)), ClaimOutcome::Held);
    }

    #[test]
    fn regenerate_on_a_device_that_did_not_hold_the_turn_shows() {
        let (mut s, t) = chat();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        fold_claim(&mut s, t, Claim::first(a));
        let first = answer(&mut s, t, a);
        s.messages.retain(|m| m.id != first);
        note_removed(&mut s, first);
        let again = answer(&mut s, t, b);
        assert!(!superseded(&s, again));
    }

    #[test]
    fn a_reply_to_a_trailing_system_row_answers_the_trigger() {
        let (mut s, t) = chat();
        let note = ChatMessage::new(MessageRole::System, "ligus", "note");
        let n = note.id;
        s.messages.push(note);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        fold_claim(&mut s, t, Claim::first(a));
        let by_b = answer(&mut s, n, b);
        assert!(
            superseded(&s, by_b),
            "b replied to the System row but the turn is a's"
        );
        let by_a = answer(&mut s, n, a);
        assert!(!superseded(&s, by_a));
    }

    fn at(ms: i64) -> ClaimAt {
        ClaimAt {
            ms: Some(ms),
            holder_gone: false,
        }
    }

    fn answer_at(s: &mut ChatSession, trigger: Uuid, origin: Uuid, ms: i64) -> Uuid {
        let mut m = ChatMessage::new(MessageRole::Assistant, "ligus", "...");
        m.reply_to_message_id = Some(trigger);
        m.origin_device_id = Some(origin);
        let id = m.id;
        s.messages.push(m);
        note_answer_at(s, id, origin, Some(ms));
        id
    }

    /// Review (adversarial): a member's device can't chain hand-overs to
    /// itself up to the epoch cap and lock the turn.
    #[test]
    fn no_hand_over_to_yourself_and_at_most_two_holds_per_device() {
        let (mut s, t) = chat();
        let (a, m1, m2) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let step = (CLAIM_TIMEOUT.as_millis()) as i64;
        fold_claim_at(&mut s, t, Claim::first(a), at(0));
        let c = |d, e, sup| Claim {
            device: d,
            epoch: e,
            supersedes: Some(sup),
            repo_id: None,
        };
        assert_eq!(
            fold_claim_at(&mut s, t, c(a, 2, a), at(step)),
            ClaimOutcome::Stale,
            "no hand-over to yourself"
        );
        assert_eq!(
            fold_claim_at(&mut s, t, c(m1, 2, a), at(step)),
            ClaimOutcome::HandedOver
        );
        assert_eq!(
            fold_claim_at(&mut s, t, c(m2, 3, m1), at(2 * step)),
            ClaimOutcome::HandedOver
        );
        assert_eq!(
            fold_claim_at(&mut s, t, c(m1, 4, m2), at(3 * step)),
            ClaimOutcome::HandedOver
        );
        assert_eq!(
            fold_claim_at(&mut s, t, c(m2, 5, m1), at(4 * step)),
            ClaimOutcome::HandedOver
        );
        assert_eq!(
            fold_claim_at(&mut s, t, c(m1, 6, m2), at(5 * step)),
            ClaimOutcome::Stale,
            "a third hold"
        );
        assert_eq!(s.turn_dispatch[&t].epoch, 5);
        // The first holder may come back once more.
        assert_eq!(
            fold_claim_at(&mut s, t, c(a, 6, m2), at(5 * step)),
            ClaimOutcome::HandedOver
        );
        assert_eq!(
            decide(
                &s,
                t,
                None,
                &View {
                    waited: CLAIM_TIMEOUT,
                    ..view(m1)
                }
            ),
            Step::StandDown,
            "m1 held twice"
        );
    }

    /// Review (adversarial): a hand-over slipped in by lamport right after a
    /// holder's claim is ignored, and a holder's answer signed before a
    /// hand-over that did fold still commits over the new holder's.
    #[test]
    fn a_backdated_hand_over_never_fences_a_prompt_answer() {
        let (mut s, t) = chat();
        let (a, m) = (Uuid::new_v4(), Uuid::new_v4());
        fold_claim_at(&mut s, t, Claim::first(a), at(1_000));
        let early = Claim {
            device: m,
            epoch: 2,
            supersedes: Some(a),
            repo_id: None,
        };
        assert_eq!(
            fold_claim_at(&mut s, t, early, at(2_000)),
            ClaimOutcome::Stale,
            "too early"
        );
        // Signed late enough: it folds, but a's answer was signed first.
        assert_eq!(
            fold_claim_at(&mut s, t, early, at(1_000 + 20_000)),
            ClaimOutcome::HandedOver
        );
        let by_m = answer_at(&mut s, t, m, 21_000);
        assert!(!superseded(&s, by_m));
        let by_a = answer_at(&mut s, t, a, 3_000);
        assert!(
            !superseded(&s, by_a),
            "a answered before it was handed over from"
        );
        assert!(superseded(&s, by_m), "one committed answer");
        assert_eq!(s.turn_dispatch[&t].answer, Some(by_a));
        // A late answer of a (after the hand-over) stays fenced.
        let late = answer_at(&mut s, t, a, 30_000);
        assert!(superseded(&s, late));
    }

    #[test]
    fn a_gone_holder_is_handed_over_at_once_even_past_the_cap() {
        let (mut s, t) = chat();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        fold_claim_at(&mut s, t, Claim::first(a), at(0));
        let h = Claim {
            device: b,
            epoch: 2,
            supersedes: Some(a),
            repo_id: None,
        };
        assert_eq!(
            fold_claim_at(
                &mut s,
                t,
                h,
                ClaimAt {
                    ms: Some(1),
                    holder_gone: true
                }
            ),
            ClaimOutcome::HandedOver
        );
        s.turn_dispatch.get_mut(&t).unwrap().epoch = MAX_TURN_EPOCH;
        let c = Uuid::new_v4();
        let past = Claim {
            device: c,
            epoch: MAX_TURN_EPOCH + 1,
            supersedes: Some(b),
            repo_id: None,
        };
        assert_eq!(
            fold_claim_at(&mut s, t, past, at(i64::MAX / 2)),
            ClaimOutcome::Stale
        );
        assert_eq!(
            fold_claim_at(
                &mut s,
                t,
                past,
                ClaimAt {
                    ms: Some(2),
                    holder_gone: true
                }
            ),
            ClaimOutcome::HandedOver
        );
    }

    /// Liveness (review): a live holder that never commits is handed over
    /// after HOLDER_COMMIT_TIMEOUT; a covering one only by a covering device.
    #[test]
    fn a_live_holder_that_never_answers_is_handed_over() {
        let (mut s, t) = chat();
        let (a, me) = (Uuid::new_v4(), Uuid::new_v4());
        fold_claim(&mut s, t, Claim::first(a));
        let live = View {
            holder_live: true,
            ..view(me)
        };
        assert!(matches!(
            decide(
                &s,
                t,
                None,
                &View {
                    waited: CLAIM_TIMEOUT,
                    ..live
                }
            ),
            Step::Wait(_)
        ));
        assert!(
            matches!(decide(&s, t, None, &View { waited: HOLDER_COMMIT_TIMEOUT, ..live }), Step::Claim(c) if c.epoch == 2)
        );
        let repo = Uuid::new_v4();
        let (mut s, t) = chat();
        s.repo = Some(crate::workspace_repo::ChatRepo {
            repo_id: Some(repo),
            ..Default::default()
        });
        fold_claim(
            &mut s,
            t,
            Claim {
                device: a,
                epoch: 1,
                supersedes: None,
                repo_id: Some(repo),
            },
        );
        let slow = View {
            waited: HOLDER_COMMIT_TIMEOUT,
            coverage: Coverage::Missing,
            ..live
        };
        assert!(
            matches!(decide(&s, t, Some(repo), &slow), Step::Wait(_)),
            "a slow covering holder isn't replaced by a refusal"
        );
        assert!(
            matches!(decide(&s, t, Some(repo), &View { coverage: Coverage::Present, ..slow }), Step::Claim(c) if c.epoch == 2)
        );
    }

    /// Two agents mentioned in one message, each run where it lives: each
    /// answer commits for its own agent.
    #[test]
    fn another_agents_answer_to_the_same_trigger_is_its_own_turn() {
        let (mut s, t) = chat();
        let w = Uuid::new_v4();
        let (x, y) = (Uuid::new_v4(), Uuid::new_v4());
        fold_claim(&mut s, t, Claim::first(w));
        let mut first = ChatMessage::new(MessageRole::Assistant, "x", "...");
        first.reply_to_message_id = Some(t);
        first.responder_id = Some(x);
        let fx = first.id;
        s.messages.push(first);
        note_answer(&mut s, fx, w);
        let mut second = ChatMessage::new(MessageRole::Assistant, "y", "...");
        second.reply_to_message_id = Some(t);
        second.responder_id = Some(y);
        let fy = second.id;
        s.messages.push(second);
        note_answer(&mut s, fy, w);
        assert!(!superseded(&s, fx) && !superseded(&s, fy));
        // A second answer by the same agent is still fenced.
        let mut dup = ChatMessage::new(MessageRole::Assistant, "y", "...");
        dup.reply_to_message_id = Some(t);
        dup.responder_id = Some(y);
        let fd = dup.id;
        s.messages.push(dup);
        note_answer(&mut s, fd, w);
        assert!(superseded(&s, fd));
    }

    fn view(me: Uuid) -> View {
        View {
            me,
            eligible: true,
            coverage: Coverage::NoRepo,
            waited: Duration::ZERO,
            age: Duration::ZERO,
            holder_live: false,
            holder_gone: false,
        }
    }

    #[test]
    fn decide_table() {
        let (mut s, t) = chat();
        let (a, me) = (Uuid::new_v4(), Uuid::new_v4());
        assert!(
            matches!(decide(&s, t, None, &view(me)), Step::Claim(c) if c.epoch == 1 && c.repo_id.is_none())
        );
        assert_eq!(
            decide(
                &s,
                t,
                None,
                &View {
                    eligible: false,
                    ..view(me)
                }
            ),
            Step::StandDown
        );
        let repo = Uuid::new_v4();
        assert_eq!(
            decide(
                &s,
                t,
                Some(repo),
                &View {
                    coverage: Coverage::Missing,
                    ..view(me)
                }
            ),
            Step::Wait(UNCOVERED_BACKOFF)
        );
        assert!(
            matches!(decide(&s, t, Some(repo), &View { coverage: Coverage::Cloning, ..view(me) }), Step::Claim(c) if c.repo_id.is_none())
        );
        assert!(
            matches!(decide(&s, t, Some(repo), &View { coverage: Coverage::Present, ..view(me) }), Step::Claim(c) if c.repo_id == Some(repo))
        );
        fold_claim(&mut s, t, Claim::first(a));
        assert_eq!(decide(&s, t, None, &view(a)), Step::Run);
        assert_eq!(decide(&s, t, None, &view(me)), Step::Wait(CLAIM_TIMEOUT));
        let late = View {
            waited: CLAIM_TIMEOUT,
            ..view(me)
        };
        assert!(
            matches!(decide(&s, t, None, &late), Step::Claim(c) if c.epoch == 2 && c.supersedes == Some(a))
        );
        assert!(
            matches!(
                decide(
                    &s,
                    t,
                    None,
                    &View {
                        holder_live: true,
                        ..late
                    }
                ),
                Step::Wait(_)
            ),
            "a live holder keeps the turn"
        );
        assert!(
            matches!(decide(&s, t, None, &View { holder_gone: true, ..view(me) }), Step::Claim(c) if c.epoch == 2),
            "a revoked holder is replaced at once"
        );
        assert_eq!(
            decide(
                &s,
                t,
                None,
                &View {
                    age: TAKEOVER_MAX_AGE * 2,
                    ..late
                }
            ),
            Step::StandDown,
            "no hand-over of an old turn"
        );
        answer(&mut s, t, a);
        assert_eq!(decide(&s, t, None, &late), Step::StandDown);
    }
}
