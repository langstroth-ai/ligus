//! A per-turn home for CLI agents (hardening C-9).
//!
//! An agent that can write the CLI's own config (`~/.claude/settings.json`
//! hooks, `~/.claude.json` or `~/.codex/config.toml` MCP servers, …) gets
//! code run **unsandboxed** the next time the user starts that CLI. So a CLI
//! child never writes the user's real config: it runs with `HOME` (and the
//! XDG dirs and the CLI's own config-dir variable) pointing at a fresh
//! directory inside the turn's 0700 scratch dir, which is removed with the
//! turn. Only what the CLI needs to start and stay signed in is copied in:
//! the top-level files of its config dirs (no hook scripts, no history), at
//! most [`MAX_COPY_BYTES`] each.
//!
//! - The real config dirs are no longer writable by the child (they're not
//!   in the plan's `write_extra`); a write there fails in the sandbox.
//! - Sign-in tokens a CLI refreshes during the turn are copied back after it
//!   (only the credential files in [`credential_files`], only when they
//!   still parse as a JSON object), so a rotated refresh token isn't lost.
//! - macOS: the login keychain is reached through symlinks to the real
//!   `~/Library/Keychains` and the security preferences (the keychain is a
//!   service, not a file the agent can write through the profile).
//!
//! `LIGUS_CLI_HOME=shared` (device-local) turns this off and lets the CLIs
//! write their real state dirs, as before 2.0.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use hive_core::ModelProviderKind;

/// Largest file copied into a turn home.
pub const MAX_COPY_BYTES: u64 = 1024 * 1024;

/// Whether CLI children get a per-turn home (the default).
pub fn enabled() -> bool {
    !hive_core::compat::var("LIGUS_CLI_HOME").is_ok_and(|v| v.trim().eq_ignore_ascii_case("shared"))
}

/// Config dirs (relative to home) whose top-level files a CLI needs.
pub fn config_dirs(kind: ModelProviderKind) -> &'static [&'static str] {
    match kind {
        ModelProviderKind::ClaudeCode => &[".claude"],
        ModelProviderKind::Codex => &[".codex"],
        ModelProviderKind::Copilot => &[".copilot", ".config/github-copilot"],
        ModelProviderKind::Antigravity => &[".gemini", ".antigravity", ".config/agy"],
        ModelProviderKind::Pi => &[".pi"],
        ModelProviderKind::Hermes => &[".hermes"],
        _ => &[],
    }
}

/// Single config files (relative to home) a CLI reads.
pub fn config_files(kind: ModelProviderKind) -> &'static [&'static str] {
    match kind {
        ModelProviderKind::ClaudeCode => &[".claude.json"],
        ModelProviderKind::Aider => &[".aider.conf.yml"],
        _ => &[],
    }
}

/// Sign-in files a CLI may refresh during a turn; copied back afterwards.
pub fn credential_files(kind: ModelProviderKind) -> &'static [&'static str] {
    match kind {
        ModelProviderKind::ClaudeCode => &[".claude/.credentials.json"],
        ModelProviderKind::Codex => &[".codex/auth.json"],
        ModelProviderKind::Copilot => &[".config/github-copilot/apps.json", ".config/github-copilot/hosts.json"],
        ModelProviderKind::Antigravity => &[".gemini/oauth_creds.json", ".gemini/google_accounts.json"],
        _ => &[],
    }
}

/// The CLI's own config-dir variable, when the user set one: where its real
/// config lives, and what the turn home overrides.
fn dir_var(kind: ModelProviderKind) -> Option<(&'static str, &'static str)> {
    match kind {
        ModelProviderKind::ClaudeCode => Some(("CLAUDE_CONFIG_DIR", ".claude")),
        ModelProviderKind::Codex => Some(("CODEX_HOME", ".codex")),
        ModelProviderKind::Copilot => Some(("COPILOT_HOME", ".copilot")),
        _ => None,
    }
}

/// Where a relative config path really lives (a user-set config-dir
/// variable wins over `~`).
fn real_path(kind: ModelProviderKind, real_home: &Path, rel: &str) -> PathBuf {
    if let Some((var, dir)) = dir_var(kind) {
        if let Some(custom) = std::env::var_os(var).filter(|v| !v.is_empty()) {
            if let Some(rest) = rel.strip_prefix(dir) {
                return PathBuf::from(custom).join(rest.trim_start_matches('/'));
            }
        }
    }
    real_home.join(rel)
}

fn copy_file(from: &Path, to: &Path) {
    let Ok(meta) = std::fs::symlink_metadata(from) else { return };
    if !meta.is_file() || meta.len() > MAX_COPY_BYTES {
        return;
    }
    if let Some(parent) = to.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(bytes) = std::fs::read(from) {
        let _ = crate::private_file::write_private_atomic(to, &bytes);
    }
}

/// Build the turn home for `kind` at `home` from `real_home`.
pub fn populate(kind: ModelProviderKind, real_home: &Path, home: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(home)?;
    for d in config_dirs(kind) {
        let src = real_path(kind, real_home, d);
        let dst = home.join(d);
        let _ = std::fs::create_dir_all(&dst);
        if let Ok(rd) = std::fs::read_dir(&src) {
            for e in rd.flatten() {
                copy_file(&e.path(), &dst.join(e.file_name()));
            }
        }
    }
    for f in config_files(kind) {
        copy_file(&real_path(kind, real_home, f), &home.join(f));
    }
    #[cfg(target_os = "macos")]
    {
        // The login keychain and its search list, reached through the real
        // paths (the profile keeps them unwritable).
        let lib = home.join("Library");
        let _ = std::fs::create_dir_all(lib.join("Preferences"));
        let _ = std::os::unix::fs::symlink(real_home.join("Library/Keychains"), lib.join("Keychains"));
        for p in ["com.apple.security.plist", "com.apple.security.KCN.plist"] {
            let _ = std::os::unix::fs::symlink(real_home.join("Library/Preferences").join(p), lib.join("Preferences").join(p));
        }
    }
    Ok(())
}

/// The env a CLI child gets for its turn home: `HOME`, the XDG dirs, and
/// the CLI's config-dir variable when the user had set one.
pub fn env(kind: ModelProviderKind, home: &Path) -> Vec<(OsString, OsString)> {
    let mut v: Vec<(OsString, OsString)> = vec![("HOME".into(), home.as_os_str().to_owned())];
    for (k, rel) in [("XDG_CONFIG_HOME", ".config"), ("XDG_DATA_HOME", ".local/share"), ("XDG_STATE_HOME", ".local/state"), ("XDG_CACHE_HOME", ".cache")] {
        v.push((k.into(), home.join(rel).into_os_string()));
    }
    if let Some((var, dir)) = dir_var(kind) {
        if std::env::var_os(var).is_some_and(|x| !x.is_empty()) {
            v.push((var.into(), home.join(dir).into_os_string()));
        }
    }
    v
}

/// Read a file the agent could have written in its turn home, as the host
/// (unsandboxed) without following anything the agent planted: no symlinked
/// directory on the way, no symlink at the end (`O_NOFOLLOW`), and a regular
/// file with one link (a hard link to a file the sandbox denies the agent
/// would otherwise be read through).
fn read_turn_file(home: &Path, rel: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    // One `openat` per component, none followed if it is a symlink: a
    // directory the agent swaps for a link mid-read can't redirect it.
    let f = crate::safe_fs::open_beneath(home, Path::new(rel), crate::safe_fs::Mode::Read).ok()?;
    let meta = f.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_COPY_BYTES {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() != 1 {
            return None;
        }
    }
    let mut buf = Vec::new();
    f.take(MAX_COPY_BYTES + 1).read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// After the turn: copy refreshed sign-in files back to their real place
/// (only valid JSON objects, only the files in [`credential_files`]).
pub fn copy_back(kind: ModelProviderKind, real_home: &Path, home: &Path) {
    for f in credential_files(kind) {
        let real = real_path(kind, real_home, f);
        let (Some(new), old) = (read_turn_file(home, f), std::fs::read(&real).ok()) else { continue };
        if old.as_deref() == Some(new.as_slice()) || new.len() as u64 > MAX_COPY_BYTES {
            continue;
        }
        if !serde_json::from_slice::<serde_json::Value>(&new).is_ok_and(|v| v.is_object()) {
            continue;
        }
        // Only a file the user already had (a sign-in this turn didn't make).
        if old.is_some() {
            let _ = crate::private_file::write_private_atomic(&real, &new);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_top_level_config_and_never_subdirs() {
        let real = tempfile::tempdir().unwrap();
        let turn = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(real.path().join(".claude/hooks")).unwrap();
        std::fs::write(real.path().join(".claude/settings.json"), "{}").unwrap();
        std::fs::write(real.path().join(".claude/hooks/x.sh"), "echo").unwrap();
        std::fs::write(real.path().join(".claude.json"), "{\"a\":1}").unwrap();
        let home = turn.path().join("home");
        populate(ModelProviderKind::ClaudeCode, real.path(), &home).unwrap();
        assert!(home.join(".claude/settings.json").is_file());
        assert!(home.join(".claude.json").is_file());
        assert!(!home.join(".claude/hooks").exists());
        let e = env(ModelProviderKind::ClaudeCode, &home);
        assert!(e.iter().any(|(k, v)| k == "HOME" && *v == home.as_os_str()));
    }

    #[test]
    fn only_valid_existing_credentials_are_copied_back() {
        let real = tempfile::tempdir().unwrap();
        let turn = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(real.path().join(".codex")).unwrap();
        std::fs::write(real.path().join(".codex/auth.json"), "{\"refresh\":\"old\"}").unwrap();
        std::fs::write(real.path().join(".codex/config.toml"), "model='x'").unwrap();
        let home = turn.path().join("home");
        populate(ModelProviderKind::Codex, real.path(), &home).unwrap();
        // The CLI rotates its token and an agent edits the config.
        std::fs::write(home.join(".codex/auth.json"), "{\"refresh\":\"new\"}").unwrap();
        std::fs::write(home.join(".codex/config.toml"), "[mcp_servers.evil]\ncommand='sh'").unwrap();
        copy_back(ModelProviderKind::Codex, real.path(), &home);
        assert_eq!(std::fs::read_to_string(real.path().join(".codex/auth.json")).unwrap(), "{\"refresh\":\"new\"}");
        assert_eq!(std::fs::read_to_string(real.path().join(".codex/config.toml")).unwrap(), "model='x'");
        // Not JSON: not copied back.
        std::fs::write(home.join(".codex/auth.json"), "#!/bin/sh").unwrap();
        copy_back(ModelProviderKind::Codex, real.path(), &home);
        assert_eq!(std::fs::read_to_string(real.path().join(".codex/auth.json")).unwrap(), "{\"refresh\":\"new\"}");
    }

    /// The host copies back with its own (unsandboxed) reads, so a credential
    /// file the agent swapped for a symlink or a hard link to a file the
    /// sandbox denies it (another tool's JSON credentials, Ligus's data) would
    /// be laundered into the CLI's real sign-in file and handed to the agent
    /// by the next turn's home.
    #[cfg(unix)]
    #[test]
    fn copy_back_never_follows_links_the_agent_planted() {
        let real = tempfile::tempdir().unwrap();
        let turn = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(real.path().join(".codex")).unwrap();
        std::fs::write(real.path().join(".codex/auth.json"), "{\"refresh\":\"old\"}").unwrap();
        let secret = real.path().join("other-tool-creds.json");
        std::fs::write(&secret, "{\"token\":\"not-for-agents\"}").unwrap();
        let home = turn.path().join("home");
        populate(ModelProviderKind::Codex, real.path(), &home).unwrap();
        let planted = home.join(".codex/auth.json");
        std::fs::remove_file(&planted).unwrap();
        std::os::unix::fs::symlink(&secret, &planted).unwrap();
        copy_back(ModelProviderKind::Codex, real.path(), &home);
        assert_eq!(std::fs::read_to_string(real.path().join(".codex/auth.json")).unwrap(), "{\"refresh\":\"old\"}", "symlink followed");
        std::fs::remove_file(&planted).unwrap();
        std::fs::hard_link(&secret, &planted).unwrap();
        copy_back(ModelProviderKind::Codex, real.path(), &home);
        assert_eq!(std::fs::read_to_string(real.path().join(".codex/auth.json")).unwrap(), "{\"refresh\":\"old\"}", "hard link followed");
        // A symlinked config dir in the turn home is refused too.
        std::fs::remove_file(&planted).unwrap();
        std::fs::remove_dir_all(home.join(".codex")).unwrap();
        let elsewhere = real.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("auth.json"), "{\"token\":\"not-for-agents\"}").unwrap();
        std::os::unix::fs::symlink(&elsewhere, home.join(".codex")).unwrap();
        copy_back(ModelProviderKind::Codex, real.path(), &home);
        assert_eq!(std::fs::read_to_string(real.path().join(".codex/auth.json")).unwrap(), "{\"refresh\":\"old\"}", "symlinked dir followed");
    }
}
