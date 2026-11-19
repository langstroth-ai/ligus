//! Secure file attachments (issue #118): the fold rules, prove-then-fix.
//! Every case is signed by real devices in a rules-v2 workspace.

use std::collections::BTreeSet;

use hive_core::attachment::{AttachmentPolicy, AttachmentRef};
use hive_core::authz::votes::diff_digest;
use hive_core::crypto::sign_envelope;
use hive_core::testkit::{Phone, SignedWorld};
use hive_core::watermark::heads_of;
use hive_core::{
    workspace_config_session_id, ActionProposal, ChatMessage, MessageRole, ProposalApproval, ProposalKind, ProposalStatus, SessionEvent, Timestamp,
    Verdict, WorkspaceRole,
};
use uuid::Uuid;

fn world() -> SignedWorld {
    let mut w = SignedWorld::genesis(&["owner", "ada", "mara", "vic", "bea"], None);
    w.admit("owner", "ada", WorkspaceRole::Admin);
    w.admit("owner", "mara", WorkspaceRole::Contributor);
    w.admit("owner", "vic", WorkspaceRole::Viewer);
    w
}

fn on(w: &mut SignedWorld) -> Uuid {
    w.push("ada", SessionEvent::AttachmentPolicySet { policy: AttachmentPolicy::recommended() })
}

fn file(name: &str, sha: &str, size: u64) -> AttachmentRef {
    AttachmentRef {
        id: Uuid::new_v4(),
        // A fresh relay blob per file (the relay mints one per upload).
        blob_id: format!("{:b>43}", Uuid::new_v4().simple()),
        sha256: sha.into(),
        size,
        content_type: "application/octet-stream".into(),
        name: name.into(),
        message_id: None,
        proposal_id: None,
    }
}

/// `a` under the id bound to `who`'s device and its blob (what an honest
/// sharer's device signs).
fn by(w: &SignedWorld, who: &str, mut a: AttachmentRef) -> AttachmentRef {
    a.id = hive_core::attachment::share_id(w.people[who].device_id, &a.blob_id);
    a
}

fn share(a: &AttachmentRef) -> SessionEvent {
    SessionEvent::AttachmentShared { attachment: a.clone() }
}

const H1: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const H2: &str = "2222222222222222222222222222222222222222222222222222222222222222";

fn honest_watermark(w: &SignedWorld, who: &str) -> Vec<hive_core::watermark::Watermark> {
    let signers: BTreeSet<Uuid> = [w.people[who].device_id].into_iter().collect();
    let mut out = heads_of(&w.cfg, workspace_config_session_id(w.wid), &signers);
    for (id, log) in &w.chats {
        out.extend(heads_of(log, *id, &signers));
    }
    out
}

#[test]
fn the_policy_is_an_admins_with_presence_and_off_by_default() {
    let mut w = world();
    assert!(!w.roster().attachments.policy.enabled, "attachments must start off");
    let chat = w.chat("owner", "c");
    let early = w.say(chat, "mara", share(&by(&w, "mara", file("a.pdf", H1, 10))));
    assert_eq!(w.verdict(early), Verdict::Denied, "a share before any policy folds");
    let p = SessionEvent::AttachmentPolicySet { policy: AttachmentPolicy::recommended() };
    let by_contributor = w.push("mara", p.clone());
    let bare = w.push_bare("ada", p.clone());
    let on_chat = w.say(chat, "ada", p.clone());
    let bad = w.push("ada", SessionEvent::AttachmentPolicySet { policy: AttachmentPolicy { max_bytes: 0, ..AttachmentPolicy::recommended() } });
    let ok = w.push("ada", p);
    assert_eq!(
        [w.verdict(by_contributor), w.verdict(bare), w.verdict(on_chat), w.verdict(bad), w.verdict(ok)],
        [Verdict::Denied, Verdict::Denied, Verdict::Denied, Verdict::Denied, Verdict::Allowed]
    );
    assert!(w.roster().attachments.policy.enabled);
    // The early share stays refused: it was judged at its own position.
    assert_eq!(w.verdict(early), Verdict::Denied);
}

#[test]
fn who_may_share_and_what() {
    let mut w = world();
    on(&mut w);
    let chat = w.chat("owner", "c");
    let ok = w.say(chat, "mara", share(&by(&w, "mara", file("report.pdf", H1, 10))));
    let viewer = w.say(chat, "vic", share(&by(&w, "vic", file("v.pdf", H1, 10))));
    let outsider = w.say(chat, "bea", share(&by(&w, "bea", file("b.pdf", H1, 10))));
    let exe = w.say(chat, "mara", share(&by(&w, "mara", file("tool.exe", H1, 10))));
    let script = w.say(chat, "mara", share(&by(&w, "mara", file("run.sh", H1, 10))));
    let big = w.say(chat, "mara", share(&by(&w, "mara", file("big.pdf", H1, AttachmentPolicy::recommended().max_bytes + 1))));
    let dirty = w.say(chat, "mara", share(&by(&w, "mara", file("../../etc/passwd", H1, 10))));
    let bidi = w.say(chat, "mara", share(&by(&w, "mara", file("x\u{202E}fdp.exe", H1, 10))));
    let bad_hash = w.say(chat, "mara", share(&by(&w, "mara", file("a.pdf", "nothex", 10))));
    let mut no_blob = file("a.pdf", H1, 10);
    no_blob.blob_id = "../x".into();
    let bad_blob = w.say(chat, "mara", share(&no_blob));
    let on_config = w.push_bare("mara", share(&file("c.pdf", H1, 10)));
    assert_eq!(w.verdict(ok), Verdict::Allowed);
    for (what, id) in [
        ("viewer", viewer),
        ("outsider", outsider),
        ("exe", exe),
        ("script", script),
        ("too big", big),
        ("traversal name", dirty),
        ("bidi name", bidi),
        ("bad hash", bad_hash),
        ("bad blob id", bad_blob),
        ("config log", on_config),
    ] {
        assert_eq!(w.verdict(id), Verdict::Denied, "{what}");
    }
    let s = w.state(chat);
    assert_eq!(s.attachments.len(), 1);
    let a = s.attachments.values().next().unwrap();
    assert_eq!((a.author.as_str(), a.signer_device), ("mara", Some(w.people["mara"].device_id)));
}

#[test]
fn one_id_one_file_and_equivocation_drops_both() {
    let mut w = world();
    on(&mut w);
    let chat = w.chat("owner", "c");
    let a = by(&w, "mara", file("a.pdf", H1, 10));
    let first = w.say(chat, "mara", share(&a));
    // Same attachment id, other content: refused (first wins).
    let second = w.say(chat, "ada", share(&AttachmentRef { sha256: H2.into(), ..a.clone() }));
    assert_eq!((w.verdict(first), w.verdict(second)), (Verdict::Allowed, Verdict::Denied));
    assert_eq!(w.state(chat).attachments[&a.id].attachment.sha256, H1);
    // Two signed envelopes under one event id with different files: neither folds.
    let b = by(&w, "mara", file("b.pdf", H1, 10));
    let e1 = w.draft("mara", Some(chat), share(&b), None);
    let mut e2 = e1.clone();
    e2.payload = share(&AttachmentRef { sha256: H2.into(), ..b.clone() });
    e2.clear_raw();
    sign_envelope(&mut e2, w.people["mara"].device_id, &w.people["mara"].device);
    let id = w.push_env(Some(chat), e1);
    w.push_env(Some(chat), e2);
    assert_eq!(w.verdict(id), Verdict::Denied);
    assert!(!w.state(chat).attachments.contains_key(&b.id));
}

#[test]
fn the_blocklist_is_an_admins_and_bites_at_its_position() {
    let mut w = world();
    on(&mut w);
    let chat = w.chat("owner", "c");
    let before = w.say(chat, "mara", share(&by(&w, "mara", file("a.pdf", H1, 10))));
    let by_contributor = w.push_bare("mara", SessionEvent::AttachmentHashBlocked { sha256: H1.into() });
    let bad = w.push_bare("ada", SessionEvent::AttachmentHashBlocked { sha256: "XYZ".into() });
    // Blocking only narrows: no presence needed.
    let block = w.push_bare("ada", SessionEvent::AttachmentHashBlocked { sha256: H1.into() });
    assert_eq!(
        [w.verdict(by_contributor), w.verdict(bad), w.verdict(block)],
        [Verdict::Denied, Verdict::Denied, Verdict::Allowed]
    );
    assert!(w.roster().attachments.blocked.contains(H1));
    let after = w.say(chat, "mara", share(&by(&w, "mara", file("b.pdf", H1, 10))));
    assert_eq!((w.verdict(before), w.verdict(after)), (Verdict::Allowed, Verdict::Denied));
    // Lifting a block widens the rules: Admin and presence.
    let lift_bare = w.push_bare("ada", SessionEvent::AttachmentHashUnblocked { sha256: H1.into() });
    let lift = w.push("ada", SessionEvent::AttachmentHashUnblocked { sha256: H1.into() });
    assert_eq!((w.verdict(lift_bare), w.verdict(lift)), (Verdict::Denied, Verdict::Allowed));
    let again = w.say(chat, "mara", share(&by(&w, "mara", file("c.pdf", H1, 10))));
    assert_eq!(w.verdict(again), Verdict::Allowed);
}

#[test]
fn a_revoked_device_and_a_removed_member_share_nothing_past_their_watermark() {
    let mut w = world();
    on(&mut w);
    let chat = w.chat("owner", "c");
    let kept = w.say(chat, "mara", share(&by(&w, "mara", file("kept.pdf", H1, 10))));
    let wm = honest_watermark(&w, "mara");
    w.push("ada", SessionEvent::DeviceRevoked { device_id: w.people["mara"].device_id, watermark: wm });
    let after_revoke = w.say(chat, "mara", share(&by(&w, "mara", file("late.pdf", H1, 10))));
    // A backdated share from the revoked device (an old frontier): never folds.
    let mut env = w.draft("mara", Some(chat), share(&by(&w, "mara", file("backdated.pdf", H1, 10))), w.cfg.get(3).map(|e| hive_core::CfgFrontier { lamport: e.lamport, event_id: e.event_id }));
    env.clear_raw();
    sign_envelope(&mut env, w.people["mara"].device_id, &w.people["mara"].device);
    let backdated = w.push_env(Some(chat), env);
    assert_eq!(w.verdict(kept), Verdict::Allowed, "covered by the watermark");
    assert_eq!((w.verdict(after_revoke), w.verdict(backdated)), (Verdict::Denied, Verdict::Denied));

    let mut w = world();
    on(&mut w);
    let chat = w.chat("owner", "c");
    let kept = w.say(chat, "mara", share(&by(&w, "mara", file("kept.pdf", H1, 10))));
    let wm = honest_watermark(&w, "mara");
    w.push("ada", SessionEvent::MemberRemoved { member_id: "mara".into(), watermark: wm });
    let after_removal = w.say(chat, "mara", share(&by(&w, "mara", file("late.pdf", H1, 10))));
    assert_eq!((w.verdict(kept), w.verdict(after_removal)), (Verdict::Allowed, Verdict::Denied));
    // With an empty watermark nothing of theirs is vouched for.
    let mut w2 = world();
    on(&mut w2);
    let chat = w2.chat("owner", "c");
    let uncovered = w2.say(chat, "mara", share(&by(&w2, "mara", file("x.pdf", H1, 10))));
    w2.push("ada", SessionEvent::MemberRemoved { member_id: "mara".into(), watermark: vec![] });
    assert_eq!(w2.verdict(uncovered), Verdict::Denied);
}

#[test]
fn deletion_is_the_sharers_or_an_admins() {
    let mut w = world();
    on(&mut w);
    w.admit("owner", "bea", WorkspaceRole::Contributor);
    let chat = w.chat("owner", "c");
    let a = by(&w, "mara", file("a.pdf", H1, 10));
    let b = by(&w, "mara", file("b.pdf", H1, 10));
    w.say(chat, "mara", share(&a));
    w.say(chat, "mara", share(&b));
    let by_peer = w.say(chat, "bea", SessionEvent::AttachmentDeleted { attachment_id: a.id });
    let by_author = w.say(chat, "mara", SessionEvent::AttachmentDeleted { attachment_id: a.id });
    let twice = w.say(chat, "mara", SessionEvent::AttachmentDeleted { attachment_id: a.id });
    let by_admin = w.say(chat, "ada", SessionEvent::AttachmentDeleted { attachment_id: b.id });
    let unknown = w.say(chat, "ada", SessionEvent::AttachmentDeleted { attachment_id: Uuid::new_v4() });
    assert_eq!(
        [w.verdict(by_peer), w.verdict(by_author), w.verdict(twice), w.verdict(by_admin), w.verdict(unknown)],
        [Verdict::Denied, Verdict::Allowed, Verdict::Denied, Verdict::Allowed, Verdict::Denied]
    );
    let s = w.state(chat);
    assert!(s.attachments[&a.id].deleted && s.attachments[&b.id].deleted);
    // A deleted id is never reused.
    let reuse = w.say(chat, "mara", share(&a));
    assert_eq!(w.verdict(reuse), Verdict::Denied);
}

#[test]
fn a_share_names_only_its_sharers_own_message() {
    let mut w = world();
    on(&mut w);
    let chat = w.chat("owner", "c");
    let mut m = ChatMessage::new(MessageRole::User, "ada", "here");
    m.created_at = Timestamp::epoch();
    let mid = m.id;
    w.say(chat, "ada", SessionEvent::MessageAppended { message: m });
    let mut a = by(&w, "mara", file("a.pdf", H1, 10));
    a.message_id = Some(mid);
    let theirs = w.say(chat, "mara", share(&a));
    let mut b = by(&w, "ada", file("b.pdf", H1, 10));
    b.message_id = Some(mid);
    let own = w.say(chat, "ada", share(&b));
    assert_eq!((w.verdict(theirs), w.verdict(own)), (Verdict::Denied, Verdict::Allowed));
}

#[test]
fn runtimes_phones_and_services_dont_share_by_default() {
    let mut w = world();
    on(&mut w);
    let chat = w.chat("owner", "c");
    // The chat's runtime (a host-signed effect) can't share.
    let mut env = w.draft("mara", Some(chat), share(&by(&w, "mara", file("a.pdf", H1, 10))), None);
    env.actor_stamp.as_mut().unwrap().runtime_effect = true;
    env.clear_raw();
    sign_envelope(&mut env, w.people["mara"].device_id, &w.people["mara"].device);
    let by_runtime = w.push_env(Some(chat), env);
    assert_eq!(w.verdict(by_runtime), Verdict::Denied);
    // A phone's key can't share (iOS opens files, it doesn't share them yet).
    let phone = Phone::new();
    let cert = w.phone_certificate("mara", &phone);
    w.push("mara", SessionEvent::DeviceCertificateAdded { certificate: cert });
    let by_phone = w.phone_write("mara", &phone, Some(chat), share(&file("p.pdf", H1, 10)));
    assert_eq!(w.verdict(by_phone), Verdict::Denied);
}

/// mara's host files `agent`'s offer of `a` (under the id bound to her
/// device and the proposal); returns the proposal, the event and the bound
/// reference.
fn agent_offer(w: &mut SignedWorld, chat: Uuid, agent: Uuid, a: &AttachmentRef) -> (Uuid, Uuid, AttachmentRef) {
    let mut p = ActionProposal::new("Share report", ProposalKind::Decision, agent.to_string());
    p.created_at = Timestamp::epoch();
    p.producing_agent_id = Some(agent);
    p.produced_on = Some(w.people["mara"].device_id);
    let mut a = a.clone();
    a.id = hive_core::attachment::offer_id(w.people["mara"].device_id, p.id);
    p.attachment = Some(Box::new(a.clone()));
    let pid = p.id;
    let ev = w.say(chat, "mara", SessionEvent::ProposalUpserted { proposal: p });
    (pid, ev, a)
}

#[test]
fn an_agents_file_is_a_proposal_a_person_approves() {
    let mut w = world();
    on(&mut w);
    let chat = w.chat("owner", "c");
    let agent = Uuid::new_v4();
    let mut offered = file("report.pdf", H1, 10);
    offered.blob_id = String::new();
    // Agents off (the recommended policy): the offer doesn't fold.
    let (_, refused, _) = agent_offer(&mut w, chat, agent, &offered);
    assert_eq!(w.verdict(refused), Verdict::Denied);
    w.push("ada", SessionEvent::AttachmentPolicySet { policy: AttachmentPolicy { agents: true, ..AttachmentPolicy::recommended() } });
    // A blocked type stays blocked for agents too.
    let mut exe = offered.clone();
    exe.name = "x.exe".into();
    let (_, exe_offer, _) = agent_offer(&mut w, chat, agent, &exe);
    assert_eq!(w.verdict(exe_offer), Verdict::Denied);
    // An offer whose file id isn't bound to its host and proposal: refused.
    let mut p = ActionProposal::new("Share report", ProposalKind::Decision, agent.to_string());
    p.created_at = Timestamp::epoch();
    p.producing_agent_id = Some(agent);
    p.produced_on = Some(w.people["mara"].device_id);
    p.attachment = Some(Box::new(offered.clone()));
    let unbound = w.say(chat, "mara", SessionEvent::ProposalUpserted { proposal: p });
    assert_eq!(w.verdict(unbound), Verdict::Denied);
    let (pid, offer, offered) = agent_offer(&mut w, chat, agent, &offered);
    assert_eq!(w.verdict(offer), Verdict::Allowed);
    let mut shared = offered.clone();
    shared.blob_id = "c".repeat(43);
    shared.proposal_id = Some(pid);
    // Before a person approves: refused.
    let early = w.say(chat, "mara", share(&shared));
    assert_eq!(w.verdict(early), Verdict::Denied);
    let d = diff_digest(w.state(chat).proposals.iter().find(|p| p.id == pid).unwrap());
    let vote = ProposalApproval { actor_id: "ada".into(), role: WorkspaceRole::Admin, approved: true, created_at: Timestamp::epoch(), diff_digest: Some(d) };
    w.say(chat, "ada", SessionEvent::ProposalVoteCast { proposal_id: pid, approval: vote });
    assert_eq!(w.state(chat).proposals.iter().find(|p| p.id == pid).unwrap().status, ProposalStatus::Approved);
    // Another file than the approved one: refused.
    let swapped = w.say(chat, "mara", share(&AttachmentRef { sha256: H2.into(), ..shared.clone() }));
    // Shared by a device other than the producing host: refused.
    let elsewhere = w.say(chat, "ada", share(&shared));
    let ok = w.say(chat, "mara", share(&shared));
    let twice = w.say(chat, "mara", share(&AttachmentRef { id: Uuid::new_v4(), ..shared.clone() }));
    assert_eq!(
        [w.verdict(swapped), w.verdict(elsewhere), w.verdict(ok), w.verdict(twice)],
        [Verdict::Denied, Verdict::Denied, Verdict::Allowed, Verdict::Denied]
    );
}

#[test]
fn a_snapshot_cant_plant_an_attachment() {
    // `attachments` is never serialized; a snapshot that names one is
    // decoded without it.
    let mut s = hive_core::ChatSession::new("t", Uuid::new_v4(), "x");
    let a = file("a.pdf", H1, 10);
    s.attachments.insert(
        a.id,
        hive_core::attachment::SharedAttachment {
            attachment: a.clone(),
            author: "mal".into(),
            author_kind: hive_core::ActorKind::Human,
            author_name: "mal".into(),
            author_account: None,
            signer_device: None,
            event_id: Uuid::new_v4(),
            shared_at: Timestamp::epoch(),
            deleted: false,
        },
    );
    let json = serde_json::to_string(&s).unwrap();
    assert!(!json.contains("attachments"), "{json}");
    let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
    v["attachments"] = serde_json::json!({ a.id.to_string(): { "attachment": a.id } });
    let back: hive_core::ChatSession = serde_json::from_value(v).unwrap();
    assert!(back.attachments.is_empty());
}

#[test]
fn proposal_digest_is_unchanged_without_a_file() {
    // Proposals without a file keep the digest they always had: the eight
    // length-prefixed parts, computed here independently.
    use sha2::{Digest, Sha256};
    let mut p = ActionProposal::new("t", ProposalKind::Decision, "a");
    p.id = Uuid::from_u128(7);
    p.created_at = Timestamp::epoch();
    let mut h = Sha256::new();
    h.update(hive_core::authz::votes::PROPOSAL_DIGEST_TAG);
    for part in [&b"decision"[..], b"", b"[]", b"", b"", p.id.as_bytes(), b"", b""] {
        h.update((part.len() as u32).to_le_bytes());
        h.update(part);
    }
    let want: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(diff_digest(&p), want);
    let mut q = p.clone();
    let mut a = file("a.pdf", H1, 10);
    a.blob_id = String::new();
    q.attachment = Some(Box::new(a));
    assert_ne!(diff_digest(&p), diff_digest(&q), "the offered file is bound into the vote");
}

#[test]
fn an_offer_names_its_own_signing_host_and_an_agent() {
    let mut w = world();
    w.push("ada", SessionEvent::AttachmentPolicySet { policy: AttachmentPolicy { agents: true, ..AttachmentPolicy::recommended() } });
    let chat = w.chat("owner", "c");
    let agent = Uuid::new_v4();
    let mut offered = file("secret.pdf", H1, 10);
    offered.blob_id = String::new();
    // Mallory (ada here) files an offer naming mara's device as its host.
    let mut p = ActionProposal::new("Share", ProposalKind::Decision, agent.to_string());
    p.created_at = Timestamp::epoch();
    p.producing_agent_id = Some(agent);
    p.produced_on = Some(w.people["mara"].device_id);
    p.attachment = Some(Box::new(offered.clone()));
    let forged = w.say(chat, "ada", SessionEvent::ProposalUpserted { proposal: p.clone() });
    // A human's own "offer" (no producing agent) isn't an offer at all.
    let mut q = ActionProposal::new("Share", ProposalKind::Decision, "mara");
    q.created_at = Timestamp::epoch();
    q.produced_on = Some(w.people["mara"].device_id);
    q.attachment = Some(Box::new(offered));
    let humans = w.say(chat, "mara", SessionEvent::ProposalUpserted { proposal: q });
    assert_eq!((w.verdict(forged), w.verdict(humans)), (Verdict::Denied, Verdict::Denied));
}

/// Second review: another member can't take over (squat) someone's attachment
/// id by backdating a share of their own to sort first. A member may name an
/// older log head (an offline device does), so "first share of an id wins"
/// alone lets any sharer suppress anyone's file, an approved agent offer
/// included.
#[test]
fn a_backdated_share_cant_squat_anothers_attachment_id() {
    let mut w = world();
    on(&mut w);
    w.admit("owner", "bea", WorkspaceRole::Contributor);
    let chat = w.chat("owner", "c");
    let before = w.chats[&chat].len();
    let a = by(&w, "mara", file("a.pdf", H1, 10));
    let victim = w.say(chat, "mara", share(&a));
    // bea saw mara's share and drafts her own under its id, counting from
    // the log head before it, so it sorts first.
    let squat = AttachmentRef { sha256: H2.into(), blob_id: "s".repeat(43), ..a.clone() };
    let mut env = w.draft("bea", Some(chat), share(&squat), None);
    hive_core::lamport::stamp_after(&mut env, &w.chats[&chat][..before]);
    env.clear_raw();
    sign_envelope(&mut env, w.people["bea"].device_id, &w.people["bea"].device);
    let squatter = w.push_env(Some(chat), env);
    assert_eq!(w.verdict(victim), Verdict::Allowed, "mara's own file stays hers");
    assert_eq!(w.verdict(squatter), Verdict::Denied);
    assert_eq!(w.state(chat).attachments[&a.id].author, "mara");
}

/// Second review: a forged copy (another signer's envelope under the event id
/// of a genuine share) is no equivocation: the genuine share stays. On this
/// base every two envelopes under one id drop out whoever signed them, so a
/// member can suppress any share (or any event) by forging a copy; the
/// same-signer-only rule lands with the forged-copy work
/// (fix/forged-equivocation), which this test then holds to.
#[test]
#[ignore = "needs the same-signer-only equivocation rule (fix/forged-equivocation)"]
fn a_forged_copy_of_a_share_doesnt_drop_the_genuine_one() {
    let mut w = world();
    on(&mut w);
    w.admit("owner", "bea", WorkspaceRole::Contributor);
    let chat = w.chat("owner", "c");
    let a = by(&w, "mara", file("a.pdf", H1, 10));
    let genuine = w.draft("mara", Some(chat), share(&a), None);
    let mut forged = genuine.clone();
    forged.payload = share(&AttachmentRef { sha256: H2.into(), ..a.clone() });
    forged.actor_stamp.as_mut().unwrap().actor = w.people["bea"].actor();
    forged.clear_raw();
    sign_envelope(&mut forged, w.people["bea"].device_id, &w.people["bea"].device);
    let id = w.push_env(Some(chat), genuine);
    w.push_env(Some(chat), forged);
    assert_eq!(w.verdict(id), Verdict::Allowed);
    assert_eq!(w.state(chat).attachments[&a.id].attachment.sha256, H1);
}

/// Second review: a share authored before the policy went off (or stamped
/// with a frontier from before it, which an author may name) still folds,
/// judged at its position. The fold can't do better, so every device also
/// refuses to fetch, preview, save or hand an agent a file the policy at the
/// head refuses (`attachment_blob::receivable`).
#[test]
fn a_share_from_before_a_policy_off_still_folds_at_its_position() {
    let mut w = world();
    on(&mut w);
    let chat = w.chat("owner", "c");
    let before_off = w.head();
    w.push("ada", SessionEvent::AttachmentPolicySet { policy: AttachmentPolicy::default() });
    assert!(!w.roster().attachments.policy.enabled);
    let late = w.say(chat, "mara", share(&by(&w, "mara", file("late.pdf", H1, 10))));
    let a = by(&w, "mara", file("old.pdf", H1, 10));
    let id = w.write("mara", Some(chat), share(&a), before_off, hive_core::testkit::Proof::None);
    assert_eq!((w.verdict(late), w.verdict(id)), (Verdict::Denied, Verdict::Allowed));
}
