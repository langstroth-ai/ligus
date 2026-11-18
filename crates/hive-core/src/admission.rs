//! Box admissions (Phase B): how a `ligus serve` or `ligus worker` box joins a
//! workspace as its **own service member**, without a person at the box.
//!
//! An Admin issues a single-use invite whose `JoinApproved` carries an
//! [`Admission`]: it admits only a member of kind `Service`, at most
//! Contributor, until `expires_ms`, and (when the box first sent a join
//! request) only the account that request named. Issuing it is a
//! membership change, so it needs the Admin's passkey presence like every
//! invite; a phone may issue one with its own key (the phone tier allows
//! nothing else about membership).
//!
//! The box redeems it by adding itself (`MemberAdded { invite }`), signed by
//! its own device: the redemption binds the invite to that key, and the
//! invite is spent (`GuardState::used_invites`). The workspace key never
//! rides the event log: it reaches the box only inside the invite code
//! (sealed to the box's key-agreement key when the box sent a request) or
//! through a later rotation sealed to the box's published key.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::events::{SessionEvent, SessionEventEnvelope};
use crate::identity::{ActorKind, WorkspaceMember, WorkspaceRole};
use crate::session::ChatSession;

/// The longest an admission may stay redeemable after it was issued.
pub const MAX_ADMISSION_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// The terms of a box admission (the optional part of a `JoinApproved`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Admission {
    /// The kind of member it admits. Only `service` is defined; any other
    /// kind is refused, so an older build's reading can't widen it.
    pub kind: ActorKind,
    /// Unix milliseconds after which a redemption no longer counts.
    pub expires_ms: i64,
    /// When set, only this account may redeem it (the account the box's
    /// join request named). Left out when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<Uuid>,
}

impl Admission {
    /// A service admission until `expires_ms`, optionally bound to `account`.
    pub fn service(expires_ms: i64, account_id: Option<Uuid>) -> Self {
        Self { kind: ActorKind::Service, expires_ms, account_id }
    }
}

/// Unix milliseconds of an envelope's signed timestamp.
pub fn envelope_ms(env: &SessionEventEnvelope) -> i64 {
    let t = env.timestamp.inner();
    (t.unix_timestamp_nanos() / 1_000_000) as i64
}

/// The invite id a `JoinApproved` subject issues, if it issues one.
pub fn invite_of_subject(subject: &str) -> Option<Uuid> {
    subject.strip_prefix("invite:").and_then(|s| Uuid::parse_str(s).ok())
}

/// The admission terms of invite `id`, if it is a box admission.
pub fn admission_of(fold: &ChatSession, id: Uuid) -> Option<&Admission> {
    fold.guards.service_invites.get(&id)
}

/// Why issuing `admission` under `subject` is refused, if it is: it names a
/// fresh invite, admits a service, and expires after it was issued and within
/// [`MAX_ADMISSION_TTL_MS`]. A plain `JoinApproved` may not re-issue a box
/// admission's invite as a person's.
pub fn issuance_refusal(env: &SessionEventEnvelope, fold: &ChatSession, on_config: bool) -> Option<&'static str> {
    let SessionEvent::JoinApproved { subject, admission } = &env.payload else { return None };
    let invite = invite_of_subject(subject);
    let Some(a) = admission else {
        return invite
            .filter(|id| fold.guards.service_invites.contains_key(id))
            .map(|_| "that invite is a box admission");
    };
    if !on_config {
        return Some("a box admission is issued on the workspace config log");
    }
    let Some(id) = invite else {
        return Some("a box admission names an invite");
    };
    if a.kind != ActorKind::Service {
        return Some("an admission admits only a service member");
    }
    if fold.guards.join_approvals.contains_key(subject) || fold.guards.used_invites.contains(&id) {
        return Some("that invite already exists");
    }
    let issued = envelope_ms(env);
    if a.expires_ms <= issued || a.expires_ms - issued > MAX_ADMISSION_TTL_MS {
        return Some("an admission expires after it is issued and within 7 days");
    }
    None
}

/// Why a self-enrollment through `invite` is refused, if it is.
///
/// * A box admission admits a `Service` member, at most Contributor, signed
///   no later than it expires, by the account it names (if it names one).
/// * A `Service` member joins only through a box admission: a person's
///   invite never admits a box.
pub fn redemption_refusal(
    env: &SessionEventEnvelope,
    member: &WorkspaceMember,
    invite: Option<Uuid>,
    fold: &ChatSession,
) -> Option<&'static str> {
    let author = env.actor_stamp.as_ref().map(|s| s.actor.id.as_str()).unwrap_or("");
    if member.id != author {
        // Not a self-enrollment: the role gate decides.
        return None;
    }
    let terms = invite.and_then(|id| admission_of(fold, id));
    if let Some(existing) = fold.members.iter().find(|m| m.id == member.id) {
        // A member re-adding itself never spends a box admission.
        if terms.is_some() {
            return Some("already a member: a box admission admits a new box");
        }
        // Nor changes what it is: a box never re-adds itself as a person (or
        // the reverse), and no one moves their membership to another account.
        if existing.actor.kind != member.actor.kind {
            return Some("a member's kind never changes when it re-adds itself");
        }
        if existing.actor.account_id.is_some() && existing.actor.account_id != member.actor.account_id {
            return Some("a member's account never changes when it re-adds itself");
        }
        return None;
    }
    let Some(terms) = terms else {
        return (member.actor.kind == ActorKind::Service).then_some("a service member joins only through a box admission");
    };
    if member.actor.kind != terms.kind {
        return Some("a box admission admits only a service member");
    }
    if member.role.rank() > WorkspaceRole::Contributor.rank() {
        return Some("a service member holds at most Contributor");
    }
    if envelope_ms(env) > terms.expires_ms {
        return Some("that box admission has expired");
    }
    if let Some(account) = terms.account_id {
        let stamped = env.actor_stamp.as_ref().and_then(|s| s.actor.account_id);
        if stamped != Some(account) || member.actor.account_id != Some(account) {
            return Some("that box admission is for another box");
        }
    }
    None
}
