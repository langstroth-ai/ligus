//! Adversarial review of fix/held-and-dispute: each attack here failed
//! before its fix (prove, then fix).

use ligus_core::crypto::{sign_envelope, DeviceCertificate, SigningKeypair};
use ligus_core::testkit::SignedWorld;
use ligus_core::{CfgFrontier, ChatSession, SessionEvent, SessionEventEnvelope, WorkspaceAuthz, WorkspaceRole};
use ligus_runtime::event_store::EventStore;
use ligus_runtime::relay_client::RelayClient;
use ligus_runtime::sync_engine::SyncEngine;
use serde_json::Value;
use uuid::Uuid;

fn key() -> [u8; 32] {
    static KEY: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    *KEY.get_or_init(|| ligus_core::derive_workspace_key("held dispute review"))
}

struct Device {
    store: EventStore,
    engine: SyncEngine,
    relay: Vec<(u64, Value)>,
}

impl Device {
    fn new(w: &SignedWorld) -> Self {
        let engine = SyncEngine::new(RelayClient::new("http://127.0.0.1:0"), Uuid::new_v4().to_string())
            .with_room_workspace(w.wid)
            .with_key(key());
        Self { store: EventStore::open_in_memory().unwrap(), engine, relay: Vec::new() }
    }

    fn deliver(&mut self, envs: &[SessionEventEnvelope]) {
        let start = self.relay.len() as u64;
        for (e, seq) in envs.iter().zip(start + 1..) {
            let sealed = ligus_core::seal_symmetric(&key(), 0, &serde_json::to_vec(e).unwrap()).unwrap();
            self.relay.push((seq, serde_json::to_value(sealed).unwrap()));
        }
        let cursor = self.engine.cursor();
        let page: Vec<(u64, Value)> = self.relay.iter().filter(|(s, _)| *s > cursor).cloned().collect();
        if !page.is_empty() {
            self.engine.apply_fetched(&mut self.store, &page).unwrap();
        }
    }

    /// What `SyncEngine::refetch_dropped` does against a relay: fetch the
    /// due sequences again and judge them.
    fn refetch(&mut self) -> usize {
        let due = self.engine.refetch_due(&self.store).unwrap();
        let rows: Vec<(u64, Value)> = self.relay.iter().filter(|(s, _)| due.contains(s)).cloned().collect();
        self.engine.apply_refetched(&mut self.store, &due, &rows).unwrap()
    }

    fn chat(&self, w: &SignedWorld, chat: Uuid) -> Option<ChatSession> {
        let (_, t) = self.store.load_config(w.wid, None, None).unwrap();
        self.store.load_session_authorized(chat, None, None, WorkspaceAuthz::Timeline(&t)).unwrap()
    }

    fn roster(&self, w: &SignedWorld) -> ligus_core::authz::RosterState {
        self.store.load_config(w.wid, None, None).unwrap().1.current().clone()
    }

    fn ingest_roster(&self, w: &SignedWorld) -> ligus_runtime::envelope_verifier::WorkspaceRoster {
        let mut r = ligus_runtime::envelope_verifier::build_rosters(&self.store.trust_envelopes(w.wid).unwrap(), &Default::default());
        r.remove(&w.wid).unwrap()
    }
}

fn title(t: &str) -> SessionEvent {
    SessionEvent::SessionTitleChanged { title: t.into() }
}

/// `payload` on `chat` (or the config log) stamped as `as_who`, signed by
/// `by`'s device; with `log_head` set, at that head and a small event id.
fn forge(w: &mut SignedWorld, by: &str, as_who: &str, chat: Option<Uuid>, payload: SessionEvent, log_head: Option<Option<CfgFrontier>>) -> SessionEventEnvelope {
    let mut env = w.draft(by, chat, payload, None);
    let p = &w.people[by];
    if let Some(s) = env.actor_stamp.as_mut() {
        let mut actor = w.people[as_who].actor();
        actor.device_id = Some(p.device_id);
        s.actor = actor;
        s.prev = None;
        if let Some(h) = log_head {
            s.log_head = h;
        }
    }
    if let Some(h) = log_head {
        env.lamport = ligus_core::lamport::next(h);
        env.event_id = Uuid::from_u128(1 + env.event_id.as_u128() % 1000);
    }
    env.clear_raw();
    sign_envelope(&mut env, p.device_id, &p.device);
    env
}

/// owner founds; eve is an Admin-added Contributor who attacks; con is an
/// Admin-added Contributor. One chat.
/// The owner's name is their account id (as on a real log), so a device
/// can pin them as the founder.
fn world() -> (SignedWorld, Uuid, String) {
    let id = Uuid::new_v4();
    let owner = id.to_string();
    let people = vec![ligus_core::testkit::Person::new(&owner).with_account_id(id), ligus_core::testkit::Person::new("con"), ligus_core::testkit::Person::new("eve")];
    let mut w = SignedWorld::genesis_of(people, None);
    w.admit(&owner, "con", WorkspaceRole::Contributor);
    w.admit(&owner, "eve", WorkspaceRole::Contributor);
    let chat = w.chat(&owner, "c");
    w.say(chat, "con", title("con was here"));
    (w, chat, owner)
}

/// A device of the workspace, pinning the founder when `pinned`.
fn device(w: &SignedWorld, owner: &str, pinned: bool) -> Device {
    let d = Device::new(w);
    if pinned {
        let fk = ligus_runtime::envelope_verifier::FounderPin::fingerprint_hex(&w.people[owner].account.public_key_bytes());
        d.store.set_workspace_pin(w.wid, owner, &fk).unwrap();
    }
    d
}

/// eve's self-evident identity for `victim`'s account (her own device,
/// certified under a key of hers, registered by that device), stamped as the
/// victim, backdated to `head`.
fn self_evident_forgery(w: &mut SignedWorld, victim: &str, head: Option<Option<CfgFrontier>>) -> (SigningKeypair, Vec<SessionEventEnvelope>) {
    let account = w.people[victim].account_id;
    let k = SigningKeypair::generate().unwrap();
    let c = DeviceCertificate::issue(&k, account, &w.people["eve"].device.public_key_bytes(), ligus_core::Timestamp::epoch());
    let cert = forge(w, "eve", victim, None, SessionEvent::DeviceCertificateAdded { certificate: c }, head);
    let reg = forge(w, "eve", victim, None, SessionEvent::AccountKeyRegistered { account_id: account, signing_public_key: k.public_key_bytes().to_vec() }, head);
    (k, vec![cert, reg])
}

/// Finding: a competing founding seed. The batch fixpoint lets a founding
/// seed join when its signer "vouches for" the founder's account itself (its
/// own certificate under a key it registered), and anyone's device can
/// vouch for any account id that way. eve publishes a self-evident identity
/// for the owner's account and a seed stamped as the owner at lamport 1 with
/// a smaller event id: it became the roster's base, anchoring the owner's
/// account to eve's device (her key; she signs as the Owner).
#[test]
fn a_competing_founding_seed_never_anchors_the_founder_to_someone_else() {
    let (mut w, chat, owner_name) = world();
    let o = owner_name.as_str();
    let (_k, mut attack) = self_evident_forgery(&mut w, o, None);
    let seed_payload = w.cfg[0].payload.clone();
    attack.push(forge(&mut w, "eve", o, None, seed_payload, Some(None)));
    let as_owner = forge(&mut w, "eve", o, Some(chat), title("eve as the owner"), None);
    let honest = w.say(chat, o, title("the owner after"));
    let honest_env: Vec<SessionEventEnvelope> = w.chats[&chat].iter().filter(|e| e.event_id == honest).cloned().collect();
    // Unpinned with the attack first is the B2 residual (pre-existing: a
    // device with no founder pin takes the first self-consistent seed it can
    // verify); every device pins its founder (creating, joining, or TOFU on
    // upgrade).
    for (attack_first, pinned) in [(false, true), (true, true), (false, false)] {
        let mut d = device(&w, o, pinned);
        if attack_first {
            d.deliver(&attack);
            d.deliver(std::slice::from_ref(&as_owner));
            d.deliver(&w.cfg);
            d.deliver(&w.chats[&chat][..w.chats[&chat].len() - 1]);
        } else {
            d.deliver(&w.cfg);
            d.deliver(&w.chats[&chat][..w.chats[&chat].len() - 1]);
            let mut batch = attack.clone();
            batch.push(as_owner.clone());
            d.deliver(&batch);
        }
        d.deliver(&honest_env);
        let owner = w.people[o].account_id;
        let r = d.ingest_roster(&w);
        let case = format!("attack_first={attack_first} pinned={pinned}");
        assert_eq!(r.account_signing_key(owner), Some(&w.people[o].account.public_key_bytes()[..]), "{case}");
        assert!(!d.store.has_event(as_owner.event_id).unwrap(), "eve never signs as the owner ({case})");
        assert!(d.store.has_event(honest).unwrap(), "the owner's events keep applying ({case})");
        assert_eq!(d.chat(&w, chat).unwrap().title, "the owner after", "{case}");
        assert_eq!(d.roster(&w).account_keys.get(&owner), Some(&w.people[o].account.public_key_bytes().to_vec()), "{case}");
    }
}

/// Finding: a replayed rotation disputes the account. An account that
/// rotated twice (K0 -> K1 -> K2) has K0's signature over K1 on the log; anyone
/// can publish that rotation again (a new event, stamped as the account): K0
/// "designated another successor" in the fork rule, though K1 is the very
/// successor it already designated. On rules v2 the ingest roster counted
/// rotations of an account without a passkey (a box) that the config fold
/// denies (every rotation needs presence there), so the account was
/// disputed at ingest and every event its owner signed was held, while the
/// fold kept K0.
#[test]
fn a_replayed_rotation_never_disputes_the_account() {
    let (mut w, chat, owner) = world();
    // bo: a member with no passkey (as a box is).
    let bo = ligus_core::testkit::Person::new("bo");
    std::sync::Arc::get_mut(&mut w.people).unwrap().insert("bo".into(), bo);
    let m = w.member_of("bo", WorkspaceRole::Contributor);
    w.push(&owner, SessionEvent::MemberAdded { member: m, invite: None, proof: None });
    w.publish_identity("bo");
    let account = w.people["bo"].account_id;
    let device_key = w.people["bo"].device.public_key_bytes();
    let k0 = SigningKeypair::from_seed(&w.people["bo"].account.seed_bytes()).unwrap();
    let (k1, k2) = (SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap());
    let mut first = None;
    for (from, to) in [(&k0, &k1), (&k1, &k2)] {
        let sig = from.sign(&to.public_key_bytes()).to_vec();
        let rot = SessionEvent::AccountKeyRotated { account_id: account, new_signing_public_key: to.public_key_bytes().to_vec(), succession_signature: sig };
        first.get_or_insert(rot.clone());
        w.push_bare("bo", rot);
        let cert = DeviceCertificate::issue(to, account, &device_key, ligus_core::Timestamp::epoch());
        w.push_bare("bo", SessionEvent::DeviceCertificateAdded { certificate: cert });
    }
    let replay = forge(&mut w, "eve", "bo", None, first.unwrap(), None);
    let after = w.say(chat, "bo", title("bo after the replay"));
    let mut d = device(&w, &owner, true);
    d.deliver(&w.cfg);
    d.deliver(&[replay]);
    d.deliver(&w.chats[&chat]);
    let r = d.ingest_roster(&w);
    assert!(!r.is_disputed(account), "a replay of the account's own rotation disputes nothing");
    assert!(d.store.has_event(after).unwrap(), "bo's events keep applying");
    // Ingest and the fold agree on bo's key.
    assert_eq!(r.account_signing_key(account).map(<[u8]>::to_vec), d.roster(&w).account_keys.get(&account).cloned());
}

/// Finding: the anchor without its key. vic's admission anchors her account
/// to her device, but until her own certificate is on the device (it follows
/// the admission on the relay, so a page can end between them, or it sits on
/// a chat log the config fold doesn't read), the anchored key was unknown and
/// the earliest self-evident registration pinned the account: eve's, made in
/// advance. Her events stamped as vic were applied on a device whose page
/// ended there.
#[test]
fn an_anchor_whose_key_has_not_arrived_pins_nothing_else() {
    let (mut w, chat, owner) = world();
    let vic = ligus_core::testkit::Person::new("vic");
    std::sync::Arc::get_mut(&mut w.people).unwrap().insert("vic".into(), vic);
    // eve's identity for vic, and an event as vic, before vic ever joins.
    let (_k, mut attack) = self_evident_forgery(&mut w, "vic", None);
    attack.push(forge(&mut w, "eve", "vic", Some(chat), title("eve as vic"), None));
    let invite = Uuid::new_v4();
    w.push(&owner, SessionEvent::JoinApproved { subject: ligus_core::authorization::invite_subject(invite), admission: None, redeem_key: None });
    let m = w.member_of("vic", WorkspaceRole::Contributor);
    let admission = w.push_bare("vic", SessionEvent::MemberAdded { member: m, invite: Some(invite), proof: None });
    w.publish_identity("vic");
    let mine = w.say(chat, "vic", title("vic herself"));
    let cut = w.cfg.iter().position(|e| e.event_id == admission).unwrap() + 1;
    for (by_page, on_config) in [(true, true), (false, true), (true, false)] {
        let mut d = device(&w, &owner, true);
        d.deliver(&w.chats[&chat][..1]);
        d.deliver(&attack);
        let mut cfg = w.cfg.clone();
        if !on_config {
            // vic's identity reaches this device only on a chat log.
            for e in cfg.drain(cut..) {
                let mut e = e;
                if matches!(e.payload, SessionEvent::AccountKeyRegistered { .. } | SessionEvent::DeviceCertificateAdded { .. }) {
                    e.session_id = chat;
                    e.clear_raw();
                    let p = &w.people["vic"];
                    sign_envelope(&mut e, p.device_id, &p.device);
                    d.deliver(&[e]);
                }
            }
        }
        if by_page && on_config {
            d.deliver(&cfg[..cut]);
            d.deliver(&cfg[cut..]);
        } else {
            d.deliver(&cfg);
        }
        d.deliver(&w.chats[&chat][1..]);
        let case = format!("by_page={by_page} on_config={on_config}");
        let account = w.people["vic"].account_id;
        assert!(!d.store.has_event(attack[2].event_id).unwrap(), "eve never signs as vic ({case})");
        let r = d.ingest_roster(&w);
        assert_ne!(r.account_signing_key(account), Some(&_k.public_key_bytes()[..]), "{case}");
        assert_ne!(d.roster(&w).account_keys.get(&account), Some(&_k.public_key_bytes().to_vec()), "the fold never takes eve's key for vic ({case})");
        if on_config {
            assert!(d.store.has_event(mine).unwrap(), "{case}");
            assert_eq!(r.account_signing_key(account), Some(&w.people["vic"].account.public_key_bytes()[..]), "{case}");
        }
    }
}

/// `n` events from `n` fresh devices (one each), stamped as `as_who`, on
/// `chat`: the cheapest flood of the held table (no per-signer cap applies).
fn sybil_flood(w: &mut SignedWorld, as_who: &str, chat: Uuid, n: usize) -> Vec<SessionEventEnvelope> {
    let template = forge(w, "eve", as_who, Some(chat), title("sybil"), None);
    (0..n)
        .map(|i| {
            let k = SigningKeypair::generate().unwrap();
            let id = ligus_core::crypto::device_id_for(ligus_core::crypto::SigAlg::Ed25519, &k.public_key_bytes());
            let mut e = template.clone();
            e.event_id = Uuid::new_v4();
            e.payload = title(&format!("sybil {i}"));
            if let Some(s) = e.actor_stamp.as_mut() {
                s.actor.device_id = Some(id);
            }
            e.clear_raw();
            sign_envelope(&mut e, id, &k);
            e
        })
        .collect()
}

/// Finding: a flood evicts honest held events for good. A member fills the
/// workspace's held table with one event each from 10,000 fresh devices; an
/// honest event that has to wait (here: vic's chat message, which reached
/// this device before the config log that admits her) is dropped, and when
/// the config log arrives this device never applies it ("lost until a full
/// resync"), while every other device does. Dropped events are fetched again
/// from the relay once what they waited for arrives.
#[test]
fn a_flood_never_loses_an_honest_held_event() {
    let (mut w, chat, owner) = world();
    let vic = ligus_core::testkit::Person::new("vic");
    std::sync::Arc::get_mut(&mut w.people).unwrap().insert("vic".into(), vic);
    let invite = Uuid::new_v4();
    w.push(&owner, SessionEvent::JoinApproved { subject: ligus_core::authorization::invite_subject(invite), admission: None, redeem_key: None });
    let m = w.member_of("vic", WorkspaceRole::Contributor);
    w.push_bare("vic", SessionEvent::MemberAdded { member: m, invite: Some(invite), proof: None });
    w.publish_identity("vic");
    let hers = w.say(chat, "vic", title("vic was here"));
    let flood = sybil_flood(&mut w, "con", chat, ligus_runtime::event_store::HELD_PER_WORKSPACE as usize);
    let mut d = device(&w, &owner, true);
    d.deliver(&flood);
    d.deliver(&w.chats[&chat]);
    assert!(!d.store.has_event(hers).unwrap());
    d.deliver(&w.cfg);
    d.engine.retry_held_now(&mut d.store).unwrap();
    // What it waited for arrived: fetched again from the relay (once), and
    // the flood from devices nobody certified is not.
    let stats = d.store.held_stats(w.wid).unwrap();
    assert!(stats.refetch > 0, "{stats:?}");
    assert_eq!(d.engine.refetch_due(&d.store).unwrap().len(), stats.refetch.min(3) as usize, "only vic's dropped events are due: {stats:?}");
    d.refetch();
    assert!(d.store.has_event(hers).unwrap(), "vic's message is applied once her admission arrives: {:?}", d.store.held_stats(w.wid).unwrap());
    assert_eq!(d.chat(&w, chat).unwrap().title, "vic was here");
    let after = d.store.held_stats(w.wid).unwrap();
    assert_eq!(after.lost, 0, "{after:?}");
    assert!(d.engine.refetch_due(&d.store).unwrap().is_empty());
}


fn shuffle<T>(v: &mut [T], mut s: u64) {
    for i in (1..v.len()).rev() {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        v.swap(i, (s >> 33) as usize % (i + 1));
    }
}

/// A world for the agreement property: owner founds (pinned), con and eve
/// are Admin-added, vic joins through a keyed invite, bo (no passkey) rotates
/// twice. One chat.
fn agreement_world() -> (SignedWorld, Uuid, String, Vec<SessionEventEnvelope>) {
    let (mut w, chat, owner) = world();
    for n in ["vic", "bo"] {
        let p = ligus_core::testkit::Person::new(n);
        std::sync::Arc::get_mut(&mut w.people).unwrap().insert(n.into(), p);
    }
    let invite = Uuid::new_v4();
    let k = ligus_core::admission::RedeemKey::generate().unwrap();
    w.push(&owner, SessionEvent::JoinApproved { subject: ligus_core::authorization::invite_subject(invite), admission: None, redeem_key: Some(k.public().to_vec()) });
    let m = w.member_of("vic", WorkspaceRole::Contributor);
    let proof = k.prove_person(w.wid, invite, &m.id, w.people["vic"].device_id);
    w.push_bare("vic", SessionEvent::MemberAdded { member: m, invite: Some(invite), proof: Some(proof) });
    w.publish_identity("vic");
    let m = w.member_of("bo", WorkspaceRole::Contributor);
    w.push(&owner, SessionEvent::MemberAdded { member: m, invite: None, proof: None });
    w.publish_identity("bo");
    let account = w.people["bo"].account_id;
    let k0 = SigningKeypair::from_seed(&w.people["bo"].account.seed_bytes()).unwrap();
    let k1 = SigningKeypair::generate().unwrap();
    let rot = SessionEvent::AccountKeyRotated { account_id: account, new_signing_public_key: k1.public_key_bytes().to_vec(), succession_signature: k0.sign(&k1.public_key_bytes()).to_vec() };
    w.push_bare("bo", rot.clone());
    w.say(chat, "vic", title("vic was here"));
    // The attacks, all by eve: for each victim, a self-evident identity late
    // and backdated to the log's start, a bare registration, and a replay of
    // bo's rotation.
    let mut attacks = Vec::new();
    for victim in [owner.as_str(), "vic", "con", "bo"] {
        attacks.extend(self_evident_forgery(&mut w, victim, None).1);
        attacks.extend(self_evident_forgery(&mut w, victim, Some(None)).1);
        let other = SigningKeypair::generate().unwrap();
        let account = w.people[victim].account_id;
        attacks.push(forge(&mut w, "eve", victim, None, SessionEvent::AccountKeyRegistered { account_id: account, signing_public_key: other.public_key_bytes().to_vec() }, None));
    }
    attacks.push(forge(&mut w, "eve", "bo", None, rot, None));
    (w, chat, owner, attacks)
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(16))]

    /// The three readers of account keys agree, whatever arrives first and
    /// whatever subset of the attacks is on the log: the ingest roster
    /// (`build_roster_pinned`), the config fold (`RosterTracker`) and the
    /// signer directory hold the same key for every account (a disputed one
    /// none), every member keeps their own key, and none of eve's forgeries
    /// counts.
    #[test]
    fn prop_the_readers_of_account_keys_agree(order in proptest::prelude::any::<u64>(), chunks in 1usize..5, pick in proptest::collection::vec(proptest::prelude::any::<bool>(), 15)) {
        let (w, chat, owner, attacks) = agreement_world();
        let attacks: Vec<SessionEventEnvelope> = attacks.into_iter().zip(pick.iter().cycle()).filter(|(_, p)| **p).map(|(a, _)| a).collect();
        let mut all: Vec<SessionEventEnvelope> = w.cfg.iter().chain(&w.chats[&chat]).chain(&attacks).cloned().collect();
        shuffle(&mut all, order);
        let mut d = device(&w, &owner, true);
        let size = all.len().div_ceil(chunks);
        for part in all.chunks(size) {
            d.deliver(part);
        }
        d.engine.retry_held_now(&mut d.store).unwrap();
        let pin = d.store.workspace_pin(w.wid).unwrap();
        let ingest = ligus_runtime::envelope_verifier::build_roster_pinned(&d.store.trust_envelopes(w.wid).unwrap(), pin.as_ref());
        let fold = d.roster(&w);
        let cfg = d.store.config_envelopes(w.wid).unwrap();
        let dir = ligus_core::copies::SignerDirectory::of(&cfg);
        for name in [owner.as_str(), "con", "eve", "vic", "bo"] {
            let p = &w.people[name];
            let a = p.account_id;
            let i = if ingest.is_disputed(a) { None } else { ingest.account_signing_key(a).map(<[u8]>::to_vec) };
            proptest::prop_assert_eq!(&i, &fold.account_keys.get(&a).cloned(), "{} ingest vs fold", name);
            // The anchored accounts (the founder, vic through her invite)
            // keep their own key whatever eve forged; the accounts an Admin
            // added directly have no anchor, so a forged self-certified
            // registration disputes them (residual, fail closed) but never
            // gives them eve's key.
            if name == owner.as_str() || name == "vic" {
                proptest::prop_assert!(!ingest.is_disputed(a), "{} disputed", name);
                proptest::prop_assert_eq!(i.as_deref(), Some(&p.account.public_key_bytes()[..]), "{} keeps their key", name);
            } else {
                proptest::prop_assert!(i.is_none() || i.as_deref() == Some(&p.account.public_key_bytes()[..]), "{} never takes another key", name);
            }
            // The signer directory attests each member's own events.
            if let Some(e) = cfg.iter().find(|e| e.signer_device_id == Some(p.device_id)) {
                proptest::prop_assert_eq!(dir.attesting_key(e), Some(&p.device.public_key_bytes()[..]), "{} attested", name);
            }
        }
        // None of eve's forgeries counts anywhere.
        for e in &attacks {
            proptest::prop_assert!(!fold.devices.get(&w.people["eve"].device_id).is_some_and(|d| d.accounts.iter().any(|a| *a != w.people["eve"].account_id)), "eve's device signs only for eve in the fold: {:?}", e.payload.kind_str());
        }
        proptest::prop_assert!(!ingest.device_signs_for(w.people["eve"].device_id, w.people[owner.as_str()].account_id));
        proptest::prop_assert!(d.store.has_event(w.chats[&chat].iter().find(|e| e.signer_device_id == Some(w.people["vic"].device_id)).unwrap().event_id).unwrap());
    }
}
