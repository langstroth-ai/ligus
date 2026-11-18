//! Phase A, item 6 (wire frozen in 0.1; Phase B builds the clients):
//! phones sign with their own P-256 Secure Enclave key, are enrolled only by
//! a trusted device of their person with that person's passkey, and are
//! capped to the phone tier; `ligus serve` becomes a service member, capped
//! at Contributor, that never has presence and never counts as a human.

use hive_core::crypto::{sign_envelope_p256, verify_envelope, verify_p256, SigAlg};
use hive_core::testkit::{Phone, SignedWorld};
use hive_core::{ActionProposal, ActorKind, ChatMessage, MessageRole, ProposalApproval, SessionEvent, Timestamp, Verdict, WorkspaceRole};
use uuid::Uuid;

fn world() -> SignedWorld {
    let mut w = SignedWorld::genesis(&["owner", "mara", "box"], None);
    w.admit("owner", "mara", WorkspaceRole::Contributor);
    w
}

fn enrolled(w: &mut SignedWorld, who: &str) -> Phone {
    let phone = Phone::new();
    let cert = w.phone_certificate(who, &phone);
    let id = w.push(who, SessionEvent::DeviceCertificateAdded { certificate: cert });
    assert_eq!(w.verdict(id), Verdict::Allowed);
    phone
}

#[test]
fn a_phone_is_enrolled_only_by_a_trusted_device_of_its_person_with_presence() {
    let mut w = world();
    let phone = Phone::new();
    let cert = w.phone_certificate("mara", &phone);
    // No presence: refused (even for an account that has a passkey).
    let bare = w.push_bare("mara", SessionEvent::DeviceCertificateAdded { certificate: cert.clone() });
    // The phone publishing its own certificate: refused (no self-enrollment).
    let self_enrolled = w.phone_write("mara", &phone, None, SessionEvent::DeviceCertificateAdded { certificate: cert.clone() });
    // Someone else's device, with their own presence: refused.
    let by_owner = w.push("owner", SessionEvent::DeviceCertificateAdded { certificate: cert.clone() });
    assert_eq!([w.verdict(bare), w.verdict(self_enrolled), w.verdict(by_owner)], [Verdict::Denied; 3]);
    let ok = w.push("mara", SessionEvent::DeviceCertificateAdded { certificate: cert });
    assert_eq!(w.verdict(ok), Verdict::Allowed);
    let d = &w.roster().devices[&phone.device_id];
    assert!(d.phone && !d.delegated_for(w.people["mara"].account_id));
}

#[test]
fn a_phone_posts_votes_and_asks_but_never_governs() {
    let mut w = world();
    let phone = enrolled(&mut w, "mara");
    let chat = w.chat("owner", "c");
    let post = w.phone_write("mara", &phone, Some(chat), SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "mara", "from my phone") });
    let p = ActionProposal::new("q", hive_core::ProposalKind::Decision, "owner");
    let pid = p.id;
    w.say(chat, "owner", SessionEvent::ProposalUpserted { proposal: p });
    let vote = w.phone_write("mara", &phone, Some(chat), SessionEvent::ProposalVoteCast {
        proposal_id: pid,
        approval: ProposalApproval { actor_id: "mara".into(), role: WorkspaceRole::Contributor, approved: true, created_at: Timestamp::epoch(), diff_digest: None },
    });
    let v = w.verdicts();
    assert_eq!((v[&post], v[&vote]), (Verdict::Allowed, Verdict::Allowed));
    // Never membership, roles, devices, keys or workspace config.
    let laptop = w.people["owner"].device_id;
    let denied = [
        w.phone_write("mara", &phone, None, SessionEvent::DeviceRevoked { device_id: laptop, watermark: vec![] }),
        w.phone_write("mara", &phone, None, SessionEvent::WorkspaceRulesSet { version: 2, presence: None }),
        w.phone_write("mara", &phone, Some(chat), SessionEvent::SessionTitleChanged { title: "x".into() }),
        w.phone_write("mara", &phone, Some(chat), SessionEvent::ChatRepoSet { repo_id: None, base_branch: String::new() }),
    ];
    let v = w.verdicts();
    for id in denied {
        assert_eq!(v[&id], Verdict::Denied);
    }
    // Its own revocation is allowed, with its person's presence (the
    // presence table covers self-revocation too).
    let bare = w.phone_write("mara", &phone, None, SessionEvent::DeviceRevoked { device_id: phone.device_id, watermark: vec![] });
    assert_eq!(w.verdict(bare), Verdict::Denied);
    let mut env = w.draft("mara", None, SessionEvent::DeviceRevoked { device_id: phone.device_id, watermark: vec![] }, None);
    env.actor_stamp.as_mut().unwrap().actor.device_id = Some(phone.device_id);
    env.actor_stamp.as_mut().unwrap().sig_alg = SigAlg::P256;
    let a = w.recovery_presence("mara", &env);
    env.actor_stamp.as_mut().unwrap().presence = Some(a);
    sign_envelope_p256(&mut env, phone.device_id, &phone.key);
    let own = w.push_env(None, env);
    assert_eq!(w.verdict(own), Verdict::Allowed);
}

#[test]
fn p256_envelopes_are_algorithm_tagged_low_s_and_bound() {
    let mut w = world();
    let phone = enrolled(&mut w, "mara");
    let chat = w.chat("owner", "c");
    let id = w.phone_write("mara", &phone, Some(chat), SessionEvent::SessionTitleChanged { title: "t".into() });
    let env = w.chats[&chat].iter().find(|e| e.event_id == id).unwrap().clone();
    assert_eq!(env.actor_stamp.as_ref().unwrap().sig_alg, SigAlg::P256);
    verify_envelope(&env, &phone.public_key()).unwrap();
    // Untagged (claims Ed25519): no longer verifies.
    let mut untagged = env.clone();
    untagged.actor_stamp.as_mut().unwrap().sig_alg = SigAlg::Ed25519;
    untagged.clear_raw();
    assert!(verify_envelope(&untagged, &phone.public_key()).is_err());
    // The high-S twin of a valid signature is refused (one encoding only).
    let preimage = hive_core::crypto::envelope_preimage(&env);
    let sig = env.signature.clone().unwrap();
    verify_p256(&phone.public_key(), &preimage, &sig).unwrap();
    let high = high_s(&sig);
    assert!(verify_p256(&phone.public_key(), &preimage, &high).is_err());
    // A P-256 certificate re-labelled Ed25519 no longer verifies (the tag is
    // inside the account's signature).
    let mut cert = w.phone_certificate("mara", &phone);
    let account = w.people["mara"].account.public_key_bytes();
    cert.verify(&account).unwrap();
    cert.alg = SigAlg::Ed25519;
    assert!(cert.verify(&account).is_err());
    // An Ed25519 device's envelope never verifies under a P-256 tag.
    let mut e2 = env.clone();
    sign_envelope_p256(&mut e2, phone.device_id, &phone.key);
    assert!(verify_envelope(&e2, &w.people["mara"].device.public_key_bytes()).is_err());
}

/// n - s for the P-256 group order n (big-endian 32-byte arithmetic).
fn high_s(sig: &[u8]) -> Vec<u8> {
    const N: [u8; 32] = [
        0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xbc, 0xe6, 0xfa, 0xad,
        0xa7, 0x17, 0x9e, 0x84, 0xf3, 0xb9, 0xca, 0xc2, 0xfc, 0x63, 0x25, 0x51,
    ];
    let s = &sig[32..];
    let mut out = [0u8; 32];
    let mut borrow = 0i16;
    for i in (0..32).rev() {
        let mut d = N[i] as i16 - s[i] as i16 - borrow;
        borrow = 0;
        if d < 0 {
            d += 256;
            borrow = 1;
        }
        out[i] = d as u8;
    }
    let mut v = sig[..32].to_vec();
    v.extend_from_slice(&out);
    v
}

#[test]
fn a_service_member_is_capped_and_never_a_human() {
    let mut w = world();
    w.kinds.insert("box".into(), ActorKind::Service);
    let as_admin = w.push("owner", SessionEvent::MemberAdded { member: w.member_of("box", WorkspaceRole::Admin), invite: None });
    assert_eq!(w.verdict(as_admin), Verdict::Denied, "a service member holds at most Contributor");
    w.admit("owner", "box", WorkspaceRole::Contributor);
    assert!(w.config().members.iter().any(|m| m.id == "box" && m.actor.kind == ActorKind::Service));
    // admit() also had it register a recovery key: refused for a service.
    assert!(w.roster().passkeys.get(&w.people["box"].account_id).is_none(), "a service member holds no passkey");
    let raise = w.push("owner", SessionEvent::MemberRoleChanged {
        change: hive_core::MemberRoleChange { member_id: "box".into(), old_role: WorkspaceRole::Contributor, new_role: WorkspaceRole::Admin },
        watermark: vec![],
    });
    assert_eq!(w.verdict(raise), Verdict::Denied);
    // It never applies on its own, and never decides a set.
    let chat = w.chat("owner", "c");
    let p = ActionProposal::new("q", hive_core::ProposalKind::Decision, "owner");
    let pid = p.id;
    w.say(chat, "owner", SessionEvent::ProposalUpserted { proposal: p });
    let d = hive_core::authz::votes::diff_digest(&w.state(chat).proposals[0]);
    let vote = w.say(chat, "box", SessionEvent::ProposalVoteCast {
        proposal_id: pid,
        approval: ProposalApproval { actor_id: "box".into(), role: WorkspaceRole::Contributor, approved: true, created_at: Timestamp::epoch(), diff_digest: Some(d.clone()) },
    });
    assert_eq!(w.verdict(vote), Verdict::Allowed, "its vote counts by its role");
    let s = w.state(chat);
    assert!(!hive_core::guard::has_human_approval(&s, pid), "a service's approval never satisfies a human gate");
    let apply = w.say(chat, "box", SessionEvent::ProposalApplied {
        proposal_id: pid, diff_digest: d, commit: None, at: Timestamp::epoch(),
        repo_id: None, applied_on: Some(w.people["box"].device_id), base_commit: None, applied_tree_hash: None, request_id: None,
    });
    let decide = w.say(chat, "box", SessionEvent::ProposalSetDecided { set_id: Uuid::new_v4(), winner: None, at: Timestamp::epoch() });
    let v = w.verdicts();
    assert_eq!((v[&apply], v[&decide]), (Verdict::Denied, Verdict::Denied));
}
