//! Phase 0, B1: signed rotations and the one keyring (K1-K4, S2/R3, R4).
//!
//! A [`World`] is a workspace config log written by real, signing devices:
//! `owner` (Owner), `ada` (Admin), `mara` (Contributor) and `eve`, who
//! published her identity but was never let in. Property tests run 256 cases
//! by default (`PROPTEST_CASES=4096` nightly).

use std::collections::BTreeMap;

use hive_core::crypto::{sign_envelope, DeviceCertificate};
use hive_core::e2ee::{seal_symmetric_kid, KeyAgreementKeypair, SealedBlob};
use hive_core::keys::{key_commitment, kid_of, SignedRotation};
use hive_core::{
    fold_decisions, project_config, workspace_config_session_id, ActorIdentity, ActorKind, ActorStamp, CfgFrontier,
    ChatMessage, ChatSession, MessageRole, SessionEvent, SessionEventEnvelope, SigningKeypair, Timestamp, Verdict,
    WorkspaceAuthz, WorkspaceMember, WorkspaceRole,
};
use proptest::prelude::*;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::keyring::{new_rotation, KeySource, KeyringEngine, SealStatus};

const PASSPHRASE: &str = "b1 test passphrase";

/// The passphrase key (Argon2id is deliberately slow: derive it once).
fn pass_key() -> [u8; 32] {
    static K: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    *K.get_or_init(|| hive_core::derive_workspace_key(PASSPHRASE))
}

struct Person {
    account_id: Uuid,
    account: SigningKeypair,
    device_id: Uuid,
    device: SigningKeypair,
    ka_seed: [u8; 32],
}

impl Person {
    fn new() -> Self {
        let device = SigningKeypair::generate().unwrap();
        Self {
            account_id: Uuid::new_v4(),
            account: SigningKeypair::generate().unwrap(),
            device_id: hive_core::crypto::device_id_for(hive_core::crypto::SigAlg::Ed25519, &device.public_key_bytes()),
            device,
            ka_seed: KeyAgreementKeypair::generate().unwrap().seed_bytes(),
        }
    }
    fn ka(&self) -> KeyAgreementKeypair {
        KeyAgreementKeypair::from_seed(&self.ka_seed).unwrap()
    }
}

const NAMES: [&str; 4] = ["owner", "ada", "mara", "eve"];

struct World {
    wid: Uuid,
    clock: u64,
    cfg: Vec<SessionEventEnvelope>,
    people: BTreeMap<&'static str, Person>,
    /// People whose test recovery key is registered: their events carry
    /// its presence (rules v2 from the seed).
    present: std::collections::BTreeSet<&'static str>,
}

fn member(name: &str, p: &Person, role: WorkspaceRole) -> WorkspaceMember {
    let mut actor = ActorIdentity::new(name, name, ActorKind::Human);
    actor.account_id = Some(p.account_id);
    WorkspaceMember { id: name.into(), actor, role, title: String::new(), index: 0, joined_at: Timestamp::epoch() }
}

impl World {
    fn new() -> Self {
        Self::new_with(|_| {})
    }

    /// A world whose people `edit` adjusts before anything is written (for
    /// example, an attacker's log that registers its own key under someone
    /// else's account id).
    fn new_with(edit: impl FnOnce(&mut BTreeMap<&'static str, Person>)) -> Self {
        let wid = Uuid::new_v4();
        let mut people: BTreeMap<&'static str, Person> = NAMES.iter().map(|n| (*n, Person::new())).collect();
        edit(&mut people);
        let mut w = Self { wid, clock: 0, cfg: Vec::new(), people, present: Default::default() };
        let mut seed = ChatSession::new("", wid, "");
        seed.id = workspace_config_session_id(wid);
        let owner = w.aid("owner");
        seed.creator_actor_id = owner.clone();
        seed.members = vec![member(&owner, &w.people["owner"], WorkspaceRole::Owner)];
        w.push("owner", SessionEvent::SessionSnapshot { session: Box::new(seed) });
        for name in NAMES {
            w.publish_identity(name);
        }
        w.register_recovery("owner");
        for (name, role) in [("ada", WorkspaceRole::Admin), ("mara", WorkspaceRole::Contributor)] {
            let m = member(name, &w.people[name], role);
            w.push("owner", SessionEvent::MemberAdded { member: m, invite: None, proof: None });
            w.register_recovery(name);
        }
        for name in NAMES {
            let ka = w.people[name].ka().public_key_bytes().to_vec();
            let device_id = w.people[name].device_id;
            w.push(name, SessionEvent::DeviceKeyAgreementPublished { device_id, ka_public: ka });
        }
        w
    }

    /// `name` registers their test recovery key (it proves itself; the
    /// founder's grant or the Admin's who added them lets it register).
    fn register_recovery(&mut self, name: &'static str) {
        let account = self.people[name].account_id;
        let mut env = self.draft(name, hive_core::testkit::recovery_registration(account));
        let p = &self.people[name];
        hive_core::testkit::prove_recovery(&mut env, account, p.device_id, &p.device);
        self.cfg.push(env);
        self.present.insert(name);
    }

    fn publish_identity(&mut self, name: &'static str) {
        let p = &self.people[name];
        let (account_id, pk) = (p.account_id, p.account.public_key_bytes().to_vec());
        let cert = DeviceCertificate::issue(&p.account, p.account_id, &p.device.public_key_bytes(), Timestamp::epoch());
        self.push(name, SessionEvent::AccountKeyRegistered { account_id, signing_public_key: pk });
        self.push(name, SessionEvent::DeviceCertificateAdded { certificate: cert });
    }

    /// The actor id `name` signs as: the owner is the founder, whose actor
    /// id is its account id (what a founder pin names); the rest keep names.
    fn aid(&self, name: &str) -> String {
        if name == "owner" {
            self.people["owner"].account_id.to_string()
        } else {
            name.to_string()
        }
    }

    fn head(&self) -> Option<CfgFrontier> {
        hive_core::lamport::plausible_keys(&self.cfg).last().copied()
    }

    fn draft(&mut self, by: &str, payload: SessionEvent) -> SessionEventEnvelope {
        self.clock += 1;
        let p = &self.people[by];
        let mut env =
            SessionEventEnvelope::new(workspace_config_session_id(self.wid), self.wid, self.clock as i64, payload);
        let mut actor = ActorIdentity::new(self.aid(by), by, ActorKind::Human);
        actor.account_id = Some(p.account_id);
        env.actor_stamp = Some(ActorStamp { actor, recorded_at: Timestamp::epoch(), cfg_frontier: self.head(), presence: None, runtime_effect: false, prev: None, sig_alg: Default::default(), log_head: None });
        hive_core::lamport::stamp_after(&mut env, &self.cfg);
        sign_envelope(&mut env, p.device_id, &p.device);
        if self.present.contains(by) {
            hive_core::testkit::prove_recovery(&mut env, p.account_id, p.device_id, &p.device);
        }
        env
    }

    fn push(&mut self, by: &str, payload: SessionEvent) -> Uuid {
        let env = self.draft(by, payload);
        let id = env.event_id;
        self.cfg.push(env);
        id
    }

    fn timeline(&self) -> hive_core::authz::ConfigTimeline {
        project_config(&self.cfg, None, None).1
    }

    /// A rotation following the current authorized head, sealed to the
    /// roster's recipients (as `ChatService::rotate_workspace_key` builds it).
    fn rotation(&self) -> (SignedRotation, [u8; 32]) {
        new_rotation(self.wid, &self.timeline()).unwrap()
    }

    fn rotate(&mut self, by: &str) -> (SignedRotation, [u8; 32]) {
        let (r, k) = self.rotation();
        self.push(by, SessionEvent::WorkspaceKeyRotated { rotation: r.clone() });
        (r, k)
    }

    fn source(&self, name: &str) -> KeySource {
        let p = &self.people[name];
        KeySource {
            device_id: Some(p.device_id),
            ka: Some(p.ka()),
            passphrase: Some(pass_key()),
            delivered: Vec::new(),
        }
    }

    fn engine(&self, name: &str, rows: &[Value]) -> KeyringEngine {
        KeyringEngine::build(self.wid, Some(&self.timeline()), &self.source(name), rows)
    }

    /// A rotation body sealed to every device that published a key (what an
    /// outsider holding the directory's keys can build), at the next version.
    fn forged_rotation(&self) -> (SignedRotation, [u8; 32]) {
        let t = self.timeline();
        let head = t.current().key_epochs.head_version();
        let key = hive_core::generate_workspace_key().unwrap();
        let sealed = self
            .people
            .values()
            .map(|p| (p.device_id.to_string(), hive_core::seal(&p.ka().public_key_bytes(), &key).unwrap()))
            .collect();
        let r = SignedRotation {
            version: head + 1,
            prev_version: head,
            sealed,
            key_commitment: key_commitment(self.wid, head + 1, &key),
        };
        (r, key)
    }
}

fn passphrase_kid(wid: Uuid) -> String {
    kid_of(&key_commitment(wid, 0, &pass_key()))
}

#[test]
fn honest_rotation_seals_for_every_member_device_and_not_for_outsiders() {
    let mut w = World::new();
    let before = w.engine("owner", &[]);
    assert_eq!(before.seal_key().unwrap().version, 0, "no rotation yet: the passphrase epoch seals");
    let (r, key) = w.rotate("owner");
    assert_eq!(r.version, 1);
    for name in ["owner", "ada", "mara"] {
        let e = w.engine(name, &[]);
        let seal = e.seal_key().unwrap_or_else(|| panic!("{name} can seal"));
        assert_eq!((seal.version, seal.key), (1, key), "{name} seals under the rotation");
        assert_eq!(seal.kid, kid_of(&r.key_commitment));
        assert_eq!(e.keys_for(0, None).len(), 1, "{name} still reads the passphrase epoch");
    }
    let eve = w.engine("eve", &[]);
    assert!(!r.sealed.contains_key(&w.people["eve"].device_id.to_string()), "an outsider is never a recipient");
    assert_eq!(eve.seal_status(), SealStatus::NotARecipient { version: 1 });
    assert!(eve.keys_for(1, None).is_empty());
}

#[test]
fn rotation_rules_contributor_outsider_unsigned_forged_and_gaps_are_denied() {
    let mut w = World::new();
    let ok = |w: &World, id: Uuid| fold_decisions(&w.cfg, None, None, WorkspaceAuthz::Unchecked)[&id] == Verdict::Allowed;

    let (r, _) = w.rotation();
    let id = w.push("mara", SessionEvent::WorkspaceKeyRotated { rotation: r.clone() });
    assert!(!ok(&w, id), "a Contributor can't rotate");
    let id = w.push("eve", SessionEvent::WorkspaceKeyRotated { rotation: r.clone() });
    assert!(!ok(&w, id), "a non-member can't rotate");

    // Unsigned, and signed by eve's device while claiming ada's device.
    let mut unsigned = w.draft("ada", SessionEvent::WorkspaceKeyRotated { rotation: r.clone() });
    unsigned.signer_device_id = None;
    unsigned.signature = None;
    let uid = unsigned.event_id;
    w.cfg.push(unsigned);
    assert!(!ok(&w, uid), "an unsigned rotation never counts");
    let mut forged = w.draft("ada", SessionEvent::WorkspaceKeyRotated { rotation: r.clone() });
    sign_envelope(&mut forged, w.people["ada"].device_id, &w.people["eve"].device);
    let fid = forged.event_id;
    w.cfg.push(forged);
    assert!(!ok(&w, fid), "a rotation whose signature doesn't verify never counts");

    // A gap, and a replay of the same version.
    let mut gap = r.clone();
    gap.version = 2;
    let id = w.push("ada", SessionEvent::WorkspaceKeyRotated { rotation: gap });
    assert!(!ok(&w, id), "versions follow the head by one");
    let id = w.push("ada", SessionEvent::WorkspaceKeyRotated { rotation: r.clone() });
    assert!(ok(&w, id), "ada (Admin) rotates");
    let id = w.push("owner", SessionEvent::WorkspaceKeyRotated { rotation: r });
    assert!(!ok(&w, id), "the same version twice: the second is stale");
    assert_eq!(w.timeline().current().key_epochs.head_version(), 1);

    // A key-agreement key published for another device is refused.
    let target = w.people["owner"].device_id;
    let id = w.push("eve", SessionEvent::DeviceKeyAgreementPublished { device_id: target, ka_public: vec![9; 32] });
    assert!(!ok(&w, id), "a device publishes only its own key");
    assert_eq!(w.timeline().current().ka_keys[&target], w.people["owner"].ka().public_key_bytes().to_vec());
}

/// The model: a rotation is authorized iff an Admin+ member issued it against
/// the authorized head. Everything else (Contributors, outsiders, unsigned or
/// forged envelopes, relay rows) never moves the seal.
#[derive(Debug, Clone)]
enum Op {
    Rotate(u8),
    Unsigned,
    Forged,
    RelayRow,
    RemoveMara,
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0u8..4).prop_map(Op::Rotate),
        Just(Op::Unsigned),
        Just(Op::Forged),
        Just(Op::RelayRow),
        Just(Op::RemoveMara),
    ]
}

proptest! {
    #[test]
    fn prop_keyring_only_from_authorized_rotations(ops in proptest::collection::vec(arb_op(), 1..8)) {
        let mut w = World::new();
        let mut rows: Vec<Value> = Vec::new();
        // (version, kid, key) of the authorized head, and whether mara got it.
        let mut head: Option<(u32, String, [u8; 32])> = None;
        let mut mara_member = true;
        let mut mara_has_head = true;
        for op in ops {
            match op {
                Op::Rotate(by) => {
                    let by = NAMES[by as usize];
                    let (r, k) = w.rotation();
                    w.push(by, SessionEvent::WorkspaceKeyRotated { rotation: r.clone() });
                    if by == "owner" || by == "ada" {
                        mara_has_head = r.sealed.contains_key(&w.people["mara"].device_id.to_string());
                        head = Some((r.version, kid_of(&r.key_commitment), k));
                    }
                }
                Op::Unsigned => {
                    let (r, _) = w.rotation();
                    let mut env = w.draft("ada", SessionEvent::WorkspaceKeyRotated { rotation: r });
                    env.signer_device_id = None;
                    env.signature = None;
                    w.cfg.push(env);
                }
                Op::Forged => {
                    let (r, _) = w.forged_rotation();
                    let mut env = w.draft("owner", SessionEvent::WorkspaceKeyRotated { rotation: r });
                    sign_envelope(&mut env, w.people["owner"].device_id, &w.people["eve"].device);
                    w.cfg.push(env);
                }
                Op::RelayRow => {
                    let (r, _) = w.forged_rotation();
                    rows.push(serde_json::to_value(&r).unwrap());
                }
                Op::RemoveMara => {
                    if mara_member {
                        w.push("owner", SessionEvent::MemberRemoved { member_id: "mara".into(), watermark: Vec::new() });
                        mara_member = false;
                    }
                }
            }
        }
        for name in ["owner", "ada", "mara", "eve"] {
            let e = w.engine(name, &rows);
            let expected = match (&head, name) {
                (None, "eve") => None,
                (None, _) => Some((0, passphrase_kid(w.wid))),
                (Some(_), "eve") => None,
                (Some(_), "mara") if !mara_has_head => None,
                (Some((v, kid, _)), _) => Some((*v, kid.clone())),
            };
            // eve holds the passphrase too in this world, so before any
            // rotation she could seal at epoch 0 (an outsider's writes are
            // refused by authz, not by the key).
            let expected = if name == "eve" && head.is_none() { Some((0, passphrase_kid(w.wid))) } else { expected };
            let got = e.seal_key().map(|s| (s.version, s.kid.clone()));
            prop_assert_eq!(got, expected, "{} seals only under the authorized head", name);
            if let (Some((v, kid, k)), Some(s)) = (&head, e.seal_key()) {
                if s.version == *v {
                    prop_assert_eq!(&s.kid, kid);
                    prop_assert_eq!(s.key, *k);
                }
            }
        }
    }

    #[test]
    fn prop_concurrent_rotations_converge(swap in any::<bool>(), lamports in (0u64..3, 0u64..3), perm in Just(()).prop_perturb(|_, mut rng| rng.next_u64())) {
        let mut w = World::new();
        // Both Admins rotate against the same head.
        let (ra, ka) = w.rotation();
        let (rb, kb) = w.rotation();
        let base = w.clock;
        let mut ea = w.draft("owner", SessionEvent::WorkspaceKeyRotated { rotation: ra.clone() });
        let mut eb = w.draft("ada", SessionEvent::WorkspaceKeyRotated { rotation: rb.clone() });
        // Both stamped with the same frontier; lamports may collide.
        let f = w.head();
        for (env, l, p) in [(&mut ea, lamports.0, "owner"), (&mut eb, lamports.1, "ada")] {
            // Drafted `l` config events ago (an author lagging): lamports may
            // collide.
            hive_core::lamport::stamp_back(env, &w.cfg, l as usize);
            env.actor_stamp.as_mut().unwrap().cfg_frontier = f;
            let who = &w.people[p];
            hive_core::testkit::prove_recovery(env, who.account_id, who.device_id, &who.device);
        }
        let winner = if ea.canonical_key() < eb.canonical_key() { (&ra, ka) } else { (&rb, kb) };
        let loser = if ea.canonical_key() < eb.canonical_key() { (&rb, kb) } else { (&ra, ka) };
        if swap { w.cfg.push(eb); w.cfg.push(ea); } else { w.cfg.push(ea); w.cfg.push(eb); }
        w.clock = base + 3;
        // Delivery order never matters: shuffle the whole log per device.
        let mut views = Vec::new();
        for (i, name) in ["owner", "ada", "mara"].iter().enumerate() {
            let mut log = w.cfg.clone();
            let n = log.len();
            let mut x = perm.wrapping_add((i as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)) | 1;
            for j in (1..n).rev() {
                x ^= x << 13; x ^= x >> 7; x ^= x << 17;
                log.swap(j, (x % (j as u64 + 1)) as usize);
            }
            let t = project_config(&log, None, None).1;
            let e = KeyringEngine::build(w.wid, Some(&t), &w.source(name), &[]);
            views.push((name, e));
        }
        for (name, e) in &views {
            let s = e.seal_key().expect("every member device seals");
            prop_assert_eq!((s.version, &s.kid, s.key), (1, &kid_of(&winner.0.key_commitment), winner.1), "{} seals under the winner", name);
            // K2: the loser's key stays readable, picked by kid.
            let lk = e.keys_for(1, Some(&kid_of(&loser.0.key_commitment)));
            prop_assert_eq!(lk, vec![&loser.1], "{} reads the loser's events", name);
            prop_assert!(e.lost_rotations().contains(&(1, kid_of(&loser.0.key_commitment))));
        }
        // No lost events: a body sealed under the loser's key opens everywhere.
        let body = seal_symmetric_kid(&loser.1, 1, &kid_of(&loser.0.key_commitment), b"written under the losing key").unwrap();
        for (_, e) in &views {
            let k = e.keys_for(body.version, body.kid.as_deref());
            prop_assert!(k.iter().any(|k| hive_core::open_symmetric(k, &body).is_ok()));
        }
        // The loser's issuer rotates again: version 2 is authorized.
        w.rotate("ada");
        prop_assert_eq!(w.engine("mara", &[]).seal_key().unwrap().version, 2);
    }

    #[test]
    fn junk_relay_rotations_never_seal(junk in proptest::collection::vec(arb_junk(), 0..12)) {
        let mut w = World::new();
        let (r, key) = w.rotate("owner");
        let mut rows: Vec<Value> = junk.iter().map(|j| j.row(&w)).collect();
        let (forged, _) = w.forged_rotation();
        rows.push(serde_json::to_value(forged).unwrap());
        for name in ["owner", "ada", "mara"] {
            let e = w.engine(name, &rows);
            let s = e.seal_key().unwrap();
            prop_assert_eq!((s.version, s.key), (1, key));
            prop_assert_eq!(&s.kid, &kid_of(&r.key_commitment));
            // Every key in the ring matches its own commitment.
            for (v, kid) in e.readable() {
                for k in e.keys_for(v, Some(&kid)) {
                    prop_assert_eq!(kid_of(&key_commitment(w.wid, v, k)), kid.clone());
                }
            }
        }
    }
}

/// Junk a relay (or anyone who can POST to the keyring route) may serve.
#[derive(Debug, Clone)]
enum Junk {
    Garbage(String),
    Legacy,
    BadCommitment,
    MismatchedBlob,
    Huge,
}

fn arb_junk() -> impl Strategy<Value = Junk> {
    prop_oneof![
        ".{0,40}".prop_map(Junk::Garbage),
        Just(Junk::Legacy),
        Just(Junk::BadCommitment),
        Just(Junk::MismatchedBlob),
        Just(Junk::Huge),
    ]
}

impl Junk {
    fn row(&self, w: &World) -> Value {
        let owner = &w.people["owner"];
        let key = hive_core::generate_workspace_key().unwrap();
        let blob = hive_core::seal(&owner.ka().public_key_bytes(), &key).unwrap();
        let sealed = BTreeMap::from([(owner.device_id.to_string(), blob)]);
        match self {
            Junk::Garbage(s) => json!({ "kind": s, "sealed": s }),
            // A Hive 1.x unsigned rotation sealed to us at a higher version.
            Junk::Legacy => json!({ "version": 9, "sealed": sealed }),
            Junk::BadCommitment => {
                serde_json::to_value(SignedRotation { version: 2, prev_version: 1, sealed, key_commitment: "0".repeat(64) }).unwrap()
            }
            Junk::MismatchedBlob => {
                // The commitment is to another key than the one sealed.
                let other = hive_core::generate_workspace_key().unwrap();
                serde_json::to_value(SignedRotation {
                    version: 2,
                    prev_version: 1,
                    sealed,
                    key_commitment: key_commitment(w.wid, 2, &other),
                })
                .unwrap()
            }
            Junk::Huge => json!({
                "version": u32::MAX,
                "prevVersion": u32::MAX - 1,
                "sealed": { "x": SealedBlob { ephemeral_public: vec![0; 32], ciphertext: vec![1; 64] } },
                "keyCommitment": "zz",
            }),
        }
    }
}

/// Attachments follow the keyring (B1): once a removed member is rotated
/// out, an attachment shared afterwards is sealed under the new epoch, not
/// the passphrase epoch they still hold.
#[test]
fn removed_member_cannot_read_later_attachments() {
    use crate::attachment_blob::{open_framed, open_with_ring, seal_framed, seal_with_ring};
    let mut w = World::new();
    w.push("owner", SessionEvent::MemberRemoved { member_id: "mara".into(), watermark: Vec::new() });
    w.rotate("owner");
    let data = b"quarterly numbers";
    // Control: the passphrase epoch is exactly what mara kept.
    assert_eq!(open_framed(&pass_key(), &seal_framed(&pass_key(), 0, data).unwrap()).as_deref(), Some(&data[..]));
    let framed = seal_with_ring(&w.engine("owner", &[]), data).unwrap();
    assert_eq!(u32::from_le_bytes(framed[0..4].try_into().unwrap()), 1, "sealed at the authorized head");
    assert_eq!(open_framed(&pass_key(), &framed), None, "the passphrase doesn't open it");
    assert_eq!(open_with_ring(&w.engine("mara", &[]), &framed), None, "the removed member can't read it");
    assert_eq!(open_with_ring(&w.engine("ada", &[]), &framed).as_deref(), Some(&data[..]));
    // Older attachments (epoch 0) still open for current members.
    let old = seal_framed(&pass_key(), 0, data).unwrap();
    assert_eq!(open_with_ring(&w.engine("ada", &[]), &old).as_deref(), Some(&data[..]));
    // A device that isn't a recipient of the head refuses to seal.
    assert!(seal_with_ring(&w.engine("mara", &[]), data).is_err());
}

/// A removed member's device stays certified (nobody revoked it), so it must
/// not come back as a rotation recipient through an agent binding: a member
/// who binds an agent's host to that device id would otherwise have the next
/// rotation seal the new key to it.
#[test]
fn removed_members_device_is_not_a_recipient_through_an_agent_host() {
    let mut w = World::new();
    w.push("owner", SessionEvent::MemberRemoved { member_id: "mara".into(), watermark: Vec::new() });
    let mut agent = hive_core::WorkspaceAgent::new("helper", "rt");
    agent.host_id = w.people["mara"].device_id.to_string();
    agent.owner_actor_id = "ada".into();
    let id = w.push("ada", SessionEvent::AgentUpserted { agent });
    assert_eq!(fold_decisions(&w.cfg, None, None, WorkspaceAuthz::Unchecked)[&id], Verdict::Allowed, "control: an Admin binds the agent");
    let recipients = w.timeline().current().rotation_recipients();
    assert!(!recipients.contains_key(&w.people["mara"].device_id), "the removed member's device would get the next key");
}

/// Host beats are unsigned liveness facts; after a rotation, a removed
/// member still holding the passphrase can neither read them nor make a host
/// look online: readers take only beats sealed at the authorized head.
#[test]
fn removed_member_cannot_forge_or_read_host_beats() {
    use crate::host_presence::{host_presence_slot, open_host_beats, seal_host_beat};
    let mut w = World::new();
    w.push("owner", SessionEvent::MemberRemoved { member_id: "mara".into(), watermark: Vec::new() });
    w.rotate("owner");
    let owner = w.engine("owner", &[]);
    // mara seals with what she still has: the passphrase epoch.
    let mara = KeyringEngine::passphrase(w.wid, pass_key());
    let mut blobs = std::collections::HashMap::new();
    blobs.insert(host_presence_slot("box"), seal_host_beat(&mara, "box", 1_000).unwrap());
    blobs.insert(host_presence_slot("desk"), seal_host_beat(&owner, "desk", 1_000).unwrap());
    let beats = open_host_beats(&blobs, &w.engine("ada", &[]), 5_000);
    assert_eq!(beats.get("desk"), Some(&1_000), "a beat at the authorized head");
    assert_eq!(beats.get("box"), None, "a forged beat under the old epoch");
    assert!(!open_host_beats(&blobs, &mara, 5_000).contains_key("desk"), "mara can't read the new beats");
}

#[test]
fn removed_member_cannot_inject_epoch() {
    // K1: mara is removed and the key rotated; she then publishes a rotation
    // of her own (signed by her still-certified device) and posts a body
    // sealed to everyone to the relay keyring route.
    let mut w = World::new();
    w.push("owner", SessionEvent::MemberRemoved { member_id: "mara".into(), watermark: Vec::new() });
    let (r, key) = w.rotate("owner");
    assert!(!r.sealed.contains_key(&w.people["mara"].device_id.to_string()), "the removed member isn't a recipient");
    let mara = w.engine("mara", &[]);
    assert_eq!(mara.seal_status(), SealStatus::NotARecipient { version: 1 }, "she can't read or write the new epoch");
    assert!(mara.keys_for(1, None).is_empty());

    let (inj, _) = w.forged_rotation();
    let id = w.push("mara", SessionEvent::WorkspaceKeyRotated { rotation: inj.clone() });
    let verdicts = fold_decisions(&w.cfg, None, None, WorkspaceAuthz::Unchecked);
    assert_eq!(verdicts[&id], Verdict::Denied, "a non-member's rotation is denied");
    let rows = vec![serde_json::to_value(&inj).unwrap()];
    for name in ["owner", "ada"] {
        let e = w.engine(name, &rows);
        assert_eq!(e.seal_key().unwrap().key, key, "{name} never seals under the injected key");
        assert_eq!(e.authorized_head(), 1);
    }
}

#[test]
fn outsider_with_directory_ka_cannot_inject_epoch() {
    // Review §7.9: eve knows every device's key-agreement key (a relay
    // directory hands them out) and was never a member.
    let w = World::new();
    let (inj, _) = w.forged_rotation();
    let rows = vec![serde_json::to_value(&inj).unwrap()];
    for name in ["owner", "ada", "mara"] {
        let e = w.engine(name, &rows);
        assert_eq!(e.seal_key().unwrap().version, 0, "{name} keeps sealing under the passphrase epoch");
        assert_eq!(e.keys_for(1, None).len(), 1, "{name} may read with it (harmless: every event is still authz-checked)");
    }
    // And the directory is never a recipient source: only published keys.
    let t = w.timeline();
    let recipients = t.current().rotation_recipients();
    assert!(!recipients.contains_key(&w.people["eve"].device_id));
    assert_eq!(recipients.len(), 3);
}

#[test]
fn legacy_unsigned_rows_are_never_read_or_sealed() {
    // 0.1 starts from scratch: an unsigned `WorkspaceKeyRotation` row sealed
    // to this device is ignored entirely.
    let w = World::new();
    let owner = &w.people["owner"];
    let key = hive_core::generate_workspace_key().unwrap();
    let legacy = hive_core::WorkspaceKeyRotation::seal_for_devices(
        3,
        &key,
        &[(owner.device_id.to_string(), owner.ka().public_key_bytes().to_vec())],
    )
    .unwrap();
    let e = w.engine("owner", &[serde_json::to_value(legacy).unwrap()]);
    assert!(e.keys_for(3, None).is_empty());
    assert_eq!(e.seal_key().unwrap().version, 0);
}

#[test]
fn stale_epoch_write_refused_after_frontier() {
    // S2 / R3: an event that arrived sealed under epoch 0 although its author
    // had folded rotation 1 is refused; one stamped before the rotation, or
    // sealed at 1, is accepted. Config log and chat log alike.
    let mut w = World::new();
    let before = w.head();
    w.rotate("owner");
    let after = w.head();
    let wid = w.wid;

    let mut chat = ChatSession::new("chat", wid, "anthropic");
    chat.creator_actor_id = w.aid("owner");
    chat.members = vec![member(&w.aid("owner"), &w.people["owner"], WorkspaceRole::Owner)];
    let sid = chat.id;
    let mut log = Vec::new();
    let mut clock = 1000;
    let mut chat_event = |by: &str, payload: SessionEvent, f: Option<CfgFrontier>, sealed: Option<u32>, w: &World| {
        clock += 1;
        let p = &w.people[by];
        let mut env = SessionEventEnvelope::new(sid, wid, clock, payload);
        let mut actor = ActorIdentity::new(w.aid(by), by, ActorKind::Human);
        actor.account_id = Some(p.account_id);
        env.actor_stamp = Some(ActorStamp { actor, recorded_at: Timestamp::epoch(), cfg_frontier: f, presence: None, runtime_effect: false, prev: None, sig_alg: Default::default(), log_head: None });
        hive_core::lamport::stamp_after(&mut env, &log);
        sign_envelope(&mut env, p.device_id, &p.device);
        env.set_sealed_epoch(sealed);
        let id = env.event_id;
        log.push(env);
        id
    };
    chat_event("owner", SessionEvent::SessionSnapshot { session: Box::new(chat) }, after, None, &w);
    let msg = |t: &str| SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "ada", t) };
    let stale = chat_event("ada", msg("stale"), after, Some(0), &w);
    let old_frontier = chat_event("ada", msg("written before the rotation"), before, Some(0), &w);
    let current = chat_event("ada", msg("current"), after, Some(1), &w);
    let local = chat_event("ada", msg("authored here"), after, None, &w);
    let t = w.timeline();
    let v = fold_decisions(&log, None, None, WorkspaceAuthz::Timeline(&t));
    assert_eq!(v[&stale], Verdict::Denied, "sealed under 0 after folding rotation 1");
    assert_eq!(v[&old_frontier], Verdict::Allowed);
    assert_eq!(v[&current], Verdict::Allowed);
    assert_eq!(v[&local], Verdict::Allowed, "events authored here carry no sealed epoch");

    // On the config log.
    let mut env = w.draft("ada", SessionEvent::ChannelCreated { channel: hive_core::Channel::new("ops", "ops") });
    env.set_sealed_epoch(Some(0));
    let id = env.event_id;
    w.cfg.push(env);
    let v = fold_decisions(&w.cfg, None, None, WorkspaceAuthz::Unchecked);
    assert_eq!(v[&id], Verdict::Denied);
}

#[test]
fn host_parity_keyring() {
    // The desktop and serve build the ring through the same engine; over the
    // same inputs (the one `KeySource` shape every host fills) the rings are
    // byte-identical, including through `SyncEngine::with_key_source`.
    let mut w = World::new();
    w.rotate("ada");
    let rows = vec![serde_json::to_value(w.forged_rotation().0).unwrap()];
    let a = w.engine("mara", &rows);
    let b = w.engine("mara", &rows);
    assert_eq!(a.fingerprint(), b.fingerprint());

    let dir = tempfile::tempdir().unwrap();
    let mut store = crate::EventStore::open(dir.path().join("h.db")).unwrap();
    for env in &w.cfg {
        store.ingest(env).unwrap();
    }
    let owner = &w.people["owner"];
    let fk = crate::envelope_verifier::FounderPin::fingerprint_hex(&owner.account.public_key_bytes());
    store.set_workspace_pin(w.wid, &owner.account_id.to_string(), &fk).unwrap();
    let p = &w.people["mara"];
    let src = KeySource::for_device(p.device_id, p.ka(), Some(PASSPHRASE), &[]);
    let mut engine = crate::SyncEngine::new(crate::RelayClient::new("http://127.0.0.1:9"), "parity")
        .with_room_workspace(w.wid)
        .with_key_source(src);
    engine.refresh_keyring(&store).unwrap();
    assert_eq!(engine.keyring().fingerprint(), w.engine("mara", &[]).fingerprint());
    let _ = &mut store;
}

#[test]
fn ingest_refuses_old_key_writes_from_non_members() {
    // A removed member keeps the old key; once this device knows of the
    // removal and the rotation, their writes under the old key are refused.
    let mut w = World::new();
    w.push("owner", SessionEvent::MemberRemoved { member_id: "mara".into(), watermark: Vec::new() });
    w.rotate("owner");
    let head = w.timeline().current().clone();
    let mut env = w.draft("mara", SessionEvent::ChannelCreated { channel: hive_core::Channel::new("x", "x") });
    env.set_sealed_epoch(Some(0));
    assert!(crate::sync_engine::late_from_outsider(Some(&head), &env));
    env.set_sealed_epoch(Some(1));
    assert!(!crate::sync_engine::late_from_outsider(Some(&head), &env), "the head epoch is never refused here");
    let mut ada = w.draft("ada", SessionEvent::ChannelCreated { channel: hive_core::Channel::new("y", "y") });
    ada.set_sealed_epoch(Some(0));
    assert!(!crate::sync_engine::late_from_outsider(Some(&head), &ada), "members are judged by the fold");
}

// ── Integration: serve and worker over the test relay (R4 / K4) ────────────

fn service(path: &std::path::Path, name: &str) -> (crate::ChatService, KeyAgreementKeypair, Uuid) {
    let store = crate::EventStore::open(path).unwrap();
    let account = Uuid::new_v4();
    let mut author = ActorIdentity::new(account.to_string(), name, ActorKind::Human);
    author.account_id = Some(account);
    let svc = crate::ChatService::new(store, SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), author);
    let device = svc.device_id();
    (svc, KeyAgreementKeypair::generate().unwrap(), device)
}

/// What an invite does on a joining device: the founder pin.
fn pin_like_an_invite(joiner: &crate::ChatService, founder: &crate::ChatService, wid: Uuid) {
    let founder_id = founder.author().account_id.unwrap().to_string();
    let fk = crate::envelope_verifier::FounderPin::fingerprint_hex(&founder.account_public_key());
    joiner.store().set_workspace_pin(wid, &founder_id, &fk).unwrap();
}

async fn spawn_relay() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, crate::test_relay::router()).await.unwrap() });
    format!("http://{addr}")
}

fn engine(relay: &str, room: &str, device: Uuid, ka: &KeyAgreementKeypair, delivered: &[crate::keyring::DeliveredKey]) -> crate::SyncEngine {
    let src = KeySource::for_device(device, KeyAgreementKeypair::from_seed(&ka.seed_bytes()).unwrap(), Some(PASSPHRASE), delivered);
    crate::SyncEngine::new(crate::RelayClient::new(relay), room).with_key_source(src)
}

async fn round(e: &mut crate::SyncEngine, db: &std::path::Path) {
    let mut store = crate::EventStore::open(db).unwrap();
    for _ in 0..4 {
        e.push_new(&store).await.unwrap();
        e.pull_round(&mut store).await.unwrap();
    }
}

fn bodies(db: &std::path::Path, chat: Uuid) -> Vec<String> {
    let (svc, _, _) = service(db, "reader");
    svc.load(chat).unwrap().map(|s| s.messages.iter().map(|m| m.body.clone()).collect()).unwrap_or_default()
}

fn sealed_epoch_of(db: &std::path::Path, chat: Uuid, body: &str) -> Option<u32> {
    let store = crate::EventStore::open(db).unwrap();
    let env = store
        .list(chat)
        .unwrap()
        .into_iter()
        .find(|e| matches!(&e.payload, SessionEvent::MessageAppended { message } if message.body == body))?;
    store.sealed_epoch_of(env.event_id).unwrap()
}

#[tokio::test]
async fn cli_host_reads_and_seals_after_rotation() {
    let relay = spawn_relay().await;
    let room = "ws-b1-rotation";
    let wid = hive_core::room_workspace_id(room);
    let dir = tempfile::tempdir().unwrap();
    let (desk_db, serve_db, worker_db) = (dir.path().join("desk.db"), dir.path().join("serve.db"), dir.path().join("worker.db"));

    // The desktop founds the workspace, publishes its key-agreement key and
    // invites the serve box.
    let (mut desk, desk_ka, desk_dev) = service(&desk_db, "owner");
    desk.ensure_workspace_config(wid).unwrap();
    let chat = desk.create_chat("Home", wid, "anthropic").unwrap().id;
    desk.publish_key_agreement(wid, desk_ka.public_key_bytes()).unwrap();
    let invite = desk.issue_invite(wid).unwrap();
    let mut desk_sync = engine(&relay, room, desk_dev, &desk_ka, &[]);
    round(&mut desk_sync, &desk_db).await;

    // serve joins with the invite (passphrase only), enrolls, publishes its key.
    let (mut serve, serve_ka, serve_dev) = service(&serve_db, "serve-box");
    serve.store().set_join_invite(wid, invite).unwrap();
    pin_like_an_invite(&serve, &desk, wid);
    let mut serve_sync = engine(&relay, room, serve_dev, &serve_ka, &[]);
    round(&mut serve_sync, &serve_db).await;
    serve.ensure_self_member(chat, wid).unwrap();
    serve.publish_key_agreement(wid, serve_ka.public_key_bytes()).unwrap();
    round(&mut serve_sync, &serve_db).await;
    round(&mut desk_sync, &desk_db).await;

    // The desktop rotates and posts at the new epoch.
    let issued = desk.rotate_workspace_key(wid).unwrap();
    assert_eq!(issued.rotation.version, 1);
    assert!(issued.rotation.sealed.contains_key(&serve_dev.to_string()), "serve is a recipient");
    crate::keyring::publish_rotation(&crate::RelayClient::new(&relay), room, &issued.rotation).await.unwrap();
    desk.post_user_message(chat, wid, "after the rotation").unwrap();
    round(&mut desk_sync, &desk_db).await;
    assert_eq!(desk_sync.keyring().seal_key().unwrap().version, 1);

    // serve reads it at epoch 1, and seals its own post at epoch 1.
    round(&mut serve_sync, &serve_db).await;
    assert!(bodies(&serve_db, chat).contains(&"after the rotation".to_string()));
    assert_eq!(sealed_epoch_of(&serve_db, chat, "after the rotation"), Some(1));
    assert_eq!(serve_sync.keyring().seal_key().unwrap().version, 1, "serve seals at the new epoch (not stuck on 0)");
    serve.post_user_message(chat, wid, "from serve").unwrap();
    round(&mut serve_sync, &serve_db).await;
    round(&mut desk_sync, &desk_db).await;
    assert!(bodies(&desk_db, chat).contains(&"from serve".to_string()));
    assert_eq!(sealed_epoch_of(&desk_db, chat, "from serve"), Some(1));

    // A worker joins later with an invite that delivers the epoch keys: it
    // reads the whole history and seals at the head before any rotation
    // names it.
    let src = KeySource::for_device(desk_dev, KeyAgreementKeypair::from_seed(&desk_ka.seed_bytes()).unwrap(), Some(PASSPHRASE), &[]);
    let delivered = desk.delivered_epoch_keys(wid, &src).unwrap();
    assert_eq!(delivered.iter().map(|d| d.v).collect::<Vec<_>>(), vec![1]);
    let invite = desk.issue_invite(wid).unwrap();
    round(&mut desk_sync, &desk_db).await;
    let (mut worker, worker_ka, worker_dev) = service(&worker_db, "worker-box");
    worker.store().set_join_invite(wid, invite).unwrap();
    pin_like_an_invite(&worker, &desk, wid);
    let mut worker_sync = engine(&relay, room, worker_dev, &worker_ka, &delivered);
    round(&mut worker_sync, &worker_db).await;
    let seen = bodies(&worker_db, chat);
    assert!(seen.contains(&"after the rotation".to_string()) && seen.contains(&"from serve".to_string()), "{seen:?}");
    assert_eq!(worker_sync.keyring().seal_key().unwrap().version, 1);
    worker.ensure_self_member(chat, wid).unwrap();
    worker.post_user_message(chat, wid, "from the worker").unwrap();
    round(&mut worker_sync, &worker_db).await;
    round(&mut desk_sync, &desk_db).await;
    assert!(bodies(&desk_db, chat).contains(&"from the worker".to_string()));
}

/// A-13: a well-formed body claiming an epoch nobody announced is parked on
/// this device and never pins the fetch cursor; a later body still lands.
#[tokio::test]
async fn junk_epoch_body_does_not_pin_cursor() {
    let relay = spawn_relay().await;
    let room = "ws-a13";
    let wid = hive_core::room_workspace_id(room);
    let dir = tempfile::tempdir().unwrap();
    let (desk_db, other_db) = (dir.path().join("desk.db"), dir.path().join("other.db"));
    let (mut desk, desk_ka, desk_dev) = service(&desk_db, "owner");
    desk.ensure_workspace_config(wid).unwrap();
    let chat = desk.create_chat("Home", wid, "anthropic").unwrap().id;
    let mut desk_sync = engine(&relay, room, desk_dev, &desk_ka, &[]);
    round(&mut desk_sync, &desk_db).await;

    // Anyone with relay access posts a body under an epoch nobody holds.
    let client = crate::RelayClient::new(&relay);
    let junk = json!({ "version": 77, "kid": "deadbeefdeadbeef", "nonce": vec![1u8; 12], "ciphertext": vec![2u8; 48] });
    client.push_value(room, &junk).await.unwrap();
    desk.post_user_message(chat, wid, "after the junk").unwrap();
    round(&mut desk_sync, &desk_db).await;

    // Another device of the room reads past it.
    let (other, other_ka, other_dev) = service(&other_db, "reader-device");
    pin_like_an_invite(&other, &desk, wid);
    let mut other_sync = engine(&relay, room, other_dev, &other_ka, &[]);
    round(&mut other_sync, &other_db).await;
    assert!(bodies(&other_db, chat).contains(&"after the junk".to_string()));
    let store = crate::EventStore::open(&other_db).unwrap();
    let parked = store.parked_bodies(room).unwrap();
    assert_eq!(parked.len(), 1, "the junk body is parked, not retried from the cursor");
    assert!(other_sync.cursor() >= parked[0].0, "the cursor moved past the junk body");
    assert_eq!(store.sync_cursor(room).unwrap(), other_sync.cursor());
}

/// B2 residual: keys are only decided on a pinned fold. A device without a
/// founder pin adopts no rotation (it would be judging the roster from
/// whatever seed sorts first), and can't issue one.
#[test]
fn unpinned_device_never_decides_keys() {
    let mut w = World::new();
    w.rotate("owner");
    let dir = tempfile::tempdir().unwrap();
    let mut store = crate::EventStore::open(dir.path().join("u.db")).unwrap();
    for env in &w.cfg {
        store.ingest(env).unwrap();
    }
    let p = &w.people["ada"];
    let src = KeySource::for_device(p.device_id, p.ka(), Some(PASSPHRASE), &[]);
    let mut engine = crate::SyncEngine::new(crate::RelayClient::new("http://127.0.0.1:9"), "unpinned")
        .with_room_workspace(w.wid)
        .with_key_source(src);
    engine.refresh_keyring(&store).unwrap();
    assert_eq!(engine.keyring().authorized_head(), 0, "no rotation adopted without a pin");
    let owner = &w.people["owner"];
    let fk = crate::envelope_verifier::FounderPin::fingerprint_hex(&owner.account.public_key_bytes());
    store.set_workspace_pin(w.wid, &owner.account_id.to_string(), &fk).unwrap();
    engine.refresh_keyring(&store).unwrap();
    assert_eq!(engine.keyring().authorized_head(), 1);
    let _ = &mut store;
}

/// Track B residual 3: coordinator and workflow lease beats follow the
/// keyring like host beats (B-05). They seal under the authorized head, and a
/// reader takes only beats sealed there, so after a rotation a removed member
/// still holding the passphrase can't see which device holds a lease.
#[test]
fn removed_member_cannot_read_lease_beats_after_rotation() {
    use crate::lease_presence::{lease_presence_slot, open_lease_beats, seal_lease_beat, LeaseBeat};
    use hive_core::lease::LeaseScope;
    let mut w = World::new();
    w.push("owner", SessionEvent::MemberRemoved { member_id: "mara".into(), watermark: Vec::new() });
    w.rotate("owner");
    let owner = w.engine("owner", &[]);
    let mara = KeyringEngine::passphrase(w.wid, pass_key());
    let kp = hive_core::SigningKeypair::generate().unwrap();
    let pk = kp.public_key_bytes().to_vec();
    let beat = LeaseBeat {
        scope: LeaseScope::Workspace,
        scope_id: w.wid.to_string(),
        holder: Uuid::new_v4(),
        epoch: 1,
        beat: 1,
        lease_ms: 30_000,
        at_ms: 1_000,
        signature: Vec::new(),
    }
    .sign(&kp);
    let slot = lease_presence_slot(LeaseScope::Workspace, &w.wid.to_string());
    let mut blobs = std::collections::HashMap::new();
    blobs.insert(slot.clone(), seal_lease_beat(&owner, &beat).unwrap());
    assert!(open_lease_beats(&blobs, &mara, 5_000, |_| Some(pk.clone())).is_empty(), "mara read a beat sealed after her removal");
    assert_eq!(open_lease_beats(&blobs, &w.engine("ada", &[]), 5_000, |_| Some(pk.clone())).len(), 1, "a member reads it");
    // A beat sealed under the passphrase epoch is refused by readers at the head.
    blobs.insert(slot, seal_lease_beat(&mara, &beat).unwrap());
    assert!(open_lease_beats(&blobs, &owner, 5_000, |_| Some(pk.clone())).is_empty(), "a beat under an old epoch");
}

/// Track B residual 4 (B-11): parked bodies are capped per room, and a flood
/// of junk (an epoch no authorized rotation names) must not evict a real body
/// sealed under an authorized rotation this device can't open yet: that one
/// opens as soon as the rotation reaches it.
#[test]
fn parked_flood_does_not_evict_pending_rotation_bodies() {
    let mut w = World::new();
    w.rotate("owner");
    let owner = w.engine("owner", &[]);
    let head = owner.seal_key().unwrap().clone();
    assert_eq!(head.version, 1);
    let mut store = crate::EventStore::open_in_memory().unwrap();
    for env in &w.cfg {
        store.ingest(env).unwrap();
    }
    let founder = &w.people["owner"];
    let fk = crate::envelope_verifier::FounderPin::fingerprint_hex(&founder.account.public_key_bytes());
    store.set_workspace_pin(w.wid, &founder.account_id.to_string(), &fk).unwrap();
    // eve isn't a recipient of the rotation: she knows it's authorized but
    // can't open it yet.
    let p = &w.people["eve"];
    let src = KeySource::for_device(p.device_id, p.ka(), Some(PASSPHRASE), &[]);
    let room = "ws-b11";
    let mut engine = crate::SyncEngine::new(crate::RelayClient::new("http://127.0.0.1:9"), room)
        .with_room_workspace(w.wid)
        .with_key_source(src);
    engine.refresh_keyring(&store).unwrap();
    assert_eq!(engine.keyring().authorized_head(), 1);
    assert!(engine.keyring().seal_key().is_none(), "eve can't open the rotation");

    let real = seal_symmetric_kid(&head.key, head.version, &head.kid, b"{\"pending\":true}").unwrap();
    let mut fetched = vec![(1u64, serde_json::to_value(&real).unwrap())];
    for seq in 2..(crate::event_store::PARKED_KEEP as u64 + 50) {
        let junk = json!({ "version": 77, "kid": "deadbeefdeadbeef", "nonce": vec![1u8; 12], "ciphertext": vec![2u8; 48] });
        fetched.push((seq, junk));
    }
    engine.apply_fetched(&mut store, &fetched).unwrap();
    let parked = store.parked_bodies(room).unwrap();
    assert!(parked.len() as i64 <= crate::event_store::PARKED_KEEP, "the cap holds");
    assert!(parked.iter().any(|(seq, _)| *seq == 1), "the flood evicted the body waiting for the authorized rotation");
}

/// World's config log in a store, pinned to its founder (what a member's
/// device holds).
fn pinned_store(w: &World) -> crate::EventStore {
    let mut store = crate::EventStore::open_in_memory().unwrap();
    for env in &w.cfg {
        store.ingest(env).unwrap();
    }
    let founder = &w.people["owner"];
    let fk = crate::envelope_verifier::FounderPin::fingerprint_hex(&founder.account.public_key_bytes());
    store.set_workspace_pin(w.wid, &founder.account_id.to_string(), &fk).unwrap();
    store
}

fn recipient(id: &str, ka: [u8; 32]) -> crate::directory_trust::Recipient {
    crate::directory_trust::Recipient { device_id: id.into(), ka_public: ka }
}

/// A co-member's key counts as verified only when a device certified for
/// that very account published it, signed, on a config log this device holds
/// with a founder pin. A key the relay substituted, a key another account
/// published, and a revoked device's key are not.
#[test]
fn co_member_keys_verified_only_from_a_certified_devices_publication() {
    use crate::directory_trust::signed_ka_keys;
    let mut w = World::new();
    let ada = w.people["ada"].account_id;
    let ada_ka = w.people["ada"].ka().public_key_bytes();
    let mara_ka = w.people["mara"].ka().public_key_bytes();
    let signed = signed_ka_keys(&pinned_store(&w), ada);
    assert!(signed.contains(&ada_ka), "ada's own publication");
    assert!(!signed.contains(&mara_ka), "another account's key");
    assert!(!signed.contains(&KeyAgreementKeypair::generate().unwrap().public_key_bytes()), "a key nobody published");
    // An unpinned store decides nothing.
    let mut unpinned = crate::EventStore::open_in_memory().unwrap();
    for env in &w.cfg {
        unpinned.ingest(env).unwrap();
    }
    assert!(signed_ka_keys(&unpinned, ada).is_empty(), "keys from an unpinned fold");
    // Revoked: no longer vouched for.
    let device = w.people["ada"].device_id;
    w.push("owner", SessionEvent::DeviceRevoked { device_id: device, watermark: Vec::new() });
    assert!(!signed_ka_keys(&pinned_store(&w), ada).contains(&ada_ka), "a revoked device's key");
}

/// S6 (`relay_substituted_ka_key_not_sealed_to`): an invite or a DM is sealed
/// only to verified devices when the invitee has any; a key only the relay
/// (or any other unchecked source) offers is left out, and with none
/// verified, sealing needs the inviter's explicit confirmation.
#[test]
fn relay_substituted_ka_key_not_sealed_to() {
    use crate::directory_trust::{plan, UNVERIFIED_RECIPIENT};
    let w = World::new();
    let ada = w.people["ada"].account_id;
    let ada_ka = w.people["ada"].ka().public_key_bytes();
    let relay_ka = KeyAgreementKeypair::generate().unwrap().public_key_bytes();
    let signed = crate::directory_trust::signed_ka_keys(&pinned_store(&w), ada);
    let listed = [recipient("relay-added", relay_ka), recipient("ada-desk", ada_ka)];
    let p = plan(&listed, &signed, false).unwrap();
    assert!(p.verified);
    assert_eq!(p.recipients.iter().map(|r| r.device_id.as_str()).collect::<Vec<_>>(), vec!["ada-desk"], "the relay's extra device is left out");
    let stranger = [recipient("relay-added", relay_ka)];
    let err = plan(&stranger, &signed, false).unwrap_err();
    assert!(err.starts_with(UNVERIFIED_RECIPIENT), "{err}");
    let p = plan(&stranger, &signed, true).unwrap();
    assert!(!p.verified);
    assert_eq!(p.recipients.len(), 1, "sealed after the inviter confirmed");
    assert!(plan(&[], &signed, false).unwrap().recipients.is_empty(), "no devices: nothing to seal, nothing to confirm");
    // A co-member's own seal plan holds only keys from the config log.
    let me = w.people["owner"].account_id;
    let ada_entry = crate::directory_trust::co_members(&pinned_store(&w), me).into_iter().find(|c| c.account_id == ada).unwrap();
    let p = ada_entry.seal_plan().unwrap();
    assert!(p.verified);
    assert_eq!(p.recipients.iter().map(|r| r.ka_public).collect::<Vec<_>>(), vec![ada_ka]);
}

/// Co-members come from the pinned config logs, keyed by their random
/// account id, each with the safety code of the account key the log
/// registers. Non-members (eve published keys but was never added) and this
/// device's own account are left out.
#[test]
fn co_members_listed_with_verified_keys_and_safety_codes() {
    let w = World::new();
    let me = w.people["owner"].account_id;
    let list = crate::directory_trust::co_members(&pinned_store(&w), me);
    let names: Vec<&str> = list.iter().map(|c| c.display_name.as_str()).collect();
    assert_eq!(list.len(), 2, "{names:?}");
    for name in ["ada", "mara"] {
        let p = &w.people[name];
        let c = list.iter().find(|c| c.account_id == p.account_id).unwrap();
        let fk = crate::envelope_verifier::FounderPin::fingerprint_hex(&p.account.public_key_bytes());
        assert_eq!(c.fingerprint.as_deref(), Some(fk.as_str()));
        assert_eq!(c.safety_code().unwrap().replace(' ', ""), fk);
        assert!(!c.key_conflict);
        assert_eq!(c.workspaces, vec![w.wid]);
        assert_eq!(c.devices.len(), 1);
    }
}

/// Impersonation via a name: a second member who calls themself "ada" is a
/// separate co-member with their own account id and safety code, and a DM
/// invite that names ada's account but carries the impostor's key
/// fingerprint is refused.
#[test]
fn impersonation_by_display_name_is_a_different_co_member() {
    let mut w = World::new();
    w.people.insert("impostor", Person::new());
    w.publish_identity("impostor");
    let mut m = member("impostor", &w.people["impostor"], WorkspaceRole::Contributor);
    m.actor.display_name = "ada".into();
    w.push("owner", SessionEvent::MemberAdded { member: m, invite: None, proof: None });
    let me = w.people["owner"].account_id;
    let store = pinned_store(&w);
    let list = crate::directory_trust::co_members(&store, me);
    let adas: Vec<_> = list.iter().filter(|c| c.display_name == "ada").collect();
    assert_eq!(adas.len(), 2, "same name, two accounts");
    assert_ne!(adas[0].account_id, adas[1].account_id);
    assert_ne!(adas[0].fingerprint, adas[1].fingerprint);
    // An invite claiming to be from ada's account with the impostor's key.
    let ada = &w.people["ada"];
    let impostor_fk = crate::envelope_verifier::FounderPin::fingerprint_hex(&w.people["impostor"].account.public_key_bytes());
    let known = crate::dm::known_fingerprints(&list);
    assert_eq!(
        crate::dm::check_founder(Some(&ada.account_id.to_string()), Some(&impostor_fk), &known, Some(ada.account_id)),
        crate::dm::FounderTrust::Mismatch
    );
    let ada_fk = crate::envelope_verifier::FounderPin::fingerprint_hex(&ada.account.public_key_bytes());
    assert_eq!(crate::dm::check_founder(Some(&ada.account_id.to_string()), Some(&ada_fk), &known, Some(ada.account_id)), crate::dm::FounderTrust::KnownMatch);
}

/// S3: a member named with a right-to-left override and Cyrillic letters
/// to look like "ada" is listed with the controls stripped, and both accounts
/// whose names look alike are flagged; mara is not.
#[test]
fn look_alike_co_members_flagged_and_names_cleaned() {
    let mut w = World::new();
    w.people.insert("impostor", Person::new());
    w.publish_identity("impostor");
    let mut m = member("impostor", &w.people["impostor"], WorkspaceRole::Contributor);
    m.actor.display_name = "\u{202D}\u{0430}d\u{0430}\u{200B}".into();
    w.push("owner", SessionEvent::MemberAdded { member: m, invite: None, proof: None });
    let me = w.people["owner"].account_id;
    let list = crate::directory_trust::co_members(&pinned_store(&w), me);
    let imp = list.iter().find(|c| c.account_id == w.people["impostor"].account_id).unwrap();
    assert_eq!(imp.display_name, "\u{0430}d\u{0430}", "controls stripped, letters kept");
    assert!(imp.look_alike);
    assert!(list.iter().find(|c| c.account_id == w.people["ada"].account_id).unwrap().look_alike);
    assert!(!list.iter().find(|c| c.account_id == w.people["mara"].account_id).unwrap().look_alike);
}

/// A co-member forges a DM "from Bob": the invite names Bob as founder with
/// Bob's (public) key fingerprint, but Mallory chose the room and key and
/// founded the config log herself. Alice pins Bob as the invite says. Until a
/// seed signed by a device certified under Bob's pinned key is on the log,
/// Alice must not write there: anything she sends is readable by whoever
/// made the room, not by Bob.
#[tokio::test]
async fn forged_dm_founder_never_gets_writes() {
    let relay = spawn_relay().await;
    let room = "dm-forged-founder";
    let wid = hive_core::room_workspace_id(room);
    let dir = tempfile::tempdir().unwrap();
    let (mal_db, alice_db, bob_db) = (dir.path().join("mal.db"), dir.path().join("alice.db"), dir.path().join("bob.db"));

    // Bob exists (Alice knows his account id and key fingerprint) but never
    // touches this room.
    let (bob, _, _) = service(&bob_db, "bob");

    // Mallory founds the room and admits whoever redeems her invite.
    let (mut mal, mal_ka, mal_dev) = service(&mal_db, "mallory");
    mal.ensure_workspace_config(wid).unwrap();
    let chat = mal.create_chat("DM", wid, "anthropic").unwrap().id;
    mal.publish_key_agreement(wid, mal_ka.public_key_bytes()).unwrap();
    let invite = mal.issue_invite(wid).unwrap();
    let mut mal_sync = engine(&relay, room, mal_dev, &mal_ka, &[]);
    round(&mut mal_sync, &mal_db).await;

    // Alice joins from the forged invite: pinned to Bob, as the invite says.
    let (mut alice, alice_ka, alice_dev) = service(&alice_db, "alice");
    alice.store().set_join_invite(wid, invite).unwrap();
    pin_like_an_invite(&alice, &bob, wid);
    alice.set_trusted_founder(Some(bob.author().account_id.unwrap().to_string()));
    let mut alice_sync = engine(&relay, room, alice_dev, &alice_ka, &[]);
    round(&mut alice_sync, &alice_db).await;
    let _ = alice.ensure_self_member(workspace_config_session_id(wid), wid);
    round(&mut alice_sync, &alice_db).await;
    round(&mut mal_sync, &mal_db).await;
    let _ = alice.ensure_self_member(chat, wid);

    assert!(!alice.founder_verified(wid).unwrap(), "no seed from Bob's pinned key is on the log");
    assert_eq!(alice.waiting_for_founder(wid).unwrap(), bob.author().account_id, "shown as waiting for Bob");
    assert_eq!(alice.store().config_founded_at(wid).unwrap(), None, "a forged DM has no founding time for D12");
    let sent = alice.post_user_message(chat, wid, "for Bob only");
    assert!(sent.is_err(), "Alice wrote into a room whose pinned founder never founded it");
    round(&mut alice_sync, &alice_db).await;
    round(&mut mal_sync, &mal_db).await;
    assert!(!bodies(&mal_db, chat).contains(&"for Bob only".to_string()), "Mallory read Alice's DM to Bob");
    // Nor does the forged log vouch for anyone: names, keys and drop boxes
    // come only from logs whose pinned founder really founded them (else
    // Mallory registers a key under any co-member's account there).
    let me = alice.author().account_id.unwrap();
    let listed = crate::directory_trust::co_members(alice.store(), me);
    assert!(listed.iter().all(|c| !c.workspaces.contains(&wid)), "a forged log fed the co-member directory: {listed:?}");
}

/// S1: Mallory founds a workspace, adds Ada's account id and registers her
/// own key (and device) under it there. Alice shares an honest workspace with
/// Ada, pinned earlier. Mallory's log alone must neither mark Ada conflicted
/// (a denial of service on every invite and DM to her) nor give Alice
/// Mallory's key or device as Ada's.
#[test]
fn co_member_key_conflict_from_attacker_founded_log() {
    let honest = World::new();
    let ada = honest.people["ada"].account_id;
    let ada_fk = crate::envelope_verifier::FounderPin::fingerprint_hex(&honest.people["ada"].account.public_key_bytes());
    let ada_ka = honest.people["ada"].ka().public_key_bytes();
    let attacker = World::new_with(|p| p.get_mut("ada").unwrap().account_id = ada);
    let fake_ka = attacker.people["ada"].ka().public_key_bytes();
    let me = honest.people["mara"].account_id;

    let pin = |store: &crate::EventStore, w: &World| {
        let f = &w.people["owner"];
        let fk = crate::envelope_verifier::FounderPin::fingerprint_hex(&f.account.public_key_bytes());
        store.set_workspace_pin(w.wid, &f.account_id.to_string(), &fk).unwrap();
    };
    let mut store = crate::EventStore::open_in_memory().unwrap();
    for env in honest.cfg.iter().chain(attacker.cfg.iter()) {
        store.ingest(env).unwrap();
    }
    pin(&store, &honest);
    pin(&store, &attacker);
    let listed = crate::directory_trust::co_members(&store, me);
    let entry = listed.iter().find(|c| c.account_id == ada).unwrap();
    assert!(!entry.key_conflict, "one attacker-founded log marked Ada conflicted");
    assert_eq!(entry.fingerprint.as_deref(), Some(ada_fk.as_str()), "the earliest pinned registration decides");
    let kas: Vec<[u8; 32]> = entry.devices.iter().map(|r| r.ka_public).collect();
    assert!(kas.contains(&ada_ka));
    assert!(!kas.contains(&fake_ka), "the attacker's device was offered as Ada's");
    assert!(entry.seal_plan().is_ok());
    assert!(!crate::directory_trust::signed_ka_keys(&store, ada).contains(&fake_ka), "the attacker's key verified for Ada");
    assert_eq!(entry.key_disputed_in, vec![attacker.wid], "the attacker's log is flagged, not trusted");
    assert_eq!(entry.workspaces, vec![honest.wid]);
    let _ = &mut store;

    // Joined Mallory's log first, but Ada founded a workspace Alice joined
    // later: the founder pin (from Ada's signed invite) outranks the earlier
    // log's registration.
    let ada_p = &honest.people["ada"];
    let ada_founded = World::new_with(|p| {
        let o = p.get_mut("owner").unwrap();
        o.account_id = ada;
        o.account = SigningKeypair::from_seed(&ada_p.account.seed_bytes()).unwrap();
    });
    let mut store = crate::EventStore::open_in_memory().unwrap();
    for env in attacker.cfg.iter().chain(ada_founded.cfg.iter()) {
        store.ingest(env).unwrap();
    }
    pin(&store, &attacker);
    pin(&store, &ada_founded);
    let listed = crate::directory_trust::co_members(&store, me);
    let entry = listed.iter().find(|c| c.account_id == ada).unwrap();
    assert!(!entry.key_conflict);
    assert_eq!(entry.fingerprint.as_deref(), Some(ada_fk.as_str()), "the founder pin decides");
    assert!(!entry.devices.iter().any(|r| r.ka_public == fake_ka));
    let _ = &mut store;
}

/// S2: after this device leaves a workspace its log no longer vouches for
/// anyone: its founder pin is gone, so co-members, verified keys and contact
/// ids stop coming from it (a rejoin pins again from the new invite).
#[test]
fn left_workspace_stops_contributing_co_members() {
    let w = World::new();
    let me = w.people["owner"].account_id;
    let ada = w.people["ada"].account_id;
    let store = pinned_store(&w);
    assert!(crate::directory_trust::co_members(&store, me).iter().any(|c| c.account_id == ada));
    store.forget_workspace(w.wid).unwrap();
    assert!(store.workspace_pin(w.wid).unwrap().is_none(), "the founder pin outlived leaving");
    assert!(crate::directory_trust::co_members(&store, me).is_empty(), "a left workspace still feeds co-members");
    assert!(crate::directory_trust::signed_ka_keys(&store, ada).is_empty(), "a left workspace still verifies keys");
}

/// S2: a contact id rides on whatever an account's devices stamp. Mara signs
/// an event with her own (certified) device but stamps it as Ada with
/// Mara's drop box: invites meant for Ada must not be routed there. Nor may a
/// revoked device of Ada's move it.
#[test]
fn forged_stamp_cannot_set_contact() {
    let mut w = World::new();
    let ada = w.people["ada"].account_id;
    let me = w.people["owner"].account_id;
    let stamp_contact = |w: &mut World, by: &str, as_account: Uuid, cid: &str| {
        let mut env = w.draft(by, SessionEvent::ChannelCreated { channel: hive_core::Channel::new("x", "x") });
        let stamp = env.actor_stamp.as_mut().unwrap();
        stamp.actor.account_id = Some(as_account);
        stamp.actor.id = "ada".into();
        stamp.actor.contact = Some(cid.into());
        let p = &w.people[by];
        sign_envelope(&mut env, p.device_id, &p.device);
        w.cfg.push(env);
    };
    let real = crate::contacts::new_cid();
    stamp_contact(&mut w, "ada", ada, &real);
    let contact = |w: &World| {
        crate::directory_trust::co_members(&pinned_store(w), me).into_iter().find(|c| c.account_id == ada).unwrap().contact.map(|c| c.0)
    };
    assert_eq!(contact(&w).as_deref(), Some(real.as_str()));
    let forged = crate::contacts::new_cid();
    stamp_contact(&mut w, "mara", ada, &forged);
    assert_eq!(contact(&w).as_deref(), Some(real.as_str()), "Mara's stamp moved Ada's drop box");
    // An unsigned copy (signature stripped) doesn't count either.
    let mut env = w.draft("ada", SessionEvent::ChannelCreated { channel: hive_core::Channel::new("y", "y") });
    env.actor_stamp.as_mut().unwrap().actor.contact = Some(crate::contacts::new_cid());
    env.signature = None;
    w.cfg.push(env);
    assert_eq!(contact(&w).as_deref(), Some(real.as_str()), "an unsigned stamp moved Ada's drop box");
}

/// The control: the real founder's seed verifies under the pinned key, and the
/// joiner writes as usual.
#[tokio::test]
async fn genuine_founder_verifies_and_joiner_writes() {
    let relay = spawn_relay().await;
    let room = "dm-genuine-founder";
    let wid = hive_core::room_workspace_id(room);
    let dir = tempfile::tempdir().unwrap();
    let (bob_db, alice_db) = (dir.path().join("bob.db"), dir.path().join("alice.db"));
    let (mut bob, bob_ka, bob_dev) = service(&bob_db, "bob");
    bob.ensure_workspace_config(wid).unwrap();
    let chat = bob.create_chat("DM", wid, "anthropic").unwrap().id;
    bob.publish_key_agreement(wid, bob_ka.public_key_bytes()).unwrap();
    let invite = bob.issue_invite(wid).unwrap();
    let mut bob_sync = engine(&relay, room, bob_dev, &bob_ka, &[]);
    round(&mut bob_sync, &bob_db).await;
    assert!(bob.founder_verified(wid).unwrap(), "the founder's own device");

    let (mut alice, alice_ka, alice_dev) = service(&alice_db, "alice");
    alice.store().set_join_invite(wid, invite).unwrap();
    pin_like_an_invite(&alice, &bob, wid);
    alice.set_trusted_founder(Some(bob.author().account_id.unwrap().to_string()));
    let mut alice_sync = engine(&relay, room, alice_dev, &alice_ka, &[]);
    assert!(!alice.founder_verified(wid).unwrap(), "nothing synced yet");
    assert!(alice.waiting_for_founder(wid).unwrap().is_some());
    round(&mut alice_sync, &alice_db).await;
    assert!(alice.founder_verified(wid).unwrap());
    assert_eq!(alice.waiting_for_founder(wid).unwrap(), None, "the waiting state clears once the founding syncs");
    assert_eq!(bob.waiting_for_founder(wid).unwrap(), None, "the founder never waits for itself");
    // D12's founding time is the founder's signed seed, which no later event
    // moves: a config event dated before it (a skewed clock, or a party
    // backdating to flip which DM is kept) changes nothing.
    let founded = alice.store().config_founded_at(wid).unwrap().expect("founded");
    let mut early = SessionEventEnvelope::new(
        workspace_config_session_id(wid),
        wid,
        999,
        SessionEvent::DeviceKeyAgreementPublished { device_id: alice_dev, ka_public: alice_ka.public_key_bytes().to_vec() },
    );
    early.timestamp = Timestamp(time::OffsetDateTime::from_unix_timestamp(946_684_800).unwrap());
    alice.store().append_envelope(&early).unwrap();
    assert_eq!(alice.store().config_founded_at(wid).unwrap(), Some(founded));
    alice.ensure_self_member(chat, wid).unwrap();
    alice.post_user_message(chat, wid, "hi Bob").unwrap();
    round(&mut alice_sync, &alice_db).await;
    round(&mut bob_sync, &bob_db).await;
    assert!(bodies(&bob_db, chat).contains(&"hi Bob".to_string()));
}
