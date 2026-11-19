//! Final pre-release review (area A): what falls between features.
//!
//! Each targeted test pins one interaction no single feature review covered;
//! the property test at the end folds a random mixed history (messages and
//! the dispatcher, lockdowns, memory, attachments, removals, revocations,
//! copies and equivocations, backdated drafts) by honest and adversarial
//! members and checks that it converges in any arrival order and that the
//! key invariants hold. Setup is built once; `PROPTEST_CASES` overrides the
//! case count.

use ligus_core::guard::{ChatLockdown, LockdownSignal};
use ligus_core::testkit::{Proof, SignedWorld};
use ligus_core::{SessionEvent, SessionEventEnvelope, Timestamp, Verdict, WorkspaceRole};
use uuid::Uuid;

fn lockdown(id: Uuid, signal: LockdownSignal, by: &str) -> ChatLockdown {
    ChatLockdown {
        id,
        signal,
        detail: "detail".into(),
        raised_by: by.into(),
        raised_at: Timestamp::epoch(),
        message_id: None,
        raise_event_id: None,
    }
}

fn world() -> SignedWorld {
    let mut w = SignedWorld::genesis(&["owner", "ada", "con", "vic", "mal"], None);
    w.admit("owner", "ada", WorkspaceRole::Admin);
    w.admit("owner", "con", WorkspaceRole::Contributor);
    w.admit("owner", "vic", WorkspaceRole::Viewer);
    w.admit("owner", "mal", WorkspaceRole::Viewer);
    w
}

/// A config event by `by`, drafted as if they held the log only up to
/// `back` events before its newest (a backdated draft), with `proof`.
fn push_back(w: &mut SignedWorld, by: &str, payload: SessionEvent, back: usize, proof: Proof) -> Uuid {
    let mut env = w.draft(by, None, payload, None);
    ligus_core::lamport::stamp_back(&mut env, &w.cfg, back);
    // The lowest id: it sorts first among the events of its lamport.
    env.event_id = Uuid::from_u128(7);
    let presence = match proof {
        Proof::Recovery => Some(w.recovery_presence(by, &env)),
        Proof::None => None,
    };
    w.finish(&mut env, by, presence);
    w.push_env(None, env)
}

fn active_raises(w: &SignedWorld, chat: Uuid) -> Vec<(Uuid, LockdownSignal)> {
    w.config().guards.lockdowns_of(chat).iter().map(|l| (l.raise_event_id.unwrap(), l.signal)).collect()
}

/// Lockdowns x author-chosen ids x backdating: a Viewer who reuses the id
/// of an Admin's lockdown in a raise backdated before it must neither hide
/// the Admin's raise (its signal) nor let a clear that never named it lift
/// it. A clear lifts exactly the raises it names.
#[test]
fn a_reused_lockdown_id_never_hides_or_lifts_another_raise() {
    let mut w = world();
    let chat = w.chat("owner", "c");
    let l = Uuid::new_v4();
    let real = w.push_bare("ada", SessionEvent::ChatLockdownRaised { chat_id: chat, lockdown: lockdown(l, LockdownSignal::SecretInDiff, "ada") });
    assert_eq!(active_raises(&w, chat), vec![(real, LockdownSignal::SecretInDiff)]);
    // mal (a Viewer: may raise, may not clear) re-raises the same lockdown
    // id as a harmless manual lockdown, drafted before the Admin's raise.
    let fake = push_back(&mut w, "mal", SessionEvent::ChatLockdownRaised { chat_id: chat, lockdown: lockdown(l, LockdownSignal::Manual, "mal") }, 1, Proof::None);
    let shown = active_raises(&w, chat);
    assert!(shown.contains(&(real, LockdownSignal::SecretInDiff)), "the Admin's raise stays in force and visible: {shown:?}");
    // A Contributor clears what they were shown of mal's (its raise id only).
    if shown.iter().any(|(r, _)| *r == fake) {
        w.push("con", SessionEvent::ChatLockdownCleared { chat_id: chat, raises: vec![fake] });
    }
    let after = active_raises(&w, chat);
    assert!(after.contains(&(real, LockdownSignal::SecretInDiff)), "a clear that never named the Admin's raise lifted it: {after:?}");
    assert_eq!(w.verdict(real), Verdict::Allowed);
}

/// Lockdowns x copies: another signer's copy under a raise's event id
/// (rule 3 of `copies`: both fold) never takes the raise out of force.
#[test]
fn a_copy_under_a_raise_id_never_lifts_it() {
    let mut w = world();
    let chat = w.chat("owner", "c");
    let real = w.push_bare("ada", SessionEvent::ChatLockdownRaised { chat_id: chat, lockdown: lockdown(Uuid::new_v4(), LockdownSignal::SecretInDiff, "ada") });
    let orig = w.cfg.iter().find(|e| e.event_id == real).unwrap().clone();
    let mut copy: SessionEventEnvelope = w.draft("mal", None, SessionEvent::ChatLockdownRaised { chat_id: chat, lockdown: lockdown(Uuid::new_v4(), LockdownSignal::Manual, "mal") }, None);
    copy.event_id = real;
    copy.lamport = orig.lamport;
    if let Some(s) = copy.actor_stamp.as_mut() {
        s.log_head = orig.actor_stamp.as_ref().and_then(|o| o.log_head);
    }
    w.finish(&mut copy, "mal", None);
    w.push_env(None, copy);
    assert_eq!(w.verdict(real), Verdict::Allowed);
    let after = active_raises(&w, chat);
    assert!(after.contains(&(real, LockdownSignal::SecretInDiff)), "{after:?}");
}

/// Sign `env` (drafted for `by`) with another device of theirs, `device`.
fn resign_with(env: &mut SessionEventEnvelope, device: Uuid, key: &ligus_core::SigningKeypair) {
    if let Some(s) = env.actor_stamp.as_mut() {
        s.actor.device_id = Some(device);
    }
    env.clear_raw();
    ligus_core::sign_envelope(env, device, key);
}

/// A config event by `by` through `device` (not their testkit device),
/// with their recovery presence when `present`, its `prev` the device's
/// own previous event, drafted `back` events behind the head.
fn write_via(w: &mut SignedWorld, by: &str, device: Uuid, key: &ligus_core::SigningKeypair, payload: SessionEvent, present: bool, back: usize) -> Uuid {
    let mut env = w.draft(by, None, payload, None);
    ligus_core::lamport::stamp_back(&mut env, &w.cfg, back);
    let log_head = env.actor_stamp.as_ref().and_then(|s| s.log_head);
    let prev = w
        .cfg
        .iter()
        .filter(|e| e.signer_device_id == Some(device) && log_head.is_some_and(|h| (e.lamport, e.event_id) <= (h.lamport, h.event_id)))
        .max_by_key(|e| (e.lamport, e.event_id))
        .map(|e| e.event_id);
    if let Some(s) = env.actor_stamp.as_mut() {
        s.actor.device_id = Some(device);
        s.prev = prev;
        s.presence = None;
    }
    if present {
        let a = w.recovery_presence(by, &env);
        if let Some(s) = env.actor_stamp.as_mut() {
            s.presence = Some(a);
        }
    }
    resign_with(&mut env, device, key);
    w.push_env(None, env)
}

/// Account-key anchors x device revocation: the founder's anchor device
/// (the one that signed the seed) is stolen and revoked from the founder's
/// second device. The thief still holds its key and can publish, from it,
/// a certificate under a key of their own and a registration of that key,
/// backdated before the founder's genuine registration. Neither may move
/// the founder's account key, and the thief can't certify a new device for
/// the founder.
#[test]
fn a_revoked_anchor_device_cannot_re_anchor_its_account() {
    use ligus_core::crypto::{device_id_for, DeviceCertificate, SigAlg};
    let mut w = world();
    let owner_acct = w.people["owner"].account_id;
    let genuine = w.people["owner"].account.public_key_bytes().to_vec();
    let stolen = w.people["owner"].device_id;
    let stolen_key = ligus_core::SigningKeypair::from_seed(&w.people["owner"].device.seed_bytes()).unwrap();
    // The founder's second device, certified with their presence.
    let d2 = ligus_core::SigningKeypair::generate().unwrap();
    let d2_id = device_id_for(SigAlg::Ed25519, &d2.public_key_bytes());
    let cert2 = DeviceCertificate::issue(&w.people["owner"].account, owner_acct, &d2.public_key_bytes(), Timestamp::epoch());
    w.push("owner", SessionEvent::DeviceCertificateAdded { certificate: cert2 });
    assert!(w.roster().account_certified(owner_acct, d2_id));
    assert_eq!(w.roster().account_keys.get(&owner_acct), Some(&genuine), "before the attack");
    // Revoke the stolen anchor device from the second one: its watermark
    // covers everything it signed.
    let signers = [stolen].into_iter().collect();
    let wm = ligus_core::watermark::heads_of(&w.cfg, ligus_core::workspace_config_session_id(w.wid), &signers);
    let rev = write_via(&mut w, "owner", d2_id, &d2, SessionEvent::DeviceRevoked { device_id: stolen, watermark: wm }, true, 0);
    assert_eq!(w.verdict(rev), Verdict::Allowed, "the founder revokes the stolen device");
    assert_eq!(w.roster().account_keys.get(&owner_acct), Some(&genuine), "after the revocation");
    // The thief, with the stolen device's key, re-certifies it under a key
    // of their own and registers that key for the founder's account, both
    // drafted right after the seed (before the genuine registration).
    let thief = ligus_core::SigningKeypair::generate().unwrap();
    let depth = w.cfg.len();
    let fake_cert = DeviceCertificate::issue(&thief, owner_acct, &stolen_key.public_key_bytes(), Timestamp::epoch());
    let mut reg = w.draft("owner", None, SessionEvent::AccountKeyRegistered { account_id: owner_acct, signing_public_key: thief.public_key_bytes().to_vec() }, None);
    ligus_core::lamport::stamp_back(&mut reg, &w.cfg, depth);
    if let Some(s) = reg.actor_stamp.as_mut() {
        s.prev = None;
    }
    resign_with(&mut reg, stolen, &stolen_key);
    // A low event id so it sorts first among its lamport.
    let mut cert_env = w.draft("owner", None, SessionEvent::DeviceCertificateAdded { certificate: fake_cert }, None);
    ligus_core::lamport::stamp_back(&mut cert_env, &w.cfg, depth);
    if let Some(s) = cert_env.actor_stamp.as_mut() {
        s.prev = None;
    }
    cert_env.event_id = Uuid::nil();
    resign_with(&mut cert_env, stolen, &stolen_key);
    reg.event_id = Uuid::from_u128(1);
    resign_with(&mut reg, stolen, &stolen_key);
    w.push_env(None, cert_env);
    w.push_env(None, reg);
    let roster = w.roster();
    assert_eq!(w.verdict(rev), Verdict::Allowed, "the revocation stands: the thief's backdated events don't decide it");
    assert_eq!(w.verdict(Uuid::from_u128(1)), Verdict::Denied, "the revoked device's uncovered registration");
    assert_eq!(roster.account_keys.get(&owner_acct), Some(&genuine), "the founder's account key never moves to the thief's");
    assert!(roster.account_certified(owner_acct, d2_id), "the founder's second device stays certified");
    // The thief certifies a fresh device of theirs for the founder.
    let d3 = ligus_core::SigningKeypair::generate().unwrap();
    let d3_id = device_id_for(SigAlg::Ed25519, &d3.public_key_bytes());
    let cert3 = DeviceCertificate::issue(&thief, owner_acct, &d3.public_key_bytes(), Timestamp::epoch());
    write_via(&mut w, "owner", d3_id, &d3, SessionEvent::DeviceCertificateAdded { certificate: cert3 }, false, 0);
    assert!(!w.roster().account_certified(owner_acct, d3_id), "a device certified by the thief's key never acts for the founder");
    assert!(w.roster().members.contains_key("owner"));
}

/// Authorship x the fold: one member's certified device signs events
/// stamped as another member (their actor id and account, or their actor id
/// and the signer's own account). Ingest quarantines these; the fold must
/// not fold them as the victim's either (a store that took one in, a peer
/// path, a copy beside a real event).
#[test]
fn a_member_device_never_signs_as_another_member() {
    let mut w = world();
    let chat = w.chat("owner", "c");
    let mal = (w.people["mal"].device_id, ligus_core::SigningKeypair::from_seed(&w.people["mal"].device.seed_bytes()).unwrap());
    let raise = w.push_bare("ada", SessionEvent::ChatLockdownRaised { chat_id: chat, lockdown: lockdown(Uuid::new_v4(), LockdownSignal::SecretInDiff, "ada") });
    for own_account in [false, true] {
        // A chat message stamped as the Owner.
        let mut m = w.draft("owner", Some(chat), SessionEvent::MessageAppended { message: ligus_core::ChatMessage::new(ligus_core::MessageRole::User, "owner", "forged") }, None);
        if let Some(s) = m.actor_stamp.as_mut() {
            s.prev = None;
            if own_account {
                s.actor.account_id = Some(w.people["mal"].account_id);
            }
        }
        resign_with(&mut m, mal.0, &mal.1);
        let m = w.push_env(Some(chat), m);
        assert_eq!(w.verdict(m), Verdict::Denied, "a message signed by mal's device folded as the Owner's (own account: {own_account})");
        // A memory entry stamped as the Admin, on the config log.
        let rec = ligus_core::flow::FlowRecord::MemoryWritten(ligus_core::memory::MemoryWrite { entry_id: Uuid::new_v4(), title: "t".into(), body: "b".into(), ..Default::default() });
        let mut e = w.draft("ada", None, SessionEvent::FlowRecorded { record: rec }, None);
        if let Some(s) = e.actor_stamp.as_mut() {
            s.prev = None;
            if own_account {
                s.actor.account_id = Some(w.people["mal"].account_id);
            }
        }
        resign_with(&mut e, mal.0, &mal.1);
        let e = w.push_env(None, e);
        assert_eq!(w.verdict(e), Verdict::Denied, "a memory write signed by mal's device folded as the Admin's (own account: {own_account})");
    }
    assert_eq!(w.verdict(raise), Verdict::Allowed);
}

// ------------------------------------------------------------------------
// A combined random history.

mod combined {
    use super::*;
    use ligus_core::flow::FlowRecord;
    use ligus_core::memory::MemoryWrite;
    use ligus_core::{ChatMessage, MessageRole};
    use proptest::prelude::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::OnceLock;

    /// owner (Owner), ada (Admin), con and mal (Contributors; mal is the
    /// adversary), vic (Viewer), eve (a stranger who published an identity).
    const PEOPLE: [&str; 6] = ["owner", "ada", "con", "mal", "vic", "eve"];
    /// Who may clear a lockdown or write memory: human Contributors and up.
    const WRITERS: [&str; 4] = ["owner", "ada", "con", "mal"];

    fn base() -> &'static (SignedWorld, Uuid) {
        static BASE: OnceLock<(SignedWorld, Uuid)> = OnceLock::new();
        BASE.get_or_init(|| {
            let mut w = SignedWorld::genesis(&PEOPLE, None);
            w.admit("owner", "ada", WorkspaceRole::Admin);
            w.admit("owner", "con", WorkspaceRole::Contributor);
            w.admit("owner", "mal", WorkspaceRole::Contributor);
            w.admit("owner", "vic", WorkspaceRole::Viewer);
            w.publish_identity("eve");
            w.push("ada", SessionEvent::AttachmentPolicySet { policy: ligus_core::attachment::AttachmentPolicy::recommended() });
            let chat = w.chat("owner", "c");
            (w, chat)
        })
    }

    #[derive(Debug, Clone)]
    enum Op {
        Post { by: u8 },
        Answer { by: u8, to: u8 },
        Claim { by: u8, to: u8 },
        Raise { by: u8, id: u8 },
        Clear { by: u8, which: u8 },
        MemCreate { by: u8 },
        MemEdit { by: u8, which: u8 },
        Remove { who: u8, covered: bool },
        /// mal signs another payload under someone's event id.
        Copy { which: u16, cfg: bool },
        /// Someone signs a second, different envelope under their own id.
        Equivocate { which: u16 },
        /// A post drafted `back` events behind the chat's head.
        Backdated { by: u8, back: u8 },
        /// mal's device signs an event stamped as someone else.
        Forge { victim: u8, cfg: bool },
        /// Someone shares a file (on their own message, or none).
        Share { by: u8, on: Option<u8> },
        /// Someone deletes a shared file.
        Unshare { by: u8, which: u8 },
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => any::<u8>().prop_map(|by| Op::Post { by }),
            3 => (any::<u8>(), any::<u8>()).prop_map(|(by, to)| Op::Answer { by, to }),
            3 => (any::<u8>(), any::<u8>()).prop_map(|(by, to)| Op::Claim { by, to }),
            2 => (any::<u8>(), 0u8..3).prop_map(|(by, id)| Op::Raise { by, id }),
            2 => (any::<u8>(), any::<u8>()).prop_map(|(by, which)| Op::Clear { by, which }),
            2 => any::<u8>().prop_map(|by| Op::MemCreate { by }),
            2 => (any::<u8>(), any::<u8>()).prop_map(|(by, which)| Op::MemEdit { by, which }),
            1 => (any::<u8>(), any::<bool>()).prop_map(|(who, covered)| Op::Remove { who, covered }),
            2 => (any::<u16>(), any::<bool>()).prop_map(|(which, cfg)| Op::Copy { which, cfg }),
            1 => any::<u16>().prop_map(|which| Op::Equivocate { which }),
            2 => (any::<u8>(), 1u8..4).prop_map(|(by, back)| Op::Backdated { by, back }),
            1 => any::<u8>().prop_map(|by| Op::Post { by: by % 2 + 4 }),
            1 => (any::<u8>(), any::<bool>()).prop_map(|(victim, cfg)| Op::Forge { victim, cfg }),
            2 => (any::<u8>(), proptest::option::of(any::<u8>())).prop_map(|(by, on)| Op::Share { by, on }),
            1 => (any::<u8>(), any::<u8>()).prop_map(|(by, which)| Op::Unshare { by, which }),
        ]
    }

    fn pick<T: Clone>(xs: &[T], n: impl Into<u64>) -> Option<T> {
        (!xs.is_empty()).then(|| xs[(n.into() % xs.len() as u64) as usize].clone())
    }

    fn who(n: u8) -> &'static str {
        PEOPLE[n as usize % PEOPLE.len()]
    }

    struct Run {
        w: SignedWorld,
        chat: Uuid,
        users: Vec<Uuid>,
        raises: Vec<Uuid>,
        entries: Vec<Uuid>,
        /// Events that must never fold: forgeries.
        forged: Vec<Uuid>,
        lockdown_ids: [Uuid; 3],
        files: Vec<Uuid>,
    }

    fn msg(role: MessageRole, by: &str, body: String) -> ChatMessage {
        let mut m = ChatMessage::new(role, by, body);
        m.created_at = Timestamp::epoch();
        m
    }

    impl Run {
        fn apply(&mut self, op: &Op, n: usize) {
            let chat = self.chat;
            match *op {
                Op::Post { by } => {
                    let m = msg(MessageRole::User, who(by), format!("m{n}"));
                    self.users.push(m.id);
                    self.w.say(chat, who(by), SessionEvent::MessageAppended { message: m });
                }
                Op::Answer { by, to } => {
                    let Some(t) = pick(&self.users, to) else { return };
                    let by = who(by);
                    let mut m = msg(MessageRole::Assistant, by, format!("a{n}"));
                    m.reply_to_message_id = Some(t);
                    m.origin_device_id = Some(self.w.people[by].device_id);
                    self.w.say(chat, by, SessionEvent::MessageAppended { message: m });
                }
                Op::Claim { by, to } => {
                    let Some(t) = pick(&self.users, to) else { return };
                    let by = who(by);
                    let device_id = self.w.people[by].device_id;
                    self.w.say(chat, by, SessionEvent::TurnClaimed { trigger_id: t, device_id, epoch: 1, supersedes: None, repo_id: None });
                }
                Op::Raise { by, id } => {
                    let l = lockdown(self.lockdown_ids[id as usize], LockdownSignal::SecretInDiff, who(by));
                    let id = self.w.push_bare(who(by), SessionEvent::ChatLockdownRaised { chat_id: chat, lockdown: l });
                    self.raises.push(id);
                }
                Op::Clear { by, which } => {
                    let Some(r) = pick(&self.raises, which) else { return };
                    self.w.push(who(by), SessionEvent::ChatLockdownCleared { chat_id: chat, raises: vec![r] });
                }
                Op::MemCreate { by } => {
                    let rec = FlowRecord::MemoryWritten(MemoryWrite { entry_id: Uuid::new_v4(), title: format!("t{n}"), body: format!("b{n}"), ..Default::default() });
                    let id = self.w.push_bare(who(by), SessionEvent::FlowRecorded { record: rec });
                    self.entries.push(id);
                }
                Op::MemEdit { by, which } => {
                    let Some(e) = pick(&self.entries, which) else { return };
                    let m = self.w.config().flow.memory;
                    let Some(head) = m.entries.get(&e).map(|x| x.head()) else { return };
                    let rec = FlowRecord::MemoryWritten(MemoryWrite { entry_id: e, base: Some(head), title: format!("e{n}"), body: format!("e{n}"), ..Default::default() });
                    self.w.push_bare(who(by), SessionEvent::FlowRecorded { record: rec });
                }
                Op::Remove { who: x, covered } => {
                    let target = ["con", "mal", "vic"][x as usize % 3];
                    if !self.w.config().members.iter().any(|m| m.id == target) {
                        return;
                    }
                    let watermark = if covered {
                        let signers: BTreeSet<Uuid> = [self.w.people[target].device_id].into_iter().collect();
                        let mut wm = ligus_core::watermark::heads_of(&self.w.cfg, ligus_core::workspace_config_session_id(self.w.wid), &signers);
                        wm.extend(ligus_core::watermark::heads_of(&self.w.chats[&chat], chat, &signers));
                        wm
                    } else {
                        Vec::new()
                    };
                    self.w.push("owner", SessionEvent::MemberRemoved { member_id: target.into(), watermark });
                }
                Op::Copy { which, cfg } => {
                    let log = if cfg { &self.w.cfg } else { &self.w.chats[&chat] };
                    let Some(orig) = pick(&log.iter().skip(1).filter(|e| e.signer_device_id != Some(self.w.people["mal"].device_id)).cloned().collect::<Vec<_>>(), which) else { return };
                    let payload = if cfg {
                        SessionEvent::ChatLockdownRaised { chat_id: chat, lockdown: lockdown(Uuid::new_v4(), LockdownSignal::Manual, "mal") }
                    } else {
                        SessionEvent::MessageAppended { message: msg(MessageRole::User, "mal", format!("copy{n}")) }
                    };
                    let target = if cfg { None } else { Some(chat) };
                    let mut c = self.w.draft("mal", target, payload, None);
                    c.event_id = orig.event_id;
                    c.lamport = orig.lamport;
                    if let Some(s) = c.actor_stamp.as_mut() {
                        s.log_head = orig.actor_stamp.as_ref().and_then(|o| o.log_head);
                        s.prev = None;
                    }
                    self.w.finish(&mut c, "mal", None);
                    self.w.push_env(target, c);
                }
                Op::Equivocate { which } => {
                    let log = &self.w.chats[&chat];
                    let Some(orig) = pick(&log.iter().skip(1).cloned().collect::<Vec<_>>(), which) else { return };
                    let Some(by) = PEOPLE.iter().find(|p| Some(self.w.people[**p].device_id) == orig.signer_device_id) else { return };
                    let mut twin = self.w.draft(by, Some(chat), SessionEvent::MessageAppended { message: msg(MessageRole::User, by, format!("twin{n}")) }, None);
                    twin.event_id = orig.event_id;
                    twin.lamport = orig.lamport;
                    if let Some(s) = twin.actor_stamp.as_mut() {
                        let o = orig.actor_stamp.as_ref();
                        s.log_head = o.and_then(|o| o.log_head);
                        s.prev = o.and_then(|o| o.prev);
                        s.cfg_frontier = o.and_then(|o| o.cfg_frontier);
                    }
                    self.w.finish(&mut twin, by, None);
                    self.w.push_env(Some(chat), twin);
                }
                Op::Backdated { by, back } => {
                    let by = who(by);
                    let m = msg(MessageRole::User, by, format!("old{n}"));
                    let id = m.id;
                    let mut env = self.w.draft(by, Some(chat), SessionEvent::MessageAppended { message: m }, None);
                    let log = self.w.chats[&chat].clone();
                    ligus_core::lamport::stamp_back(&mut env, &log, back as usize);
                    let head = env.actor_stamp.as_ref().and_then(|s| s.log_head);
                    let device = self.w.people[by].device_id;
                    let prev = log
                        .iter()
                        .filter(|e| e.signer_device_id == Some(device) && head.is_some_and(|h| (e.lamport, e.event_id) <= (h.lamport, h.event_id)))
                        .max_by_key(|e| (e.lamport, e.event_id))
                        .map(|e| e.event_id);
                    if let Some(s) = env.actor_stamp.as_mut() {
                        s.prev = prev;
                    }
                    self.w.finish(&mut env, by, None);
                    self.w.push_env(Some(chat), env);
                    self.users.push(id);
                }
                Op::Share { by, on } => {
                    let by = who(by);
                    let blob_id = format!("{:b>43}", Uuid::new_v4().simple());
                    let a = ligus_core::attachment::AttachmentRef {
                        id: ligus_core::attachment::share_id(self.w.people[by].device_id, &blob_id),
                        blob_id,
                        sha256: "1111111111111111111111111111111111111111111111111111111111111111".into(),
                        size: 10,
                        content_type: "application/octet-stream".into(),
                        name: format!("f{n}.txt"),
                        message_id: on.and_then(|i| pick(&self.users, i)),
                        proposal_id: None,
                    };
                    self.files.push(a.id);
                    self.w.say(chat, by, SessionEvent::AttachmentShared { attachment: a });
                }
                Op::Unshare { by, which } => {
                    let Some(f) = pick(&self.files, which) else { return };
                    self.w.say(chat, who(by), SessionEvent::AttachmentDeleted { attachment_id: f });
                }
                Op::Forge { victim, cfg } => {
                    let victim = ["owner", "ada", "con", "vic"][victim as usize % 4];
                    let target = if cfg { None } else { Some(chat) };
                    let payload = if cfg {
                        SessionEvent::ChatLockdownCleared { chat_id: chat, raises: self.raises.last().copied().into_iter().collect() }
                    } else {
                        SessionEvent::MessageAppended { message: msg(MessageRole::User, victim, format!("forged{n}")) }
                    };
                    if cfg && self.raises.is_empty() {
                        return;
                    }
                    let mut env = self.w.draft(victim, target, payload, None);
                    let mal = &self.w.people["mal"];
                    let (d, k) = (mal.device_id, ligus_core::SigningKeypair::from_seed(&mal.device.seed_bytes()).unwrap());
                    if let Some(s) = env.actor_stamp.as_mut() {
                        s.prev = None;
                    }
                    resign_with(&mut env, d, &k);
                    // The victim's presence can't be had; mal's own is over
                    // another account.
                    self.forged.push(env.event_id);
                    self.w.push_env(target, env);
                }
            }
        }
    }

    fn shuffled(log: &[SessionEventEnvelope], seed: u64) -> Vec<SessionEventEnvelope> {
        let mut out = log.to_vec();
        let mut s = seed | 1;
        for i in (1..out.len()).rev() {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            out.swap(i, (s % (i as u64 + 1)) as usize);
        }
        out
    }

    /// Config and chat verdicts, both folds (as JSON) and the held events.
    type Outcome = (BTreeMap<Uuid, Verdict>, BTreeMap<Uuid, Verdict>, serde_json::Value, serde_json::Value, Vec<Uuid>);

    /// Everything the folds decide, in an order-free form.
    fn outcome(cfg: &[SessionEventEnvelope], chat: &[SessionEventEnvelope]) -> Outcome {
        let (c, t) = ligus_core::events::project_config(cfg, None, None);
        let cv = ligus_core::events::fold_decisions(cfg, None, None, ligus_core::WorkspaceAuthz::Unchecked);
        let hv = ligus_core::events::fold_decisions(chat, None, None, ligus_core::WorkspaceAuthz::Timeline(&t));
        let s = ligus_core::events::project_authorized(chat, None, None, ligus_core::WorkspaceAuthz::Timeline(&t)).unwrap();
        let mut held: Vec<Uuid> = s.authz.held.iter().map(|h| h.event_id).collect();
        held.sort();
        (cv, hv, serde_json::to_value(c.unwrap()).unwrap(), serde_json::to_value(&s).unwrap(), held)
    }

    proptest! {
        // 64 cases by default (each folds a few dozen signed events several
        // times); PROPTEST_CASES overrides it (4096 for the release gate).
        #![proptest_config(ProptestConfig { cases: std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(64), ..ProptestConfig::default() })]

        #[test]
        fn prop_a_mixed_adversarial_history_converges_and_keeps_the_invariants(ops in prop::collection::vec(op(), 1..28), seed in any::<u64>()) {
            let (w, chat) = base().clone();
            let mut run = Run { w, chat, users: Vec::new(), raises: Vec::new(), entries: Vec::new(), forged: Vec::new(), lockdown_ids: [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()], files: Vec::new() };
            for (n, op) in ops.iter().enumerate() {
                run.apply(op, n);
            }
            let (cfg, log) = (run.w.cfg.clone(), run.w.chats[&chat].clone());
            let here = outcome(&cfg, &log);
            // 1. Any arrival order folds alike.
            let there = outcome(&shuffled(&cfg, seed), &shuffled(&log, seed.rotate_left(17)));
            prop_assert_eq!(&here.0, &there.0, "config verdicts depend on arrival order");
            prop_assert_eq!(&here.1, &there.1, "chat verdicts depend on arrival order");
            prop_assert!(here.2 == there.2, "the config fold depends on arrival order");
            prop_assert!(here.3 == there.3, "the chat fold depends on arrival order");
            prop_assert_eq!(&here.4, &there.4);
            let cv = &here.0;
            // 2. No forged authorship folds, and 3. nothing a non-member
            //    signed does (beyond publishing their own identity). Judged
            //    per envelope: another signer's copy under the id is its own.
            let t0 = run.w.timeline();
            let cc = ligus_core::events::fold_copy_decisions(&cfg, None, None, ligus_core::WorkspaceAuthz::Unchecked);
            let hc = ligus_core::events::fold_copy_decisions(&log, None, None, ligus_core::WorkspaceAuthz::Timeline(&t0));
            let eve = run.w.people["eve"].device_id;
            for (e, v) in cfg.iter().map(|e| (e, cc.get(e))).chain(log.iter().map(|e| (e, hc.get(e)))) {
                if run.forged.contains(&e.event_id) && e.signer_device_id == Some(run.w.people["mal"].device_id) {
                    prop_assert!(v != Some(Verdict::Allowed), "a forgery folded: {:?}", e.payload.kind_str());
                }
                let publication = matches!(e.payload, SessionEvent::AccountKeyRegistered { .. } | SessionEvent::DeviceCertificateAdded { .. } | SessionEvent::PasskeyRegistered { .. });
                if e.signer_device_id == Some(eve) && !publication {
                    prop_assert!(v != Some(Verdict::Allowed), "a stranger's {} folded", e.payload.kind_str());
                }
            }
            let t = run.w.timeline();
            let s = ligus_core::events::project_authorized(&log, None, None, ligus_core::WorkspaceAuthz::Timeline(&t)).unwrap();
            let config = run.w.config();
            // 4. At most one committed answer per turn and responder.
            let mut committed: BTreeMap<(Uuid, Option<Uuid>), usize> = BTreeMap::new();
            for m in s.messages.iter().filter(|m| ligus_core::dispatch::is_answer(m) && !m.superseded) {
                let trigger = ligus_core::dispatch::trigger_of(&s, m.reply_to_message_id.unwrap());
                *committed.entry((trigger, m.responder_id)).or_default() += 1;
            }
            prop_assert!(committed.values().all(|n| *n <= 1), "two committed answers to one turn: {:?}", committed);
            // 5. A raise out of force was named by an allowed clear of a
            //    human Contributor or above.
            let active: BTreeSet<Uuid> = config.guards.lockdowns_of(chat).iter().filter_map(|l| l.raise_event_id).collect();
            for r in run.raises.iter().filter(|r| cv.get(r) == Some(&Verdict::Allowed)) {
                if active.contains(r) {
                    continue;
                }
                let named = cfg.iter().any(|e| {
                    matches!(&e.payload, SessionEvent::ChatLockdownCleared { raises, .. } if raises.contains(r))
                        && cv.get(&e.event_id) == Some(&Verdict::Allowed)
                        && e.actor_stamp.as_ref().is_some_and(|s| WRITERS.contains(&s.actor.id.as_str()))
                });
                prop_assert!(named, "raise {} was lifted without a clear naming it", r);
            }
            // 7. A shared file is its signer's: a member's, from a device of
            //    that member's account, under the id bound to that device.
            for (id, a) in &s.attachments {
                let p = &run.w.people[a.author.as_str()];
                prop_assert_eq!(a.signer_device, Some(p.device_id));
                prop_assert_eq!(*id, ligus_core::attachment::share_id(p.device_id, &a.attachment.blob_id));
                prop_assert!(a.author != "eve");
                if let Some(m) = a.attachment.message_id {
                    prop_assert_eq!(s.authz.message_authors.get(&m), Some(&a.author), "a file on someone else's message");
                }
            }
            // 6. Memory is written by Contributors and up only, and a new
            //    entry is its creating event's.
            for (id, e) in &config.flow.memory.entries {
                prop_assert!(WRITERS.contains(&e.created_by.id.as_str()), "memory by {}", e.created_by.id);
                prop_assert_eq!(cv.get(id), Some(&Verdict::Allowed));
            }
        }
    }
}
