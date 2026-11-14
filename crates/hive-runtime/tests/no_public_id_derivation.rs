//! S9 (`no_room_or_invite_id_derived_from_public_ids`): no room, workspace,
//! chat or invite id may be derived from a public identifier (an account id,
//! an account key, a forge id, a handle or an email). Every `Uuid::new_v5(`
//! call site in the shipped sources is on an allow-list whose inputs are
//! random or are themselves ids that were random to begin with; a new call
//! site fails this test until someone reviews it and adds it here.

use std::path::{Path, PathBuf};

/// (file suffix, a fragment of the call line, why it's fine).
const ALLOWED: &[(&str, &str, &str)] = &[
    (
        "crates/hive-core/src/channel.rs",
        "Uuid::new_v5(&NS, workspace_id.as_bytes())",
        "config-log session id: input is the workspace id",
    ),
    (
        "crates/hive-core/src/channel.rs",
        "Uuid::new_v5(&NS, room.trim().as_bytes())",
        "room workspace id: input is the room, random for every room 2.0 makes",
    ),
    (
        "crates/hive-core/src/session.rs",
        "Uuid::new_v5(&NS, &name)",
        "turn-claim key: input is a random message id and an agent id",
    ),
    (
        "crates/hive-core/src/crypto.rs",
        "Uuid::new_v5(&agent_id, signing_public_key)",
        "agent key id: names an agent signing key, not a room, chat or invite",
    ),
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_dir() {
            if name != "target" && name != "tests" && !name.starts_with('.') {
                rust_sources(&p, out);
            }
        } else if name.ends_with(".rs") && !name.ends_with("_tests.rs") && name != "tests.rs" {
            out.push(p);
        }
    }
}

/// The part of a file before its first `#[cfg(test)]` module (inline tests
/// may derive whatever they like).
fn non_test_part(text: &str) -> &str {
    match text.find("#[cfg(test)]\nmod ") {
        Some(i) => &text[..i],
        None => text,
    }
}

#[test]
fn no_room_or_invite_id_derived_from_public_ids() {
    let root = workspace_root();
    let mut files = Vec::new();
    for dir in ["crates", "app/src"] {
        rust_sources(&root.join(dir), &mut files);
    }
    assert!(files.len() > 50, "found the sources ({})", files.len());
    let mut offenders = Vec::new();
    for f in &files {
        let rel = f.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
        let text = std::fs::read_to_string(f).unwrap();
        for (n, line) in non_test_part(&text).lines().enumerate() {
            if !line.contains("new_v5(") || line.trim_start().starts_with("//") {
                continue;
            }
            let ok = ALLOWED.iter().any(|(file, frag, _)| rel.ends_with(file) && line.contains(frag));
            if !ok {
                offenders.push(format!("{rel}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    assert!(offenders.is_empty(), "UUIDv5 call sites not on the allow-list:\n{}", offenders.join("\n"));
}
