//! Workspace model v3, 15 and 16.3 (L6, L7, L8): the released 0.4.1 build
//! and this one on the same logs once rules 4 exists.
//!
//! - Rules 3, with every 0.5 addition interleaved (`turnDelegated`,
//!   `memberLeft`, agents carrying `run` and `placement`, host `offers`, a
//!   completion's `outcome`): every verdict of a pre-existing kind, the
//!   roster and the agents' legacy fields agree (L6).
//! - Rules 4 set, as 0.5 founds a workspace and as an Owner switches one:
//!   the 0.4.1 core reports the workspace read-only, nothing before the
//!   switch folds differently, and this core applies the gates (L7).
//! - The pinned 0.4.1 runtime ingests every 0.5 event and stores it byte
//!   for byte, then refuses every write on a rules-4 workspace: no message,
//!   no turn claim, no placeholder, so it never starts a turn and never
//!   calls a model (L8).

use ligus_core::agent_run::{legacy_id, AgentRun, Placement, ResolvedRun};
use ligus_core::testkit::SignedWorld;
use ligus_core::{ChatMessage, HostKind, MessageRole, SessionEvent, SessionEventEnvelope, WorkspaceAgent, WorkspaceHost, WorkspaceRole};
use uuid::Uuid;

use ligus_core_v04 as old;
use ligus_runtime_v04 as old_rt;

fn to_old(envs: &[SessionEventEnvelope]) -> Vec<old::SessionEventEnvelope> {
    envs.iter()
        .map(|e| serde_json::from_str::<old::SessionEventEnvelope>(&serde_json::to_string(e).unwrap()).expect("0.4 reads every envelope"))
        .collect()
}

fn host_of(w: &SignedWorld, who: &str) -> String {
    w.people[who].device_id.to_string()
}

/// Ana (Owner), Ben (Admin), Cy (Contributor) with a host offering
/// `claude-code`, on rules 3, with a chat Cy started.
fn base() -> (SignedWorld, Uuid) {
    let mut w = SignedWorld::genesis(&["ana", "ben", "cy"], None);
    w.admit("ana", "ben", WorkspaceRole::Admin);
    w.admit("ana", "cy", WorkspaceRole::Contributor);
    w.push("ana", SessionEvent::WorkspaceRulesSet { version: 3, presence: None });
    let mut h = WorkspaceHost::new(host_of(&w, "cy"), HostKind::Device, "cy-mbp");
    h.owner_actor_id = "cy".into();
    h.offers = Some(vec![ligus_core::workspace_host::HostOffer { name: "claude-code".into(), kind: ligus_core::workspace_host::OfferKind::Cli, harness: "claudeCode".into(), wire: String::new() }]);
    w.push_bare("cy", SessionEvent::WorkspaceHostUpserted { host: h });
    let chat = w.chat("cy", "plans");
    (w, chat)
}

/// Every 0.5 addition, interleaved with ordinary traffic, on rules 3.
fn with_additions(w: &mut SignedWorld, chat: Uuid) -> Uuid {
    let mut a = WorkspaceAgent::new("scout", "ws-claude");
    a.owner_actor_id = "cy".into();
    let a = a.for_write(Some(ResolvedRun { harness: "claudeCode".into(), model: "opus".into() }));
    w.push("ben", SessionEvent::AgentUpserted { agent: a.clone() });
    // A full run spec and a pool are inert on rules 3 (the mirror).
    let mut full = a.clone();
    let spec = AgentRun { harness: "claudeCode".into(), source: "claude-code".into(), model: "sonnet".into(), resolved: None };
    full.run = Some(spec.clone());
    full.placement = Some(Placement::Pool);
    w.push("ben", SessionEvent::AgentUpserted { agent: full });
    let q = ChatMessage::new(MessageRole::User, "cy", "@scout hi");
    let qid = q.id;
    w.say(chat, "cy", SessionEvent::MessageAppended { message: q });
    w.say(chat, "cy", SessionEvent::TurnDelegated { trigger_id: qid, agent_id: a.id, device_id: w.people["cy"].device_id });
    let mut r = ChatMessage::new(MessageRole::Assistant, "scout", "");
    r.responder_id = Some(a.id);
    r.reply_to_message_id = Some(qid);
    r.is_streaming = true;
    let rid = r.id;
    w.say(chat, "cy", SessionEvent::MessageAppended { message: r });
    let outcome = ligus_core::outcome::ReplyOutcome { model: "opus".into(), ..Default::default() };
    w.say(chat, "cy", SessionEvent::MessageCompleted { message_id: rid, body: "done".into(), outcome: Some(outcome) });
    w.push("cy", SessionEvent::MemberLeft { member_id: "cy".into(), account_id: Some(w.people["cy"].account_id), watermark: Vec::new() });
    w.push("ben", SessionEvent::SessionTitleChanged { title: "Team".into() });
    let _ = legacy_id(&spec);
    a.id
}

fn is_new_kind(e: &SessionEventEnvelope) -> bool {
    matches!(e.payload, SessionEvent::MemberLeft { .. } | SessionEvent::TurnDelegated { .. })
}

/// L6: under rules 3 every new kind and field is inert for authz: the
/// verdicts of every pre-existing kind, the roster and the agents' legacy
/// fields match the pinned 0.4 core.
#[test]
fn l6_rules_3_with_every_addition_folds_as_0_4_does() {
    let (mut w, chat) = base();
    let agent = with_additions(&mut w, chat);
    let (old_cfg, old_chat) = (to_old(&w.cfg), to_old(&w.chats[&chat]));
    let new_v = ligus_core::fold_decisions(&w.cfg, None, None, ligus_core::WorkspaceAuthz::Unchecked);
    let old_v = old::fold_decisions(&old_cfg, None, None, old::WorkspaceAuthz::Unchecked);
    for e in w.cfg.iter().filter(|e| !is_new_kind(e)) {
        assert_eq!(new_v.get(&e.event_id).map(|v| format!("{v:?}")), old_v.get(&e.event_id).map(|v| format!("{v:?}")), "{}", e.payload.kind_str());
    }
    let (new_s, new_t) = ligus_core::project_config(&w.cfg, None, None);
    let (old_s, old_t) = old::project_config(&old_cfg, None, None);
    let (new_s, old_s) = (new_s.unwrap(), old_s.unwrap());
    let members = |m: Vec<(String, String)>| m;
    assert_eq!(
        members(new_s.members.iter().map(|m| (m.id.clone(), format!("{:?}", m.role))).collect()),
        members(old_s.members.iter().map(|m| (m.id.clone(), format!("{:?}", m.role))).collect()),
    );
    let na = new_s.workspace_agents.iter().find(|a| a.id == agent).unwrap();
    let oa = old_s.workspace_agents.iter().find(|a| a.id == agent).unwrap();
    assert_eq!((na.runtime_id.as_str(), na.host_id.as_str()), (oa.runtime_id.as_str(), oa.host_id.as_str()), "the same routing on both");
    assert_eq!(na.placement, Some(Placement::mirror(&na.host_id)), "a pool is inert on rules 3");
    let (nt, ot) = (new_t.clone(), old_t.clone());
    let nv = ligus_core::fold_decisions(&w.chats[&chat], None, None, ligus_core::WorkspaceAuthz::Timeline(&nt));
    let ov = old::fold_decisions(&old_chat, None, None, old::WorkspaceAuthz::Timeline(&ot));
    for e in w.chats[&chat].iter().filter(|e| !is_new_kind(e)) {
        assert_eq!(nv.get(&e.event_id).map(|v| format!("{v:?}")), ov.get(&e.event_id).map(|v| format!("{v:?}")), "chat {}", e.payload.kind_str());
    }
    let n = ligus_core::project_authorized(&w.chats[&chat], None, None, ligus_core::WorkspaceAuthz::Timeline(&nt)).unwrap();
    assert!(n.authz.delegations.is_empty(), "a delegation is inert on rules 3");
}

/// L7: a workspace founded the way 0.5 founds one (genesis rules 2, then
/// rules 4 with the founder's first credential) is read-only on 0.4.1; so is
/// one an Owner switched from rules 3. What came before the switch folds
/// the same on both builds.
#[test]
fn l7_rules_4_is_read_only_on_0_4_and_nothing_before_it_changes() {
    // Founded by 0.5.
    let mut f = SignedWorld::genesis(&["ana"], None);
    f.push("ana", SessionEvent::WorkspaceRulesSet { version: ligus_core::authz::RULES_NEW_WORKSPACE, presence: None });
    assert_eq!(ligus_core::authz::RULES_NEW_WORKSPACE, 4, "new workspaces start on rules 4 (D11)");
    let (old_s, _) = old::project_config(&to_old(&f.cfg), None, None);
    assert!(old_s.unwrap().authz.read_only(), "0.4.1 writes nothing on a workspace 0.5 founded");
    assert!(!ligus_core::project_config(&f.cfg, None, None).0.unwrap().authz.read_only());

    // Switched by an Owner, with traffic on both sides of the switch.
    let (mut w, chat) = base();
    with_additions(&mut w, chat);
    let before: Vec<Uuid> = w.cfg.iter().chain(w.chats[&chat].iter()).filter(|e| !is_new_kind(e)).map(|e| e.event_id).collect();
    let switch = w.push("ana", SessionEvent::WorkspaceRulesSet { version: 4, presence: None });
    let late = w.say(chat, "ben", SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "ben", "after") });
    let (old_cfg, old_chat) = (to_old(&w.cfg), to_old(&w.chats[&chat]));
    let (old_s, old_t) = old::project_config(&old_cfg, None, None);
    let (new_s, new_t) = ligus_core::project_config(&w.cfg, None, None);
    assert!(old_s.unwrap().authz.read_only());
    let new_s = new_s.unwrap();
    assert!(!new_s.authz.read_only() && new_s.authz.rules == 4);
    let new_v = ligus_core::fold_decisions(&w.cfg, None, None, ligus_core::WorkspaceAuthz::Unchecked);
    let old_v = old::fold_decisions(&old_cfg, None, None, old::WorkspaceAuthz::Unchecked);
    let new_c = ligus_core::fold_decisions(&w.chats[&chat], None, None, ligus_core::WorkspaceAuthz::Timeline(&new_t));
    let old_c = old::fold_decisions(&old_chat, None, None, old::WorkspaceAuthz::Timeline(&old_t));
    for id in &before {
        let n = new_v.get(id).or(new_c.get(id)).map(|v| format!("{v:?}"));
        let o = old_v.get(id).or(old_c.get(id)).map(|v| format!("{v:?}"));
        assert_eq!(n, o, "an event before the switch folds differently");
    }
    assert_eq!(format!("{:?}", new_v[&switch]), "Allowed");
    assert_eq!(format!("{:?}", new_c[&late]), "Allowed");
}

/// L8: the pinned 0.4.1 runtime on a rules-4 workspace. It ingests every
/// envelope (0.5 kinds included) and keeps the exact bytes, then refuses
/// every write: no message, no turn claim, no reply placeholder. A turn
/// starts only with a claim and a placeholder, so the older build never
/// calls a model there.
#[test]
fn l8_the_0_4_runtime_stores_everything_and_writes_nothing_on_rules_4() {
    let (mut w, chat) = base();
    let agent = with_additions(&mut w, chat);
    w.push("ana", SessionEvent::WorkspaceRulesSet { version: 4, presence: None });
    let q = ChatMessage::new(MessageRole::User, "ben", "@scout again");
    let qid = q.id;
    w.say(chat, "ben", SessionEvent::MessageAppended { message: q });
    w.say(chat, "ben", SessionEvent::TurnDelegated { trigger_id: qid, agent_id: agent, device_id: w.people["ben"].device_id });

    let mut store = old_rt::event_store::EventStore::open_in_memory().unwrap();
    let all: Vec<&SessionEventEnvelope> = w.cfg.iter().chain(w.chats[&chat].iter()).collect();
    for e in &all {
        let wire = serde_json::to_string(e).unwrap();
        let o: old::SessionEventEnvelope = serde_json::from_str(&wire).unwrap();
        store.ingest(&o).expect("0.4.1 ingests every 0.5 event");
    }
    // Byte for byte: what it would push again is what it received.
    let cfg_id = ligus_core::workspace_config_session_id(w.wid);
    let mut stored: Vec<String> = store.list_in(w.wid, cfg_id).unwrap().iter().chain(store.list_in(w.wid, chat).unwrap().iter()).map(|e| serde_json::to_string(e).unwrap()).collect();
    let mut sent: Vec<String> = all.iter().map(|e| serde_json::to_string(e).unwrap()).collect();
    stored.sort();
    sent.sort();
    assert_eq!(stored, sent, "0.4.1 stores and re-pushes every 0.5 event unchanged");
    let rows = stored.len();

    // Ben's own device, running 0.4.1.
    let ben = &w.people["ben"];
    let device = old::SigningKeypair::from_seed(&ben.device.seed_bytes()).unwrap();
    let account = old::SigningKeypair::from_seed(&ben.account.seed_bytes()).unwrap();
    let mut actor = ben.actor();
    actor.device_id = Some(ben.device_id);
    let actor: old::ActorIdentity = serde_json::from_value(serde_json::to_value(&actor).unwrap()).unwrap();
    let mut svc = old_rt::ChatService::new(store, device, account, actor);
    let refused = |r: Result<String, String>, what: &str| {
        let why = r.expect_err(what);
        assert!(why.contains("update Ligus"), "{what}: {why}");
    };
    refused(svc.post_user_message(chat, w.wid, "hello").map(|_| String::new()).map_err(|e| e.to_string()), "a message");
    refused(svc.claim_turn_for(chat, w.wid, qid, Some(agent)).map(|_| String::new()).map_err(|e| e.to_string()), "a turn claim");
    refused(
        svc.begin_assistant_message(chat, w.wid, "scout", "ws-claude", Some(agent), Some(qid)).map(|_| String::new()).map_err(|e| e.to_string()),
        "a reply placeholder",
    );
    refused(svc.set_title(chat, w.wid, "renamed").map(|_| String::new()).map_err(|e| e.to_string()), "a title");
    let after = svc.store().list_in(w.wid, cfg_id).unwrap().len() + svc.store().list_in(w.wid, chat).unwrap().len();
    assert_eq!(after, rows, "the older build wrote nothing");
}
