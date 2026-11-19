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

/// The transport overrides of a clone or fetch by repo id (review F12):
/// the DNS pin (`http.curloptResolve`, from
/// `hive_forge::conn::git_resolve_pin`) when there is one, and never a
/// redirect. git follows a redirect of its first request to any host by
/// default (`http.followRedirects=initial`), resolving that host itself, so
/// a server at the pinned address could send the clone to an internal one
/// and the pin would be moot. A moved repository fails instead; its
/// workspace entry should name the new place anyway.
pub fn by_id_transport_args(pin: Option<&str>) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(p) = pin {
        v.extend(["-c".to_string(), format!("http.curloptResolve={p}")]);
    }
    v.extend(["-c".to_string(), "http.followRedirects=false".to_string()]);
    v
}

/// The `git clone` for `url` into `dest`: hardened git, the optional DNS
/// pin (`http.curloptResolve`, from `hive_forge::conn::git_resolve_pin`),
/// `--reference-if-able <other checkout> --dissociate` when one is given,
/// no submodules, `--` before the URL. The caller adds credentials (the
/// connection's helper, never argv or env) and runs it.
pub fn clone_command(url: &str, dest: &Path, reference: Option<&Path>, pin: Option<&str>, auth: &[String]) -> Command {
    let mut c = hive_core::process_util::git();
    if pin.is_some() {
        c.args(by_id_transport_args(pin));
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

/// The `git clone` of a clone by repo id ([`clone_command`]): never a
/// redirect, whether or not the host's address is pinned
/// ([`by_id_transport_args`]), then the connection's credential args.
pub fn by_id_clone_command(url: &str, dest: &Path, pin: Option<&str>, auth: &[String]) -> Command {
    let mut transport = by_id_transport_args(pin);
    transport.extend(auth.iter().cloned());
    clone_command(url, dest, None, None, &transport)
}

/// After a clone by repo id, on every host: `origin` names the repo's
/// identity ([`set_identity_origin`]), and the entry's claimed root is
/// checked ([`verify_claimed_root`]; `Some(false)` is a warning, never
/// hidden).
pub fn finish_clone(dest: &Path, repo: &WorkspaceRepo) -> Result<Option<bool>, String> {
    set_identity_origin(dest, repo)?;
    Ok(verify_claimed_root(dest, repo))
}

/// A clone by repo id that landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cloned {
    pub path: std::path::PathBuf,
    /// Whether it holds the root commit the repo entry claims (`None`: it
    /// claims none).
    pub root_matches: Option<bool>,
}

/// Clone workspace repo `repo` into `parent/<clone_dir_name>`: the desktop's
/// "Clone into…", on the same safe path as the box's clone by id (4.3).
/// `url` is the one the caller built from the repo's canonical for a host
/// this device has a connection to (never a string from an event); `pin`
/// and `auth` as in [`clone_command`]. The clone lands in a hidden temp dir
/// next to it and is renamed only when complete, so a half-done clone never
/// shows up (or gets bound) as a checkout. The folder name is injective
/// (review F13) and only `[a-z0-9-]`, so it never leaves `parent`; an
/// existing folder (or a link) is refused, never reused. Like every clone
/// by id, git never follows a redirect, pinned or not
/// ([`by_id_transport_args`]). `parent` is resolved (links followed, the
/// Windows verbatim prefix dropped) before anything is created in it, so
/// the bound path is the one every later match compares.
pub fn clone_into(repo: &WorkspaceRepo, url: &str, parent: &Path, pin: Option<&str>, auth: &[String]) -> Result<Cloned, String> {
    clone_target(repo)?;
    if !parent.is_absolute() || !parent.is_dir() {
        return Err("pick an existing folder to clone into".into());
    }
    let parent = super::git::canonical(parent);
    let name = super::clone_dir_name(repo);
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
        return Err("this repository has no usable folder name".into());
    }
    let dest = parent.join(&name);
    if std::fs::symlink_metadata(&dest).is_ok() {
        return Err(format!("{} already exists", dest.display()));
    }
    let tmp = parent.join(format!(".ligus-clone-{}", uuid::Uuid::new_v4().simple()));
    let mut cmd = by_id_clone_command(url, &tmp, pin, auth);
    cmd.env("GIT_TERMINAL_PROMPT", "0").stdin(std::process::Stdio::null());
    let out = cmd.output().map_err(|e| e.to_string());
    let failed = match out {
        Ok(o) if o.status.success() => None,
        Ok(o) => Some(format!("git clone failed: {}", String::from_utf8_lossy(&o.stderr).lines().rfind(|l| !l.trim().is_empty()).unwrap_or("").trim())),
        Err(e) => Some(e),
    };
    if let Some(e) = failed {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    if std::fs::symlink_metadata(&dest).is_ok() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!("{} appeared while cloning", dest.display()));
    }
    if let Err(e) = std::fs::rename(&tmp, &dest) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!("moving the clone into place: {e}"));
    }
    let root_matches = finish_clone(&dest, repo)?;
    Ok(Cloned { path: dest, root_matches })
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

    /// The DNS pin only holds if git never leaves the pinned host (review
    /// F12): a server at the pinned address that answers with a redirect to
    /// another host (an internal one) must not be followed, or the pin is
    /// moot. Local sockets only.
    #[test]
    fn pinned_clone_never_follows_a_redirect() {
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicBool, Ordering};
        let elsewhere = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let b = elsewhere.local_addr().unwrap().port();
        elsewhere.set_nonblocking(true).unwrap();
        let hit = std::sync::Arc::new(AtomicBool::new(false));
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let (hit2, stop2) = (hit.clone(), stop.clone());
        let watcher = std::thread::spawn(move || {
            while !stop2.load(Ordering::SeqCst) {
                if let Ok((mut c, _)) = elsewhere.accept() {
                    hit2.store(true, Ordering::SeqCst);
                    let _ = c.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        });
        let pinned = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let a = pinned.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            for _ in 0..4 {
                let Ok((mut c, _)) = pinned.accept() else { break };
                let mut buf = [0u8; 4096];
                let _ = c.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{b}/o/n.git/info/refs?service=git-upload-pack\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = c.write_all(resp.as_bytes());
                if String::from_utf8_lossy(&buf).contains("info/refs") {
                    break;
                }
            }
        });
        let t = tempfile::tempdir().unwrap();
        let mut c = clone_command(&format!("http://localhost:{a}/o/n.git"), &t.path().join("d"), None, Some(&format!("localhost:{a}:127.0.0.1")), &[]);
        c.env("GIT_TERMINAL_PROMPT", "0").env_remove("http_proxy").env_remove("HTTP_PROXY").env_remove("ALL_PROXY").env_remove("all_proxy");
        let out = c.output().unwrap();
        stop.store(true, Ordering::SeqCst);
        watcher.join().unwrap();
        drop(server);
        assert!(!out.status.success());
        assert!(!hit.load(Ordering::SeqCst), "git followed the redirect off the pinned host: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// The desktop's "Clone into…" never follows a redirect either, with or
    /// without a DNS pin (a connection to a host on a private address has
    /// none), and its credentials come after the transport overrides.
    #[test]
    fn clone_into_never_follows_a_redirect_pinned_or_not() {
        for pin in [None, Some("h.example:443:93.184.216.34")] {
            let c = by_id_clone_command("https://h.example/o/n", Path::new("/tmp/x"), pin, &["-c".into(), "credential.helper=x".into()]);
            let args: Vec<String> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
            let redirect = args.iter().position(|a| a == "http.followRedirects=false").expect("no redirects");
            let cred = args.iter().position(|a| a == "credential.helper=x").unwrap();
            let clone = args.iter().position(|a| a == "clone").unwrap();
            assert!(redirect < cred && cred < clone, "{args:?}");
            assert_eq!(args.iter().any(|a| a.starts_with("http.curloptResolve=")), pin.is_some());
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
