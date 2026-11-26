//! Adversarial review of the phone-signed path (Phase B): what a box that
//! drafts the event can choose, and what the fold must refuse whatever it
//! chooses.

use ligus_core::authz::votes::diff_digest;
use ligus_core::crypto::{envelope_preimage, sign_envelope_p256, SigAlg};
use ligus_core::phone_action::{check_phone_action_preimage, PhoneAction};
use ligus_core::testkit::{Phone, SignedWorld};
use ligus_core::workspace_repo::ApplyRequest;
use ligus_core::{
    ActionProposal, ActorKind, ChatMessage, MessageRole, ProposalApproval, ProposalStatus,
    SessionEvent, SessionEventEnvelope, Timestamp, Verdict, WorkspaceRole,
};
use uuid::Uuid;

struct Fixture {
    w: SignedWorld,
    phone: Phone,
    chat: Uuid,
    pid: Uuid,
    digest: String,
    host: Uuid,
}

fn fixture() -> Fixture {
    let mut w = SignedWorld::genesis(&["owner", "ada", "mara", "box", "vic"], None);
    w.kinds.insert("box".into(), ActorKind::Service);
    w.admit("owner", "ada", WorkspaceRole::Admin);
    w.admit("owner", "mara", WorkspaceRole::Contributor);
    w.admit("owner", "box", WorkspaceRole::Contributor);
    w.admit("owner", "vic", WorkspaceRole::Viewer);
    let phone = Phone::new();
    let cert = w.phone_certificate("mara", &phone);
    let enrolled = w.push(
        "mara",
        SessionEvent::DeviceCertificateAdded { certificate: cert },
    );
    assert_eq!(w.verdict(enrolled), Verdict::Allowed);
    let chat = w.chat("owner", "c");
    let mut p = ActionProposal::file_diff(
        "p",
        "ada",
        "diff --git a/f b/f\n+1\n".into(),
        vec!["f".into()],
    );
    p.created_at = Timestamp::epoch();
    let pid = p.id;
    w.say(chat, "ada", SessionEvent::ProposalUpserted { proposal: p });
    let digest = diff_digest(&w.state(chat).proposals[0]);
    let host = w.people["box"].device_id;
    Fixture {
        w,
        phone,
        chat,
        pid,
        digest,
        host,
    }
}

/// `by`'s event drafted by a box for `phone` and signed by it (no presence).
fn phone_draft(
    f: &mut Fixture,
    phone: &Phone,
    by: &str,
    chat: Option<Uuid>,
    payload: SessionEvent,
) -> SessionEventEnvelope {
    let mut env = f.w.draft(by, chat, payload, None);
    let s = env.actor_stamp.as_mut().unwrap();
    s.actor.device_id = Some(phone.device_id);
    s.sig_alg = SigAlg::P256;
    s.prev = None;
    s.presence = None;
    env.signature = None;
    env.signer_device_id = None;
    env.clear_raw();
    env
}

fn phone_signed(f: &mut Fixture, by: &str, chat: Option<Uuid>, payload: SessionEvent) -> Uuid {
    let phone = std::mem::take(&mut f.phone);
    let mut env = phone_draft(f, &phone, by, chat, payload);
    sign_envelope_p256(&mut env, phone.device_id, &phone.key);
    f.phone = phone;
    f.w.push_env(chat, env)
}

fn mara_post(f: &Fixture, id: Option<Uuid>, text: &str) -> SessionEvent {
    let mara = f.w.people["mara"].actor();
    let mut m = ChatMessage::new(MessageRole::User, mara.display_name.clone(), text);
    let mut who = mara.clone();
    who.device_id = Some(f.phone.device_id);
    m.actor_identity = Some(who);
    if let Some(id) = id {
        m.id = id;
    }
    SessionEvent::MessageAppended { message: m }
}

/// A box picks the message id of a post. The phone can't know the chat's
/// ids, so a reused id (the person's own earlier message) would make the
/// phone's post silently rewrite that message. The fold refuses it: a
/// phone posts new messages only.
#[test]
fn a_phone_post_never_rewrites_an_earlier_message() {
    let mut f = fixture();
    let chat = f.chat;
    // Mara's earlier message, from her desktop.
    let mara = f.w.people["mara"].actor();
    let mut earlier = ChatMessage::new(
        MessageRole::User,
        mara.display_name.clone(),
        "do NOT deploy on Friday",
    );
    earlier.actor_identity = Some(mara.clone());
    let earlier_id = earlier.id;
    let first = f.w.say(
        chat,
        "mara",
        SessionEvent::MessageAppended { message: earlier },
    );
    assert_eq!(f.w.verdict(first), Verdict::Allowed);
    // The box drafts her phone post "ok" under that id; the preimage check
    // passes (the id is the box's to choose) ...
    let action = PhoneAction::Post {
        chat_id: chat,
        text: "ok".into(),
    };
    let payload = mara_post(&f, Some(earlier_id), "ok");
    let phone = std::mem::take(&mut f.phone);
    let env = phone_draft(&mut f, &phone, "mara", Some(chat), payload.clone());
    f.phone = phone;
    let account = f.w.people["mara"].account_id;
    assert!(check_phone_action_preimage(
        &envelope_preimage(&env),
        None,
        f.w.wid,
        f.phone.device_id,
        account,
        &action
    )
    .is_ok());
    // ... but the fold never lets it rewrite her earlier message.
    let rewrite = phone_signed(&mut f, "mara", Some(chat), payload);
    assert_eq!(f.w.verdict(rewrite), Verdict::Denied);
    let s = f.w.state(chat);
    let m = s.messages.iter().find(|m| m.id == earlier_id).unwrap();
    assert_eq!(m.body, "do NOT deploy on Friday");
    // A fresh id posts.
    let p = mara_post(&f, None, "fresh");
    let fresh = phone_signed(&mut f, "mara", Some(chat), p);
    assert_eq!(f.w.verdict(fresh), Verdict::Allowed);
}

fn approve(f: &mut Fixture) {
    let chat = f.chat;
    let v = SessionEvent::ProposalVoteCast {
        proposal_id: f.pid,
        approval: ProposalApproval {
            actor_id: "owner".into(),
            role: WorkspaceRole::Owner,
            approved: true,
            created_at: Timestamp::epoch(),
            diff_digest: Some(f.digest.clone()),
        },
    };
    f.w.say(chat, "owner", v);
    assert_eq!(
        f.w.state(chat).proposals[0].status,
        ProposalStatus::Approved
    );
}

fn request(f: &Fixture, id: u128, host: Uuid) -> SessionEvent {
    SessionEvent::ProposalApplyRequested {
        request: ApplyRequest {
            id: Uuid::from_u128(id),
            proposal_id: f.pid,
            diff_digest: f.digest.clone(),
            host_device_id: host,
            expected_base_commit: None,
            repo_id: None,
        },
    }
}

/// A phone never applies anything (its tier refuses `ProposalApplied`), so a
/// request naming one would stay open forever and, one applier per
/// proposal, block every other apply until someone withdraws it.
#[test]
fn an_apply_request_never_names_a_phone() {
    let mut f = fixture();
    approve(&mut f);
    let chat = f.chat;
    let phone_host = f.phone.device_id;
    let r = request(&f, 1, phone_host);
    let asked = phone_signed(&mut f, "mara", Some(chat), r);
    assert_eq!(f.w.verdict(asked), Verdict::Denied);
    assert!(f.w.state(chat).open_apply_request(f.pid).is_none());
    // The box (a real host) is fine.
    let host = f.host;
    let r = request(&f, 2, host);
    let ok = phone_signed(&mut f, "mara", Some(chat), r);
    assert_eq!(f.w.verdict(ok), Verdict::Allowed);
}

/// A Viewer's phone asks for nothing; a box's vote is never a person's.
#[test]
fn a_viewer_phone_requests_nothing_and_a_box_vote_is_not_a_persons() {
    let mut f = fixture();
    approve(&mut f);
    let chat = f.chat;
    let vic_phone = Phone::new();
    let cert = f.w.phone_certificate("vic", &vic_phone);
    f.w.push(
        "vic",
        SessionEvent::DeviceCertificateAdded { certificate: cert },
    );
    let r = request(&f, 3, f.host);
    let real = std::mem::replace(&mut f.phone, vic_phone);
    let asked = phone_signed(&mut f, "vic", Some(chat), r);
    f.phone = real;
    assert_eq!(f.w.verdict(asked), Verdict::Denied);
    // The box votes as itself on a fresh proposal: it is on record as the
    // box's vote, never as a human approval.
    let mut p = ActionProposal::file_diff(
        "q",
        "ada",
        "diff --git a/g b/g\n+1\n".into(),
        vec!["g".into()],
    );
    p.created_at = Timestamp::epoch();
    let qid = p.id;
    f.w.say(chat, "ada", SessionEvent::ProposalUpserted { proposal: p });
    let d = diff_digest(
        f.w.state(chat)
            .proposals
            .iter()
            .find(|p| p.id == qid)
            .unwrap(),
    );
    let boxed = f.w.say(
        chat,
        "box",
        SessionEvent::ProposalVoteCast {
            proposal_id: qid,
            approval: ProposalApproval {
                actor_id: "box".into(),
                role: WorkspaceRole::Owner,
                approved: true,
                created_at: Timestamp::epoch(),
                diff_digest: Some(d),
            },
        },
    );
    let _ = boxed;
    let s = f.w.state(chat);
    assert!(!ligus_core::guard::has_human_approval(&s, qid));
    let q = s.proposals.iter().find(|p| p.id == qid).unwrap();
    assert!(q
        .approvals
        .iter()
        .all(|a| a.actor_id != "mara" && a.role != WorkspaceRole::Owner));
}

/// A phone and a desktop of one person count once (the later one wins).
#[test]
fn a_phone_and_a_desktop_of_one_person_count_once() {
    let mut f = fixture();
    let chat = f.chat;
    let v = |approved: bool, d: &str| SessionEvent::ProposalVoteCast {
        proposal_id: Uuid::nil(),
        approval: ProposalApproval {
            actor_id: "mara".into(),
            role: WorkspaceRole::Contributor,
            approved,
            created_at: Timestamp::epoch(),
            diff_digest: Some(d.into()),
        },
    };
    let fix = |e: SessionEvent, pid: Uuid| match e {
        SessionEvent::ProposalVoteCast { approval, .. } => SessionEvent::ProposalVoteCast {
            proposal_id: pid,
            approval,
        },
        e => e,
    };
    let pid = f.pid;
    let d = f.digest.clone();
    f.w.say(chat, "mara", fix(v(true, &d), pid));
    let e = fix(v(true, &d), pid);
    phone_signed(&mut f, "mara", Some(chat), e);
    let s = f.w.state(chat);
    assert_eq!(
        s.proposals[0]
            .approvals
            .iter()
            .filter(|a| a.actor_id == "mara")
            .count(),
        1
    );
}

/// The strict parser refuses duplicate JSON keys in every signed field
/// (another parser could read the other copy).
#[test]
fn duplicate_json_keys_are_refused() {
    let mut f = fixture();
    let chat = f.chat;
    let action = PhoneAction::Post {
        chat_id: chat,
        text: "hi".into(),
    };
    let payload = mara_post(&f, None, "hi");
    let phone = std::mem::take(&mut f.phone);
    let env = phone_draft(&mut f, &phone, "mara", Some(chat), payload);
    f.phone = phone;
    let good = envelope_preimage(&env);
    let account = f.w.people["mara"].account_id;
    assert!(
        check_phone_action_preimage(&good, None, f.w.wid, f.phone.device_id, account, &action)
            .is_ok()
    );
    // Re-split the four length-prefixed fields.
    let head = b"hive-envelope-v3\0".len() + 16 + 16 + 8 + 8 + 16;
    let mut fields: Vec<Vec<u8>> = Vec::new();
    let mut at = head;
    for _ in 0..4 {
        let n = u32::from_le_bytes(good[at..at + 4].try_into().unwrap()) as usize;
        fields.push(good[at + 4..at + 4 + n].to_vec());
        at += 4 + n;
    }
    for i in 0..4 {
        let s = String::from_utf8(fields[i].clone()).unwrap();
        // Duplicate the first key of the object (keeping its value), at the end.
        let Some(obj) = s.strip_prefix('{') else {
            continue;
        };
        let Some(colon) = obj.find("\":") else {
            continue;
        };
        let key = &obj[..colon + 1];
        let dup = format!(
            "{},{}:{}}}",
            &s[..s.len() - 1],
            key,
            if key == "\"type\"" {
                "\"messageAppended\""
            } else {
                "null"
            }
        );
        let mut out = good[..head].to_vec();
        for (j, fld) in fields.iter().enumerate() {
            let b = if j == i {
                dup.as_bytes().to_vec()
            } else {
                fld.clone()
            };
            out.extend_from_slice(&(b.len() as u32).to_le_bytes());
            out.extend_from_slice(&b);
        }
        assert!(
            check_phone_action_preimage(&out, None, f.w.wid, f.phone.device_id, account, &action)
                .is_err(),
            "field {i}: {dup}"
        );
    }
}
