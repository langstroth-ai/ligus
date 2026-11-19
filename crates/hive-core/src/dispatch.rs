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
/// How long a device that doesn't cover a repo chat's repo waits before it
/// claims, so a covering host claims first.
pub const UNCOVERED_BACKOFF: Duration = Duration::from_secs(8);
/// No hand-over of a turn whose trigger is older than this: a device that
/// syncs a backlog doesn't answer yesterday's questions.
pub const TAKEOVER_MAX_AGE: Duration = Duration::from_secs(15 * 60);
/// At most this many claim generations per turn.
pub const MAX_TURN_EPOCH: u32 = 64;

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
    /// holder, or a hand-over whose predecessor isn't folded (yet).
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
        Self { device, epoch: 1, supersedes: None, repo_id: None }
    }

    fn epoch(&self) -> u32 {
        self.epoch.max(1)
    }
}

/// The chat's repo at this point of the fold.
fn chat_repo(session: &ChatSession) -> Option<Uuid> {
    session.repo.as_ref().and_then(|r| r.repo_id)
}

/// Fold one `TurnClaimed` (already authorized: signed by the device it
/// names) for `key`. Deterministic: reads only the fold so far.
pub fn fold_claim(session: &mut ChatSession, key: Uuid, claim: Claim) -> ClaimOutcome {
    let covers = claim.repo_id.is_some() && claim.repo_id == chat_repo(session);
    let epoch = claim.epoch();
    let outcome = match session.turn_dispatch.get_mut(&key) {
        Some(slot) if slot.answer.is_some() => ClaimOutcome::Answered,
        Some(slot) => {
            let holder = slot.holder;
            if epoch == slot.epoch {
                if holder == Some(claim.device) {
                    slot.covers |= covers;
                    ClaimOutcome::Again
                } else if covers && !slot.covers && claim.supersedes.is_none_or(|s| Some(s) == holder) {
                    slot.holder = Some(claim.device);
                    slot.covers = true;
                    ClaimOutcome::Preempted
                } else {
                    ClaimOutcome::Lost
                }
            } else if epoch == slot.epoch + 1 && epoch <= MAX_TURN_EPOCH && claim.supersedes.is_some() && claim.supersedes == holder {
                slot.holder = Some(claim.device);
                slot.epoch = epoch;
                slot.covers = covers;
                ClaimOutcome::HandedOver
            } else {
                ClaimOutcome::Stale
            }
        }
        None if epoch == 1 && claim.supersedes.is_none() => {
            session.turn_dispatch.insert(key, TurnSlot { holder: Some(claim.device), epoch: 1, covers, answer: None });
            ClaimOutcome::Held
        }
        None => ClaimOutcome::Stale,
    };
    if matches!(outcome, ClaimOutcome::Held | ClaimOutcome::Preempted | ClaimOutcome::HandedOver) {
        session.turn_claims.insert(key, claim.device);
    }
    outcome
}

/// The turn an answer belongs to: its `reply_to_message_id`, or, when that
/// is a System row, the nearest earlier non-System message (a device stamps
/// its placeholder with the last non-streaming row, which is a System row
/// when one trails the user's turn).
pub fn trigger_of(session: &ChatSession, reply_to: Uuid) -> Uuid {
    let Some(at) = session.message_index(reply_to) else { return reply_to };
    if session.messages[at].role != MessageRole::System {
        return reply_to;
    }
    session.messages[..at].iter().rev().find(|m| m.role != MessageRole::System).map(|m| m.id).unwrap_or(reply_to)
}

/// Whether `m` is an answer the dispatcher arbitrates: an assistant or agent
/// reply naming the message it answers.
pub fn is_answer(m: &ChatMessage) -> bool {
    matches!(m.role, MessageRole::Assistant | MessageRole::Agent) && m.reply_to_message_id.is_some()
}

/// Fold an answer whose origin device is verified (`origin`: the device
/// that signed it, or that certified the agent key that did). Marks it
/// `superseded` when it isn't the turn's committed answer. The message must
/// already be in `session.messages`.
pub fn note_answer(session: &mut ChatSession, message_id: Uuid, origin: Uuid) {
    let Some(idx) = session.message_index(message_id) else { return };
    let m = &session.messages[idx];
    if !is_answer(m) {
        return;
    }
    let trigger = trigger_of(session, m.reply_to_message_id.unwrap_or_default());
    let per_receiver = m.responder_id.map(|a| turn_claim_key(trigger, Some(a)));
    let key = per_receiver.filter(|k| session.turn_dispatch.contains_key(k)).or_else(|| {
        // A reply to a System row may have been claimed under that row.
        let direct = m.reply_to_message_id.filter(|r| *r != trigger && session.turn_dispatch.contains_key(r));
        direct.or(session.turn_dispatch.contains_key(&trigger).then_some(trigger))
    });
    let superseded = match key {
        Some(k) => {
            let slot = session.turn_dispatch.get_mut(&k).expect("key is present");
            match slot.answer {
                Some(a) => a != message_id,
                None if slot.holder == Some(origin) => {
                    slot.answer = Some(message_id);
                    false
                }
                None => true,
            }
        }
        None => {
            // Nobody claimed it (a local cascade, a relay-less device): it
            // commits under the trigger and its receiver's key, so a late
            // claim can't run it again.
            let slot = TurnSlot { holder: None, epoch: 0, covers: false, answer: Some(message_id) };
            session.turn_dispatch.entry(trigger).or_insert_with(|| slot.clone());
            if let Some(k) = per_receiver {
                session.turn_dispatch.entry(k).or_insert(slot);
            }
            false
        }
    };
    session.messages[idx].superseded = superseded;
}

/// A message was removed: a turn whose committed answer is gone starts
/// over (regenerate, on any device: the next answer commits whoever writes
/// it, and a fresh claim may take it).
pub fn note_removed(session: &mut ChatSession, message_id: Uuid) {
    let freed: Vec<Uuid> = session.turn_dispatch.iter().filter(|(_, s)| s.answer == Some(message_id)).map(|(k, _)| *k).collect();
    for k in freed {
        session.turn_dispatch.remove(&k);
        session.turn_claims.remove(&k);
    }
}

/// Whether the turn under `key` has a committed answer.
pub fn answered(session: &ChatSession, key: Uuid) -> bool {
    session.turn_dispatch.get(&key).is_some_and(|s| s.answer.is_some())
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
    let attested = (v.coverage == Coverage::Present).then_some(chat_repo_id).flatten();
    let holder = session.turn_claims.get(&key).copied();
    match holder {
        Some(h) if h == v.me => Step::Run,
        Some(h) => {
            if !v.eligible {
                return Step::StandDown;
            }
            let (epoch, covers) = slot.map(|s| (s.epoch.max(1), s.covers)).unwrap_or((1, false));
            if attested.is_some() && !covers {
                return Step::Claim(Claim { device: v.me, epoch, supersedes: None, repo_id: attested });
            }
            if epoch >= MAX_TURN_EPOCH || v.age > TAKEOVER_MAX_AGE {
                return Step::StandDown;
            }
            let handover = Step::Claim(Claim { device: v.me, epoch: epoch + 1, supersedes: Some(h), repo_id: attested });
            if v.holder_gone {
                return handover;
            }
            if v.waited >= CLAIM_TIMEOUT && !v.holder_live {
                return handover;
            }
            Step::Wait(CLAIM_TIMEOUT.saturating_sub(v.waited).max(Duration::from_secs(1)))
        }
        None => {
            if !v.eligible {
                return Step::StandDown;
            }
            if v.coverage == Coverage::Missing && v.waited < UNCOVERED_BACKOFF {
                return Step::Wait(UNCOVERED_BACKOFF - v.waited);
            }
            Step::Claim(Claim { device: v.me, epoch: 1, supersedes: None, repo_id: attested })
        }
    }
}

/// Whether `device` no longer counts as a holder in `roster` (the current
/// one, not the claim's position): revoked, or certified only by accounts
/// that are no longer members. A device the roster doesn't know is not
/// gone (its turn waits out the timeout instead).
pub fn holder_gone(roster: &crate::authz::RosterState, device: Uuid) -> bool {
    let Some(d) = roster.devices.get(&device) else { return false };
    if d.revoked {
        return true;
    }
    let members: std::collections::BTreeSet<Uuid> = roster.members.values().filter_map(|m| m.account_id).collect();
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
        let mut m = ChatMessage::new(MessageRole::Assistant, "hive", "...");
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
        assert_eq!(fold_claim(&mut s, t, Claim { device: b, epoch: 2, supersedes: Some(a), repo_id: None }), ClaimOutcome::Answered);
    }

    #[test]
    fn a_covering_claim_displaces_a_non_covering_holder_once() {
        let (mut s, t) = chat();
        let repo = Uuid::new_v4();
        s.repo = Some(crate::workspace_repo::ChatRepo { repo_id: Some(repo), ..Default::default() });
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        fold_claim(&mut s, t, Claim::first(a));
        let wrong_repo = Claim { device: c, epoch: 1, supersedes: None, repo_id: Some(Uuid::new_v4()) };
        assert_eq!(fold_claim(&mut s, t, wrong_repo), ClaimOutcome::Lost, "coverage of another repo is no coverage");
        assert_eq!(fold_claim(&mut s, t, Claim { device: b, epoch: 1, supersedes: None, repo_id: Some(repo) }), ClaimOutcome::Preempted);
        assert_eq!(fold_claim(&mut s, t, Claim { device: c, epoch: 1, supersedes: None, repo_id: Some(repo) }), ClaimOutcome::Lost);
        assert_eq!(s.turn_claims[&t], b);
    }

    #[test]
    fn hand_over_needs_the_next_epoch_and_the_current_holder() {
        let (mut s, t) = chat();
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        fold_claim(&mut s, t, Claim::first(a));
        assert_eq!(fold_claim(&mut s, t, Claim { device: b, epoch: 3, supersedes: Some(a), repo_id: None }), ClaimOutcome::Stale);
        assert_eq!(fold_claim(&mut s, t, Claim { device: b, epoch: 2, supersedes: Some(c), repo_id: None }), ClaimOutcome::Stale);
        assert_eq!(fold_claim(&mut s, t, Claim { device: b, epoch: 2, supersedes: Some(a), repo_id: None }), ClaimOutcome::HandedOver);
        // A concurrent hand-over from the same holder loses.
        assert_eq!(fold_claim(&mut s, t, Claim { device: c, epoch: 2, supersedes: Some(a), repo_id: None }), ClaimOutcome::Lost);
        assert_eq!(fold_claim(&mut s, t, Claim { device: c, epoch: 3, supersedes: Some(b), repo_id: None }), ClaimOutcome::HandedOver);
        let late_a = answer(&mut s, t, a);
        assert!(superseded(&s, late_a), "the old holder's late answer is fenced");
        // A hand-over without a folded predecessor does nothing.
        let (mut s2, t2) = chat();
        assert_eq!(fold_claim(&mut s2, t2, Claim { device: b, epoch: 2, supersedes: Some(a), repo_id: None }), ClaimOutcome::Stale);
        assert!(s2.turn_claims.is_empty());
    }

    #[test]
    fn an_unclaimed_answer_commits_and_fences_late_claims() {
        let (mut s, t) = chat();
        let a = Uuid::new_v4();
        let id = answer(&mut s, t, a);
        assert!(!superseded(&s, id));
        assert_eq!(fold_claim(&mut s, t, Claim::first(Uuid::new_v4())), ClaimOutcome::Answered);
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
        assert!(superseded(&s, by_b), "b replied to the System row but the turn is a's");
        let by_a = answer(&mut s, n, a);
        assert!(!superseded(&s, by_a));
    }

    fn view(me: Uuid) -> View {
        View { me, eligible: true, coverage: Coverage::NoRepo, waited: Duration::ZERO, age: Duration::ZERO, holder_live: false, holder_gone: false }
    }

    #[test]
    fn decide_table() {
        let (mut s, t) = chat();
        let (a, me) = (Uuid::new_v4(), Uuid::new_v4());
        assert!(matches!(decide(&s, t, None, &view(me)), Step::Claim(c) if c.epoch == 1 && c.repo_id.is_none()));
        assert_eq!(decide(&s, t, None, &View { eligible: false, ..view(me) }), Step::StandDown);
        let repo = Uuid::new_v4();
        assert_eq!(decide(&s, t, Some(repo), &View { coverage: Coverage::Missing, ..view(me) }), Step::Wait(UNCOVERED_BACKOFF));
        assert!(matches!(decide(&s, t, Some(repo), &View { coverage: Coverage::Cloning, ..view(me) }), Step::Claim(c) if c.repo_id.is_none()));
        assert!(matches!(decide(&s, t, Some(repo), &View { coverage: Coverage::Present, ..view(me) }), Step::Claim(c) if c.repo_id == Some(repo)));
        fold_claim(&mut s, t, Claim::first(a));
        assert_eq!(decide(&s, t, None, &view(a)), Step::Run);
        assert_eq!(decide(&s, t, None, &view(me)), Step::Wait(CLAIM_TIMEOUT));
        let late = View { waited: CLAIM_TIMEOUT, ..view(me) };
        assert!(matches!(decide(&s, t, None, &late), Step::Claim(c) if c.epoch == 2 && c.supersedes == Some(a)));
        assert!(matches!(decide(&s, t, None, &View { holder_live: true, ..late }), Step::Wait(_)), "a live holder keeps the turn");
        assert!(matches!(decide(&s, t, None, &View { holder_gone: true, ..view(me) }), Step::Claim(c) if c.epoch == 2), "a revoked holder is replaced at once");
        assert_eq!(decide(&s, t, None, &View { age: TAKEOVER_MAX_AGE * 2, ..late }), Step::StandDown, "no hand-over of an old turn");
        answer(&mut s, t, a);
        assert_eq!(decide(&s, t, None, &late), Step::StandDown);
    }
}
