//! Held DM invites: delivered once the contact id arrives, never twice,
//! bounded, re-checked against the recipient's keys at delivery, and never
//! at rest in clear.

use std::sync::{Arc, Mutex};

use super::*;
use crate::secret_store::File0600;

const INVITE: &str = "hivews1:eyJyb29tIjoiZG0tc2VjcmV0LXJvb20iLCJrZXkiOiJ0aGUtZG0ta2V5LTEyMzQ1Njc4OTAifQ";
const NOW: i64 = 1_800_000_000;

fn device(seed: u8) -> Recipient {
    let ka = KeyAgreementKeypair::from_seed(&[seed; 32]).unwrap();
    Recipient { device_id: Uuid::from_bytes([seed; 16]).to_string(), ka_public: ka.public_key_bytes() }
}

fn person(account: Uuid, devices: Vec<Recipient>, contact: bool) -> CoMember {
    CoMember {
        account_id: account,
        display_name: "Bea".into(),
        look_alike: false,
        avatar_url: None,
        fingerprint: Some("00112233445566778899aabbccddeeff".into()),
        key_conflict: false,
        devices,
        revoked_devices: Vec::new(),
        workspaces: vec![Uuid::from_u128(1)],
        key_disputed_in: Vec::new(),
        contact: contact.then(|| ("c_bea".to_string(), Uuid::from_u128(1))),
    }
}

struct Fake {
    people: Mutex<Vec<CoMember>>,
    dm: bool,
    ka: KeyAgreementKeypair,
    outcome: Mutex<Delivery>,
    /// Every drop: (recipient keys, invite).
    drops: Mutex<Vec<(Vec<[u8; 32]>, String)>>,
}

impl Fake {
    fn new(p: CoMember) -> Self {
        Self {
            people: Mutex::new(vec![p]),
            dm: true,
            ka: KeyAgreementKeypair::from_seed(&[9; 32]).unwrap(),
            outcome: Mutex::new(Delivery::Delivered),
            drops: Mutex::new(Vec::new()),
        }
    }
    fn set(&self, f: impl FnOnce(&mut CoMember)) {
        f(&mut self.people.lock().unwrap()[0]);
    }
    fn drops(&self) -> usize {
        self.drops.lock().unwrap().len()
    }
}

impl Host for Fake {
    fn co_members(&self) -> Vec<CoMember> {
        self.people.lock().unwrap().clone()
    }
    fn dm_exists(&self, _ws: Uuid) -> bool {
        self.dm
    }
    fn own_ka(&self) -> Option<KeyAgreementKeypair> {
        KeyAgreementKeypair::from_seed(&self.ka.seed_bytes()).ok()
    }
    fn deliver(&self, _person: &CoMember, plan: &SealPlan, invite: &str) -> impl Future<Output = Delivery> + Send {
        let out = self.outcome.lock().unwrap().clone();
        if out == Delivery::Delivered {
            self.drops.lock().unwrap().push((plan.recipients.iter().map(|r| r.ka_public).collect(), invite.to_string()));
        }
        async move {
            tokio::task::yield_now().await;
            out
        }
    }
}

fn setup(contact: bool) -> (tempfile::TempDir, HeldInvites, Fake, Uuid) {
    let tmp = tempfile::tempdir().unwrap();
    let store = HeldInvites::new(tmp.path(), Arc::new(File0600::new(tmp.path())));
    let p = person(Uuid::new_v4(), vec![device(1), device(2)], contact);
    let host = Fake::new(p.clone());
    let ws = Uuid::new_v4();
    store.hold(ws, &p, &p.seal_plan().unwrap(), INVITE, &host.ka.public_key_bytes(), NOW).unwrap();
    (tmp, store, host, ws)
}

#[tokio::test]
async fn an_invite_held_before_the_contact_id_is_delivered_once_it_arrives() {
    let (_tmp, store, host, ws) = setup(false);
    assert_eq!(store.get(ws).unwrap().status_json()["pending"], serde_json::json!(true));
    // No contact id yet: nothing is sent.
    assert!(run_pass(&host, &store, NOW + 1).await.is_empty());
    assert_eq!(host.drops(), 0);
    // It arrives (contact sync / roster update): delivered, to the devices
    // approved at opening, with the invite as opened.
    host.set(|p| p.contact = Some(("c_bea".into(), Uuid::from_u128(1))));
    let changed = run_pass(&host, &store, NOW + 2).await;
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].state, HeldState::Delivered);
    let drops = host.drops.lock().unwrap().clone();
    assert_eq!(drops.len(), 1);
    assert_eq!(drops[0].1, INVITE);
    assert_eq!(drops[0].0, vec![device(1).ka_public, device(2).ka_public]);
    let st = store.get(ws).unwrap().status_json();
    assert_eq!(st["delivered"], serde_json::json!(true));
    assert_eq!(st["pending"], serde_json::json!(false));
    assert_eq!(st["deliveredAt"], serde_json::json!(NOW + 2));
}

#[tokio::test]
async fn a_held_invite_is_never_delivered_twice() {
    let (tmp, store, host, ws) = setup(true);
    // Concurrent passes, then more passes, then a "restart" (a fresh store
    // on the same dir): one drop.
    let (a, b) = tokio::join!(run_pass(&host, &store, NOW + 1), run_pass(&host, &store, NOW + 1));
    assert_eq!(a.len() + b.len(), 1);
    for i in 2..6 {
        run_pass(&host, &store, NOW + i).await;
    }
    let again = HeldInvites::new(tmp.path(), Arc::new(File0600::new(tmp.path())));
    run_pass(&host, &again, NOW + 10).await;
    assert_eq!(host.drops(), 1);
    // The secret is gone once delivered.
    assert!(store.open_invite(ws, &host.ka).is_none());
}

#[tokio::test]
async fn a_crash_mid_drop_is_never_resent() {
    let (_tmp, store, host, ws) = setup(true);
    store.transition(ws, &[HeldState::Waiting], HeldState::Sending, |_| {}).unwrap();
    run_pass(&host, &store, NOW + 1).await;
    assert_eq!(host.drops(), 0);
    assert_eq!(store.get(ws).unwrap().state, HeldState::Sending);
}

#[tokio::test]
async fn one_the_peer_accepted_by_hand_is_never_delivered() {
    // The opener also shared the link and the peer joined from it before
    // their contact id was known here.
    let (_tmp, store, host, ws) = setup(false);
    host.set(|p| {
        p.workspaces.push(ws);
        p.contact = Some(("c_bea".into(), Uuid::from_u128(1)));
    });
    let changed = run_pass(&host, &store, NOW + 1).await;
    assert_eq!(changed[0].state, HeldState::Joined);
    run_pass(&host, &store, NOW + 2).await;
    assert_eq!(host.drops(), 0);
    assert!(store.open_invite(ws, &host.ka).is_none());
}

#[tokio::test]
async fn a_held_invite_expires() {
    let (_tmp, store, host, ws) = setup(false);
    let exp = store.get(ws).unwrap().expires_at;
    assert_eq!(exp, NOW + HOLD_SECS);
    run_pass(&host, &store, exp - 1).await;
    assert_eq!(store.get(ws).unwrap().state, HeldState::Waiting);
    // The contact id arrives too late.
    host.set(|p| p.contact = Some(("c_bea".into(), Uuid::from_u128(1))));
    let changed = run_pass(&host, &store, exp).await;
    assert_eq!(changed[0].state, HeldState::Expired);
    assert_eq!(host.drops(), 0);
    assert!(store.open_invite(ws, &host.ka).is_none(), "the sealed invite is forgotten");
    // Kept a day for the DM view, then gone.
    run_pass(&host, &store, exp + KEEP_FINISHED_SECS + 1).await;
    assert!(store.get(ws).is_none());
}

#[tokio::test]
async fn held_invites_are_capped() {
    let tmp = tempfile::tempdir().unwrap();
    let store = HeldInvites::new(tmp.path(), Arc::new(File0600::new(tmp.path())));
    let p = person(Uuid::new_v4(), vec![device(1)], false);
    let ka = KeyAgreementKeypair::from_seed(&[9; 32]).unwrap();
    for _ in 0..MAX_HELD {
        store.hold(Uuid::new_v4(), &p, &p.seal_plan().unwrap(), INVITE, &ka.public_key_bytes(), NOW).unwrap();
    }
    let err = store.hold(Uuid::new_v4(), &p, &p.seal_plan().unwrap(), INVITE, &ka.public_key_bytes(), NOW).unwrap_err();
    assert!(err.contains("already waiting"), "{err}");
    // Expired ones make room again.
    store.hold(Uuid::new_v4(), &p, &p.seal_plan().unwrap(), INVITE, &ka.public_key_bytes(), NOW + HOLD_SECS + KEEP_FINISHED_SECS).unwrap();
}

#[tokio::test]
async fn a_changed_key_warns_and_is_not_sealed_to_silently() {
    // A new device key appears for them between opening and delivery.
    let (_tmp, store, host, ws) = setup(false);
    host.set(|p| {
        p.devices.push(device(7));
        p.contact = Some(("c_bea".into(), Uuid::from_u128(1)));
    });
    let changed = run_pass(&host, &store, NOW + 1).await;
    assert_eq!(changed[0].state, HeldState::Warning);
    let st = store.get(ws).unwrap().status_json();
    assert!(st["warning"].as_str().unwrap().starts_with(UNVERIFIED_RECIPIENT), "{st}");
    assert_eq!(st["pending"], serde_json::json!(true));
    for i in 2..5 {
        run_pass(&host, &store, NOW + i).await;
    }
    assert_eq!(host.drops(), 0, "held for the opener, never sealed to the new key");
    // The opener confirms: sent to the current verified devices.
    let code = host.co_members()[0].safety_code().unwrap();
    store.confirm(ws, &host.co_members()[0], &code, NOW + 5).unwrap();
    run_pass(&host, &store, NOW + 6).await;
    let drops = host.drops.lock().unwrap().clone();
    assert_eq!(drops.len(), 1);
    assert!(drops[0].0.contains(&device(7).ka_public));
}

#[tokio::test]
async fn a_changed_account_key_warns() {
    let (_tmp, store, host, ws) = setup(true);
    host.set(|p| p.fingerprint = Some("ffeeddccbbaa99887766554433221100".into()));
    let changed = run_pass(&host, &store, NOW + 1).await;
    assert_eq!(changed[0].state, HeldState::Warning);
    assert!(changed[0].note.as_deref().unwrap().contains("account key changed"));
    assert_eq!(host.drops(), 0);
    // A key conflict, too, holds it (and confirming is refused).
    host.set(|p| p.key_conflict = true);
    let code = host.co_members()[0].safety_code().unwrap();
    assert!(store.confirm(ws, &host.co_members()[0], &code, NOW + 2).is_err());
    assert_eq!(host.drops(), 0);
}

#[tokio::test]
async fn a_revoked_device_is_left_out() {
    let (_tmp, store, host, _ws) = setup(true);
    host.set(|p| p.devices.retain(|d| d.ka_public != device(2).ka_public));
    run_pass(&host, &store, NOW + 1).await;
    let drops = host.drops.lock().unwrap().clone();
    assert_eq!(drops[0].0, vec![device(1).ka_public]);
}

#[tokio::test]
async fn a_failed_drop_is_retried_with_backoff() {
    let (_tmp, store, host, ws) = setup(true);
    *host.outcome.lock().unwrap() = Delivery::NotYet("relay down".into());
    run_pass(&host, &store, NOW + 1).await;
    let h = store.get(ws).unwrap();
    assert_eq!((h.state, h.attempts, h.next_try_at), (HeldState::Waiting, 1, NOW + 1 + backoff_secs(1)));
    *host.outcome.lock().unwrap() = Delivery::Delivered;
    run_pass(&host, &store, NOW + 2).await;
    assert_eq!(host.drops(), 0, "backing off");
    run_pass(&host, &store, h.next_try_at).await;
    assert_eq!(host.drops(), 1);
}

#[tokio::test]
async fn the_held_store_holds_no_plaintext_secret() {
    let (tmp, store, host, ws) = setup(false);
    let needles = ["hivews1:", "dm-secret-room", "the-dm-key", &INVITE[8..40]];
    let mut files = Vec::new();
    fn walk(d: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            if e.path().is_dir() {
                walk(&e.path(), out);
            } else {
                out.push(e.path());
            }
        }
    }
    walk(tmp.path(), &mut files);
    assert!(files.iter().any(|f| f == store.path()));
    assert!(files.len() >= 2, "the list and the sealed secret: {files:?}");
    for f in &files {
        let bytes = std::fs::read(f).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        for n in needles {
            assert!(!text.contains(n), "{} holds {n:?}", f.display());
        }
    }
    // It opens with this device's key and no other.
    assert_eq!(store.open_invite(ws, &host.ka).as_deref().map(String::as_str), Some(INVITE));
    let other = KeyAgreementKeypair::from_seed(&[3; 32]).unwrap();
    assert!(store.open_invite(ws, &other).is_none());
}

#[tokio::test]
async fn dismissing_forgets_the_secret() {
    let (_tmp, store, host, ws) = setup(false);
    store.dismiss(ws).unwrap();
    host.set(|p| p.contact = Some(("c_bea".into(), Uuid::from_u128(1))));
    run_pass(&host, &store, NOW + 1).await;
    assert_eq!(host.drops(), 0);
    assert!(store.open_invite(ws, &host.ka).is_none());
}

#[tokio::test]
async fn confirming_is_bound_to_the_safety_code_the_opener_compared() {
    // Warned about one account key; it changes again before the click. The
    // opener compared the first one: the second is never approved unseen.
    let (_tmp, store, host, ws) = setup(true);
    host.set(|p| p.fingerprint = Some("ffeeddccbbaa99887766554433221100".into()));
    run_pass(&host, &store, NOW + 1).await;
    assert_eq!(store.get(ws).unwrap().state, HeldState::Warning);
    let compared = host.co_members()[0].safety_code().unwrap();
    host.set(|p| p.fingerprint = Some("abababababababababababababababab".into()));
    let err = store.confirm(ws, &host.co_members()[0], &compared, NOW + 2).unwrap_err();
    assert!(err.starts_with(UNVERIFIED_RECIPIENT), "{err}");
    assert!(store.confirm(ws, &host.co_members()[0], "", NOW + 2).is_err(), "no code, no confirmation");
    run_pass(&host, &store, NOW + 3).await;
    assert_eq!(host.drops(), 0);
    assert_eq!(store.get(ws).unwrap().state, HeldState::Warning);
    // The code shown now (spacing and case as typed don't matter).
    let now_code = host.co_members()[0].safety_code().unwrap().to_uppercase().replace(' ', "");
    store.confirm(ws, &host.co_members()[0], &now_code, NOW + 4).unwrap();
    run_pass(&host, &store, NOW + 5).await;
    assert_eq!(host.drops(), 1);
}

#[test]
fn concurrent_holds_cannot_pass_the_cap() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(HeldInvites::new(tmp.path(), Arc::new(File0600::new(tmp.path()))));
    let p = person(Uuid::new_v4(), vec![device(1)], false);
    let ka = KeyAgreementKeypair::from_seed(&[9; 32]).unwrap().public_key_bytes();
    let n = MAX_HELD * 3;
    let gate = Arc::new(std::sync::Barrier::new(n));
    let threads: Vec<_> = (0..n)
        .map(|_| {
            let (store, p, gate) = (store.clone(), p.clone(), gate.clone());
            std::thread::spawn(move || {
                gate.wait();
                store.hold(Uuid::new_v4(), &p, &p.seal_plan().unwrap(), INVITE, &ka, NOW).is_ok()
            })
        })
        .collect();
    let ok = threads.into_iter().map(|t| t.join().unwrap()).filter(|x| *x).count();
    let held = store.list().iter().filter(|h| h.state.is_open()).count();
    assert!(held <= MAX_HELD, "{held} held, cap {MAX_HELD}");
    assert_eq!(ok, held, "every accepted hold is on the list");
    let secrets = std::fs::read_dir(tmp.path().join("secrets").join(SCOPE)).map(|d| d.count()).unwrap_or(0);
    assert_eq!(secrets, held, "no secret without an entry");
}
