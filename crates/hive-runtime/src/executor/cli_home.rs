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
//!   still parse as a JSON object with the same keys and name the same
//!   account, [`same_sign_in`]), so a rotated refresh token isn't lost and
//!   an agent can't sign the user's CLI into another account.
//! - macOS: the login keychain is reached through symlinks to the real
//!   `~/Library/Keychains` and the security preferences (the keychain is a
//!   service, not a file the agent can write through the profile).
//!
//! `LIGUS_CLI_HOME=shared` (device-local) turns this off and lets the CLIs
//! write their real state dirs, as under Hive 1.x.

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

/// Claude Code's credentials file (relative to home).
const CLAUDE_CREDENTIALS: &str = ".claude/.credentials.json";

/// The login-keychain item Claude Code keeps its sign-in in (with no
/// `CLAUDE_CONFIG_DIR`; a custom config dir names another item, which is
/// left alone and keeps the keychain reachable for that turn).
#[cfg(target_os = "macos")]
const CLAUDE_KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// Whether `real_home` is this user's own home with Claude Code's default
/// config dir (the only case whose keychain item is bridged).
#[cfg(target_os = "macos")]
fn is_user_home(real_home: &Path) -> bool {
    std::env::var_os("CLAUDE_CONFIG_DIR").is_none_or(|v| v.is_empty())
        && super::home_dir().is_some_and(|h| super::canonical(&h) == super::canonical(real_home))
}

fn is_json_object(b: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(b).is_ok_and(|v| v.is_object())
}

/// Whether `kind`'s sign-in is a file in the turn home `home` (so the turn
/// doesn't need the login keychain).
pub fn signed_in_by_file(kind: ModelProviderKind, home: &Path) -> bool {
    let f = match kind {
        ModelProviderKind::ClaudeCode => CLAUDE_CREDENTIALS,
        ModelProviderKind::Codex => ".codex/auth.json",
        _ => return false,
    };
    std::fs::symlink_metadata(home.join(f)).is_ok_and(|m| m.is_file())
}

/// The login keychain through `/usr/bin/security` (the tool Claude Code
/// stores its item with, so the item's ACL trusts it and no prompt shows).
#[cfg(target_os = "macos")]
mod keychain {
    use std::io::Write;
    use std::process::{Command, Stdio};

    const SECURITY: &str = "/usr/bin/security";

    /// The item's account and secret.
    pub fn read(service: &str) -> Option<(String, Vec<u8>)> {
        let attrs = Command::new(SECURITY).args(["find-generic-password", "-s", service]).stderr(Stdio::null()).output().ok()?;
        if !attrs.status.success() {
            return None;
        }
        let acct = account(&String::from_utf8_lossy(&attrs.stdout))?;
        let out = Command::new(SECURITY).args(["find-generic-password", "-s", service, "-a", &acct, "-w"]).stderr(Stdio::null()).output().ok()?;
        if !out.status.success() {
            return None;
        }
        let mut secret = out.stdout;
        while secret.last() == Some(&b'\n') {
            secret.pop();
        }
        Some((acct, secret))
    }

    /// The `acct` attribute of `security find-generic-password` output.
    pub fn account(attrs: &str) -> Option<String> {
        let line = attrs.lines().find(|l| l.trim_start().starts_with("\"acct\"<blob>=\""))?;
        let v = line.trim().strip_prefix("\"acct\"<blob>=\"")?.strip_suffix('"')?;
        (!v.is_empty() && !v.contains(['"', '\\', '\n'])).then(|| v.to_string())
    }

    /// The `security -i` command that updates the item (the secret as hex,
    /// so it never sits on a command line).
    pub fn update_command(service: &str, acct: &str, secret: &[u8]) -> Option<String> {
        if [service, acct].iter().any(|s| s.contains(['"', '\\', '\n'])) {
            return None;
        }
        let hex: String = secret.iter().map(|b| format!("{b:02x}")).collect();
        Some(format!("add-generic-password -U -a \"{acct}\" -s \"{service}\" -X {hex}\n"))
    }

    pub fn write(service: &str, acct: &str, secret: &[u8]) -> bool {
        let Some(cmd) = update_command(service, acct, secret) else { return false };
        let Ok(mut child) = Command::new(SECURITY).arg("-i").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
            return false;
        };
        let ok = child.stdin.take().is_some_and(|mut i| i.write_all(cmd.as_bytes()).is_ok());
        child.wait().is_ok_and(|s| s.success()) && ok
    }
}

/// Claude Code subdirectories a workspace that opted into the user's own
/// setup reaches (read-only: symlinks to the real dirs, which the profile
/// keeps unwritable).
pub const CLAUDE_USER_DIRS: &[&str] = &["plugins", "skills", "agents", "commands"];

/// The git identity (`user.name`, `user.email`) from the user's global git
/// config, read by the host: the only part of `~/.gitconfig` a turn home
/// gets. The rest can name credential helpers, URL rewrites that embed
/// tokens, and includes of other files, so it is never copied or readable.
pub fn git_identity(real_home: &Path) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for key in ["user.name", "user.email"] {
        let o = hive_core::process_util::command("git")
            .args(["config", "--global", "--get", key])
            .env("HOME", real_home)
            .env_remove("GIT_CONFIG_GLOBAL")
            .env_remove("XDG_CONFIG_HOME")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output();
        if let Some(o) = o.ok().filter(|o| o.status.success()) {
            let v = String::from_utf8_lossy(&o.stdout).trim().to_string();
            // One plain line: nothing that could close the value and open
            // another section.
            if !v.is_empty() && v.len() <= 256 && !v.chars().any(|c| c.is_control() || c == '"' || c == '\\') {
                out.push((key, v));
            }
        }
    }
    out
}

/// Write the git identity into a turn home's `.gitconfig`.
fn write_git_identity(real_home: &Path, home: &Path) {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<PathBuf, Vec<(&'static str, String)>>>> = std::sync::OnceLock::new();
    let ids = {
        let c = CACHE.get_or_init(Default::default);
        let mut m = c.lock().unwrap_or_else(|e| e.into_inner());
        m.entry(real_home.to_path_buf()).or_insert_with(|| git_identity(real_home)).clone()
    };
    if ids.is_empty() {
        return;
    }
    let mut text = String::from("[user]\n");
    for (k, v) in ids {
        text.push_str(&format!("\t{} = \"{}\"\n", k.trim_start_matches("user."), v));
    }
    let _ = crate::private_file::write_private_atomic(&home.join(".gitconfig"), text.as_bytes());
}

/// The turn home of a child that isn't a CLI agent (a command, a pre-flight
/// test, a stdio MCP server): empty but for the git identity.
pub fn populate_plain(real_home: &Path, home: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(home)?;
    write_git_identity(real_home, home);
    Ok(())
}

/// Build the turn home for `kind` at `home` from `real_home`.
pub fn populate(kind: ModelProviderKind, real_home: &Path, home: &Path) -> std::io::Result<()> {
    populate_with(kind, real_home, home, false)
}

/// [`populate`]; `user_config` = the workspace opted its CLI agents into the
/// user's own setup (Claude Code's plugins, skills, agents and commands are
/// then linked in).
pub fn populate_with(kind: ModelProviderKind, real_home: &Path, home: &Path, user_config: bool) -> std::io::Result<()> {
    std::fs::create_dir_all(home)?;
    write_git_identity(real_home, home);
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
    // macOS: Claude Code keeps its sign-in in the login keychain. Bridge it
    // into the turn home as the credentials file Claude Code also reads, so
    // the turn runs without the keychain (which would hand it every item
    // that trusts `/usr/bin/security`; `EnvelopeEnforcer::plan`).
    #[cfg(target_os = "macos")]
    if kind == ModelProviderKind::ClaudeCode && is_user_home(real_home) {
        let dst = home.join(CLAUDE_CREDENTIALS);
        if !dst.exists() {
            if let Some((_, secret)) = keychain::read(CLAUDE_KEYCHAIN_SERVICE).filter(|(_, s)| is_json_object(s)) {
                let _ = crate::private_file::write_private_atomic(&dst, &secret);
            }
        }
    }
    if kind == ModelProviderKind::ClaudeCode {
        harden_claude_settings(&home.join(".claude/settings.json"));
        #[cfg(unix)]
        if user_config {
            for d in CLAUDE_USER_DIRS {
                let src = real_path(kind, real_home, &format!(".claude/{d}"));
                if src.is_dir() {
                    let _ = std::os::unix::fs::symlink(&src, home.join(".claude").join(d));
                }
            }
        }
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

/// The turn home's copy of Claude Code's user settings, with the tools that
/// schedule or run work outside the turn denied
/// ([`crate::provider::claude_code::OUT_OF_TURN_TOOLS`]). The turn's own
/// `claude` already has them off by flag; this holds them off for a
/// `claude` the agent starts from its shell, which reads these settings.
/// A file that isn't a JSON object is replaced.
fn harden_claude_settings(path: &Path) {
    use serde_json::{json, Value};
    let mut v: Value = std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).filter(Value::is_object).unwrap_or_else(|| json!({}));
    let deny = v
        .as_object_mut()
        .map(|m| m.entry("permissions").or_insert_with(|| json!({})))
        .filter(|p| p.is_object())
        .and_then(|p| p.as_object_mut())
        .map(|p| p.entry("deny").or_insert_with(|| json!([])));
    let Some(deny) = deny else { return };
    if !deny.is_array() {
        *deny = json!([]);
    }
    let arr = deny.as_array_mut().expect("array");
    for t in crate::provider::claude_code::OUT_OF_TURN_TOOLS {
        if !arr.iter().any(|x| x.as_str() == Some(t)) {
            arr.push(json!(t));
        }
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = crate::private_file::write_private_atomic(path, v.to_string().as_bytes());
}

/// `HOME` and the XDG dirs pointing into a turn home.
pub fn home_env(home: &Path) -> Vec<(OsString, OsString)> {
    let mut v: Vec<(OsString, OsString)> = vec![("HOME".into(), home.as_os_str().to_owned())];
    for (k, rel) in [("XDG_CONFIG_HOME", ".config"), ("XDG_DATA_HOME", ".local/share"), ("XDG_STATE_HOME", ".local/state"), ("XDG_CACHE_HOME", ".cache")] {
        v.push((k.into(), home.join(rel).into_os_string()));
    }
    v
}

/// The env a CLI child gets for its turn home: `HOME`, the XDG dirs, and
/// the CLI's config-dir variable when the user had set one.
pub fn env(kind: ModelProviderKind, home: &Path) -> Vec<(OsString, OsString)> {
    let mut v = home_env(home);
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

/// The claims of a JWT (`header.payload.signature`), unverified: only used
/// to compare who a refreshed token names with who the old one named.
fn jwt_claims(token: &str) -> Option<serde_json::Value> {
    use base64::Engine;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The fields of a credential file that say whose sign-in it is, by CLI
/// and file: account ids, emails, user names, orgs, and the identity claims
/// of an embedded `id_token`. Each is (where, value); a field the file
/// doesn't have is left out.
fn identity_fields(v: &serde_json::Value) -> Vec<(String, serde_json::Value)> {
    use serde_json::Value;
    const KEYS: [&str; 16] = [
        // Claude Code's file names no account; its plan is the closest
        // thing to one.
        "subscriptionType",
        // A key that replaces the sign-in sends the work to its owner.
        "OPENAI_API_KEY",
        "apiKey",
        "api_key",
        "account_id",
        "accountId",
        "accountUuid",
        "email",
        "emailAddress",
        "user",
        "login",
        "organizationUuid",
        "org_id",
        "orgId",
        "active",
        "sub",
    ];
    const CLAIMS: [&str; 4] = ["email", "sub", "org_id", "https://api.openai.com/auth"];
    fn walk(v: &Value, path: &str, out: &mut Vec<(String, Value)>) {
        let Value::Object(m) = v else { return };
        for (k, x) in m {
            let p = format!("{path}/{k}");
            if KEYS.contains(&k.as_str()) && !x.is_object() {
                out.push((p.clone(), x.clone()));
            }
            if k == "id_token" {
                if let Some(c) = x.as_str().and_then(jwt_claims) {
                    for claim in CLAIMS {
                        if let Some(val) = c.get(claim) {
                            out.push((format!("{p}#{claim}"), val.clone()));
                        }
                    }
                }
            }
            walk(x, &p, out);
        }
    }
    let mut out = Vec::new();
    walk(v, "", &mut out);
    out
}

/// Whether `new` is a refresh of the sign-in in `old`: the same top-level
/// keys (and the same keys one level down, so a file can't grow an
/// `apiKeyHelper` or a second host), and every identity field the old file
/// had present with the same value.
pub fn same_sign_in(old: &serde_json::Value, new: &serde_json::Value) -> Result<(), &'static str> {
    use serde_json::Value;
    let keys = |v: &Value| -> Option<Vec<(String, Vec<String>)>> {
        let m = v.as_object()?;
        let mut ks: Vec<(String, Vec<String>)> = m
            .iter()
            .map(|(k, x)| {
                let mut inner: Vec<String> = x.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
                inner.sort();
                (k.clone(), inner)
            })
            .collect();
        ks.sort();
        Some(ks)
    };
    match (keys(old), keys(new)) {
        (Some(a), Some(b)) if a == b => {}
        _ => return Err("its keys changed"),
    }
    let now = identity_fields(new);
    for (path, val) in identity_fields(old) {
        if !now.iter().any(|(p, v)| *p == path && *v == val) {
            return Err("it names a different account");
        }
    }
    Ok(())
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
        let Some(new_v) = serde_json::from_slice::<serde_json::Value>(&new).ok().filter(|v| v.is_object()) else { continue };
        // Only a file the user already had (a sign-in this turn didn't make),
        // and only a refresh of that same sign-in: an agent can write a valid
        // sign-in for another account into its turn home, and copying it
        // back would sign the user's CLI into that account.
        #[cfg(target_os = "macos")]
        if old.is_none() && kind == ModelProviderKind::ClaudeCode && *f == CLAUDE_CREDENTIALS && is_user_home(real_home) {
            // Bridged in from the keychain (`populate_with`): a refresh of
            // that same sign-in goes back there.
            if let Some((acct, kc)) = keychain::read(CLAUDE_KEYCHAIN_SERVICE) {
                let kc_v = serde_json::from_slice::<serde_json::Value>(&kc).unwrap_or(serde_json::Value::Null);
                match same_sign_in(&kc_v, &new_v) {
                    Ok(()) if kc != new => {
                        if !keychain::write(CLAUDE_KEYCHAIN_SERVICE, &acct, &new) {
                            tracing::warn!(target: "sandbox", "couldn't write a refreshed Claude Code sign-in back to the keychain");
                        }
                    }
                    Ok(()) => {}
                    Err(why) => tracing::warn!(target: "sandbox", cli = ?kind, "not copying back a changed CLI sign-in: {why}"),
                }
            }
            continue;
        }
        let Some(old) = old else { continue };
        let old_v = serde_json::from_slice::<serde_json::Value>(&old).unwrap_or(serde_json::Value::Null);
        if let Err(why) = same_sign_in(&old_v, &new_v) {
            tracing::warn!(target: "sandbox", cli = ?kind, file = %f, "not copying back a changed CLI sign-in: {why}");
            continue;
        }
        let _ = crate::private_file::write_private_atomic(&real, &new);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn keychain_bridge_parses_and_quotes() {
        let attrs = "keychain: \"/x\"\nattributes:\n    \"acct\"<blob>=\"me\"\n    \"svce\"<blob>=\"Claude Code-credentials\"\n";
        assert_eq!(keychain::account(attrs).as_deref(), Some("me"));
        assert_eq!(keychain::account("    \"acct\"<blob>=<NULL>\n"), None);
        let cmd = keychain::update_command("Claude Code-credentials", "me", b"{\"a\":\" \"}").unwrap();
        assert_eq!(cmd, "add-generic-password -U -a \"me\" -s \"Claude Code-credentials\" -X 7b2261223a2220227d\n");
        assert!(keychain::update_command("s", "a\" -s x", b"{}").is_none());
    }

    #[test]
    fn signed_in_by_file_needs_the_cli_sign_in_file() {
        let t = tempfile::tempdir().unwrap();
        assert!(!signed_in_by_file(ModelProviderKind::ClaudeCode, t.path()));
        std::fs::create_dir_all(t.path().join(".claude")).unwrap();
        std::fs::write(t.path().join(".claude/.credentials.json"), "{}").unwrap();
        assert!(signed_in_by_file(ModelProviderKind::ClaudeCode, t.path()));
        assert!(!signed_in_by_file(ModelProviderKind::Copilot, t.path()));
    }

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
        // The settings copy denies the out-of-turn tools (a nested `claude`
        // from the agent's shell reads it).
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(home.join(".claude/settings.json")).unwrap()).unwrap();
        let deny: Vec<&str> = v["permissions"]["deny"].as_array().unwrap().iter().filter_map(|x| x.as_str()).collect();
        assert!(deny.contains(&"RemoteTrigger") && deny.contains(&"CronCreate"), "{deny:?}");
        let e = env(ModelProviderKind::ClaudeCode, &home);
        assert!(e.iter().any(|(k, v)| k == "HOME" && *v == home.as_os_str()));
    }

    /// The user's plugins and skills reach a turn only with the workspace's
    /// opt-in, and then as links to the real (unwritable) dirs.
    #[cfg(unix)]
    #[test]
    fn user_plugin_dirs_only_with_opt_in() {
        let real = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(real.path().join(".claude/plugins")).unwrap();
        std::fs::create_dir_all(real.path().join(".claude/skills/s")).unwrap();
        std::fs::write(real.path().join(".claude/settings.json"), r#"{"permissions":{"deny":["Bash(rm *)"]},"model":"x"}"#).unwrap();
        let turn = tempfile::tempdir().unwrap();
        let off = turn.path().join("off");
        populate(ModelProviderKind::ClaudeCode, real.path(), &off).unwrap();
        assert!(!off.join(".claude/plugins").exists() && !off.join(".claude/skills").exists());
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(off.join(".claude/settings.json")).unwrap()).unwrap();
        assert_eq!(v["model"], "x");
        assert_eq!(v["permissions"]["deny"][0], "Bash(rm *)");
        let on = turn.path().join("on");
        populate_with(ModelProviderKind::ClaudeCode, real.path(), &on, true).unwrap();
        assert!(std::fs::symlink_metadata(on.join(".claude/plugins")).unwrap().file_type().is_symlink());
        assert!(on.join(".claude/skills/s").is_dir());
    }

    fn jwt(claims: serde_json::Value) -> String {
        use base64::Engine;
        let e = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!("{}.{}.sig", e.encode(b"{\"alg\":\"none\"}"), e.encode(claims.to_string()))
    }

    /// Per first-class CLI: (credential file, the user's sign-in, a refresh
    /// of it, the same shape signed in as someone else, a shape with keys
    /// added or removed).
    fn shapes(kind: ModelProviderKind) -> (&'static str, serde_json::Value, serde_json::Value, serde_json::Value, serde_json::Value) {
        use serde_json::json;
        match kind {
            // No identity field in the file: the key sets must match.
            ModelProviderKind::ClaudeCode => (
                ".claude/.credentials.json",
                json!({"claudeAiOauth": {"accessToken": "a1", "refreshToken": "r1", "expiresAt": 1, "scopes": ["user:inference"], "subscriptionType": "max"}}),
                json!({"claudeAiOauth": {"accessToken": "a2", "refreshToken": "r2", "expiresAt": 2, "scopes": ["user:inference"], "subscriptionType": "max"}}),
                json!({"claudeAiOauth": {"accessToken": "a3", "refreshToken": "r3", "expiresAt": 3, "scopes": ["user:inference"], "subscriptionType": "pro"}}),
                json!({"claudeAiOauth": {"accessToken": "a3", "refreshToken": "r3"}, "apiKeyHelper": "sh -c evil"}),
            ),
            ModelProviderKind::Codex => (
                ".codex/auth.json",
                json!({"OPENAI_API_KEY": null, "tokens": {"id_token": jwt(json!({"email": "me@example.com"})), "access_token": "a1", "refresh_token": "r1", "account_id": "acct-me"}, "last_refresh": "t1"}),
                json!({"OPENAI_API_KEY": null, "tokens": {"id_token": jwt(json!({"email": "me@example.com"})), "access_token": "a2", "refresh_token": "r2", "account_id": "acct-me"}, "last_refresh": "t2"}),
                json!({"OPENAI_API_KEY": null, "tokens": {"id_token": jwt(json!({"email": "attacker@example.com"})), "access_token": "a3", "refresh_token": "r3", "account_id": "acct-attacker"}, "last_refresh": "t3"}),
                json!({"OPENAI_API_KEY": "sk-attacker", "tokens": {"access_token": "a3"}}),
            ),
            ModelProviderKind::Copilot => (
                ".config/github-copilot/hosts.json",
                json!({"github.com": {"user": "me", "oauth_token": "gho_1"}}),
                json!({"github.com": {"user": "me", "oauth_token": "gho_2"}}),
                json!({"github.com": {"user": "attacker", "oauth_token": "gho_3"}}),
                json!({"github.com": {"user": "me", "oauth_token": "gho_3"}, "ghe.attacker.example": {"user": "x", "oauth_token": "y"}}),
            ),
            ModelProviderKind::Antigravity => (
                ".gemini/google_accounts.json",
                json!({"active": "me@example.com", "old": []}),
                json!({"active": "me@example.com", "old": ["prev@example.com"]}),
                json!({"active": "attacker@example.com", "old": []}),
                json!({"active": "me@example.com"}),
            ),
            _ => unreachable!(),
        }
    }

    /// An agent can write a valid sign-in for a different account into its
    /// turn home; copying that back would sign the user's CLI into the
    /// attacker's account (the next session's work, prompts and code go
    /// there). Only a refresh of the same identity, with the same keys, is
    /// copied back.
    #[test]
    fn copy_back_rejects_account_swap() {
        let oauth = |sub: &str| {
            serde_json::json!({"access_token": "a", "refresh_token": "r", "scope": "s", "token_type": "Bearer", "id_token": jwt(serde_json::json!({"email": sub, "sub": sub})), "expiry_date": 1})
        };
        let mut cases: Vec<(ModelProviderKind, &str, serde_json::Value, serde_json::Value, serde_json::Value, serde_json::Value)> =
            [ModelProviderKind::ClaudeCode, ModelProviderKind::Codex, ModelProviderKind::Copilot, ModelProviderKind::Antigravity]
                .into_iter()
                .map(|k| {
                    let (f, a, b, c, d) = shapes(k);
                    (k, f, a, b, c, d)
                })
                .collect();
        cases.push((
            ModelProviderKind::Antigravity,
            ".gemini/oauth_creds.json",
            oauth("me@example.com"),
            serde_json::json!({"access_token": "a2", "refresh_token": "r2", "scope": "s", "token_type": "Bearer", "id_token": jwt(serde_json::json!({"email": "me@example.com", "sub": "me@example.com"})), "expiry_date": 2}),
            oauth("attacker@example.com"),
            serde_json::json!({"access_token": "a2", "refresh_token": "r2"}),
        ));
        cases.push((
            ModelProviderKind::Copilot,
            ".config/github-copilot/apps.json",
            serde_json::json!({"github.com:Iv1.abc": {"user": "me", "oauth_token": "gho_1", "githubAppId": "Iv1.abc"}}),
            serde_json::json!({"github.com:Iv1.abc": {"user": "me", "oauth_token": "gho_2", "githubAppId": "Iv1.abc"}}),
            serde_json::json!({"github.com:Iv1.abc": {"user": "attacker", "oauth_token": "gho_3", "githubAppId": "Iv1.abc"}}),
            serde_json::json!({"github.com:Iv1.xyz": {"user": "me", "oauth_token": "gho_3", "githubAppId": "Iv1.xyz"}}),
        ));
        for (kind, file, mine, refreshed, swapped, reshaped) in cases {
            let real = tempfile::tempdir().unwrap();
            let turn = tempfile::tempdir().unwrap();
            let real_file = real.path().join(file);
            std::fs::create_dir_all(real_file.parent().unwrap()).unwrap();
            std::fs::write(&real_file, mine.to_string()).unwrap();
            let home = turn.path().join("home");
            populate(kind, real.path(), &home).unwrap();
            let read = || serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&real_file).unwrap()).unwrap();
            for bad in [&swapped, &reshaped] {
                std::fs::write(home.join(file), bad.to_string()).unwrap();
                copy_back(kind, real.path(), &home);
                assert_eq!(read(), mine, "{kind:?} {file}: copied back {bad}");
            }
            std::fs::write(home.join(file), refreshed.to_string()).unwrap();
            copy_back(kind, real.path(), &home);
            assert_eq!(read(), refreshed, "{kind:?} {file}: a same-account refresh is kept");
        }
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
