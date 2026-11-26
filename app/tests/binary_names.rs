//! The desktop app's executable must never share a name with a workspace
//! binary, compared case-insensitively. On macOS's default (case-insensitive)
//! filesystem `target/release/Ligus` and `target/release/ligus` are the same
//! file, so a desktop build named `Ligus` silently overwrote the `ligus` CLI.

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

/// The `name = "..."` value on a TOML line, if the line is one.
fn name_value(line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix("name")?.trim_start().strip_prefix('=')?.trim();
    Some(rest.trim_matches('"').to_string())
}

/// The desktop app's main binary: `mainBinaryName`, else the package name.
fn desktop_binary() -> String {
    let conf: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root().join("app/tauri.conf.json")).unwrap()).unwrap();
    match conf.get("mainBinaryName").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => env!("CARGO_PKG_NAME").to_string(),
    }
}

/// Every binary the workspace's crates build: `[[bin]]` names, `src/bin/*.rs`,
/// and the package name of a crate with `src/main.rs` and no `[[bin]]`.
fn workspace_binaries() -> Vec<String> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(root().join("crates")).unwrap() {
        let dir = entry.unwrap().path();
        let Ok(manifest) = std::fs::read_to_string(dir.join("Cargo.toml")) else { continue };
        let mut section = String::new();
        let mut package = None;
        let mut bins = Vec::new();
        for line in manifest.lines() {
            let t = line.trim();
            if t.starts_with('[') {
                section = t.to_string();
            } else if let Some(n) = name_value(t) {
                match section.as_str() {
                    "[package]" => package = Some(n),
                    "[[bin]]" => bins.push(n),
                    _ => {}
                }
            }
        }
        if bins.is_empty() && dir.join("src/main.rs").exists() {
            bins.extend(package);
        }
        if let Ok(rd) = std::fs::read_dir(dir.join("src/bin")) {
            for f in rd {
                let p = f.unwrap().path();
                if p.extension().is_some_and(|e| e == "rs") {
                    bins.push(p.file_stem().unwrap().to_string_lossy().into_owned());
                }
            }
        }
        out.extend(bins);
    }
    out
}

#[test]
fn the_desktop_binary_never_collides_with_a_workspace_binary() {
    let desktop = desktop_binary();
    let bins = workspace_binaries();
    assert!(bins.iter().any(|b| b == "ligus"), "the CLI's `ligus` binary wasn't found: {bins:?}");
    for b in &bins {
        assert!(
            !b.eq_ignore_ascii_case(&desktop),
            "the desktop app's main binary {desktop:?} is the same file as the {b:?} binary on a case-insensitive filesystem"
        );
    }
}

#[test]
fn the_desktop_binary_is_ligus_desktop() {
    assert_eq!(desktop_binary(), "ligus-desktop");
}
