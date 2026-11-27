//! Who a request acts for: its principal `(person, device, via host)`.
//!
//! Every "you" decision this box makes for a request (unread counts, read
//! marks, which DMs it lists, "mine" on a message or a proposal, whether an
//! agent is usable, which questions it may answer, which people row is "you")
//! is made for the person behind the calling phone, resolved once per
//! request in the API's auth layer ([`super::api::principal`]):
//!
//! - through the relay tunnel, or on the direct path with the phone's own
//!   direct-path credential ([`Shared::direct_token_for`]): the person who
//!   holds the pairing's workspace device
//!   ([`ligus_runtime::ChatService::phone_person`]);
//! - the operator on the box (`serve.operator.token`): this box's own
//!   identity, as before;
//! - the shared direct-path token (`serve.token`, held by every phone paired
//!   before per-phone credentials), a pairing without a workspace device, or
//!   a phone whose person can't be told: [`Standing::Unresolved`], which
//!   fails closed: workspace-wide items only, no DMs, no previews, nothing
//!   claimed as theirs.
//!
//! The box's own identity is used only for what the box itself writes and
//! hosts (its host record, its service-member writes, agent turns it runs,
//! ntfy) and for the operator.

use ligus_core::{ActorIdentity, ChatSession, DeviceLinks};
use uuid::Uuid;

use super::api::Caller;
use super::dms::DmViewer;
use super::Shared;

/// What a phone that can't be placed is told (the `hint` of an
/// authenticated `GET /v1/health`, and the box's log).
pub const UNRESOLVED_HINT: &str =
    "This phone isn't linked to a person on this box, so it shows workspace-wide items only. Update the Ligus app to see your own chats and notices.";

/// The read-mark key of every phone this box can't place (never an account
/// or actor id, which are UUIDs).
pub const UNRESOLVED_READS: &str = "-unresolved";

/// The response header a request with the shared direct-path token gets, so
/// an app that knows it fetches its own credential over the tunnel.
pub const DIRECT_CREDENTIAL_HEADER: &str = "ligus-direct-credential";

/// How a request stands with this box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// The operator on the box: this box's own identity.
    Box,
    /// A phone placed as its person.
    Person,
    /// A phone (or credential) this box can't place: fails closed.
    Unresolved,
}

/// The principal of a request ([`super::api::principal`]).
#[derive(Clone, Debug)]
pub struct Principal {
    /// The person (the box's own identity for [`Standing::Box`]); `None`
    /// when unresolved.
    pub person: Option<ActorIdentity>,
    /// The device that made the request (the phone's workspace device, or
    /// this box's own).
    pub device: Option<Uuid>,
    /// The host relaying the request for a phone (this box's host id).
    pub via_host: Option<String>,
    pub standing: Standing,
}

impl Principal {
    /// A principal that claims nothing (also the answer off a request's task).
    pub fn unresolved() -> Self {
        Self { person: None, device: None, via_host: None, standing: Standing::Unresolved }
    }

    pub fn is_box(&self) -> bool {
        self.standing == Standing::Box
    }

    pub fn is_unresolved(&self) -> bool {
        self.standing == Standing::Unresolved
    }

    /// The person's actor (member) id; empty when unresolved.
    pub fn actor_id(&self) -> String {
        self.person.as_ref().map(|p| p.id.clone()).unwrap_or_default()
    }

    /// The person's account.
    pub fn account(&self) -> Option<Uuid> {
        self.person.as_ref().and_then(|p| p.account_id)
    }

    /// What `standing` is called on the wire (`GET /v1/health`).
    pub fn standing_name(&self) -> &'static str {
        match self.standing {
            Standing::Box => "box",
            Standing::Person => "person",
            Standing::Unresolved => "unresolved",
        }
    }

    /// Who places a primary `@ligus` turn ("this" / "mine") for this request.
    pub fn viewer(&self) -> super::api::Viewer {
        super::api::Viewer { device: self.device, account: self.account() }
    }

    /// Whose DMs this request sees.
    pub fn dm_viewer(&self) -> DmViewer {
        match self.standing {
            Standing::Box => DmViewer::Box,
            Standing::Person => self.account().map_or(DmViewer::Nobody, DmViewer::Person),
            Standing::Unresolved => DmViewer::Nobody,
        }
    }

    /// The key this person's read marks are kept under: their account
    /// (shared by all their phones), else their actor id. Every phone this
    /// box can't place shares one set ([`UNRESOLVED_READS`]), as every phone
    /// shared the box's before, so an older app's badges still clear; it is
    /// nobody's, and no person's marks are read or moved for it.
    pub fn read_key(&self) -> String {
        match &self.person {
            Some(p) => p.account_id.map(|a| a.to_string()).unwrap_or_else(|| p.id.clone()),
            None => UNRESOLVED_READS.to_string(),
        }
    }

    /// Who "you" are in `chat` for this request (mentions, own messages,
    /// the `@you` requester): nobody when unresolved.
    pub fn me_in(&self, chat: &ChatSession, links: &DeviceLinks) -> super::notify::Me {
        match &self.person {
            Some(p) => super::notify::Me::in_chat(p, chat, links),
            None => super::notify::Me::default(),
        }
    }

    /// Whether `m` was written by this person (on any of their devices).
    pub fn wrote(&self, me: &super::notify::Me, m: &ligus_core::ChatMessage) -> bool {
        if me.wrote(m) {
            return true;
        }
        let acct = self.account();
        m.role == ligus_core::MessageRole::User
            && acct.is_some()
            && m.actor_identity.as_ref().is_some_and(|a| a.account_id == acct)
    }
}

impl Shared {
    /// This box's own principal (the operator, and what the box itself
    /// builds before a request's view is applied).
    pub fn principal_for_self(&self) -> Principal {
        let svc = self.lock();
        Principal { person: Some(svc.author().clone()), device: Some(svc.device_id()), via_host: None, standing: Standing::Box }
    }

    /// The principal of the phone paired as `pid` (through the tunnel or on
    /// the direct path with its own credential): its person, or unresolved.
    pub fn principal_for_phone(&self, pid: &str) -> Principal {
        let device = self.tunnel.as_ref().and_then(|t| t.registry.get(pid)).filter(|r| r.is_active()).and_then(|r| r.device_id);
        let Some(device) = device else { return Principal::unresolved() };
        match self.lock().phone_person(self.ws, device) {
            Ok(person) => Principal { person: Some(person), device: Some(device), via_host: Some(self.host_id.clone()), standing: Standing::Person },
            Err(_) => Principal { device: Some(device), via_host: Some(self.host_id.clone()), ..Principal::unresolved() },
        }
    }

    /// The principal of a request from `caller`.
    pub fn principal_for(&self, caller: &Caller) -> Principal {
        match caller {
            Caller::Operator => self.principal_for_self(),
            Caller::Tunnel(pid) | Caller::Direct(pid) => self.principal_for_phone(pid),
            Caller::Bearer | Caller::Anonymous => Principal::unresolved(),
        }
    }

    /// The direct-path credential of the phone paired as `pid`: derived from
    /// this box's direct-path secret (`serve.token`) and the pairing, so it
    /// needs no storage and changes whenever that secret rotates (a phone
    /// revocation). Handed out only through the tunnel
    /// (`GET /v1/tunnel/direct`).
    pub fn direct_token_for(&self, pid: &str) -> String {
        direct_token(&self.token(), pid)
    }

    /// The active pairing a direct-path credential belongs to, if it is one.
    pub fn direct_phone(&self, token: &str) -> Option<String> {
        let pid = direct_token_pid(token)?;
        let active = self.tunnel.as_ref().is_some_and(|t| t.registry.is_active(pid));
        (active && super::pair::token_matches(&self.direct_token_for(pid), token)).then(|| pid.to_string())
    }
}

/// The prefix of a per-phone direct-path credential.
const DIRECT_PREFIX: &str = "ld1.";

/// `ld1.<pid>.<hex(HMAC-SHA256(secret, "ligus-direct-v1\0" || pid))>`.
pub fn direct_token(secret: &str, pid: &str) -> String {
    use hmac::{KeyInit, Mac};
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes any key length");
    mac.update(b"ligus-direct-v1\0");
    mac.update(pid.as_bytes());
    let mac: String = mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect();
    format!("{DIRECT_PREFIX}{pid}.{mac}")
}

/// The pairing id a token names, when it has the shape of a per-phone
/// credential (nothing is checked yet).
fn direct_token_pid(token: &str) -> Option<&str> {
    let rest = token.strip_prefix(DIRECT_PREFIX)?;
    let (pid, mac) = rest.rsplit_once('.')?;
    (!pid.is_empty() && mac.len() == 64).then_some(pid)
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn direct_tokens_are_per_phone_and_per_secret() {
        let a = direct_token("secret-one", "pid-a");
        assert!(a.starts_with("ld1.pid-a."));
        assert_eq!(direct_token_pid(&a), Some("pid-a"));
        assert_ne!(a, direct_token("secret-one", "pid-b"), "each phone its own");
        assert_ne!(a, direct_token("secret-two", "pid-a"), "a rotated secret changes it");
        assert_eq!(a, direct_token("secret-one", "pid-a"), "derived, not stored");
        // The shared token (64 hex chars) is never read as one.
        assert_eq!(direct_token_pid(&"ab".repeat(32)), None);
        assert_eq!(direct_token_pid("ld1..abc"), None);
    }

    #[test]
    fn an_unresolved_principal_claims_nothing() {
        let p = Principal::unresolved();
        assert_eq!(p.actor_id(), "");
        assert_eq!(p.read_key(), UNRESOLVED_READS);
        assert_eq!(p.dm_viewer(), DmViewer::Nobody);
        assert!(p.viewer().account.is_none());
    }
}
