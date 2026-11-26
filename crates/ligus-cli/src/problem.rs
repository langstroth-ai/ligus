//! Exit codes: one table for every Ligus tool (`ligus help exit-codes`;
//! docs-site/docs/operate/reference/cli-conventions.md), and the error that
//! carries one.
//!
//! | Code | Meaning |
//! |---|---|
//! | 0 | done (warnings allowed) |
//! | 1 | failed; the message or `checks[].fix` says why |
//! | 2 | usage error |
//! | 3 | a precondition you can fix is missing (a tool, a file, a sign-in) |
//! | 4 | a person must decide or act |
//! | 5 | not found (a workspace, chat, proposal, run) |
//! | 6 | refused by workspace rules (role, lockdown, agent access) |
//! | 7 | offline, the relay unreachable, or timed out |

use ligus_runtime::chat_service::ChatError;
use ligus_runtime::relay_client::RelayError;

pub const EXIT_OK: i32 = 0;
pub const EXIT_ERROR: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_FIXABLE: i32 = 3;
pub const EXIT_HUMAN: i32 = 4;
pub const EXIT_NOT_FOUND: i32 = 5;
pub const EXIT_REFUSED: i32 = 6;
pub const EXIT_OFFLINE: i32 = 7;

/// The shared table, for `ligus help exit-codes` and every command's help.
pub const TABLE: &[(i32, &str, &str)] = &[
    (EXIT_OK, "done (warnings allowed)", "continue"),
    (
        EXIT_ERROR,
        "failed: the message (or checks[].fix) says why",
        "read it, fix, retry",
    ),
    (
        EXIT_USAGE,
        "usage error: unknown command, missing or bad argument",
        "fix the command line",
    ),
    (
        EXIT_FIXABLE,
        "a precondition you can fix is missing (a tool, a file, a sign-in, the agent sandbox)",
        "fix it and rerun",
    ),
    (
        EXIT_HUMAN,
        "a person must decide or act (a passkey, an admission, exposure, deleting owned data)",
        "stop and ask",
    ),
    (
        EXIT_NOT_FOUND,
        "not found (a workspace, chat, proposal, run)",
        "check the id",
    ),
    (
        EXIT_REFUSED,
        "refused by workspace rules (a role, lockdown, agent access)",
        "ask an Admin; don't retry",
    ),
    (
        EXIT_OFFLINE,
        "offline, the relay unreachable, or timed out",
        "retry with backoff",
    ),
];

/// An error with its exit code, a stable machine code
/// (`relay_unreachable`, `not_found`) and, optionally, how to fix it.
#[derive(Debug)]
pub struct Problem {
    pub exit: i32,
    pub code: &'static str,
    pub message: String,
    pub fix: Option<String>,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Problem {}

/// Something named by id doesn't exist here (exit 5).
pub fn not_found(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Problem {
        exit: EXIT_NOT_FOUND,
        code: "not_found",
        message: message.into(),
        fix: None,
    })
}

/// What `ligus` prints for an error, and its exit code.
pub struct Rendered {
    pub exit: i32,
    /// `ligus: <what>`, then `  why: …` and `  fix: …` when known.
    pub text: String,
    /// The `ligus.error.v1` document (printed on stdout with `--json`).
    pub json: serde_json::Value,
}

/// The fix for an error that doesn't carry one, from its class.
fn default_fix(code: &str) -> Option<&'static str> {
    Some(match code {
        "relay_unreachable" | "unreachable" => "check the network and the relay URL; ligus doctor checks the relay",
        "relay_busy" => "wait and retry; the relay asked this device to slow down",
        "relay_token_rejected" => "sign in again (ligus auth login) or set a valid LIGUS_RELAY_ACCESS_TOKEN (ligus help env)",
        "refused" => "ask an Admin of the workspace; retrying won't help",
        _ => return None,
    })
}

/// The error in the one shape every Ligus tool prints (`ligus help
/// exit-codes`): the top message is what happened, the causes under it are
/// why, and the fix says what to do. `usage_hint` is the help to read for a
/// usage error.
pub fn render(e: &anyhow::Error, command: &str, usage_hint: &str) -> Rendered {
    let (exit, code) = classify(e);
    let what = e.to_string();
    let why: Vec<String> = e
        .chain()
        .skip(1)
        .map(|c| c.to_string())
        .filter(|c| !c.is_empty() && *c != what)
        .collect();
    let why = (!why.is_empty()).then(|| why.join(": "));
    let fix = e
        .chain()
        .find_map(|c| c.downcast_ref::<Problem>().and_then(|p| p.fix.clone()))
        .or_else(|| (exit == EXIT_USAGE).then(|| usage_hint.to_string()))
        .or_else(|| default_fix(code).map(str::to_string));
    let mut text = format!("ligus: {what}\n");
    if let Some(w) = &why {
        text.push_str(&format!("  why: {w}\n"));
    }
    if let Some(f) = &fix {
        text.push_str(&format!("  fix: {f}\n"));
    }
    let mut err = serde_json::json!({ "code": code, "message": what });
    if let Some(w) = why {
        err["why"] = w.into();
    }
    if let Some(f) = fix {
        err["fix"] = f.into();
    }
    let json = serde_json::json!({
        "schema": "ligus.error.v1",
        "tool": "ligus",
        "version": env!("CARGO_PKG_VERSION"),
        "command": command,
        "exitCode": exit,
        "error": err,
    });
    Rendered { exit, text, json }
}

/// The exit code and machine code for an error: a [`Problem`] says its own;
/// a usage error is 2; the workspace's refusals 6, a missing record 5; a
/// relay that can't be reached (or a rate limit to wait out) 7; a rejected
/// access token 3 (sign in again); anything else 1.
pub fn classify(e: &anyhow::Error) -> (i32, &'static str) {
    if let Some(p) = e.chain().find_map(|c| c.downcast_ref::<Problem>()) {
        return (p.exit, p.code);
    }
    if crate::help::is_usage(e) {
        return (EXIT_USAGE, "usage");
    }
    for c in e.chain() {
        if let Some(ce) = c.downcast_ref::<ChatError>() {
            match ce {
                ChatError::Unauthorized(_) => return (EXIT_REFUSED, "refused"),
                ChatError::NotFound(_) => return (EXIT_NOT_FOUND, "not_found"),
                ChatError::Store(_) => {}
            }
        }
        if let Some(re) = c.downcast_ref::<RelayError>() {
            match re {
                RelayError::Http(h) if h.is_connect() || h.is_timeout() => {
                    return (EXIT_OFFLINE, "relay_unreachable")
                }
                RelayError::Unauthorized => return (EXIT_FIXABLE, "relay_token_rejected"),
                RelayError::Status(403) => return (EXIT_REFUSED, "refused"),
                RelayError::Refused(r) if r.class().auto_retry() => {
                    return (EXIT_OFFLINE, "relay_busy")
                }
                _ => {}
            }
        }
        if let Some(h) = c.downcast_ref::<reqwest::Error>() {
            if h.is_connect() || h.is_timeout() {
                return (EXIT_OFFLINE, "unreachable");
            }
        }
    }
    (EXIT_ERROR, "error")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_map_to_the_table() {
        assert_eq!(classify(&anyhow::anyhow!("boom")), (EXIT_ERROR, "error"));
        assert_eq!(
            classify(&crate::help::usage_error("usage: x")).0,
            EXIT_USAGE
        );
        assert_eq!(
            classify(&not_found("unknown chat 1")),
            (EXIT_NOT_FOUND, "not_found")
        );
        // Context on top keeps the code.
        assert_eq!(
            classify(&not_found("unknown chat 1").context("tail")).0,
            EXIT_NOT_FOUND
        );
        let e: anyhow::Error = ChatError::Unauthorized("Viewers can't post".into()).into();
        assert_eq!(classify(&e), (EXIT_REFUSED, "refused"));
        let e: anyhow::Error = ChatError::NotFound("chat".into()).into();
        assert_eq!(classify(&e).0, EXIT_NOT_FOUND);
        let e: anyhow::Error = RelayError::Unauthorized.into();
        assert_eq!(classify(&e).0, EXIT_FIXABLE);
        let e: anyhow::Error = RelayError::Status(403).into();
        assert_eq!(classify(&e).0, EXIT_REFUSED);
        let e: anyhow::Error = RelayError::Status(500).into();
        assert_eq!(classify(&e).0, EXIT_ERROR);
    }

    #[tokio::test]
    async fn an_unreachable_relay_is_offline() {
        ligus_forge::install_tls_provider();
        // A closed port on loopback: connection refused.
        let err = reqwest::Client::new()
            .get("http://127.0.0.1:9/")
            .send()
            .await
            .unwrap_err();
        let e: anyhow::Error = RelayError::Http(err).into();
        assert_eq!(classify(&e), (EXIT_OFFLINE, "relay_unreachable"));
    }

    #[test]
    fn errors_render_in_one_shape() {
        use anyhow::Context as _;
        let e = Err::<(), _>(anyhow::anyhow!("connection refused"))
            .context("sync failed")
            .unwrap_err();
        let r = render(&e, "sync", "see `ligus sync --help`");
        assert_eq!(r.exit, EXIT_ERROR);
        assert_eq!(r.text, "ligus: sync failed\n  why: connection refused\n");
        assert_eq!(r.json["schema"], "ligus.error.v1");
        assert_eq!(r.json["command"], "sync");
        assert_eq!(r.json["exitCode"], 1);
        assert_eq!(r.json["error"]["code"], "error");
        assert_eq!(r.json["error"]["why"], "connection refused");
        assert!(r.json["error"].get("fix").is_none());

        let u = render(
            &crate::help::usage_error("usage: ligus tail <chat-id>"),
            "tail",
            "see `ligus tail --help`",
        );
        assert_eq!(u.exit, EXIT_USAGE);
        assert_eq!(
            u.text,
            "ligus: usage: ligus tail <chat-id>\n  fix: see `ligus tail --help`\n"
        );

        let n = render(&not_found("unknown chat 1"), "tail", "");
        assert_eq!(
            (n.exit, n.json["error"]["code"].as_str()),
            (EXIT_NOT_FOUND, Some("not_found"))
        );

        let p = anyhow::Error::new(Problem {
            exit: EXIT_FIXABLE,
            code: "x",
            message: "no CLI".into(),
            fix: Some("install it".into()),
        });
        assert_eq!(
            render(&p, "x", "").text,
            "ligus: no CLI\n  fix: install it\n"
        );

        let t: anyhow::Error = RelayError::Unauthorized.into();
        let t = render(&t, "sync", "");
        assert_eq!(t.exit, EXIT_FIXABLE);
        assert!(t.text.contains("  fix: sign in again"), "{}", t.text);
    }

    #[test]
    fn the_table_is_complete() {
        let codes: Vec<i32> = TABLE.iter().map(|t| t.0).collect();
        assert_eq!(codes, (0..=7).collect::<Vec<_>>());
    }
}
