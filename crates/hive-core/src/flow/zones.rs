//! Path zones: the layout of a repo encodes review policy. **Core** paths
//! (the brood: sensitive, central code) carry a high review bar; **low-bar**
//! paths (the stores: docs, tests, generated files) a light one. A change's
//! approval floor and quorum come from where it lands, and board thresholds
//! rise for core paths.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::keys::normalize_path;
use crate::identity::WorkspaceRole;
use crate::proposals::ActionProposal;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ZoneKind {
    /// Core / sensitive: high review bar.
    Core,
    /// Docs, tests, generated: low review bar.
    LowBar,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathZone {
    pub id: Uuid,
    /// A path prefix (`crates/hive-core/`), an exact path, or a glob with `*`
    /// (within one segment) and `**` (any depth): `**/*.md`.
    pub pattern: String,
    pub kind: ZoneKind,
    /// Minimum role a vote needs to count for a change landing here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_floor: Option<WorkspaceRole>,
    /// Approvals a change landing here needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_approvals: Option<u32>,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub at_ms: i64,
}

/// Glob match on `/`-separated paths: `*` matches within a segment, `**`
/// matches any number of segments. A pattern ending in `/` is a prefix.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern = normalize_path(pattern);
    let path = normalize_path(path);
    if pattern.is_empty() {
        return false;
    }
    if pattern.ends_with('/') {
        return path.starts_with(&pattern);
    }
    if !pattern.contains('*') {
        return path == pattern || path.starts_with(&format!("{pattern}/"));
    }
    let p: Vec<&str> = pattern.split('/').collect();
    let s: Vec<&str> = path.split('/').collect();
    match_segments(&p, &s)
}

fn match_segments(p: &[&str], s: &[&str]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some(&"**") => (0..=s.len()).any(|i| match_segments(&p[1..], &s[i..])),
        Some(seg) => !s.is_empty() && match_one(seg, s[0]) && match_segments(&p[1..], &s[1..]),
    }
}

fn match_one(pat: &str, s: &str) -> bool {
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == s;
    }
    let mut rest = s;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            if !rest.starts_with(part) {
                return false;
            }
            rest = &rest[part.len()..];
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else if let Some(at) = rest.find(part) {
            rest = &rest[at + part.len()..];
        } else {
            return false;
        }
    }
    true
}

/// The zone a path falls in: the longest matching pattern wins; core beats
/// low-bar on a tie.
pub fn zone_of<'a>(zones: &'a [PathZone], path: &str) -> Option<&'a PathZone> {
    zones
        .iter()
        .filter(|z| glob_match(&z.pattern, path))
        .max_by_key(|z| (z.pattern.len(), matches!(z.kind, ZoneKind::Core)))
}

/// Default bar for core paths when a zone doesn't set one.
pub const CORE_DEFAULT_FLOOR: WorkspaceRole = WorkspaceRole::Admin;
pub const CORE_DEFAULT_APPROVALS: u32 = 2;

/// What a change touching `files` needs: (floor, approvals, zone kind) —
/// the strictest zone among the files; low-bar only when *every* file is in
/// a low-bar zone.
pub fn review_bar(zones: &[PathZone], files: &[String]) -> Option<(Option<WorkspaceRole>, Option<u32>, ZoneKind)> {
    if zones.is_empty() || files.is_empty() {
        return None;
    }
    let hits: Vec<Option<&PathZone>> = files.iter().map(|f| zone_of(zones, f)).collect();
    let core: Vec<&PathZone> = hits.iter().flatten().filter(|z| z.kind == ZoneKind::Core).copied().collect();
    if !core.is_empty() {
        let floor = core.iter().map(|z| z.approval_floor.unwrap_or(CORE_DEFAULT_FLOOR)).max_by_key(|r| r.rank());
        let approvals = core.iter().map(|z| z.required_approvals.unwrap_or(CORE_DEFAULT_APPROVALS)).max();
        return Some((floor, approvals, ZoneKind::Core));
    }
    if hits.iter().all(|z| z.is_some_and(|z| z.kind == ZoneKind::LowBar)) {
        let floor = hits.iter().flatten().filter_map(|z| z.approval_floor).max_by_key(|r| r.rank());
        let approvals = hits.iter().flatten().filter_map(|z| z.required_approvals).max();
        return Some((floor, approvals, ZoneKind::LowBar));
    }
    None
}

/// Apply the zone bar to a file-diff proposal before it is filed. A core
/// zone only ever *raises* the bar; a low-bar zone may lower the quorum but
/// never below one approval. Returns the zone kind applied.
///
/// `protected`: the change is under a workspace guard's human gate (dearth
/// brood paths, a chat lockdown; see `guard::guards_protect`). Then the
/// strictest bar wins and a low-bar zone can't lower anything.
pub fn apply_review_bar(proposal: &mut ActionProposal, zones: &[PathZone], protected: bool) -> Option<ZoneKind> {
    let (floor, approvals, kind) = review_bar(zones, &proposal.changed_files)?;
    if protected && kind == ZoneKind::LowBar {
        return None;
    }
    match kind {
        ZoneKind::Core => {
            if let Some(f) = floor {
                if f.rank() > proposal.approval_role_floor.rank() {
                    proposal.approval_role_floor = f;
                }
            }
            if let Some(n) = approvals {
                proposal.required_approvals = proposal.required_approvals.max(n);
            }
        }
        ZoneKind::LowBar => {
            if let Some(f) = floor {
                proposal.approval_role_floor = f;
            }
            if let Some(n) = approvals {
                proposal.required_approvals = n.max(1);
            }
        }
    }
    Some(kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proposals::ActionProposal;

    fn zone(pattern: &str, kind: ZoneKind) -> PathZone {
        PathZone { id: Uuid::new_v4(), pattern: pattern.into(), kind, approval_floor: None, required_approvals: None, note: String::new(), at_ms: 0 }
    }

    #[test]
    fn globs() {
        assert!(glob_match("crates/hive-core/", "crates/hive-core/src/lib.rs"));
        assert!(glob_match("crates/hive-core", "crates/hive-core/src/lib.rs"));
        assert!(!glob_match("crates/hive-core", "crates/hive-core-x/a.rs"));
        assert!(glob_match("**/*.md", "README.md"));
        assert!(glob_match("**/*.md", "docs/a/b.md"));
        assert!(!glob_match("**/*.md", "docs/a/b.rs"));
        assert!(glob_match("src/*/mod.rs", "src/flow/mod.rs"));
        assert!(!glob_match("src/*/mod.rs", "src/a/b/mod.rs"));
        assert!(glob_match("src/**/crypto*.rs", "src/x/y/crypto_v2.rs"));
    }

    #[test]
    fn core_raises_the_bar_and_wins_over_low_bar() {
        let zones = vec![zone("crates/hive-core/", ZoneKind::Core), zone("**/*.md", ZoneKind::LowBar)];
        let mut p = ActionProposal::file_diff("x", "a", "d".into(), vec!["crates/hive-core/src/crypto.rs".into(), "README.md".into()]);
        assert_eq!(apply_review_bar(&mut p, &zones, false), Some(ZoneKind::Core));
        assert_eq!(p.approval_role_floor, WorkspaceRole::Admin);
        assert_eq!(p.required_approvals, 2);
    }

    #[test]
    fn low_bar_only_when_every_file_is_low_bar() {
        let mut z = zone("**/*.md", ZoneKind::LowBar);
        z.required_approvals = Some(0);
        let zones = vec![z];
        let mut docs = ActionProposal::file_diff("x", "a", "d".into(), vec!["a.md".into(), "docs/b.md".into()]);
        docs.required_approvals = 2;
        assert_eq!(apply_review_bar(&mut docs, &zones, false), Some(ZoneKind::LowBar));
        assert_eq!(docs.required_approvals, 1, "never below one approval");
        let mut mixed = ActionProposal::file_diff("x", "a", "d".into(), vec!["a.md".into(), "src/x.rs".into()]);
        mixed.required_approvals = 2;
        assert_eq!(apply_review_bar(&mut mixed, &zones, false), None);
        assert_eq!(mixed.required_approvals, 2);
    }

    #[test]
    fn core_never_lowers_an_already_higher_bar() {
        let mut z = zone("src/", ZoneKind::Core);
        z.approval_floor = Some(WorkspaceRole::Contributor);
        z.required_approvals = Some(1);
        let mut p = ActionProposal::file_diff("x", "a", "d".into(), vec!["src/a.rs".into()]);
        p.approval_role_floor = WorkspaceRole::Owner;
        p.required_approvals = 3;
        apply_review_bar(&mut p, &[z], false);
        assert_eq!(p.approval_role_floor, WorkspaceRole::Owner);
        assert_eq!(p.required_approvals, 3);
    }

    #[test]
    fn a_guarded_change_never_gets_a_lowered_bar() {
        let mut low = zone("**/*.md", ZoneKind::LowBar);
        low.required_approvals = Some(1);
        low.approval_floor = Some(WorkspaceRole::Viewer);
        let mut docs = ActionProposal::file_diff("x", "a", "d".into(), vec!["docs/a.md".into()]);
        docs.required_approvals = 2;
        docs.approval_role_floor = WorkspaceRole::Contributor;
        assert_eq!(apply_review_bar(&mut docs, std::slice::from_ref(&low), true), None);
        assert_eq!((docs.required_approvals, docs.approval_role_floor), (2, WorkspaceRole::Contributor));
        // Unguarded, the same zone lowers it.
        assert_eq!(apply_review_bar(&mut docs, std::slice::from_ref(&low), false), Some(ZoneKind::LowBar));
        assert_eq!((docs.required_approvals, docs.approval_role_floor), (1, WorkspaceRole::Viewer));
        // A core zone still raises a guarded change.
        let mut p = ActionProposal::file_diff("x", "a", "d".into(), vec!["src/a.rs".into()]);
        assert_eq!(apply_review_bar(&mut p, &[zone("src/", ZoneKind::Core)], true), Some(ZoneKind::Core));
        assert_eq!(p.approval_role_floor, WorkspaceRole::Admin);
    }

    #[test]
    fn longest_pattern_wins() {
        let zones = vec![zone("src/", ZoneKind::Core), zone("src/generated/", ZoneKind::LowBar)];
        assert_eq!(zone_of(&zones, "src/generated/a.rs").unwrap().kind, ZoneKind::LowBar);
        assert_eq!(zone_of(&zones, "src/a.rs").unwrap().kind, ZoneKind::Core);
    }
}
