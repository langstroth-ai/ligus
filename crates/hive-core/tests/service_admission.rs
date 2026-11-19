//! Phase B: box admissions and the service-member rules.
//!
//! A `ligus serve` or `ligus worker` box joins as its own service member: an
//! Admin (desktop or phone, with passkey presence) issues a single-use,
//! expiring box admission; the box redeems it by adding itself, signed by
//! its own device, which binds the invite to that key. A service member is
//! capped at Contributor, never cuts or admits anyone, never carries
//! presence, never satisfies a human gate, and its device is revoked like
//! any other.

use hive_core::admission::{Admission, RedeemKey, MAX_ADMISSION_TTL_MS};
use hive_core::authorization::invite_subject;
use hive_core::testkit::{Phone, SignedWorld};
use hive_core::{
    ActionProposal, ActorKind, ChatMessage, MemberRoleChange, MessageRole, ProposalApproval, SessionEvent, Timestamp, Verdict,
    WorkspaceRole,
};
use uuid::Uuid;

const HOUR_MS: i64 = 60 * 60 * 1000;

/// The redeem key of every admission these tests issue (its seed is what an
/// admission code carries).
fn key() -> RedeemKey {
    RedeemKey::from_seed(&[7; 32]).unwrap()
}

/// The honest watermark of `who`'s device on every log of `w`.
fn honest_watermark(w: &SignedWorld, who: &str) -> Vec<hive_core::watermark::Watermark> {
    let signers: std::collections::BTreeSet<Uuid> = [w.people[who].device_id].into_iter().collect();
    let mut out = hive_core::watermark::heads_of(&w.cfg, hive_core::workspace_config_session_id(w.wid), &signers);
    for (id, log) in &w.chats {
        out.extend(hive_core::watermark::heads_of(log, *id, &signers));
    }
    out
}

fn now_ms() -> i64 {
    (Timestamp::now().inner().unix_timestamp_nanos() / 1_000_000) as i64
}

fn at_ms(ms: i64) -> Timestamp {
    Timestamp::from_unix_seconds(ms.div_euclid(1000))
}

/// owner (Owner), adm (Admin), mara (Contributor); `box` and `box2` are
/// service boxes that exist but aren't members; `eve` is a person outside.
fn world() -> SignedWorld {
    let mut w = SignedWorld::genesis(&["owner", "adm", "mara", "box", "box2", "eve"], None);
    w.admit("owner", "adm", WorkspaceRole::Admin);
    w.admit("owner", "mara", WorkspaceRole::Contributor);
    w.kinds.insert("box".into(), ActorKind::Service);
    w.kinds.insert("box2".into(), ActorKind::Service);
    w
}

/// `by` issues a box admission for invite `id` (with presence).
fn issue(w: &mut SignedWorld, by: &str, id: Uuid, admission: Admission) -> Uuid {
    w.push(by, SessionEvent::JoinApproved { subject: invite_subject(id), admission: Some(admission), redeem_key: None })
}

/// `name` publishes its identity and redeems invite `id` as itself, at
/// `role`, signed at `when` (unix ms; `None` = now).
fn redeem(w: &mut SignedWorld, name: &str, id: Uuid, role: WorkspaceRole, when: Option<i64>) -> Uuid {
    w.publish_identity(name);
    let m = w.member_of(name, role);
    let proof = key().prove(w.wid, id, name, w.people[name].device_id);
    let mut env = w.draft(name, None, SessionEvent::MemberAdded { member: m, invite: Some(id), proof: Some(proof) }, None);
    if let Some(ms) = when {
        env.timestamp = at_ms(ms);
    }
    w.finish(&mut env, name, None);
    w.push_env(None, env)
}

fn admitted(w: &mut SignedWorld, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    issue(w, "adm", id, Admission::service(now_ms() + HOUR_MS, None, key().public()));
    let r = redeem(w, name, id, WorkspaceRole::Contributor, None);
    assert_eq!(w.verdict(r), Verdict::Allowed);
    id
}

#[test]
fn a_box_redeems_its_admission_once_as_a_contributor_service_member() {
    let mut w = world();
    let id = Uuid::new_v4();
    let i = issue(&mut w, "adm", id, Admission::service(now_ms() + HOUR_MS, None, key().public()));
    assert_eq!(w.verdict(i), Verdict::Allowed);
    let r = redeem(&mut w, "box", id, WorkspaceRole::Contributor, None);
    assert_eq!(w.verdict(r), Verdict::Allowed);
    let m = w.roster().members["box"].clone();
    assert_eq!((m.kind, m.role), (ActorKind::Service, WorkspaceRole::Contributor));
    // Bound to the box on redemption: nobody else redeems it.
    let again = redeem(&mut w, "box2", id, WorkspaceRole::Contributor, None);
    assert_eq!(w.verdict(again), Verdict::Denied, "an admission admits one box");
    assert!(!w.roster().members.contains_key("box2"));
}

#[test]
fn an_admission_admits_no_more_than_contributor() {
    let mut w = world();
    let id = Uuid::new_v4();
    issue(&mut w, "adm", id, Admission::service(now_ms() + HOUR_MS, None, key().public()));
    for role in [WorkspaceRole::Owner, WorkspaceRole::Admin] {
        let r = redeem(&mut w, "box", id, role, None);
        assert_eq!(w.verdict(r), Verdict::Denied, "{role:?}");
    }
    // A Viewer box is fine (below the cap).
    let r = redeem(&mut w, "box", id, WorkspaceRole::Viewer, None);
    assert_eq!(w.verdict(r), Verdict::Allowed);
}

#[test]
fn an_expired_admission_admits_no_one() {
    let mut w = world();
    let id = Uuid::new_v4();
    let expires = now_ms() + HOUR_MS;
    issue(&mut w, "adm", id, Admission::service(expires, None, key().public()));
    let late = redeem(&mut w, "box", id, WorkspaceRole::Contributor, Some(expires + 1000));
    assert_eq!(w.verdict(late), Verdict::Denied);
    let on_time = redeem(&mut w, "box", id, WorkspaceRole::Contributor, Some(expires - 1000));
    assert_eq!(w.verdict(on_time), Verdict::Allowed);
}

#[test]
fn a_person_invite_never_admits_a_box_and_an_admission_never_admits_a_person() {
    let mut w = world();
    // A person's invite.
    let person = Uuid::new_v4();
    w.push("adm", SessionEvent::JoinApproved { subject: invite_subject(person), admission: None, redeem_key: None });
    let r = redeem(&mut w, "box", person, WorkspaceRole::Contributor, None);
    assert_eq!(w.verdict(r), Verdict::Denied, "a box joins only through a box admission");
    // Nor without any invite.
    let m = w.member_of("box", WorkspaceRole::Contributor);
    let bare = w.push_bare("box", SessionEvent::MemberAdded { member: m, invite: None, proof: None });
    assert_eq!(w.verdict(bare), Verdict::Denied);
    // A box admission redeemed by a person.
    let boxed = Uuid::new_v4();
    issue(&mut w, "adm", boxed, Admission::service(now_ms() + HOUR_MS, None, key().public()));
    let r = redeem(&mut w, "eve", boxed, WorkspaceRole::Contributor, None);
    assert_eq!(w.verdict(r), Verdict::Denied, "a box admission admits only a service");
    // The person invite still admits the person.
    let ok = redeem(&mut w, "eve", person, WorkspaceRole::Contributor, None);
    assert_eq!(w.verdict(ok), Verdict::Allowed);
}

#[test]
fn a_bound_admission_admits_only_the_box_that_asked() {
    let mut w = world();
    let id = Uuid::new_v4();
    let asked = w.people["box"].account_id;
    issue(&mut w, "adm", id, Admission::service(now_ms() + HOUR_MS, Some(asked), key().public()));
    let other = redeem(&mut w, "box2", id, WorkspaceRole::Contributor, None);
    assert_eq!(w.verdict(other), Verdict::Denied, "another box's key");
    let mine = redeem(&mut w, "box", id, WorkspaceRole::Contributor, None);
    assert_eq!(w.verdict(mine), Verdict::Allowed);
}

#[test]
fn issuing_an_admission_is_an_admins_with_presence_and_bounded() {
    let mut w = world();
    let soon = now_ms() + HOUR_MS;
    // Contributor: no.
    let id = Uuid::new_v4();
    let by_mara = issue(&mut w, "mara", id, Admission::service(soon, None, key().public()));
    assert_eq!(w.verdict(by_mara), Verdict::Denied);
    // No presence: no.
    let bare = w.push_bare("adm", SessionEvent::JoinApproved { subject: invite_subject(Uuid::new_v4()), admission: Some(Admission::service(soon, None, key().public())), redeem_key: None });
    assert_eq!(w.verdict(bare), Verdict::Denied);
    // Expiry before issuing, or beyond the bound: no.
    let past = issue(&mut w, "adm", Uuid::new_v4(), Admission::service(now_ms() - HOUR_MS, None, key().public()));
    let far = issue(&mut w, "adm", Uuid::new_v4(), Admission::service(now_ms() + MAX_ADMISSION_TTL_MS + HOUR_MS, None, key().public()));
    assert_eq!((w.verdict(past), w.verdict(far)), (Verdict::Denied, Verdict::Denied));
    // Another kind: no.
    let human = issue(&mut w, "adm", Uuid::new_v4(), Admission { kind: ActorKind::Human, ..Admission::service(soon, None, key().public()) });
    assert_eq!(w.verdict(human), Verdict::Denied);
    // Not an invite subject: no.
    let odd = w.push("adm", SessionEvent::JoinApproved { subject: "bob".into(), admission: Some(Admission::service(soon, None, key().public())), redeem_key: None });
    assert_eq!(w.verdict(odd), Verdict::Denied);
    // One admission per invite, and a person invite can't take its id.
    let id = Uuid::new_v4();
    let first = issue(&mut w, "adm", id, Admission::service(soon, None, key().public()));
    assert_eq!(w.verdict(first), Verdict::Allowed);
    let twice = issue(&mut w, "owner", id, Admission::service(soon, Some(Uuid::new_v4()), key().public()));
    let as_person = w.push("owner", SessionEvent::JoinApproved { subject: invite_subject(id), admission: None, redeem_key: None });
    assert_eq!((w.verdict(twice), w.verdict(as_person)), (Verdict::Denied, Verdict::Denied));
    // A person invite's id can't become an admission.
    let p = Uuid::new_v4();
    w.push("adm", SessionEvent::JoinApproved { subject: invite_subject(p), admission: None, redeem_key: None });
    let upgrade = issue(&mut w, "adm", p, Admission::service(soon, None, key().public()));
    assert_eq!(w.verdict(upgrade), Verdict::Denied);
    // On a chat log: no.
    let chat = w.chat("owner", "c");
    let in_chat = w.write(
        "adm",
        Some(chat),
        SessionEvent::JoinApproved { subject: invite_subject(Uuid::new_v4()), admission: Some(Admission::service(soon, None, key().public())), redeem_key: None },
        None,
        hive_core::testkit::Proof::Recovery,
    );
    assert_ne!(w.verdict(in_chat), Verdict::Allowed);
}

#[test]
fn an_admin_admits_a_box_from_the_phone_with_presence() {
    let mut w = world();
    let phone = Phone::new();
    let cert = w.phone_certificate("adm", &phone);
    let enrolled = w.push("adm", SessionEvent::DeviceCertificateAdded { certificate: cert });
    assert_eq!(w.verdict(enrolled), Verdict::Allowed);
    let soon = now_ms() + HOUR_MS;
    let id = Uuid::new_v4();
    // Without presence: refused.
    let bare = w.phone_write("adm", &phone, None, SessionEvent::JoinApproved { subject: invite_subject(id), admission: Some(Admission::service(soon, None, key().public())), redeem_key: None });
    assert_eq!(w.verdict(bare), Verdict::Denied);
    // With the person's presence over the phone-signed event: allowed.
    let mut env = w.draft("adm", None, SessionEvent::JoinApproved { subject: invite_subject(id), admission: Some(Admission::service(soon, None, key().public())), redeem_key: None }, None);
    if let Some(s) = env.actor_stamp.as_mut() {
        s.actor.device_id = Some(phone.device_id);
        s.prev = w.cfg.iter().rev().find(|e| e.signer_device_id == Some(phone.device_id)).map(|e| e.event_id);
    }
    env.clear_raw();
    hive_core::crypto::sign_envelope_p256(&mut env, phone.device_id, &phone.key);
    let a = w.recovery_presence("adm", &env);
    env.actor_stamp.as_mut().unwrap().presence = Some(a);
    env.clear_raw();
    hive_core::crypto::sign_envelope_p256(&mut env, phone.device_id, &phone.key);
    let ok = w.push_env(None, env);
    assert_eq!(w.verdict(ok), Verdict::Allowed);
    let r = redeem(&mut w, "box", id, WorkspaceRole::Contributor, None);
    assert_eq!(w.verdict(r), Verdict::Allowed);
    // The phone still issues no person invite and adds no one.
    let mut env = w.draft("adm", None, SessionEvent::JoinApproved { subject: invite_subject(Uuid::new_v4()), admission: None, redeem_key: None }, None);
    env.actor_stamp.as_mut().unwrap().actor.device_id = Some(phone.device_id);
    let a = w.recovery_presence("adm", &env);
    env.actor_stamp.as_mut().unwrap().presence = Some(a);
    env.clear_raw();
    hive_core::crypto::sign_envelope_p256(&mut env, phone.device_id, &phone.key);
    let person = w.push_env(None, env);
    assert_eq!(w.verdict(person), Verdict::Denied);
}

#[test]
fn a_service_member_cuts_no_one_and_admits_no_one() {
    let mut w = world();
    w.admit("owner", "eve", WorkspaceRole::Viewer);
    admitted(&mut w, "box");
    let eve_device = w.people["eve"].device_id;
    let cuts = [
        SessionEvent::MemberRemoved { member_id: "eve".into(), watermark: vec![] },
        SessionEvent::MemberRoleChanged {
            change: MemberRoleChange { member_id: "eve".into(), old_role: WorkspaceRole::Viewer, new_role: WorkspaceRole::Viewer },
            watermark: vec![],
        },
        SessionEvent::DeviceRevoked { device_id: eve_device, watermark: vec![] },
        SessionEvent::MemberAdded { member: w.member_of("box2", WorkspaceRole::Viewer), invite: None, proof: None },
        SessionEvent::JoinApproved { subject: invite_subject(Uuid::new_v4()), admission: None, redeem_key: None },
        SessionEvent::JoinApproved { subject: invite_subject(Uuid::new_v4()), admission: Some(Admission::service(now_ms() + HOUR_MS, None, key().public())), redeem_key: None },
    ];
    for e in cuts {
        let kind = e.kind_str();
        let id = w.push_bare("box", e);
        assert_eq!(w.verdict(id), Verdict::Denied, "{kind}");
    }
    assert!(w.roster().members.contains_key("eve"));
}

#[test]
fn a_service_member_carries_no_presence_and_satisfies_no_human_gate() {
    let mut w = world();
    admitted(&mut w, "box");
    // Presence the box stamps (its own key, never registered) is refused.
    let chat = w.chat("owner", "c");
    let with = w.write(
        "box",
        Some(chat),
        SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "box", "hi") },
        None,
        hive_core::testkit::Proof::Recovery,
    );
    assert_eq!(w.verdict(with), Verdict::Denied);
    // Its vote counts by its role, never as a human's.
    let p = ActionProposal::new("q", hive_core::ProposalKind::Decision, "owner");
    let pid = p.id;
    w.say(chat, "owner", SessionEvent::ProposalUpserted { proposal: p });
    let d = hive_core::authz::votes::diff_digest(&w.state(chat).proposals[0]);
    w.say(chat, "box", SessionEvent::ProposalVoteCast {
        proposal_id: pid,
        approval: ProposalApproval { actor_id: "box".into(), role: WorkspaceRole::Contributor, approved: true, created_at: Timestamp::epoch(), diff_digest: Some(d) },
    });
    assert!(!hive_core::guard::has_human_approval(&w.state(chat), pid));
}

#[test]
fn a_service_members_device_is_revoked_like_any_device() {
    let mut w = world();
    admitted(&mut w, "box");
    let dev = w.people["box"].device_id;
    w.push_bare("box", SessionEvent::DeviceKeyAgreementPublished { device_id: dev, ka_public: vec![7; 32] });
    assert!(w.roster().rotation_recipients().contains_key(&dev), "an admitted box gets later workspace keys");
    let chat = w.chat("owner", "c");
    let before = w.say(chat, "box", SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "box", "hello") });
    assert_eq!(w.verdict(before), Verdict::Allowed);
    // An Admin revokes it (with presence).
    let watermark = honest_watermark(&w, "box");
    let rev = w.push("adm", SessionEvent::DeviceRevoked { device_id: dev, watermark });
    assert_eq!(w.verdict(rev), Verdict::Allowed);
    assert_eq!(w.verdict(before), Verdict::Allowed, "what it wrote before the revocation stands");
    assert!(w.roster().members.contains_key("box"));
    assert!(w.roster().devices[&dev].revoked);
    assert!(!w.roster().rotation_recipients().contains_key(&dev), "a revoked box gets no later key");
    let after = w.say(chat, "box", SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "box", "still here") });
    assert_ne!(w.verdict(after), Verdict::Allowed);
    // And removed like any member below Admin.
    let watermark = honest_watermark(&w, "box");
    let gone = w.push("adm", SessionEvent::MemberRemoved { member_id: "box".into(), watermark });
    assert_eq!(w.verdict(gone), Verdict::Allowed);
}

#[test]
fn admission_terms_round_trip_and_stay_off_the_wire_when_absent() {
    let plain = SessionEvent::JoinApproved { subject: "invite:x".into(), admission: None, redeem_key: None };
    assert_eq!(serde_json::to_string(&plain).unwrap(), r#"{"kind":"joinApproved","subject":"invite:x"}"#);
    let mut terms = Admission::service(5, None, key().public());
    terms.redeem_key = None;
    let a = SessionEvent::JoinApproved { subject: "invite:x".into(), admission: Some(terms), redeem_key: None };
    let j = serde_json::to_string(&a).unwrap();
    assert_eq!(j, r#"{"kind":"joinApproved","subject":"invite:x","admission":{"kind":"service","expiresMs":5}}"#);
    assert_eq!(serde_json::from_str::<SessionEvent>(&j).unwrap(), a);
    let keyed = SessionEvent::JoinApproved { subject: "invite:x".into(), admission: Some(Admission::service(5, None, key().public())), redeem_key: None };
    let j = serde_json::to_string(&keyed).unwrap();
    assert!(j.contains(r#""redeemKey":["#), "{j}");
    assert_eq!(serde_json::from_str::<SessionEvent>(&j).unwrap(), keyed);
    // A plain add carries neither an invite nor a proof on the wire.
    let m = SessionEvent::MemberAdded { member: world().member_of("mara", WorkspaceRole::Viewer), invite: None, proof: None };
    let j = serde_json::to_string(&m).unwrap();
    assert!(!j.contains("proof") && !j.contains("invite"), "{j}");
    let closed = SessionEvent::AdmissionClosed { invite: Uuid::nil(), redeemed: None };
    assert_eq!(serde_json::to_string(&closed).unwrap(), r#"{"kind":"admissionClosed","invite":"00000000-0000-0000-0000-000000000000"}"#);
}

#[test]
fn a_member_cannot_spend_a_box_admission_by_re_adding_itself() {
    let mut w = world();
    let id = Uuid::new_v4();
    issue(&mut w, "adm", id, Admission::service(now_ms() + HOUR_MS, None, key().public()));
    let m = w.member_of("mara", WorkspaceRole::Contributor);
    let spend = w.push_bare("mara", SessionEvent::MemberAdded { member: m, invite: Some(id), proof: None });
    assert_eq!(w.verdict(spend), Verdict::Denied);
    let r = redeem(&mut w, "box", id, WorkspaceRole::Contributor, None);
    assert_eq!(w.verdict(r), Verdict::Allowed, "the admission is still the box's");
}

#[test]
fn a_service_member_cannot_re_add_itself_as_a_person() {
    // Review R1: an idempotent self re-add replaced the whole member, kind
    // included, so a box could turn itself into a person and leave every
    // service rule behind (presence, human approvals, cuts at its role).
    let mut w = world();
    admitted(&mut w, "box");
    let mut m = w.member_of("box", WorkspaceRole::Contributor);
    m.actor.kind = ActorKind::Human;
    let flip = w.push_bare("box", SessionEvent::MemberAdded { member: m, invite: None, proof: None });
    assert_eq!(w.verdict(flip), Verdict::Denied, "a box never becomes a person");
    assert_eq!(w.roster().members["box"].kind, ActorKind::Service);
    // Having tried, its later vote signed as a person still counts for nothing.
    w.kinds.insert("box".into(), ActorKind::Human);
    let chat = w.chat("owner", "c");
    let p = ActionProposal::new("q", hive_core::ProposalKind::Decision, "owner");
    let pid = p.id;
    w.say(chat, "owner", SessionEvent::ProposalUpserted { proposal: p });
    let d = hive_core::authz::votes::diff_digest(&w.state(chat).proposals[0]);
    w.say(chat, "box", SessionEvent::ProposalVoteCast {
        proposal_id: pid,
        approval: ProposalApproval { actor_id: "box".into(), role: WorkspaceRole::Contributor, approved: true, created_at: Timestamp::epoch(), diff_digest: Some(d) },
    });
    assert!(!hive_core::guard::has_human_approval(&w.state(chat), pid));
    // A person re-adding themself (a new name or avatar) is still fine.
    let mut mara = w.member_of("mara", WorkspaceRole::Contributor);
    mara.actor.display_name = "Mara B".into();
    let ok = w.push_bare("mara", SessionEvent::MemberAdded { member: mara, invite: None, proof: None });
    assert_eq!(w.verdict(ok), Verdict::Allowed);
}


/// `name` redeems invite `id` with a proof from `k`, at `lamport` when given
/// (a position the redeemer chooses: right after the issue is the earliest).
fn redeem_with(w: &mut SignedWorld, name: &str, id: Uuid, k: &RedeemKey, lamport: Option<u64>, when: Option<i64>) -> Uuid {
    w.publish_identity(name);
    let m = w.member_of(name, WorkspaceRole::Contributor);
    let proof = k.prove(w.wid, id, name, w.people[name].device_id);
    let mut env = w.draft(name, None, SessionEvent::MemberAdded { member: m, invite: Some(id), proof: Some(proof) }, None);
    if let Some(l) = lamport {
        // Backdated to `l`: naming the newest event at `l - 1` as its head
        // (and a chain link older than that), as the lamport rule allows.
        let head = hive_core::lamport::plausible_keys(&w.cfg).into_iter().filter(|k| k.lamport == l - 1).max();
        hive_core::lamport::stamp_next(&mut env, head);
        let device = w.people[name].device_id;
        let prev = w.cfg.iter().filter(|e| e.signer_device_id == Some(device) && e.lamport < l).max_by_key(|e| e.canonical_key()).map(|e| e.event_id);
        env.actor_stamp.as_mut().unwrap().prev = prev;
    }
    if let Some(ms) = when {
        env.timestamp = at_ms(ms);
    }
    w.finish(&mut env, name, None);
    w.push_env(None, env)
}

fn lamport_of(w: &SignedWorld, id: Uuid) -> u64 {
    w.cfg.iter().find(|e| e.event_id == id).unwrap().lamport
}

#[test]
fn a_member_who_reads_the_invite_off_the_log_cannot_redeem_it() {
    // Review R5/R6: the invite id is on the config log for every member, so
    // a member's sock puppet redeemed an unbound admission before the box,
    // and forged the stamp's account to take a bound one.
    let mut w = world();
    let id = Uuid::new_v4();
    issue(&mut w, "adm", id, Admission::service(now_ms() + HOUR_MS, None, key().public()));
    let made_up = RedeemKey::from_seed(&[9; 32]).unwrap();
    let snipe = redeem_with(&mut w, "box2", id, &made_up, None, None);
    assert_eq!(w.verdict(snipe), Verdict::Denied, "a proof from another key");
    let m = w.member_of("box2", WorkspaceRole::Contributor);
    let bare = w.push_bare("box2", SessionEvent::MemberAdded { member: m, invite: Some(id), proof: None });
    assert_eq!(w.verdict(bare), Verdict::Denied, "no proof");
    // The box's proof, replayed by another device, admits no one either.
    let stolen = key().prove(w.wid, id, "box", w.people["box"].device_id);
    let mut m = w.member_of("box2", WorkspaceRole::Contributor);
    m.id = "box".into();
    let mut env = w.draft("box2", None, SessionEvent::MemberAdded { member: m, invite: Some(id), proof: Some(stolen) }, None);
    env.actor_stamp.as_mut().unwrap().actor.id = "box".into();
    w.finish(&mut env, "box2", None);
    let replay = w.push_env(None, env);
    assert_ne!(w.verdict(replay), Verdict::Allowed, "a proof for another device");
    let ok = redeem(&mut w, "box", id, WorkspaceRole::Contributor, None);
    assert_eq!(w.verdict(ok), Verdict::Allowed);

    // A bound admission: a forged account in the stamp doesn't take it.
    let mut w = world();
    let id = Uuid::new_v4();
    let asked = w.people["box"].account_id;
    issue(&mut w, "adm", id, Admission::service(now_ms() + HOUR_MS, Some(asked), key().public()));
    w.publish_identity("box2");
    let mut m = w.member_of("box2", WorkspaceRole::Contributor);
    m.actor.account_id = Some(asked);
    let mut env = w.draft("box2", None, SessionEvent::MemberAdded { member: m, invite: Some(id), proof: None }, None);
    env.actor_stamp.as_mut().unwrap().actor.account_id = Some(asked);
    w.finish(&mut env, "box2", None);
    let forged = w.push_env(None, env);
    assert_eq!(w.verdict(forged), Verdict::Denied);
    let ok = redeem(&mut w, "box", id, WorkspaceRole::Contributor, None);
    assert_eq!(w.verdict(ok), Verdict::Allowed);
}

#[test]
fn a_closed_admission_admits_no_backdated_or_retroactive_redemption() {
    // Review R7: every time on the log is the redeemer's choice, so a holder
    // of an expired code dated its redemption before the expiry and slotted
    // it right after the issue; and a thief of a spent code slotted its own
    // redemption before the box's, taking the box's place after the fact.
    let mut w = world();
    let id = Uuid::new_v4();
    let expires = now_ms() + HOUR_MS;
    let issued = issue(&mut w, "adm", id, Admission::service(expires, None, key().public()));
    let ok = redeem(&mut w, "box", id, WorkspaceRole::Contributor, None);
    assert_eq!(w.verdict(ok), Verdict::Allowed);
    // Before any close: a thief with the code slots in before the box.
    let early = lamport_of(&w, issued) + 1;
    let mut w2 = w.clone();
    let hijack = redeem_with(&mut w2, "box2", id, &key(), Some(early), None);
    assert_eq!(w2.verdict(hijack), Verdict::Allowed, "the residual a close removes: first in canonical order wins");
    assert_eq!(w2.verdict(ok), Verdict::Denied);
    // An Admin's device closes the admission, naming the box's redemption.
    let close = w.push_bare("adm", SessionEvent::AdmissionClosed { invite: id, redeemed: Some(ok) });
    assert_eq!(w.verdict(close), Verdict::Allowed, "no presence: it only narrows");
    let hijack = redeem_with(&mut w, "box2", id, &key(), Some(early), None);
    assert_eq!(w.verdict(hijack), Verdict::Denied);
    assert_eq!(w.verdict(ok), Verdict::Allowed, "the named redemption stands");
    assert!(w.roster().members.contains_key("box") && !w.roster().members.contains_key("box2"));

    // An unredeemed admission closed at its expiry: a code found later,
    // redeemed with a date before the expiry and slotted after the issue,
    // admits no one.
    let mut w = world();
    let id = Uuid::new_v4();
    let issued = issue(&mut w, "adm", id, Admission::service(expires, None, key().public()));
    let close = w.push_bare("adm", SessionEvent::AdmissionClosed { invite: id, redeemed: None });
    assert_eq!(w.verdict(close), Verdict::Allowed);
    let early = lamport_of(&w, issued) + 1;
    let backdated = redeem_with(&mut w, "box", id, &key(), Some(early), Some(expires - 1000));
    assert_eq!(w.verdict(backdated), Verdict::Denied);
    assert!(!w.roster().members.contains_key("box"));
}

#[test]
fn only_an_admin_closes_an_admission_and_only_one_issued_here() {
    let mut w = world();
    admitted(&mut w, "box");
    let id = Uuid::new_v4();
    issue(&mut w, "adm", id, Admission::service(now_ms() + HOUR_MS, None, key().public()));
    for by in ["mara", "box"] {
        let c = w.push_bare(by, SessionEvent::AdmissionClosed { invite: id, redeemed: None });
        assert_eq!(w.verdict(c), Verdict::Denied, "{by}");
    }
    let unknown = w.push_bare("adm", SessionEvent::AdmissionClosed { invite: Uuid::new_v4(), redeemed: None });
    assert_eq!(w.verdict(unknown), Verdict::Denied);
    let chat = w.chat("owner", "c");
    let in_chat = w.write("adm", Some(chat), SessionEvent::AdmissionClosed { invite: id, redeemed: None }, None, hive_core::testkit::Proof::None);
    assert_ne!(w.verdict(in_chat), Verdict::Allowed);
    let r = redeem(&mut w, "box2", id, WorkspaceRole::Contributor, None);
    assert_eq!(w.verdict(r), Verdict::Allowed, "nothing closed it");
}

#[test]
fn a_phone_signs_only_the_admission_its_passkey_confirmed() {
    // Review R8: the box drafts the phone-signed event and hands back opaque
    // preimage bytes; a box that swaps in another event (a vote as the
    // person, another admission) got the phone's P-256 signature on it. The
    // phone checks the preimage first.
    use hive_core::admission::check_phone_admission_preimage;
    let mut w = world();
    let phone = Phone::new();
    let cert = w.phone_certificate("adm", &phone);
    w.push("adm", SessionEvent::DeviceCertificateAdded { certificate: cert });
    let soon = now_ms() + HOUR_MS;
    let draft = |w: &mut SignedWorld, payload: SessionEvent| {
        let mut env = w.draft("adm", None, payload, None);
        let s = env.actor_stamp.as_mut().unwrap();
        s.actor.device_id = Some(phone.device_id);
        s.sig_alg = hive_core::crypto::SigAlg::P256;
        env.clear_raw();
        env
    };
    let id = Uuid::new_v4();
    let mut env = draft(&mut w, SessionEvent::JoinApproved { subject: invite_subject(id), admission: Some(Admission::service(soon, None, key().public())), redeem_key: None });
    let challenge = hive_core::presence::challenge(&env);
    env.actor_stamp.as_mut().unwrap().presence = Some(w.recovery_presence("adm", &env));
    env.clear_raw();
    let good = hive_core::crypto::envelope_preimage(&env);
    let (inv, terms) = check_phone_admission_preimage(&good, &challenge, w.wid, phone.device_id, None).unwrap();
    assert_eq!((inv, terms.expires_ms), (id, soon));
    // Another workspace or another phone: refused.
    assert!(check_phone_admission_preimage(&good, &challenge, Uuid::new_v4(), phone.device_id, None).is_err());
    assert!(check_phone_admission_preimage(&good, &challenge, w.wid, Uuid::new_v4(), None).is_err());
    // An admission bound to a box the phone didn't ask for (the box that
    // relays it swapping in its own device key).
    assert!(check_phone_admission_preimage(&good, &challenge, w.wid, phone.device_id, Some(&[3; 32])).is_err());
    // Another admission than the passkey confirmed (a longer one, say).
    let mut other = draft(&mut w, SessionEvent::JoinApproved { subject: invite_subject(id), admission: Some(Admission::service(soon + 5 * HOUR_MS, None, key().public())), redeem_key: None });
    other.actor_stamp.as_mut().unwrap().presence = env.actor_stamp.as_ref().unwrap().presence.clone();
    other.clear_raw();
    let swapped = hive_core::crypto::envelope_preimage(&other);
    assert_eq!(check_phone_admission_preimage(&swapped, &challenge, w.wid, phone.device_id, None).unwrap_err(), "not the event the passkey confirmed");
    // A vote as the person: refused whatever its digest.
    let chat = w.chat("owner", "c");
    let mut vote = w.draft("adm", Some(chat), SessionEvent::ProposalVoteCast {
        proposal_id: Uuid::new_v4(),
        approval: ProposalApproval { actor_id: "adm".into(), role: WorkspaceRole::Admin, approved: true, created_at: Timestamp::epoch(), diff_digest: None },
    }, None);
    let s = vote.actor_stamp.as_mut().unwrap();
    s.actor.device_id = Some(phone.device_id);
    s.sig_alg = hive_core::crypto::SigAlg::P256;
    vote.clear_raw();
    let vote_challenge = hive_core::presence::challenge(&vote);
    vote.actor_stamp.as_mut().unwrap().presence = Some(w.recovery_presence("adm", &vote));
    vote.clear_raw();
    let v = hive_core::crypto::envelope_preimage(&vote);
    assert!(check_phone_admission_preimage(&v, &vote_challenge, w.wid, phone.device_id, None).is_err());
    // Truncated or padded bytes.
    assert!(check_phone_admission_preimage(&good[..good.len() - 1], &challenge, w.wid, phone.device_id, None).is_err());
    let mut padded = good.clone();
    padded.push(0);
    assert!(check_phone_admission_preimage(&padded, &challenge, w.wid, phone.device_id, None).is_err());
}

#[test]
fn an_admission_for_a_join_request_admits_only_that_boxs_device() {
    // Review R9: a code's seed is known to whoever relayed or issued it (the
    // serve box drafting a phone's admission). Bound to the requesting box's
    // device key, the admission can't be taken with the seed alone.
    let mut w = world();
    let id = Uuid::new_v4();
    let pk = w.people["box"].device.public_key_bytes();
    issue(&mut w, "adm", id, Admission::service(now_ms() + HOUR_MS, None, key().public()).for_device(&pk));
    let other = redeem_with(&mut w, "box2", id, &key(), None, None);
    assert_eq!(w.verdict(other), Verdict::Denied, "the seed without the box's device key");
    let mine = redeem(&mut w, "box", id, WorkspaceRole::Contributor, None);
    assert_eq!(w.verdict(mine), Verdict::Allowed);
    // A malformed device key isn't issued.
    let bad = issue(&mut w, "adm", Uuid::new_v4(), Admission::service(now_ms() + HOUR_MS, None, key().public()).for_device(&[1; 3]));
    assert_eq!(w.verdict(bad), Verdict::Denied);
}

/// A holder of the code copies the box's redemption id with a redemption of
/// their own (signed by their own device, so it is their own event, not an
/// equivocation): a closure naming that id can't tell which one its closer
/// saw, so it admits neither (fail closed), and never the thief.
#[test]
fn a_thief_copying_the_named_redemptions_id_is_never_admitted() {
    // Copies of one id from two signers fold in the order of their signer
    // ids: make the thief's sort first (the case a closure naming the id
    // would otherwise admit).
    let mut w = (0..256)
        .map(|_| world())
        .find(|w| w.people["box2"].device_id < w.people["box"].device_id)
        .expect("a world where the thief's device id sorts first");
    let id = Uuid::new_v4();
    issue(&mut w, "adm", id, Admission::service(now_ms() + HOUR_MS, None, key().public()));
    // The thief published its identity early (its chain link is older).
    w.publish_identity("box2");
    let ok = redeem(&mut w, "box", id, WorkspaceRole::Contributor, None);
    assert_eq!(w.verdict(ok), Verdict::Allowed);
    let close = w.push_bare("adm", SessionEvent::AdmissionClosed { invite: id, redeemed: Some(ok) });
    assert_eq!(w.verdict(close), Verdict::Allowed);
    // The thief's own redemption, under the box's redemption id.
    let m = w.member_of("box2", WorkspaceRole::Contributor);
    let proof = key().prove(w.wid, id, "box2", w.people["box2"].device_id);
    let orig = w.cfg.iter().find(|e| e.event_id == ok).unwrap().clone();
    let mut copy = w.draft("box2", None, SessionEvent::MemberAdded { member: m, invite: Some(id), proof: Some(proof) }, None);
    copy.event_id = ok;
    copy.lamport = orig.lamport;
    if let (Some(s), Some(o)) = (copy.actor_stamp.as_mut(), orig.actor_stamp.as_ref()) {
        s.log_head = o.log_head;
    }
    w.finish(&mut copy, "box2", None);
    w.push_env(None, copy);
    let roster = w.roster();
    assert!(!roster.members.contains_key("box2"), "the thief was admitted through a closed admission");
}

/// Without the code there is no redemption to confuse a closure with: a
/// self-certified stranger copying the box's redemption id with a
/// "redemption" carrying no proof (the invite id and the redemption id are
/// on the log for anyone holding the workspace key) leaves the box admitted.
#[test]
fn a_proofless_copy_of_the_named_redemption_doesnt_unadmit_the_box() {
    let mut w = world();
    let id = Uuid::new_v4();
    issue(&mut w, "adm", id, Admission::service(now_ms() + HOUR_MS, None, key().public()));
    w.publish_identity("eve");
    let ok = redeem(&mut w, "box", id, WorkspaceRole::Contributor, None);
    let close = w.push_bare("adm", SessionEvent::AdmissionClosed { invite: id, redeemed: Some(ok) });
    assert_eq!(w.verdict(close), Verdict::Allowed);
    assert!(w.roster().members.contains_key("box"), "control");
    let orig = w.cfg.iter().find(|e| e.event_id == ok).unwrap().clone();
    let mut m = w.member_of("eve", WorkspaceRole::Contributor);
    m.actor.kind = ActorKind::Service;
    let mut copy = w.draft("eve", None, SessionEvent::MemberAdded { member: m, invite: Some(id), proof: None }, None);
    copy.event_id = ok;
    copy.lamport = orig.lamport;
    if let (Some(s), Some(o)) = (copy.actor_stamp.as_mut(), orig.actor_stamp.as_ref()) {
        s.log_head = o.log_head;
    }
    w.finish(&mut copy, "eve", None);
    w.push_env(None, copy);
    let roster = w.roster();
    assert!(!roster.members.contains_key("eve"));
    assert!(roster.members.contains_key("box"), "a proofless copy of its redemption's id unadmitted the box");
}

/// The same with the box's own record and proof replayed: a stranger
/// certifies the box's device id for themselves and signs, under it, a copy
/// of the box's redemption (its member record and proof, which name that
/// device) with the stranger's account on the stamp.
#[test]
fn a_replayed_proof_through_a_squatted_device_id_doesnt_unadmit_the_box() {
    use hive_core::crypto::{sign_envelope, DeviceCertificate, SigningKeypair};
    let mut w = world();
    let id = Uuid::new_v4();
    issue(&mut w, "adm", id, Admission::service(now_ms() + HOUR_MS, None, key().public()));
    w.publish_identity("eve");
    let ok = redeem(&mut w, "box", id, WorkspaceRole::Contributor, None);
    w.push_bare("adm", SessionEvent::AdmissionClosed { invite: id, redeemed: Some(ok) });
    let box_device = w.people["box"].device_id;
    let k = SigningKeypair::generate().unwrap();
    let eve = &w.people["eve"];
    let cert = DeviceCertificate::issue_for_id(&eve.account, eve.account_id, box_device, &k.public_key_bytes(), hive_core::Timestamp::epoch());
    w.push_bare("eve", SessionEvent::DeviceCertificateAdded { certificate: cert });
    let orig = w.cfg.iter().find(|e| e.event_id == ok).unwrap().clone();
    let mut copy = orig.clone();
    if let Some(s) = copy.actor_stamp.as_mut() {
        s.actor.account_id = Some(w.people["eve"].account_id);
    }
    copy.clear_raw();
    sign_envelope(&mut copy, box_device, &k);
    w.push_env(None, copy);
    assert!(w.roster().members.contains_key("box"), "a replayed proof under a squatted device id unadmitted the box");
}
