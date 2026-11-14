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
    let command: Vec<u8> = match &p.command {
        Some(c) => serde_json::to_vec(c).expect("serialize command"),
        None => Vec::new(),
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proposals::{ProposalKind, ProposedCommand};

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
