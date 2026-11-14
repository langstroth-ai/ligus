//! Track D2 proof tests for the Ligus MCP server.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use hive_core::crypto::SigningKeypair;
use hive_core::events::SessionEvent;
use hive_core::flow::{FlowRecord, HumanAnswered};
use hive_core::{ActorIdentity, ActorKind, WorkspaceAgent};
use serde_json::{json, Value};
use uuid::Uuid;

use super::cred::{self, TurnBinding};
use super::server::{Budgets, LigusHost, LigusMcp, Outcome, TurnSpec};
use crate::event_store::EventStore;
use crate::ChatService;

pub(crate) struct Host {
    pub svc: Mutex<ChatService>,
    pub started: Mutex<Vec<(Uuid, Uuid, Uuid)>>,
}

impl LigusHost for Host {
    fn with_service(&self, f: &mut dyn FnMut(&mut ChatService)) {
        f(&mut self.svc.lock().unwrap())
    }
    fn start_turn(&self, chat: Uuid, agent: Uuid, trigger: Uuid) {
        self.started.lock().unwrap().push((chat, agent, trigger));
    }
}

pub(crate) struct World {
    pub host: Arc<Host>,
    pub mcp: Arc<LigusMcp>,
    pub wid: Uuid,
    pub chat: Uuid,
    pub agent: Uuid,
    pub reviewer: Uuid,
    pub reply: Uuid,
    pub _dir: tempfile::TempDir,
}

/// A workspace with a chat, an agent `coder` bound to this device (so it
/// holds its own certified key), a second agent `reviewer`, and a streaming
/// reply from `coder`.
pub(crate) fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let store = EventStore::open(dir.path().join("hive.db")).unwrap();
    let kp = SigningKeypair::generate().unwrap();
    let account_kp = SigningKeypair::generate().unwrap();
    let mut svc = ChatService::new(store, Uuid::new_v4(), kp, account_kp, ActorIdentity::new("x", "Alex", ActorKind::Human));
    let account = Uuid::new_v4().to_string();
    svc.set_author_account(account.clone(), "Alex", None);
    let wid = Uuid::new_v4();
    let chat = svc.create_chat("A", wid, "rt").unwrap();
    svc.set_agent_vault(Box::new(crate::identity_store::FileKeyVault::new(dir.path())));
    let mut agent = WorkspaceAgent::new("coder", "rt");
    agent.owner_actor_id = account.clone();
    let agent_id = agent.id;
    svc.add_agent(chat.id, wid, agent).unwrap();
    svc.set_agent_host(wid, agent_id, &svc.device_id().to_string()).unwrap();
    let mut rev = WorkspaceAgent::new("reviewer", "rt");
    rev.owner_actor_id = account;
    let reviewer = rev.id;
    svc.add_agent(chat.id, wid, rev).unwrap();
    svc.post_user_message(chat.id, wid, "please fix the bug").unwrap();
    let reply = svc.begin_assistant_message(chat.id, wid, "coder", "rt", Some(agent_id), None).unwrap();
    let host = Arc::new(Host { svc: Mutex::new(svc), started: Mutex::new(Vec::new()) });
    let mcp = LigusMcp::new(host.clone());
    World { host, mcp, wid, chat: chat.id, agent: agent_id, reviewer, reply, _dir: dir }
}

pub(crate) fn spec(w: &World, budgets: Budgets) -> TurnSpec {
    TurnSpec {
        workspace_id: w.wid,
        chat_id: w.chat,
        agent_id: Some(w.agent),
        author: "coder".into(),
        message_id: w.reply,
        worktree: None,
        budgets,
        ttl: Duration::from_secs(600),
    }
}

fn body(r: &super::server::ToolResult) -> Value {
    serde_json::from_str(&r.text).unwrap_or(Value::Null)
}

fn audit(w: &World) -> Vec<Value> {
    let svc = w.host.svc.lock().unwrap();
    svc.export_facts(0).unwrap().iter().map(|l| serde_json::from_str::<Value>(l).unwrap()).filter(|v| v["kind"] == "toolCall").collect()
}

#[tokio::test]
async fn mcp_cred_bound_to_chat_agent_turn() {
    let w = world();
    let turn = w.mcp.begin_turn(spec(&w, Budgets::default()));
    assert!(w.mcp.authorize(turn.credential()).is_ok());
    // Same host key, but the binding names another chat / agent / turn.
    let exp = cred::now_unix() + 600;
    for b in [
        TurnBinding { turn_id: w.reply, chat_id: Uuid::new_v4(), agent_id: w.agent, expires_unix: exp },
        TurnBinding { turn_id: w.reply, chat_id: w.chat, agent_id: w.reviewer, expires_unix: exp },
        TurnBinding { turn_id: Uuid::new_v4(), chat_id: w.chat, agent_id: w.agent, expires_unix: exp },
    ] {
        let forged = cred::mint(&w.mcp.key, &b);
        assert!(w.mcp.authorize(&forged).is_err(), "{b:?}");
        assert!(w.mcp.call(&forged, "ligus_read_board", &json!({})).await.is_err());
    }
    // Another host's key.
    let foreign = cred::mint(&cred::host_key(), &TurnBinding { turn_id: w.reply, chat_id: w.chat, agent_id: w.agent, expires_unix: exp });
    assert!(w.mcp.authorize(&foreign).is_err());
    drop(turn);
}

#[tokio::test]
async fn mcp_cred_expires_at_turn_end() {
    let w = world();
    let turn = w.mcp.begin_turn(spec(&w, Budgets::default()));
    let c = turn.credential().to_string();
    assert!(w.mcp.call(&c, "ligus_read_board", &json!({})).await.is_ok());
    turn.finish();
    // Revoked at turn end, even though its expiry hasn't passed.
    assert!(w.mcp.call(&c, "ligus_read_board", &json!({})).await.is_err(), "no reuse after the turn");
    // A new turn of the same agent in the same chat doesn't revive it.
    let mut s2 = spec(&w, Budgets::default());
    s2.message_id = Uuid::new_v4();
    let t2 = w.mcp.begin_turn(s2);
    assert!(w.mcp.call(&c, "ligus_read_board", &json!({})).await.is_err(), "cross-turn reuse refused");
    assert!(w.mcp.call(t2.credential(), "ligus_read_board", &json!({})).await.is_ok());
    // An expired credential is refused by its MAC'd expiry.
    let past = cred::mint(&w.mcp.key, &TurnBinding { turn_id: t2.turn_id(), chat_id: w.chat, agent_id: w.agent, expires_unix: cred::now_unix() - 1 });
    assert!(matches!(w.mcp.authorize(&past), Err(super::server::AuthError::Credential(cred::CredError::Expired))));
}

#[tokio::test]
async fn mcp_effects_signed_by_agent_key() {
    use crate::envelope_verifier::{build_roster, verdict_for, Verdict};
    let w = world();
    let turn = w.mcp.begin_turn(spec(&w, Budgets::default()));
    let r = turn.call("ligus_propose_command", &json!({"title": "Run the tests", "body": "to check", "argv": ["cargo", "test"]})).await;
    assert!(!r.is_error, "{}", r.text);
    let pid = Uuid::parse_str(body(&r)["proposalId"].as_str().unwrap()).unwrap();
    let svc = w.host.svc.lock().unwrap();
    let env = svc
        .store()
        .list(w.chat)
        .unwrap()
        .into_iter()
        .find(|e| matches!(&e.payload, SessionEvent::ProposalUpserted { proposal } if proposal.id == pid))
        .unwrap();
    let stamp = env.actor_stamp.as_ref().unwrap();
    assert_eq!(stamp.actor.kind, ActorKind::Agent);
    assert_eq!(stamp.actor.id, w.agent.to_string());
    assert_ne!(env.signer_device_id, Some(svc.device_id()), "signed by the agent's key, not the host's");
    let roster = build_roster(&svc.store().trust_envelopes(w.wid).unwrap());
    assert_eq!(verdict_for(&roster, &env), Verdict::Valid);
    let p = svc.load(w.chat).unwrap().unwrap().proposals.into_iter().find(|p| p.id == pid).unwrap();
    assert_eq!(p.producing_agent_id, Some(w.agent));
    assert_eq!(p.command.unwrap().argv, vec!["cargo", "test"]);
}

/// Every kind of event a Ligus tool call may leave in a log.
fn allowed_effect(e: &SessionEvent) -> bool {
    match e {
        SessionEvent::ProposalUpserted { proposal } => proposal.approvals.is_empty() && proposal.status == hive_core::ProposalStatus::Open,
        SessionEvent::ProposalSetOpened { .. }
        | SessionEvent::ProposalSetAlternativeAdded { .. }
        | SessionEvent::ScoutAssessmentPosted { .. }
        | SessionEvent::MessageReactionAdded { .. }
        // The host's own chat notes about an effect.
        | SessionEvent::MessageAppended { .. } => true,
        SessionEvent::FlowRecorded { record } => matches!(
            record,
            FlowRecord::Mark(_) | FlowRecord::MarkCleared { .. } | FlowRecord::Handoff(_) | FlowRecord::HumanAsk(_) | FlowRecord::TurnToolSummary(_)
        ),
        _ => false,
    }
}

/// `mcp_never_emits_admin_or_vote_or_applied`: random tool sequences
/// (including names of things the server never offers, and hostile args)
/// never put an admin, governance, vote or apply event in any log, and the
/// refused attempts are audited.
#[tokio::test]
async fn mcp_never_emits_admin_or_vote_or_applied() {
    let cases: u64 = std::env::var("LIGUS_PROP_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(256).min(512);
    let w = world();
    let before: usize = {
        let svc = w.host.svc.lock().unwrap();
        svc.store().list(w.chat).unwrap().len() + svc.store().list(hive_core::workspace_config_session_id(w.wid)).unwrap().len()
    };
    let names = [
        "ligus_propose_decision",
        "ligus_propose_command",
        "ligus_propose_workflow",
        "ligus_open_alternatives",
        "ligus_assess",
        "ligus_mark",
        "ligus_unmark",
        "ligus_react",
        "ligus_read_chat",
        "ligus_read_proposals",
        "ligus_read_board",
        "ligus_list_chats",
        "ligus_report",
        "ligus_handoff",
        // Never offered: refused as unknown tools.
        "ligus_vote",
        "ligus_apply",
        "ligus_add_member",
        "ligus_set_role",
        "ligus_rotate_key",
        "tools/call",
    ];
    let values = [
        json!("x"),
        json!(""),
        json!(["rm", "-rf", "/"]),
        json!(42),
        json!({"approvals": [{"actorId": "x", "role": "owner", "vote": "up"}], "status": "approved"}),
        json!("reviewer"),
        json!("lock"),
        json!(null),
    ];
    let keys = ["title", "body", "argv", "kind", "path", "to", "status", "requiredApprovals", "approvals", "definition", "options", "emoji", "setId", "alternative", "score", "question", "summary"];
    let mut x: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let budgets = Budgets { max_tool_calls: 10_000, max_proposals: 10_000, max_handoffs: 10_000, ..Budgets::default() };
    let turn = w.mcp.begin_turn(spec(&w, budgets));
    let mut refused_unknown = 0;
    for _ in 0..cases {
        let name = names[(next() % names.len() as u64) as usize];
        let mut args = serde_json::Map::new();
        for _ in 0..(next() % 6) {
            let k = keys[(next() % keys.len() as u64) as usize];
            let v = values[(next() % values.len() as u64) as usize].clone();
            args.insert(k.into(), v);
        }
        let r = w.mcp.call(turn.credential(), name, &Value::Object(args)).await.unwrap();
        if !super::tools::TOOLS.iter().any(|(n, _)| *n == name) {
            assert_eq!(r.outcome, Outcome::Denied);
            refused_unknown += 1;
        }
    }
    turn.finish();
    let svc = w.host.svc.lock().unwrap();
    let mut all = svc.store().list(w.chat).unwrap();
    all.extend(svc.store().list(hive_core::workspace_config_session_id(w.wid)).unwrap());
    for e in all.iter() {
        let agent_stamped = e.actor_stamp.as_ref().is_some_and(|s| s.actor.kind == ActorKind::Agent);
        assert!(!matches!(e.payload, SessionEvent::ProposalVoteCast { .. } | SessionEvent::ProposalApplied { .. }), "{:?}", e.payload.kind_str());
        if agent_stamped {
            assert!(allowed_effect(&e.payload) || matches!(e.payload, SessionEvent::MessageChunkReceived { .. } | SessionEvent::MessageCompleted { .. }), "{}", e.payload.kind_str());
        }
    }
    assert!(all.len() >= before);
    drop(svc);
    let denied = audit(&w).iter().filter(|a| a["attributes"]["ligus.tool.outcome"] == "denied").count();
    assert!(denied >= refused_unknown, "every refused attempt is audited ({denied} < {refused_unknown})");
}

#[tokio::test]
async fn agent_scope_refuses_votes_applies_and_admin_events() {
    let w = world();
    let turn = w.mcp.begin_turn(spec(&w, Budgets::default()));
    let r = turn.call("ligus_propose_decision", &json!({"title": "Ship it", "body": "b"})).await;
    let pid = Uuid::parse_str(body(&r)["proposalId"].as_str().unwrap()).unwrap();
    let mut svc = w.host.svc.lock().unwrap();
    // Even if a code path inside an agent's scope tried, the tier refuses.
    let vote = svc.as_agent(Some(w.agent), |svc| svc.vote_on_proposal(w.chat, w.wid, pid, true));
    assert!(vote.is_err(), "an agent never votes");
    let apply = svc.as_agent(Some(w.agent), |svc| svc.mark_proposal_applied(w.chat, w.wid, pid, None));
    assert!(!matches!(apply, Ok(Some(_))), "an agent never applies");
    assert!(!svc.store().list(w.chat).unwrap().iter().any(|e| matches!(e.payload, SessionEvent::ProposalApplied { .. } | SessionEvent::ProposalVoteCast { .. })));
    let admin = svc.as_agent(Some(w.agent), |svc| svc.set_agent_host(w.wid, w.agent, "elsewhere"));
    assert!(admin.is_err(), "an agent never rebinds hosts");
    let answer = svc.as_agent(Some(w.agent), |svc| {
        svc.record_flow(w.chat, w.wid, FlowRecord::HumanAnswered(HumanAnswered { ask_id: Uuid::new_v4(), answer: "yes".into(), ..Default::default() }))
    });
    assert!(answer.is_err(), "an agent never answers an ask");
}

#[tokio::test]
async fn budgets_enforced() {
    let w = world();
    let b = Budgets { max_tool_calls: 6, max_proposals: 2, max_handoffs: 1, max_asks: 0, max_read_messages: 1, max_spend_usd: Some(1.0) };
    let turn = w.mcp.begin_turn(spec(&w, b));
    for i in 0..2 {
        assert!(!turn.call("ligus_propose_decision", &json!({"title": format!("d{i}"), "body": "b"})).await.is_error);
    }
    let r = turn.call("ligus_propose_decision", &json!({"title": "d3", "body": "b"})).await;
    assert!(r.is_error);
    assert_eq!(body(&r)["error"], "budget_exhausted");
    assert_eq!(body(&r)["budget"], "maxProposals");
    assert!(body(&r)["message"].as_str().unwrap().contains("Tool budget"), "the final-round nudge rides along");
    let r = turn.call("ligus_ask_human", &json!({"question": "?"})).await;
    assert_eq!(body(&r)["budget"], "maxAsks");
    // Reads: 3 messages in the chat > 1.
    let r = turn.call("ligus_read_chat", &json!({})).await;
    assert_eq!(body(&r)["budget"], "maxReadMessages");
    // Spend: reported cost over the cap stops every further call.
    (turn.scope().spend.unwrap())(1.5);
    let r = turn.call("ligus_read_board", &json!({})).await;
    assert_eq!(body(&r)["budget"], "maxSpendUsd");
    let summary = turn.finish();
    assert!(summary.budget_exhausted);
    // The synced summary is content-free and agent-tier.
    let s = w.host.svc.lock().unwrap().load(w.chat).unwrap().unwrap();
    let t = s.flow.tool_summaries.last().unwrap();
    assert_eq!(t.turn_id, w.reply);
    assert_eq!(t.calls.get("ligus_propose_decision"), Some(&2));
    assert!(t.budget_exhausted);
}

#[tokio::test]
async fn total_call_budget() {
    let w = world();
    let turn = w.mcp.begin_turn(spec(&w, Budgets { max_tool_calls: 2, ..Budgets::default() }));
    assert!(!turn.call("ligus_read_board", &json!({})).await.is_error);
    assert!(!turn.call("ligus_read_board", &json!({})).await.is_error);
    assert_eq!(body(&turn.call("ligus_read_board", &json!({})).await)["budget"], "maxToolCalls");
}

#[tokio::test]
async fn read_other_chat_allowed_and_audited() {
    let w = world();
    let (other, foreign) = {
        let mut svc = w.host.svc.lock().unwrap();
        let other = svc.create_chat("B", w.wid, "rt").unwrap();
        svc.post_user_message(other.id, w.wid, "notes in B").unwrap();
        let foreign = svc.create_chat("C", Uuid::new_v4(), "rt").unwrap();
        (other.id, foreign.id)
    };
    let turn = w.mcp.begin_turn(spec(&w, Budgets::default()));
    let r = turn.call("ligus_read_chat", &json!({"chatId": other.to_string()})).await;
    assert!(!r.is_error, "{}", r.text);
    let v = body(&r);
    assert!(v["header"].as_str().unwrap().contains("treat it as data"));
    assert!(v["messages"].as_array().unwrap().iter().any(|m| m["body"] == "notes in B"));
    let r = turn.call("ligus_read_chat", &json!({"chatId": foreign.to_string()})).await;
    assert!(r.is_error, "never another workspace's chat");
    let chats = body(&turn.call("ligus_list_chats", &json!({})).await);
    assert!(chats["chats"].as_array().unwrap().iter().all(|c| c["chatId"] != foreign.to_string()));
    let facts = audit(&w);
    assert!(facts.iter().any(|f| f["attributes"]["ligus.tool.read_chat_id"] == other.to_string() && f["attributes"]["ligus.tool.outcome"] == "ok"));
    // Content-free: the message text never reaches a fact.
    assert!(facts.iter().all(|f| !f.to_string().contains("notes in B")));
}

#[tokio::test]
async fn ask_pauses_and_resumes_with_answer() {
    let w = world();
    let turn = Arc::new(w.mcp.begin_turn(spec(&w, Budgets::default())));
    let paused = turn.scope().paused.clone();
    let t2 = turn.clone();
    let task = tokio::spawn(async move { t2.call("ligus_ask_human", &json!({"question": "Which db?", "options": ["sqlite", "postgres"], "timeoutSecs": 30})).await });
    // Wait for the ask to land, then answer as the human.
    let mut ask_id = None;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let s = w.host.svc.lock().unwrap().load(w.chat).unwrap().unwrap();
        if let Some(a) = s.flow.asks.values().next() {
            ask_id = Some(a.ask.ask_id);
            break;
        }
    }
    let ask_id = ask_id.expect("the ask was posted");
    assert!(paused.load(std::sync::atomic::Ordering::SeqCst) > 0, "the turn is paused while it waits");
    w.host
        .svc
        .lock()
        .unwrap()
        .record_flow(w.chat, w.wid, FlowRecord::HumanAnswered(HumanAnswered { ask_id, answer: "postgres".into(), option_index: Some(1), by: "Alex".into(), at_ms: 1 }))
        .unwrap();
    let r = task.await.unwrap();
    assert!(!r.is_error, "{}", r.text);
    assert_eq!(body(&r)["answer"], "postgres");
    assert_eq!(body(&r)["optionIndex"], 1);
    assert_eq!(paused.load(std::sync::atomic::Ordering::SeqCst), 0, "resumed");
    // The ask itself is agent-signed.
    let svc = w.host.svc.lock().unwrap();
    let ask_env = svc.store().list(w.chat).unwrap().into_iter().find(|e| matches!(&e.payload, SessionEvent::FlowRecorded { record: FlowRecord::HumanAsk(_) })).unwrap();
    assert_eq!(ask_env.actor_stamp.unwrap().actor.kind, ActorKind::Agent);
}

#[tokio::test]
async fn ask_times_out() {
    let w = world();
    let turn = w.mcp.begin_turn(spec(&w, Budgets::default()));
    let r = turn.call("ligus_ask_human", &json!({"question": "Anyone?", "timeoutSecs": 1})).await;
    assert_eq!(body(&r)["error"], "no_answer");
    let s = w.host.svc.lock().unwrap().load(w.chat).unwrap().unwrap();
    let a = s.flow.asks.values().next().unwrap();
    assert!(!a.is_open(), "closed on timeout");
    // A late answer doesn't count.
    let id = a.ask.ask_id;
    w.host.svc.lock().unwrap().record_flow(w.chat, w.wid, FlowRecord::HumanAnswered(HumanAnswered { ask_id: id, answer: "late".into(), ..Default::default() })).unwrap();
    let s = w.host.svc.lock().unwrap().load(w.chat).unwrap().unwrap();
    assert!(s.flow.asks[&id].answer.is_none());
}

#[tokio::test]
async fn handoff_starts_the_receiver_once() {
    let w = world();
    let turn = w.mcp.begin_turn(spec(&w, Budgets::default()));
    let r = turn.call("ligus_handoff", &json!({"to": "reviewer", "keyFiles": ["src/lib.rs"], "notes": "check the fix"})).await;
    assert!(!r.is_error, "{}", r.text);
    assert_eq!(body(&r)["receiverStarted"], true);
    assert_eq!(*w.host.started.lock().unwrap(), vec![(w.chat, w.reviewer, w.reply)]);
    // To itself: refused.
    assert!(turn.call("ligus_handoff", &json!({"to": "coder"})).await.is_error);
    let s = w.host.svc.lock().unwrap().load(w.chat).unwrap().unwrap();
    let card = s.flow.handoffs.last().unwrap();
    assert_eq!(card.via, "tool");
    assert_eq!(card.parcel.as_ref().unwrap().key_files, vec!["src/lib.rs"]);
}

/// The comparable shape of a chat's agent effects: proposals, sets, marks,
/// hand-offs.
type Effects = (Vec<(String, String, String, bool)>, Vec<(String, usize)>, Vec<(String, String)>, Vec<(String, Vec<String>)>);

fn effects_of(w: &World) -> Effects {
    let svc = w.host.svc.lock().unwrap();
    let s = svc.load(w.chat).unwrap().unwrap();
    let mut props: Vec<_> = s
        .proposals
        .iter()
        .filter(|p| p.set_id.is_none())
        .map(|p| (crate::finalize::kind_key(p.kind), p.title.clone(), p.body.clone(), p.producing_agent_id == Some(w.agent)))
        .collect();
    props.sort();
    let sets: Vec<_> = s.proposal_sets.iter().map(|x| (x.question.clone(), x.alternatives.len())).collect();
    let marks: Vec<_> = svc.flow_config(w.wid).unwrap().marks.values().map(|m| (m.path.clone(), m.kind.as_str().to_string())).collect();
    let hand: Vec<_> = s.flow.handoffs.iter().map(|h| (h.to_name.clone(), h.parcel.as_ref().map(|p| p.key_files.clone()).unwrap_or_default())).collect();
    (props, sets, marks, hand)
}

/// `directive_fallback_parity`: a runtime without tool use writing
/// directives and one calling the Ligus tools produce the same effects.
#[tokio::test]
async fn directive_fallback_parity() {
    // Through MCP.
    let a = world();
    let turn = a.mcp.begin_turn(spec(&a, Budgets::default()));
    for (tool, args) in [
        ("ligus_propose_decision", json!({"title": "Use sqlite", "body": "smaller"})),
        ("ligus_open_alternatives", json!({"question": "Which cache?", "options": [{"title": "LRU", "body": "a"}, {"title": "LFU", "body": "b"}]})),
        ("ligus_mark", json!({"kind": "lock", "path": "src/db.rs"})),
        ("ligus_handoff", json!({"to": "reviewer", "keyFiles": ["src/db.rs"]})),
    ] {
        let r = turn.call(tool, &args).await;
        assert!(!r.is_error, "{tool}: {}", r.text);
    }
    turn.finish();

    // Through directives, on a host that applies flow directives (worker/serve).
    let b = world();
    let full = "Done.\n[[propose: {\"title\": \"Use sqlite\", \"kind\": \"decision\", \"body\": \"smaller\"}]]\n\
[[alternatives: {\"question\": \"Which cache?\", \"options\": [{\"title\": \"LRU\", \"body\": \"a\"}, {\"title\": \"LFU\", \"body\": \"b\"}]}]]\n\
[[mark: {\"kind\": \"lock\", \"path\": \"src/db.rs\"}]]\n[[handoff: {\"to\": \"reviewer\", \"keyFiles\": [\"src/db.rs\"]}]]";
    {
        let mut svc = b.host.svc.lock().unwrap();
        let out = crate::finalize::finalize_reply(
            &mut svc,
            crate::finalize::FinalizeInput {
                ctx: EffectCtxB::ctx(&b),
                workspace_root: "",
                full,
                edits_in_review: false,
                mcp: None,
                base_commit: None,
                apply_flow: true,
            },
        )
        .unwrap();
        assert_eq!(out.cleaned, "Done.");
        assert_eq!(out.handoffs.len(), 1);
    }
    assert_eq!(effects_of(&a), effects_of(&b));
}

struct EffectCtxB;
impl EffectCtxB {
    fn ctx(w: &World) -> crate::effects::EffectCtx {
        crate::effects::EffectCtx {
            session_id: w.chat,
            workspace_id: w.wid,
            author: "coder".into(),
            agent_id: Some(w.agent),
            message_id: Some(w.reply),
            via: crate::effects::Via::Directive,
        }
    }
}

/// When a turn both calls the tools and writes the same directives, the
/// MCP effects win and the duplicates are dropped (D.4.4).
#[tokio::test]
async fn mcp_effects_win_over_duplicate_directives() {
    let w = world();
    let turn = w.mcp.begin_turn(spec(&w, Budgets::default()));
    assert!(!turn.call("ligus_propose_decision", &json!({"title": "Use sqlite", "body": "x"})).await.is_error);
    assert!(!turn.call("ligus_mark", &json!({"kind": "claim", "path": "a.rs"})).await.is_error);
    let log = turn.effects();
    turn.finish();
    let full = "ok\n[[propose: {\"title\": \"use SQLite\", \"kind\": \"decision\"}]]\n[[propose: {\"title\": \"Another\", \"kind\": \"decision\"}]]\n[[mark: {\"kind\": \"claim\", \"path\": \"a.rs\"}]]";
    let mut svc = w.host.svc.lock().unwrap();
    crate::finalize::finalize_reply(
        &mut svc,
        crate::finalize::FinalizeInput { ctx: EffectCtxB::ctx(&w), workspace_root: "", full, edits_in_review: false, mcp: Some(&log), base_commit: None, apply_flow: true },
    )
    .unwrap();
    let s = svc.load(w.chat).unwrap().unwrap();
    let titles: Vec<_> = s.proposals.iter().map(|p| p.title.to_lowercase()).collect();
    assert_eq!(titles.iter().filter(|t| *t == "use sqlite").count(), 1);
    assert!(titles.contains(&"another".to_string()));
    assert_eq!(svc.flow_config(w.wid).unwrap().marks.len(), 1);
}

/// `handoff_starts_receiver_on_every_host`: a tool hand-off queues the
/// receiver in `pending_turns` (what every host's queue drains), exactly
/// once under duplicate delivery, and the receiver's reply clears it.
#[tokio::test]
async fn handoff_starts_receiver_on_every_host() {
    let w = world();
    let turn = w.mcp.begin_turn(spec(&w, Budgets::default()));
    assert!(!turn.call("ligus_handoff", &json!({"to": "reviewer"})).await.is_error);
    turn.finish();
    let mut svc = w.host.svc.lock().unwrap();
    svc.complete_assistant_message(w.chat, w.wid, w.reply, "over to reviewer").unwrap();
    let s = svc.load(w.chat).unwrap().unwrap();
    let pending: Vec<_> = crate::pending_turns(&s).into_iter().filter(|p| p.agent_id == w.reviewer).collect();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].message_id, w.reply, "the receiver answers the handing reply");
    // A duplicate delivery of the same card folds to one entry.
    let card = s.flow.handoffs.last().unwrap().clone();
    svc.record_flow(w.chat, w.wid, FlowRecord::Handoff(card)).unwrap();
    let s = svc.load(w.chat).unwrap().unwrap();
    assert_eq!(crate::pending_turns(&s).iter().filter(|p| p.agent_id == w.reviewer).count(), 1);
    // The receiver replies: nothing left to run.
    let r = svc.begin_assistant_message(w.chat, w.wid, "reviewer", "rt", Some(w.reviewer), Some(w.reply)).unwrap();
    svc.complete_assistant_message(w.chat, w.wid, r, "looks good").unwrap();
    let s = svc.load(w.chat).unwrap().unwrap();
    assert!(crate::pending_turns(&s).iter().all(|p| p.agent_id != w.reviewer));
}

/// `agent_cannot_answer_ask`: an agent's effect scope refuses
/// `HumanAnswered`, and a person's answer is what the asking tool returns.
#[tokio::test]
async fn agent_cannot_answer_ask() {
    let w = world();
    let mut svc = w.host.svc.lock().unwrap();
    let ask_id = Uuid::new_v4();
    let ctx = crate::effects::EffectCtx { session_id: w.chat, workspace_id: w.wid, author: "coder".into(), agent_id: Some(w.agent), message_id: Some(w.reply), via: crate::effects::Via::Mcp };
    let ask = hive_core::flow::HumanAsk { ask_id, turn_id: w.reply, agent_key: ctx.agent_key(), question: "ok?".into(), at_ms: 1, ..Default::default() };
    svc.as_agent(Some(w.agent), |svc| svc.record_flow(w.chat, w.wid, FlowRecord::HumanAsk(ask))).unwrap();
    for agent in [w.agent, w.reviewer] {
        let r = svc.as_agent(Some(agent), |svc| {
            svc.record_flow(w.chat, w.wid, FlowRecord::HumanAnswered(HumanAnswered { ask_id, answer: "yes".into(), ..Default::default() }))
        });
        assert!(r.is_err(), "agent {agent} answered");
    }
    assert!(svc.load(w.chat).unwrap().unwrap().flow.asks[&ask_id].is_open());
    // Another agent can't post an ask as this one, either.
    let forged = hive_core::flow::HumanAsk { ask_id: Uuid::new_v4(), turn_id: w.reply, agent_key: ctx.agent_key(), question: "?".into(), at_ms: 2, ..Default::default() };
    assert!(svc.as_agent(Some(w.reviewer), |svc| svc.record_flow(w.chat, w.wid, FlowRecord::HumanAsk(forged))).is_err());
}

/// An ACP permission escalation is an authorization: "Allow once" lets the
/// agent run a command or edit on this host. Any other member (a Viewer, a
/// teammate on another device) answering first must not grant it; only the
/// host's own person (or the agent's owner) can.
#[tokio::test]
async fn permission_escalation_needs_the_hosts_own_person() {
    for (answerer, expect_allowed) in [("bob", false), ("host", true)] {
        let w = world();
        let turn = w.mcp.begin_turn(spec(&w, Budgets::default()));
        let asker = turn.asker();
        let host_id = w.host.svc.lock().unwrap().author().id.clone();
        let host = w.host.clone();
        let (chat, wid) = (w.chat, w.wid);
        let answer = tokio::spawn(async move {
            let mut ask_id = None;
            for _ in 0..200 {
                tokio::time::sleep(Duration::from_millis(20)).await;
                let s = host.svc.lock().unwrap().load(chat).unwrap().unwrap();
                if let Some(a) = s.flow.asks.values().next() {
                    ask_id = Some(a.ask.ask_id);
                    break;
                }
            }
            let ask_id = ask_id.expect("the escalation was posted");
            let mut svc = host.svc.lock().unwrap();
            if answerer == "bob" {
                // A teammate with write access (a Viewer can't record flow at all).
                let bob = Uuid::new_v4().to_string();
                let member = hive_core::WorkspaceMember {
                    id: bob.clone(),
                    actor: ActorIdentity::new(bob.as_str(), "Bob", ActorKind::Human),
                    role: hive_core::WorkspaceRole::Contributor,
                    title: String::new(),
                    index: 0,
                    joined_at: hive_core::Timestamp::now(),
                };
                svc.add_member(chat, wid, member).unwrap();
                svc.set_author_account(bob, "Bob", None);
            }
            svc.record_flow(chat, wid, FlowRecord::HumanAnswered(HumanAnswered { ask_id, answer: "Allow once".into(), option_index: Some(0), by: "Alex".into(), at_ms: 1 }))
                .unwrap();
            if answerer == "bob" {
                svc.set_author_account(host_id, "Alex", None);
            }
        });
        let got = asker.ask("coder asks permission to run rm (execute). Allow it once?".into(), vec!["Allow once".into(), "Deny".into()], Duration::from_secs(10)).await;
        answer.await.unwrap();
        assert_eq!(got.is_some_and(|a| a.option_index == Some(0)), expect_allowed, "answered by {answerer}");
    }
}
