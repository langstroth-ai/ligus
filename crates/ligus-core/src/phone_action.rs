//! Phone-signed actions (Phase B): the typed, allow-listed set of events a
//! phone signs with its own P-256 (Secure Enclave) key, drafted by its box.
//!
//! A box (`ligus serve`) is its own service member: it never acts as the
//! phone's person. For what a person does from the phone (post, vote or
//! approve, answer an agent's question, clear a lockdown, ask a host to
//! apply or withdraw that request), the box **drafts the exact event** as the
//! person, stamped for the phone; the phone confirms with the person's
//! passkey when the rules need presence; the phone **reads the preimage**
//! with [`check_phone_action_preimage`] and signs it only if it is exactly
//! the action it asked for; the box verifies the signature against the
//! phone's certificate, judges the event as every fold will and appends it.
//!
//! The box chooses only what the phone can't know in advance and what grants
//! nothing: the event id, sequence, lamport clock, timestamps, its config
//! frontier and chain link, a new message's or request's id. Every field the
//! person decides is checked against the phone's own request; every other
//! field must be its default, and no field this build doesn't read may ride
//! along (one a newer build might honour is not something the person saw).
//!
//! Box admissions keep their own check
//! ([`crate::admission::check_phone_admission_preimage`]); both share the
//! strict parser [`parse_phone_preimage`].

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::chat::{ChatMessage, MessageRole, TranscriptEventKind};
use crate::events::{EventScope, SessionEvent};
use crate::identity::{ActorKind, ActorStamp};
use crate::presence::PresenceAssertion;

/// The frozen signing context of every envelope preimage.
const PREFIX: &[u8] = b"hive-envelope-v3\0";

/// The longest message a phone posts (characters).
pub const MAX_POST_CHARS: usize = 16_000;

/// What a phone asks its box to draft (`POST /v1/phone-actions/prepare`,
/// field `action`). The JSON is `{"kind": "<kind>", …camelCase fields}`;
/// unknown kinds and unknown fields are refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PhoneAction {
    /// Post `text` in `chat_id` as the person (`messageAppended`, role user).
    Post { chat_id: Uuid, text: String },
    /// Approve (`approved: true`) or reject a proposal, binding the content
    /// digest the phone showed (`proposalVoteCast`).
    Vote {
        chat_id: Uuid,
        proposal_id: Uuid,
        diff_digest: String,
        approved: bool,
    },
    /// Answer an agent's question (`flowRecorded` / `humanAnswered`). On a
    /// v2 card (`docs/design/agent-questions.md` 6.3) the phone also names
    /// `askDigest`, computed from the signed ask it was given (the payload
    /// of its preimage), and may add free `text` or say Not now
    /// (`decline`); `answer` is then the option's text (empty for a
    /// free-text-only answer).
    Answer {
        chat_id: Uuid,
        ask_id: Uuid,
        answer: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        option_index: Option<u32>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        ask_digest: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        text: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        decline: bool,
    },
    /// Clear the lockdowns of a chat the phone shows (`chatLockdownCleared`,
    /// on the workspace config log; always with the person's passkey
    /// presence): `raises` names each by the event that raised it (the
    /// `raiseEventId` of `GET /v1/guards`), at least one. A lockdown it
    /// doesn't name stays in force.
    ClearLockdown { chat_id: Uuid, raises: Vec<Uuid> },
    /// Ask `host_device_id` to apply an approved proposal
    /// (`proposalApplyRequested`): the phone's "Apply on ‹host›".
    RequestApply {
        chat_id: Uuid,
        proposal_id: Uuid,
        diff_digest: String,
        host_device_id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_base_commit: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repo_id: Option<Uuid>,
    },
    /// Withdraw an open apply request (`proposalApplyWithdrawn`).
    WithdrawApply {
        chat_id: Uuid,
        proposal_id: Uuid,
        request_id: Uuid,
    },
    /// Archive a settled proposal for everyone (`archived: true`), or
    /// restore it to the inbox (`proposalDismissed`, the wire name kept
    /// from Dismiss). Additive (0.4): an older box refuses the kind.
    Archive {
        chat_id: Uuid,
        proposal_id: Uuid,
        archived: bool,
    },
    /// Propose a workspace memory change as the person (`flowRecorded` /
    /// `memoryProposed`, on the workspace config log): a person decides it on
    /// a desktop. `chat_id` is the chat the phone was in (the source).
    ProposeMemory {
        chat_id: Uuid,
        op: crate::memory::MemoryOp,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        rationale: String,
    },
}

impl PhoneAction {
    /// The wire name of the action (its `kind`).
    pub fn kind(&self) -> &'static str {
        match self {
            PhoneAction::Post { .. } => "post",
            PhoneAction::Vote { .. } => "vote",
            PhoneAction::Answer { .. } => "answer",
            PhoneAction::ClearLockdown { .. } => "clearLockdown",
            PhoneAction::RequestApply { .. } => "requestApply",
            PhoneAction::WithdrawApply { .. } => "withdrawApply",
            PhoneAction::ProposeMemory { .. } => "proposeMemory",
            PhoneAction::Archive { .. } => "archive",
        }
    }

    /// The chat the action concerns.
    pub fn chat_id(&self) -> Uuid {
        match self {
            PhoneAction::Post { chat_id, .. }
            | PhoneAction::Vote { chat_id, .. }
            | PhoneAction::Answer { chat_id, .. }
            | PhoneAction::ClearLockdown { chat_id, .. }
            | PhoneAction::RequestApply { chat_id, .. }
            | PhoneAction::WithdrawApply { chat_id, .. }
            | PhoneAction::ProposeMemory { chat_id, .. }
            | PhoneAction::Archive { chat_id, .. } => *chat_id,
        }
    }

    /// The log the event goes on: the chat's, or (clearing a lockdown) the
    /// workspace config log.
    pub fn session_id(&self, workspace_id: Uuid) -> Uuid {
        match self {
            PhoneAction::ClearLockdown { .. } | PhoneAction::ProposeMemory { .. } => {
                crate::channel::workspace_config_session_id(workspace_id)
            }
            other => other.chat_id(),
        }
    }

    /// The SessionEvent kind it signs.
    pub fn event_kind(&self) -> &'static str {
        match self {
            PhoneAction::Post { .. } => "messageAppended",
            PhoneAction::Vote { .. } => "proposalVoteCast",
            PhoneAction::Answer { .. } => "flowRecorded",
            PhoneAction::ClearLockdown { .. } => "chatLockdownCleared",
            PhoneAction::RequestApply { .. } => "proposalApplyRequested",
            PhoneAction::WithdrawApply { .. } => "proposalApplyWithdrawn",
            PhoneAction::ProposeMemory { .. } => "flowRecorded",
            PhoneAction::Archive { .. } => "proposalDismissed",
        }
    }

    /// Why the phone's own request is malformed, if it is (checked by the
    /// box at prepare and by the phone before it asks).
    pub fn malformed(&self) -> Option<&'static str> {
        match self {
            PhoneAction::Post { text, .. } if text.trim().is_empty() => {
                Some("a post can't be empty")
            }
            PhoneAction::Post { text, .. } if text.chars().count() > MAX_POST_CHARS => {
                Some("a post is too long")
            }
            PhoneAction::Vote { diff_digest, .. }
            | PhoneAction::RequestApply { diff_digest, .. }
                if !is_digest(diff_digest) =>
            {
                Some("a content digest is 64 lowercase hex digits")
            }
            PhoneAction::Answer {
                answer,
                text,
                decline,
                ..
            } if answer.trim().is_empty() && text.trim().is_empty() && !decline => {
                Some("an answer can't be empty")
            }
            PhoneAction::Answer { text, .. }
                if text.chars().count() > ligus_proto::ask_text::MAX_TEXT_ANSWER_CHARS =>
            {
                Some("the reply is too long")
            }
            PhoneAction::Answer { ask_digest, .. }
                if !ask_digest.is_empty()
                    && (ask_digest.len() != 64
                        || !ask_digest
                            .bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())) =>
            {
                Some("askDigest is 64 lowercase hex characters")
            }
            PhoneAction::Answer { answer, .. }
                if answer.chars().count() > crate::flow::MAX_ANSWER_CHARS =>
            {
                Some("an answer is too long")
            }
            PhoneAction::RequestApply {
                expected_base_commit: Some(c),
                ..
            } if !crate::workspace_repo::is_commit_id(c) => {
                Some("a base commit is 40 or 64 hex digits")
            }
            PhoneAction::ClearLockdown { raises, .. } => {
                crate::authz::bounds::clear_names_raises(raises).err()
            }
            PhoneAction::ProposeMemory { op, rationale, .. } => {
                use crate::memory::MemoryOp;
                if rationale.chars().count() > crate::memory::MAX_NOTE_CHARS {
                    return Some("a rationale is at most 500 characters");
                }
                match op {
                    MemoryOp::Create {
                        title, body, tags, ..
                    }
                    | MemoryOp::Edit {
                        title, body, tags, ..
                    } => crate::memory::content_ok(title, body, tags).err(),
                    MemoryOp::Remove { .. } => None,
                }
            }
            _ => None,
        }
    }
}

fn is_digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A strictly parsed phone preimage.
#[derive(Debug, Clone, PartialEq)]
pub struct PhonePreimage {
    pub workspace_id: Uuid,
    pub session_id: Uuid,
    pub sequence: i64,
    pub lamport: u64,
    pub event_id: Uuid,
    pub stamp: ActorStamp,
    pub event: SessionEvent,
}

/// Whether `bytes` are exactly the JSON of a `T` this build reads: they
/// decode, and re-encode to the same JSON value (no unknown field, no field
/// a newer build might honour).
fn exact<T: Serialize + for<'de> Deserialize<'de>>(bytes: &[u8]) -> Option<T> {
    let typed: T = serde_json::from_slice(bytes).ok()?;
    let raw: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    (serde_json::to_value(&typed).ok()? == raw).then_some(typed)
}

/// The presence digest (`presence::challenge`) over a parsed preimage's own
/// bytes.
fn presence_digest(
    workspace: Uuid,
    session: Uuid,
    event_id: Uuid,
    lamport: u64,
    payload: &[u8],
    stamp: &ActorStamp,
) -> [u8; 32] {
    let frontier = stamp
        .cfg_frontier
        .map(|f| serde_json::to_vec(&f).unwrap_or_default())
        .unwrap_or_default();
    let account = stamp.actor.account_id.unwrap_or_else(Uuid::nil);
    let mut h = Sha256::new();
    h.update(crate::presence::PRESENCE_CONTEXT);
    h.update(workspace.as_bytes());
    h.update(session.as_bytes());
    h.update(event_id.as_bytes());
    h.update(lamport.to_le_bytes());
    h.update((payload.len() as u32).to_le_bytes());
    h.update(payload);
    h.update((frontier.len() as u32).to_le_bytes());
    h.update(&frontier);
    h.update(account.as_bytes());
    h.finalize().into()
}

/// What the phone confirmed with its person's passkey: the challenge it
/// signed and the assertion it produced.
#[derive(Debug, Clone, Copy)]
pub struct Confirmed<'a> {
    pub challenge: &'a [u8; 32],
    /// The assertion the phone produced; `None` when only the challenge is
    /// known (the box admission check, which predates this).
    pub assertion: Option<&'a PresenceAssertion>,
}

/// The strict parser every phone check shares. `preimage` must be exactly a
/// `hive-envelope-v3` preimage (no trailing bytes) on `workspace_id`, whose
/// scope, timestamp, stamp and payload each decode and re-encode to the same
/// JSON (no unknown field), whose scope is the payload's own, stamped for a
/// human signing with `phone`'s P-256 key (not a runtime effect), of
/// `account` when given. With `confirmed`, the stamp carries presence whose
/// digest over these very bytes is the challenge the passkey signed (and is
/// the very assertion the phone produced, when it gives one); without, the
/// stamp carries no presence at all.
pub fn parse_phone_preimage(
    preimage: &[u8],
    workspace_id: Uuid,
    phone: Uuid,
    account: Option<Uuid>,
    confirmed: Option<Confirmed<'_>>,
) -> Result<PhonePreimage, &'static str> {
    let mut rest = preimage
        .strip_prefix(PREFIX)
        .ok_or("not an envelope preimage")?;
    let mut take = |n: usize| -> Result<&[u8], &'static str> {
        if rest.len() < n {
            return Err("a truncated preimage");
        }
        let (a, b) = rest.split_at(n);
        rest = b;
        Ok(a)
    };
    let uuid = |b: &[u8]| Uuid::from_slice(b).map_err(|_| "a malformed preimage");
    let workspace = uuid(take(16)?)?;
    let session = uuid(take(16)?)?;
    let sequence = i64::from_le_bytes(take(8)?.try_into().map_err(|_| "a malformed preimage")?);
    let lamport = u64::from_le_bytes(take(8)?.try_into().map_err(|_| "a malformed preimage")?);
    let event_id = uuid(take(16)?)?;
    let mut fields: Vec<&[u8]> = Vec::with_capacity(4);
    for _ in 0..4 {
        let len =
            u32::from_le_bytes(take(4)?.try_into().map_err(|_| "a malformed preimage")?) as usize;
        fields.push(take(len)?);
    }
    if !rest.is_empty() {
        return Err("trailing bytes after the preimage");
    }
    let [scope, timestamp, stamp_bytes, payload] = [fields[0], fields[1], fields[2], fields[3]];
    if workspace != workspace_id {
        return Err("not on this workspace");
    }
    let scope: EventScope = exact(scope).ok_or("a malformed scope")?;
    exact::<crate::Timestamp>(timestamp).ok_or("a malformed timestamp")?;
    let stamp: Option<ActorStamp> =
        exact(stamp_bytes).ok_or("a stamp with fields this build doesn't read")?;
    let stamp = stamp.ok_or("an unstamped event")?;
    if stamp.actor.device_id != Some(phone) || stamp.sig_alg != crate::crypto::SigAlg::P256 {
        return Err("not stamped for this phone");
    }
    if stamp.actor.kind != ActorKind::Human || stamp.runtime_effect {
        return Err("not stamped as a person");
    }
    // Lamport bounds (`crate::lamport`): one past the log head it names (1
    // with none), so the event sorts no later than one past what the box
    // held when it drafted it. Every fold refuses anything else.
    if crate::lamport::next(stamp.log_head) != lamport
        || stamp.log_head.is_some_and(|h| h.lamport == u64::MAX)
    {
        return Err("a lamport that isn't one past its log head");
    }
    if account.is_some_and(|a| stamp.actor.account_id != Some(a)) {
        return Err("not stamped for this phone's person");
    }
    let event: SessionEvent =
        exact(payload).ok_or("a payload with fields this build doesn't read")?;
    if matches!(event, SessionEvent::Unknown) {
        return Err("a payload this build doesn't read");
    }
    if event.scope() != scope {
        return Err("a scope that isn't the event's own");
    }
    match confirmed {
        Some(c) => {
            let Some(presence) = stamp.presence.as_ref() else {
                return Err("not stamped with its person's presence");
            };
            if c.assertion.is_some_and(|a| a != presence) {
                return Err("not the presence this phone produced");
            }
            if &presence_digest(workspace, session, event_id, lamport, payload, &stamp)
                != c.challenge
            {
                return Err("not the event the passkey confirmed");
            }
        }
        None => {
            if stamp.presence.is_some() {
                return Err("stamped with a presence this phone didn't produce");
            }
        }
    }
    Ok(PhonePreimage {
        workspace_id: workspace,
        session_id: session,
        sequence,
        lamport,
        event_id,
        stamp,
        event,
    })
}

/// What a checked phone action names, for the phone to show and keep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedPhoneAction {
    pub event_id: Uuid,
    pub session_id: Uuid,
    /// The member the event is stamped as (the phone's person).
    pub actor_id: String,
    /// The new message's id (`post`).
    pub message_id: Option<Uuid>,
    /// The new apply request's id (`requestApply`).
    pub request_id: Option<Uuid>,
    /// The new memory proposal's id (`proposeMemory`).
    pub memory_proposal_id: Option<Uuid>,
}

/// What a phone checks before signing the preimage its box drafted for
/// `action` (`POST /v1/phone-actions/presence`, or `prepare` when no
/// presence is needed): [`parse_phone_preimage`] on `workspace_id`, for
/// `phone` and its person's `account`, with the passkey's `confirmed`
/// challenge (or none, when the box said no presence is needed: then the
/// stamp must carry none); on the action's own log; and a payload that is
/// exactly `action`:
///
/// * `post`: a user message whose body is `text`, authored and identified as
///   the stamped person, every other field its default;
/// * `vote`: a vote on `proposalId` cast by the stamped person, `approved` as
///   asked, bound to `diffDigest`;
/// * `answer`: the answer to `askId`, its text and option, by the stamped
///   person's name;
/// * `clearLockdown`: of `chatId`, on the workspace config log, naming
///   exactly the `raises` the phone asked to clear;
/// * `requestApply`: of `proposalId` on `hostDeviceId`, with that digest,
///   base and repo (the request id is the box's);
/// * `withdrawApply`: of `requestId` for `proposalId`;
/// * `archive`: of `proposalId`, archived or restored as asked;
/// * `proposeMemory`: a memory proposal of exactly `op` and `rationale`, as
///   the person (no agent proposer), sourced from `chatId` (the proposal id
///   is the box's; a create's new entry is named by it, whatever entry id
///   the phone sent; the box may record the chat's egress).
///
/// Anything else (another kind, workspace, chat, person or device, a field
/// the phone didn't ask for) is refused.
pub fn check_phone_action_preimage(
    preimage: &[u8],
    confirmed: Option<Confirmed<'_>>,
    workspace_id: Uuid,
    phone: Uuid,
    account: Uuid,
    action: &PhoneAction,
) -> Result<CheckedPhoneAction, &'static str> {
    if let Some(why) = action.malformed() {
        return Err(why);
    }
    let p = parse_phone_preimage(preimage, workspace_id, phone, Some(account), confirmed)?;
    if p.session_id != action.session_id(workspace_id) {
        return Err("not on the log this action goes on");
    }
    let actor = &p.stamp.actor;
    let mut out = CheckedPhoneAction {
        event_id: p.event_id,
        session_id: p.session_id,
        actor_id: actor.id.clone(),
        message_id: None,
        request_id: None,
        memory_proposal_id: None,
    };
    let mismatch = "not the action this phone asked for";
    match (action, &p.event) {
        (PhoneAction::Post { text, .. }, SessionEvent::MessageAppended { message }) => {
            let mut expected =
                ChatMessage::new(MessageRole::User, actor.display_name.clone(), text.clone());
            expected.id = message.id;
            expected.created_at = message.created_at;
            expected.kind = TranscriptEventKind::Message;
            expected.actor_identity = Some(actor.clone());
            if *message != expected {
                return Err(mismatch);
            }
            out.message_id = Some(message.id);
        }
        (
            PhoneAction::Vote {
                proposal_id,
                diff_digest,
                approved,
                ..
            },
            SessionEvent::ProposalVoteCast {
                proposal_id: pid,
                approval,
            },
        ) => {
            if pid != proposal_id
                || approval.actor_id != actor.id
                || approval.approved != *approved
                || approval.diff_digest.as_deref() != Some(diff_digest.as_str())
            {
                return Err(mismatch);
            }
        }
        (
            PhoneAction::Answer {
                ask_id,
                answer,
                option_index,
                ask_digest,
                text,
                decline,
                ..
            },
            SessionEvent::FlowRecorded {
                record: crate::flow::FlowRecord::HumanAnswered(a),
            },
        ) => {
            if a.ask_id != *ask_id
                || a.answer != *answer
                || a.option_index != *option_index
                || a.by != actor.display_name
                || a.ask_digest != *ask_digest
                || a.text != *text
                || a.decline != *decline
            {
                return Err(mismatch);
            }
        }
        (
            PhoneAction::ClearLockdown { chat_id, raises },
            SessionEvent::ChatLockdownCleared {
                chat_id: c,
                raises: r,
            },
        ) => {
            if c != chat_id || r != raises {
                return Err(mismatch);
            }
        }
        (
            PhoneAction::RequestApply {
                proposal_id,
                diff_digest,
                host_device_id,
                expected_base_commit,
                repo_id,
                ..
            },
            SessionEvent::ProposalApplyRequested { request },
        ) => {
            if request.proposal_id != *proposal_id
                || request.diff_digest != *diff_digest
                || request.host_device_id != *host_device_id
                || request.expected_base_commit != *expected_base_commit
                || request.repo_id != *repo_id
            {
                return Err(mismatch);
            }
            out.request_id = Some(request.id);
        }
        (
            PhoneAction::WithdrawApply {
                proposal_id,
                request_id,
                ..
            },
            SessionEvent::ProposalApplyWithdrawn {
                request_id: r,
                proposal_id: pid,
            },
        ) => {
            if r != request_id || pid != proposal_id {
                return Err(mismatch);
            }
        }
        (
            PhoneAction::Archive {
                proposal_id,
                archived,
                ..
            },
            SessionEvent::ProposalDismissed {
                proposal_id: pid,
                dismissed,
            },
        ) => {
            if pid != proposal_id || dismissed != archived {
                return Err(mismatch);
            }
        }
        (
            PhoneAction::ProposeMemory {
                chat_id,
                op,
                rationale,
            },
            SessionEvent::FlowRecorded {
                record: crate::flow::FlowRecord::MemoryProposed(mp),
            },
        ) => {
            use crate::memory::MemoryOp;
            // The proposal is known by the event that files it
            // (`memory::bound_event_id`); a draft that isn't would be denied.
            if mp.proposal_id != p.event_id {
                return Err(mismatch);
            }
            let p = mp;
            // A new entry is named by the proposal (the box picks both): the
            // phone's own entry id for a create is not used.
            let same_op = match (op, &p.op) {
                (
                    MemoryOp::Create {
                        title, body, tags, ..
                    },
                    MemoryOp::Create {
                        entry_id,
                        title: t,
                        body: b,
                        tags: g,
                    },
                ) => *entry_id == p.proposal_id && (title, body, tags) == (t, b, g),
                (asked, drafted) => asked == drafted,
            };
            // The source is the phone's chat. The box may record that chat's
            // egress: it can only narrow which turns read the note.
            let source_ok = p.source.chat_id == Some(*chat_id)
                && p.source.message_id.is_none()
                && p.source.turn_id.is_none();
            if !same_op || p.rationale != *rationale || !p.proposer.is_empty() || !source_ok {
                return Err(mismatch);
            }
            out.memory_proposal_id = Some(p.proposal_id);
        }
        _ => return Err(mismatch),
    }
    Ok(out)
}

/// The SessionEvent kinds a phone-signed action may carry, plus the box
/// admission: everything the generic path drafts. The phone tier
/// ([`crate::authz::phone::phone_tier_allows`]) allows these and a few
/// more Phase A content kinds (reactions, notes, dismissing, raising a
/// lockdown, removing its person's own message, its own revocation); see
/// `tests/phone_actions.rs` for the exact set.
pub const PHONE_ACTION_EVENT_KINDS: &[&str] = &[
    "messageAppended",
    "proposalVoteCast",
    "flowRecorded",
    "chatLockdownCleared",
    "proposalApplyRequested",
    "proposalApplyWithdrawn",
    "proposalDismissed",
];
