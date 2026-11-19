//! Apply on a host, with a single applier (Phase B): a person (desktop or
//! phone) asks one named host to apply an approved proposal; exactly that
//! host applies it, once; it reports a failure back; a person withdraws a
//! request; and while a request is open nobody else applies the proposal.
//! A service box applies only on a person's request, never on its own word.

use ligus_core::authz::votes::diff_digest;
use ligus_core::testkit::{Phone, SignedWorld};
use ligus_core::workspace_repo::{ApplyOutcome, ApplyRequest};
use ligus_core::{ActionProposal, ActorKind, ProposalApproval, ProposalStatus, SessionEvent, Timestamp, Verdict, WorkspaceRole};
use uuid::Uuid;

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const SHA2: &str = "89abcdef0123456789abcdef0123456789abcdef";

fn world() -> SignedWorld {
    let mut w = SignedWorld::genesis(&["owner", "ada", "mara", "bea", "box"], None);
    w.kinds.insert("box".into(), ActorKind::Service);
    w.admit("owner", "ada", WorkspaceRole::Admin);
    w.admit("owner", "mara", WorkspaceRole::Contributor);
    w.admit("owner", "bea", WorkspaceRole::Contributor);
    w.admit("owner", "box", WorkspaceRole::Contributor);
    w
}

/// An approved file-diff proposal in a fresh chat: filed by mara, approved by ada.
fn approved(w: &mut SignedWorld) -> (Uuid, Uuid, String) {
    let chat = w.chat("owner", "c");
    let mut p = ActionProposal::file_diff("p", "mara", "diff --git a/f b/f\n+1\n".into(), vec!["f".into()]);
    p.created_at = Timestamp::epoch();
    let pid = p.id;
    w.say(chat, "mara", SessionEvent::ProposalUpserted { proposal: p });
    let d = diff_digest(&w.state(chat).proposals[0]);
    let vote = ProposalApproval { actor_id: "ada".into(), role: WorkspaceRole::Admin, approved: true, created_at: Timestamp::epoch(), diff_digest: Some(d.clone()) };
    w.say(chat, "ada", SessionEvent::ProposalVoteCast { proposal_id: pid, approval: vote });
    assert_eq!(w.state(chat).proposals[0].status, ProposalStatus::Approved);
    (chat, pid, d)
}

fn request(id: u128, pid: Uuid, d: &str, host: Uuid) -> SessionEvent {
    SessionEvent::ProposalApplyRequested {
        request: ApplyRequest {
            id: Uuid::from_u128(id),
            proposal_id: pid,
            diff_digest: d.into(),
            host_device_id: host,
            expected_base_commit: Some(SHA.into()),
            repo_id: None,
        },
    }
}

fn applied(pid: Uuid, d: &str, on: Uuid, request: Option<u128>) -> SessionEvent {
    SessionEvent::ProposalApplied {
        proposal_id: pid,
        diff_digest: d.into(),
        commit: Some(SHA2.into()),
        at: Timestamp::epoch(),
        repo_id: None,
        applied_on: Some(on),
        base_commit: Some(SHA.into()),
        applied_tree_hash: Some(SHA2.into()),
        request_id: request.map(Uuid::from_u128),
    }
}

fn failed(pid: Uuid, request: u128, reason: &str) -> SessionEvent {
    SessionEvent::ProposalApplyFailed { request_id: Uuid::from_u128(request), proposal_id: pid, reason: reason.into() }
}

fn withdrawn(pid: Uuid, request: u128) -> SessionEvent {
    SessionEvent::ProposalApplyWithdrawn { request_id: Uuid::from_u128(request), proposal_id: pid }
}

#[test]
fn only_the_named_host_applies_and_only_once() {
    let mut w = world();
    let (chat, pid, d) = approved(&mut w);
    let host = w.people["box"].device_id;
    let asked = w.say(chat, "mara", request(1, pid, &d, host));
    // Another host (a person's desktop) can't take the request.
    let ada_dev = w.people["ada"].device_id;
    let by_other = w.say(chat, "ada", applied(pid, &d, ada_dev, Some(1)));
    let done = w.say(chat, "box", applied(pid, &d, host, Some(1)));
    // A retry by the same host (it crashed after writing, say): refused.
    let again = w.say(chat, "box", applied(pid, &d, host, Some(1)));
    let v = w.verdicts();
    assert_eq!(
        [v[&asked], v[&by_other], v[&done], v[&again]],
        [Verdict::Allowed, Verdict::Denied, Verdict::Allowed, Verdict::Denied]
    );
    let s = w.state(chat);
    assert_eq!(s.proposals[0].status, ProposalStatus::Applied);
    assert_eq!(s.apply_outcomes[&Uuid::from_u128(1)], ApplyOutcome::Applied { commit: Some(SHA2.into()) });
    assert!(s.open_apply_request(pid).is_none());
}

#[test]
fn one_open_request_per_proposal_and_nobody_else_applies_meanwhile() {
    let mut w = world();
    let (chat, pid, d) = approved(&mut w);
    let host = w.people["box"].device_id;
    let ada_dev = w.people["ada"].device_id;
    let first = w.say(chat, "mara", request(1, pid, &d, host));
    // A second request (another host, or the same one) while it is open.
    let second = w.say(chat, "bea", request(2, pid, &d, ada_dev));
    // A person applying it themselves while a host is named.
    let self_apply = w.say(chat, "ada", applied(pid, &d, ada_dev, None));
    let v = w.verdicts();
    assert_eq!([v[&first], v[&second], v[&self_apply]], [Verdict::Allowed, Verdict::Denied, Verdict::Denied]);
    assert_eq!(w.state(chat).open_apply_request(pid).map(|r| r.id), Some(Uuid::from_u128(1)));
}

#[test]
fn the_named_host_reports_a_failure_and_a_person_asks_again() {
    let mut w = world();
    let (chat, pid, d) = approved(&mut w);
    let host = w.people["box"].device_id;
    let ada_dev = w.people["ada"].device_id;
    w.say(chat, "mara", request(1, pid, &d, host));
    // Only the named host reports on it.
    let forged = w.say(chat, "ada", failed(pid, 1, "nope"));
    let too_long = w.say(chat, "box", failed(pid, 1, &"x".repeat(501)));
    let wrong_proposal = w.say(chat, "box", failed(Uuid::new_v4(), 1, "x"));
    let fail = w.say(chat, "box", failed(pid, 1, "the checkout moved"));
    let fail_again = w.say(chat, "box", failed(pid, 1, "again"));
    // The failed request can't be fulfilled any more...
    let late = w.say(chat, "box", applied(pid, &d, host, Some(1)));
    // ...and a new request (another host) is allowed, then fulfilled there.
    let retry = w.say(chat, "mara", request(2, pid, &d, ada_dev));
    let done = w.say(chat, "ada", applied(pid, &d, ada_dev, Some(2)));
    let v = w.verdicts();
    assert_eq!(
        [v[&forged], v[&too_long], v[&wrong_proposal], v[&fail], v[&fail_again], v[&late], v[&retry], v[&done]],
        [
            Verdict::Denied,
            Verdict::Denied,
            Verdict::Denied,
            Verdict::Allowed,
            Verdict::Denied,
            Verdict::Denied,
            Verdict::Allowed,
            Verdict::Allowed
        ]
    );
    let s = w.state(chat);
    assert_eq!(s.apply_outcomes[&Uuid::from_u128(1)], ApplyOutcome::Failed { reason: "the checkout moved".into() });
    assert_eq!(s.proposals[0].status, ProposalStatus::Applied);
}

#[test]
fn the_requester_or_an_admin_withdraws_a_request() {
    let mut w = world();
    let (chat, pid, d) = approved(&mut w);
    let host = w.people["box"].device_id;
    w.say(chat, "mara", request(1, pid, &d, host));
    // Another Contributor can't withdraw mara's request; the box can't.
    let by_bea = w.say(chat, "bea", withdrawn(pid, 1));
    let by_box = w.say(chat, "box", withdrawn(pid, 1));
    let by_mara = w.say(chat, "mara", withdrawn(pid, 1));
    // Withdrawn: the host no longer applies it.
    let late = w.say(chat, "box", applied(pid, &d, host, Some(1)));
    let v = w.verdicts();
    assert_eq!([v[&by_bea], v[&by_box], v[&by_mara], v[&late]], [Verdict::Denied, Verdict::Denied, Verdict::Allowed, Verdict::Denied]);
    assert_eq!(w.state(chat).apply_outcomes[&Uuid::from_u128(1)], ApplyOutcome::Withdrawn);
    // An Admin withdraws someone else's request.
    w.say(chat, "mara", request(2, pid, &d, host));
    let by_admin = w.say(chat, "ada", withdrawn(pid, 2));
    assert_eq!(w.verdict(by_admin), Verdict::Allowed);
}

#[test]
fn a_service_box_applies_only_on_a_persons_request() {
    let mut w = world();
    let (chat, pid, d) = approved(&mut w);
    let host = w.people["box"].device_id;
    // On its own word: neither asking nor applying.
    let own_request = w.say(chat, "box", request(1, pid, &d, host));
    let own_apply = w.say(chat, "box", applied(pid, &d, host, None));
    let v = w.verdicts();
    assert_eq!((v[&own_request], v[&own_apply]), (Verdict::Denied, Verdict::Denied));
    assert_eq!(w.state(chat).proposals[0].status, ProposalStatus::Approved);
    // On mara's request: applied.
    w.say(chat, "mara", request(2, pid, &d, host));
    let done = w.say(chat, "box", applied(pid, &d, host, Some(2)));
    assert_eq!(w.verdict(done), Verdict::Allowed);
}

#[test]
fn a_phone_asks_and_withdraws_but_never_applies_or_reports() {
    let mut w = world();
    let phone = Phone::new();
    let cert = w.phone_certificate("mara", &phone);
    w.push("mara", SessionEvent::DeviceCertificateAdded { certificate: cert });
    let (chat, pid, d) = approved(&mut w);
    let host = w.people["box"].device_id;
    let asked = w.phone_write("mara", &phone, Some(chat), request(1, pid, &d, host));
    let withdrew = w.phone_write("mara", &phone, Some(chat), withdrawn(pid, 1));
    w.say(chat, "mara", request(2, pid, &d, phone.device_id));
    // Even named as the host, a phone never applies or reports.
    let phone_apply = w.phone_write("mara", &phone, Some(chat), applied(pid, &d, phone.device_id, Some(2)));
    let phone_fail = w.phone_write("mara", &phone, Some(chat), failed(pid, 2, "x"));
    let v = w.verdicts();
    assert_eq!(
        [v[&asked], v[&withdrew], v[&phone_apply], v[&phone_fail]],
        [Verdict::Allowed, Verdict::Allowed, Verdict::Denied, Verdict::Denied]
    );
}
