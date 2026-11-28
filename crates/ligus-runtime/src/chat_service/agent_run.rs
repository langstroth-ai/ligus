//! Writing an agent's run and placement (workspace model v3, 5.2, 5.3 and
//! M13 of 16.2).
//!
//! Every agent this build writes carries `run` and `placement` in the same
//! record as its name, runtime and host: one event. Under rules 3 they only
//! mirror `runtime_id` and `host_id` (`ligus_core::agent_run`), so writing
//! them never needs an Admin or a passkey, and older peers, which read only
//! the legacy fields, route the agent the same way.

use uuid::Uuid;

use ligus_core::agent_run::{AgentRun, ResolvedRun, RunRules};
use ligus_core::{workspace_config_session_id, SessionEvent, WorkspaceAgent, WorkspaceRole};

use super::{ChatError, ChatService, Result};

impl ChatService {
    /// What workspace runtime `runtime_id` resolves to here, as a display
    /// hint (harness and model only; never an address or a key).
    pub(crate) fn resolved_hint(&self, workspace_id: Uuid, runtime_id: &str) -> Option<ResolvedRun> {
        let rt = self.list_workspace_runtimes(workspace_id).ok()?.into_iter().find(|r| r.id == runtime_id)?;
        let harness = serde_json::to_value(rt.provider).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
        Some(ResolvedRun { harness, model: rt.model })
    }

    /// `agent` as this build writes it: its run and placement mirror its
    /// runtime and host, an agent whose host is gone keeps naming that host,
    /// and the writer's display hint rides along.
    /// On rules 4 an agent that carries a full run spec (or a pool) is
    /// written as it is (`WorkspaceAgent::for_write_at`).
    pub(crate) fn agent_for_write(&self, workspace_id: Uuid, agent: &WorkspaceAgent) -> WorkspaceAgent {
        let rules = self.workspace_config(workspace_id).ok().flatten().map(|c| c.session().authz.rules).unwrap_or(0);
        let run_rules = if rules >= ligus_core::authz::RULES_V4 { RunRules::Full } else { RunRules::Mirror };
        agent.for_write_at(self.resolved_hint(workspace_id, &agent.runtime_id), run_rules)
    }

    /// Refuse to place `agent` on a host whose published record says it
    /// can't run it (5.5): its runtime goes through a model gateway the host
    /// doesn't offer. A host with no record here (not published yet) and
    /// the owner's devices are not judged, nor is an edit that keeps the
    /// agent's host and runtime (a rename, its access settings).
    ///
    /// A full run spec (rules 4) is also refused on a host that doesn't
    /// offer its source, and on one whose signed beat reports a build below
    /// 0.5, which can't run it (workspace model v3, 5.4, H16).
    pub(crate) fn refuse_unoffered_placement(&self, workspace_id: Uuid, agent: &WorkspaceAgent) -> Result<()> {
        if agent.host_id.is_empty() {
            return Ok(());
        }
        let existing = self.workspace_config(workspace_id)?.and_then(|c| c.agent(agent.id).cloned());
        if existing.is_some_and(|e| e.host_id == agent.host_id && e.runtime_id == agent.runtime_id) {
            return Ok(());
        }
        let Some(host) = self.list_hosts(workspace_id)?.into_iter().find(|h| h.id == agent.host_id) else {
            return Ok(());
        };
        let label = || if host.label.trim().is_empty() { host.id.clone() } else { host.label.clone() };
        let spec = if ligus_core::agent_run::is_full_spec_id(&agent.runtime_id) {
            Some(ligus_core::agent_run::from_legacy_id(&agent.runtime_id))
        } else {
            agent.run.clone().filter(|r| r.runtime_ref().is_none() && !r.source.is_empty())
        };
        if let Some(spec) = spec {
            let source = spec.source;
            if host.offers.is_some() && !host.offers_source(&source) {
                return Err(ChatError::Unauthorized(format!("{} doesn't offer {source}", label())));
            }
            if let Some(build) = self.host_build(&host.id).filter(|b| !build_runs_full_specs(b)) {
                return Err(ChatError::Unauthorized(format!(
                    "{} runs Ligus {build}, which can't run an agent's own model choice: update it to 0.5 or later first",
                    label()
                )));
            }
        }
        let Some(rt) = self.list_workspace_runtimes(workspace_id)?.into_iter().find(|r| r.id == agent.runtime_id) else {
            return Ok(());
        };
        if !rt.gateway.is_empty() && !host.offers_gateway(&rt.gateway) {
            return Err(ChatError::Unauthorized(format!("{} doesn't offer {}", label(), rt.gateway)));
        }
        Ok(())
    }

    /// Change agent `agent_id` in one record (Change model…, Runs on…;
    /// workspace model v3, 5.7 and 17.4): name `runtime_id` instead of what
    /// it runs now, carry `run` (rules 4 only: `{harness, source, model}`),
    /// and/or place it on `host_id` (`""`: its owner's own devices). `None`
    /// keeps what it has. The fold judges the merged record: on rules 4 a
    /// model change within one source on your own agent needs neither an
    /// Admin nor a passkey; a move or a source change follows the placement
    /// rules (G1, G2); a shared agent is an Admin's. On rules 3 any change
    /// of runtime or host is an Admin's, with a passkey.
    pub fn edit_agent(&mut self, workspace_id: Uuid, agent_id: Uuid, runtime_id: Option<&str>, run: Option<AgentRun>, host_id: Option<&str>) -> Result<()> {
        let config_id = self.ensure_workspace_config(workspace_id)?;
        let config = self.workspace_config(workspace_id)?;
        let rules = config.as_ref().map(|c| c.session().authz.rules).unwrap_or(0);
        let Some(current) = config.as_ref().and_then(|c| c.agent(agent_id)).cloned() else {
            return Err(ChatError::NotFound("unknown agent".into()));
        };
        if run.is_some() && rules < ligus_core::authz::RULES_V4 {
            return Err(ChatError::Unauthorized(
                "an agent carries its own model choice on rules 4: on this workspace its model belongs to its runtime".into(),
            ));
        }
        if let Some(r) = &run {
            if r.source.is_empty() || r.source.starts_with(ligus_core::agent_run::RUNTIME_SOURCE_PREFIX) {
                return Err(ChatError::Unauthorized("a run spec names a source a host offers".into()));
            }
        }
        // An agent whose host is gone keeps naming it (a model change isn't
        // a move).
        let mut agent = current.as_judged().into_owned();
        if let Some(h) = host_id {
            agent.place(h);
        }
        if let Some(r) = runtime_id {
            agent.runtime_id = r.to_string();
            agent.run = None;
        }
        if let Some(mut r) = run {
            r.resolved = None;
            agent.run = Some(r);
        }
        // As written (a run spec's legacy id included), so a source change
        // on the same host is judged too.
        let agent = self.agent_for_write(workspace_id, &agent);
        self.ensure_own_host_record(workspace_id, &agent.host_id)?;
        self.refuse_unoffered_placement(workspace_id, &agent)?;
        self.append_signed(config_id, workspace_id, SessionEvent::AgentUpserted { agent })?;
        Ok(())
    }

    /// "Run here this time" on rules 4 (workspace model v3, 5.3, G14): sign
    /// a `TurnDelegated` that lets this device answer the turn `trigger_id`
    /// starts for `agent_id`, once, before running it. A no-op below rules
    /// 4 (where only the owner's own devices answer, as before), for an
    /// agent this device already runs by its placement, and when the turn
    /// is already delegated to this device. Returns whether it wrote one.
    pub fn delegate_turn(&mut self, session_id: Uuid, workspace_id: Uuid, trigger_id: Uuid, agent_id: Uuid) -> Result<bool> {
        let (config, timeline) = self.store.load_config(workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
        if config.as_ref().is_none_or(|c| c.authz.rules < ligus_core::authz::RULES_V4) {
            return Ok(false);
        }
        if timeline.current().agent_bound_to(agent_id, self.device_id) {
            return Ok(false);
        }
        let chat = self.load_in(workspace_id, session_id)?.ok_or_else(|| ChatError::NotFound("unknown chat".into()))?;
        match chat.authz.delegations.get(&trigger_id) {
            Some(d) if d.agent_id == agent_id && d.device_id == self.device_id => return Ok(false),
            Some(_) => return Err(ChatError::Unauthorized("that turn is already delegated to another device".into())),
            None => {}
        }
        let device_id = self.device_id;
        self.append_signed(session_id, workspace_id, SessionEvent::TurnDelegated { trigger_id, agent_id, device_id })?;
        Ok(true)
    }

    /// Rules 4 (G2): an agent placed on this device needs this device's own
    /// host record, signed here. Written first when it is missing (its
    /// offers are left for the host loop to publish); a no-op for another
    /// host, below rules 4, and when the record exists.
    pub(crate) fn ensure_own_host_record(&mut self, workspace_id: Uuid, host_id: &str) -> Result<()> {
        if host_id.is_empty() || host_id != self.device_id.to_string() || self.local_workspace_id == Some(workspace_id) {
            return Ok(());
        }
        let rules = self.workspace_config(workspace_id)?.map(|c| c.session().authz.rules).unwrap_or(0);
        if rules < ligus_core::authz::RULES_V4 || self.list_hosts(workspace_id)?.iter().any(|h| h.id == host_id) {
            return Ok(());
        }
        let host = ligus_core::WorkspaceHost::new(host_id, ligus_core::HostKind::Device, String::new());
        self.ensure_host(workspace_id, host).map(|_| ())
    }

    /// The build a host's last signed beat reported, when this service was
    /// given the hosts' presence ([`ChatService::set_host_presence`]).
    pub(crate) fn host_build(&self, host_id: &str) -> Option<String> {
        self.host_presence.as_ref()?.status_of(host_id).map(|s| s.build).filter(|b| !b.is_empty())
    }

    /// M13 (rules 3): write `run` and `placement` for the agents this
    /// device's person owns that don't carry them yet. `runtime_id` and
    /// `host_id` are written unchanged, so no Admin or passkey is needed and
    /// older peers see no difference. Agents whose host is gone are left for
    /// their owner to place again. Idempotent: an agent that already carries
    /// both is skipped, and two of the person's devices racing write the
    /// same record. Returns how many records were written.
    pub fn mirror_agent_run_fields(&mut self, workspace_id: Uuid) -> Result<usize> {
        let config_id = workspace_config_session_id(workspace_id);
        let Some(config) = self.load_in(workspace_id, config_id)? else { return Ok(0) };
        // Rules 4 offers "Pin @x to ..." instead (it changes what an agent
        // runs); a read-only workspace takes no writes.
        if config.authz.read_only() || config.authz.rules > ligus_core::authz::RULES_V3 {
            return Ok(0);
        }
        let me = self.author().id.clone();
        if me.is_empty() || config.role_of(&me).rank() < WorkspaceRole::Contributor.rank() {
            return Ok(0);
        }
        let todo: Vec<WorkspaceAgent> = config
            .workspace_agents
            .iter()
            .filter(|a| a.owner_actor_id == me && !a.is_host_gone() && (a.run.is_none() || a.placement.is_none()))
            .cloned()
            .collect();
        let mut written = 0;
        for a in todo {
            let agent = self.agent_for_write(workspace_id, &a);
            debug_assert!(agent.runtime_id == a.runtime_id && agent.host_id == a.host_id);
            self.append_signed(config_id, workspace_id, SessionEvent::AgentUpserted { agent })?;
            written += 1;
        }
        Ok(written)
    }
}

/// Whether a host on `build` (its beat's version, "0.5.0") runs full run
/// specs: 0.5 and later. An unparsable build is taken as a newer one (it
/// isn't refused on a guess).
pub fn build_runs_full_specs(build: &str) -> bool {
    let mut parts = build.trim().trim_start_matches('v').split(['.', '-', '+']).map(|p| p.parse::<u64>());
    match (parts.next(), parts.next()) {
        (Some(Ok(major)), Some(Ok(minor))) => (major, minor) >= (0, 5),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_service::as_an_older_build;
    use crate::event_store::EventStore;
    use ligus_core::agent_run::{AgentRun, Placement};
    use ligus_core::{ActorIdentity, SigningKeypair};

    /// The rules-3 writer (M13 and the mirror are rules 3's).
    fn svc() -> ChatService {
        let store = EventStore::open_in_memory().unwrap();
        let author = ActorIdentity::new("u1", "Mara", ligus_core::ActorKind::Human);
        let mut svc = ChatService::new(store, SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), author);
        svc.set_founding_rules(ligus_core::authz::RULES_V3);
        svc
    }

    /// A person with an account, on a workspace this build founds (rules 4).
    fn svc4() -> (ChatService, Uuid, Uuid) {
        let store = EventStore::open_in_memory().unwrap();
        let account = Uuid::new_v4();
        let mut author = ActorIdentity::new(account.to_string(), "Mara", ligus_core::ActorKind::Human);
        author.account_id = Some(account);
        let mut svc = ChatService::new(store, SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), author);
        let chat = svc.create_chat("t", Uuid::new_v4(), "rt").unwrap();
        let ws = chat.workspace_id;
        // The founder's first credential moves a new workspace to rules 4.
        found_with_passkey(&mut svc, ws, account);
        assert_eq!(svc.workspace_config(ws).unwrap().unwrap().session().authz.rules, ligus_core::authz::RULES_V4, "founded on rules 4");
        (svc, ws, chat.id)
    }

    /// The founding path: the founder's first passkey moves the workspace
    /// from its genesis rules to this build's founding rules (4).
    fn found_with_passkey(svc: &mut ChatService, ws: Uuid, account: Uuid) {
        let key = ligus_core::testkit::test_recovery_key(account);
        svc.publish_identity(workspace_config_session_id(ws), ws).unwrap();
        svc.register_passkey(ws, key.credential(account, "Test recovery key"), &|req| Ok(key.assert(&req.challenge))).unwrap();
    }

    fn full_agent(svc: &ChatService, source: &str, model: &str) -> WorkspaceAgent {
        let run = AgentRun { harness: String::new(), source: source.into(), model: model.into(), resolved: None };
        let mut a = WorkspaceAgent::new("scout", ligus_core::agent_run::legacy_id(&run));
        a.owner_actor_id = svc.author().id.clone();
        a.run = Some(run);
        a
    }

    /// Rules 4: an agent's own run spec is written as it is, in one record,
    /// and placing it on this device writes this device's host record first
    /// (G2 needs it).
    #[test]
    fn rules_4_writes_the_full_run_spec_and_this_devices_host_record() {
        let (mut svc, ws, chat) = svc4();
        let a = full_agent(&svc, "claude-code", "opus");
        let id = a.id;
        svc.add_agent(chat, ws, a).unwrap();
        let got = agent_in(&svc, ws, id);
        assert_eq!(got.run.as_ref().map(|r| (r.source.as_str(), r.model.as_str())), Some(("claude-code", "opus")));
        assert!(ligus_core::agent_run::is_full_spec_id(&got.runtime_id));
        let here = svc.device_id().to_string();
        assert!(svc.list_hosts(ws).unwrap().iter().all(|h| h.id != here));
        svc.set_agent_host(ws, id, &here).unwrap();
        assert!(svc.list_hosts(ws).unwrap().iter().any(|h| h.id == here), "its own host record, signed here");
        let got = agent_in(&svc, ws, id);
        assert_eq!(got.placement, Some(Placement::Host { host_id: here.clone() }));
        assert_eq!(got.run.unwrap().model, "opus", "the run spec stays");
    }

    /// H16 (client): a full run spec is never placed on a host whose signed
    /// beat reports a build below 0.5, nor on one that doesn't offer its
    /// source.
    #[test]
    fn a_full_run_spec_is_refused_on_a_host_below_0_5_or_without_the_source() {
        let (mut svc, ws, chat) = svc4();
        let here = svc.device_id().to_string();
        let mut host = ligus_core::WorkspaceHost::new(here.clone(), ligus_core::HostKind::Worker, "build-1");
        host.offers = Some(vec![ligus_core::HostOffer { name: "codex".into(), kind: ligus_core::OfferKind::Cli, harness: "codex".into(), wire: String::new() }]);
        svc.register_host(ws, host).unwrap();
        let presence = crate::HostPresence::new();
        let now_ms = ligus_core::Timestamp::now().inner().unix_timestamp() as u64 * 1000;
        presence.record_status(&here, now_ms, ligus_core::HostStatus { build: "0.4.1".into(), ..Default::default() });
        svc.set_host_presence(presence.clone());
        let a = full_agent(&svc, "codex", "gpt-5.3-codex");
        let id = a.id;
        svc.add_agent(chat, ws, a).unwrap();
        let err = svc.set_agent_host(ws, id, &here).unwrap_err().to_string();
        assert!(err.contains("build-1 runs Ligus 0.4.1"), "{err}");
        presence.record_status(&here, now_ms + 1, ligus_core::HostStatus { build: "0.5.0".into(), ..Default::default() });
        svc.set_agent_host(ws, id, &here).unwrap();
        let b = full_agent(&svc, "ollama", "llama3");
        let bid = b.id;
        svc.add_agent(chat, ws, b).unwrap();
        let err = svc.set_agent_host(ws, bid, &here).unwrap_err().to_string();
        assert!(err.contains("build-1 doesn't offer ollama"), "{err}");
        assert!(build_runs_full_specs("0.5.0") && build_runs_full_specs("v1.2") && build_runs_full_specs("dev"));
        assert!(!build_runs_full_specs("0.4.1") && !build_runs_full_specs("0.4.0-rc1"));
    }

    /// "Run here this time" on rules 4 signs a one-turn delegation; on rules
    /// 3, and for an agent this device already runs, it writes nothing.
    #[test]
    fn delegating_a_turn_writes_one_event_on_rules_4_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ligus.db");
        let account = Uuid::new_v4();
        let seed = SigningKeypair::generate().unwrap().seed_bytes();
        let open = || {
            let mut author = ActorIdentity::new(account.to_string(), "Mara", ligus_core::ActorKind::Human);
            author.account_id = Some(account);
            ChatService::new(EventStore::open(&path).unwrap(), SigningKeypair::generate().unwrap(), SigningKeypair::from_seed(&seed).unwrap(), author)
        };
        let mut desk = open();
        let chat = desk.create_chat("t", Uuid::new_v4(), "rt").unwrap();
        let (ws, chat) = (chat.workspace_id, chat.id);
        found_with_passkey(&mut desk, ws, account);
        // Mara's box: another device of hers, with its own host record.
        let mut boxed = open();
        assert!(boxed.confirm_device(ws).unwrap(), "her second device, with her passkey");
        let box_id = boxed.device_id().to_string();
        let mut h = ligus_core::WorkspaceHost::new(box_id.clone(), ligus_core::HostKind::Worker, "box-1");
        h.offers = Some(vec![ligus_core::HostOffer { name: "claude-code".into(), kind: ligus_core::OfferKind::Cli, harness: "claudeCode".into(), wire: String::new() }]);
        boxed.register_host(ws, h).unwrap();
        let a = full_agent(&desk, "claude-code", "");
        let id = a.id;
        desk.add_agent(chat, ws, a).unwrap();
        let q = desk.post_user_message(chat, ws, "@scout hi").unwrap();
        assert!(!desk.delegate_turn(chat, ws, q.id, id).unwrap(), "on the owner's devices it runs here already");
        desk.set_agent_host(ws, id, &box_id).unwrap();
        assert!(desk.delegate_turn(chat, ws, q.id, id).unwrap());
        assert!(!desk.delegate_turn(chat, ws, q.id, id).unwrap(), "once");
        let d = desk.load_in(ws, chat).unwrap().unwrap().authz.delegations.get(&q.id).copied().unwrap();
        assert_eq!((d.agent_id, d.device_id), (id, desk.device_id()));
        // The delegated device's reply for that turn is accepted.
        let m = desk.begin_assistant_message(chat, ws, "scout", &agent_in(&desk, ws, id).runtime_id, Some(id), Some(q.id)).unwrap();
        desk.complete_assistant_message(chat, ws, m, "done").unwrap();
        assert!(desk.load_in(ws, chat).unwrap().unwrap().messages.iter().any(|x| x.id == m && x.body == "done"));
        // Not for another turn.
        let q2 = desk.post_user_message(chat, ws, "@scout again").unwrap();
        assert!(desk.begin_assistant_message(chat, ws, "scout", &agent_in(&desk, ws, id).runtime_id, Some(id), Some(q2.id)).is_err());

        let mut old = svc();
        let c3 = old.create_chat("t", Uuid::new_v4(), "rt").unwrap();
        let q3 = old.post_user_message(c3.id, c3.workspace_id, "hi").unwrap();
        assert!(!old.delegate_turn(c3.id, c3.workspace_id, q3.id, Uuid::new_v4()).unwrap(), "rules 3: nothing written");
    }

    fn agent_in(svc: &ChatService, ws: Uuid, id: Uuid) -> WorkspaceAgent {
        svc.workspace_config(ws).unwrap().unwrap().agent(id).cloned().unwrap()
    }

    #[test]
    fn a_new_agent_is_one_record_with_its_run_and_placement() {
        let mut svc = svc();
        let chat = svc.create_chat("t", Uuid::new_v4(), "rt").unwrap();
        let ws = chat.workspace_id;
        svc.ensure_workspace_config(ws).unwrap();
        let before = svc.store().list_in(ws, workspace_config_session_id(ws)).unwrap().len();
        let mut a = WorkspaceAgent::new("scout", "ws-claude");
        a.owner_actor_id = svc.author().id.clone();
        a.host_id = "worker-9".into();
        let id = a.id;
        svc.add_agent(chat.id, ws, a).unwrap();
        let after = svc.store().list_in(ws, workspace_config_session_id(ws)).unwrap();
        let new: Vec<_> = after[before..].iter().filter(|e| matches!(e.payload, SessionEvent::AgentUpserted { .. })).collect();
        assert_eq!(new.len(), 1, "one event");
        let got = agent_in(&svc, ws, id);
        assert_eq!(got.run, Some(AgentRun::mirror("ws-claude", None)));
        assert_eq!(got.placement, Some(Placement::Host { host_id: "worker-9".into() }));
        assert_eq!((got.runtime_id.as_str(), got.host_id.as_str()), ("ws-claude", "worker-9"));
    }

    #[test]
    fn m13_mirrors_only_my_agents_and_never_rewrites_runtime_id() {
        let mut svc = svc();
        let chat = svc.create_chat("t", Uuid::new_v4(), "rt").unwrap();
        let ws = chat.workspace_id;
        let cfg = workspace_config_session_id(ws);
        svc.ensure_workspace_config(ws).unwrap();
        // Agents written the 0.4 way: no run, no placement.
        let mut mine = WorkspaceAgent::new("mine", "ws-claude");
        mine.owner_actor_id = svc.author().id.clone();
        mine.host_id = "worker-9".into();
        let mut theirs = WorkspaceAgent::new("theirs", "rt-2");
        theirs.owner_actor_id = "someone-else".into();
        for a in [&mine, &theirs] {
            as_an_older_build(|| svc.append_signed(cfg, ws, SessionEvent::AgentUpserted { agent: a.clone() })).unwrap();
        }
        assert!(agent_in(&svc, ws, mine.id).run.is_none());
        assert_eq!(svc.mirror_agent_run_fields(ws).unwrap(), 1);
        let m = agent_in(&svc, ws, mine.id);
        assert_eq!(m.run.as_ref().and_then(|r| r.runtime_ref()), Some("ws-claude"));
        assert_eq!(m.placement, Some(Placement::Host { host_id: "worker-9".into() }));
        assert_eq!((m.runtime_id.as_str(), m.host_id.as_str()), ("ws-claude", "worker-9"), "runtime_id untouched");
        assert!(agent_in(&svc, ws, theirs.id).run.is_none(), "only the person's own agents");
        assert_eq!(svc.mirror_agent_run_fields(ws).unwrap(), 0, "idempotent");
    }

    #[test]
    fn placing_on_a_host_that_doesnt_offer_the_gateway_is_refused() {
        let mut svc = svc();
        let chat = svc.create_chat("t", Uuid::new_v4(), "rt").unwrap();
        let ws = chat.workspace_id;
        let mut rt = ligus_core::WorkspaceRuntime::new("gw-rt", "Gateway", ligus_core::ModelProviderKind::OpenAI, "m");
        rt.gateway = "gw-home".into();
        svc.add_workspace_runtime(ws, rt).unwrap();
        // Readers count a host record only when the device it names signed
        // it, so the host is this device's own record, labelled "build-1".
        let host_id = svc.device_id().to_string();
        let host = ligus_core::WorkspaceHost::new(host_id.clone(), ligus_core::HostKind::Worker, "build-1");
        svc.register_host(ws, host).unwrap();
        let mut a = WorkspaceAgent::new("gw", "gw-rt");
        a.owner_actor_id = svc.author().id.clone();
        let id = a.id;
        svc.add_agent(chat.id, ws, a.clone()).unwrap();
        let err = svc.set_agent_host(ws, id, &host_id).unwrap_err().to_string();
        assert!(err.contains("build-1 doesn't offer gw-home"), "{err}");
        a.host_id = host_id;
        assert!(svc.add_agent(chat.id, ws, a).is_err());
        assert_eq!(agent_in(&svc, ws, id).host_id, "", "nothing was written");
    }
}
