//! Push notifications through a self-hosted ntfy server.
//!
//! `POST <url>/<topic>` with `Title`, `Click: ligus://chat/<chatId>` (the app
//! opens the chat on tap) and `Tags`. The ntfy server and anyone who
//! subscribes to the topic read what is sent, so by default the text is
//! generic ("A reply is ready"): no agent, person, chat or proposal name.
//! An operator who trusts their ntfy server opts into the fixed-shape line
//! (who did what, in which chat, never message contents) with
//! `LIGUS_NTFY_TITLES=1`. The URL must be `https://` unless the operator sets
//! `LIGUS_NTFY_ALLOW_HTTP=1` (a server on the tailnet or this host). An
//! optional access token (`LIGUS_NTFY_TOKEN`) is sent as a bearer token.

use anyhow::{bail, Result};

use std::time::Duration;

use base64::Engine;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct Ntfy {
    pub url: String,
    pub topic: String,
    pub token: Option<String>,
    /// Send names and chat titles (`LIGUS_NTFY_TITLES=1`); otherwise only
    /// generic text.
    pub titles: bool,
}

/// The ntfy settings from flags and env: both parts or neither, `https://`
/// unless `allow_http` (`LIGUS_NTFY_ALLOW_HTTP=1`).
pub fn settings(
    url: Option<String>,
    topic: Option<String>,
    token: Option<String>,
    allow_http: bool,
    titles: bool,
) -> Result<Option<Ntfy>> {
    let (url, topic) = match (url, topic) {
        (Some(url), Some(topic)) => (url.trim().to_string(), topic),
        (Some(_), None) | (None, Some(_)) => {
            bail!("ntfy needs both a URL and a topic (--ntfy-url / --ntfy-topic)")
        }
        (None, None) => return Ok(None),
    };
    let scheme = url
        .split_once("://")
        .map(|(s, _)| s.to_ascii_lowercase())
        .unwrap_or_default();
    match scheme.as_str() {
        "https" => {}
        "http" if allow_http => {}
        "http" => bail!(
            "refusing the ntfy URL {url}: it is plain http, so the topic, the access token and every notification              travel unencrypted. Use https, or set LIGUS_NTFY_ALLOW_HTTP=1 for a server on this host or your tailnet"
        ),
        _ => bail!("the ntfy URL must start with https:// (got {url})"),
    }
    Ok(Some(Ntfy {
        url,
        topic,
        token,
        titles,
    }))
}

/// One notification.
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub title: String,
    pub body: String,
    pub chat_id: Uuid,
    pub tags: &'static str,
}

impl Ntfy {
    /// The topic URL (`<url>/<topic>`).
    pub fn endpoint(&self) -> String {
        format!(
            "{}/{}",
            self.url.trim_end_matches('/'),
            self.topic.trim_matches('/')
        )
    }

    /// What goes to the ntfy server: the notice itself when the operator
    /// opted into titles, otherwise [`generic`] text.
    pub fn outgoing(&self, n: &Notice) -> Notice {
        if self.titles {
            n.clone()
        } else {
            generic(n)
        }
    }

    pub async fn send(&self, client: &reqwest::Client, n: &Notice) {
        let n = &self.outgoing(n);
        let mut req = client
            .post(self.endpoint())
            .timeout(Duration::from_secs(5))
            .header("Title", header_text(&n.title))
            .header(
                "Click",
                ligus_core::names::deep_link(&format!("chat/{}", n.chat_id)),
            )
            .header("Tags", n.tags)
            .body(n.body.clone());
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        match req.send().await {
            Ok(r) if r.status().is_success() => {}
            Ok(r) => eprintln!("ntfy: HTTP {}", r.status()),
            Err(e) => eprintln!("ntfy: {}", e.without_url()),
        }
    }
}

/// A header-safe rendering of `s`: plain printable ASCII passes through;
/// anything else is sent as an RFC 2047 encoded word, which ntfy decodes.
pub fn header_text(s: &str) -> String {
    let clean: String = s.chars().filter(|c| !c.is_control()).collect();
    if clean.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        clean
    } else {
        format!(
            "=?UTF-8?B?{}?=",
            base64::engine::general_purpose::STANDARD.encode(clean.as_bytes())
        )
    }
}

/// A notice with nothing but its kind: no agent, person, chat, proposal or
/// workflow name (each of those is chosen by someone in the workspace, and
/// the ntfy server reads it).
pub fn generic(n: &Notice) -> Notice {
    let (title, body) = match n.tags {
        "speech_balloon" => ("Ligus", "A reply is ready"),
        "warning" => ("Ligus", "An agent hit an error"),
        "memo" => ("Ligus", "A change is ready for review"),
        "question" => ("Ligus", "An agent has a question"),
        "bell" => ("Ligus", "You were mentioned"),
        "raised_hand" => ("Ligus", "A workflow needs approval"),
        "inbox_tray" => ("Ligus", "The review queue needs you"),
        _ => ("Ligus", "Something needs your attention"),
    };
    Notice {
        title: title.into(),
        body: body.into(),
        chat_id: n.chat_id,
        tags: n.tags,
    }
}

fn short(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!(
            "{}…",
            s.chars().take(n.saturating_sub(1)).collect::<String>()
        )
    }
}

/// The notice for a finished agent reply.
pub fn reply_notice(agent: &str, chat_title: &str, chat_id: Uuid, failed: bool) -> Notice {
    let chat = short(
        if chat_title.trim().is_empty() {
            "Untitled"
        } else {
            chat_title
        },
        60,
    );
    let agent = short(agent, 40);
    Notice {
        title: if failed {
            format!("{agent} hit an error")
        } else {
            format!("{agent} replied")
        },
        body: format!("in “{chat}”"),
        chat_id,
        tags: if failed { "warning" } else { "speech_balloon" },
    }
}

/// The notice asking this member to help with a backed-up review queue.
pub fn recruit_notice(queue_len: u32, chat_title: &str, chat_id: Uuid) -> Notice {
    let chat = short(
        if chat_title.trim().is_empty() {
            "Untitled"
        } else {
            chat_title
        },
        60,
    );
    let n = queue_len.max(1);
    Notice {
        title: "The review queue needs you".into(),
        body: format!(
            "{n} proposal{} waiting — from “{chat}”",
            if n == 1 { "" } else { "s" }
        ),
        chat_id,
        tags: "inbox_tray",
    }
}

/// The notice for a new proposal awaiting review.
pub fn proposal_notice(title: &str, chat_title: &str, chat_id: Uuid) -> Notice {
    let chat = short(
        if chat_title.trim().is_empty() {
            "Untitled"
        } else {
            chat_title
        },
        60,
    );
    Notice {
        title: "Change ready for review".into(),
        body: format!("{} — in “{chat}”", short(title, 80)),
        chat_id,
        tags: "memo",
    }
}

/// The notice for an agent's question waiting for an answer.
pub fn ask_notice(agent: &str, chat_title: &str, chat_id: Uuid) -> Notice {
    let chat = short(
        if chat_title.trim().is_empty() {
            "Untitled"
        } else {
            chat_title
        },
        60,
    );
    Notice {
        title: format!("{} has a question", short(agent, 40)),
        body: format!("in \u{201c}{chat}\u{201d}"),
        chat_id,
        tags: "question",
    }
}

/// The notice for a message that `@mentions` the user.
pub fn mention_notice(sender: &str, chat_title: &str, chat_id: Uuid) -> Notice {
    let chat = short(
        if chat_title.trim().is_empty() {
            "Untitled"
        } else {
            chat_title
        },
        60,
    );
    Notice {
        title: format!("{} mentioned you", short(sender, 40)),
        body: format!("in “{chat}”"),
        chat_id,
        tags: "bell",
    }
}

/// The notice for a workflow run waiting on an approval gate.
pub fn workflow_notice(workflow: &str, stage: &str, chat_title: &str, chat_id: Uuid) -> Notice {
    let chat = short(
        if chat_title.trim().is_empty() {
            "Untitled"
        } else {
            chat_title
        },
        60,
    );
    Notice {
        title: workflow_title(workflow),
        body: format!("{} — in “{chat}”", short(stage, 80)),
        chat_id,
        tags: "raised_hand",
    }
}

/// "<workflow> needs approval".
pub fn workflow_title(workflow: &str) -> String {
    let name = if workflow.trim().is_empty() {
        "Workflow"
    } else {
        workflow
    };
    format!("{} needs approval", short(name, 40))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_titles_pass_and_unicode_is_encoded() {
        assert_eq!(header_text("coder replied"), "coder replied");
        let enc = header_text("café ☕");
        assert!(enc.starts_with("=?UTF-8?B?") && enc.ends_with("?="));
        assert_eq!(header_text("a\nb"), "ab");
    }

    #[test]
    fn notices_carry_no_message_content_and_link_the_chat() {
        let id = Uuid::new_v4();
        let n = reply_notice("@coder", "Fix the build", id, false);
        assert_eq!(n.title, "@coder replied");
        assert_eq!(n.body, "in “Fix the build”");
        assert_eq!(n.chat_id, id);
        let p = proposal_notice("@coder proposed changes to 1 file", "", id);
        assert!(p.body.contains("Untitled"));
    }

    #[test]
    fn endpoint_joins_cleanly() {
        let n = Ntfy {
            url: "http://h:8095/".into(),
            topic: "/t1".into(),
            token: None,
            titles: false,
        };
        assert_eq!(n.endpoint(), "http://h:8095/t1");
    }

    /// Serve hardening: plain-http ntfy only when the operator says so.
    #[test]
    fn http_ntfy_needs_an_explicit_opt_in() {
        let s = |url: &str, allow: bool| {
            settings(Some(url.into()), Some("t".into()), None, allow, false)
        };
        assert!(s("http://ntfy.lan:8095", false)
            .unwrap_err()
            .to_string()
            .contains("LIGUS_NTFY_ALLOW_HTTP"));
        assert!(s("HTTP://ntfy.lan", false).is_err());
        assert_eq!(
            s("http://ntfy.lan:8095", true).unwrap().unwrap().endpoint(),
            "http://ntfy.lan:8095/t"
        );
        assert_eq!(
            s("https://ntfy.example", false)
                .unwrap()
                .unwrap()
                .endpoint(),
            "https://ntfy.example/t"
        );
        assert!(s("ftp://x", true).is_err());
        assert!(s("ntfy.example", true).is_err());
        assert!(settings(Some("https://x".into()), None, None, false, false).is_err());
        assert!(settings(None, None, None, false, false).unwrap().is_none());
    }

    /// Serve hardening: by default the ntfy server sees only what kind of
    /// thing happened; names and titles go out only when opted in.
    #[test]
    fn ntfy_text_is_generic_unless_titles_are_opted_in() {
        let id = Uuid::new_v4();
        let off = Ntfy {
            url: "https://n".into(),
            topic: "t".into(),
            token: None,
            titles: false,
        };
        let on = Ntfy {
            titles: true,
            ..off.clone()
        };
        for n in [
            reply_notice("@coder", "Acquire Initech", id, false),
            reply_notice("@coder", "Acquire Initech", id, true),
            proposal_notice("Drop the prod DB", "Acquire Initech", id),
            ask_notice("@coder", "Acquire Initech", id),
            mention_notice("Ana", "Acquire Initech", id),
            workflow_notice("Payroll", "sign-off", "Acquire Initech", id),
            recruit_notice(3, "Acquire Initech", id),
        ] {
            let g = off.outgoing(&n);
            let text = format!("{} {}", g.title, g.body);
            for secret in ["coder", "Initech", "prod DB", "Ana", "Payroll", "sign-off"] {
                assert!(!text.contains(secret), "{secret} in {text}");
            }
            assert_eq!((g.chat_id, g.tags), (id, n.tags));
            assert_eq!(on.outgoing(&n), n);
        }
    }
}
