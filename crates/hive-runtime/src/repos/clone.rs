//! Cloning a workspace repo by its id (repos-and-checkouts 4.3, review
//! F12, F13, F11c). The URL is never taken from an event as a string: only
//! the canonical's host and path, re-validated, rebuilt as `https://`, and
//! only for a host this device has a forge connection to (the caller looks
//! it up). Submodules never recurse; a `--reference` to another local
//! checkout always dissociates, so deleting or gc-ing that checkout never
//! corrupts this clone.

use std::path::Path;
use std::process::Command;

use hive_core::workspace_repo::{split_canonical, WorkspaceRepo};
use hive_forge::RepoRef;

/// The forge host and repository a clone by id targets: the canonical's
/// parts, re-validated as a strict `https://host/owner/name` spec. A
/// `local:` repo can't be cloned.
pub fn clone_target(repo: &WorkspaceRepo) -> Result<(String, RepoRef), String> {
    let (host, _) = split_canonical(&repo.canonical).ok_or_else(|| format!("{} has no forge to clone it from", repo.display_name))?;
    let spec = hive_forge::spec::parse_repo_spec(&format!("https://{}", repo.canonical), true).map_err(|e| e.to_string())?;
    if spec.host.as_deref() != Some(host) || !hive_forge::spec::valid_host(host) {
        return Err("that repository's host isn't valid".into());
    }
    Ok((host.to_string(), spec.repo))
}

/// The `git clone` for `url` into `dest`: hardened git, the optional DNS
/// pin (`http.curloptResolve`, from `hive_forge::conn::git_resolve_pin`),
/// `--reference-if-able <other checkout> --dissociate` when one is given,
/// no submodules, `--` before the URL. The caller adds credentials (the
/// connection's helper, never argv or env) and runs it.
pub fn clone_command(url: &str, dest: &Path, reference: Option<&Path>, pin: Option<&str>, auth: &[String]) -> Command {
    let mut c = hive_core::process_util::git();
    if let Some(p) = pin {
        c.arg("-c").arg(format!("http.curloptResolve={p}"));
    }
    c.args(auth);
    c.args(["clone", "--progress", "--no-recurse-submodules"]);
    if let Some(r) = reference {
        c.arg("--reference-if-able").arg(r).arg("--dissociate");
    }
    c.arg("--").arg(url).arg(dest);
    c
}

/// Run [`clone_command`] to completion (no pin, no credentials): a clone
/// from a source this device names itself, such as a local mirror.
pub fn clone_local_reference(url: &str, dest: &Path, reference: Option<&Path>) -> Result<(), String> {
    let out = clone_command(url, dest, reference, None, &[]).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// After a clone by repo id: make `origin`'s URL the repo's identity
/// (`https://<canonical>.git`), so the checkout matches it by remote. When
/// the clone came from another URL, a local `insteadOf` keeps git fetching
/// from there; identity never follows `insteadOf`.
pub fn set_identity_origin(dest: &Path, repo: &WorkspaceRepo) -> Result<(), String> {
    if repo.canonical.starts_with("local:") {
        return Ok(());
    }
    let identity = format!("https://{}.git", repo.canonical);
    let current = super::git::git(dest, &["remote", "get-url", "origin"])?;
    let aliases = Default::default();
    if hive_core::workspace_repo::canonical_repo(&current, &repo.forge_kind, &aliases).is_ok_and(|c| c == repo.canonical) {
        return Ok(());
    }
    super::git::git(dest, &["config", &format!("url.{current}.insteadOf"), &identity])?;
    super::git::git(dest, &["remote", "set-url", "origin", &identity])?;
    Ok(())
}

/// After a clone: whether the repo entry's claimed root commit is in it
/// (review F10: membership, never a recomputation). `None` when the entry
/// claims none.
pub fn verify_claimed_root(dest: &Path, repo: &WorkspaceRepo) -> Option<bool> {
    repo.root_commit.as_deref().map(|r| super::git::has_root(dest, r))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::workspace_repo::repo_id;

    fn r(c: &str) -> WorkspaceRepo {
        WorkspaceRepo { id: repo_id(c), canonical: c.into(), display_name: "R".into(), forge_kind: String::new(), default_branch: String::new(), root_commit: None }
    }

    #[test]
    fn clone_url_rebuilt_never_from_event_string() {
        let (h, repo) = clone_target(&r("gitlab.example.com/grp/sub/proj")).unwrap();
        assert_eq!((h.as_str(), repo.full_name().as_str()), ("gitlab.example.com", "grp/sub/proj"));
        // A canonical that isn't a strict spec (the fold refuses these too).
        for bad in ["local:0123456789abcdef0123456789abcdef01234567", "h.example/-x/y", "h.example/a/..", "h.example/a b/c"] {
            assert!(clone_target(&r(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn ext_and_file_protocols_refused_and_submodules_never_recurse() {
        let c = clone_command("https://h.example/o/n", Path::new("/tmp/x"), Some(Path::new("/src")), Some("h.example:443:93.184.216.34"), &[]);
        let args: Vec<String> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        let dash = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(args[dash + 1], "https://h.example/o/n");
        assert!(args.contains(&"--no-recurse-submodules".to_string()) && args.contains(&"--dissociate".to_string()));
        assert!(args.contains(&"http.curloptResolve=h.example:443:93.184.216.34".to_string()));
        let envs: Vec<String> = c.get_envs().filter_map(|(_, v)| v.map(|v| v.to_string_lossy().into_owned())).collect();
        assert!(envs.iter().any(|v| v == "never"), "protocol.ext.allow=never");
        // A git that tries an ext:: URL refuses it.
        let t = tempfile::tempdir().unwrap();
        let e = clone_local_reference("ext::sh -c touch% /tmp/ligus-pwned", &t.path().join("d"), None);
        assert!(e.is_err());
        assert!(!Path::new("/tmp/ligus-pwned").exists());
    }
}
