//! The rules every surface applies the same way: agent names, URLs (https,
//! the loopback exception), MCP server ids and arguments, folders.

use std::net::IpAddr;
use std::path::Path;

use super::{exit, Ctx, Field, FieldKind, FolderFact, Inputs, Secrets};

/// Why an agent name can't be used, if it can't. `taken`: agent and member
/// display names already in the workspace.
pub fn agent_name_problem(name: &str, taken: &[String]) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return Some("an agent needs a name".into());
    }
    if name.chars().count() > 40 {
        return Some("keep the name to 40 characters".into());
    }
    if name.starts_with('@') || name.chars().any(|c| c.is_whitespace() || c.is_control() || c == ',') {
        return Some("a name is one word (letters, digits, - and _), without @".into());
    }
    if ligus_core::agent::is_reserved_agent_name(name) {
        return Some(format!("'{name}' is a reserved mention name"));
    }
    let skel = ligus_proto::names::name_skeleton(name);
    if taken.iter().any(|t| t.eq_ignore_ascii_case(name) || ligus_proto::names::name_skeleton(t) == skel) {
        return Some(format!("'{name}' is already taken by an agent or a member"));
    }
    None
}

/// A URL's problem for the relay or an MCP server: `https://` (or plain
/// `http://` to a loopback host on this machine), no userinfo, no token in
/// the query.
pub fn url_problem(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let Ok(u) = url::Url::parse(raw) else {
        return Some("not a URL (it starts https://)".into());
    };
    if !u.username().is_empty() || u.password().is_some() {
        return Some("the URL carries a user name or password: give the token separately".into());
    }
    if u.query_pairs().any(|(k, _)| looks_like_secret_key(&k)) {
        return Some("the URL carries a token in its query: give the token separately".into());
    }
    match u.scheme() {
        "https" => u.host_str().is_none().then(|| "the URL has no host".into()),
        "http" if is_loopback_url(&u) => None,
        "http" => Some("plain http is only for a relay or server on this machine (localhost); use https://".into()),
        s => Some(format!("{s}:// isn't supported: use https://")),
    }
}

/// `http://localhost`, `127.0.0.0/8` or `[::1]`: exactly, never a look-alike
/// (`localhost.example.com`, `127.0.0.1.nip.io`).
pub fn is_loopback_url(u: &url::Url) -> bool {
    match u.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(a)) => IpAddr::V4(a).is_loopback(),
        Some(url::Host::Ipv6(a)) => IpAddr::V6(a).is_loopback(),
        None => false,
    }
}

/// The URL's origin (`https://host[:port]`), for checks: never a path, a
/// query or userinfo.
pub fn origin(raw: &str) -> String {
    url::Url::parse(raw.trim()).map(|u| u.origin().ascii_serialization()).unwrap_or_else(|_| "that URL".into())
}

/// Whether two URLs have the same origin (scheme, host, port).
pub fn same_origin(a: &str, b: &str) -> bool {
    match (url::Url::parse(a.trim()), url::Url::parse(b.trim())) {
        (Ok(x), Ok(y)) => x.origin() == y.origin() && x.origin().is_tuple(),
        _ => false,
    }
}

fn looks_like_secret_key(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    ["token", "key", "secret", "password", "passwd", "auth", "sig", "credential"].iter().any(|w| k.contains(w))
}

/// Whether a command-line argument looks like it carries a secret (a token
/// pattern, or `--token=…` / `--api-key …` style).
pub fn arg_looks_secret(arg: &str, prev: Option<&str>) -> bool {
    if !crate::guard_scan::secret_kinds(arg).is_empty() {
        return true;
    }
    let flag_is_secret = |f: &str| f.starts_with('-') && looks_like_secret_key(f.trim_start_matches('-'));
    if let Some((k, v)) = arg.split_once('=') {
        if flag_is_secret(k) && !v.is_empty() {
            return true;
        }
    }
    prev.is_some_and(flag_is_secret) && !arg.starts_with('-')
}

/// An MCP server id: lowercase letters, digits and `-`, 1–40.
pub fn mcp_id_problem(id: &str) -> Option<String> {
    let ok = !id.is_empty()
        && id.len() <= 40
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-');
    (!ok).then(|| "an id is lowercase letters, digits and -, up to 40".into())
}

/// Split a command line on whitespace, keeping "double" or 'single' quoted
/// words together. `None` when a quote isn't closed.
pub fn split_command(s: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                any = true;
            }
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if quote.is_some() {
        return None;
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    Some(out)
}

/// The default branch of the checkout at `dir`: `origin`'s HEAD, else
/// `main` or `master` when it exists, else the current branch. One copy for
/// the desktop, `ligus repos` and setup.
pub fn default_branch(dir: &Path) -> Option<String> {
    use crate::repos::git::git;
    git(dir, &["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"])
        .ok()
        .and_then(|r| r.trim().strip_prefix("origin/").map(str::to_string))
        .filter(|b| !b.is_empty())
        .or_else(|| {
            ["main", "master"]
                .into_iter()
                .find(|b| git(dir, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{b}")]).is_ok())
                .map(str::to_string)
        })
        .or_else(|| git(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok().map(|b| b.trim().to_string()).filter(|b| !b.is_empty()))
}

/// What git says about `path`, without a store: a checkout, a linked
/// worktree, its canonical repository and remote, default branch, root
/// commit. Surfaces with a store add `managed_elsewhere` and `bound_to`.
pub fn inspect_folder(path: &str) -> FolderFact {
    let p = Path::new(path.trim());
    let mut f = FolderFact { path: path.trim().to_string(), exists: p.is_dir(), ..Default::default() };
    if !f.exists {
        return f;
    }
    f.is_checkout = crate::git_worktree::is_git_repo(p);
    if !f.is_checkout {
        return f;
    }
    f.linked_worktree = std::fs::symlink_metadata(p.join(".git")).is_ok_and(|m| m.is_file());
    f.canonical = crate::repos::git::suggested_canonical(p, &Default::default()).map(|(c, _)| c);
    f.remote = crate::repos::git::git(p, &["remote", "get-url", "origin"]).ok().map(|u| strip_userinfo(u.trim()));
    f.default_branch = default_branch(p);
    f.root_commit = crate::repos::git::root_commit(p);
    f
}

/// A remote URL without `user:password@`.
pub fn strip_userinfo(u: &str) -> String {
    match url::Url::parse(u) {
        Ok(mut url) if url.has_host() => {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.to_string()
        }
        _ => u.to_string(),
    }
}

/// Refuse inputs the flow doesn't know, a secret given as a plain input,
/// and a plain value given for a secret field (a surface mix-up). Also
/// checks required fields and choices.
pub(crate) fn unknown_inputs(ctx: &mut Ctx, fields: &[Field], inputs: &Inputs, secrets: &Secrets) {
    for k in inputs.0.keys() {
        match fields.iter().find(|f| f.id == k) {
            None => {
                return ctx.fail(
                    &format!("input.{k}"),
                    "input",
                    format!("this flow has no field '{k}'"),
                    format!("see `ligus setup {} --help`", ctx.flow.as_str()),
                    exit::USAGE,
                )
            }
            Some(f) if f.kind == FieldKind::Secret && inputs.get(k).is_some() => {
                return ctx.fail(
                    &format!("input.{k}"),
                    f.label,
                    "a secret was given as a plain value",
                    format!("pass --{k} - (stdin) or --{k} @FILE (owner-only); never on the command line"),
                    exit::USAGE,
                )
            }
            _ => {}
        }
    }
    for k in secrets.ids() {
        if !fields.iter().any(|f| f.id == k && f.kind == FieldKind::Secret) {
            return ctx.fail(
                &format!("input.{k}"),
                "input",
                format!("'{k}' isn't a secret field of this flow"),
                format!("see `ligus setup {} --help`", ctx.flow.as_str()),
                exit::USAGE,
            );
        }
    }
    for f in fields.iter().filter(|f| f.applies(inputs)) {
        let v = inputs.get(f.id);
        if f.required && f.kind != FieldKind::Secret && v.is_none() && f.default.is_none() {
            return ctx.fail(
                &format!("input.{}", f.id),
                f.label,
                format!("--{} is needed", f.id),
                format!("pass --{} <{}>", f.id, f.label.to_lowercase()),
                exit::USAGE,
            );
        }
        if let (FieldKind::Choice { options }, Some(v)) = (&f.kind, v) {
            match options.iter().find(|o| o.value == v) {
                None => {
                    let list: Vec<&str> = options.iter().map(|o| o.value.as_str()).collect();
                    return ctx.fail(
                        &format!("input.{}", f.id),
                        f.label,
                        format!("'{v}' isn't one of {}", list.join(", ")),
                        format!("pass --{} with one of: {}", f.id, list.join(", ")),
                        exit::USAGE,
                    );
                }
                Some(o) if o.disabled => {
                    let why = o.hint.clone().unwrap_or_else(|| "not available".into());
                    return ctx.fail(&format!("input.{}", f.id), f.label, format!("{}: {why}", o.label), "pick another", exit::USAGE);
                }
                _ => {}
            }
        }
    }
}

/// `inputs[k]`, else the field's default.
pub(crate) fn value<'a>(inputs: &'a Inputs, fields: &'a [Field], k: &str) -> Option<&'a str> {
    inputs.get(k).or_else(|| fields.iter().find(|f| f.id == k).and_then(|f| f.default.as_deref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_rule_rejects_lookalikes() {
        for ok in ["https://relay.example.com", "http://localhost:8080", "http://127.0.0.1:9", "http://127.8.0.1", "http://[::1]:7"] {
            assert_eq!(url_problem(ok), None, "{ok}");
        }
        for bad in [
            "http://relay.example.com",
            "http://localhost.example.com",
            "http://127.0.0.1.nip.io",
            "http://10.0.0.2",
            "ftp://localhost",
            "https://user:pw@relay.example.com",
            "https://relay.example.com/?token=abc",
            "https://relay.example.com/?api_key=abc",
            "not a url",
        ] {
            assert!(url_problem(bad).is_some(), "{bad}");
        }
    }

    #[test]
    fn same_origin_is_scheme_host_and_port() {
        assert!(same_origin("https://r.example.com", "https://r.example.com:443/v1"));
        assert!(!same_origin("https://r.example.com", "https://r.example.org"));
        assert!(!same_origin("https://r.example.com", "http://r.example.com"));
        assert!(!same_origin("https://r.example.com", "https://r.example.com:8443"));
        assert!(!same_origin("nonsense", "nonsense"));
    }

    #[test]
    fn origin_never_carries_path_or_query() {
        assert_eq!(origin("https://mcp.example.com:8443/v1/sse?x=1"), "https://mcp.example.com:8443");
    }

    #[test]
    fn names_follow_the_desktop_rules() {
        let taken = vec!["Reviewer".to_string(), "Ana".to_string()];
        assert!(agent_name_problem("", &taken).is_some());
        assert!(agent_name_problem("ligus", &taken).is_some());
        assert!(agent_name_problem("Hive", &taken).is_some(), "the legacy default agent name");
        assert!(agent_name_problem("reviewer", &taken).is_some(), "case-insensitive clash");
        assert!(agent_name_problem("ana", &taken).is_some(), "a member's name");
        assert!(agent_name_problem("two words", &taken).is_some());
        assert!(agent_name_problem("@bot", &taken).is_some());
        assert_eq!(agent_name_problem("builder", &taken), None);
    }

    #[test]
    fn stdio_args_secret_pattern_refused() {
        assert!(arg_looks_secret("--token=abc123", None));
        assert!(arg_looks_secret("abc123", Some("--api-key")));
        assert!(arg_looks_secret("ghp_0123456789abcdefghijklmnopqrstuvwxyzAB", None));
        assert!(!arg_looks_secret("--port", None));
        assert!(!arg_looks_secret("8080", Some("--port")));
        assert!(!arg_looks_secret("@modelcontextprotocol/server-filesystem", None));
    }

    #[test]
    fn commands_split_like_a_shell_would_for_simple_quoting() {
        assert_eq!(split_command("npx -y \"a b\" 'c d'").unwrap(), vec!["npx", "-y", "a b", "c d"]);
        assert_eq!(split_command("x \"\"").unwrap(), vec!["x", ""]);
        assert!(split_command("x \"open").is_none());
    }

    #[test]
    fn userinfo_is_stripped_from_remotes() {
        assert_eq!(strip_userinfo("https://u:p@github.com/o/r.git"), "https://github.com/o/r.git");
        assert_eq!(strip_userinfo("git@github.com:o/r.git"), "git@github.com:o/r.git");
    }
}
