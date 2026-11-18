//! A signed workspace for tests (not part of the product API).
//!
//! Every person has an account key, one certified device and an offline
//! recovery key registered on the config log, so tests can write events the
//! way real devices do: signed by a certified device, stamped with the
//! author's config frontier, and (for events that need it) carrying the
//! author's presence. Rules v2 is the only rule set, so a test that wants a
//! governance event without presence uses [`SignedWorld::push_bare`].

use std::collections::BTreeMap;
use std::sync::Arc;

use uuid::Uuid;

use crate::crypto::{sign_envelope, DeviceCertificate, SigningKeypair};
use crate::events::{SessionEvent, SessionEventEnvelope};
use crate::identity::{ActorIdentity, ActorKind, ActorStamp, CfgFrontier, WorkspaceMember, WorkspaceRole};
use crate::presence::{PresenceAssertion, PresencePolicy, RecoveryKey};
use crate::session::ChatSession;
use crate::time_util::Timestamp;
use crate::workspace_config_session_id;

/// One person: an account, one device and a recovery key.
pub struct Person {
    pub name: String,
    pub account_id: Uuid,
    pub account: SigningKeypair,
    pub device_id: Uuid,
    pub device: SigningKeypair,
    pub recovery: RecoveryKey,
}

impl Person {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            account_id: Uuid::new_v4(),
            account: SigningKeypair::generate().unwrap(),
            device_id: Uuid::new_v4(),
            device: SigningKeypair::generate().unwrap(),
            recovery: RecoveryKey::from_seed([0; 32]),
        }
        .with_test_recovery()
    }

    fn with_test_recovery(mut self) -> Self {
        self.recovery = test_recovery_key(self.account_id);
        self
    }

    pub fn actor(&self) -> ActorIdentity {
        let mut a = ActorIdentity::new(&self.name, &self.name, ActorKind::Human);
        a.account_id = Some(self.account_id);
        a
    }

    pub fn member(&self, role: WorkspaceRole) -> WorkspaceMember {
        WorkspaceMember { id: self.name.clone(), actor: self.actor(), role, title: String::new(), index: 0, joined_at: Timestamp::epoch() }
    }

    pub fn certificate(&self) -> DeviceCertificate {
        DeviceCertificate::issue(&self.account, self.account_id, self.device_id, &self.device.public_key_bytes(), Timestamp::epoch())
    }
}

/// A workspace's config log plus any number of chat logs, written by real
/// signing devices.
#[derive(Clone)]
pub struct SignedWorld {
    pub wid: Uuid,
    /// One lamport clock for every log, so a later write sorts later.
    pub clock: u64,
    pub cfg: Vec<SessionEventEnvelope>,
    pub chats: BTreeMap<Uuid, Vec<SessionEventEnvelope>>,
    pub people: Arc<BTreeMap<String, Person>>,
    /// People who aren't humans (a service member), by name.
    pub kinds: BTreeMap<String, ActorKind>,
}

/// How an event is stamped.
#[derive(Clone, Copy)]
pub enum Proof {
    /// No presence.
    None,
    /// The author's recovery key.
    Recovery,
}

impl SignedWorld {
    /// A workspace founded by `names[0]` (its Owner), who publishes their
    /// identity, registers their recovery key and sets rules v2 with `policy`
    /// (the genesis `WorkspaceRulesSet`). The other people exist but aren't
    /// members; add them with [`SignedWorld::admit`].
    pub fn genesis(names: &[&str], policy: Option<PresencePolicy>) -> Self {
        let people: BTreeMap<String, Person> = names.iter().map(|n| (n.to_string(), Person::new(n))).collect();
        let mut w = Self { wid: Uuid::new_v4(), clock: 0, cfg: Vec::new(), chats: BTreeMap::new(), people: Arc::new(people), kinds: BTreeMap::new() };
        let founder = names[0];
        let mut seed = ChatSession::new("", w.wid, "");
        seed.id = workspace_config_session_id(w.wid);
        seed.creator_actor_id = founder.into();
        seed.members = vec![w.people[founder].member(WorkspaceRole::Owner)];
        w.push_bare(founder, SessionEvent::SessionSnapshot { session: Box::new(seed) });
        w.publish_identity(founder);
        w.register_recovery(founder);
        w.push(founder, SessionEvent::WorkspaceRulesSet { version: 2, presence: policy });
        w
    }

    /// `name` publishes their account key and device certificate.
    pub fn publish_identity(&mut self, name: &str) {
        let p = &self.people[name];
        let (account_id, pk, cert) = (p.account_id, p.account.public_key_bytes().to_vec(), p.certificate());
        self.push_bare(name, SessionEvent::AccountKeyRegistered { account_id, signing_public_key: pk });
        self.push_bare(name, SessionEvent::DeviceCertificateAdded { certificate: cert });
    }

    /// `name` registers their recovery key (it proves itself).
    pub fn register_recovery(&mut self, name: &str) -> Uuid {
        let p = &self.people[name];
        let cred = p.recovery.credential(p.account_id, "Recovery key");
        let mut env = self.draft(name, None, SessionEvent::PasskeyRegistered { credential: cred }, None);
        let a = self.people[name].recovery.assert(&crate::presence::challenge(&env));
        self.finish(&mut env, name, Some(a));
        self.push_env(None, env)
    }

    /// The Owner (`by`, with presence) adds `name` at `role`; `name` then
    /// publishes their identity and registers their recovery key.
    pub fn admit(&mut self, by: &str, name: &str, role: WorkspaceRole) {
        let m = self.member_of(name, role);
        self.push(by, SessionEvent::MemberAdded { member: m, invite: None, proof: None });
        self.publish_identity(name);
        self.register_recovery(name);
    }

    /// `name`'s member record (with their kind).
    pub fn member_of(&self, name: &str, role: WorkspaceRole) -> WorkspaceMember {
        let mut m = self.people[name].member(role);
        m.actor.kind = self.kinds.get(name).copied().unwrap_or(ActorKind::Human);
        m
    }

    /// The newest config event: the frontier an honest author stamps.
    pub fn head(&self) -> Option<CfgFrontier> {
        self.cfg.iter().map(|e| CfgFrontier { lamport: e.lamport, event_id: e.event_id }).max()
    }

    /// Draft an envelope by `by` on `chat` (`None` = the config log), stamped
    /// at `frontier` (`None` for the config log = the head), signed, without
    /// presence.
    pub fn draft(&mut self, by: &str, chat: Option<Uuid>, payload: SessionEvent, frontier: Option<CfgFrontier>) -> SessionEventEnvelope {
        self.clock += 1;
        let sid = chat.unwrap_or_else(|| workspace_config_session_id(self.wid));
        let mut env = SessionEventEnvelope::new(sid, self.wid, self.clock as i64, payload);
        env.lamport = self.clock;
        let p = &self.people[by];
        let mut actor = p.actor();
        actor.kind = self.kinds.get(by).copied().unwrap_or(ActorKind::Human);
        actor.device_id = Some(p.device_id);
        let cfg_frontier = frontier.or_else(|| self.head());
        // The device's previous event on this log (its chain, for removal
        // watermarks).
        let log = match chat {
            None => &self.cfg,
            Some(c) => self.chats.get(&c).map(Vec::as_slice).unwrap_or(&[]),
        };
        let prev = log.iter().rev().find(|e| e.signer_device_id == Some(p.device_id)).map(|e| e.event_id);
        env.actor_stamp = Some(ActorStamp { actor, recorded_at: Timestamp::epoch(), cfg_frontier, presence: None, runtime_effect: false, prev, sig_alg: Default::default() });
        sign_envelope(&mut env, p.device_id, &p.device);
        env
    }

    /// Attach `presence` (inside the stamp) and re-sign as `by`.
    pub fn finish(&self, env: &mut SessionEventEnvelope, by: &str, presence: Option<PresenceAssertion>) {
        if let Some(s) = env.actor_stamp.as_mut() {
            s.presence = presence;
        }
        env.clear_raw();
        let p = &self.people[by];
        sign_envelope(env, p.device_id, &p.device);
    }

    /// The author's recovery-key presence over `env`'s digest.
    pub fn recovery_presence(&self, by: &str, env: &SessionEventEnvelope) -> PresenceAssertion {
        self.people[by].recovery.assert(&crate::presence::challenge(env))
    }

    /// Store `env` on its log; returns its event id.
    pub fn push_env(&mut self, chat: Option<Uuid>, env: SessionEventEnvelope) -> Uuid {
        let id = env.event_id;
        match chat {
            None => self.cfg.push(env),
            Some(c) => self.chats.entry(c).or_default().push(env),
        }
        id
    }

    /// A config event by `by` with their recovery-key presence.
    pub fn push(&mut self, by: &str, payload: SessionEvent) -> Uuid {
        self.write(by, None, payload, None, Proof::Recovery)
    }

    /// A config event by `by` without presence.
    pub fn push_bare(&mut self, by: &str, payload: SessionEvent) -> Uuid {
        self.write(by, None, payload, None, Proof::None)
    }

    /// Any event: on `chat` (or the config log), at `frontier` (or the head),
    /// with `proof`.
    pub fn write(&mut self, by: &str, chat: Option<Uuid>, payload: SessionEvent, frontier: Option<CfgFrontier>, proof: Proof) -> Uuid {
        let mut env = self.draft(by, chat, payload, frontier);
        if let Proof::Recovery = proof {
            let a = self.recovery_presence(by, &env);
            self.finish(&mut env, by, Some(a));
        }
        self.push_env(chat, env)
    }

    /// Found a chat (seeded by `by` at the config head) and return its id.
    pub fn chat(&mut self, by: &str, title: &str) -> Uuid {
        let mut s = ChatSession::new(title, self.wid, "anthropic");
        s.creator_actor_id = by.into();
        s.members = vec![self.people[by].member(WorkspaceRole::Owner)];
        let id = s.id;
        self.write(by, Some(id), SessionEvent::SessionSnapshot { session: Box::new(s) }, None, Proof::None);
        id
    }

    /// A chat event by `by` at the config head (an honest author), without
    /// presence.
    pub fn say(&mut self, chat: Uuid, by: &str, payload: SessionEvent) -> Uuid {
        self.write(by, Some(chat), payload, None, Proof::None)
    }

    /// The config log's timeline.
    pub fn timeline(&self) -> crate::authz::ConfigTimeline {
        crate::events::project_config(&self.cfg, None, None).1
    }

    /// The roster at the config head.
    pub fn roster(&self) -> crate::authz::RosterState {
        self.timeline().current().clone()
    }

    /// The config fold.
    pub fn config(&self) -> ChatSession {
        crate::events::project_config(&self.cfg, None, None).0.unwrap()
    }

    /// A chat's checked fold.
    pub fn state(&self, chat: Uuid) -> ChatSession {
        let t = self.timeline();
        crate::events::project_authorized(&self.chats[&chat], None, None, crate::events::WorkspaceAuthz::Timeline(&t)).unwrap()
    }

    /// Every verdict: the config log's and every chat's.
    pub fn verdicts(&self) -> BTreeMap<Uuid, crate::events::Verdict> {
        let t = self.timeline();
        let mut v = crate::events::fold_decisions(&self.cfg, None, None, crate::events::WorkspaceAuthz::Unchecked);
        for log in self.chats.values() {
            v.extend(crate::events::fold_decisions(log, None, None, crate::events::WorkspaceAuthz::Timeline(&t)));
        }
        v
    }

    /// One event's verdict.
    pub fn verdict(&self, id: Uuid) -> crate::events::Verdict {
        self.verdicts()[&id]
    }
}

/// The recovery key a test account confirms with: derived from the account,
/// so every test device and world of one person holds the same one.
pub fn test_recovery_key(account: Uuid) -> RecoveryKey {
    use sha2::Digest;
    let h = sha2::Sha256::digest(format!("ligus-test-recovery:{account}").as_bytes());
    RecoveryKey::from_seed(h.into())
}

/// The payload that registers `account`'s test recovery key. Draft it, then
/// [`prove_recovery`] it.
pub fn recovery_registration(account: Uuid) -> SessionEvent {
    let mut credential = test_recovery_key(account).credential(account, "Recovery key");
    credential.registered_at = Timestamp::epoch();
    SessionEvent::PasskeyRegistered { credential }
}

/// Stamp `account`'s test recovery-key presence over `env` (the event's own
/// digest) and re-sign it with `device`. Works for a recovery registration's
/// self-proof and for any presence-gated event once the key is registered.
pub fn prove_recovery(env: &mut SessionEventEnvelope, account: Uuid, device_id: Uuid, device: &SigningKeypair) {
    let a = test_recovery_key(account).assert(&crate::presence::challenge(env));
    if let Some(s) = env.actor_stamp.as_mut() {
        s.presence = Some(a);
    }
    env.clear_raw();
    sign_envelope(env, device_id, device);
}

/// A phone: a P-256 (Secure Enclave) key and its workspace device id.
pub struct Phone {
    pub device_id: Uuid,
    pub key: p256::ecdsa::SigningKey,
}

impl Phone {
    pub fn new() -> Self {
        let mut b = [0u8; 32];
        loop {
            getrandom::fill(&mut b).expect("randomness");
            if let Ok(key) = p256::ecdsa::SigningKey::from_slice(&b) {
                return Self { device_id: Uuid::new_v4(), key };
            }
        }
    }
    pub fn public_key(&self) -> Vec<u8> {
        crate::crypto::p256_public_key(&self.key)
    }
}

impl Default for Phone {
    fn default() -> Self {
        Self::new()
    }
}

impl SignedWorld {
    /// `owner`'s certificate for `phone` (P-256, signed by their account key).
    pub fn phone_certificate(&self, owner: &str, phone: &Phone) -> DeviceCertificate {
        let p = &self.people[owner];
        DeviceCertificate::issue_p256(&p.account, p.account_id, phone.device_id, &phone.public_key(), Timestamp::epoch())
    }

    /// An event by `by`, signed by their phone's P-256 key (no presence).
    pub fn phone_write(&mut self, by: &str, phone: &Phone, chat: Option<Uuid>, payload: SessionEvent) -> Uuid {
        let mut env = self.draft(by, chat, payload, None);
        if let Some(s) = env.actor_stamp.as_mut() {
            s.actor.device_id = Some(phone.device_id);
            let log = match chat {
                None => &self.cfg,
                Some(c) => self.chats.get(&c).map(Vec::as_slice).unwrap_or(&[]),
            };
            s.prev = log.iter().rev().find(|e| e.signer_device_id == Some(phone.device_id)).map(|e| e.event_id);
        }
        crate::crypto::sign_envelope_p256(&mut env, phone.device_id, &phone.key);
        self.push_env(chat, env)
    }
}
