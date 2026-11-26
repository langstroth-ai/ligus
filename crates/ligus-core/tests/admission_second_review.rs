//! Second, independent review of the box-admission fixes (R1, R5–R8).
//!
//! Each test attacks one fix: a redemption replayed or transplanted to
//! another workspace or invite, a forged or changed account on a bearer
//! redemption, closures that name nothing or something else, a closure by a
//! cut Admin, equivocated redemptions and closures, the phone's preimage
//! check, and the fold's independence from arrival order with all of them
//! interleaved (a property run at `PROPTEST_CASES=4096` in release).

use ligus_core::admission::{
    check_phone_admission_preimage, redemption_message, Admission, RedeemKey, REDEEM_CONTEXT,
};
use ligus_core::authorization::invite_subject;
use ligus_core::events::{fold_decisions, WorkspaceAuthz};
use ligus_core::testkit::{Phone, SignedWorld};
use ligus_core::{
    ActorKind, MemberRoleChange, SessionEvent, SessionEventEnvelope, Timestamp, Verdict,
    WorkspaceRole,
};
use proptest::prelude::*;
use std::collections::BTreeMap;
use uuid::Uuid;

const HOUR_MS: i64 = 60 * 60 * 1000;

fn key() -> RedeemKey {
    RedeemKey::from_seed(&[7; 32]).unwrap()
}

fn now_ms() -> i64 {
    (Timestamp::now().inner().unix_timestamp_nanos() / 1_000_000) as i64
}

fn world() -> SignedWorld {
    let mut w = SignedWorld::genesis(&["owner", "adm", "mara", "box", "box2", "eve"], None);
    w.admit("owner", "adm", WorkspaceRole::Admin);
    w.admit("owner", "mara", WorkspaceRole::Contributor);
    w.kinds.insert("box".into(), ActorKind::Service);
    w.kinds.insert("box2".into(), ActorKind::Service);
    w.publish_identity("box");
    w.publish_identity("box2");
    w
}

fn issue(w: &mut SignedWorld, id: Uuid, a: Admission) -> Uuid {
    w.push(
        "adm",
        SessionEvent::JoinApproved {
            subject: invite_subject(id),
            admission: Some(a),
            redeem_key: None,
        },
    )
}

/// `name` redeems `id` with `proof`, its member record edited by `edit`.
fn redeem_raw(
    w: &mut SignedWorld,
    name: &str,
    id: Uuid,
    proof: Vec<u8>,
    edit: impl FnOnce(&mut ligus_core::WorkspaceMember),
    lamport: Option<u64>,
) -> Uuid {
    let mut m = w.member_of(name, WorkspaceRole::Contributor);
    edit(&mut m);
    let mut env = w.draft(
        name,
        None,
        SessionEvent::MemberAdded {
            member: m,
            invite: Some(id),
            proof: Some(proof),
        },
        None,
    );
    if let Some(l) = lamport {
        env.lamport = l;
        w.finish(&mut env, name, None);
    }
    w.push_env(None, env)
}

fn redeem(w: &mut SignedWorld, name: &str, id: Uuid) -> Uuid {
    let proof = key().prove(w.wid, id, name, w.people[name].device_id);
    redeem_raw(w, name, id, proof, |_| {}, None)
}

fn lamport_of(w: &SignedWorld, id: Uuid) -> u64 {
    w.cfg.iter().find(|e| e.event_id == id).unwrap().lamport
}

#[test]
fn a_proof_is_bound_to_its_workspace_invite_member_and_device() {
    let mut w = world();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    // Two admissions under one redeem key (an issuer reusing a code's key).
    issue(
        &mut w,
        a,
        Admission::service(now_ms() + HOUR_MS, None, key().public()),
    );
    issue(
        &mut w,
        b,
        Admission::service(now_ms() + HOUR_MS, None, key().public()),
    );
    let dev = w.people["box"].device_id;
    // Transplanted from another workspace.
    let other_ws = key().prove(Uuid::new_v4(), a, "box", dev);
    let t1 = redeem_raw(&mut w, "box", a, other_ws, |_| {}, None);
    assert_eq!(
        w.verdict(t1),
        Verdict::Denied,
        "a proof for another workspace"
    );
    // Transplanted from another invite.
    let for_b = key().prove(w.wid, b, "box", dev);
    let t2 = redeem_raw(&mut w, "box", a, for_b.clone(), |_| {}, None);
    assert_eq!(w.verdict(t2), Verdict::Denied, "a proof for another invite");
    // For another member id through the same device.
    let for_other = key().prove(w.wid, a, "box-x", dev);
    let t3 = redeem_raw(&mut w, "box", a, for_other, |_| {}, None);
    assert_eq!(w.verdict(t3), Verdict::Denied, "a proof for another member");
    // The right one still works, once.
    let ok = redeem(&mut w, "box", a);
    assert_eq!(w.verdict(ok), Verdict::Allowed);
    // Replayed by the same box on the other invite: it is a member now.
    let again = redeem_raw(&mut w, "box", b, for_b, |_| {}, None);
    assert_eq!(
        w.verdict(again),
        Verdict::Denied,
        "a member never spends a second admission"
    );
}

#[test]
fn a_signer_device_claim_without_its_key_redeems_nothing() {
    // The proof names the signing device; claiming the box's device id while
    // signing with another key must not borrow the box's proof.
    let mut w = world();
    let id = Uuid::new_v4();
    issue(
        &mut w,
        id,
        Admission::service(now_ms() + HOUR_MS, None, key().public()),
    );
    let box_dev = w.people["box"].device_id;
    let stolen = key().prove(w.wid, id, "box", box_dev);
    let m = w.member_of("box", WorkspaceRole::Contributor);
    let mut env = w.draft(
        "box2",
        None,
        SessionEvent::MemberAdded {
            member: m,
            invite: Some(id),
            proof: Some(stolen),
        },
        None,
    );
    {
        let s = env.actor_stamp.as_mut().unwrap();
        s.actor.id = "box".into();
        s.actor.account_id = Some(w.people["box"].account_id);
        s.actor.device_id = Some(box_dev);
    }
    w.finish(&mut env, "box2", None);
    env.signer_device_id = Some(box_dev);
    let forged = w.push_env(None, env);
    assert_ne!(
        w.verdict(forged),
        Verdict::Allowed,
        "signed by box2's key under box's device id"
    );
    assert!(!w.roster().members.contains_key("box"));
}

#[test]
fn a_bearer_redemption_names_the_redeeming_account() {
    // An unbound (bearer) admission names no account, so nothing tied the
    // new member's account to the device that redeems it: a box could join
    // as a member whose record names a person's account (or none, and fill
    // one in later by re-adding itself).
    let mut w = world();
    let id = Uuid::new_v4();
    issue(
        &mut w,
        id,
        Admission::service(now_ms() + HOUR_MS, None, key().public()),
    );
    let owner_account = w.people["owner"].account_id;
    let dev = w.people["box"].device_id;
    let p = key().prove(w.wid, id, "box", dev);
    let other = redeem_raw(
        &mut w,
        "box",
        id,
        p.clone(),
        |m| m.actor.account_id = Some(owner_account),
        None,
    );
    assert_eq!(
        w.verdict(other),
        Verdict::Denied,
        "a member record naming another account"
    );
    let none = redeem_raw(
        &mut w,
        "box",
        id,
        p.clone(),
        |m| m.actor.account_id = None,
        None,
    );
    assert_eq!(
        w.verdict(none),
        Verdict::Denied,
        "a member record naming no account"
    );
    let ok = redeem_raw(&mut w, "box", id, p, |_| {}, None);
    assert_eq!(w.verdict(ok), Verdict::Allowed);
}

#[test]
fn a_service_member_re_adding_itself_changes_neither_kind_account_nor_identity() {
    // R1, every member-record field that carries authority.
    let mut w = world();
    let id = Uuid::new_v4();
    issue(
        &mut w,
        id,
        Admission::service(now_ms() + HOUR_MS, None, key().public()),
    );
    let r = redeem(&mut w, "box", id);
    assert_eq!(w.verdict(r), Verdict::Allowed);
    let owner_account = w.people["owner"].account_id;
    type Edit = Box<dyn Fn(&mut ligus_core::WorkspaceMember)>;
    let cases: Vec<(&str, Edit)> = vec![
        ("kind", Box::new(|m| m.actor.kind = ActorKind::Human)),
        ("kind agent", Box::new(|m| m.actor.kind = ActorKind::Agent)),
        (
            "account",
            Box::new(move |m| m.actor.account_id = Some(owner_account)),
        ),
        ("no account", Box::new(|m| m.actor.account_id = None)),
        ("actor id", Box::new(|m| m.actor.id = "owner".into())),
        ("role up", Box::new(|m| m.role = WorkspaceRole::Admin)),
    ];
    for (what, edit) in cases {
        let mut m = w.member_of("box", WorkspaceRole::Contributor);
        edit(&mut m);
        let e = w.push_bare(
            "box",
            SessionEvent::MemberAdded {
                member: m,
                invite: None,
                proof: None,
            },
        );
        assert_eq!(w.verdict(e), Verdict::Denied, "{what}");
        let r = w.roster();
        assert_eq!(
            (
                r.members["box"].kind,
                r.members["box"].role,
                r.members["box"].account_id
            ),
            (
                ActorKind::Service,
                WorkspaceRole::Contributor,
                Some(w.people["box"].account_id)
            ),
            "{what}"
        );
    }
    // A cosmetic re-add (a new display name) stays fine; demoting itself too.
    let mut m = w.member_of("box", WorkspaceRole::Contributor);
    m.actor.display_name = "build box".into();
    let ok = w.push_bare(
        "box",
        SessionEvent::MemberAdded {
            member: m,
            invite: None,
            proof: None,
        },
    );
    assert_eq!(w.verdict(ok), Verdict::Allowed);
}

#[test]
fn a_closure_naming_nothing_real_denies_only_that_invites_redemptions() {
    // An Admin's closure that names a redemption that doesn't exist (or one
    // of another invite) denies every redemption of its own invite, and
    // nothing else. An Admin can deny a legitimate box this way: closing is
    // the entrance policy's, like refusing to issue (documented).
    let mut w = world();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    issue(
        &mut w,
        a,
        Admission::service(now_ms() + HOUR_MS, None, key().public()),
    );
    issue(
        &mut w,
        b,
        Admission::service(now_ms() + HOUR_MS, None, key().public()),
    );
    let ra = redeem(&mut w, "box", a);
    let rb = redeem(&mut w, "box2", b);
    assert_eq!(
        (w.verdict(ra), w.verdict(rb)),
        (Verdict::Allowed, Verdict::Allowed)
    );
    // Names b's redemption while closing a: a's is denied, b's stands.
    let c = w.push_bare(
        "adm",
        SessionEvent::AdmissionClosed {
            invite: a,
            redeemed: Some(rb),
        },
    );
    assert_eq!(w.verdict(c), Verdict::Allowed);
    assert_eq!(
        (w.verdict(ra), w.verdict(rb)),
        (Verdict::Denied, Verdict::Allowed)
    );
    // Names an id nobody wrote: still only a's.
    let mut w2 = world();
    issue(
        &mut w2,
        a,
        Admission::service(now_ms() + HOUR_MS, None, key().public()),
    );
    issue(
        &mut w2,
        b,
        Admission::service(now_ms() + HOUR_MS, None, key().public()),
    );
    let ra = redeem(&mut w2, "box", a);
    let rb = redeem(&mut w2, "box2", b);
    w2.push_bare(
        "adm",
        SessionEvent::AdmissionClosed {
            invite: a,
            redeemed: Some(Uuid::new_v4()),
        },
    );
    assert_eq!(
        (w2.verdict(ra), w2.verdict(rb)),
        (Verdict::Denied, Verdict::Allowed)
    );
    assert!(!w2.roster().members.contains_key("box") && w2.roster().members.contains_key("box2"));
    // People are untouched.
    assert!(w2.roster().members.contains_key("mara"));
}

#[test]
fn closures_that_disagree_admit_no_one() {
    let mut w = world();
    let id = Uuid::new_v4();
    issue(
        &mut w,
        id,
        Admission::service(now_ms() + HOUR_MS, None, key().public()),
    );
    let ok = redeem(&mut w, "box", id);
    w.push_bare(
        "adm",
        SessionEvent::AdmissionClosed {
            invite: id,
            redeemed: Some(ok),
        },
    );
    assert_eq!(w.verdict(ok), Verdict::Allowed);
    w.push_bare(
        "owner",
        SessionEvent::AdmissionClosed {
            invite: id,
            redeemed: None,
        },
    );
    assert_eq!(
        w.verdict(ok),
        Verdict::Denied,
        "fail closed: any closure that doesn't name it"
    );
}

#[test]
fn a_cut_admins_uncovered_closure_never_counts() {
    // The closure is judged after the cuts: an Admin removed with a
    // watermark that doesn't cover their closure leaves it denied, and the
    // redemption it would have denied stands, on every device.
    let mut w = world();
    let id = Uuid::new_v4();
    let issued = issue(
        &mut w,
        id,
        Admission::service(now_ms() + HOUR_MS, None, key().public()),
    );
    let ok = redeem(&mut w, "box", id);
    let close = w.push_bare(
        "adm",
        SessionEvent::AdmissionClosed {
            invite: id,
            redeemed: None,
        },
    );
    assert_eq!(w.verdict(ok), Verdict::Denied);
    let cut = w.push(
        "owner",
        SessionEvent::MemberRemoved {
            member_id: "adm".into(),
            watermark: vec![],
        },
    );
    assert_eq!(w.verdict(cut), Verdict::Allowed);
    assert_eq!(
        w.verdict(close),
        Verdict::Denied,
        "an uncovered act of the cut Admin"
    );
    // The issue itself was the cut Admin's too, so nothing is admitted.
    assert_eq!(w.verdict(issued), Verdict::Denied);
    assert_eq!(w.verdict(ok), Verdict::Denied);
    assert_order_independent(&w);
}

#[test]
fn an_equivocated_redemption_or_closure_never_counts() {
    let mut w = world();
    let id = Uuid::new_v4();
    issue(
        &mut w,
        id,
        Admission::service(now_ms() + HOUR_MS, None, key().public()),
    );
    let ok = redeem(&mut w, "box", id);
    let close = w.push_bare(
        "adm",
        SessionEvent::AdmissionClosed {
            invite: id,
            redeemed: Some(ok),
        },
    );
    // The Admin equivocates its closure: neither copy counts, so the
    // redemption stands on its own (the residual before any close).
    let mut w1 = w.clone();
    let orig = w1.cfg.iter().find(|e| e.event_id == close).unwrap().clone();
    let mut twin = w1.draft(
        "adm",
        None,
        SessionEvent::AdmissionClosed {
            invite: id,
            redeemed: None,
        },
        None,
    );
    twin.event_id = close;
    twin.lamport = orig.lamport;
    twin.actor_stamp.as_mut().unwrap().prev = orig.actor_stamp.as_ref().and_then(|s| s.prev);
    w1.finish(&mut twin, "adm", None);
    w1.push_env(None, twin);
    assert_eq!(w1.verdict(close), Verdict::Denied);
    assert_eq!(w1.verdict(ok), Verdict::Allowed);
    assert_order_independent(&w1);
    // The box equivocates its redemption: neither copy admits it, and the
    // closure naming that id then admits no one.
    let mut w2 = w.clone();
    let orig = w2.cfg.iter().find(|e| e.event_id == ok).unwrap().clone();
    let proof = key().prove(w2.wid, id, "box", w2.people["box"].device_id);
    let mut m = w2.member_of("box", WorkspaceRole::Contributor);
    m.actor.display_name = "other".into();
    let mut twin = w2.draft(
        "box",
        None,
        SessionEvent::MemberAdded {
            member: m,
            invite: Some(id),
            proof: Some(proof),
        },
        None,
    );
    twin.event_id = ok;
    twin.lamport = orig.lamport;
    twin.actor_stamp.as_mut().unwrap().prev = orig.actor_stamp.as_ref().and_then(|s| s.prev);
    w2.finish(&mut twin, "box", None);
    w2.push_env(None, twin);
    assert_eq!(w2.verdict(ok), Verdict::Denied);
    assert!(!w2.roster().members.contains_key("box"));
    assert_order_independent(&w2);
}

#[test]
fn a_closure_sorting_before_the_redemption_it_names_admits_no_one() {
    // An honest closer always sorts after what it saw; a closure that sorts
    // first (forged lamport) closes the admission before the named
    // redemption folds, so the in-fold check denies it: fail closed.
    let mut w = world();
    let id = Uuid::new_v4();
    let issued = issue(
        &mut w,
        id,
        Admission::service(now_ms() + HOUR_MS, None, key().public()),
    );
    // Something in between, so the closure can sort strictly between the
    // issue and the redemption.
    issue(
        &mut w,
        Uuid::new_v4(),
        Admission::service(now_ms() + HOUR_MS, None, key().public()),
    );
    let ok = redeem(&mut w, "box", id);
    let mut env = w.draft(
        "adm",
        None,
        SessionEvent::AdmissionClosed {
            invite: id,
            redeemed: Some(ok),
        },
        None,
    );
    // Backdated as far as the lamport rule allows: naming the issue as its
    // head (and no chain link newer than that).
    ligus_core::lamport::stamp_next(
        &mut env,
        Some(ligus_core::CfgFrontier {
            lamport: lamport_of(&w, issued),
            event_id: issued,
        }),
    );
    env.actor_stamp.as_mut().unwrap().prev = None;
    w.finish(&mut env, "adm", None);
    w.push_env(None, env);
    assert_eq!(w.verdict(ok), Verdict::Denied);
    assert_order_independent(&w);
}

#[test]
fn a_phone_checks_strictly_what_it_signs() {
    let mut w = world();
    let phone = Phone::new();
    let cert = w.phone_certificate("adm", &phone);
    w.push(
        "adm",
        SessionEvent::DeviceCertificateAdded { certificate: cert },
    );
    let soon = now_ms() + HOUR_MS;
    let id = Uuid::new_v4();
    let draft = |w: &mut SignedWorld, chat: Option<Uuid>, payload: SessionEvent| {
        let mut env = w.draft("adm", chat, payload, None);
        let s = env.actor_stamp.as_mut().unwrap();
        s.actor.device_id = Some(phone.device_id);
        s.sig_alg = ligus_core::crypto::SigAlg::P256;
        env.clear_raw();
        let challenge = ligus_core::presence::challenge(&env);
        env.actor_stamp.as_mut().unwrap().presence = Some(w.recovery_presence("adm", &env));
        env.clear_raw();
        (env, challenge)
    };
    let admission = SessionEvent::JoinApproved {
        subject: invite_subject(id),
        admission: Some(Admission::service(soon, None, key().public())),
        redeem_key: None,
    };
    let (env, challenge) = draft(&mut w, None, admission.clone());
    let good = ligus_core::crypto::envelope_preimage(&env);
    assert!(
        check_phone_admission_preimage(&good, &challenge, w.wid, phone.device_id, None).is_ok()
    );
    // Another version prefix.
    let mut v2 = good.clone();
    v2[15] = b'2';
    assert!(check_phone_admission_preimage(&v2, &challenge, w.wid, phone.device_id, None).is_err());
    // The same admission on a chat log of this workspace.
    let chat = w.chat("owner", "c");
    let (in_chat, c2) = draft(&mut w, Some(chat), admission);
    let bytes = ligus_core::crypto::envelope_preimage(&in_chat);
    assert_eq!(
        check_phone_admission_preimage(&bytes, &c2, w.wid, phone.device_id, None).unwrap_err(),
        "not on this workspace's config log"
    );
    // A payload carrying a field this build doesn't know (a newer build
    // might read it): refused, the phone signs only what it understood.
    let (env, _) = draft(
        &mut w,
        None,
        SessionEvent::AdmissionClosed {
            invite: id,
            redeemed: None,
        },
    );
    let mut json: serde_json::Value = serde_json::to_value(&SessionEvent::JoinApproved {
        subject: invite_subject(id),
        admission: Some(Admission::service(soon, None, key().public())),
        redeem_key: None,
    })
    .unwrap();
    json["admission"]["role"] = serde_json::json!("owner");
    let payload = serde_json::to_vec(&json).unwrap();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"hive-envelope-v3\0");
    bytes.extend_from_slice(env.workspace_id.as_bytes());
    bytes.extend_from_slice(env.session_id.as_bytes());
    bytes.extend_from_slice(&env.sequence.to_le_bytes());
    bytes.extend_from_slice(&env.lamport.to_le_bytes());
    bytes.extend_from_slice(env.event_id.as_bytes());
    let stamp = serde_json::to_vec(&env.actor_stamp).unwrap();
    for f in [
        serde_json::to_vec(&env.scope).unwrap(),
        serde_json::to_vec(&env.timestamp).unwrap(),
        stamp,
        payload.clone(),
    ] {
        bytes.extend_from_slice(&(f.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&f);
    }
    let digest = {
        use sha2::{Digest, Sha256};
        let s = env.actor_stamp.as_ref().unwrap();
        let frontier = s
            .cfg_frontier
            .map(|f| serde_json::to_vec(&f).unwrap())
            .unwrap_or_default();
        let mut h = Sha256::new();
        h.update(ligus_core::presence::PRESENCE_CONTEXT);
        h.update(env.workspace_id.as_bytes());
        h.update(env.session_id.as_bytes());
        h.update(env.event_id.as_bytes());
        h.update(env.lamport.to_le_bytes());
        h.update((payload.len() as u32).to_le_bytes());
        h.update(&payload);
        h.update((frontier.len() as u32).to_le_bytes());
        h.update(&frontier);
        h.update(s.actor.account_id.unwrap().as_bytes());
        let d: [u8; 32] = h.finalize().into();
        d
    };
    assert_eq!(
        check_phone_admission_preimage(&bytes, &digest, w.wid, phone.device_id, None).unwrap_err(),
        "a payload with fields this build doesn't read"
    );
    // An overlong length prefix and an empty input.
    let mut long = good.clone();
    let at = 17 + 16 + 16 + 8 + 8 + 16;
    long[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(
        check_phone_admission_preimage(&long, &challenge, w.wid, phone.device_id, None).is_err()
    );
    assert!(check_phone_admission_preimage(&[], &challenge, w.wid, phone.device_id, None).is_err());
}

/// Known-answer vectors for the new identifiers (documented in
/// `docs/design/v2/phase0-specs.md`): the redemption preimage and its
/// signature under a fixed seed.
#[test]
fn redemption_proof_golden_vector() {
    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }
    let u = |n: u128| Uuid::from_u128(n);
    assert_eq!(
        hex(REDEEM_CONTEXT),
        "6c696775732d626f782d61646d697373696f6e2d72656465656d2d763100"
    );
    let m = redemption_message(u(2), u(0x71), "box-1", u(1));
    assert_eq!(
        hex(&m),
        concat!(
            "6c696775732d626f782d61646d697373696f6e2d72656465656d2d763100",
            "00000000000000000000000000000002",
            "00000000000000000000000000000071",
            "05000000",
            "626f782d31",
            "00000000000000000000000000000001"
        )
    );
    let k = RedeemKey::from_seed(&[0x5a; 32]).unwrap();
    let sig = k.prove(u(2), u(0x71), "box-1", u(1));
    assert!(ligus_core::crypto::verify(&k.public(), &m, &sig).is_ok());
    assert_eq!(hex(&k.public()), GOLDEN_REDEEM_PUBLIC);
    assert_eq!(hex(&sig), GOLDEN_REDEEM_SIG);
}

const GOLDEN_REDEEM_PUBLIC: &str =
    "0d7550754e0800a5d237eef5826035766b9b3e5a15868a940ab289958788e3b0";
const GOLDEN_REDEEM_SIG: &str = "44971b77a518223837bc29acc59858404371b5348b7bfb9a9525409e0455770e73148f4c7b32f57db578308c27f2fc12d26c50744fc5bc34c75eed331a19a005";

// ---------------------------------------------------------------------------
// Order independence: redemptions, closures, cuts and equivocations
// interleaved, folded in any arrival order.
// ---------------------------------------------------------------------------

fn verdicts_of(log: &[SessionEventEnvelope]) -> BTreeMap<Uuid, Verdict> {
    fold_decisions(log, None, None, WorkspaceAuthz::Unchecked)
}

fn members_of(log: &[SessionEventEnvelope]) -> Vec<(String, ActorKind, WorkspaceRole)> {
    let (s, _) = ligus_core::events::project_config(log, None, None);
    let mut v: Vec<_> = s
        .map(|s| {
            s.members
                .into_iter()
                .map(|m| (m.id, m.actor.kind, m.role))
                .collect()
        })
        .unwrap_or_default();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

fn assert_order_independent(w: &SignedWorld) {
    let base = verdicts_of(&w.cfg);
    let members = members_of(&w.cfg);
    let mut rev = w.cfg.clone();
    rev.reverse();
    assert_eq!(base, verdicts_of(&rev));
    assert_eq!(members, members_of(&rev));
}

#[derive(Debug, Clone)]
enum Op {
    Issue {
        slot: u8,
        bind_device: bool,
    },
    Redeem {
        who: u8,
        slot: u8,
        holds: bool,
        back: bool,
    },
    Close {
        by: u8,
        slot: u8,
        name: u8,
    },
    Cut {
        target: u8,
        kind: u8,
        honest: bool,
    },
    Equivocate {
        pick: u8,
    },
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0u8..3, any::<bool>()).prop_map(|(slot, bind_device)| Op::Issue { slot, bind_device }),
        (0u8..4, 0u8..3, any::<bool>(), any::<bool>()).prop_map(|(who, slot, holds, back)| {
            Op::Redeem {
                who,
                slot,
                holds,
                back,
            }
        }),
        (0u8..3, 0u8..3, 0u8..3).prop_map(|(by, slot, name)| Op::Close { by, slot, name }),
        (0u8..4, 0u8..3, any::<bool>()).prop_map(|(target, kind, honest)| Op::Cut {
            target,
            kind,
            honest
        }),
        any::<u8>().prop_map(|pick| Op::Equivocate { pick }),
    ]
}

const REDEEMERS: [&str; 4] = ["box", "box2", "mara", "eve"];
const CLOSERS: [&str; 3] = ["adm", "owner", "mara"];
const CUTTABLE: [&str; 4] = ["adm", "mara", "box", "box2"];

fn honest_watermark(w: &SignedWorld, who: &str) -> Vec<ligus_core::watermark::Watermark> {
    let signers: std::collections::BTreeSet<Uuid> = [w.people[who].device_id].into_iter().collect();
    ligus_core::watermark::heads_of(
        &w.cfg,
        ligus_core::workspace_config_session_id(w.wid),
        &signers,
    )
}

fn play(w: &mut SignedWorld, slots: &[Uuid; 3], ops: &[Op]) {
    let now = now_ms();
    for op in ops {
        match op.clone() {
            Op::Issue { slot, bind_device } => {
                let mut a = Admission::service(now + HOUR_MS, None, key().public());
                if bind_device {
                    a = a.for_device(&w.people["box"].device.public_key_bytes());
                }
                w.push(
                    "adm",
                    SessionEvent::JoinApproved {
                        subject: invite_subject(slots[slot as usize % 3]),
                        admission: Some(a),
                        redeem_key: None,
                    },
                );
            }
            Op::Redeem {
                who,
                slot,
                holds,
                back,
            } => {
                let who = REDEEMERS[who as usize % 4];
                let id = slots[slot as usize % 3];
                let k = if holds {
                    key()
                } else {
                    RedeemKey::from_seed(&[9; 32]).unwrap()
                };
                let proof = k.prove(w.wid, id, who, w.people[who].device_id);
                let lamport = back
                    .then(|| {
                        w.cfg
                            .iter()
                            .filter(|e| matches!(&e.payload, SessionEvent::JoinApproved { subject, .. } if *subject == invite_subject(id)))
                            .map(|e| e.lamport + 1)
                            .max()
                    })
                    .flatten();
                redeem_raw(w, who, id, proof, |_| {}, lamport);
            }
            Op::Close { by, slot, name } => {
                let by = CLOSERS[by as usize % 3];
                let id = slots[slot as usize % 3];
                let redemptions: Vec<Uuid> = w
                    .cfg
                    .iter()
                    .filter(|e| matches!(&e.payload, SessionEvent::MemberAdded { invite: Some(i), .. } if *i == id))
                    .map(|e| e.event_id)
                    .collect();
                let redeemed = match name % 3 {
                    0 => None,
                    1 => redemptions.first().copied(),
                    _ => redemptions.last().copied(),
                };
                w.push_bare(
                    by,
                    SessionEvent::AdmissionClosed {
                        invite: id,
                        redeemed,
                    },
                );
            }
            Op::Cut {
                target,
                kind,
                honest,
            } => {
                let t = CUTTABLE[target as usize % 4];
                let wm = if honest {
                    honest_watermark(w, t)
                } else {
                    vec![]
                };
                let e = match kind % 3 {
                    0 => SessionEvent::MemberRemoved {
                        member_id: t.into(),
                        watermark: wm,
                    },
                    1 => SessionEvent::MemberRoleChanged {
                        change: MemberRoleChange {
                            member_id: t.into(),
                            old_role: WorkspaceRole::Admin,
                            new_role: WorkspaceRole::Viewer,
                        },
                        watermark: wm,
                    },
                    _ => SessionEvent::DeviceRevoked {
                        device_id: w.people[t].device_id,
                        watermark: wm,
                    },
                };
                w.push("owner", e);
            }
            Op::Equivocate { pick } => {
                // A twin of a redemption or a closure (same id, same signer,
                // other content).
                let targets: Vec<SessionEventEnvelope> = w
                    .cfg
                    .iter()
                    .filter(|e| {
                        matches!(
                            e.payload,
                            SessionEvent::MemberAdded {
                                invite: Some(_),
                                ..
                            } | SessionEvent::AdmissionClosed { .. }
                        )
                    })
                    .cloned()
                    .collect();
                if targets.is_empty() {
                    continue;
                }
                let orig = targets[pick as usize % targets.len()].clone();
                let who = REDEEMERS
                    .iter()
                    .chain(CLOSERS.iter())
                    .find(|n| orig.signer_device_id == Some(w.people[**n].device_id))
                    .copied()
                    .unwrap();
                let payload = match &orig.payload {
                    SessionEvent::AdmissionClosed { invite, redeemed } => {
                        SessionEvent::AdmissionClosed {
                            invite: *invite,
                            redeemed: if redeemed.is_some() {
                                None
                            } else {
                                Some(Uuid::new_v4())
                            },
                        }
                    }
                    SessionEvent::MemberAdded {
                        member,
                        invite,
                        proof,
                    } => {
                        let mut m = member.clone();
                        m.actor.display_name.push('!');
                        SessionEvent::MemberAdded {
                            member: m,
                            invite: *invite,
                            proof: proof.clone(),
                        }
                    }
                    _ => unreachable!(),
                };
                let mut twin = w.draft(who, None, payload, None);
                twin.event_id = orig.event_id;
                twin.lamport = orig.lamport;
                twin.actor_stamp.as_mut().unwrap().prev =
                    orig.actor_stamp.as_ref().and_then(|s| s.prev);
                w.finish(&mut twin, who, None);
                w.push_env(None, twin);
            }
        }
    }
}

fn shuffled(log: &[SessionEventEnvelope], seed: u64) -> Vec<SessionEventEnvelope> {
    let mut v = log.to_vec();
    let mut s = seed | 1;
    for i in (1..v.len()).rev() {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        v.swap(i, (s % (i as u64 + 1)) as usize);
    }
    v
}

proptest! {
    #[test]
    fn prop_admission_fold_is_order_independent(ops in prop::collection::vec(arb_op(), 0..16), seed in any::<u64>()) {
        let mut w = world();
        let slots = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
        play(&mut w, &slots, &ops);
        let base = verdicts_of(&w.cfg);
        let members = members_of(&w.cfg);
        let other = shuffled(&w.cfg, seed);
        prop_assert_eq!(&base, &verdicts_of(&other));
        prop_assert_eq!(&members, &members_of(&other));
        // Whatever was played: no allowed redemption stands against an
        // allowed closure of its invite that doesn't name it, at most one
        // member per admission, and only a holder of the code (a proof
        // under the key) is admitted through one.
        let allowed = |id: &Uuid| base.get(id) == Some(&Verdict::Allowed);
        let config = w.config();
        let mut per: BTreeMap<Uuid, Vec<String>> = BTreeMap::new();
        for e in w.cfg.iter().filter(|e| allowed(&e.event_id)) {
            let SessionEvent::MemberAdded { member, invite: Some(id), proof } = &e.payload else { continue };
            // A self-add through an issued box admission (an existing member
            // re-adding itself while naming an id nobody issued is not one).
            let issued = ligus_core::admission::admission_of(&config, *id).is_some();
            if !slots.contains(id) || !issued || e.actor_stamp.as_ref().map(|s| s.actor.id.as_str()) != Some(member.id.as_str()) {
                continue;
            }
            prop_assert!(member.actor.kind == ActorKind::Service, "{} joined a box admission as {:?}", member.id, member.actor.kind);
            let dev = e.signer_device_id.unwrap();
            prop_assert!(proof.as_ref().is_some_and(|p| ligus_core::crypto::verify(&key().public(), &redemption_message(w.wid, *id, &member.id, dev), p).is_ok()));
            per.entry(*id).or_default().push(member.id.clone());
            for c in w.cfg.iter().filter(|c| allowed(&c.event_id)) {
                if let SessionEvent::AdmissionClosed { invite, redeemed } = &c.payload {
                    prop_assert!(!(invite == id && *redeemed != Some(e.event_id)), "a closure of {} doesn't name {}", id, e.event_id);
                }
            }
        }
        for (id, who) in per {
            prop_assert!(who.len() <= 1, "{} admitted {:?}", id, who);
        }
    }
}

/// Regression (property minimal case): a member's re-add naming an invite
/// nobody had issued yet spends it, so in the first pass the issue, the
/// box's redemption and the closure naming nothing are all refused. A
/// dishonest removal of that member then denies the re-add: the issue, the
/// redemption and the closure all stand. The closures must be read from the
/// fold the cuts leave, not skipped because the first pass had none, or the
/// redemption stands against an allowed closure that doesn't name it.
#[test]
fn a_closure_a_cut_lets_stand_denies_what_it_doesnt_name() {
    let mut w = world();
    let slots = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
    play(
        &mut w,
        &slots,
        &[
            Op::Redeem {
                who: 2,
                slot: 1,
                holds: false,
                back: false,
            },
            Op::Issue {
                slot: 1,
                bind_device: false,
            },
            Op::Redeem {
                who: 0,
                slot: 1,
                holds: true,
                back: false,
            },
            Op::Redeem {
                who: 0,
                slot: 1,
                holds: false,
                back: false,
            },
            Op::Close {
                by: 0,
                slot: 1,
                name: 0,
            },
            Op::Cut {
                target: 1,
                kind: 0,
                honest: false,
            },
        ],
    );
    let v = verdicts_of(&w.cfg);
    let of = |f: &dyn Fn(&SessionEvent) -> bool| {
        w.cfg
            .iter()
            .filter(|e| f(&e.payload))
            .map(|e| v[&e.event_id])
            .collect::<Vec<_>>()
    };
    assert_eq!(
        of(&|p| matches!(p, SessionEvent::AdmissionClosed { .. })),
        vec![Verdict::Allowed],
        "the closure stands once the re-add is cut"
    );
    assert!(
        of(&|p| matches!(p, SessionEvent::MemberAdded { member, invite: Some(i), .. } if *i == slots[1] && member.id.starts_with("box")))
            .iter()
            .all(|x| *x == Verdict::Denied),
        "the closure names no redemption"
    );
    assert!(!w.roster().members.contains_key("box"));
    for seed in 0..64 {
        let other = shuffled(&w.cfg, seed);
        assert_eq!(v, verdicts_of(&other));
        assert_eq!(members_of(&w.cfg), members_of(&other));
    }
}

/// The generator reaches allowed redemptions, closures and cuts.
#[test]
fn the_generator_reaches_redemptions_closures_and_cuts() {
    let mut w = world();
    let slots = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
    play(
        &mut w,
        &slots,
        &[
            Op::Issue {
                slot: 0,
                bind_device: false,
            },
            Op::Redeem {
                who: 0,
                slot: 0,
                holds: true,
                back: false,
            },
            Op::Close {
                by: 0,
                slot: 0,
                name: 1,
            },
            Op::Issue {
                slot: 1,
                bind_device: true,
            },
            Op::Redeem {
                who: 1,
                slot: 1,
                holds: true,
                back: true,
            },
            Op::Cut {
                target: 1,
                kind: 0,
                honest: true,
            },
            Op::Equivocate { pick: 2 },
        ],
    );
    let v = verdicts_of(&w.cfg);
    let allowed = |f: &dyn Fn(&SessionEvent) -> bool| {
        w.cfg
            .iter()
            .any(|e| f(&e.payload) && v.get(&e.event_id) == Some(&Verdict::Allowed))
    };
    assert!(allowed(
        &|p| matches!(p, SessionEvent::MemberAdded { invite: Some(i), .. } if *i == slots[0])
    ));
    assert!(allowed(&|p| matches!(
        p,
        SessionEvent::AdmissionClosed { .. }
    )));
    assert!(allowed(&|p| matches!(
        p,
        SessionEvent::MemberRemoved { .. }
    )));
    assert!(w.roster().members.contains_key("box") && !w.roster().members.contains_key("box2"));
    assert_order_independent(&w);
}

/// Integration with phone revocation: who may revoke a box's device. An
/// Admin above it (with presence) may; a Contributor, a peer box, the box
/// itself (it never carries presence) and an Admin's phone may not. A box
/// never revokes a person's phone.
#[test]
fn who_may_revoke_a_boxs_device() {
    let mut w = world();
    let id = Uuid::new_v4();
    issue(
        &mut w,
        id,
        Admission::service(now_ms() + HOUR_MS, None, key().public()),
    );
    let r = redeem(&mut w, "box", id);
    assert_eq!(w.verdict(r), Verdict::Allowed);
    let id2 = Uuid::new_v4();
    issue(
        &mut w,
        id2,
        Admission::service(now_ms() + HOUR_MS, None, key().public()),
    );
    let r2 = redeem(&mut w, "box2", id2);
    assert_eq!(w.verdict(r2), Verdict::Allowed);
    let dev = w.people["box"].device_id;
    let revoke = |w: &SignedWorld, d: Uuid| SessionEvent::DeviceRevoked {
        device_id: d,
        watermark: honest_watermark(w, "box"),
    };
    let e = revoke(&w, dev);
    let by_mara = w.push("mara", e);
    assert_eq!(w.verdict(by_mara), Verdict::Denied, "a Contributor");
    let e = revoke(&w, dev);
    let by_peer = w.push_bare("box2", e);
    assert_eq!(w.verdict(by_peer), Verdict::Denied, "a peer box");
    let e = revoke(&w, dev);
    let by_itself = w.push_bare("box", e);
    assert_eq!(
        w.verdict(by_itself),
        Verdict::Denied,
        "the box itself carries no presence"
    );
    let phone = Phone::new();
    let cert = w.phone_certificate("adm", &phone);
    w.push(
        "adm",
        SessionEvent::DeviceCertificateAdded { certificate: cert },
    );
    let e = revoke(&w, dev);
    let by_phone = w.phone_write("adm", &phone, None, e);
    assert_eq!(
        w.verdict(by_phone),
        Verdict::Denied,
        "an Admin's phone revokes only itself"
    );
    // A box never revokes a person's phone.
    let e = SessionEvent::DeviceRevoked {
        device_id: phone.device_id,
        watermark: vec![],
    };
    let box_on_phone = w.push_bare("box", e);
    assert_eq!(w.verdict(box_on_phone), Verdict::Denied);
    assert!(!w.roster().devices[&phone.device_id].revoked);
    let e = revoke(&w, dev);
    let by_admin = w.push("adm", e);
    assert_eq!(w.verdict(by_admin), Verdict::Allowed);
    assert!(w.roster().devices[&dev].revoked);
    assert_order_independent(&w);
}
