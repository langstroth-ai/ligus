//! Source lint: no event-store reader takes "newest", "latest" or "after"
//! from the local row order.
//!
//! The fold's order is `(lamport, event_id)`. `row_id` (and the local
//! `sequence` column, assigned the same way) is a storage key: the order rows
//! arrived on this device. An old event pulled again from the relay lands as a
//! new row, so a reader that picks the highest row picks a stale event.
//!
//! Every SQL statement in `event_store.rs` and `event_store/*.rs` (tests
//! aside) that orders by `row_id`, `rowid` or `sequence`, or takes
//! `MAX`/`MIN` of a row id, must sit in a function listed below with the
//! reason row order is right there. A new one fails this test: order it by
//! `(lamport, event_id)` (or sort with `canonical_key`) instead, or add it here
//! with its reason. An entry that matches nothing fails too, so the list stays
//! exact.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// A row-id cursor: "what this device stored since row N", which is arrival
/// order by definition (push scans, change feeds, exports).
const CURSOR: &str = "an arrival cursor over this device's rows";
/// A set read: the consumer sorts by `canonical_key`, or only checks
/// membership.
const SET: &str = "a set read; the consumer sorts by canonical_key or checks membership";
/// A device-local table that isn't a log (facts, signals, pins, witnesses).
const LOCAL: &str = "a device-local table, not a log";
/// Storage maintenance.
const STORAGE: &str = "a migration or prune scan";

/// `(file under src/, function or const, why row order is right)`.
const ALLOWED: &[(&str, &str, &str)] = &[
    ("event_store.rs", "migrate_event_ids_per_workspace", STORAGE),
    ("event_store.rs", "prune_superseded_chunks", STORAGE),
    ("event_store.rs", "rows_sig", CURSOR),
    ("event_store.rs", "max_row_id", CURSOR),
    ("event_store.rs", "max_equivocation_row", CURSOR),
    ("event_store.rs", "max_equivocation_row_id", CURSOR),
    ("event_store.rs", "envelopes_after_row", CURSOR),
    ("event_store.rs", "logs_changed_after", CURSOR),
    ("event_store.rs", "logs_equivocated_after", CURSOR),
    ("event_store.rs", "workspace_rows", CURSOR),
    ("event_store.rs", "list_session_ids", SET),
    ("event_store.rs", "roster_envelopes", SET),
    ("event_store.rs", "trust_envelopes", SET),
    ("event_store.rs", "identity_envelopes", SET),
    ("event_store.rs", "has_device_certificate", SET),
    ("event_store.rs", "has_device_certificate_in", SET),
    ("event_store.rs", "for_each_signed_by", SET),
    ("event_store.rs", "witnesses_of", LOCAL),
    ("event_store.rs", "witness_ids", LOCAL),
    ("event_store.rs", "max_fact_row", LOCAL),
    ("event_store.rs", "session_facts", LOCAL),
    ("event_store.rs", "facts_since", LOCAL),
    ("event_store.rs", "facts_after", LOCAL),
    ("event_store.rs", "list_signals", LOCAL),
    ("event_store.rs", "signals_after", LOCAL),
    ("event_store.rs", "max_signal_row", LOCAL),
    ("event_store.rs", "pinned_workspaces", LOCAL),
    ("event_store/held.rs", "trust_rows_sig", CURSOR),
];

/// Whether a source line orders by a row key or takes the max/min of one.
fn orders_by_row(line: &str) -> bool {
    let row_key = |s: &str| {
        s.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
            .any(|w| matches!(w.rsplit('.').next(), Some("row_id" | "rowid" | "sequence")))
    };
    if let Some(at) = line.find("ORDER BY") {
        let clause = &line[at + "ORDER BY".len()..];
        let clause = clause.split("LIMIT").next().unwrap_or(clause);
        if row_key(clause) {
            return true;
        }
    }
    // `MAX(sequence)` allocates the next local sequence: storage, not a read.
    let row_id = |s: &str| matches!(s.trim().rsplit('.').next(), Some("row_id" | "rowid"));
    ["MAX(", "MIN("].iter().any(|f| line.match_indices(f).any(|(i, _)| row_id(line[i + f.len()..].split(')').next().unwrap_or(""))))
}

/// The item a line opens, when it opens a function or const at the top
/// level or in an `impl` (indented at most 4 spaces; a helper `fn` nested in
/// a function body belongs to that function).
fn item_name(line: &str) -> Option<&str> {
    let indent = line.len() - line.trim_start().len();
    if indent > 4 {
        return None;
    }
    let mut rest = line.trim_start();
    for vis in ["pub(crate) ", "pub(super) ", "pub "] {
        rest = rest.strip_prefix(vis).unwrap_or(rest);
    }
    let rest = rest.strip_prefix("fn ").or_else(|| rest.strip_prefix("const "))?;
    let name = rest.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).next()?;
    (!name.is_empty()).then_some(name)
}

/// `(file, item) -> lines` of every row-ordered statement outside tests.
fn hits(src: &Path) -> BTreeMap<(String, String), Vec<usize>> {
    let mut files = vec!["event_store.rs".to_string()];
    let mut subs: Vec<String> = std::fs::read_dir(src.join("event_store"))
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".rs") && !n.ends_with("_tests.rs") && n != "order_lint.rs")
        .map(|n| format!("event_store/{n}"))
        .collect();
    subs.sort();
    files.extend(subs);
    let mut out: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for file in files {
        let text = std::fs::read_to_string(src.join(&file)).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let mut item = String::new();
        for (n, line) in lines.iter().enumerate() {
            // The test module ends what this lint reads.
            if line.trim() == "#[cfg(test)]" && lines.get(n + 1).is_some_and(|l| l.trim_start().starts_with("mod ") && l.trim_end().ends_with('{')) {
                break;
            }
            if line.trim_start().starts_with("//") {
                continue;
            }
            if let Some(name) = item_name(line) {
                item = name.to_string();
            }
            if orders_by_row(line) {
                out.entry((file.clone(), item.clone())).or_default().push(n + 1);
            }
        }
    }
    out
}

#[test]
fn no_event_store_reader_orders_by_row_id_outside_the_allow_list() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let found = hits(&src);
    let allowed: BTreeSet<(String, String)> = ALLOWED.iter().map(|(f, i, _)| (f.to_string(), i.to_string())).collect();
    let unlisted: Vec<String> = found
        .iter()
        .filter(|(k, _)| !allowed.contains(*k))
        .map(|((f, i), lines)| format!("{f} {i} (lines {lines:?})"))
        .collect();
    assert!(
        unlisted.is_empty(),
        "order these by (lamport, event_id), never by row_id or sequence, or list them in order_lint.rs with a reason: {unlisted:#?}"
    );
    let stale: Vec<&(String, String)> = allowed.iter().filter(|k| !found.contains_key(*k)).collect();
    assert!(stale.is_empty(), "allow-list entries that match no statement (remove them): {stale:#?}");
}

#[test]
fn the_lint_sees_what_it_should() {
    assert!(orders_by_row(r#""SELECT x FROM events ORDER BY row_id DESC LIMIT 1""#));
    assert!(orders_by_row(r#""SELECT x FROM events q ORDER BY q.row_id""#));
    assert!(orders_by_row(r#""SELECT x FROM events WHERE a = ?1 ORDER BY sequence ASC""#));
    assert!(orders_by_row(r#""SELECT MAX(row_id) FROM events WHERE kind = 'sessionSnapshot'""#));
    assert!(orders_by_row(r#""SELECT x FROM pins ORDER BY rowid ASC""#));
    assert!(!orders_by_row(r#""SELECT x FROM events WHERE row_id > ?1""#));
    assert!(!orders_by_row(r#""SELECT x FROM events ORDER BY json_extract(envelope_json, '$.lamport') DESC, event_id DESC LIMIT 1""#));
    assert!(!orders_by_row("last = last.max(row_id);"));
    assert!(!orders_by_row(r#""SELECT COALESCE(MAX(sequence), 0) + 1 FROM events WHERE session_id = ?1""#));
    assert_eq!(item_name("    pub fn list_in(&self) {"), Some("list_in"));
    assert_eq!(item_name("const LAST_SIGNED_BY_SQL: &str = \"\";"), Some("LAST_SIGNED_BY_SQL"));
    assert_eq!(item_name("        fn helper(json: &str) {"), None);
}
