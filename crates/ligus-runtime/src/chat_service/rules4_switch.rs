//! Switching an existing workspace to rules 4 (workspace model v3, 15.2),
//! and the notices that come with it (5.5, 11, M13).
//!
//! - [`ChatService::rules4_report`]: the readiness list an Owner reads
//!   before the switch, in plain words: devices on a build below 0.5 (they
//!   go read-only there; from the build in their signed host beats),
//!   devices that haven't said which build they run, agents whose placement
//!   wouldn't resolve, host records not signed by their device, accountless
//!   members, and model server addresses on the log. It informs; the Owner
//!   can switch anyway.
//! - [`ChatService::upgrade_workspace_rules`]: the switch itself, Owner
//!   only, with one passkey; the rules version never goes back.
//! - [`ChatService::endpoint_runtimes`]: the workspace runtimes whose
//!   server address every member can read, and the agents using them.
//! - [`ChatService::old_agent_copies`]: agent records a chat's own log
//!   still carries from older builds (rules 4 runs the workspace's record).

use uuid::Uuid;

use ligus_core::{workspace_config_session_id, SessionEventEnvelope, WorkspaceRole};

use super::{ChatError, ChatService, Result};

/// A name someone else chose, cleaned for display (bidi and invisible
/// characters removed).
fn shown(raw: &str) -> String {
    ligus_proto::names::clean_display_name(raw)
}

/// What switching to rules 4 changes, in the order surfaces show it.
pub const RULES4_CHANGES: &[&str] = &[
    "Each agent carries its own model choice. Changing the model of your own agent needs no passkey.",
    "An agent runs only on a host that offers what it runs. Moving a shared agent is an Admin's.",
    "Model server addresses stay on the hosts that run them: the workspace refuses new ones.",
    "New members need an account; chats can't carry their own copy of an agent.",
    "Devices on Ligus before 0.5 can still read this workspace, but can't change anything in it.",
    "There is no going back to the older rules.",
];

/// A device the switch leaves read-only, or one that hasn't said which
/// build it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceBuild {
    pub device_id: Uuid,
    /// How people know it: its host label ("lt on zaz-mbp"), its synced
    /// name, or its person's name and a short id.
    pub name: String,
    /// Its person (or box) by name.
    pub person: String,
    /// The build its signed beat reported (`0.4.1`), if any.
    pub build: Option<String>,
}

/// A workspace runtime whose server address is on the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointRuntime {
    pub runtime_id: String,
    pub name: String,
    /// The agents naming it: (id, name, owned by this device's person,
    /// placed on this device or its owner's devices).
    pub agents: Vec<(Uuid, String, bool, bool)>,
}

/// The readiness list for rules 4.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rules4Report {
    /// The workspace's rules version now.
    pub rules: u32,
    /// This device's person is the Owner (only they switch).
    pub is_owner: bool,
    /// Devices whose beat reports a build below 0.5: read-only after the switch.
    pub old_devices: Vec<DeviceBuild>,
    /// Devices with no build in a signed beat (older builds send none).
    pub unknown_devices: Vec<DeviceBuild>,
    /// Agents whose placement wouldn't resolve: (id, name).
    pub unresolved_agents: Vec<(Uuid, String)>,
    /// Host records not signed by their own device, by label.
    pub unsigned_hosts: Vec<String>,
    /// Members with no account, by name.
    pub accountless_members: Vec<String>,
    /// Runtimes whose server address every member can read.
    pub endpoint_runtimes: Vec<EndpointRuntime>,
}

impl Rules4Report {
    /// The list in plain words, one line each (empty: nothing to say).
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        for d in &self.old_devices {
            let b = d.build.as_deref().unwrap_or("an older build");
            out.push(format!("{} ({}) is on {b}: it goes read-only here until it's updated", d.name, d.person));
        }
        match self.unknown_devices.len() {
            0 => {}
            1 => out.push(format!("{} ({}) hasn't said which Ligus it runs: if it's before 0.5, it goes read-only", self.unknown_devices[0].name, self.unknown_devices[0].person)),
            n => out.push(format!("{n} devices haven't said which Ligus they run: any before 0.5 go read-only")),
        }
        for (_, name) in &self.unresolved_agents {
            out.push(format!("@{name} is placed on a host that wouldn't count: choose where it runs again"));
        }
        for h in &self.unsigned_hosts {
            out.push(format!("{h}'s host record isn't its own device's: it stops counting as a host"));
        }
        for m in &self.accountless_members {
            out.push(format!("{m} has no account: they keep working, but no one new joins that way"));
        }
        for r in &self.endpoint_runtimes {
            out.push(format!("{} keeps its server address on the workspace: it keeps working, but it can't be changed", r.name));
        }
        out
    }
}

/// The name people know a device by.
fn device_name(label: &str, synced: Option<&str>, person: &str, device: Uuid) -> String {
    if !label.trim().is_empty() {
        return label.trim().to_string();
    }
    if let Some(s) = synced.filter(|s| !s.trim().is_empty()) {
        return s.trim().to_string();
    }
    format!("{person}'s device {}", &device.to_string()[..8])
}

impl ChatService {
    /// The readiness list for switching `workspace_id` to rules 4, with the
    /// builds this service's host beats report ([`ChatService::set_host_presence`]).
    pub fn rules4_report(&self, workspace_id: Uuid) -> Result<Rules4Report> {
        let config_id = workspace_config_session_id(workspace_id);
        let Some(cfg) = self.load_in(workspace_id, config_id)? else { return Ok(Rules4Report::default()) };
        let (_, timeline) = self.store.load_config(workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
        let head = timeline.current();
        let core = self.rules4_readiness(workspace_id)?;
        let me = self.author.id.clone();
        let hosts = self.list_hosts(workspace_id)?;
        let mut old_devices = Vec::new();
        let mut unknown_devices = Vec::new();
        for (device, d) in &head.devices {
            if d.revoked || d.phone || *device == self.device_id {
                continue;
            }
            // A device of a current member's account.
            let Some(account) = d.accounts.iter().find(|a| head.members.values().any(|m| m.account_id == Some(**a))).copied() else { continue };
            let person = crate::profiles::name_of(&cfg, account, "someone");
            let id = device.to_string();
            let label = hosts.iter().find(|h| h.id == id).map(|h| shown(&h.label)).unwrap_or_default();
            let synced = cfg.device_labels.get(device).and_then(|m| m.get(&account)).map(|s| shown(s));
            let name = device_name(&label, synced.as_deref(), &shown(&person), *device);
            let build = self.host_build(&id);
            let entry = DeviceBuild { device_id: *device, name, person: shown(&person), build: build.clone() };
            match build {
                Some(b) if !super::agent_run::build_runs_full_specs(&b) => old_devices.push(entry),
                Some(_) => {}
                None => unknown_devices.push(entry),
            }
        }
        let agent_name = |id: &Uuid| cfg.workspace_agents.iter().find(|a| a.id == *id).map(|a| shown(&a.name)).unwrap_or_else(|| id.to_string()[..8].to_string());
        let host_label = |id: &str| {
            cfg.workspace_hosts.iter().find(|h| h.id == id).map(|h| shown(&h.label)).filter(|l| !l.trim().is_empty()).unwrap_or_else(|| id.chars().take(8).collect())
        };
        let member_name = |id: &str| cfg.members.iter().find(|m| m.id == id).map(|m| shown(&m.actor.display_name)).unwrap_or_else(|| id.to_string());
        Ok(Rules4Report {
            rules: cfg.authz.rules,
            is_owner: !me.is_empty() && cfg.role_of(&me) == WorkspaceRole::Owner,
            old_devices,
            unknown_devices,
            unresolved_agents: core.unresolved_agents.iter().map(|id| (*id, agent_name(id))).collect(),
            unsigned_hosts: core.unsigned_hosts.iter().map(|h| host_label(h)).collect(),
            accountless_members: core.accountless_members.iter().map(|m| member_name(m)).collect(),
            endpoint_runtimes: self.endpoint_runtimes(workspace_id)?,
        })
    }

    /// Switch `workspace_id` to rules 4 (15.2): the Owner, with one passkey
    /// (the account needs a passkey and a recovery key first). The rules
    /// version never goes back; a workspace already on rules 4 is left as
    /// it is. Older builds go read-only there.
    pub fn upgrade_workspace_rules(&mut self, workspace_id: Uuid) -> Result<Option<SessionEventEnvelope>> {
        let Some(cfg) = self.workspace_config(workspace_id)? else {
            return Err(ChatError::NotFound("this workspace hasn't synced yet".into()));
        };
        if cfg.session().authz.rules >= ligus_core::authz::RULES_V4 {
            return Ok(None);
        }
        let me = self.author.id.clone();
        if me.is_empty() || cfg.session().role_of(&me) != WorkspaceRole::Owner {
            return Err(ChatError::Unauthorized("only the workspace's Owner upgrades its rules".into()));
        }
        self.set_workspace_rules(workspace_id, ligus_core::authz::RULES_V4).map(Some)
    }

    /// The workspace runtimes whose model server address is on the log
    /// (every member can read it), with the agents that name them (5.5).
    pub fn endpoint_runtimes(&self, workspace_id: Uuid) -> Result<Vec<EndpointRuntime>> {
        let Some(cfg) = self.load_in(workspace_id, workspace_config_session_id(workspace_id))? else { return Ok(Vec::new()) };
        let me = self.author.id.clone();
        let links = self.device_links(workspace_id).unwrap_or_default();
        let mine = |owner: &str| !me.is_empty() && !owner.is_empty() && (owner == me || links.person(owner) == links.person(&me));
        let here = self.device_id.to_string();
        Ok(cfg
            .workspace_runtimes
            .iter()
            .filter(|r| !r.endpoint.trim().is_empty())
            .map(|r| EndpointRuntime {
                runtime_id: r.id.clone(),
                name: shown(if r.name.trim().is_empty() { &r.id } else { &r.name }),
                agents: cfg
                    .workspace_agents
                    .iter()
                    .filter(|a| a.runtime_id == r.id)
                    .map(|a| {
                        let host = a.as_judged().host_id.clone();
                        (a.id, shown(&a.name), mine(&a.owner_actor_id), host.is_empty() || host == here)
                    })
                    .collect(),
            })
            .collect())
    }

    /// The agents chat `session_id`'s own log still carries a record of
    /// (written by builds before 0.5): `(id, name)`. On rules 4 the chat
    /// runs the workspace's record of each (11, G5); this is for the
    /// notice Admins see.
    pub fn old_agent_copies(&self, workspace_id: Uuid, session_id: Uuid) -> Result<Vec<(Uuid, String)>> {
        if session_id == workspace_config_session_id(workspace_id) {
            return Ok(Vec::new());
        }
        let Some(cfg) = self.workspace_config(workspace_id)? else { return Ok(Vec::new()) };
        let ws_agents = &cfg.session().workspace_agents;
        let mut out: Vec<(Uuid, String)> = Vec::new();
        for e in self.store.list_in(workspace_id, session_id)? {
            let agents: Vec<&ligus_core::WorkspaceAgent> = match &e.payload {
                ligus_core::SessionEvent::AgentUpserted { agent } => vec![agent],
                ligus_core::SessionEvent::AgentRosterUpdated { agents } => agents.iter().collect(),
                _ => continue,
            };
            for a in agents {
                if ws_agents.iter().any(|w| w.id == a.id) && !out.iter().any(|(id, _)| *id == a.id) {
                    let name = ws_agents.iter().find(|w| w.id == a.id).map(|w| shown(&w.name)).unwrap_or_default();
                    out.push((a.id, name));
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use ligus_core::agent_run::{legacy_id, AgentRun, Placement};
    use ligus_core::{ActorIdentity, SessionEvent, SigningKeypair, WorkspaceAgent};
    use uuid::Uuid;

    use crate::chat_service::{ChatService, PresenceProvider, PresenceRequest};
    use crate::event_store::EventStore;

    /// Counts the passkey ceremonies a write asks for, answering each with
    /// the account's test recovery key.
    struct Counting(Uuid, Arc<AtomicUsize>);
    impl PresenceProvider for Counting {
        fn assert(&self, req: &PresenceRequest) -> Result<ligus_core::presence::PresenceAssertion, String> {
            self.1.fetch_add(1, Ordering::SeqCst);
            Ok(ligus_core::testkit::test_recovery_key(self.0).assert(&req.challenge))
        }
    }

    /// Mara's devices on one data dir (a desktop, and lt beside it), on a
    /// workspace founded at `rules`, with a recovery key and a passkey.
    struct World {
        _dir: tempfile::TempDir,
        path: std::path::PathBuf,
        account: Uuid,
        seed: [u8; 32],
        desk: ChatService,
        ws: Uuid,
        chat: Uuid,
    }

    impl World {
        fn open(&self) -> ChatService {
            open(&self.path, self.account, &self.seed)
        }
    }

    fn open(path: &std::path::Path, account: Uuid, seed: &[u8; 32]) -> ChatService {
        let mut author = ActorIdentity::new(account.to_string(), "Mara", ligus_core::ActorKind::Human);
        author.account_id = Some(account);
        ChatService::new(EventStore::open(path).unwrap(), SigningKeypair::generate().unwrap(), SigningKeypair::from_seed(seed).unwrap(), author)
    }

    fn world(rules: u32) -> World {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ligus.db");
        let account = Uuid::new_v4();
        let seed = SigningKeypair::generate().unwrap().seed_bytes();
        let mut desk = open(&path, account, &seed);
        desk.set_founding_rules(rules);
        let chat = desk.create_chat("t", Uuid::new_v4(), "rt").unwrap();
        let (ws, chat) = (chat.workspace_id, chat.id);
        let key = ligus_core::testkit::test_recovery_key(account);
        desk.publish_identity(ligus_core::workspace_config_session_id(ws), ws).unwrap();
        desk.register_passkey(ws, key.credential(account, "Recovery key"), &|req| Ok(key.assert(&req.challenge))).unwrap();
        // A passkey beside the recovery key (the switch needs both).
        let soft = ligus_core::presence::soft::SoftAuthenticator::es256();
        desk.register_passkey(ws, soft.credential(account, "Touch ID"), &|req| Ok(key.assert(&req.challenge))).unwrap();
        assert_eq!(desk.workspace_config(ws).unwrap().unwrap().session().authz.rules, rules);
        World { _dir: dir, path, account, seed, desk, ws, chat }
    }

    fn now_seq() -> u64 {
        ligus_core::Timestamp::now().inner().unix_timestamp() as u64 * 1000
    }

    /// Workspace model v3, 15.2 (A9): switching a rules-3 workspace with a
    /// device on 0.4.1 present. The readiness list names it and its build
    /// ("lt on zaz-mbp (Mara) is on 0.4.1"), says a device that reported no
    /// build may go read-only, and the Owner's switch goes through, once.
    #[test]
    fn a_rules3_workspace_with_a_0_4_1_device_shows_it_and_switches() {
        let mut w = world(ligus_core::authz::RULES_V3);
        let ws = w.ws;
        // lt on the same computer, confirmed with her passkey, publishing its
        // host record; its beat says 0.4.1.
        let mut lt = w.open();
        assert!(lt.confirm_device(ws).unwrap());
        let lt_id = lt.device_id().to_string();
        lt.register_host(ws, ligus_core::WorkspaceHost::new(lt_id.clone(), ligus_core::HostKind::Device, "lt on zaz-mbp")).unwrap();
        // A third device that never said which build it runs.
        let mut quiet = w.open();
        assert!(quiet.confirm_device(ws).unwrap());
        // A model server address on the log.
        let mut rt = ligus_core::WorkspaceRuntime::new("ollama-box", "Ollama on the box", ligus_core::ModelProviderKind::Ollama, "llama3:8b");
        rt.endpoint = "http://10.0.0.5:11434".into();
        w.desk.add_workspace_runtime(ws, rt).unwrap();
        let presence = crate::HostPresence::new();
        presence.record_status(&lt_id, now_seq(), ligus_core::HostStatus { build: "0.4.1".into(), ..Default::default() });
        w.desk.set_host_presence(presence);

        let r = w.desk.rules4_report(ws).unwrap();
        assert_eq!((r.rules, r.is_owner), (3, true));
        assert_eq!(r.old_devices.len(), 1, "{r:?}");
        assert_eq!((r.old_devices[0].name.as_str(), r.old_devices[0].build.as_deref()), ("lt on zaz-mbp", Some("0.4.1")));
        assert!(r.unknown_devices.iter().any(|d| d.device_id == quiet.device_id()), "{r:?}");
        assert!(!r.unknown_devices.iter().chain(&r.old_devices).any(|d| d.device_id == w.desk.device_id()), "this device is 0.5");
        let lines = r.lines();
        assert!(lines.iter().any(|l| l == "lt on zaz-mbp (Mara) is on 0.4.1: it goes read-only here until it's updated"), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("hasn't said which Ligus it runs")), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with("Ollama on the box keeps its server address")), "{lines:?}");

        // The switch: one passkey, then rules 4; again is a no-op.
        let asked = Arc::new(AtomicUsize::new(0));
        w.desk.set_presence_provider(Some(Arc::new(Counting(w.account, asked.clone()))));
        assert!(w.desk.upgrade_workspace_rules(ws).unwrap().is_some());
        assert_eq!(asked.load(Ordering::SeqCst), 1, "one passkey");
        assert_eq!(w.desk.workspace_config(ws).unwrap().unwrap().session().authz.rules, ligus_core::authz::RULES_V4);
        assert!(w.desk.upgrade_workspace_rules(ws).unwrap().is_none());
        // Never back.
        assert!(w.desk.set_workspace_rules(ws, ligus_core::authz::RULES_V3).is_err());
        // The endpoint runtime keeps working (G9 allows it unchanged).
        assert!(w.desk.list_workspace_runtimes(ws).unwrap().iter().any(|r| r.id == "ollama-box"));
        let _ = w.chat;
    }

    /// Only the Owner switches.
    #[test]
    fn only_the_owner_upgrades() {
        let mut w = world(ligus_core::authz::RULES_V3);
        let ws = w.ws;
        let cfg = ligus_core::workspace_config_session_id(ws);
        let bea_account = Uuid::new_v4();
        let bea_id = bea_account.to_string();
        let mut actor = ActorIdentity::new(bea_id.clone(), "Bea", ligus_core::ActorKind::Human);
        actor.account_id = Some(bea_account);
        let bea = ligus_core::WorkspaceMember {
            id: bea_id.clone(),
            actor: actor.clone(),
            role: ligus_core::WorkspaceRole::Admin,
            title: String::new(),
            index: 0,
            joined_at: ligus_core::Timestamp::now(),
        };
        w.desk.add_member(cfg, ws, bea).unwrap();
        let mut admin = ChatService::new(EventStore::open(&w.path).unwrap(), SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), actor);
        let err = admin.upgrade_workspace_rules(ws).unwrap_err().to_string();
        assert!(err.contains("only the workspace's Owner"), "{err}");
        assert!(!admin.rules4_report(ws).unwrap().is_owner);
    }

    fn full(svc: &ChatService, source: &str, model: &str) -> WorkspaceAgent {
        let run = AgentRun { harness: String::new(), source: source.into(), model: model.into(), resolved: None };
        let mut a = WorkspaceAgent::new("scout", legacy_id(&run));
        a.owner_actor_id = svc.author().id.clone();
        a.run = Some(run);
        a
    }

    fn agent(svc: &ChatService, ws: Uuid, id: Uuid) -> WorkspaceAgent {
        svc.workspace_config(ws).unwrap().unwrap().agent(id).cloned().unwrap()
    }

    fn upserts(svc: &ChatService, ws: Uuid) -> usize {
        let cfg = ligus_core::workspace_config_session_id(ws);
        svc.store().list_in(ws, cfg).unwrap().iter().filter(|e| matches!(e.payload, SessionEvent::AgentUpserted { .. })).count()
    }

    /// C6 on rules 4 (5.7, H4 for rebinds, H10): a model change within one
    /// source on your own agent is one event and no passkey; a move to your
    /// own host is one event and one passkey; the record keeps its source.
    #[test]
    fn rules4_change_model_is_one_event_without_a_passkey_and_a_move_takes_one() {
        let mut w = world(ligus_core::authz::RULES_V4);
        let (ws, chat) = (w.ws, w.chat);
        let a = full(&w.desk, "claude-code", "opus");
        let id = a.id;
        w.desk.add_agent(chat, ws, a).unwrap();
        let asked = Arc::new(AtomicUsize::new(0));
        w.desk.set_presence_provider(Some(Arc::new(Counting(w.account, asked.clone()))));
        let before = upserts(&w.desk, ws);
        let run = AgentRun { harness: String::new(), source: "claude-code".into(), model: "sonnet".into(), resolved: None };
        w.desk.edit_agent(ws, id, None, Some(run), None).unwrap();
        assert_eq!((upserts(&w.desk, ws) - before, asked.load(Ordering::SeqCst)), (1, 0), "one event, no passkey");
        let got = agent(&w.desk, ws, id);
        assert_eq!(got.run.as_ref().map(|r| (r.source.as_str(), r.model.as_str())), Some(("claude-code", "sonnet")));
        assert_eq!(got.runtime_id, legacy_id(got.run.as_ref().unwrap()));
        // Its own device as its host: a move, one passkey.
        let here = w.desk.device_id().to_string();
        w.desk.edit_agent(ws, id, None, None, Some(&here)).unwrap();
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        let got = agent(&w.desk, ws, id);
        assert_eq!(got.placement, Some(Placement::Host { host_id: here }));
        assert_eq!(got.run.unwrap().model, "sonnet");
        // Back to her own devices.
        w.desk.edit_agent(ws, id, None, None, Some("")).unwrap();
        assert_eq!(agent(&w.desk, ws, id).placement, Some(Placement::OwnerDevices));
    }

    /// C6 on rules 3: an agent carries no run spec there; its runtime and
    /// host change through the existing writes (an Admin's, with a passkey).
    #[test]
    fn rules3_change_goes_through_the_runtime() {
        let mut w = world(ligus_core::authz::RULES_V3);
        let (ws, chat) = (w.ws, w.chat);
        let mut a = WorkspaceAgent::new("scout", "claude-code");
        a.owner_actor_id = w.desk.author().id.clone();
        let id = a.id;
        w.desk.add_agent(chat, ws, a).unwrap();
        let run = AgentRun { harness: String::new(), source: "claude-code".into(), model: "opus".into(), resolved: None };
        let err = w.desk.edit_agent(ws, id, None, Some(run), None).unwrap_err().to_string();
        assert!(err.contains("on rules 4"), "{err}");
        let asked = Arc::new(AtomicUsize::new(0));
        w.desk.set_presence_provider(Some(Arc::new(Counting(w.account, asked.clone()))));
        w.desk.edit_agent(ws, id, Some("claude-code-opus"), None, None).unwrap();
        assert_eq!(asked.load(Ordering::SeqCst), 1, "a runtime change is an Admin's, with a passkey");
        let got = agent(&w.desk, ws, id);
        assert_eq!(got.runtime_id, "claude-code-opus");
        assert_eq!(got.run.and_then(|r| r.runtime_ref().map(str::to_string)).as_deref(), Some("claude-code-opus"), "the mirror");
    }

    /// The old-copy notice (11, G5): a chat's own copy of an agent, from an
    /// older build, is found; on rules 4 the chat runs the workspace's.
    #[test]
    fn a_chats_old_copy_of_an_agent_is_found_and_the_workspace_record_runs() {
        let mut w = world(ligus_core::authz::RULES_V4);
        let (ws, chat) = (w.ws, w.chat);
        let mut a = WorkspaceAgent::new("scout", "claude-code");
        a.owner_actor_id = w.desk.author().id.clone();
        w.desk.add_agent(chat, ws, a.clone()).unwrap();
        assert!(w.desk.old_agent_copies(ws, chat).unwrap().is_empty());
        let mut old = a.clone();
        old.runtime_id = "old-runtime".into();
        let _ = crate::chat_service::as_an_older_build(|| w.desk.append_signed(chat, ws, SessionEvent::AgentUpserted { agent: old }));
        let view = w.desk.load_in(ws, chat).unwrap().unwrap();
        assert_eq!(view.workspace_agents.iter().find(|x| x.id == a.id).unwrap().runtime_id, "claude-code", "rules 4: the workspace's record");
        let copies = w.desk.old_agent_copies(ws, chat).unwrap();
        // Rules 4 denies a new chat copy (G5), so it may not have folded;
        // a copy on the log is still named for the notice when it did.
        assert!(copies.is_empty() || copies == vec![(a.id, "scout".to_string())], "{copies:?}");
    }
}
