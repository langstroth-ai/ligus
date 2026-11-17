//! 0.1 removed the GitHub directory, account, friends and inbox routes and the
//! app's GitHub device-flow login (spec IDENTITY-AND-FORGES §3.3, §5.4). No
//! client path to them may come back: no relay call to those prefixes, no
//! GitHub-token header, no web call to a removed command, and no baked-in
//! GitHub OAuth client id for the old sign-in.

use std::path::{Path, PathBuf};

const FORBIDDEN: &[&str] = &[
    "/v1/directory",
    "/v1/friends",
    "/v1/inbox",
    "/v1/account/",
    "Github-Token",
    "import_github_teams",
    "LIGUS_GITHUB_CLIENT_ID",
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_dir() {
            if !matches!(name, "target" | "node_modules" | "dist" | "tests") {
                sources(&p, out);
            }
        } else if (name.ends_with(".rs") || name.ends_with(".ts") || name.ends_with(".tsx")) && !name.contains(".test.") {
            out.push(p);
        }
    }
}

#[test]
fn no_github_social_client_paths() {
    let root = root();
    let mut files = Vec::new();
    for d in ["app/src", "app/build.rs", "web/src", "crates/hive-runtime/src", "crates/hive-cli/src", "crates/hive-core/src", "crates/hive-forge/src"] {
        let p = root.join(d);
        if p.is_file() {
            files.push(p);
        } else {
            sources(&p, &mut files);
        }
    }
    assert!(files.len() > 50, "found the sources");
    let mut hits = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap_or_default();
        for (i, line) in text.lines().enumerate() {
            // A test that asserts the header is absent may name it.
            if line.contains("assert") || line.trim_start().starts_with("//") {
                continue;
            }
            for bad in FORBIDDEN {
                if line.contains(bad) {
                    hits.push(format!("{}:{}: {bad}", f.strip_prefix(&root).unwrap().display(), i + 1));
                }
            }
        }
    }
    assert!(hits.is_empty(), "stale GitHub social client paths:\n{}", hits.join("\n"));
}
