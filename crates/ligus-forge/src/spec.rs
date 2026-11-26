//! Parsing what people paste: repository specs, remote URLs, hosts.
//!
//! Strict on purpose: a spec is `owner/name` or `https://<host>/<owner>/<name>`
//! (GitLab: `https://<host>/<group>/<sub>/<name>`), never credentials,
//! queries, fragments, other schemes or path tricks.

use crate::{ForgeError, RepoRef, Result};

/// A DNS host name, optionally with `:port` (lowercase ASCII letters,
/// digits, `-` and `.`; no leading or trailing `-`/`.`).
pub fn valid_host(h: &str) -> bool {
    let (name, port) = match h.rsplit_once(':') {
        Some((n, p)) => (n, Some(p)),
        None => (h, None),
    };
    if port.is_some_and(|p| p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit())) {
        return false;
    }
    !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|l| {
            !l.is_empty() && l.len() <= 63 && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') && !l.starts_with('-') && !l.ends_with('-')
        })
}

/// The host (with port) of an `http(s)://host[:port]/…` URL, lowercased.
pub fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let auth = rest.split(['/', '?', '#']).next()?;
    let host = auth.rsplit('@').next()?.to_ascii_lowercase();
    valid_host(&host).then_some(host)
}

/// Whether two hosts name the same forge (`www.` and GitHub's
/// `ssh.github.com` folded in; ports ignored).
pub fn same_host(a: &str, b: &str) -> bool {
    fn norm(h: &str) -> String {
        let h = h.split(':').next().unwrap_or(h).to_ascii_lowercase();
        let h = h.strip_prefix("www.").unwrap_or(&h).to_string();
        if h == "ssh.github.com" {
            "github.com".into()
        } else {
            h
        }
    }
    norm(a) == norm(b)
}

/// One owner/group path segment: 1–100 of `[A-Za-z0-9._-]`, not `.`/`..`,
/// not starting with `-` or `.`.
pub fn valid_segment(s: &str) -> bool {
    (1..=100).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && !s.starts_with('-')
        && !s.starts_with('.')
}

/// A repository name: like a segment, but may start with `.` (`.github`),
/// never `.` or `..`.
pub fn valid_repo_name(s: &str) -> bool {
    (1..=100).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')) && s != "." && s != ".." && !s.starts_with('-')
}

/// A parsed spec: the host when a URL named one, and the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSpec {
    pub host: Option<String>,
    pub repo: RepoRef,
}

/// `owner/name`, `group/sub/name` (when `nested` is allowed) or
/// `https://<host>/<owner>/<name>[.git][/]`.
pub fn parse_repo_spec(input: &str, nested: bool) -> Result<RepoSpec> {
    let raw = input.trim();
    let bad = |m: &str| Err(ForgeError::Invalid(m.into()));
    if raw.is_empty() || raw.len() > 300 || raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return bad("give a repository as owner/name or https://<host>/owner/name");
    }
    let (host, path) = if let Some(rest) = raw.strip_prefix("https://") {
        let (auth, path) = rest.split_once('/').unwrap_or((rest, ""));
        if auth.contains('@') {
            return bad("a repository URL can't carry credentials");
        }
        let host = auth.to_ascii_lowercase();
        if !valid_host(&host) {
            return bad("that URL's host isn't valid");
        }
        if path.contains(['?', '#']) {
            return bad("a repository URL can't carry a query or fragment");
        }
        (Some(host.strip_prefix("www.").unwrap_or(&host).to_string()), path.trim_end_matches('/'))
    } else if raw.contains("://") || raw.contains('@') || raw.contains(':') {
        return bad("only https://<host>/owner/name URLs (or owner/name) are accepted");
    } else {
        (None, raw)
    };
    let path = path.strip_suffix(".git").unwrap_or(path);
    let segs: Vec<&str> = path.split('/').collect();
    let max = if nested { 20 } else { 2 };
    if segs.len() < 2 || segs.len() > max {
        return bad(if nested { "give a repository as group/name" } else { "give a repository as owner/name" });
    }
    let (name, owner) = segs.split_last().expect("two or more");
    if !owner.iter().all(|s| valid_segment(s)) || !valid_repo_name(name) || owner.iter().any(|s| s.contains("..")) {
        return bad(&format!("'{path}' isn't a valid repository name"));
    }
    Ok(RepoSpec { host, repo: RepoRef::new(owner.join("/"), *name) })
}

/// A git remote URL (`https://[user[:pw]@]host/o/n(.git)`, `ssh://git@host[:port]/o/n`,
/// `git@host:o/n.git`) → (host, repo). Credentials are dropped. Paths with
/// more than two segments keep the leading ones as the owner (GitLab
/// subgroups). Local paths and `file://` give no host (`None`).
pub fn parse_remote(url: &str) -> Option<(String, RepoRef)> {
    let url = url.trim();
    // git (curl) ends the host at `#` or `?`: `https://evil#@github.com/o/n`
    // connects to evil, so it is never github.com's repo.
    if url.contains(['#', '?', '\\']) {
        return None;
    }
    let (host, path) = if let Some((scheme, rest)) = url.split_once("://") {
        if scheme.eq_ignore_ascii_case("file") {
            return None;
        }
        let (auth_host, path) = rest.split_once('/')?;
        let host = auth_host.rsplit('@').next()?.split(':').next()?.to_ascii_lowercase();
        (host, path.to_string())
    } else if let Some((left, path)) = url.split_once(':').filter(|(l, _)| !l.contains('/') && l.len() > 1) {
        (left.rsplit('@').next()?.to_ascii_lowercase(), path.to_string())
    } else {
        return None;
    };
    if host.is_empty() {
        return None;
    }
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.rsplit_once('/')?;
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some((host, RepoRef::new(owner, name)))
}

/// Percent-encode one path segment (RFC 3986 unreserved kept).
pub fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Percent-encode a file path, keeping `/` between segments.
pub fn encode_path(p: &str) -> String {
    p.split('/').map(encode_segment).collect::<Vec<_>>().join("/")
}

/// A repository-relative file path an agent may ask for: no `..`, no
/// absolute paths, no control characters, at most 1,024 bytes.
pub fn valid_file_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 1024
        && !p.starts_with('/')
        && !p.chars().any(char::is_control)
        && p.split('/').all(|s| !s.is_empty() && s != "." && s != "..")
}

/// A git ref an agent may ask for: a branch, tag or commit; no `..`, no
/// spaces or control characters, no leading `-`.
pub fn valid_git_ref(r: &str) -> bool {
    !r.is_empty()
        && r.len() <= 255
        && !r.starts_with('-')
        && !r.contains("..")
        && !r.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_accept_any_https_host_and_nothing_else() {
        let ok = |s: &str, nested: bool| parse_repo_spec(s, nested).unwrap();
        assert_eq!(ok("acme/web", false), RepoSpec { host: None, repo: RepoRef::new("acme", "web") });
        assert_eq!(ok("https://codeberg.org/forgejo/forgejo.git/", false).host.as_deref(), Some("codeberg.org"));
        assert_eq!(ok("https://www.github.com/a/.github", false), RepoSpec { host: Some("github.com".into()), repo: RepoRef::new("a", ".github") });
        assert_eq!(ok("https://gitlab.example.com:8443/grp/sub/proj", true).repo, RepoRef::new("grp/sub", "proj"));
        assert!(parse_repo_spec("https://gitlab.com/grp/sub/proj", false).is_err(), "nesting only where the forge has groups");
        for bad in [
            "",
            "ligus",
            "../etc",
            "a/..",
            "a/.",
            "-a/b",
            "a/b c",
            "a/b;rm",
            "http://github.com/a/b",
            "https://user:tok@github.com/a/b",
            "https://github.com/a/b?x=1",
            "https://github.com/a/b#x",
            "git@github.com:a/b.git",
            "file:///etc/passwd",
            "/abs/path",
            "ext::sh -c id",
            "a/\nb",
            "https://bad_host/a/b",
            "https://h/../a/b",
        ] {
            assert!(parse_repo_spec(bad, true).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn remotes_parse_drop_credentials_and_keep_subgroups() {
        let (h, r) = parse_remote("https://x-access-token:SECRET@github.com/Owner/Repo.git").unwrap();
        assert_eq!((h.as_str(), r.full_name().as_str()), ("github.com", "Owner/Repo"));
        assert!(!format!("{r:?}").contains("SECRET"));
        assert_eq!(parse_remote("git@gitlab.com:grp/sub/p.git").unwrap().1, RepoRef::new("grp/sub", "p"));
        assert_eq!(parse_remote("ssh://git@codeberg.org:22/o/n").unwrap().0, "codeberg.org");
        assert!(parse_remote("file:///tmp/o/n.git").is_none());
        assert!(parse_remote("/srv/git/o/n").is_none());
        // git connects to the host before `#`/`?`, never the one after `@`.
        for spoof in ["https://evil.example#@github.com/o/n", "https://evil.example?@github.com/o/n", "evil.example#@github.com:o/n"] {
            assert!(parse_remote(spoof).is_none_or(|(h, _)| h != "github.com"), "{spoof}");
        }
    }

    #[test]
    fn hosts_paths_and_refs_are_validated() {
        assert!(valid_host("git.example.com:3000") && !valid_host("a..b") && !valid_host("x:abc") && !valid_host("-x.com"));
        assert!(same_host("ssh.github.com", "github.com") && same_host("www.codeberg.org", "codeberg.org:443") && !same_host("gitlab.com", "github.com"));
        assert_eq!(host_of("https://u@Git.Example.com:8443/x").as_deref(), Some("git.example.com:8443"));
        assert!(valid_file_path("src/lib.rs") && !valid_file_path("../x") && !valid_file_path("/etc") && !valid_file_path("a//b"));
        assert!(valid_git_ref("main") && valid_git_ref("v1.2.3") && valid_git_ref("abc123") && !valid_git_ref("-x") && !valid_git_ref("a..b") && !valid_git_ref("a b"));
        assert_eq!(encode_segment("grp/sub"), "grp%2Fsub");
        assert_eq!(encode_path("a b/c.rs"), "a%20b/c.rs");
    }
}
