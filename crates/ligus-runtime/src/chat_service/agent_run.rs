//! Writing an agent's run and placement (workspace model v3, 5.2, 5.3 and
//! M13 of 16.2).
//!
//! Every agent this build writes carries `run` and `placement` in the same
//! record as its name, runtime and host: one event. Under rules 3 they only
//! mirror `runtime_id` and `host_id` (`ligus_core::agent_run`), so writing
//! them never needs an Admin or a passkey, and older peers, which read only
//! the legacy fields, route the agent the same way.

use uuid::Uuid;

use ligus_core::agent_run::ResolvedRun;
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
    pub(crate) fn agent_for_write(&self, workspace_id: Uuid, agent: &WorkspaceAgent) -> WorkspaceAgent {
        agent.for_write(self.resolved_hint(workspace_id, &agent.runtime_id))
    }

    /// Refuse to place `agent` on a host whose published record says it
    /// can't run it (5.5): its runtime goes through a model gateway the host
    /// doesn't offer. A host with no record here (not published yet) and
    /// the owner's devices are not judged, nor is an edit that keeps the
    /// agent's host and runtime (a rename, its access settings).
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
        let Some(rt) = self.list_workspace_runtimes(workspace_id)?.into_iter().find(|r| r.id == agent.runtime_id) else {
            return Ok(());
        };
        if !rt.gateway.is_empty() && !host.offers_gateway(&rt.gateway) {
            let label = if host.label.trim().is_empty() { host.id.clone() } else { host.label.clone() };
            return Err(ChatError::Unauthorized(format!("{label} doesn't offer {}", rt.gateway)));
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_service::as_an_older_build;
    use crate::event_store::EventStore;
    use ligus_core::agent_run::{AgentRun, Placement};
    use ligus_core::{ActorIdentity, SigningKeypair};

    fn svc() -> ChatService {
        let store = EventStore::open_in_memory().unwrap();
        let author = ActorIdentity::new("u1", "Mara", ligus_core::ActorKind::Human);
        ChatService::new(store, SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), author)
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
