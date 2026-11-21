//! Golden vectors for the proposal content digest a vote binds
//! (`ligus_core::authz::votes::diff_digest`), including proposals that offer
//! a file (issue #118). The phone apps recompute this digest from the copy
//! they show and refuse to vote when it differs, so they check against these.
//!
//! `golden/proposal_digest_v1.jsonl`: one line per case, `label`, `kind`
//! (the wire kind: `fileDiff`, `decision`), `diff` (or null),
//! `changedFiles`, `id`, `repoId` (or null), `baseCommit` (or null),
//! optional `attachment` (the offered `AttachmentRef`, as the serve API
//! sends it), `attachmentJson` (the exact bytes of the ninth part: the
//! reference's serde JSON, fields in declaration order) and `diffDigest`.
//! None of these cases has a workflow or a command (both parts empty). The
//! values were computed independently (Python `hashlib`); existing lines are
//! frozen.

use ligus_core::attachment::AttachmentRef;
use ligus_core::authz::votes::diff_digest;
use ligus_core::{ActionProposal, ProposalKind};
use uuid::Uuid;

const GOLDEN: &str = include_str!("golden/proposal_digest_v1.jsonl");

fn proposal(v: &serde_json::Value) -> ActionProposal {
    let kind: ProposalKind = serde_json::from_value(v["kind"].clone()).unwrap();
    let mut p = ActionProposal::new("p", kind, "a");
    p.id = Uuid::parse_str(v["id"].as_str().unwrap()).unwrap();
    p.diff = v["diff"].as_str().map(str::to_owned);
    p.changed_files = serde_json::from_value(v["changedFiles"].clone()).unwrap();
    p.repo_id = v["repoId"].as_str().map(|r| Uuid::parse_str(r).unwrap());
    p.base_commit = v["baseCommit"].as_str().map(str::to_owned);
    p.workflow = None;
    p.command = None;
    p.attachment = v.get("attachment").map(|a| Box::new(serde_json::from_value::<AttachmentRef>(a.clone()).unwrap()));
    p
}

#[test]
fn proposal_digest_vectors_hold() {
    let mut n = 0;
    let mut offers = 0;
    for line in GOLDEN.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let label = v["label"].as_str().unwrap();
        let p = proposal(&v);
        if let Some(a) = &p.attachment {
            // The ninth part is exactly this JSON.
            assert_eq!(serde_json::to_string(a).unwrap(), v["attachmentJson"].as_str().unwrap(), "{label}");
            offers += 1;
        }
        assert_eq!(diff_digest(&p), v["diffDigest"].as_str().unwrap(), "{label}");
        n += 1;
    }
    assert!(n >= 6 && offers >= 4, "{n} vectors, {offers} with a file");
}

/// The offered file is bound: the same proposal with and without it, or with
/// another file, has another digest.
#[test]
fn the_offered_file_is_bound() {
    let lines: Vec<serde_json::Value> = GOLDEN.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let by = |label: &str| lines.iter().find(|v| v["label"] == label).unwrap()["diffDigest"].clone();
    let plain = by("decision/plain");
    let offer = by("decision/offer");
    let other = by("decision/offer-other-file");
    assert!(plain != offer && offer != other && plain != other);
}
