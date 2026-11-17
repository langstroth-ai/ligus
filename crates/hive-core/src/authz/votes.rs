//! Votes bound to their signer and to the content they approve; proposal
//! status derived from counted votes, never asserted.

use sha2::{Digest, Sha256};

use crate::proposals::{ActionProposal, ProposalStatus};

/// Domain tag of [`diff_digest`] (Phase 0 list, section 0.1).
pub const PROPOSAL_DIGEST_TAG: &[u8] = b"ligus-proposal-digest-v1\0";

/// The apply-content digest a vote binds to: SHA-256 over the tag and the
/// length-prefixed parts, in order: kind tag, diff, changed-files JSON,
/// workflow JSON, command JSON (Track D's structured command; empty when
/// none), proposal id (16 bytes), repo id (16 bytes; empty when none) and
/// base commit (its ASCII hex; empty when none). Lowercase hex.
///
/// Phase A: the proposal id, repo and base are in the preimage, so a vote
/// for "this diff, in this proposal, to this repo, on this base" never
/// counts for another proposal with the same patch, the same patch against
/// another repo, or a re-based copy. The domain string stays
/// `ligus-proposal-digest-v1`: it was never in a released build (no v1.x tag
/// contains it), so 2.0 defines it with these parts from the start and no
/// older client computes it differently.
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
    let command: Vec<u8> = match &p.command {
        Some(c) => serde_json::to_vec(c).expect("serialize command"),
        None => Vec::new(),
    };
    let repo: Vec<u8> = p.repo_id.map(|r| r.as_bytes().to_vec()).unwrap_or_default();
    let base = p.base_commit.as_deref().unwrap_or("").as_bytes().to_vec();
    let mut h = Sha256::new();
    h.update(PROPOSAL_DIGEST_TAG);
    for part in [kind.as_bytes(), &diff, &files, &workflow, &command, p.id.as_bytes(), &repo, &base] {
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
            // The repo is sticky (repos review F1): an upsert never re-points
            // it, so it can't make a vote for one repo count for another.
            p.repo_id = old.repo_id;
            let same_content = diff_digest(old) == diff_digest(&p);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proposals::{ProposalKind, ProposedCommand};
    use uuid::Uuid;

    #[test]
    fn the_structured_command_is_bound_into_the_digest() {
        let mut p = ActionProposal::new("run", ProposalKind::Command, "a");
        let none = diff_digest(&p);
        p.command = Some(ProposedCommand { argv: vec!["cargo".into(), "test".into()], cwd: None });
        let a = diff_digest(&p);
        p.command = Some(ProposedCommand { argv: vec!["cargo".into(), "test".into(), "--all".into()], cwd: None });
        let b = diff_digest(&p);
        assert!(none != a && a != b, "a vote for one argv never counts for another");
        p.command = None;
        assert_eq!(diff_digest(&p), none, "no command keeps the pre-D4 digest");
    }

    /// Phase A: the proposal id, the repo and the base are bound in.
    #[test]
    fn proposal_repo_and_base_are_bound_into_the_digest() {
        let mut a = ActionProposal::file_diff("p", "a", "diff --git a/f b/f\n+1\n".into(), vec!["f".into()]);
        let mut b = a.clone();
        b.id = Uuid::from_u128(2);
        a.id = Uuid::from_u128(1);
        assert_ne!(diff_digest(&a), diff_digest(&b), "the same patch in another proposal");
        let plain = diff_digest(&a);
        a.repo_id = Some(Uuid::from_u128(7));
        let with_repo = diff_digest(&a);
        a.base_commit = Some("0123456789abcdef0123456789abcdef01234567".into());
        let with_base = diff_digest(&a);
        assert!(plain != with_repo && with_repo != with_base);
    }

    /// The pinned digest of a known proposal (a test vector other
    /// implementations, the iOS app's included, check against).
    #[test]
    fn digest_vector() {
        let mut p = ActionProposal::file_diff("p", "a", "diff --git a/f b/f\n+1\n".into(), vec!["f".into()]);
        p.id = Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888);
        p.repo_id = Some(Uuid::from_u128(0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0000));
        p.base_commit = Some("0123456789abcdef0123456789abcdef01234567".into());
        assert_eq!(diff_digest(&p), DIGEST_VECTOR);
    }
    // Cross-checked with an independent Python hashlib computation of the
    // same length-prefixed preimage.
    const DIGEST_VECTOR: &str = "855cb7b2384b0e4343481fcd43dcc14a659d346677c06fbb5e14937756caae69";

    #[test]
    fn command_shape_is_checked() {
        let ok = ProposedCommand { argv: vec!["make".into()], cwd: Some("crates/x".into()) };
        assert!(ok.validate().is_ok());
        for bad in [
            ProposedCommand { argv: vec![], cwd: None },
            ProposedCommand { argv: vec!["make".into()], cwd: Some("../x".into()) },
            ProposedCommand { argv: vec!["make".into()], cwd: Some("/etc".into()) },
            ProposedCommand { argv: vec!["a\0b".into()], cwd: None },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }
}
