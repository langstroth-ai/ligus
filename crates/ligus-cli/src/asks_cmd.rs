//! `ligus asks …`: agents' questions from a person's own CLI device
//! (`docs/design/agent-questions.md` 7.5). Answering is a human step: every
//! answer needs a yes at the terminal (`/dev/tty`), so an agent running as
//! the person can't settle a card through this CLI by accident. A grant
//! needs the person's passkey, which this CLI can't ask for: it is answered
//! on the desktop or the phone. Prompts from `ligus hook permission` are
//! never answered here, since this CLI is the very place the asking agent
//! could reach.

use anyhow::Result;
use ligus_core::flow::ORIGIN_OUTSIDE;
use ligus_runtime::ask_view::{self, AskView};
use ligus_runtime::chat_service::AskChoice;
use serde_json::json;
use uuid::Uuid;

use crate::checks::{Check, Report};
use crate::problem::{EXIT_ERROR, EXIT_HUMAN, EXIT_NOT_FOUND, EXIT_OK, EXIT_REFUSED, EXIT_USAGE};
use crate::{open_service, Config};

/// The flags `ligus asks` reads.
#[derive(Debug, Default, Clone)]
pub struct Flags {
    pub chat: Option<String>,
    pub mine: bool,
    pub option: Option<String>,
    pub yes: bool,
    pub no: bool,
    pub text: Option<String>,
    pub json: bool,
}

pub const USAGE: &str = "usage: ligus asks list [--chat ID] [--mine] [--json] | answer <ask-id> (--option N | --yes | --no) [--text TEXT] [--json] | decline <ask-id> [--json]";

/// Finish: print the report (text or `--json`) and exit with `code`.
fn finish(
    report: Report,
    code: i32,
    json_out: bool,
    extra: Option<serde_json::Value>,
) -> Result<()> {
    if json_out {
        let mut j = report.to_json(code);
        j["schema"] = json!("ligus.agent-deploy.v1");
        if let Some(x) = extra {
            j["asks"] = x;
        }
        println!("{j}");
    } else {
        for c in &report.checks {
            eprintln!("{}", Report::check_text(c));
        }
    }
    if code != EXIT_OK {
        std::process::exit(code);
    }
    Ok(())
}

fn line(v: &AskView) -> String {
    let who = match v.audience.kind {
        "people" => format!("asks {}", v.audience.names.join(", ")),
        _ => format!(
            "asks {}",
            v.audience
                .min_role
                .map(|r| format!("{r}s"))
                .unwrap_or_else(|| "the room".into())
        ),
    };
    let state = if v.live {
        if v.eligible {
            "you can answer"
        } else {
            "waiting"
        }
    } else if v.answer.is_some() {
        "answered"
    } else {
        "closed"
    };
    let opts = v
        .options
        .iter()
        .enumerate()
        .map(|(i, o)| format!("{i} {o}"))
        .collect::<Vec<_>>()
        .join("  ");
    let effect = match &v.effect {
        Some(e) if v.kind != "question" => format!(
            "\n    grant: {} on this host: {}",
            e.tool,
            e.detail.lines().next().unwrap_or("")
        ),
        _ => "\n    no new permission".into(),
    };
    format!(
        "{}  {} {who} ({state})\n    \"{}\"\n    {opts}{effect}",
        v.ask_id,
        v.agent_name,
        v.question.replace('\n', " ")
    )
}

/// `ligus asks …` (the words after `asks`).
pub fn run(cfg: &Config, positional: &[&String], f: &Flags) -> Result<()> {
    let sub = positional.first().map(|s| s.as_str());
    let svc = open_service(cfg)?;
    let me = svc.author().id.clone();
    let now = ligus_core::flow::now_ms();
    let chats: Vec<ligus_core::ChatSession> = match &f.chat {
        Some(c) => {
            let id = Uuid::parse_str(c.trim())
                .map_err(|_| crate::help::usage_error(format!("--chat is an id, not {c:?}")))?;
            svc.load_resolving(id)?.into_iter().collect()
        }
        None => svc
            .store()
            .list_session_ids()?
            .into_iter()
            .filter_map(|id| svc.load_resolving(id).ok().flatten())
            .filter(|s| !s.flow.asks.is_empty())
            .collect(),
    };
    match sub {
        Some("list") | None => {
            let mut all: Vec<AskView> = chats
                .iter()
                .flat_map(|s| ask_view::views(s, &me, now, 50, &|_| None))
                .filter(|v| v.live)
                .collect();
            if f.mine {
                all.retain(|v| v.eligible);
            }
            let mut r = Report::new("asks list");
            r.add(Check::ok(
                "asks.open",
                "Open questions",
                format!(
                    "{} open, {} you can answer",
                    all.len(),
                    all.iter().filter(|v| v.eligible).count()
                ),
            ));
            if !f.json {
                for v in &all {
                    println!("{}", line(v));
                }
            }
            finish(r, EXIT_OK, f.json, Some(serde_json::to_value(&all)?))
        }
        Some("answer") | Some("decline") => {
            let sub = sub.unwrap_or_default();
            let id = positional.get(1).map(|s| s.as_str()).unwrap_or("");
            let mut r = Report::new(&format!("asks {sub}"));
            let Ok(ask_id) = Uuid::parse_str(id.trim()) else {
                r.add(Check::fail("asks.usage", "Which question", USAGE));
                return finish(r, EXIT_USAGE, f.json, None);
            };
            let Some(session) = chats
                .iter()
                .find(|s| s.flow.asks.contains_key(&ask_id))
                .cloned()
            else {
                r.add(
                    Check::fail(
                        "asks.found",
                        "The question",
                        format!("no question {ask_id} here"),
                    )
                    .fix("ligus asks list"),
                );
                return finish(r, EXIT_NOT_FOUND, f.json, None);
            };
            let st = session.flow.asks[&ask_id].clone();
            let v = ask_view::view(&session, &st, &me, now, &|_| None);
            if st
                .ask
                .origin
                .as_ref()
                .is_some_and(|o| o.via == ORIGIN_OUTSIDE)
            {
                r.add(Check::fail("asks.outside", "An outside prompt", "prompts from ligus hook permission are answered on the desktop or the phone, never from this CLI").fix("answer it in the Ligus app"));
                return finish(r, EXIT_REFUSED, f.json, None);
            }
            if !v.live {
                r.add(Check::fail(
                    "asks.open",
                    "Still open",
                    "already answered, closed or expired",
                ));
                return finish(r, EXIT_ERROR, f.json, None);
            }
            if !v.eligible {
                r.add(
                    Check::fail(
                        "asks.eligible",
                        "Yours to answer",
                        "this question isn't put to you",
                    )
                    .fix("ask the person it names"),
                );
                return finish(r, EXIT_REFUSED, f.json, None);
            }
            if st.grant().is_some() {
                r.add(Check::fail("asks.grant", "A grant", "allowing or denying a tool call needs your passkey, which this CLI can't ask for").fix("answer it in the Ligus desktop app or on your phone"));
                return finish(r, EXIT_HUMAN, f.json, None);
            }
            let decline = sub == "decline";
            let option: Option<u32> = if decline {
                None
            } else if f.yes || f.no {
                let want = if f.yes { "Yes" } else { "No" };
                match st.ask.options.iter().position(|o| o == want) {
                    Some(i) => Some(i as u32),
                    None => {
                        r.add(Check::fail(
                            "asks.option",
                            "The option",
                            "this question isn't yes/no: use --option N",
                        ));
                        return finish(r, EXIT_USAGE, f.json, None);
                    }
                }
            } else {
                match f.option.as_deref().map(str::parse::<u32>) {
                    Some(Ok(i)) if (i as usize) < v.options.len() => Some(i),
                    Some(_) => {
                        r.add(Check::fail(
                            "asks.option",
                            "The option",
                            format!("--option is 0 to {}", v.options.len().saturating_sub(1)),
                        ));
                        return finish(r, EXIT_USAGE, f.json, None);
                    }
                    None if f.text.is_some() && v.allow_text => None,
                    None => {
                        r.add(Check::fail("asks.option", "The option", USAGE));
                        return finish(r, EXIT_USAGE, f.json, None);
                    }
                }
            };
            let what = if decline {
                "Not now".to_string()
            } else {
                option
                    .and_then(|i| v.options.get(i as usize).cloned())
                    .unwrap_or_else(|| "your reply".into())
            };
            if !crate::hook_cmd::confirm_tty(&format!(
                "Answer {}'s question with '{what}'?",
                v.agent_name
            )) {
                r.add(
                    Check::fail(
                        "asks.confirm",
                        "Your yes",
                        "an answer needs your yes at the terminal (/dev/tty)",
                    )
                    .fix("run it yourself in a terminal, or answer in the Ligus app"),
                );
                return finish(r, EXIT_HUMAN, f.json, None);
            }
            let mut svc = svc;
            match svc.answer_ask(
                session.id,
                session.workspace_id,
                ask_id,
                AskChoice {
                    option,
                    text: f.text.clone().unwrap_or_default(),
                    decline,
                },
            ) {
                Ok(env) => {
                    r.add(Check::ok(
                        "asks.answered",
                        "Answered",
                        format!("answered with '{what}' (event {})", env.event_id),
                    ));
                    finish(r, EXIT_OK, f.json, None)
                }
                Err(ligus_runtime::chat_service::ChatError::Unauthorized(why)) => {
                    r.add(Check::fail("asks.refused", "Answered", why));
                    finish(r, EXIT_REFUSED, f.json, None)
                }
                Err(e) => {
                    r.add(Check::fail("asks.answered", "Answered", e.to_string()));
                    finish(r, EXIT_ERROR, f.json, None)
                }
            }
        }
        Some(other) => Err(crate::help::usage_error(format!(
            "unknown `ligus asks {other}`: {USAGE}"
        ))),
    }
}
