//! Phase B: box admissions and the service-member rules.
//!
//! A `ligus serve` or `ligus worker` box joins as its own service member: an
//! Admin (desktop or phone, with passkey presence) issues a single-use,
//! expiring box admission; the box redeems it by adding itself, signed by
//! its own device, which binds the invite to that key. A service member is
//! capped at Contributor, never cuts or admits anyone, never carries
//! presence, never satisfies a human gate, and its device is revoked like
//! any other.

use hive_core::admission::{Admission, MAX_ADMISSION_TTL_MS};
use hive_core::authorization::invite_subject;
use hive_core::testkit::{Phone, SignedWorld};
use hive_core::{
    ActionProposal, ActorKind, ChatMessage, MemberRoleChange, MessageRole, ProposalApproval, SessionEvent, Timestamp, Verdict,
    WorkspaceRole,
};
use uuid::Uuid;

const HOUR_MS: i64 = 60 * 60 * 1000;

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
    w.push(by, SessionEvent::JoinApproved { subject: invite_subject(id), admission: Some(admission) })
}

/// `name` publishes its identity and redeems invite `id` as itself, at
/// `role`, signed at `when` (unix ms; `None` = now).
fn redeem(w: &mut SignedWorld, name: &str, id: Uuid, role: WorkspaceRole, when: Option<i64>) -> Uuid {
    w.publish_identity(name);
    let m = w.member_of(name, role);
    let mut env = w.draft(name, None, SessionEvent::MemberAdded { member: m, invite: Some(id) }, None);
    if let Some(ms) = when {
        env.timestamp = at_ms(ms);
    }
    w.finish(&mut env, name, None);
    w.push_env(None, env)
}

fn admitted(w: &mut SignedWorld, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    issue(w, "adm", id, Admission::service(now_ms() + HOUR_MS, None));
    let r = redeem(w, name, id, WorkspaceRole::Contributor, None);
    assert_eq!(w.verdict(r), Verdict::Allowed);
    id
}

#[test]
fn a_box_redeems_its_admission_once_as_a_contributor_service_member() {
    let mut w = world();
    let id = Uuid::new_v4();
    let i = issue(&mut w, "adm", id, Admission::service(now_ms() + HOUR_MS, None));
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
    issue(&mut w, "adm", id, Admission::service(now_ms() + HOUR_MS, None));
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
    issue(&mut w, "adm", id, Admission::service(expires, None));
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
    w.push("adm", SessionEvent::JoinApproved { subject: invite_subject(person), admission: None });
    let r = redeem(&mut w, "box", person, WorkspaceRole::Contributor, None);
    assert_eq!(w.verdict(r), Verdict::Denied, "a box joins only through a box admission");
    // Nor without any invite.
    let m = w.member_of("box", WorkspaceRole::Contributor);
    let bare = w.push_bare("box", SessionEvent::MemberAdded { member: m, invite: None });
    assert_eq!(w.verdict(bare), Verdict::Denied);
    // A box admission redeemed by a person.
    let boxed = Uuid::new_v4();
    issue(&mut w, "adm", boxed, Admission::service(now_ms() + HOUR_MS, None));
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
    issue(&mut w, "adm", id, Admission::service(now_ms() + HOUR_MS, Some(asked)));
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
    let by_mara = issue(&mut w, "mara", id, Admission::service(soon, None));
    assert_eq!(w.verdict(by_mara), Verdict::Denied);
    // No presence: no.
    let bare = w.push_bare("adm", SessionEvent::JoinApproved { subject: invite_subject(Uuid::new_v4()), admission: Some(Admission::service(soon, None)) });
    assert_eq!(w.verdict(bare), Verdict::Denied);
    // Expiry before issuing, or beyond the bound: no.
    let past = issue(&mut w, "adm", Uuid::new_v4(), Admission::service(now_ms() - HOUR_MS, None));
    let far = issue(&mut w, "adm", Uuid::new_v4(), Admission::service(now_ms() + MAX_ADMISSION_TTL_MS + HOUR_MS, None));
    assert_eq!((w.verdict(past), w.verdict(far)), (Verdict::Denied, Verdict::Denied));
    // Another kind: no.
    let human = issue(&mut w, "adm", Uuid::new_v4(), Admission { kind: ActorKind::Human, expires_ms: soon, account_id: None });
    assert_eq!(w.verdict(human), Verdict::Denied);
    // Not an invite subject: no.
    let odd = w.push("adm", SessionEvent::JoinApproved { subject: "bob".into(), admission: Some(Admission::service(soon, None)) });
    assert_eq!(w.verdict(odd), Verdict::Denied);
    // One admission per invite, and a person invite can't take its id.
    let id = Uuid::new_v4();
    let first = issue(&mut w, "adm", id, Admission::service(soon, None));
    assert_eq!(w.verdict(first), Verdict::Allowed);
    let twice = issue(&mut w, "owner", id, Admission::service(soon, Some(Uuid::new_v4())));
    let as_person = w.push("owner", SessionEvent::JoinApproved { subject: invite_subject(id), admission: None });
    assert_eq!((w.verdict(twice), w.verdict(as_person)), (Verdict::Denied, Verdict::Denied));
    // A person invite's id can't become an admission.
    let p = Uuid::new_v4();
    w.push("adm", SessionEvent::JoinApproved { subject: invite_subject(p), admission: None });
    let upgrade = issue(&mut w, "adm", p, Admission::service(soon, None));
    assert_eq!(w.verdict(upgrade), Verdict::Denied);
    // On a chat log: no.
    let chat = w.chat("owner", "c");
    let in_chat = w.write(
        "adm",
        Some(chat),
        SessionEvent::JoinApproved { subject: invite_subject(Uuid::new_v4()), admission: Some(Admission::service(soon, None)) },
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
    let bare = w.phone_write("adm", &phone, None, SessionEvent::JoinApproved { subject: invite_subject(id), admission: Some(Admission::service(soon, None)) });
    assert_eq!(w.verdict(bare), Verdict::Denied);
    // With the person's presence over the phone-signed event: allowed.
    let mut env = w.draft("adm", None, SessionEvent::JoinApproved { subject: invite_subject(id), admission: Some(Admission::service(soon, None)) }, None);
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
    let mut env = w.draft("adm", None, SessionEvent::JoinApproved { subject: invite_subject(Uuid::new_v4()), admission: None }, None);
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
        SessionEvent::MemberAdded { member: w.member_of("box2", WorkspaceRole::Viewer), invite: None },
        SessionEvent::JoinApproved { subject: invite_subject(Uuid::new_v4()), admission: None },
        SessionEvent::JoinApproved { subject: invite_subject(Uuid::new_v4()), admission: Some(Admission::service(now_ms() + HOUR_MS, None)) },
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
    let plain = SessionEvent::JoinApproved { subject: "invite:x".into(), admission: None };
    assert_eq!(serde_json::to_string(&plain).unwrap(), r#"{"kind":"joinApproved","subject":"invite:x"}"#);
    let a = SessionEvent::JoinApproved { subject: "invite:x".into(), admission: Some(Admission::service(5, None)) };
    let j = serde_json::to_string(&a).unwrap();
    assert_eq!(j, r#"{"kind":"joinApproved","subject":"invite:x","admission":{"kind":"service","expiresMs":5}}"#);
    assert_eq!(serde_json::from_str::<SessionEvent>(&j).unwrap(), a);
}

#[test]
fn a_member_cannot_spend_a_box_admission_by_re_adding_itself() {
    let mut w = world();
    let id = Uuid::new_v4();
    issue(&mut w, "adm", id, Admission::service(now_ms() + HOUR_MS, None));
    let m = w.member_of("mara", WorkspaceRole::Contributor);
    let spend = w.push_bare("mara", SessionEvent::MemberAdded { member: m, invite: Some(id) });
    assert_eq!(w.verdict(spend), Verdict::Denied);
    let r = redeem(&mut w, "box", id, WorkspaceRole::Contributor, None);
    assert_eq!(w.verdict(r), Verdict::Allowed, "the admission is still the box's");
}
