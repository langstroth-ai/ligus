//! Decision cards (`docs/design/agent-questions.md`): `ligus_ask_person`
//! and `ligus_ask_room`, and the grant cards a host posts for an ACP
//! permission request. An agent's card is a question: its answer grants
//! nothing. A grant card has an effect only this host writes, and its answer
//! counts only from the people the host named (with their passkey where the
//! effect says so), all judged by the fold (`ligus_core::flow::asks`).

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ligus_core::flow::{self, AskAudience, AskState, AskVote, FlowRecord, HumanAsk, ASK_V2};
use ligus_core::WorkspaceRole;
use ligus_proto::ask_text::{self, MAX_OPTIONS, MAX_OPTION_CHARS, MAX_QUESTION_CHARS, MIN_OPTIONS, YES_NO};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{LigusHost, LigusMcp, Outcome, ToolResult, TurnState};
use crate::effects::EffectCtx;

/// Open asks one agent may have in a chat, and a chat in all.
pub const MAX_OPEN_PER_AGENT: usize = 2;
pub const MAX_OPEN_PER_CHAT: usize = 5;
/// Default and bounds of a card's wait.
pub const DEFAULT_TIMEOUT_SECS: u64 = 600;
pub const MIN_TIMEOUT_SECS: u64 = 10;
pub const MAX_TIMEOUT_SECS: u64 = 3600;

/// A card's question and choices, cleaned and checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub question: String,
    pub options: Vec<String>,
    pub allow_text: bool,
    pub timeout: Duration,
}

/// A refusal before anything is posted: `{"error": code, "message": …}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}

fn refuse(code: &'static str, message: impl Into<String>) -> Refusal {
    Refusal { code, message: message.into() }
}

impl Refusal {
    pub fn result(&self) -> ToolResult {
        ToolResult::err(Outcome::Denied, json!({"error": self.code, "message": self.message}))
    }
}

/// Parse a card's shared arguments (section 4.1, 6.4).
pub fn parse_card(args: &Value) -> Result<Card, Refusal> {
    let raw_q = args.get("question").and_then(Value::as_str).unwrap_or("");
    let question = ask_text::clean_question(raw_q);
    let n = question.chars().count();
    if n == 0 {
        return Err(refuse("question_required", "a question is required"));
    }
    if n > MAX_QUESTION_CHARS {
        return Err(refuse("question_too_long", format!("the question is {n} characters; at most {MAX_QUESTION_CHARS}")));
    }
    let yes_no = args.get("yesNo").and_then(Value::as_bool).unwrap_or(false);
    let given = args.get("options").and_then(Value::as_array);
    let options: Vec<String> = if yes_no {
        if given.is_some_and(|a| !a.is_empty()) {
            return Err(refuse("options_and_yes_no", "give options or yesNo, not both"));
        }
        YES_NO.iter().map(|s| s.to_string()).collect()
    } else {
        let Some(given) = given else {
            return Err(refuse("options_required", format!("give {MIN_OPTIONS} to {MAX_OPTIONS} options, or yesNo: true")));
        };
        let mut out: Vec<String> = Vec::new();
        for v in given {
            let o = ask_text::clean_option(v.as_str().unwrap_or(""));
            let len = o.chars().count();
            if len == 0 {
                return Err(refuse("option_empty", "an option is empty"));
            }
            if len > MAX_OPTION_CHARS {
                return Err(refuse("option_too_long", format!("an option is {len} characters; at most {MAX_OPTION_CHARS}")));
            }
            if ask_text::is_reserved(&o) {
                return Err(refuse("option_reserved", format!("{o:?} is one of Ligus's own control labels; word it differently")));
            }
            if out.iter().any(|x| x.to_lowercase() == o.to_lowercase()) {
                return Err(refuse("options_not_distinct", "two options are the same"));
            }
            out.push(o);
        }
        if out.len() < MIN_OPTIONS || out.len() > MAX_OPTIONS {
            return Err(refuse("options_count", format!("give {MIN_OPTIONS} to {MAX_OPTIONS} options, or yesNo: true")));
        }
        out
    };
    let allow_text = args.get("allowText").and_then(Value::as_bool).unwrap_or(false);
    let secs = args.get("timeoutSecs").and_then(Value::as_u64).unwrap_or(DEFAULT_TIMEOUT_SECS).clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS);
    Ok(Card { question, options, allow_text, timeout: Duration::from_secs(secs) })
}

/// Parse `minRole` (never below Contributor).
pub fn parse_min_role(v: Option<&str>) -> Result<WorkspaceRole, Refusal> {
    match v.map(str::trim).unwrap_or("contributor").to_ascii_lowercase().as_str() {
        "" | "contributor" => Ok(WorkspaceRole::Contributor),
        "admin" => Ok(WorkspaceRole::Admin),
        "owner" => Ok(WorkspaceRole::Owner),
        other => Err(refuse("min_role", format!("minRole is contributor, admin or owner, not {other:?}"))),
    }
}

/// How a card ended for the host.
#[derive(Debug, Clone, PartialEq)]
pub enum CardOutcome {
    Answered(AskState),
    Declined,
    NoAnswer(Vec<u32>),
    Cancelled,
    /// Someone else's record holds the ask id: never waited on.
    Foreign,
}

/// A posted card: the record and the digest of the envelope that signed it.
pub struct PostedCard {
    pub ask: HumanAsk,
    pub digest: String,
}

/// Post `ask` as the turn's agent (the Agent tier; the host's key as the
/// fallback), returning the digest an answer must name.
pub fn post_card(host: &dyn LigusHost, ctx: &EffectCtx, ask: HumanAsk) -> Result<PostedCard, String> {
    let mut r: Result<String, String> = Err("no service".into());
    host.with_service(&mut |svc| {
        r = svc
            .as_agent(ctx.agent_id, |svc| svc.record_flow_signed(ctx.session_id, ctx.workspace_id, FlowRecord::HumanAsk(ask.clone())))
            .map_err(|e| e.to_string())
            .and_then(|env| flow::asks::envelope_ask_digest(&env).ok_or_else(|| "not an ask".to_string()));
    });
    let digest = r?;
    host.ask_posted(ctx.session_id, &ask);
    Ok(PostedCard { ask, digest })
}

/// Close `ask` (timeout, cancel, decline), naming the answer it acted on
/// when it did.
pub fn close_card(host: &dyn LigusHost, ctx: &EffectCtx, ask: &HumanAsk, settled_by: Option<Uuid>) {
    let close = HumanAsk {
        ask_id: ask.ask_id,
        turn_id: ask.turn_id,
        agent_key: ask.agent_key.clone(),
        closed: true,
        at_ms: flow::now_ms(),
        v: ask.v,
        settled_by,
        ..Default::default()
    };
    host.with_service(&mut |svc| {
        let _ = svc.as_agent(ctx.agent_id, |svc| svc.record_flow(ctx.session_id, ctx.workspace_id, FlowRecord::HumanAsk(close.clone())));
    });
}

/// Wait until the fold settles `posted` (an answer that counts, "Not now"),
/// `timeout`, or `cancelled()`. Only the ask this host posted is waited on:
/// its digest must be the one the fold holds.
pub async fn wait_card(host: &dyn LigusHost, chat: Uuid, posted: &PostedCard, timeout: Duration, cancelled: impl Fn() -> bool) -> CardOutcome {
    let deadline = Instant::now() + timeout;
    let ask_id = posted.ask.ask_id;
    loop {
        let mut seen: Option<AskState> = None;
        let mut foreign = false;
        host.with_service(&mut |svc| {
            let device = svc.device_id();
            let Some(st) = svc.load(chat).ok().flatten().and_then(|s| s.flow.asks.get(&ask_id).cloned()) else { return };
            let ours = st.posted_by.is_none()
                && st.host.is_none_or(|h| h == device)
                && st.ask.turn_id == posted.ask.turn_id
                && st.ask.agent_key == posted.ask.agent_key
                && st.ask.question == posted.ask.question
                && st.ask.options == posted.ask.options
                && (st.digest.is_empty() || st.digest == posted.digest);
            if ours {
                seen = Some(st);
            } else {
                foreign = true;
            }
        });
        if foreign {
            tracing::warn!(target: "ligus_mcp", "ask {ask_id} is held by another record; not waiting on it");
            return CardOutcome::Foreign;
        }
        if let Some(st) = &seen {
            if st.answer.is_some() {
                return CardOutcome::Answered(st.clone());
            }
            if st.declined {
                return CardOutcome::Declined;
            }
        }
        if cancelled() {
            return CardOutcome::Cancelled;
        }
        if Instant::now() >= deadline {
            return CardOutcome::NoAnswer(seen.map(|s| s.tally()).unwrap_or_default());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// The settling vote the host acts on: the last of the winners (the one
/// that made the quorum, or the only one).
pub fn acted_on(st: &AskState) -> Option<&AskVote> {
    st.answered_by.last()
}

fn rfc3339(ms: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000)
        .ok()
        .and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok())
        .unwrap_or_default()
}

/// The tool result for an outcome (section 4.2). Names come from the roster.
pub fn result_of(outcome: &CardOutcome, members: &[ligus_core::identity::WorkspaceMember], quorum: u8) -> ToolResult {
    match outcome {
        CardOutcome::Answered(st) => {
            let ans = st.answer.clone().unwrap_or_default();
            let name = |v: &AskVote| {
                members
                    .iter()
                    .find(|m| m.id == v.member_id)
                    .map(|m| ligus_proto::names::clean_display_name(&m.actor.display_name))
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| ligus_proto::names::clean_display_name(&v.name))
            };
            let by: Vec<Value> = st.answered_by.iter().map(|v| json!({"memberId": v.member_id, "name": name(v), "at": rfc3339(v.at_ms)})).collect();
            let option = ans.option_index.and_then(|i| st.ask.options.get(i as usize).cloned());
            let mut v = json!({"status": "answered", "optionIndex": ans.option_index, "option": option, "answeredBy": by});
            if !ans.text.is_empty() {
                v["text"] = json!(ans.text);
            }
            ToolResult::ok(v)
        }
        CardOutcome::Declined => ToolResult::err(Outcome::Ok, json!({"status": "declined"})),
        CardOutcome::NoAnswer(tally) if quorum > 1 => ToolResult::err(Outcome::Ok, json!({"status": "no_answer", "tally": tally})),
        CardOutcome::NoAnswer(_) | CardOutcome::Foreign => ToolResult::err(Outcome::Ok, json!({"status": "no_answer"})),
        CardOutcome::Cancelled => ToolResult::err(Outcome::Ok, json!({"status": "cancelled"})),
    }
}

/// Whether the chat can take another open ask from `agent_key` at `now`.
fn open_caps(session: &ligus_core::ChatSession, agent_key: &str, now: i64) -> Result<(), Refusal> {
    let live: Vec<&AskState> = session.flow.asks.values().filter(|a| a.is_live(now)).collect();
    if live.iter().filter(|a| a.ask.agent_key == agent_key).count() >= MAX_OPEN_PER_AGENT {
        return Err(refuse("too_many_open", format!("you already have {MAX_OPEN_PER_AGENT} open questions in this chat")));
    }
    if live.len() >= MAX_OPEN_PER_CHAT {
        return Err(refuse("too_many_open", format!("this chat already has {MAX_OPEN_PER_CHAT} open questions")));
    }
    Ok(())
}

/// The people who could answer a room ask at `floor` now: human members at
/// or above it.
fn eligible_now(session: &ligus_core::ChatSession, floor: WorkspaceRole) -> usize {
    session
        .members
        .iter()
        .filter(|m| m.actor.kind == ligus_core::ActorKind::Human && m.role.rank() >= floor.rank().max(WorkspaceRole::Contributor.rank()))
        .count()
}

impl LigusMcp {
    /// `ligus_ask_person` (`person`) or `ligus_ask_room`: post a v2 card,
    /// pause the turn, wait, close it, and return the outcome.
    pub(super) async fn ask_card(&self, st: &Arc<TurnState>, ctx: &EffectCtx, args: &Value, person: bool) -> (ToolResult, Option<Uuid>) {
        let r = self.ask_card_inner(st, ctx, args, person).await;
        if r.0.outcome == Outcome::Denied && r.0.text.contains("\"error\"") && !r.0.text.contains("\"status\"") {
            LigusMcp::refund_ask(st);
        }
        r
    }

    async fn ask_card_inner(&self, st: &Arc<TurnState>, ctx: &EffectCtx, args: &Value, person: bool) -> (ToolResult, Option<Uuid>) {
        let card = match parse_card(args) {
            Ok(c) => c,
            Err(r) => return (r.result(), None),
        };
        let chat = ctx.session_id;
        let turn = st.spec.message_id;
        let mut session = None;
        let mut requester = None;
        self.host.with_service(&mut |svc| {
            session = svc.load_in(ctx.workspace_id, chat).ok().flatten();
            if person {
                let trigger = session.as_ref().and_then(|s| s.messages.iter().find(|m| m.id == turn)).and_then(|m| m.reply_to_message_id);
                requester = trigger.and_then(|t| svc.trigger_people_in(ctx.workspace_id, chat, t).ok().flatten()).and_then(|p| p.first().cloned());
            }
        });
        let Some(session) = session else {
            return (refuse("failed", "the chat isn't here").result(), None);
        };
        let now = flow::now_ms();
        if let Err(r) = open_caps(&session, &ctx.agent_key(), now) {
            return (r.result(), None);
        }
        let (audience, quorum) = if person {
            let to = args.get("to").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).unwrap_or("requester");
            let id = if to == "requester" {
                match requester {
                    Some(id) => id,
                    None => return (refuse("no_requester", "no person started this turn; use ligus_ask_room").result(), None),
                }
            } else {
                to.to_string()
            };
            let member = session.members.iter().find(|m| m.id == id);
            match member {
                Some(m) if m.actor.kind == ligus_core::ActorKind::Human && m.role.rank() >= WorkspaceRole::Contributor.rank() => {}
                Some(_) => return (refuse("not_eligible", "that member can't answer questions here").result(), None),
                None => return (refuse("unknown_member", format!("no member {id:?} in this chat")).result(), None),
            }
            (AskAudience::people(vec![id]), 1u8)
        } else {
            let floor = match parse_min_role(args.get("minRole").and_then(Value::as_str)) {
                Ok(r) => r,
                Err(r) => return (r.result(), None),
            };
            let q = args.get("quorum").and_then(Value::as_u64).unwrap_or(1);
            if !(1..=flow::MAX_QUORUM as u64).contains(&q) {
                return (refuse("quorum_range", format!("quorum is 1 to {}", flow::MAX_QUORUM)).result(), None);
            }
            if q as usize > eligible_now(&session, floor) {
                return (refuse("quorum_unreachable", format!("quorum {q} is more than the people who could answer now")).result(), None);
            }
            (AskAudience::room(floor), q as u8)
        };
        let ask = HumanAsk {
            ask_id: Uuid::new_v4(),
            turn_id: turn,
            agent_key: ctx.agent_key(),
            question: card.question.clone(),
            options: card.options.clone(),
            expires_at_ms: now + card.timeout.as_millis() as i64,
            at_ms: now,
            v: ASK_V2,
            audience: Some(audience),
            quorum: (quorum > 1).then_some(quorum),
            allow_text: card.allow_text,
            ..Default::default()
        };
        let posted = match post_card(&*self.host, ctx, ask) {
            Ok(p) => p,
            Err(e) => return (ToolResult::err(Outcome::Denied, json!({"error": "denied", "message": e})), None),
        };
        let _pause = crate::agent_turn::pause_counter(&st.paused);
        let outcome = wait_card(&*self.host, chat, &posted, card.timeout, || st.revoked.load(Ordering::SeqCst)).await;
        match &outcome {
            CardOutcome::Answered(s) => close_card(&*self.host, ctx, &posted.ask, acted_on(s).and_then(|v| v.event_id)),
            CardOutcome::Foreign => {}
            _ => close_card(&*self.host, ctx, &posted.ask, None),
        }
        let mut members = Vec::new();
        self.host.with_service(&mut |svc| {
            members = svc.load_in(ctx.workspace_id, chat).ok().flatten().map(|s| s.members).unwrap_or_default();
        });
        (result_of(&outcome, &members, quorum), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(v: Value) -> &'static str {
        parse_card(&v).err().map(|r| r.code).unwrap_or("ok")
    }

    /// Q-T1: bounds, option counts, yes/no in Ligus's words, reserved labels.
    #[test]
    fn card_arguments_are_bounded_and_cleaned() {
        assert_eq!(code(json!({"question": "", "yesNo": true})), "question_required");
        assert_eq!(code(json!({"question": "q".repeat(501), "yesNo": true})), "question_too_long");
        assert_eq!(code(json!({"question": "q".repeat(500), "yesNo": true})), "ok");
        assert_eq!(code(json!({"question": "q"})), "options_required");
        assert_eq!(code(json!({"question": "q", "options": ["a"]})), "options_count");
        assert_eq!(code(json!({"question": "q", "options": ["a", "b", "c", "d", "e", "f"]})), "options_count");
        assert_eq!(code(json!({"question": "q", "options": ["a", "o".repeat(61)]})), "option_too_long");
        assert_eq!(code(json!({"question": "q", "options": ["Yes", " yes "]})), "options_not_distinct");
        assert_eq!(code(json!({"question": "q", "options": ["Allow\u{200B} once", "b"]})), "option_reserved");
        assert_eq!(code(json!({"question": "q", "options": ["a", "b"], "yesNo": true})), "options_and_yes_no");
        let c = parse_card(&json!({"question": "Ship\u{202E} it?\n\n\n", "yesNo": true, "options": [], "timeoutSecs": 5})).unwrap();
        assert_eq!(c.question, "Ship it?");
        assert_eq!(c.options, vec!["Yes", "No"], "Ligus's words, not the agent's");
        assert_eq!(c.timeout, Duration::from_secs(MIN_TIMEOUT_SECS));
        let c = parse_card(&json!({"question": "q", "options": ["Post\ngres", "SQLite"], "allowText": true})).unwrap();
        assert_eq!(c.options[0], "Post gres");
        assert!(c.allow_text);
        assert_eq!(parse_min_role(Some("viewer")).unwrap_err().code, "min_role");
        assert_eq!(parse_min_role(None).unwrap(), WorkspaceRole::Contributor);
    }

    #[test]
    fn results_name_people_from_the_roster() {
        let mut st = AskState::default();
        st.ask.options = vec!["Postgres".into(), "SQLite".into()];
        st.answer = Some(flow::HumanAnswered { option_index: Some(0), text: "keep the dev db".into(), ..Default::default() });
        st.answered_by = vec![AskVote { member_id: "m1".into(), name: "Mallory".into(), at_ms: 1_764_064_931_000, ..Default::default() }];
        let m = ligus_core::identity::WorkspaceMember {
            id: "m1".into(),
            actor: ligus_core::ActorIdentity::new("m1", "Ana", ligus_core::ActorKind::Human),
            role: WorkspaceRole::Owner,
            title: String::new(),
            index: 1,
            joined_at: Default::default(),
        };
        let r = result_of(&CardOutcome::Answered(st), &[m], 1);
        let v: Value = serde_json::from_str(&r.text).unwrap();
        assert!(!r.is_error);
        assert_eq!(v["status"], "answered");
        assert_eq!(v["option"], "Postgres");
        assert_eq!(v["text"], "keep the dev db");
        assert_eq!(v["answeredBy"][0]["name"], "Ana", "the roster's name, not the stamp's");
        assert!(v["answeredBy"][0]["at"].as_str().unwrap().starts_with("2025-11-25T"));
        for (o, s) in [(CardOutcome::Declined, "declined"), (CardOutcome::Cancelled, "cancelled"), (CardOutcome::NoAnswer(vec![1, 0]), "no_answer")] {
            let r = result_of(&o, &[], 2);
            assert!(r.is_error);
            let v: Value = serde_json::from_str(&r.text).unwrap();
            assert_eq!(v["status"], s);
        }
        let v: Value = serde_json::from_str(&result_of(&CardOutcome::NoAnswer(vec![1, 0]), &[], 2).text).unwrap();
        assert_eq!(v["tally"], json!([1, 0]));
    }
}
