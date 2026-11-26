//! Projection-level tests for coordinator leases: forged claims, stale epochs,
//! split brain between concurrent devices, and fencing of coordinator writes.

use uuid::Uuid;

use crate::events::{SessionEvent, SessionEventEnvelope};
use crate::identity::{ActorIdentity, ActorKind, ActorStamp, WorkspaceMember, WorkspaceRole};
use crate::lease::{
    lease_key, CoordinatorLease, LeaseClaimReason, LeaseRelease, LeaseScope, DEFAULT_LEASE_MS,
};
use crate::session::ChatSession;
use crate::testkit::project_honest as project;
use crate::time_util::Timestamp;
use crate::workflow::{WorkflowDefinition, WorkflowRun, WorkflowRunStatus};

pub(crate) fn member(id: &str, role: WorkspaceRole) -> WorkspaceMember {
    WorkspaceMember {
        id: id.into(),
        actor: ActorIdentity::new(id, id, ActorKind::Human),
        role,
        title: String::new(),
        index: 0,
        joined_at: Timestamp::epoch(),
    }
}

pub(crate) fn base() -> ChatSession {
    let mut s = ChatSession::new("t", Uuid::new_v4(), "anthropic");
    s.creator_actor_id = "owner".into();
    s.members.push(member("owner", WorkspaceRole::Owner));
    s.members.push(member("alice", WorkspaceRole::Contributor));
    s.members.push(member("victor", WorkspaceRole::Viewer));
    s
}

pub(crate) fn seed(s: &ChatSession) -> SessionEventEnvelope {
    let mut e = SessionEventEnvelope::new(
        s.id,
        s.workspace_id,
        1,
        SessionEvent::SessionSnapshot {
            session: Box::new(s.clone()),
        },
    );
    e.lamport = 0;
    stamp(e, "owner", None)
}

/// An envelope at `lamport`, authored by `author`, signed (claimed) by `device`.
pub(crate) fn ev(
    s: &ChatSession,
    lamport: u64,
    author: &str,
    device: Option<Uuid>,
    payload: SessionEvent,
) -> SessionEventEnvelope {
    let mut e = SessionEventEnvelope::new(s.id, s.workspace_id, lamport as i64, payload);
    e.lamport = lamport;
    stamp(e, author, device)
}

fn stamp(mut e: SessionEventEnvelope, author: &str, device: Option<Uuid>) -> SessionEventEnvelope {
    e.actor_stamp = Some(ActorStamp {
        actor: ActorIdentity::new(author, author, ActorKind::Human),
        recorded_at: Timestamp::epoch(),
        cfg_frontier: None,
        presence: None,
        runtime_effect: false,
        prev: None,
        sig_alg: Default::default(),
        log_head: None,
    });
    e.signer_device_id = device;
    e
}

fn claim(run: Uuid, holder: Uuid, epoch: u64) -> SessionEvent {
    SessionEvent::CoordinatorLeaseClaimed {
        lease: CoordinatorLease {
            scope: LeaseScope::WorkflowRun,
            scope_id: run.to_string(),
            holder,
            epoch,
            lease_ms: DEFAULT_LEASE_MS,
            reason: LeaseClaimReason::Initial,
            claimed_at: Timestamp::epoch(),
        },
    }
}

fn run_write(run_id: Uuid, input: &str, epoch: Option<u64>) -> SessionEvent {
    let def = WorkflowDefinition {
        id: Uuid::nil(),
        name: "wf".into(),
        description: String::new(),
        input_label: None,
        nodes: Vec::new(),
        created_at: Timestamp::epoch(),
        disabled: false,
    };
    let mut run: WorkflowRun = crate::workflow::new_run(&def, input, "alice");
    run.id = run_id;
    run.status = WorkflowRunStatus::Running;
    run.lease_epoch = epoch;
    SessionEvent::WorkflowRunUpserted { run: Box::new(run) }
}

fn holder(s: &ChatSession, run: Uuid) -> Option<(Uuid, u64)> {
    s.guards
        .leases
        .get(&lease_key(LeaseScope::WorkflowRun, &run.to_string()))
        .map(|l| (l.current.holder, l.current.epoch))
}

#[test]
fn a_claim_signed_by_another_device_is_dropped() {
    let s = base();
    let (run, a, mallory_dev) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let envs = vec![
        seed(&s),
        ev(&s, 1, "alice", Some(a), claim(run, a, 1)),
        // Forged: claims epoch 2 for device `a` but is signed by another device.
        ev(&s, 2, "alice", Some(mallory_dev), claim(run, a, 2)),
        // Forged: unsigned claim.
        ev(&s, 3, "alice", None, claim(run, mallory_dev, 3)),
    ];
    assert_eq!(holder(&project(&envs).unwrap(), run), Some((a, 1)));
}

#[test]
fn a_viewer_cannot_author_a_claim_and_a_member_outside_the_chat_roster_can_project_one() {
    let s = base();
    // Authoring: a Viewer is refused (checked against the hoisted roster).
    let (run, v) = (Uuid::new_v4(), Uuid::new_v4());
    let d = crate::authorization::evaluate(&claim(run, v, 1), "victor", WorkspaceRole::Viewer, &s);
    assert!(!d.allowed);
    // Projection: a claim by a workspace member who isn't on this chat's own
    // roster (members live on the config log) still folds when it's signed by
    // the device it names. Without this, only a chat's creator could drive runs.
    let mut chat = base();
    chat.members.retain(|m| m.id == "owner");
    let dev = Uuid::new_v4();
    let envs = vec![
        seed(&chat),
        ev(&chat, 1, "alice", Some(dev), claim(run, dev, 1)),
    ];
    assert_eq!(holder(&project(&envs).unwrap(), run), Some((dev, 1)));
}

#[test]
fn stale_epochs_are_fenced_and_replays_ignored() {
    let s = base();
    let (run, a, b) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let envs = vec![
        seed(&s),
        ev(&s, 1, "alice", Some(a), claim(run, a, 1)),
        ev(&s, 2, "owner", Some(b), claim(run, b, 2)),
        // Device a, partitioned, tries to renew at epoch 1 and then 2.
        ev(&s, 3, "alice", Some(a), claim(run, a, 1)),
        ev(&s, 4, "alice", Some(a), claim(run, a, 2)),
    ];
    let p = project(&envs).unwrap();
    assert_eq!(holder(&p, run), Some((b, 2)));
    let st = &p.guards.leases[&lease_key(LeaseScope::WorkflowRun, &run.to_string())];
    assert_eq!(
        st.fenced.len(),
        2,
        "both stale claims are recorded for the split-brain view"
    );
}

#[test]
fn concurrent_successors_converge_on_one_holder_in_any_order() {
    // Two devices both see the lease lapse and claim epoch 2 concurrently
    // (same Lamport value, so the canonical tie-break decides). Every device
    // must project the same single holder whatever order the events arrive in.
    let s = base();
    let (run, a, b, c) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    let e_seed = seed(&s);
    let e1 = ev(&s, 1, "alice", Some(a), claim(run, a, 1));
    let eb = ev(&s, 5, "owner", Some(b), claim(run, b, 2));
    let ec = ev(&s, 5, "alice", Some(c), claim(run, c, 2));
    // Each successor's write under epoch 2.
    let wb = ev(&s, 6, "owner", Some(b), run_write(run, "from-b", Some(2)));
    let wc = ev(&s, 6, "alice", Some(c), run_write(run, "from-c", Some(2)));
    let all = vec![e_seed, e1, eb, ec, wb, wc];
    let first = project(&all).unwrap();
    let winner = holder(&first, run).unwrap();
    assert_eq!(winner.1, 2);
    let expected_input = if winner.0 == b { "from-b" } else { "from-c" };
    assert_eq!(first.workflow_runs.len(), 1);
    assert_eq!(
        first.workflow_runs[0].input, expected_input,
        "only the winner's write survives"
    );
    // Every permutation converges.
    let mut perm = all.clone();
    for round in 0..12 {
        perm.rotate_left(1);
        if round % 3 == 0 {
            perm.reverse();
        }
        let p = project(&perm).unwrap();
        assert_eq!(holder(&p, run), Some(winner));
        assert_eq!(p.workflow_runs[0].input, expected_input);
    }
}

#[test]
fn writes_from_a_superseded_holder_are_fenced() {
    let s = base();
    let (run, a, b) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let envs = vec![
        seed(&s),
        ev(&s, 1, "alice", Some(a), claim(run, a, 1)),
        ev(&s, 2, "alice", Some(a), run_write(run, "a-before", Some(1))),
        ev(&s, 3, "owner", Some(b), claim(run, b, 2)),
        // The old holder kept going after b took over: fenced.
        ev(&s, 4, "alice", Some(a), run_write(run, "a-after", Some(1))),
    ];
    let p = project(&envs).unwrap();
    assert_eq!(p.workflow_runs[0].input, "a-before");
    // The new holder's write lands.
    let mut more = envs.clone();
    more.push(ev(&s, 5, "owner", Some(b), run_write(run, "b", Some(2))));
    assert_eq!(project(&more).unwrap().workflow_runs[0].input, "b");
}

#[test]
fn forged_epoch_stamps_are_dropped() {
    let s = base();
    let (run, a, m) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let envs = vec![
        seed(&s),
        // An epoch stamp with no lease at all.
        ev(&s, 1, "alice", Some(m), run_write(run, "no-lease", Some(1))),
        ev(&s, 2, "alice", Some(a), claim(run, a, 1)),
        // Current epoch, but signed by a device that does not hold it.
        ev(
            &s,
            3,
            "alice",
            Some(m),
            run_write(run, "not-holder", Some(1)),
        ),
        // A future epoch nobody claimed.
        ev(&s, 4, "alice", Some(a), run_write(run, "future", Some(9))),
        // Legacy unstamped writes (a cancel from another device) still land.
        ev(&s, 5, "alice", Some(m), run_write(run, "cancel", None)),
    ];
    let p = project(&envs).unwrap();
    assert_eq!(p.workflow_runs.len(), 1);
    assert_eq!(p.workflow_runs[0].input, "cancel");
}

#[test]
fn planned_handover_reserves_the_next_epoch() {
    let s = base();
    let (run, a, b, c) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    let release = |signer: Uuid| SessionEvent::CoordinatorLeaseReleased {
        release: LeaseRelease {
            scope: LeaseScope::WorkflowRun,
            scope_id: run.to_string(),
            holder: signer,
            epoch: 1,
            successor: Some(b),
        },
    };
    let envs = vec![
        seed(&s),
        ev(&s, 1, "alice", Some(a), claim(run, a, 1)),
        // A release signed by someone else naming themselves holder is refused
        // by the fold (not the holder) — c cannot open the door for itself.
        ev(&s, 2, "alice", Some(c), release(c)),
        ev(&s, 3, "alice", Some(a), release(a)),
        ev(&s, 4, "alice", Some(c), claim(run, c, 2)),
        ev(&s, 5, "owner", Some(b), claim(run, b, 2)),
    ];
    assert_eq!(holder(&project(&envs).unwrap(), run), Some((b, 2)));
}
