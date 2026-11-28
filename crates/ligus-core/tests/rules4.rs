//! Rules version 4 (workspace model v3, 15): the gates G1 to G14 in the
//! signed fold, each with the test that proves it, plus the mixed rules
//! position (events before the switch are judged as before) and the
//! readiness list. The property tests run 48 cases each (every case folds
//! a whole signed workspace).

use ligus_core::agent_run::{legacy_id, AgentRun, Placement};
use ligus_core::authz::rules4::{default_agent_id, readiness, DEFAULT_AGENT_NAME};
use ligus_core::events::Verdict;
use ligus_core::testkit::{Proof, SignedWorld};
use ligus_core::{
    sign_envelope, ActorIdentity, ActorKind, ChatMessage, HostKind, MessageRole, SessionEvent, SigningKeypair, Timestamp,
    WorkspaceAgent, WorkspaceHost, WorkspaceRole,
};
use proptest::prelude::*;
use uuid::Uuid;

const AGENT: u128 = 0xa9e;

fn host_of(w: &SignedWorld, who: &str) -> String {
    w.people[who].device_id.to_string()
}

fn offer(name: &str, harness: &str) -> ligus_core::workspace_host::HostOffer {
    ligus_core::workspace_host::HostOffer { name: name.into(), kind: ligus_core::workspace_host::OfferKind::Cli, harness: harness.into(), wire: String::new() }
}

fn host_record(w: &SignedWorld, who: &str, kind: HostKind, offers: &[&str]) -> WorkspaceHost {
    let mut h = WorkspaceHost::new(host_of(w, who), kind, format!("{who}-host"));
    h.owner_actor_id = who.into();
    h.offers = Some(offers.iter().map(|o| offer(o, o)).collect());
    h
}

/// Ana (Owner), Ben (Admin), Cy and Dee (Contributors), two boxes (service
/// members, workers offering `ollama`), each person's computer a host
/// offering `claude-code` and `codex`. Then the Owner switches to rules 4.
fn world() -> SignedWorld {
    let mut w = SignedWorld::genesis(&["ana", "ben", "cy", "dee", "box", "box2", "eve"], None);
    w.admit("ana", "ben", WorkspaceRole::Admin);
    w.admit("ana", "cy", WorkspaceRole::Contributor);
    w.admit("ana", "dee", WorkspaceRole::Contributor);
    for b in ["box", "box2"] {
        w.kinds.insert(b.into(), ActorKind::Service);
        let m = w.member_of(b, WorkspaceRole::Contributor);
        w.push("ana", SessionEvent::MemberAdded { member: m, invite: None, proof: None });
        w.publish_identity(b);
    }
    w.push("ana", SessionEvent::WorkspaceRulesSet { version: 3, presence: None });
    for who in ["cy", "dee"] {
        let h = host_record(&w, who, HostKind::Device, &["claude-code", "codex"]);
        w.push_bare(who, SessionEvent::WorkspaceHostUpserted { host: h });
    }
    for b in ["box", "box2"] {
        let h = host_record(&w, b, HostKind::Worker, &["ollama"]);
        w.push_bare(b, SessionEvent::WorkspaceHostUpserted { host: h });
    }
    let id = w.push("ana", SessionEvent::WorkspaceRulesSet { version: 4, presence: None });
    assert_eq!(w.verdict(id), Verdict::Allowed, "the Owner switches with presence");
    assert_eq!(w.roster().rules, 4);
    w
}

fn spec(source: &str, model: &str) -> AgentRun {
    AgentRun { harness: source.into(), source: source.into(), model: model.into(), resolved: None }
}

/// An agent owned by `owner`, running `run`, placed at `placement`.
fn agent(w: &SignedWorld, owner: &str, run: AgentRun, placement: Placement) -> WorkspaceAgent {
    let mut a = WorkspaceAgent::new("scout", legacy_id(&run));
    a.id = Uuid::from_u128(AGENT);
    a.owner_actor_id = owner.into();
    a.created_at = Timestamp::from_unix_seconds(0);
    a.host_id = match &placement {
        Placement::Host { host_id } => host_id.clone(),
        Placement::Pool => host_of(w, "box"),
        _ => String::new(),
    };
    a.run = Some(run);
    a.placement = Some(placement);
    a
}

fn up(a: &WorkspaceAgent) -> SessionEvent {
    SessionEvent::AgentUpserted { agent: a.clone() }
}

fn folded(w: &SignedWorld) -> Option<WorkspaceAgent> {
    w.config().workspace_agents.into_iter().find(|a| a.id == Uuid::from_u128(AGENT))
}

/// What an honest remover holds of `who`'s device on the config log.
fn held(w: &SignedWorld, who: &str) -> Vec<ligus_core::watermark::Watermark> {
    let signers = std::iter::once(w.people[who].device_id).collect();
    ligus_core::watermark::heads_of(&w.cfg, ligus_core::workspace_config_session_id(w.wid), &signers)
}

fn v(w: &SignedWorld, id: Uuid) -> Verdict {
    w.verdict(id)
}

// ---- G1: who may create and move agents (table 5.7) ----

#[test]
fn g1_a_contributor_creates_their_own_agent_on_their_devices_without_a_passkey() {
    let mut w = world();
    let a = agent(&w, "cy", spec("claude-code", "opus"), Placement::OwnerDevices);
    let id = w.push_bare("cy", up(&a));
    assert_eq!(v(&w, id), Verdict::Allowed);
    let f = folded(&w).unwrap();
    assert_eq!(f.run.as_ref().map(|r| r.model.as_str()), Some("opus"), "rules 4 keeps the full run spec");
    assert_eq!(f.runtime_id, legacy_id(&spec("claude-code", "opus")));
}

#[test]
fn g1_placing_on_your_own_host_needs_a_passkey_and_no_admin() {
    let mut w = world();
    let a = agent(&w, "cy", spec("codex", ""), Placement::Host { host_id: host_of(&w, "cy") });
    let bare = w.push_bare("cy", up(&a));
    assert_eq!(v(&w, bare), Verdict::Denied, "without presence");
    let ok = w.push("cy", up(&a));
    assert_eq!(v(&w, ok), Verdict::Allowed, "a Contributor with presence");
}

#[test]
fn g1_someone_elses_host_or_a_pool_is_an_admins() {
    let mut w = world();
    for p in [Placement::Host { host_id: host_of(&w, "dee") }, Placement::Pool] {
        let run = if p == Placement::Pool { spec("ollama", "llama3") } else { spec("codex", "") };
        let a = agent(&w, "cy", run, p.clone());
        let id = w.push("cy", up(&a));
        assert_eq!(v(&w, id), Verdict::Denied, "a Contributor, even with presence: {p:?}");
    }
    let mut a = agent(&w, "cy", spec("ollama", "llama3"), Placement::Pool);
    a.owner_actor_id = "ben".into();
    let bare = w.push_bare("ben", up(&a));
    assert_eq!(v(&w, bare), Verdict::Denied, "an Admin needs presence for a pool");
    let ok = w.push("ben", up(&a));
    assert_eq!(v(&w, ok), Verdict::Allowed);
    assert_eq!(folded(&w).unwrap().placement, Some(Placement::Pool), "rules 4 keeps a pool");
}

#[test]
fn g1_a_model_change_within_one_source_needs_no_passkey_a_source_change_does() {
    let mut w = world();
    let a = agent(&w, "cy", spec("claude-code", "opus"), Placement::OwnerDevices);
    w.push_bare("cy", up(&a));
    let mut b = folded(&w).unwrap();
    b.run = Some(spec("claude-code", "sonnet"));
    b.runtime_id = legacy_id(b.run.as_ref().unwrap());
    let id = w.push_bare("cy", up(&b));
    assert_eq!(v(&w, id), Verdict::Allowed, "a model change in one source (T15 residual)");
    assert_eq!(folded(&w).unwrap().run.unwrap().model, "sonnet");
    let mut c = folded(&w).unwrap();
    c.run = Some(spec("codex", "gpt-5.3-codex"));
    c.runtime_id = legacy_id(c.run.as_ref().unwrap());
    let bare = w.push_bare("cy", up(&c));
    assert_eq!(v(&w, bare), Verdict::Denied, "a source change needs presence");
    let ok = w.push("cy", up(&c));
    assert_eq!(v(&w, ok), Verdict::Allowed);
}

#[test]
fn g1_someone_elses_agent_is_still_an_admins() {
    let mut w = world();
    let a = agent(&w, "cy", spec("claude-code", "opus"), Placement::OwnerDevices);
    w.push_bare("cy", up(&a));
    let mut b = folded(&w).unwrap();
    b.name = "dees-now".into();
    let id = w.push("dee", up(&b));
    assert_eq!(v(&w, id), Verdict::Denied);
}

/// H10: a Contributor's 0.4-shaped upsert (no sticky fields, `host_id` set
/// to someone else's host) is judged on the merged record: a move, denied.
#[test]
fn h10_an_old_shaped_upsert_cant_move_an_agent() {
    let mut w = world();
    let a = agent(&w, "cy", spec("claude-code", "opus"), Placement::OwnerDevices);
    w.push_bare("cy", up(&a));
    let mut old = folded(&w).unwrap();
    old.run = None;
    old.placement = None;
    old.host_id = host_of(&w, "dee");
    for proof in [Proof::None, Proof::Recovery] {
        let id = w.write("cy", None, up(&old), None, proof);
        assert_eq!(v(&w, id), Verdict::Denied);
    }
    assert_eq!(folded(&w).unwrap().effective_placement(), Placement::OwnerDevices);
}

// ---- G2: references resolve (H5) ----

#[test]
fn g2_h5_a_placement_names_a_live_host_that_offers_the_source() {
    let mut w = world();
    let ghost = Uuid::new_v4().to_string();
    let cases = [
        ("unknown host", agent(&w, "", spec("codex", ""), Placement::Host { host_id: ghost })),
        ("not offered", agent(&w, "", spec("ollama", ""), Placement::Host { host_id: host_of(&w, "cy") })),
        ("pool of a runtime", agent(&w, "", AgentRun::mirror("ws-claude", None), Placement::Pool)),
    ];
    for (why, a) in cases {
        let id = w.push("ben", up(&a));
        assert_eq!(v(&w, id), Verdict::Denied, "{why}");
    }
    // A revoked device's host, and a removed owner's.
    let mut r = world();
    let d = r.people["dee"].device_id;
    let wm = held(&r, "dee");
    r.push("ana", SessionEvent::DeviceRevoked { device_id: d, watermark: wm });
    let a = agent(&r, "", spec("codex", ""), Placement::Host { host_id: host_of(&r, "dee") });
    let id = r.push("ben", up(&a));
    assert_eq!(v(&r, id), Verdict::Denied, "revoked");
    let mut m = world();
    let wm = held(&m, "dee");
    m.push("ana", SessionEvent::MemberRemoved { member_id: "dee".into(), watermark: wm });
    let a = agent(&m, "", spec("codex", ""), Placement::Host { host_id: host_of(&m, "dee") });
    let id = m.push("ben", up(&a));
    assert_eq!(v(&m, id), Verdict::Denied, "removed owner");
    // The live, offering host is fine.
    let a = agent(&w, "", spec("codex", ""), Placement::Host { host_id: host_of(&w, "cy") });
    let id = w.push("ben", up(&a));
    assert_eq!(v(&w, id), Verdict::Allowed);
}

#[test]
fn g2_a_rename_of_an_agent_whose_host_went_isnt_refused() {
    let mut w = world();
    let a = agent(&w, "", spec("codex", ""), Placement::Host { host_id: host_of(&w, "dee") });
    w.push("ben", up(&a));
    let wm = held(&w, "dee");
    w.push("ana", SessionEvent::MemberRemoved { member_id: "dee".into(), watermark: wm });
    let f = folded(&w).unwrap();
    assert!(f.is_host_gone());
    let mut r = f.as_judged().into_owned();
    r.name = "renamed".into();
    let id = w.push_bare("cy", up(&r));
    assert_eq!(v(&w, id), Verdict::Allowed, "a shared agent's rename");
    assert!(folded(&w).unwrap().is_host_gone(), "still gone, never the owner's devices");
}

// ---- G3: inert hosts ----

#[test]
fn g3_a_revoked_devices_host_is_inert_and_its_agents_are_gone() {
    let mut w = world();
    let a = agent(&w, "", spec("codex", ""), Placement::Host { host_id: host_of(&w, "dee") });
    w.push("ben", up(&a));
    let d = w.people["dee"].device_id;
    let wm = held(&w, "dee");
    w.push("ana", SessionEvent::DeviceRevoked { device_id: d, watermark: wm });
    let cfg = w.config();
    assert!(!cfg.workspace_hosts.iter().any(|h| h.id == host_of(&w, "dee")));
    let f = folded(&w).unwrap();
    assert!(f.is_host_gone() && f.host_id.is_empty());
    assert_eq!(f.effective_placement(), Placement::Host { host_id: host_of(&w, "dee") });
}

// ---- G4: a leave is a removal (W3 under rules 4) ----

fn leave(w: &SignedWorld, who: &str) -> SessionEvent {
    SessionEvent::MemberLeft { member_id: who.into(), account_id: Some(w.people[who].account_id), watermark: Vec::new() }
}

#[test]
fn g4_a_leave_removes_the_member_and_a_departed_box_leaves_its_agents_gone() {
    let mut w = world();
    let a = agent(&w, "", spec("ollama", ""), Placement::Host { host_id: host_of(&w, "box") });
    w.push("ben", up(&a));
    let id = w.push("cy", leave(&w, "cy"));
    assert_eq!(v(&w, id), Verdict::Allowed);
    let id = w.push_bare("box", leave(&w, "box"));
    assert_eq!(v(&w, id), Verdict::Allowed);
    let cfg = w.config();
    assert!(!cfg.members.iter().any(|m| m.id == "cy" || m.id == "box"), "removed under rules 4");
    assert!(cfg.left.contains_key("cy"), "the record stays for display");
    assert!(folded(&w).unwrap().is_host_gone(), "the box's agent runs nowhere");
    // Not a member any more: Cy's later writes are refused.
    let late = w.push_bare("cy", SessionEvent::SessionTitleChanged { title: "x".into() });
    assert_eq!(v(&w, late), Verdict::Denied);
    // The rotation set drops the leavers' devices.
    let rec = w.roster().rotation_recipients();
    assert!(!rec.contains_key(&w.people["cy"].device_id));
}

#[test]
fn g4_the_last_owner_and_the_founder_never_leave_and_two_owners_leave_one() {
    let mut w = world();
    let id = w.push("ana", leave(&w, "ana"));
    assert_eq!(v(&w, id), Verdict::Denied, "the founder");
    // Ben and Cy made Owners; Ana stays (founder), so both may leave.
    for who in ["ben", "cy"] {
        let role = w.config().members.iter().find(|m| m.id == who).unwrap().role;
        w.push("ana", SessionEvent::MemberRoleChanged { change: ligus_core::events::MemberRoleChange { member_id: who.into(), old_role: role, new_role: WorkspaceRole::Owner }, watermark: Vec::new() });
    }
    let a = w.push("ben", leave(&w, "ben"));
    let b = w.push("cy", leave(&w, "cy"));
    assert_eq!((v(&w, a), v(&w, b)), (Verdict::Allowed, Verdict::Allowed));
    assert!(w.config().members.iter().any(|m| m.id == "ana" && m.role == WorkspaceRole::Owner));
}

#[test]
fn g4_a_leave_then_a_retirement_the_first_applies() {
    let mut w = world();
    let id = w.push("cy", leave(&w, "cy"));
    assert_eq!(v(&w, id), Verdict::Allowed);
    let again = w.push("cy", leave(&w, "cy"));
    assert_eq!(v(&w, again), Verdict::Denied, "not twice: not a member any more");
}

#[test]
fn g4_a_leaves_watermark_is_validated_and_capped() {
    let w = world();
    let cfg = ligus_core::workspace_config_session_id(w.wid);
    let entry = |signer: Uuid| ligus_core::watermark::Watermark { signer, log: cfg, heads: vec![Uuid::new_v4()], gaps: Vec::new() };
    let mk = |wm: Vec<ligus_core::watermark::Watermark>| SessionEvent::MemberLeft { member_id: "cy".into(), account_id: Some(w.people["cy"].account_id), watermark: wm };
    let cases = [
        ("someone else's signer", vec![entry(w.people["dee"].device_id)]),
        ("no heads", vec![ligus_core::watermark::Watermark { signer: w.people["cy"].device_id, log: cfg, heads: Vec::new(), gaps: Vec::new() }]),
        ("twice", vec![entry(w.people["cy"].device_id), entry(w.people["cy"].device_id)]),
        ("too large", (0..ligus_core::authz::rules4::MAX_LEAVE_WATERMARK + 1).map(|_| ligus_core::watermark::Watermark { signer: w.people["cy"].device_id, log: Uuid::new_v4(), heads: vec![Uuid::new_v4()], gaps: Vec::new() }).collect()),
    ];
    for (why, wm) in cases {
        let mut x = w.clone();
        let id = x.push("cy", mk(wm));
        assert_eq!(v(&x, id), Verdict::Denied, "{why}");
    }
    let mut ok = w.clone();
    let id = ok.push("cy", mk(vec![entry(w.people["cy"].device_id)]));
    assert_eq!(v(&ok, id), Verdict::Allowed, "its own signer");
}

/// The leaver's own watermark never erases what it did before: an empty
/// one leaves every earlier event of theirs standing.
#[test]
fn g4_an_empty_leave_watermark_erases_nothing() {
    let mut w = world();
    let chat = w.chat("ana", "plans");
    let m = w.say(chat, "cy", SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "cy", "before") });
    let t = w.push_bare("cy", SessionEvent::ChannelCreated { channel: ligus_core::channel::Channel::new("c1", "general") });
    w.push("cy", leave(&w, "cy"));
    assert_eq!((v(&w, m), v(&w, t)), (Verdict::Allowed, Verdict::Allowed));
}

// ---- G5, G7, G9, G11 ----

#[test]
fn g5_w2_no_agent_records_on_a_chat_log_and_older_copies_stand() {
    let mut w = SignedWorld::genesis(&["ana", "cy"], None);
    w.admit("ana", "cy", WorkspaceRole::Contributor);
    w.push("ana", SessionEvent::WorkspaceRulesSet { version: 3, presence: None });
    let chat = w.chat("cy", "plans");
    let mut a = WorkspaceAgent::new("copy", "ws-claude");
    a.owner_actor_id = "cy".into();
    let before = w.write("cy", Some(chat), up(&a), None, Proof::Recovery);
    w.push("ana", SessionEvent::WorkspaceRulesSet { version: 4, presence: None });
    let mut b = WorkspaceAgent::new("copy2", "ws-claude");
    b.owner_actor_id = "cy".into();
    let after = w.write("cy", Some(chat), up(&b), None, Proof::Recovery);
    let removed = w.write("cy", Some(chat), SessionEvent::AgentRemoved { agent_id: a.id }, None, Proof::Recovery);
    assert_eq!(v(&w, before), Verdict::Allowed, "judged at its own position, never re-judged");
    assert_eq!((v(&w, after), v(&w, removed)), (Verdict::Denied, Verdict::Denied));
}

#[test]
fn g7_w1_a_person_or_box_joins_with_an_account() {
    let mut w = world();
    let mut m = w.member_of("eve", WorkspaceRole::Viewer);
    m.actor.account_id = None;
    m.id = "eve-local".into();
    m.actor.id = "eve-local".into();
    let id = w.push("ana", SessionEvent::MemberAdded { member: m, invite: None, proof: None });
    assert_eq!(v(&w, id), Verdict::Denied);
    // Rules 3 already asks a member's id to be its account id.
    let mut ok = w.member_of("eve", WorkspaceRole::Viewer);
    let acct = w.people["eve"].account_id.to_string();
    ok.id = acct.clone();
    ok.actor.id = acct;
    let id = w.push("ana", SessionEvent::MemberAdded { member: ok, invite: None, proof: None });
    assert_eq!(v(&w, id), Verdict::Allowed);
}

#[test]
fn g9_no_new_or_changed_runtime_address_and_an_old_one_may_stay() {
    let mut w = SignedWorld::genesis(&["ana"], None);
    w.push("ana", SessionEvent::WorkspaceRulesSet { version: 3, presence: None });
    let mut rt = ligus_core::WorkspaceRuntime::new("local", "Local", ligus_core::ModelProviderKind::Ollama, "llama3");
    rt.endpoint = "http://10.0.0.5:11434".into();
    let before = w.push("ana", SessionEvent::WorkspaceRuntimeUpserted { runtime: rt.clone() });
    w.push("ana", SessionEvent::WorkspaceRulesSet { version: 4, presence: None });
    let mut renamed = rt.clone();
    renamed.name = "Local box".into();
    let kept = w.push("ana", SessionEvent::WorkspaceRuntimeUpserted { runtime: renamed });
    let mut moved = rt.clone();
    moved.endpoint = "http://10.0.0.6:11434".into();
    let changed = w.push("ana", SessionEvent::WorkspaceRuntimeUpserted { runtime: moved });
    let mut fresh = rt.clone();
    fresh.id = "other".into();
    let added = w.push("ana", SessionEvent::WorkspaceRuntimeUpserted { runtime: fresh });
    let mut cleared = rt;
    cleared.endpoint = String::new();
    let off = w.push("ana", SessionEvent::WorkspaceRuntimeUpserted { runtime: cleared });
    assert_eq!(
        (v(&w, before), v(&w, kept), v(&w, changed), v(&w, added), v(&w, off)),
        (Verdict::Allowed, Verdict::Allowed, Verdict::Denied, Verdict::Denied, Verdict::Allowed)
    );
}

#[test]
fn g11_whole_list_events_are_refused() {
    let mut w = world();
    let a = agent(&w, "cy", spec("claude-code", ""), Placement::OwnerDevices);
    let r = w.push("ana", SessionEvent::AgentRosterUpdated { agents: vec![a] });
    let h = w.push_bare("cy", SessionEvent::WorkspaceHostsUpdated { hosts: vec![host_record(&w, "cy", HostKind::Device, &[])] });
    let rt = w.push("ana", SessionEvent::WorkspaceRuntimesUpdated { runtimes: Vec::new() });
    assert_eq!((v(&w, r), v(&w, h), v(&w, rt)), (Verdict::Denied, Verdict::Denied, Verdict::Denied));
}

// ---- G13: a host record is its own device's (H11) ----

#[test]
fn g13_h11_a_host_record_under_someone_elses_device_id_is_denied() {
    let mut w = world();
    let mut squat = host_record(&w, "dee", HostKind::Device, &["ollama"]);
    squat.owner_actor_id = "cy".into();
    let id = w.push("cy", SessionEvent::WorkspaceHostUpserted { host: squat.clone() });
    assert_eq!(v(&w, id), Verdict::Denied, "signed by Cy's device, naming Dee's");
    squat.owner_actor_id = "dee".into();
    let id = w.push("ben", SessionEvent::WorkspaceHostUpserted { host: squat });
    assert_eq!(v(&w, id), Verdict::Denied, "an Admin rewriting from another device");
    // The record never satisfies G2: Dee's host still offers no ollama.
    let a = agent(&w, "", spec("ollama", ""), Placement::Host { host_id: host_of(&w, "dee") });
    let id = w.push("ben", up(&a));
    assert_eq!(v(&w, id), Verdict::Denied);
    // Its own device naming an owner who doesn't hold it is denied too.
    let mut not_held = host_record(&w, "cy", HostKind::Device, &[]);
    not_held.owner_actor_id = "dee".into();
    let id = w.push("cy", SessionEvent::WorkspaceHostUpserted { host: not_held });
    assert_eq!(v(&w, id), Verdict::Denied);
}

// ---- G6: the default agent ----

#[test]
fn g6_the_default_agents_record_is_locked_and_its_replies_come_from_the_chat_creator() {
    let mut w = world();
    let did = default_agent_id(w.wid);
    let run = spec("claude-code", "opus");
    let mut d = WorkspaceAgent::new(DEFAULT_AGENT_NAME, legacy_id(&run));
    d.id = did;
    d.run = Some(run);
    d.placement = Some(Placement::ChatCreator);
    let mut renamed = d.clone();
    renamed.name = "Mallory".into();
    let bad = w.push("ana", up(&renamed));
    assert_eq!(v(&w, bad), Verdict::Denied, "the name is locked");
    let contrib = w.push("cy", up(&d));
    assert_eq!(v(&w, contrib), Verdict::Denied, "an Admin's");
    let ok = w.push("ana", up(&d));
    assert_eq!(v(&w, ok), Verdict::Allowed);
    // No other agent takes the chat creator's devices.
    let other = agent(&w, "cy", spec("claude-code", ""), Placement::ChatCreator);
    let id = w.push("cy", up(&other));
    assert_eq!(v(&w, id), Verdict::Denied);
    // A reply as the default agent: from the chat creator's device only.
    let chat = w.chat("cy", "plans");
    let q = ChatMessage::new(MessageRole::User, "cy", "hi");
    let qid = q.id;
    w.say(chat, "cy", SessionEvent::MessageAppended { message: q });
    let reply = |by: &str| {
        let mut m = ChatMessage::new(MessageRole::Assistant, "Ligus", by);
        m.responder_id = Some(did);
        m.reply_to_message_id = Some(qid);
        SessionEvent::MessageAppended { message: m }
    };
    let mine = w.say(chat, "cy", reply("cy"));
    let theirs = w.say(chat, "dee", reply("dee"));
    assert_eq!((v(&w, mine), v(&w, theirs)), (Verdict::Allowed, Verdict::Denied));
}

// ---- G8: who hints agree with the stamp ----

#[test]
fn g8_a_payloads_who_names_its_signed_author() {
    let mut w = world();
    let chat = w.chat("cy", "plans");
    let mut m = ChatMessage::new(MessageRole::User, "ana", "as ana");
    m.actor_identity = Some(w.people["ana"].actor());
    let forged = w.say(chat, "cy", SessionEvent::MessageAppended { message: m });
    let mut own = ChatMessage::new(MessageRole::User, "cy", "as me");
    own.actor_identity = Some(w.people["cy"].actor());
    let mid = own.id;
    let ok = w.say(chat, "cy", SessionEvent::MessageAppended { message: own });
    let reaction = |actor: &str| ligus_core::chat::MessageReaction {
        emoji: "+1".into(),
        actor_id: actor.into(),
        actor_display_name: actor.into(),
        actor_kind: ActorKind::Human,
        created_at: Timestamp::epoch(),
    };
    let r_forged = w.say(chat, "cy", SessionEvent::MessageReactionAdded { message_id: mid, reaction: reaction("ana") });
    let r_ok = w.say(chat, "cy", SessionEvent::MessageReactionAdded { message_id: mid, reaction: reaction("cy") });
    assert_eq!((v(&w, forged), v(&w, ok), v(&w, r_forged), v(&w, r_ok)), (Verdict::Denied, Verdict::Allowed, Verdict::Denied, Verdict::Allowed));
}

// ---- G12: filing a chat into a channel follows the workspace role ----

#[test]
fn g12_a_contributor_files_someone_elses_chat_under_rules_4_only() {
    let file = |w: &mut SignedWorld| {
        let chat = w.chat("ana", "plans");
        w.say(chat, "cy", SessionEvent::ChatChannelChanged { channel_id: "general".into() })
    };
    let mut w3 = SignedWorld::genesis(&["ana", "cy"], None);
    w3.admit("ana", "cy", WorkspaceRole::Contributor);
    w3.push("ana", SessionEvent::WorkspaceRulesSet { version: 3, presence: None });
    let id3 = file(&mut w3);
    let mut w4 = world();
    let id4 = file(&mut w4);
    assert_eq!((v(&w3, id3), v(&w4, id4)), (Verdict::Denied, Verdict::Allowed));
}

// ---- G14: agent keys and delegated turns (H12) ----

/// `who`'s device certifies a key for the agent; returns the key.
fn certify(w: &mut SignedWorld, who: &str, agent_id: Uuid, seed: u8) -> (Uuid, SigningKeypair) {
    let akp = SigningKeypair::from_seed(&[seed; 32]).unwrap();
    let p = &w.people[who];
    let cert = ligus_core::AgentKeyCertificate::issue(&p.device, p.device_id, agent_id, &akp.public_key_bytes(), Timestamp::epoch());
    let key = cert.key_id;
    w.push_bare(who, SessionEvent::AgentKeyCertified { cert });
    (key, akp)
}

/// A chat event signed by an agent key.
fn agent_say(w: &mut SignedWorld, chat: Uuid, agent_id: Uuid, key: (Uuid, &SigningKeypair), payload: SessionEvent) -> Uuid {
    let mut env = w.draft("ana", Some(chat), payload, None);
    let s = env.actor_stamp.as_mut().unwrap();
    let mut actor = ActorIdentity::new(agent_id.to_string(), "scout", ActorKind::Agent);
    actor.device_id = Some(key.0);
    s.actor = actor;
    s.prev = None;
    env.clear_raw();
    sign_envelope(&mut env, key.0, key.1);
    w.push_env(Some(chat), env)
}

fn reply_to(agent_id: Uuid, trigger: Uuid) -> SessionEvent {
    let mut m = ChatMessage::new(MessageRole::Assistant, "scout", "on it");
    m.responder_id = Some(agent_id);
    m.reply_to_message_id = Some(trigger);
    SessionEvent::MessageAppended { message: m }
}

#[test]
fn g14_h12_every_pool_worker_answers_and_no_other_key_does() {
    let mut w = world();
    let mut a = agent(&w, "ben", spec("ollama", "llama3"), Placement::Pool);
    a.owner_actor_id = "ben".into();
    w.push("ben", up(&a));
    let k1 = certify(&mut w, "box", a.id, 1);
    let k2 = certify(&mut w, "box2", a.id, 2);
    let kc = certify(&mut w, "cy", a.id, 3);
    let roster = w.roster();
    assert!(roster.valid_agent_key(k1.0).is_some() && roster.valid_agent_key(k2.0).is_some(), "both pool workers");
    assert!(roster.valid_agent_key(kc.0).is_none(), "a person's computer isn't in the pool");
    let chat = w.chat("ben", "plans");
    let q = ChatMessage::new(MessageRole::User, "ben", "hi");
    let qid = q.id;
    w.say(chat, "ben", SessionEvent::MessageAppended { message: q });
    let r2 = agent_say(&mut w, chat, a.id, (k2.0, &k2.1), reply_to(a.id, qid));
    let rc = agent_say(&mut w, chat, a.id, (kc.0, &kc.1), reply_to(a.id, qid));
    assert_eq!((v(&w, r2), v(&w, rc)), (Verdict::Allowed, Verdict::Denied), "the non-first pool host's reply, and no other");
}

#[test]
fn g14_a_delegated_turn_runs_once_on_the_owners_device() {
    let mut w = world();
    // Cy's agent on the box, which is offline: Cy runs it here this time.
    let mut a = agent(&w, "cy", spec("ollama", "llama3"), Placement::Host { host_id: host_of(&w, "box") });
    a.owner_actor_id = "cy".into();
    w.push("ben", up(&a));
    let chat = w.chat("cy", "plans");
    let ask = |w: &mut SignedWorld, body: &str| {
        let q = ChatMessage::new(MessageRole::User, "cy", body);
        let id = q.id;
        w.say(chat, "cy", SessionEvent::MessageAppended { message: q });
        id
    };
    let (t1, t2) = (ask(&mut w, "one"), ask(&mut w, "two"));
    let undelegated = w.say(chat, "cy", reply_to(a.id, t1));
    let cy_dev = w.people["cy"].device_id;
    let dee_dev = w.people["dee"].device_id;
    let by_dee = w.say(chat, "dee", SessionEvent::TurnDelegated { trigger_id: t1, agent_id: a.id, device_id: dee_dev });
    let other_device = w.say(chat, "cy", SessionEvent::TurnDelegated { trigger_id: t1, agent_id: a.id, device_id: dee_dev });
    let d = w.say(chat, "cy", SessionEvent::TurnDelegated { trigger_id: t1, agent_id: a.id, device_id: cy_dev });
    let twice = w.say(chat, "cy", SessionEvent::TurnDelegated { trigger_id: t1, agent_id: a.id, device_id: cy_dev });
    let ok = w.say(chat, "cy", reply_to(a.id, t1));
    let other_turn = w.say(chat, "cy", reply_to(a.id, t2));
    let verdicts = [undelegated, by_dee, other_device, d, twice, ok, other_turn].map(|id| v(&w, id));
    assert_eq!(
        verdicts,
        [Verdict::Denied, Verdict::Denied, Verdict::Denied, Verdict::Allowed, Verdict::Denied, Verdict::Allowed, Verdict::Denied]
    );
}

#[test]
fn g14_a_delegation_is_inert_under_rules_3() {
    let mut w = SignedWorld::genesis(&["ana", "cy"], None);
    w.admit("ana", "cy", WorkspaceRole::Contributor);
    w.push("ana", SessionEvent::WorkspaceRulesSet { version: 3, presence: None });
    let chat = w.chat("cy", "plans");
    let q = ChatMessage::new(MessageRole::User, "cy", "x");
    let qid = q.id;
    w.say(chat, "cy", SessionEvent::MessageAppended { message: q });
    w.say(chat, "cy", SessionEvent::TurnDelegated { trigger_id: qid, agent_id: Uuid::new_v4(), device_id: w.people["cy"].device_id });
    assert!(w.state(chat).authz.delegations.is_empty());
}

// ---- G10: eviction by position (E4) ----

#[test]
fn g10_e4_a_far_future_at_ms_doesnt_keep_a_question_resident() {
    use ligus_core::flow::{FlowRecord, HumanAsk, MAX_ASKS_PER_CHAT};
    let mut w = world();
    let chat = w.chat("cy", "plans");
    let ask = |i: u128, at_ms: i64| {
        FlowRecord::HumanAsk(HumanAsk {
            ask_id: Uuid::from_u128(0x1000 + i),
            turn_id: Uuid::from_u128(0x2000 + i),
            agent_key: "primary".into(),
            question: format!("q{i}"),
            at_ms,
            closed: true,
            ..Default::default()
        })
    };
    // The first ask claims a far-future time; then the chat fills up.
    w.say(chat, "cy", SessionEvent::FlowRecorded { record: ask(0, i64::MAX / 2) });
    for i in 1..=MAX_ASKS_PER_CHAT as u128 {
        w.say(chat, "cy", SessionEvent::FlowRecorded { record: ask(i, 10 + i as i64) });
    }
    let asks = w.state(chat).flow.asks;
    assert_eq!(asks.len(), MAX_ASKS_PER_CHAT);
    assert!(!asks.contains_key(&Uuid::from_u128(0x1000)), "evicted by position, whatever its clock says");
}

// ---- L7: the switch, and what came before it ----

#[test]
fn l7_events_before_the_switch_are_judged_as_before() {
    // Under rules 3 a Contributor's new agent on their own host is an
    // Admin's; the same event stays denied after the switch, and an agent
    // an Admin made then keeps folding.
    let mut w = SignedWorld::genesis(&["ana", "ben", "cy"], None);
    w.admit("ana", "ben", WorkspaceRole::Admin);
    w.admit("ana", "cy", WorkspaceRole::Contributor);
    w.push("ana", SessionEvent::WorkspaceRulesSet { version: 3, presence: None });
    let h = host_record(&w, "cy", HostKind::Device, &["codex"]);
    w.push_bare("cy", SessionEvent::WorkspaceHostUpserted { host: h });
    let mut a = WorkspaceAgent::new("scout", "ws-claude");
    a.id = Uuid::from_u128(AGENT);
    a.owner_actor_id = "cy".into();
    a.host_id = host_of(&w, "cy");
    let before = w.push("cy", up(&a));
    w.push("ana", SessionEvent::WorkspaceRulesSet { version: 4, presence: None });
    let after = w.push("cy", up(&a));
    assert_eq!((v(&w, before), v(&w, after)), (Verdict::Denied, Verdict::Allowed));
    // The switch never goes back, and only an Owner makes it.
    let back = w.push("ana", SessionEvent::WorkspaceRulesSet { version: 3, presence: None });
    assert_eq!(v(&w, back), Verdict::Denied);
}

#[test]
fn l7_the_switch_needs_the_owner_and_a_passkey() {
    let mut w = SignedWorld::genesis(&["ana", "ben"], None);
    w.admit("ana", "ben", WorkspaceRole::Admin);
    w.push("ana", SessionEvent::WorkspaceRulesSet { version: 3, presence: None });
    let admin = w.push("ben", SessionEvent::WorkspaceRulesSet { version: 4, presence: None });
    let bare = w.push_bare("ana", SessionEvent::WorkspaceRulesSet { version: 4, presence: None });
    assert_eq!((v(&w, admin), v(&w, bare)), (Verdict::Denied, Verdict::Denied));
    assert_eq!(w.roster().rules, 3);
    let ok = w.push("ana", SessionEvent::WorkspaceRulesSet { version: 4, presence: None });
    assert_eq!(v(&w, ok), Verdict::Allowed);
    assert!(!w.config().authz.read_only(), "this build enforces rules 4");
}

#[test]
fn readiness_lists_what_wouldnt_resolve() {
    let mut w = SignedWorld::genesis(&["ana", "cy", "dee"], None);
    w.admit("ana", "cy", WorkspaceRole::Contributor);
    w.admit("ana", "dee", WorkspaceRole::Contributor);
    w.push("ana", SessionEvent::WorkspaceRulesSet { version: 3, presence: None });
    // A host record Cy wrote under Dee's device id, and an agent on a host
    // that has no record.
    let mut squat = host_record(&w, "dee", HostKind::Device, &[]);
    squat.owner_actor_id = "cy".into();
    w.push("cy", SessionEvent::WorkspaceHostUpserted { host: squat });
    let mut a = WorkspaceAgent::new("scout", "ws-claude");
    a.host_id = Uuid::new_v4().to_string();
    w.push("ana", up(&a));
    let r = readiness(&w.config(), &w.roster());
    assert_eq!(r.unsigned_hosts, vec![host_of(&w, "dee")]);
    assert_eq!(r.unresolved_agents, vec![a.id]);
    assert!(r.accountless_members.is_empty());
    assert!(!r.is_clear());
}

// ---- properties ----

fn arb_placement(w: &SignedWorld) -> impl Strategy<Value = Option<Placement>> {
    let hosts = vec![host_of(w, "cy"), host_of(w, "dee"), host_of(w, "box"), String::new()];
    prop_oneof![
        Just(None),
        Just(Some(Placement::OwnerDevices)),
        proptest::sample::select(hosts.clone()).prop_map(|h| Some(Placement::Host { host_id: h })),
        Just(Some(Placement::Pool)),
        Just(Some(Placement::ChatCreator)),
    ]
}

fn arb_shape(w: &SignedWorld) -> impl Strategy<Value = (Option<AgentRun>, Option<Placement>, String, String)> {
    let hosts = vec![host_of(w, "cy"), host_of(w, "dee"), host_of(w, "box"), String::new()];
    let runs = prop_oneof![
        Just(None),
        Just(Some(spec("claude-code", "opus"))),
        Just(Some(spec("codex", ""))),
        Just(Some(spec("ollama", "llama3:8b"))),
        Just(Some(AgentRun::mirror("ws-claude", None))),
    ];
    let runtime_ids = prop_oneof![Just("ws-claude".to_string()), Just(legacy_id(&spec("claude-code", "opus"))), Just(legacy_id(&spec("ollama", "llama3:8b")))];
    (runs, arb_placement(w), proptest::sample::select(hosts), runtime_ids)
}

fn shaped(base: &WorkspaceAgent, (run, placement, host, runtime): (Option<AgentRun>, Option<Placement>, String, String)) -> WorkspaceAgent {
    let mut a = base.clone();
    a.run = run;
    a.placement = placement;
    a.host_id = host;
    a.runtime_id = runtime;
    a
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    /// H10 as a property: whatever the payload shape (old or new writer,
    /// sticky fields agreeing with the legacy ones or not), a Contributor's
    /// upsert of their agent never leaves it on someone else's host or in a
    /// pool, with or without presence.
    #[test]
    fn h10_a_contributor_never_moves_their_agent_away(shapes in proptest::collection::vec(arb_shape(&world()), 1..4), presence in any::<bool>()) {
        let mut w = world();
        let base = agent(&w, "cy", spec("claude-code", "opus"), Placement::OwnerDevices);
        w.push_bare("cy", up(&base));
        let dee = host_of(&w, "dee");
        let boxh = host_of(&w, "box");
        for s in shapes {
            let a = shaped(&folded(&w).unwrap(), s);
            w.write("cy", None, up(&a), None, if presence { Proof::Recovery } else { Proof::None });
            let f = folded(&w).unwrap();
            let p = f.effective_placement();
            prop_assert!(p != Placement::Pool && p != Placement::Host { host_id: dee.clone() } && p != Placement::Host { host_id: boxh.clone() }, "{p:?}");
            prop_assert!(f.host_id != dee && f.host_id != boxh);
        }
    }

    /// L1 under rules 4: the fold is a function of the event set: agent
    /// upserts from older and newer writers (the full merge) in any
    /// arrival order give the same agents.
    #[test]
    fn l1_any_arrival_order_gives_the_same_agents(shapes in proptest::collection::vec(arb_shape(&world()), 1..5), seed in any::<u64>()) {
        let mut w = world();
        let base = agent(&w, "", spec("claude-code", "opus"), Placement::OwnerDevices);
        w.push("ben", up(&base));
        for s in shapes {
            let a = shaped(&base, s);
            w.push("ben", up(&a));
        }
        let want = w.config().workspace_agents;
        let mut shuffled = w.clone();
        let n = shuffled.cfg.len();
        let mut rng = seed;
        for i in (1..n).rev() {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let j = (rng >> 33) as usize % (i + 1);
            shuffled.cfg.swap(i, j);
        }
        prop_assert_eq!(shuffled.config().workspace_agents, want);
    }
}

/// G4 residual: a leaver can still sign chat events claiming a frontier
/// before its leave (as after any self-removal); an Admin's removal of the
/// member who left, with the Admin's watermark, closes that.
#[test]
fn g4_a_backdated_event_after_a_leave_is_closed_by_an_admins_removal() {
    let mut w = world();
    let chat = w.chat("ana", "plans");
    let before_leave = w.head();
    w.push("cy", leave(&w, "cy"));
    let late = w.write("cy", Some(chat), SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "cy", "backdated") }, before_leave, Proof::None);
    assert_eq!(v(&w, late), Verdict::Allowed, "the residual: its own word about its position");
    let removal = w.push("ana", SessionEvent::MemberRemoved { member_id: "cy".into(), watermark: Vec::new() });
    assert_eq!(v(&w, removal), Verdict::Allowed, "an Admin removes a member who left");
    assert_eq!(v(&w, late), Verdict::Denied, "its watermark doesn't cover the backdated event");
}

/// G14: a shared agent with no owner, on "the owner's devices", answers from
/// any member's trusted device (as below rules 4), never from a stranger's.
#[test]
fn g14_a_shared_agent_answers_from_any_members_device() {
    let mut w = world();
    let a = agent(&w, "", spec("claude-code", ""), Placement::OwnerDevices);
    w.push_bare("cy", up(&a));
    let chat = w.chat("cy", "plans");
    let q = ChatMessage::new(MessageRole::User, "cy", "hi");
    let qid = q.id;
    w.say(chat, "cy", SessionEvent::MessageAppended { message: q });
    let by_dee = w.say(chat, "dee", reply_to(a.id, qid));
    let by_eve = w.say(chat, "eve", reply_to(a.id, qid));
    assert_eq!((v(&w, by_dee), v(&w, by_eve)), (Verdict::Allowed, Verdict::Denied));
}

// ---- Security review: holes found after the self-review, each proved first ----

/// A person's own computer can't join a pool by calling itself a worker: a
/// `pool` admits workers that are boxes (a service member's, or one enrolled
/// for its owner), so a Contributor's laptop with a `kind: worker` record
/// offering the pool's source neither holds the pool agent's keys nor
/// writes its replies.
#[test]
fn sr_a_persons_computer_calling_itself_a_worker_isnt_in_a_pool() {
    let mut w = world();
    let h = host_record(&w, "dee", HostKind::Worker, &["ollama"]);
    let rec = w.push_bare("dee", SessionEvent::WorkspaceHostUpserted { host: h });
    assert_eq!(v(&w, rec), Verdict::Allowed, "dee may describe her own computer");
    let mut a = agent(&w, "ben", spec("ollama", "llama3"), Placement::Pool);
    a.owner_actor_id = "ben".into();
    w.push("ben", up(&a));
    let kd = certify(&mut w, "dee", a.id, 7);
    assert!(w.roster().valid_agent_key(kd.0).is_none(), "a person's computer holds no pool key");
    let chat = w.chat("ben", "plans");
    let q = ChatMessage::new(MessageRole::User, "ben", "hi");
    let qid = q.id;
    w.say(chat, "ben", SessionEvent::MessageAppended { message: q });
    let by_key = agent_say(&mut w, chat, a.id, (kd.0, &kd.1), reply_to(a.id, qid));
    let by_host = w.say(chat, "dee", reply_to(a.id, qid));
    let k1 = certify(&mut w, "box", a.id, 8);
    let by_box = agent_say(&mut w, chat, a.id, (k1.0, &k1.1), reply_to(a.id, qid));
    assert_eq!(
        (v(&w, by_key), v(&w, by_host), v(&w, by_box)),
        (Verdict::Denied, Verdict::Denied, Verdict::Allowed),
        "only the pool's boxes answer for it"
    );
}

/// A delegation never revives a revoked device: an agent key its host
/// certified while the agent ran there doesn't sign the delegated turn once
/// that device is revoked (a stolen laptop's keys).
#[test]
fn sr_a_revoked_devices_agent_key_doesnt_ride_a_delegation() {
    let mut w = world();
    let a = agent(&w, "cy", spec("ollama", "llama3"), Placement::OwnerDevices);
    w.push_bare("cy", up(&a));
    let kc = certify(&mut w, "cy", a.id, 9);
    assert!(w.roster().valid_agent_key(kc.0).is_some(), "bound while on cy's devices");
    let mut moved = folded(&w).unwrap();
    moved.placement = Some(Placement::Host { host_id: host_of(&w, "box") });
    moved.host_id = host_of(&w, "box");
    let m = w.push("ben", up(&moved));
    assert_eq!(v(&w, m), Verdict::Allowed, "an Admin moves it to the box");
    let chat = w.chat("cy", "plans");
    let q = ChatMessage::new(MessageRole::User, "cy", "one");
    let t1 = q.id;
    w.say(chat, "cy", SessionEvent::MessageAppended { message: q });
    let cy_dev = w.people["cy"].device_id;
    let d = w.say(chat, "cy", SessionEvent::TurnDelegated { trigger_id: t1, agent_id: a.id, device_id: cy_dev });
    // The laptop is stolen: an Admin revokes it, holding everything it wrote.
    let signers = std::iter::once(cy_dev).collect();
    let mut wm = held(&w, "cy");
    wm.extend(ligus_core::watermark::heads_of(&w.chats[&chat], chat, &signers));
    let r = w.push("ana", SessionEvent::DeviceRevoked { device_id: cy_dev, watermark: wm });
    assert_eq!((v(&w, d), v(&w, r)), (Verdict::Allowed, Verdict::Allowed));
    let late = agent_say(&mut w, chat, a.id, (kc.0, &kc.1), reply_to(a.id, t1));
    assert_eq!(v(&w, late), Verdict::Denied, "a revoked device's key signs nothing, delegated or not");
}

/// A shared agent (no owner) is the workspace's: moving it, changing its
/// source, or changing its model is an Admin's, as moving it is on rules 3.
/// A Contributor may still rename it.
#[test]
fn sr_a_contributor_cant_move_or_remodel_a_shared_agent() {
    let mut w = world();
    let a = agent(&w, "", spec("ollama", "llama3"), Placement::Host { host_id: host_of(&w, "box") });
    let made = w.push("ben", up(&a));
    assert_eq!(v(&w, made), Verdict::Allowed, "an Admin puts a shared agent on the box");
    let mut off_box = folded(&w).unwrap();
    off_box.placement = Some(Placement::OwnerDevices);
    off_box.host_id = String::new();
    let moved = w.push("cy", up(&off_box));
    let mut remodel = folded(&w).unwrap();
    remodel.run = Some(spec("ollama", "llama3:70b"));
    remodel.runtime_id = legacy_id(remodel.run.as_ref().unwrap());
    let remodeled = w.push("cy", up(&remodel));
    let mut renamed = folded(&w).unwrap();
    renamed.name = "helper".into();
    let rename = w.push_bare("cy", up(&renamed));
    assert_eq!(
        (v(&w, moved), v(&w, remodeled), v(&w, rename)),
        (Verdict::Denied, Verdict::Denied, Verdict::Allowed),
        "a Contributor, even with presence, neither moves nor remodels the workspace's agent"
    );
    let f = folded(&w).unwrap();
    assert_eq!((f.effective_placement(), f.run.unwrap().model), (Placement::Host { host_id: host_of(&w, "box") }, "llama3".to_string()));
}

/// Residual (documented, not fixed): a chat event is judged at the config
/// frontier its author stamps, so a member of a workspace switched from
/// rules 3 who stamps a frontier from before the switch is judged by
/// rules 3 there, as an honest device that hadn't synced the switch yet
/// would be. The rules-4 chat-log gates (G5, G8, G10, G14) then don't bind
/// it. A cut (removal, demotion, revocation) still lifts the frontier.
#[test]
fn sr_residual_a_pre_switch_frontier_is_judged_by_rules_3() {
    let mut w = world();
    let switch = w.cfg.iter().position(|e| matches!(e.payload, SessionEvent::WorkspaceRulesSet { version: 4, .. })).unwrap();
    let before = ligus_core::CfgFrontier { lamport: w.cfg[switch - 1].lamport, event_id: w.cfg[switch - 1].event_id };
    let chat = w.chat("cy", "plans");
    let forged = |w: &SignedWorld| {
        let mut m = ChatMessage::new(MessageRole::User, "ana", "as ana");
        m.actor_identity = Some(w.people["ana"].actor());
        SessionEvent::MessageAppended { message: m }
    };
    let at_head = w.say(chat, "cy", forged(&w));
    let stale = w.write("cy", Some(chat), forged(&w), Some(before), Proof::None);
    assert_eq!((v(&w, at_head), v(&w, stale)), (Verdict::Denied, Verdict::Allowed), "the residual");
}
