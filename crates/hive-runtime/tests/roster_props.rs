//! Per-workspace trust rosters built from verified events, with the founder
//! pinned (Phase 0, P0.4).

use std::collections::HashMap;

use hive_core::crypto::{DeviceCertificate, SigningKeypair};
use hive_core::{
    sign_envelope, workspace_config_session_id, ActorIdentity, ActorKind, ActorStamp, ChatSession, SessionEvent,
    SessionEventEnvelope, Timestamp, WorkspaceMember, WorkspaceRole,
};
use hive_runtime::envelope_verifier::{
    build_roster, build_rosters, roster_with_batch, verdict_for, DeviceKeyResolver, FounderPin, QuarantineReason,
    Verdict,
};
use proptest::prelude::*;
use uuid::Uuid;

struct Principal {
    name: &'static str,
    account: SigningKeypair,
    account_id: Uuid,
    device: SigningKeypair,
    device_id: Uuid,
}

impl Principal {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            account: SigningKeypair::generate().unwrap(),
            account_id: Uuid::new_v4(),
            device: SigningKeypair::generate().unwrap(),
            device_id: Uuid::new_v4(),
        }
    }
    fn actor(&self) -> ActorIdentity {
        let mut a = ActorIdentity::new(self.account_id.to_string(), self.name, ActorKind::Human);
        a.account_id = Some(self.account_id);
        a.device_id = Some(self.device_id);
        a
    }
    fn member(&self, role: WorkspaceRole) -> WorkspaceMember {
        WorkspaceMember { id: self.account_id.to_string(), actor: self.actor(), role, title: String::new(), index: 0, joined_at: Timestamp::epoch() }
    }
    /// An event on `ws`'s config log, signed by this principal's device.
    fn sign(&self, ws: Uuid, lamport: u64, payload: SessionEvent) -> SessionEventEnvelope {
        let mut e = SessionEventEnvelope::new(workspace_config_session_id(ws), ws, lamport as i64, payload);
        e.lamport = lamport;
        e.actor_stamp = Some(ActorStamp { actor: self.actor(), recorded_at: Timestamp::epoch(), cfg_frontier: None, presence: None, runtime_effect: false });
        sign_envelope(&mut e, self.device_id, &self.device);
        e
    }
    fn trust(&self, ws: Uuid, lamport: u64) -> Vec<SessionEventEnvelope> {
        let cert = DeviceCertificate::issue(&self.account, self.account_id, self.device_id, &self.device.public_key_bytes(), Timestamp::epoch());
        vec![
            self.sign(ws, lamport, SessionEvent::AccountKeyRegistered { account_id: self.account_id, signing_public_key: self.account.public_key_bytes().to_vec() }),
            self.sign(ws, lamport + 1, SessionEvent::DeviceCertificateAdded { certificate: cert }),
        ]
    }
    fn seed(&self, ws: Uuid, lamport: u64) -> SessionEventEnvelope {
        let mut s = ChatSession::new("", ws, "");
        s.id = workspace_config_session_id(ws);
        s.creator_actor_id = self.account_id.to_string();
        s.members = vec![self.member(WorkspaceRole::Owner)];
        self.sign(ws, lamport, SessionEvent::SessionSnapshot { session: Box::new(s) })
    }
    fn content(&self, ws: Uuid, lamport: u64) -> SessionEventEnvelope {
        self.sign(ws, lamport, SessionEvent::SessionTitleChanged { title: format!("t{lamport}") })
    }
}

/// A workspace founded by `owner` with `others` added as Contributors.
fn workspace(ws: Uuid, owner: &Principal, others: &[&Principal]) -> Vec<SessionEventEnvelope> {
    let mut out = vec![owner.seed(ws, 1)];
    out.extend(owner.trust(ws, 2));
    let mut l = 10;
    for p in others {
        out.push(owner.sign(ws, l, SessionEvent::MemberAdded { invite: None, member: p.member(WorkspaceRole::Contributor) }));
        out.extend(p.trust(ws, l + 1));
        l += 10;
    }
    out
}

proptest! {
    /// An Admin of W1 removing an account never touches its devices in W2,
    /// whatever else happens and in whatever order the events are held.
    #[test]
    fn prop_removal_scoped_to_workspace(noise in 0usize..12, seed in any::<u64>(), remove_in_first in any::<bool>()) {
        let (o1, o2, a) = (Principal::new("o1"), Principal::new("o2"), Principal::new("a"));
        let (w1, w2) = (Uuid::new_v4(), Uuid::new_v4());
        let mut all = workspace(w1, &o1, &[&a]);
        all.extend(workspace(w2, &o2, &[&a]));
        let (wr, owner) = if remove_in_first { (w1, &o1) } else { (w2, &o2) };
        all.push(owner.sign(wr, 500, SessionEvent::MemberRemoved { member_id: a.account_id.to_string() }));
        for i in 0..noise {
            let (w, o) = if i % 2 == 0 { (w1, &o1) } else { (w2, &o2) };
            all.push(o.content(w, 600 + i as u64));
        }
        let mut rng = seed | 1;
        for i in (1..all.len()).rev() {
            rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17;
            all.swap(i, (rng % (i as u64 + 1)) as usize);
        }
        let rosters = build_rosters(&all, &HashMap::new());
        let other = if remove_in_first { w2 } else { w1 };
        prop_assert!(rosters[&wr].is_revoked(a.device_id), "removed where it was removed");
        prop_assert!(rosters[&other].is_certified(a.device_id), "still trusted in the other workspace");
        prop_assert_eq!(verdict_for(&rosters[&other], &a.content(other, 700)), Verdict::Valid);
        // The pre-Phase-0 shared roster let the removal leak across.
        prop_assert!(build_roster(&all).is_revoked(a.device_id));
    }
}

/// A batch carrying a forged membership (eve stamping herself in as the
/// owner) plus eve's own certificate: the old roster, built from the store
/// plus the raw batch, certified eve; the batch fixpoint doesn't.
#[test]
fn roster_ignores_unverified_batch_membership() {
    let (owner, eve) = (Principal::new("owner"), Principal::new("eve"));
    let ws = Uuid::new_v4();
    let stored = workspace(ws, &owner, &[]);
    // Eve signs with her own device but stamps the owner as author.
    let mut forged = eve.sign(ws, 100, SessionEvent::MemberAdded { invite: None, member: eve.member(WorkspaceRole::Admin) });
    forged.actor_stamp = Some(ActorStamp { actor: owner.actor(), recorded_at: Timestamp::epoch(), cfg_frontier: None, presence: None, runtime_effect: false });
    forged.clear_raw();
    sign_envelope(&mut forged, eve.device_id, &eve.device);
    let mut batch = eve.trust(ws, 101);
    batch.push(forged.clone());
    let eves_post = eve.content(ws, 110);
    batch.push(eves_post.clone());

    let mut raw = stored.clone();
    raw.extend(batch.iter().cloned());
    assert!(build_roster(&raw).is_certified(eve.device_id), "precondition: the raw batch certified eve");

    let refs: Vec<&SessionEventEnvelope> = batch.iter().collect();
    let roster = roster_with_batch(stored, &refs, None);
    assert!(!roster.is_certified(eve.device_id));
    assert_ne!(verdict_for(&roster, &eves_post), Verdict::Valid);
    // Her forged membership is never accepted: it claims the owner from a
    // device the owner never certified.
    assert!(matches!(
        verdict_for(&roster, &forged),
        Verdict::Quarantine(_) | Verdict::Unverifiable(QuarantineReason::UncertifiedDevice)
    ));
    // A fresh device id (no certificate at all) claiming the owner is held.
    let mut fresh = forged.clone();
    fresh.clear_raw();
    let ghost = SigningKeypair::generate().unwrap();
    sign_envelope(&mut fresh, Uuid::new_v4(), &ghost);
    assert_eq!(verdict_for(&roster, &fresh), Verdict::Unverifiable(QuarantineReason::UncertifiedDevice));
}

/// The batch fixpoint still admits a whole workspace arriving at once: the
/// founding seed and self-validating certificates, then the founder's own
/// signed additions, then the added member's certificate.
#[test]
fn a_fresh_workspace_verifies_in_one_batch() {
    let (owner, mara) = (Principal::new("owner"), Principal::new("mara"));
    let ws = Uuid::new_v4();
    let batch = workspace(ws, &owner, &[&mara]);
    let refs: Vec<&SessionEventEnvelope> = batch.iter().collect();
    let roster = roster_with_batch(Vec::new(), &refs, None);
    assert!(roster.is_certified(owner.device_id) && roster.is_certified(mara.device_id));
    assert_eq!(verdict_for(&roster, &mara.content(ws, 99)), Verdict::Valid);
    // A self-enrollment below Admin, signed by the joiner's own device, too.
    let joe = Principal::new("joe");
    let mut batch2 = joe.trust(ws, 200);
    batch2.push(joe.sign(ws, 202, SessionEvent::MemberAdded { invite: None, member: joe.member(WorkspaceRole::Contributor) }));
    let refs: Vec<&SessionEventEnvelope> = batch2.iter().collect();
    let roster = roster_with_batch(batch, &refs, None);
    assert!(roster.is_certified(joe.device_id));
}

/// The invite pins the founder's key fingerprint: a registration of the
/// founder's account under another key (sorted first) is rejected, so the
/// founder's real devices verify and the forger's don't. Without the pin the
/// account is merely disputed (everyone's events under it are refused).
#[test]
fn founder_pin_disputes_forged_founder_key() {
    let founder = Principal::new("founder");
    let ws = Uuid::new_v4();
    let mut events = workspace(ws, &founder, &[]);
    // The forger registers the founder's account id under its own key, with a
    // device certificate chained to that key, at a lower lamport.
    let forger = Principal { account_id: founder.account_id, ..Principal::new("forger") };
    let fcert = DeviceCertificate::issue(&forger.account, founder.account_id, forger.device_id, &forger.device.public_key_bytes(), Timestamp::epoch());
    let mut reg = forger.sign(ws, 0, SessionEvent::AccountKeyRegistered { account_id: founder.account_id, signing_public_key: forger.account.public_key_bytes().to_vec() });
    reg.lamport = 0;
    reg.clear_raw();
    sign_envelope(&mut reg, forger.device_id, &forger.device);
    events.push(reg);
    events.push(forger.sign(ws, 0, SessionEvent::DeviceCertificateAdded { certificate: fcert }));

    let unpinned = build_roster(&events);
    assert!(unpinned.is_disputed(founder.account_id), "without a pin the binding is contested");

    let fk = FounderPin::fingerprint_hex(&founder.account.public_key_bytes());
    let pin = FounderPin::parse(&founder.account_id.to_string(), &fk).unwrap();
    let pins: HashMap<Uuid, FounderPin> = [(ws, pin)].into_iter().collect();
    let roster = &build_rosters(&events, &pins)[&ws];
    assert!(!roster.is_disputed(founder.account_id));
    assert_eq!(roster.account_signing_key(founder.account_id), Some(&founder.account.public_key_bytes()[..]));
    assert_eq!(verdict_for(roster, &founder.content(ws, 50)), Verdict::Valid);
    assert!(roster.public_key(forger.device_id).is_none(), "the forger's device chains to a rejected key");
    assert_ne!(verdict_for(roster, &forger.content(ws, 51)), Verdict::Valid);
}

/// An ex-member keeps old keys, but after the removal syncs, none of their
/// events is accepted: their certified devices are revoked, and a fresh
/// device stamping their account is held (never accepted on the stamp).
#[test]
fn an_ex_members_events_are_refused_at_ingest() {
    let (owner, mara) = (Principal::new("owner"), Principal::new("mara"));
    let ws = Uuid::new_v4();
    let mut events = workspace(ws, &owner, &[&mara]);
    events.push(owner.sign(ws, 100, SessionEvent::MemberRemoved { member_id: mara.account_id.to_string() }));
    let roster = build_roster(&events);
    assert!(matches!(verdict_for(&roster, &mara.content(ws, 120)), Verdict::Quarantine(QuarantineReason::RevokedDevice)));
    let mut fresh = mara.content(ws, 121);
    fresh.clear_raw();
    sign_envelope(&mut fresh, Uuid::new_v4(), &SigningKeypair::generate().unwrap());
    assert_eq!(verdict_for(&roster, &fresh), Verdict::Unverifiable(QuarantineReason::UncertifiedDevice));
}

proptest! {
    /// Delivery with duplicates and in any order converges: two stores that
    /// ingest the same events (one in order, one shuffled with repeats) hold
    /// the same rows and fold the same config and chat.
    #[test]
    fn prop_duplicate_and_reordered_delivery_converge(dups in prop::collection::vec(0usize..64, 0..24), seed in any::<u64>()) {
        use hive_runtime::event_store::EventStore;
        let (owner, mara) = (Principal::new("owner"), Principal::new("mara"));
        let ws = Uuid::new_v4();
        let mut events = workspace(ws, &owner, &[&mara]);
        let head = |evs: &[SessionEventEnvelope]| evs.iter().map(|e| hive_core::CfgFrontier { lamport: e.lamport, event_id: e.event_id }).max();
        let f = head(&events);
        let sid = Uuid::new_v4();
        let mut chat = ChatSession::new("c", ws, "x");
        chat.id = sid;
        chat.creator_actor_id = owner.account_id.to_string();
        chat.members = vec![owner.member(WorkspaceRole::Owner)];
        let in_chat = |p: &Principal, l: u64, payload: SessionEvent| {
            let mut e = SessionEventEnvelope::new(sid, ws, l as i64, payload);
            e.lamport = l;
            e.actor_stamp = Some(ActorStamp { actor: p.actor(), recorded_at: Timestamp::epoch(), cfg_frontier: f, presence: None, runtime_effect: false });
            sign_envelope(&mut e, p.device_id, &p.device);
            e
        };
        let pid = Uuid::new_v4();
        let mut prop = hive_core::ActionProposal::new("p", hive_core::ProposalKind::Decision, "planner");
        prop.id = pid;
        events.push(in_chat(&owner, 200, SessionEvent::SessionSnapshot { session: Box::new(chat) }));
        events.push(in_chat(&mara, 201, SessionEvent::ProposalUpserted { proposal: prop }));
        for (i, p) in [&owner, &mara].iter().enumerate() {
            let vote = SessionEvent::ProposalVoteCast {
                proposal_id: pid,
                approval: hive_core::ProposalApproval { actor_id: p.account_id.to_string(), role: WorkspaceRole::Viewer, approved: i == 0, created_at: Timestamp::epoch(), diff_digest: None },
            };
            events.push(in_chat(p, 202 + i as u64, vote));
        }
        let mut delivery = events.clone();
        for d in &dups {
            delivery.push(events[d % events.len()].clone());
        }
        let mut rng = seed | 1;
        for i in (1..delivery.len()).rev() {
            rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17;
            delivery.swap(i, (rng % (i as u64 + 1)) as usize);
        }
        let fold = |evs: &[SessionEventEnvelope]| {
            let mut store = EventStore::open_in_memory().unwrap();
            for e in evs {
                // Through the wire, as sync delivers it.
                let back: SessionEventEnvelope = serde_json::from_str(&serde_json::to_string(e).unwrap()).unwrap();
                store.ingest(&back).unwrap();
            }
            let (cfg, timeline) = store.load_config(ws, None, None).unwrap();
            let chat = store.load_session_authorized(sid, None, None, hive_core::WorkspaceAuthz::Timeline(&timeline)).unwrap();
            (cfg.map(|c| c.members), chat.map(|c| (c.proposals, c.authz.held)), store.list(sid).unwrap().len())
        };
        let a = fold(&events);
        prop_assert_eq!(a.2, 4);
        prop_assert_eq!(&a, &fold(&delivery));
    }
}

/// Hardening A-9: an account key registration (or rotation) speaks for one
/// account, and only that account's own devices may publish it. Otherwise any
/// member, or anyone posting an unsigned body, registers a second key under a
/// victim's account id: the account becomes disputed and every event the
/// victim signs is quarantined (a lockout).
#[test]
fn only_an_account_registers_its_own_key() {
    let (owner, mara, eve) = (Principal::new("owner"), Principal::new("mara"), Principal::new("eve"));
    let ws = Uuid::new_v4();
    let stored = workspace(ws, &owner, &[&mara, &eve]);
    let roster = build_roster(&stored);
    assert_eq!(verdict_for(&roster, &mara.content(ws, 90)), Verdict::Valid, "precondition");
    let other_key = SigningKeypair::generate().unwrap();
    let forged = SessionEvent::AccountKeyRegistered { account_id: mara.account_id, signing_public_key: other_key.public_key_bytes().to_vec() };
    // A member (eve) signs it as herself...
    let by_member = eve.sign(ws, 100, forged.clone());
    // ...or nobody signs it at all.
    let mut unsigned = SessionEventEnvelope::new(workspace_config_session_id(ws), ws, 101, forged);
    unsigned.lamport = 101;
    for env in [&by_member, &unsigned] {
        assert!(
            matches!(verdict_for(&roster, env), Verdict::Quarantine(_)),
            "a registration for someone else's account was accepted: {:?}",
            verdict_for(&roster, env)
        );
    }
    // Had it been stored, mara would be locked out.
    let mut poisoned = stored.clone();
    poisoned.push(unsigned);
    assert_ne!(verdict_for(&build_roster(&poisoned), &mara.content(ws, 120)), Verdict::Valid);
    // Her own registration (idempotent) stays fine.
    let own = mara.sign(ws, 130, SessionEvent::AccountKeyRegistered { account_id: mara.account_id, signing_public_key: mara.account.public_key_bytes().to_vec() });
    assert_eq!(verdict_for(&roster, &own), Verdict::Valid);
}

/// Hardening A-10: the trust roster's own projection of the config log must
/// use the founder pin, and only a removal that projection accepted is
/// sticky. A self-enrolled joiner (below Admin; key possession is enough to
/// verify) signs a second, lower-lamport founding seed naming herself the
/// lone Owner (optionally listing the founder as a Viewer), then removes the
/// founder or revokes their device. Unpinned, her seed was the projection's
/// base and a raw removal was sticky, so the founder was locked out on every
/// device, pinned ones included.
#[test]
fn a_forged_low_lamport_seed_cannot_lock_out_the_founder() {
    for list_founder in [false, true] {
        let (owner, eve) = (Principal::new("owner"), Principal::new("eve"));
        let ws = Uuid::new_v4();
        let stored = workspace(ws, &owner, &[]);
        let fk = FounderPin::fingerprint_hex(&owner.account.public_key_bytes());
        let pin = FounderPin::parse(&owner.account_id.to_string(), &fk).unwrap();
        let mut held = stored.clone();
        held.extend(eve.trust(ws, 100));
        held.push(eve.sign(ws, 102, SessionEvent::MemberAdded { invite: None, member: eve.member(WorkspaceRole::Contributor) }));
        let mut seed = ChatSession::new("", ws, "");
        seed.id = workspace_config_session_id(ws);
        seed.creator_actor_id = eve.account_id.to_string();
        seed.members = vec![eve.member(WorkspaceRole::Owner)];
        if list_founder {
            seed.members.push(owner.member(WorkspaceRole::Viewer));
        }
        let forged = eve.sign(ws, 0, SessionEvent::SessionSnapshot { session: Box::new(seed) });
        let attack = [
            forged,
            eve.sign(ws, 103, SessionEvent::DeviceRevoked { device_id: owner.device_id }),
            eve.sign(ws, 104, SessionEvent::MemberRemoved { member_id: owner.account_id.to_string() }),
        ];
        let refs: Vec<&SessionEventEnvelope> = attack.iter().collect();
        let roster = roster_with_batch(held.clone(), &refs, Some(&pin));
        let mut after = held;
        after.extend(attack.iter().filter(|e| !matches!(verdict_for(&roster, e), Verdict::Quarantine(_))).cloned());
        let pins: HashMap<Uuid, FounderPin> = [(ws, pin)].into_iter().collect();
        let r = &build_rosters(&after, &pins)[&ws];
        assert_eq!(verdict_for(r, &owner.content(ws, 200)), Verdict::Valid, "the founder was locked out ({list_founder})");
        // Without a pin, a raw removal the projection refused isn't sticky.
        if !list_founder {
            assert_eq!(verdict_for(&build_roster(&after), &owner.content(ws, 201)), Verdict::Valid, "unpinned lockout");
        }
    }
}
