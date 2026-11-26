//! Person invites carry a redeem key (fix/held-and-dispute, review): a
//! person invite's id is on the log for every member, so before this a
//! member could backdate a redemption of someone's invite before theirs and
//! take that admission, and with it the account's key anchor
//! (`ligus_core::account_keys`). Now an invite issued with a `redeemKey` is
//! redeemed only with that key's signature over the workspace, the invite,
//! the new member and the redeeming device, and the key's secret rides only
//! in the invite code.

use ligus_core::admission::{person_redemption_message, RedeemKey, PERSON_REDEEM_CONTEXT, REDEEM_CONTEXT};
use ligus_core::crypto::sign_envelope;
use ligus_core::events::Verdict;
use ligus_core::testkit::{Proof, SignedWorld};
use ligus_core::{CfgFrontier, SessionEvent, SessionEventEnvelope, WorkspaceRole};
use uuid::Uuid;

fn key() -> RedeemKey {
    RedeemKey::from_seed(&[0x42; 32]).unwrap()
}

/// owner founds; eve is an Admin-added Contributor; the owner issues an
/// invite for vic (with a redeem key when `keyed`).
fn world(keyed: bool) -> (SignedWorld, Uuid) {
    let mut w = SignedWorld::genesis(&["owner", "eve", "vic"], None);
    w.admit("owner", "eve", WorkspaceRole::Contributor);
    let invite = Uuid::new_v4();
    let redeem_key = keyed.then(|| key().public().to_vec());
    w.write(
        "owner",
        None,
        SessionEvent::JoinApproved { subject: ligus_core::authorization::invite_subject(invite), admission: None, redeem_key },
        None,
        Proof::Recovery,
    );
    (w, invite)
}

/// vic's own redemption (with the code's proof when `proof`).
fn vic_redeems(w: &mut SignedWorld, invite: Uuid, proof: bool) -> Uuid {
    let m = w.member_of("vic", WorkspaceRole::Contributor);
    let proof = proof.then(|| key().prove_person(w.wid, invite, &m.id, w.people["vic"].device_id));
    let id = w.push_bare("vic", SessionEvent::MemberAdded { member: m, invite: Some(invite), proof });
    w.publish_identity("vic");
    id
}

/// eve's redemption of vic's invite as vic, from her own device, backdated
/// right after the invite was issued, with `proof`.
fn eve_steals(w: &mut SignedWorld, invite: Uuid, proof: Option<Vec<u8>>) -> SessionEventEnvelope {
    let issued = w
        .cfg
        .iter()
        .find(|e| matches!(&e.payload, SessionEvent::JoinApproved { subject, .. } if *subject == ligus_core::authorization::invite_subject(invite)))
        .unwrap()
        .clone();
    let m = w.member_of("vic", WorkspaceRole::Contributor);
    let mut env = w.draft("eve", None, SessionEvent::MemberAdded { member: m, invite: Some(invite), proof }, None);
    let p = &w.people["eve"];
    let s = env.actor_stamp.as_mut().unwrap();
    let mut actor = w.people["vic"].actor();
    actor.device_id = Some(p.device_id);
    s.actor = actor;
    s.prev = None;
    let head = Some(CfgFrontier { lamport: issued.lamport, event_id: issued.event_id });
    s.log_head = head;
    env.lamport = ligus_core::lamport::next(head);
    env.event_id = Uuid::from_u128(1);
    env.clear_raw();
    sign_envelope(&mut env, p.device_id, &p.device);
    env
}

#[test]
fn the_person_redeem_context_is_frozen_and_distinct() {
    assert_eq!(PERSON_REDEEM_CONTEXT, b"ligus-person-invite-redeem-v1\0");
    assert_ne!(PERSON_REDEEM_CONTEXT, REDEEM_CONTEXT);
    let (ws, inv, dev) = (Uuid::from_u128(2), Uuid::from_u128(3), Uuid::from_u128(4));
    let m = person_redemption_message(ws, inv, "vic", dev);
    assert!(m.starts_with(PERSON_REDEEM_CONTEXT));
    assert_eq!(&m[PERSON_REDEEM_CONTEXT.len()..], &ligus_core::admission::redemption_message(ws, inv, "vic", dev)[REDEEM_CONTEXT.len()..]);
}

#[test]
fn a_member_cannot_steal_a_keyed_person_invite() {
    let (mut w, invite) = world(true);
    // eve can't sign the code's proof: none, one under another key, or
    // vic's own proof (it names vic's device, not hers).
    let vic_proof = key().prove_person(w.wid, invite, "vic", w.people["vic"].device_id);
    let wrong = RedeemKey::from_seed(&[9; 32]).unwrap().prove_person(w.wid, invite, "vic", w.people["eve"].device_id);
    let thefts: Vec<SessionEventEnvelope> = [None, Some(wrong), Some(vic_proof)].into_iter().map(|p| eve_steals(&mut w, invite, p)).collect();
    let real = vic_redeems(&mut w, invite, true);
    for t in &thefts {
        let mut w2 = w.clone();
        w2.push_env(None, t.clone());
        assert_eq!(w2.verdict(t.event_id), Verdict::Denied, "a backdated theft without the code's proof is refused");
        assert_eq!(w2.verdict(real), Verdict::Allowed, "vic's own redemption stands");
        let vic = w2.people["vic"].account_id;
        assert_eq!(w2.timeline().anchors().get(&vic), Some(&w2.people["vic"].device_id), "vic's account is anchored to her own device");
        assert_eq!(w2.roster().account_keys.get(&vic), Some(&w2.people["vic"].account.public_key_bytes().to_vec()));
    }
}

#[test]
fn a_keyed_invite_without_its_proof_admits_no_one() {
    let (mut w, invite) = world(true);
    let unproven = vic_redeems(&mut w, invite, false);
    assert_eq!(w.verdict(unproven), Verdict::Denied);
}

/// The residual for invites issued before the redeem key: a keyless invite
/// is still redeemable by whoever writes first in canonical order.
#[test]
fn a_keyless_invite_redeems_as_before() {
    let (mut w, invite) = world(false);
    let theft = eve_steals(&mut w, invite, None);
    let real = vic_redeems(&mut w, invite, false);
    w.push_env(None, theft.clone());
    assert_eq!(w.verdict(theft.event_id), Verdict::Allowed, "legacy: the backdated redemption takes the invite");
    // vic's own redemption then reads as a re-add, and the theft anchored
    // her account to eve's device.
    let _ = real;
    let vic = w.people["vic"].account_id;
    assert_eq!(w.timeline().anchors().get(&vic), Some(&w.people["eve"].device_id));
}

#[test]
fn a_redeem_key_belongs_to_a_person_invite_on_the_config_log() {
    let (mut w, _) = world(true);
    let k = key().public().to_vec();
    let bad = [
        SessionEvent::JoinApproved { subject: "someone".into(), admission: None, redeem_key: Some(k.clone()) },
        SessionEvent::JoinApproved { subject: ligus_core::authorization::invite_subject(Uuid::new_v4()), admission: None, redeem_key: Some(vec![1; 31]) },
        SessionEvent::JoinApproved {
            subject: ligus_core::authorization::invite_subject(Uuid::new_v4()),
            admission: Some(ligus_core::admission::Admission::service(i64::MAX / 2, None, key().public())),
            redeem_key: Some(k.clone()),
        },
    ];
    for e in bad {
        let id = w.push("owner", e);
        assert_eq!(w.verdict(id), Verdict::Denied);
    }
}
