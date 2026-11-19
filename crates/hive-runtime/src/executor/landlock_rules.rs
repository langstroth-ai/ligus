//! The Landlock tier's rules (Linux without usable user namespaces).
//!
//! Without bubblewrap there are no mounts to hide paths behind, and
//! Landlock is allow-only: a rule on a directory grants its whole subtree,
//! and a narrower rule can never take a right away. So a deny becomes a
//! **carve**: every directory on the way down to a denied path is replaced
//! by rules on its entries, leaving the denied path (and the directories
//! above it) without a rule.
//!
//! - **Read**: `/`, carved around the plan's denied paths (Ligus's data dir,
//!   `serve.token`, `~/.ssh` and the other credential dirs), the session-bus
//!   dir (`$XDG_RUNTIME_DIR`) and, when the CLI runs with a per-turn home,
//!   the real CLI config dirs (`~/.claude`, `~/.codex`, …): the turn sees
//!   the copies in its own home. Directories above a denied path can be
//!   traversed but not listed. Paths the plan allows inside a denied one
//!   (the worktree, a worker's repos dir) get rules of their own.
//! - **Write**: the worktree, carved around `.git` and the protected paths.
//!   Its top-level directory carries no write rule, because any right on it
//!   would reach `.git` too: existing top-level files and directories are
//!   writable, but nothing can be created, removed or renamed directly in
//!   the worktree's top level. Plus the turn's scratch dir (and its per-turn
//!   home), carved around denied paths.
//! - **Devices**: `/dev/null`, `/dev/zero`, `/dev/full`, `/dev/random`,
//!   `/dev/urandom` only (no `/dev/pts`: those are the user's terminals).
//! - **Network** (Landlock ABI 4+): TCP connect only to the turn's egress
//!   proxy, the Ligus MCP listener and allow-listed loopback endpoints; no
//!   TCP bind. Landlock matches the **port only**, not the address.
//!
//! The rules are computed here, in the parent, and handed to the helper in
//! a file in the turn's 0700 scratch dir (never on its command line, where
//! every local user could read the paths).
//!
//! Symlinks are never followed while carving (a link inside a granted
//! directory resolves to its target, which Landlock checks on its own).

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use super::{canonical, EgressPlan, EnforcementPlan};

/// What the helper enforces in the Landlock tier.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LandlockRules {
    /// Read and execute (directories: and list).
    pub read: Vec<PathBuf>,
    /// Every filesystem right.
    pub write: Vec<PathBuf>,
    /// Character devices: read, write, ioctl.
    pub dev: Vec<PathBuf>,
    /// Whether TCP is restricted (every plan but an open-egress MCP server).
    pub restrict_net: bool,
    /// TCP ports the child may connect to (when `restrict_net`).
    pub connect: Vec<u16>,
}

/// Rule kinds in the rules file.
const K_READ: &str = "r";
const K_WRITE: &str = "w";
const K_DEV: &str = "d";
const K_PORT: &str = "p";
const K_NET: &str = "n";

/// Rules on the entries of `root`, leaving out `exclude` (canonical paths)
/// and the directories above them. Never follows a symlink.
pub fn carve(root: &Path, exclude: &[PathBuf]) -> Vec<PathBuf> {
    let root = canonical(root);
    let mut out = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut work = vec![root];
    while let Some(p) = work.pop() {
        if exclude.iter().any(|d| p.starts_with(d)) {
            continue;
        }
        if exclude.iter().any(|d| d.starts_with(&p) && *d != p) {
            // Above a denied path: its entries get rules, it doesn't.
            if !seen.insert(p.clone()) {
                continue;
            }
            if let Ok(rd) = std::fs::read_dir(&p) {
                for e in rd.flatten() {
                    if e.file_type().is_ok_and(|t| t.is_symlink()) {
                        continue;
                    }
                    work.push(p.join(e.file_name()));
                }
            }
            continue;
        }
        if std::fs::symlink_metadata(&p).is_ok_and(|m| !m.file_type().is_symlink()) {
            out.push(p);
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Config dirs and files of every CLI under the real `home` (denied to a
/// CLI that runs with a per-turn home, which holds the copies it needs).
pub fn cli_config_paths(home: &Path) -> Vec<PathBuf> {
    use hive_core::ModelProviderKind as K;
    let mut v = Vec::new();
    for k in [K::ClaudeCode, K::Codex, K::Copilot, K::Antigravity, K::Aider, K::Pi, K::Hermes] {
        v.extend(super::runtime_state_dirs(k, home));
        v.extend(super::cli_home::config_files(k).iter().map(|f| home.join(f)));
    }
    v
}

/// CLI install dirs inside a config dir, readable even when the config dir
/// is denied (Claude Code's local install keeps its binary there).
fn cli_install_dirs(home: &Path) -> Vec<PathBuf> {
    vec![home.join(".claude/local")]
}

/// Whether the plan's `HOME` is a per-turn home (inside the turn's scratch
/// dir), so the real CLI config isn't needed.
pub fn has_turn_home(plan: &EnforcementPlan) -> bool {
    plan.env.iter().any(|(k, v)| k == "HOME" && Path::new(v).starts_with(&plan.tmp))
}

/// The Landlock tier's rules for `plan`. `home` is the real home dir;
/// `proxy_port` the turn's egress proxy, `mcp_port` the Ligus MCP listener.
pub fn rules(plan: &EnforcementPlan, home: Option<&Path>, proxy_port: Option<u16>, mcp_port: Option<u16>) -> LandlockRules {
    let mut deny: Vec<PathBuf> = plan.deny_read.iter().map(|p| canonical(p)).collect();
    if let Some(run) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        deny.push(canonical(Path::new(&run)));
    }
    // Phase C: the home dir is denied as a whole; what the plan lets back in
    // (the worktree, the turn's dirs, the writable extras and `home_read`)
    // gets rules of its own below, carved around the hard denies.
    if let Some(h) = &plan.home {
        deny.push(canonical(h));
    }
    // Other turns' scratch dirs (their homes hold their CLIs' sign-ins):
    // the dir holding them is denied, this turn's own allowed below. Rules
    // name inodes, so a turn dir made later is outside every rule.
    if let Some(root) = super::sibling_turns_root(&plan.tmp) {
        deny.push(root);
    }
    let mut allow: Vec<PathBuf> = plan.allow_read.iter().map(|p| canonical(p)).collect();
    allow.push(canonical(&plan.worktree));
    allow.push(canonical(&plan.tmp));
    // The plan judged these where they resolved; one that resolves
    // elsewhere now (swapped for a symlink since) opens nothing.
    allow.extend(plan.write_extra.iter().filter(|p| super::unmoved(p)).map(|p| canonical(p)));
    allow.extend(plan.home_read.iter().filter(|p| super::unmoved(p)).map(|p| canonical(p)));
    if let Some(h) = home.filter(|_| has_turn_home(plan)) {
        let turn_home = canonical(&plan.tmp);
        deny.extend(cli_config_paths(h).iter().map(|p| canonical(p)).filter(|p| !turn_home.starts_with(p) && !p.starts_with(&turn_home)));
        allow.extend(cli_install_dirs(h).iter().map(|p| canonical(p)));
    }
    deny.sort();
    deny.dedup();

    let mut read = carve(Path::new("/"), &deny);
    for a in allow {
        if a.exists() && !read.iter().any(|r| a.starts_with(r)) {
            read.extend(carve(&a, &deny.iter().filter(|d| !a.starts_with(d)).cloned().collect::<Vec<_>>()));
        }
    }

    let mut write = Vec::new();
    let wt = canonical(&plan.worktree);
    let extra: Vec<PathBuf> = plan.write_extra.iter().filter(|p| super::unmoved(p)).map(|p| canonical(p)).collect();
    // Denies below a writable path are carved out of it; one above it (the
    // home, a data dir holding a worker's checkout) is the plan's to lift.
    let below = |root: &Path| -> Vec<PathBuf> { deny.iter().filter(|d| !root.starts_with(d)).cloned().collect() };
    if plan.worktree_writable && !extra.iter().any(|w| wt.starts_with(w)) {
        let mut protected: Vec<PathBuf> = plan.read_only.iter().map(|p| canonical(p)).collect();
        protected.push(wt.join(".git"));
        protected.extend(below(&wt));
        write.extend(carve(&wt, &protected));
    }
    for w in &extra {
        if w.exists() {
            write.extend(carve(w, &below(w)));
        }
    }
    write.sort();
    write.dedup();

    let dev = ["/dev/null", "/dev/zero", "/dev/full", "/dev/random", "/dev/urandom"].iter().map(PathBuf::from).filter(|p| p.exists()).collect();

    let restrict_net = !matches!(plan.egress, EgressPlan::Open);
    let mut connect = Vec::new();
    if restrict_net {
        connect.extend(proxy_port);
        connect.extend(mcp_port);
        if let EgressPlan::Proxy { allow } = &plan.egress {
            // Loopback endpoints (a local Ollama) bypass the proxy
            // (`NO_PROXY`), so the child connects to them itself.
            connect.extend(allow.iter().filter(|h| h.is_loopback()).map(|h| h.port));
        }
        connect.sort();
        connect.dedup();
    }
    LandlockRules { read, write, dev, restrict_net, connect }
}

impl LandlockRules {
    /// The rules file: `kind \0 value \0` pairs.
    pub fn encode(&self) -> Vec<u8> {
        use std::os::unix::ffi::OsStrExt;
        let mut b = Vec::new();
        let mut push = |k: &str, v: &[u8]| {
            b.extend_from_slice(k.as_bytes());
            b.push(0);
            b.extend_from_slice(v);
            b.push(0);
        };
        for p in &self.read {
            push(K_READ, p.as_os_str().as_bytes());
        }
        for p in &self.write {
            push(K_WRITE, p.as_os_str().as_bytes());
        }
        for p in &self.dev {
            push(K_DEV, p.as_os_str().as_bytes());
        }
        if self.restrict_net {
            push(K_NET, b"1");
        }
        for p in &self.connect {
            push(K_PORT, p.to_string().as_bytes());
        }
        b
    }

    pub fn decode(b: &[u8]) -> io::Result<Self> {
        use std::os::unix::ffi::OsStrExt;
        let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, format!("landlock rules: {m}"));
        let mut r = LandlockRules::default();
        let mut parts = b.split(|c| *c == 0);
        while let Some(k) = parts.next() {
            if k.is_empty() {
                break;
            }
            let v = parts.next().ok_or_else(|| bad("missing value"))?;
            let path = || PathBuf::from(std::ffi::OsStr::from_bytes(v));
            match std::str::from_utf8(k).unwrap_or("") {
                K_READ => r.read.push(path()),
                K_WRITE => r.write.push(path()),
                K_DEV => r.dev.push(path()),
                K_NET => r.restrict_net = true,
                K_PORT => r.connect.push(std::str::from_utf8(v).ok().and_then(|s| s.parse().ok()).ok_or_else(|| bad("port"))?),
                other => return Err(bad(&format!("unknown kind {other:?}"))),
            }
        }
        Ok(r)
    }

    /// Write the rules file (0600) into `dir`.
    pub fn write_file(&self, dir: &Path) -> io::Result<PathBuf> {
        use std::io::Write;
        let path = dir.join(".ligus-landlock-rules");
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        o.open(&path)?.write_all(&self.encode())?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::egress::HostPort;

    fn tree() -> (tempfile::TempDir, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let root = canonical(t.path());
        for d in ["a/secret/inner", "a/pub", "b", "wt/.git/hooks", "wt/src", "wt/.ligus"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        for f in ["a/file", "a/secret/key", "wt/README.md", "wt/ligus.config.toml", "wt/src/main.rs", "wt/.git/config"] {
            std::fs::write(root.join(f), "x").unwrap();
        }
        (t, root)
    }

    #[test]
    fn carve_leaves_out_the_denied_path_and_its_parents() {
        let (_t, root) = tree();
        let got = carve(&root, &[root.join("a/secret")]);
        assert_eq!(got, vec![root.join("a/file"), root.join("a/pub"), root.join("b"), root.join("wt")]);
        // Nothing denied: the root itself.
        assert_eq!(carve(&root, &[]), vec![root.clone()]);
        // A denied path that doesn't exist yet still keeps its parent
        // unruled, so it can't be read once it appears.
        let got = carve(&root, &[root.join("b/later")]);
        assert!(!got.contains(&root.join("b")) && got.contains(&root.join("a")), "{got:?}");
        // The root itself denied: nothing.
        assert!(carve(&root, std::slice::from_ref(&root)).is_empty());
    }

    #[test]
    fn carve_never_follows_symlinks() {
        let (_t, root) = tree();
        std::os::unix::fs::symlink(root.join("a/secret"), root.join("a/alias")).unwrap();
        std::os::unix::fs::symlink(&root, root.join("a/loop")).unwrap();
        let got = carve(&root, &[root.join("a/secret")]);
        assert!(!got.iter().any(|p| p.ends_with("alias") || p.ends_with("loop")), "{got:?}");
        // A symlinked carve root is its target, carved the same way.
        assert!(carve(&root.join("a/alias"), &[root.join("a/secret")]).is_empty());
    }

    fn plan(root: &Path) -> EnforcementPlan {
        let wt = root.join("wt");
        let tmp = root.join("tmp");
        std::fs::create_dir_all(tmp.join("home")).unwrap();
        EnforcementPlan {
            worktree: wt.clone(),
            worktree_writable: true,
            read_only: vec![wt.join(".git"), wt.join(".ligus"), wt.join("ligus.config.toml"), wt.join(".envrc")],
            deny_read: vec![root.join("a/secret")],
            allow_read: vec![wt.clone()],
            home: None,
            home_read: vec![],
            write_extra: vec![tmp.clone()],
            tmp: tmp.clone(),
            egress: EgressPlan::Proxy { allow: vec![HostPort::new("api.anthropic.com", 443), HostPort::new("127.0.0.1", 11434)] },
            env: vec![("HOME".into(), tmp.join("home").into_os_string())],
            args: vec![],
            mcp: vec![],
            ligus_mcp_socket: None,
            gateway: None,
        }
    }

    #[test]
    fn worktree_writes_skip_git_and_protected_paths() {
        let (_t, root) = tree();
        let r = rules(&plan(&root), None, Some(4000), Some(5000));
        let wt = root.join("wt");
        assert!(r.write.contains(&wt.join("src")) && r.write.contains(&wt.join("README.md")), "{:?}", r.write);
        assert!(r.write.contains(&root.join("tmp")), "{:?}", r.write);
        for p in [wt.clone(), wt.join(".git"), wt.join(".ligus"), wt.join("ligus.config.toml")] {
            assert!(!r.write.iter().any(|w| p.starts_with(w)), "{} is writable: {:?}", p.display(), r.write);
        }
        // Reads: never the denied dir, or a dir above it (which would grant
        // it); the worktree is readable.
        assert!(!r.read.iter().any(|p| root.join("a/secret/key").starts_with(p)), "{:?}", r.read);
        assert!(r.read.iter().any(|p| wt.join(".git/config").starts_with(p)));
        assert_eq!(r.connect, vec![4000, 5000, 11434]);
        assert!(r.restrict_net);
        // A read-only turn writes only its scratch dir.
        let mut p = plan(&root);
        p.worktree_writable = false;
        assert!(!rules(&p, None, None, None).write.iter().any(|w| w.starts_with(&wt)));
        // An open-egress MCP server: TCP isn't restricted.
        p.egress = EgressPlan::Open;
        let r = rules(&p, None, None, Some(5000));
        assert!(!r.restrict_net && r.connect.is_empty());
        // Closed: only the Ligus MCP listener.
        p.egress = EgressPlan::Closed;
        assert_eq!(rules(&p, None, None, Some(5000)).connect, vec![5000]);
    }

    #[test]
    fn real_cli_config_is_denied_with_a_turn_home() {
        let (_t, root) = tree();
        let home = root.join("home");
        for d in [".claude/local", ".codex", "projects"] {
            std::fs::create_dir_all(home.join(d)).unwrap();
        }
        std::fs::write(home.join(".claude/.credentials.json"), "{}").unwrap();
        std::fs::write(home.join(".claude.json"), "{}").unwrap();
        let r = rules(&plan(&root), Some(&home), None, None);
        for p in [home.join(".claude/.credentials.json"), home.join(".claude.json"), home.join(".codex")] {
            assert!(!r.read.iter().any(|x| p.starts_with(x)), "{} readable: {:?}", p.display(), r.read);
        }
        assert!(r.read.iter().any(|x| home.join("projects").starts_with(x)));
        assert!(r.read.iter().any(|x| home.join(".claude/local").starts_with(x)), "the local install stays readable");
        // Without a turn home (`LIGUS_CLI_HOME=shared`), the real config is
        // the CLI's own and stays readable.
        let mut p = plan(&root);
        p.env = vec![("HOME".into(), home.clone().into_os_string())];
        let r = rules(&p, Some(&home), None, None);
        assert!(r.read.iter().any(|x| home.join(".claude.json").starts_with(x)));
    }

    #[test]
    fn rules_file_round_trips() {
        let (_t, root) = tree();
        let r = rules(&plan(&root), None, Some(4000), Some(5000));
        assert_eq!(LandlockRules::decode(&r.encode()).unwrap(), r);
        let f = r.write_file(&root).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(r.write_file(&root).is_err(), "never overwrites (create_new)");
        assert!(LandlockRules::decode(b"x\0y\0").is_err());
    }
}
