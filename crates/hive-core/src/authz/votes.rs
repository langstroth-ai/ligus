//! Votes bound to their signer and to the content they approve; proposal
//! status derived from counted votes, never asserted.

use sha2::{Digest, Sha256};

use crate::proposals::{ActionProposal, ProposalStatus};

/// Domain tag of [`diff_digest`] (Phase 0 list, section 0.1).
pub const PROPOSAL_DIGEST_TAG: &[u8] = b"ligus-proposal-digest-v1\0";

/// The apply-content digest a vote binds to: SHA-256 over the tag and the
/// length-prefixed kind tag, diff, changed-files JSON, workflow JSON and
/// command JSON (Track D's structured command; empty until it exists).
/// Lowercase hex.
pub fn diff_digest(p: &ActionProposal) -> String {
    let kind = serde_json::to_value(p.kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    let diff = p.diff.as_deref().unwrap_or("").as_bytes().to_vec();
    let files = serde_json::to_vec(&p.changed_files).expect("serialize changed files");
    let workflow = match &p.workflow {
        Some(w) => serde_json::to_vec(w).expect("serialize workflow"),
        None => Vec::new(),
    };
    let command: Vec<u8> = Vec::new();
    let mut h = Sha256::new();
    h.update(PROPOSAL_DIGEST_TAG);
    for part in [kind.as_bytes(), &diff, &files, &workflow, &command] {
        h.update((part.len() as u32).to_le_bytes());
        h.update(part);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// A proposal upsert as the checked fold applies it: what would be applied
/// (patch, files, kind, workflow) and the descriptive fields come from the
/// upsert, but votes and status never do. Votes ride `ProposalVoteCast` (bound
/// to their signer); a content change clears them, since they were cast for
/// other content. Status is re-derived from the counted votes; `Applied` only
/// comes from `ProposalApplied`, and `reverted` only from `ProposalReverted`.
pub fn normalize_upsert(existing: Option<&ActionProposal>, incoming: &ActionProposal) -> ActionProposal {
    let mut p = incoming.clone();
    match existing {
        Some(old) => {
            let same_content = diff_digest(old) == diff_digest(incoming);
            p.approvals = if same_content { old.approvals.clone() } else { Vec::new() };
            p.status = if old.status == ProposalStatus::Applied { ProposalStatus::Applied } else { ProposalStatus::Open };
            p.reverted = old.reverted;
        }
        None => {
            p.approvals = Vec::new();
            p.status = ProposalStatus::Open;
            p.reverted = false;
        }
    }
    p.rederive_status();
    p
}

/// Hold a proposal to the core path zones' review bar (raise only): the
/// strictest core zone among every path it may write (its diff included, not
/// just the declared files). Authoring applies the full bar (low-bar zones may
/// lower it there); the fold only makes sure nobody filed below a core bar.
pub fn hold_to_core_bar(p: &mut ActionProposal, zones: &[crate::flow::PathZone]) {
    let touched = crate::guard::touched_paths(p);
    let Some((floor, approvals, crate::flow::ZoneKind::Core)) = crate::flow::zones::review_bar(zones, &touched) else {
        return;
    };
    let floor = floor.unwrap_or(crate::flow::zones::CORE_DEFAULT_FLOOR);
    let approvals = approvals.unwrap_or(crate::flow::zones::CORE_DEFAULT_APPROVALS);
    let mut raised = false;
    if floor.rank() > p.approval_role_floor.rank() {
        p.approval_role_floor = floor;
        raised = true;
    }
    if approvals > p.required_approvals {
        p.required_approvals = approvals;
        raised = true;
    }
    if raised {
        p.rederive_status();
    }
}
