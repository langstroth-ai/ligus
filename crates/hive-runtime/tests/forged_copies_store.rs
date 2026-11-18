//! Forged copies through the store (review finding: erase-by-forgery): the
//! reviewer's probe, end to end. Relay bodies go through the real ingest
//! gate (`SyncEngine::apply_fetched`) into an `EventStore`, and the chat is
//! folded as a device folds it (`load_config` + `load_session_authorized`).
//! A copy forged under a real event's id is refused, or kept beside it, and
//! never decides the fold, whichever arrived first.

use hive_core::crypto::{sign_envelope, SigningKeypair};
use hive_core::testkit::SignedWorld;
use hive_core::{ChatMessage, ChatSession, MessageRole, SessionEvent, SessionEventEnvelope, WorkspaceAuthz, WorkspaceRole};
use hive_runtime::event_store::EventStore;
use hive_runtime::relay_client::RelayClient;
use hive_runtime::sync_engine::SyncEngine;
use proptest::prelude::*;
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
enum Forgery {
    UnsignedStampless,
    UnsignedStamped,
    WrongKey,
    Stranger,
}

const FORGERIES: [Forgery; 4] = [Forgery::UnsignedStampless, Forgery::UnsignedStamped, Forgery::WrongKey, Forgery::Stranger];

fn title(t: &str) -> SessionEvent {
    SessionEvent::SessionTitleChanged { title: t.into() }
}

fn forge(w: &mut SignedWorld, chat: Uuid, orig: &SessionEventEnvelope, how: Forgery) -> SessionEventEnvelope {
    let mut f = orig.clone();
    f.payload = title("forged");
    f.clear_raw();
    match how {
        Forgery::UnsignedStampless => {
            f.actor_stamp = None;
            f.signature = None;
            f.signer_device_id = None;
        }
        Forgery::UnsignedStamped => {
            f.signature = None;
            f.signer_device_id = None;
        }
        Forgery::WrongKey => sign_envelope(&mut f, orig.signer_device_id.unwrap(), &SigningKeypair::generate().unwrap()),
        Forgery::Stranger => {
            f = w.draft("eve", Some(chat), title("forged"), None);
            f.event_id = orig.event_id;
            f.lamport = orig.lamport;
            if let (Some(s), Some(o)) = (f.actor_stamp.as_mut(), orig.actor_stamp.as_ref()) {
                s.log_head = o.log_head;
            }
            w.finish(&mut f, "eve", None);
        }
    }
    f
}

/// A workspace (Owner, Contributor, and a stranger with a key of their own
/// no member certified), one chat, two title changes.
fn world() -> (SignedWorld, Uuid, Uuid) {
    let mut w = SignedWorld::genesis(&["owner", "con", "eve"], None);
    w.admit("owner", "con", WorkspaceRole::Contributor);
    let chat = w.chat("owner", "c");
    w.say(chat, "con", title("before"));
    let id = w.say(chat, "con", title("honest"));
    w.say(chat, "con", SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "con", "after") });
    (w, chat, id)
}

fn key() -> [u8; 32] {
    hive_core::derive_workspace_key("forged copies")
}

struct Device {
    store: EventStore,
    engine: SyncEngine,
    seq: u64,
}

impl Device {
    fn new(w: &SignedWorld) -> Self {
        let engine = SyncEngine::new(RelayClient::new("http://127.0.0.1:0"), Uuid::new_v4().to_string())
            .with_room_workspace(w.wid)
            .with_key(key());
        Self { store: EventStore::open_in_memory().unwrap(), engine, seq: 0 }
    }

    /// Bodies as the relay hands them out: sealed, so each opens to the
    /// bytes its author serialized.
    fn deliver(&mut self, envs: &[SessionEventEnvelope]) {
        let batch: Vec<(u64, Value)> = envs
            .iter()
            .map(|e| {
                self.seq += 1;
                let sealed = hive_core::seal_symmetric(&key(), 0, &serde_json::to_vec(e).unwrap()).unwrap();
                (self.seq, serde_json::to_value(sealed).unwrap())
            })
            .collect();
        self.engine.apply_fetched(&mut self.store, &batch).unwrap();
    }

    fn chat(&self, w: &SignedWorld, chat: Uuid) -> ChatSession {
        let (_, t) = self.store.load_config(w.wid, None, None).unwrap();
        self.store.load_session_authorized(chat, None, None, WorkspaceAuthz::Timeline(&t)).unwrap().unwrap()
    }
}

#[test]
fn the_reviewers_probe_an_honest_title_change_survives_a_forged_copy_in_either_order() {
    for how in FORGERIES {
        let (mut w, chat, id) = world();
        let orig = w.chats[&chat].iter().find(|e| e.event_id == id).unwrap().clone();
        let forged = forge(&mut w, chat, &orig, how);
        let honest: Vec<SessionEventEnvelope> = w.chats[&chat].clone();
        let before_it: Vec<SessionEventEnvelope> = honest.iter().take_while(|e| e.event_id != id).cloned().collect();
        let from_it: Vec<SessionEventEnvelope> = honest.iter().skip_while(|e| e.event_id != id).cloned().collect();

        // Honest copy first.
        let mut a = Device::new(&w);
        a.deliver(&w.cfg);
        a.deliver(&honest);
        a.deliver(std::slice::from_ref(&forged));
        // Forged copy first.
        let mut b = Device::new(&w);
        b.deliver(&w.cfg);
        b.deliver(&before_it);
        b.deliver(std::slice::from_ref(&forged));
        b.deliver(&from_it);

        let (sa, sb) = (a.chat(&w, chat), b.chat(&w, chat));
        assert_eq!(sa.title, "honest", "{how:?}: honest copy first");
        assert_eq!(sb.title, "honest", "{how:?}: forged copy first");
        assert_eq!(sa, sb, "{how:?}: arrival order decided the fold");
        // An unsigned copy is refused at ingest (a lamport witness only).
        if matches!(how, Forgery::UnsignedStampless | Forgery::UnsignedStamped) {
            for d in [&a, &b] {
                assert_eq!(d.store.list_copies(chat).unwrap().iter().filter(|e| e.event_id == id).count(), 1, "{how:?}: stored");
            }
        }
    }
}

/// A forged copy of the config seed, delivered before the real one, doesn't
/// take the workspace (or its chats) down.
#[test]
fn a_forged_seed_copy_delivered_first_does_not_take_the_workspace_down() {
    let (w, chat, _) = world();
    let seed = w.cfg[0].clone();
    let mut forged = seed.clone();
    if let SessionEvent::SessionSnapshot { session } = &mut forged.payload {
        session.title = "taken".into();
    }
    forged.clear_raw();
    sign_envelope(&mut forged, seed.signer_device_id.unwrap(), &SigningKeypair::generate().unwrap());
    let mut d = Device::new(&w);
    d.deliver(std::slice::from_ref(&forged));
    d.deliver(&w.cfg);
    d.deliver(&w.chats[&chat]);
    let (config, _) = d.store.load_config(w.wid, None, None).unwrap();
    let members: Vec<String> = config.expect("the workspace folds").members.into_iter().map(|m| m.id).collect();
    assert!(members.contains(&"owner".to_string()) && members.contains(&"con".to_string()), "{members:?}");
    assert_eq!(d.chat(&w, chat).title, "honest");
}

fn shuffle<T>(v: &mut [T], mut s: u64) {
    for i in (1..v.len()).rev() {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        v.swap(i, (s >> 33) as usize % (i + 1));
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    /// 2–4 devices, each receiving the honest events and its own forged
    /// copies in its own order and batches (re-fetched until nothing is
    /// held): every device folds the honest chat and config.
    #[test]
    fn prop_devices_fold_the_honest_log_whatever_arrives_first(
        forgeries in proptest::collection::vec((0u8..4, any::<u8>()), 0..6),
        devices in proptest::collection::vec((any::<u64>(), any::<u64>(), 1usize..5), 2..=4),
    ) {
        let (mut w, chat, _) = world();
        let honest_chat = w.chats[&chat].clone();
        let mut forged = Vec::new();
        for (how, target) in &forgeries {
            // Any chat event but the seed (a forged snapshot is its own case).
            let orig = honest_chat[1 + (*target as usize) % (honest_chat.len() - 1)].clone();
            forged.push(forge(&mut w, chat, &orig, FORGERIES[*how as usize]));
        }
        let mut reference = Device::new(&w);
        reference.deliver(&w.cfg);
        reference.deliver(&honest_chat);
        let want = reference.chat(&w, chat);
        prop_assert_eq!(&want.title, "honest");
        let mut folds = Vec::new();
        for (subset, order, chunks) in &devices {
            let mut all: Vec<SessionEventEnvelope> = w.cfg.iter().chain(&honest_chat).cloned().collect();
            all.extend(forged.iter().enumerate().filter(|(i, _)| subset & (1 << i) != 0).map(|(_, f)| f.clone()));
            shuffle(&mut all, *order);
            let mut d = Device::new(&w);
            let size = all.len().div_ceil(*chunks);
            for part in all.chunks(size) {
                d.deliver(part);
            }
            // The relay re-serves what was held (a body of a device whose
            // certificate came later).
            for _ in 0..3 {
                d.deliver(&all);
            }
            let got = d.chat(&w, chat);
            prop_assert_eq!(&got.title, &want.title);
            prop_assert_eq!(&got.messages, &want.messages);
            prop_assert_eq!(&got.members, &want.members);
            folds.push(got);
        }
        for f in &folds[1..] {
            prop_assert_eq!(f, &folds[0], "two devices folded differently");
        }
    }
}
