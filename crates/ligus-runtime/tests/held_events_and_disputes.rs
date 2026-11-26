//! Held events never stall sync, and the account-key dispute DoS is gone,
//! end to end: relay bodies go through the real ingest gate
//! (`SyncEngine::apply_fetched`) into an `EventStore`, and the workspace is
//! folded as a device folds it.
//!
//! - An event ingest can't judge yet (a signer whose certificate or
//!   admission never comes) waits in the held table; the cursor moves past
//!   it, so everything after it still syncs.
//! - An event held and released later folds exactly as if it had arrived
//!   after what it waited for: devices that receive the same events in any
//!   order and batching end with identical folds.
//! - A member registering a key under someone else's account id is refused
//!   and never silences them, in any order.

use ligus_core::crypto::{sign_envelope, DeviceCertificate, SigningKeypair};
use ligus_core::testkit::{Proof, SignedWorld};
use ligus_core::{
    CfgFrontier, ChatSession, SessionEvent, SessionEventEnvelope, WorkspaceAuthz, WorkspaceRole,
};
use ligus_runtime::event_store::EventStore;
use ligus_runtime::relay_client::RelayClient;
use ligus_runtime::sync_engine::SyncEngine;
use proptest::prelude::*;
use serde_json::Value;
use uuid::Uuid;

/// Derived once: argon2 per sealed body would make the property below take
/// minutes at CI's case counts.
fn key() -> [u8; 32] {
    static KEY: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    *KEY.get_or_init(|| ligus_core::derive_workspace_key("held events and disputes"))
}

struct Device {
    store: EventStore,
    engine: SyncEngine,
    /// Every body this device was handed, as the relay keeps it.
    relay: Vec<(u64, Value)>,
}

impl Device {
    fn new(w: &SignedWorld) -> Self {
        let engine = SyncEngine::new(
            RelayClient::new("http://127.0.0.1:0"),
            Uuid::new_v4().to_string(),
        )
        .with_room_workspace(w.wid)
        .with_key(key());
        Self {
            store: EventStore::open_in_memory().unwrap(),
            engine,
            relay: Vec::new(),
        }
    }

    /// The relay takes these bodies and the device fetches from its cursor.
    fn deliver(&mut self, envs: &[SessionEventEnvelope]) {
        let start = self.relay.len() as u64;
        for (e, seq) in envs.iter().zip(start + 1..) {
            let sealed =
                ligus_core::seal_symmetric(&key(), 0, &serde_json::to_vec(e).unwrap()).unwrap();
            self.relay
                .push((seq, serde_json::to_value(sealed).unwrap()));
        }
        let cursor = self.engine.cursor();
        let page: Vec<(u64, Value)> = self
            .relay
            .iter()
            .filter(|(s, _)| *s > cursor)
            .cloned()
            .collect();
        if !page.is_empty() {
            self.engine.apply_fetched(&mut self.store, &page).unwrap();
        }
    }

    fn chat(&self, w: &SignedWorld, chat: Uuid) -> Option<ChatSession> {
        let (_, t) = self.store.load_config(w.wid, None, None).unwrap();
        self.store
            .load_session_authorized(w.wid, chat, None, None, WorkspaceAuthz::Timeline(&t))
            .unwrap()
    }

    fn roster(&self, w: &SignedWorld) -> ligus_core::authz::RosterState {
        self.store
            .load_config(w.wid, None, None)
            .unwrap()
            .1
            .current()
            .clone()
    }

    fn held_ids(&self, w: &SignedWorld) -> Vec<Uuid> {
        let mut v: Vec<Uuid> = self
            .store
            .held_events(w.wid)
            .unwrap()
            .into_iter()
            .map(|h| h.env.event_id)
            .collect();
        v.sort();
        v
    }
}

/// owner founds; con is an Admin-added Contributor; vic joins through an
/// invite and publishes her identity; eve is a member who attacks; ghost has
/// a device nobody certified and is never admitted. One chat with vic's and
/// con's messages.
fn world() -> (SignedWorld, Uuid) {
    let mut w = SignedWorld::genesis(&["owner", "con", "vic", "eve", "ghost"], None);
    w.admit("owner", "con", WorkspaceRole::Contributor);
    w.admit("owner", "eve", WorkspaceRole::Contributor);
    let invite = Uuid::new_v4();
    w.write(
        "owner",
        None,
        SessionEvent::JoinApproved {
            subject: ligus_core::authorization::invite_subject(invite),
            admission: None,
            redeem_key: None,
        },
        None,
        Proof::Recovery,
    );
    let m = w.member_of("vic", WorkspaceRole::Contributor);
    w.push_bare(
        "vic",
        SessionEvent::MemberAdded {
            member: m,
            invite: Some(invite),
            proof: None,
        },
    );
    w.publish_identity("vic");
    w.register_recovery("vic");
    let chat = w.chat("owner", "c");
    w.say(chat, "vic", title("vic was here"));
    w.say(chat, "con", msg("con", "hello"));
    (w, chat)
}

fn title(t: &str) -> SessionEvent {
    SessionEvent::SessionTitleChanged { title: t.into() }
}

fn msg(by: &str, body: &str) -> SessionEvent {
    SessionEvent::MessageAppended {
        message: ligus_core::ChatMessage::new(ligus_core::MessageRole::User, by, body),
    }
}

/// `payload` on `chat` (or the config log) stamped as `as_who`, signed by
/// `by`'s device, with `log_head` (an honest head by default).
fn forge(
    w: &mut SignedWorld,
    by: &str,
    as_who: &str,
    chat: Option<Uuid>,
    payload: SessionEvent,
    log_head: Option<Option<CfgFrontier>>,
) -> SessionEventEnvelope {
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

/// The config position just before vic's own admission.
fn before_vics_admission(w: &SignedWorld) -> Option<CfgFrontier> {
    let vic = w.people["vic"].account_id;
    let adm = w
        .cfg
        .iter()
        .find(|e| matches!(&e.payload, SessionEvent::MemberAdded { member, .. } if member.actor.account_id == Some(vic)))
        .unwrap()
        .canonical_key();
    w.cfg
        .iter()
        .filter(|e| e.canonical_key() < adm)
        .map(|e| CfgFrontier {
            lamport: e.lamport,
            event_id: e.event_id,
        })
        .max()
}

/// Events whose certificate or admission never comes: ghost's fresh device
/// stamping con, eve's own device stamping con, and ghost's own identity
/// (published, never admitted) with a chat event as ghost.
fn never_judgeable(w: &mut SignedWorld, chat: Uuid) -> Vec<SessionEventEnvelope> {
    let ghost = &w.people["ghost"];
    let cert = DeviceCertificate::issue(
        &ghost.account,
        ghost.account_id,
        &ghost.device.public_key_bytes(),
        ligus_core::Timestamp::epoch(),
    );
    let reg = SessionEvent::AccountKeyRegistered {
        account_id: ghost.account_id,
        signing_public_key: ghost.account.public_key_bytes().to_vec(),
    };
    vec![
        forge(w, "ghost", "con", Some(chat), title("ghost as con"), None),
        forge(w, "eve", "con", Some(chat), title("eve as con"), None),
        forge(
            w,
            "ghost",
            "ghost",
            None,
            SessionEvent::DeviceCertificateAdded { certificate: cert },
            None,
        ),
        forge(w, "ghost", "ghost", None, reg, None),
        forge(
            w,
            "ghost",
            "ghost",
            Some(chat),
            title("unadmitted ghost"),
            None,
        ),
    ]
}

/// The account-key attacks on vic: eve registers a key under vic's account
/// id from ghost's device and from her own, without and with certifying the
/// signing device under that key, late and backdated before vic's admission.
fn dispute_attempts(w: &mut SignedWorld) -> Vec<SessionEventEnvelope> {
    let vic = w.people["vic"].account_id;
    let early = before_vics_admission(w);
    let mut out = Vec::new();
    for (by, cert, head) in [
        ("ghost", false, None),
        ("eve", true, None),
        ("ghost", true, Some(early)),
        ("eve", false, Some(early)),
    ] {
        let k = SigningKeypair::generate().unwrap();
        if cert {
            let c = DeviceCertificate::issue(
                &k,
                vic,
                &w.people[by].device.public_key_bytes(),
                ligus_core::Timestamp::epoch(),
            );
            out.push(forge(
                w,
                by,
                "vic",
                None,
                SessionEvent::DeviceCertificateAdded { certificate: c },
                head,
            ));
        }
        let reg = SessionEvent::AccountKeyRegistered {
            account_id: vic,
            signing_public_key: k.public_key_bytes().to_vec(),
        };
        out.push(forge(w, by, "vic", None, reg, head));
    }
    out
}

#[test]
fn events_whose_certificate_never_comes_do_not_stall_sync() {
    let (mut w, chat) = world();
    let stuck = never_judgeable(&mut w, chat);
    let mut d = Device::new(&w);
    d.deliver(&w.cfg);
    d.deliver(&w.chats[&chat]);
    d.deliver(&stuck);
    // Everything after them still syncs.
    let after = w.say(chat, "vic", title("after the stuck ones"));
    let tail: Vec<SessionEventEnvelope> = w.chats[&chat]
        .iter()
        .filter(|e| e.event_id == after)
        .cloned()
        .collect();
    d.deliver(&tail);
    assert_eq!(
        d.engine.cursor(),
        d.relay.len() as u64,
        "the cursor reached the head"
    );
    assert_eq!(d.chat(&w, chat).unwrap().title, "after the stuck ones");
    // The never-judgeable chat events wait in the held table, counted for
    // diagnostics; none of them applied.
    let held = d.held_ids(&w);
    for e in stuck
        .iter()
        .filter(|e| matches!(e.payload, SessionEvent::SessionTitleChanged { .. }))
    {
        assert!(held.contains(&e.event_id), "{:?} isn't held", e.payload);
        assert!(!d.store.has_event(e.event_id).unwrap());
    }
    let stats = d.engine.held_stats(&d.store).unwrap();
    assert_eq!(stats.held as usize, held.len());
    assert!(stats.signers >= 2);
    // Judging them again (the slow timer) changes nothing: still held.
    d.engine.retry_held_now(&mut d.store).unwrap();
    assert_eq!(d.held_ids(&w), held);
}

#[test]
fn a_removed_member_never_readmitted_does_not_stall_sync() {
    let (mut w, chat) = world();
    w.write(
        "owner",
        None,
        SessionEvent::MemberRemoved {
            member_id: "eve".into(),
            watermark: Vec::new(),
        },
        None,
        Proof::Recovery,
    );
    // eve keeps writing: from her own (now revoked) device, and from ghost's
    // fresh device stamped as her.
    let own = w.say(chat, "eve", title("eve after removal"));
    let fresh = forge(
        &mut w,
        "ghost",
        "eve",
        Some(chat),
        title("eve from a fresh device"),
        None,
    );
    let after = w.say(chat, "con", title("con after"));
    let mut d = Device::new(&w);
    d.deliver(&w.cfg);
    let mut chat_log = w.chats[&chat].clone();
    chat_log.insert(chat_log.len() - 1, fresh.clone());
    d.deliver(&chat_log);
    assert_eq!(d.engine.cursor(), d.relay.len() as u64);
    // Never applied: refused where the device knew eve's certificate before
    // her removal, held where it didn't (her identity arrived with the
    // removal, so it was refused and her device is unknown here).
    assert!(
        !d.store.has_event(own).unwrap(),
        "a removed member's own device is never applied"
    );
    assert!(
        d.held_ids(&w).contains(&fresh.event_id),
        "a fresh device stamped as the removed member waits"
    );
    assert!(d.store.has_event(after).unwrap());
    let c = d.chat(&w, chat).unwrap();
    assert_eq!(
        c.title, "con after",
        "an event counting from a held one doesn't wait on it: {:?}",
        c.authz.held
    );
}

#[test]
fn an_event_held_before_its_certificate_folds_as_if_it_came_after() {
    let (w, chat) = world();
    // The chat (vic's message among it) reaches this device before the
    // config log that admits and certifies her.
    let mut late = Device::new(&w);
    late.deliver(&w.chats[&chat]);
    assert!(
        !late.held_ids(&w).is_empty() || late.chat(&w, chat).is_none(),
        "nothing judged without the config log"
    );
    late.deliver(&w.cfg);
    let mut reference = Device::new(&w);
    reference.deliver(&w.cfg);
    reference.deliver(&w.chats[&chat]);
    assert!(
        late.held_ids(&w).is_empty(),
        "released once the certificate and admission arrived"
    );
    assert_eq!(late.chat(&w, chat), reference.chat(&w, chat));
    assert_eq!(late.roster(&w), reference.roster(&w));
    assert_eq!(late.chat(&w, chat).unwrap().title, "vic was here");
}

#[test]
fn a_member_cannot_silence_another_by_registering_a_key_for_them() {
    let (mut w, chat) = world();
    let attacks = dispute_attempts(&mut w);
    let vic = w.people["vic"].account_id;
    for attacks_first in [false, true] {
        let mut d = Device::new(&w);
        if attacks_first {
            d.deliver(&attacks);
            d.deliver(&w.cfg);
        } else {
            d.deliver(&w.cfg);
            d.deliver(&attacks);
        }
        d.deliver(&w.chats[&chat]);
        let roster = d.roster(&w);
        assert_eq!(
            roster.account_keys.get(&vic),
            Some(&w.people["vic"].account.public_key_bytes().to_vec()),
            "first={attacks_first}"
        );
        assert_eq!(
            d.chat(&w, chat).unwrap().title,
            "vic was here",
            "vic's events keep folding (first={attacks_first})"
        );
        let (_, t) = d.store.load_config(w.wid, None, None).unwrap();
        let roster = ligus_runtime::envelope_verifier::build_rosters(
            &d.store.trust_envelopes(w.wid).unwrap(),
            &Default::default(),
        );
        let r = &roster[&w.wid];
        assert!(!r.is_disputed(vic), "first={attacks_first}");
        // Attributed: each refused registration names its signer.
        for a in attacks
            .iter()
            .filter(|e| matches!(e.payload, SessionEvent::AccountKeyRegistered { .. }))
        {
            assert!(
                r.refused_keys()
                    .iter()
                    .any(|x| x.event_id == a.event_id && x.signer_device_id == a.signer_device_id),
                "refused and attributed (first={attacks_first})"
            );
        }
        assert!(t.anchors().get(&vic).is_some());
    }
}

fn shuffle<T>(v: &mut [T], mut s: u64) {
    for i in (1..v.len()).rev() {
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        v.swap(i, (s >> 33) as usize % (i + 1));
    }
}

proptest! {
    // PROPTEST_CASES overrides this (CI runs 256, the nightly 4096), so a
    // case stays cheap: the workspace key is derived once.
    #![proptest_config(ProptestConfig::with_cases(16))]

    /// 2–3 devices receive the honest log, the never-judgeable events and the
    /// dispute attempts, each in its own order and batches: every device
    /// folds the config and the chat identically (held-then-released events
    /// fold as if they arrived in order), vic keeps her key and her events,
    /// and every device holds the same never-judgeable events.
    #[test]
    fn prop_devices_converge_whatever_arrives_first(
        devices in proptest::collection::vec((any::<u64>(), 1usize..6), 2..=3),
        with_attacks in any::<bool>(),
    ) {
        let (mut w, chat) = world();
        let stuck = never_judgeable(&mut w, chat);
        let attacks = if with_attacks { dispute_attempts(&mut w) } else { Vec::new() };
        let mut reference = Device::new(&w);
        reference.deliver(&w.cfg);
        reference.deliver(&w.chats[&chat]);
        reference.deliver(&attacks);
        reference.deliver(&stuck);
        let want_chat = reference.chat(&w, chat);
        let want_roster = reference.roster(&w);
        let want_held = reference.held_ids(&w);
        let vic = w.people["vic"].account_id;
        prop_assert_eq!(want_roster.account_keys.get(&vic), Some(&w.people["vic"].account.public_key_bytes().to_vec()));
        prop_assert_eq!(&want_chat.as_ref().unwrap().messages.len(), &1);
        for (order, chunks) in &devices {
            let mut all: Vec<SessionEventEnvelope> = w.cfg.iter().chain(&w.chats[&chat]).chain(&attacks).chain(&stuck).cloned().collect();
            shuffle(&mut all, *order);
            let mut d = Device::new(&w);
            let size = all.len().div_ceil(*chunks);
            for part in all.chunks(size) {
                d.deliver(part);
            }
            prop_assert_eq!(d.engine.cursor(), d.relay.len() as u64, "held events never pin the cursor");
            prop_assert_eq!(&d.chat(&w, chat), &want_chat);
            prop_assert_eq!(&d.roster(&w), &want_roster);
            let all_envs: Vec<SessionEventEnvelope> = w.cfg.iter().chain(&w.chats[&chat]).chain(&attacks).chain(&stuck).cloned().collect();
            let describe = |ids: &[Uuid]| ids.iter().map(|i| all_envs.iter().find(|e| e.event_id == *i).map(|e| format!("{}:{:?}", e.payload.kind_str(), e.actor_stamp.as_ref().map(|s| s.actor.id.clone()))).unwrap_or_default()).collect::<Vec<_>>();
            prop_assert_eq!(&d.held_ids(&w), &want_held, "the same events wait on every device: {:?} vs {:?}", describe(&d.held_ids(&w)), describe(&want_held));
        }
    }
}

/// A flood of held events from one device is capped; the honest event after
/// it still syncs.
#[test]
fn a_flood_of_held_events_is_bounded() {
    let (mut w, chat) = world();
    let n = ligus_runtime::event_store::HELD_PER_SIGNER as usize + 60;
    let flood: Vec<SessionEventEnvelope> = (0..n)
        .map(|i| {
            forge(
                &mut w,
                "ghost",
                "con",
                Some(chat),
                title(&format!("flood {i}")),
                None,
            )
        })
        .collect();
    let after = w.say(chat, "vic", title("after the flood"));
    let mut d = Device::new(&w);
    d.deliver(&w.cfg);
    d.deliver(&w.chats[&chat][..w.chats[&chat].len() - 1]);
    d.deliver(&flood);
    let tail: Vec<SessionEventEnvelope> = w.chats[&chat]
        .iter()
        .filter(|e| e.event_id == after)
        .cloned()
        .collect();
    d.deliver(&tail);
    assert_eq!(d.engine.cursor(), d.relay.len() as u64);
    assert_eq!(d.chat(&w, chat).unwrap().title, "after the flood");
    let stats = d.engine.held_stats(&d.store).unwrap();
    assert_eq!(stats.held, ligus_runtime::event_store::HELD_PER_SIGNER);
    assert_eq!(
        stats.dropped as usize,
        n - ligus_runtime::event_store::HELD_PER_SIGNER as usize
    );
}
