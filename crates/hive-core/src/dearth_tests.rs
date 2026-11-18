//! Dearth mode, end to end through projection: who may switch it, the
//! suspended self-enrollment, the two-approval join, verified human votes and
//! the tightened envelope. Adversarial cases included.

use uuid::Uuid;

use crate::events::SessionEvent;
use crate::testkit::project_honest as project;
use crate::guard::{human_gate_reason, DearthMode, DEFAULT_BROOD_PATHS};
use crate::identity::{ActorIdentity, ActorKind, WorkspaceMember, WorkspaceRole};
use crate::lease_projection_tests::{base, ev, member, seed};
use crate::proposals::{ActionProposal, ProposalApproval};
use crate::sandbox::{envelope_for_session, EgressPolicy};
use crate::time_util::Timestamp;

fn dearth(on: bool) -> SessionEvent {
    SessionEvent::DearthModeChanged {
        dearth: DearthMode {
            enabled: on,
            reason: "leaked invite".into(),
            brood_paths: DEFAULT_BROOD_PATHS.iter().map(|s| s.to_string()).collect(),
            allowed_mcp_servers: vec!["docs".into()],
            admitted_devices: vec![],
            set_by: "owner".into(),
            set_at: Timestamp::epoch(),
        },
    }
}

fn add(id: &str) -> SessionEvent {
    SessionEvent::MemberAdded { invite: None, proof: None, member: member(id, WorkspaceRole::Contributor) }
}

fn with_admin2() -> crate::ChatSession {
    let mut s = base();
    s.members.push(member("admin2", WorkspaceRole::Admin));
    s
}

#[test]
fn only_admins_switch_dearth_mode() {
    let s = base();
    let envs = vec![seed(&s), ev(&s, 1, "alice", None, dearth(true))];
    assert!(project(&envs).unwrap().guards.dearth_on().is_none(), "a Contributor can't open or close the entrance");
    let envs = vec![seed(&s), ev(&s, 1, "owner", None, dearth(true)), ev(&s, 2, "alice", None, dearth(false))];
    assert!(project(&envs).unwrap().guards.dearth_on().is_some(), "a Contributor can't switch it off either");
}

#[test]
fn self_enrollment_is_suspended() {
    let s = base();
    let envs = vec![
        seed(&s),
        ev(&s, 1, "owner", None, dearth(true)),
        // A stranger with the leaked invite tries to join as a Contributor.
        ev(&s, 2, "eve", None, add("eve")),
    ];
    let p = project(&envs).unwrap();
    assert!(!p.members.iter().any(|m| m.id == "eve"));
    assert!(p.guards.pending_members.is_empty());
    // Without dearth mode the same self-enroll lands when an Admin issued
    // the invite it redeems (control); key possession alone never does.
    let envs = vec![seed(&s), ev(&s, 2, "eve", None, add("eve"))];
    assert!(!project(&envs).unwrap().members.iter().any(|m| m.id == "eve"));
    let invite = uuid::Uuid::new_v4();
    let issue = SessionEvent::JoinApproved { subject: crate::authorization::invite_subject(invite), admission: None };
    let redeem = SessionEvent::MemberAdded { invite: Some(invite), proof: None, member: member("eve", WorkspaceRole::Contributor) };
    let envs = vec![seed(&s), ev(&s, 1, "owner", None, issue), ev(&s, 2, "eve", None, redeem)];
    assert!(project(&envs).unwrap().members.iter().any(|m| m.id == "eve"));
}

#[test]
fn a_new_member_needs_two_different_admins() {
    let s = with_admin2();
    let mut envs = vec![seed(&s), ev(&s, 1, "owner", None, dearth(true)), ev(&s, 2, "owner", None, add("bob"))];
    let p = project(&envs).unwrap();
    assert!(!p.members.iter().any(|m| m.id == "bob"));
    assert_eq!(p.guards.pending_members.len(), 1);
    // The adder approving their own add doesn't count.
    envs.push(ev(&s, 3, "owner", None, SessionEvent::JoinApproved { subject: "bob".into(), admission: None }));
    assert!(!project(&envs).unwrap().members.iter().any(|m| m.id == "bob"));
    // A Contributor's approval is dropped on ingest.
    envs.push(ev(&s, 4, "alice", None, SessionEvent::JoinApproved { subject: "bob".into(), admission: None }));
    assert!(!project(&envs).unwrap().members.iter().any(|m| m.id == "bob"));
    // A second Admin admits.
    envs.push(ev(&s, 5, "admin2", None, SessionEvent::JoinApproved { subject: "bob".into(), admission: None }));
    let p = project(&envs).unwrap();
    assert!(p.members.iter().any(|m| m.id == "bob"));
    assert!(p.guards.pending_members.is_empty());
}

#[test]
fn switching_off_lands_pending_members() {
    let s = with_admin2();
    let envs = vec![
        seed(&s),
        ev(&s, 1, "owner", None, dearth(true)),
        ev(&s, 2, "owner", None, add("bob")),
        ev(&s, 3, "admin2", None, dearth(false)),
    ];
    assert!(project(&envs).unwrap().members.iter().any(|m| m.id == "bob"));
}

#[test]
fn dearth_narrows_the_envelope() {
    let s = base();
    let p = project(&[seed(&s), ev(&s, 1, "owner", None, dearth(true))]).unwrap();
    let e = envelope_for_session(&p);
    assert_eq!(e.egress, EgressPolicy::AllowList { mcp_servers: vec!["docs".into()] });
    assert!(e.isolate_writes);
    assert!(envelope_for_session(&project(&[seed(&s)]).unwrap()).egress.is_open());
}

fn proposal_touching(files: &[&str]) -> ActionProposal {
    ActionProposal::file_diff("p", "", "diff".into(), files.iter().map(|f| f.to_string()).collect())
}

fn vote(pid: Uuid, voter: &str) -> SessionEvent {
    SessionEvent::ProposalVoteCast {
        proposal_id: pid,
        approval: ProposalApproval { actor_id: voter.into(), role: WorkspaceRole::Owner, approved: true, created_at: Timestamp::epoch(), diff_digest: None },
    }
}

#[test]
fn brood_changes_need_a_signed_human_vote() {
    let mut s = base();
    // An agent-kind member (e.g. a bot account) can't be the human.
    s.members.push(WorkspaceMember {
        id: "bot".into(),
        actor: ActorIdentity::new("bot", "bot", ActorKind::Agent),
        role: WorkspaceRole::Owner,
        title: String::new(),
        index: 0,
        joined_at: Timestamp::epoch(),
    });
    let brood = proposal_touching(&[".github/workflows/ci.yml"]);
    let plain = proposal_touching(&["src/lib.rs"]);
    let (bp, pp) = (brood.id, plain.id);
    let mut envs = vec![
        seed(&s),
        ev(&s, 1, "owner", None, dearth(true)),
        ev(&s, 2, "alice", None, SessionEvent::ProposalUpserted { proposal: brood.clone() }),
        ev(&s, 3, "alice", None, SessionEvent::ProposalUpserted { proposal: plain }),
    ];
    let p = project(&envs).unwrap();
    let get = |p: &crate::ChatSession, id| p.proposals.iter().find(|x| x.id == id).unwrap().clone();
    assert!(human_gate_reason(&p, &get(&p, bp)).is_some());
    assert!(human_gate_reason(&p, &get(&p, pp)).is_none(), "non-brood paths are not gated");
    // A forged vote naming a human but signed by someone else doesn't count.
    envs.push(ev(&s, 4, "victor", None, vote(bp, "owner")));
    // An agent-kind member's own vote doesn't count.
    envs.push(ev(&s, 5, "bot", None, vote(bp, "bot")));
    // A proposal snapshot carrying a fake approval doesn't count.
    let mut forged = brood.clone();
    forged.approvals.push(ProposalApproval { actor_id: "owner".into(), role: WorkspaceRole::Owner, approved: true, created_at: Timestamp::epoch(), diff_digest: None });
    envs.push(ev(&s, 6, "alice", None, SessionEvent::ProposalUpserted { proposal: forged }));
    let p = project(&envs).unwrap();
    assert!(human_gate_reason(&p, &get(&p, bp)).is_some());
    // The human's own signed vote satisfies the gate.
    envs.push(ev(&s, 7, "owner", None, vote(bp, "owner")));
    let p = project(&envs).unwrap();
    assert!(human_gate_reason(&p, &get(&p, bp)).is_none());
}
