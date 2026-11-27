//! The dispatch outcome (workspace model v3, 5.4): where a turn goes, seen
//! from one device, and the one quiet line a chat shows when it doesn't run.
//!
//! Every device computes the same route from the same inputs (the agent
//! record, the host records, the hosts' signed beats and the clock); only the
//! sender's device shows it, under the message (class L, nothing is written).
//! Nothing here runs a turn anywhere: a turn that can't run where it is
//! placed waits, or says why, until a person picks "Run here this time".
//!
//! - **Queued**: the agent's host is offline (or none of its owner's devices
//!   took the turn yet). It answers when the host is back.
//! - **Host offline / gone** ([`Why::HostGone`]): the host was removed,
//!   revoked or its owner left, or it has stayed offline past
//!   [`QUEUE_WINDOW`]. It never becomes "the owner's devices".
//! - **Can't run** ([`TurnRoute::Unrunnable`]): the host said it won't run
//!   the agent (its beat's refusals: a source it doesn't have, no key, a
//!   desktop that runs only its own person's agents), or this device would
//!   run it and can't (no key, an unknown runtime). Never a silent
//!   substitute.

use std::collections::HashSet;

use ligus_core::outcome::{GoneReason, TurnRoute, Why};
use ligus_core::{HostStatus, Timestamp, WorkspaceAgent, WorkspaceHost};
use uuid::Uuid;

/// How long a turn for an agent on an offline host reads as queued before
/// the chat says the host is offline ([`GoneReason::Offline`]). The turn
/// still answers when the host is back; only the line changes.
pub const QUEUE_WINDOW_SECS: i64 = 120;
/// [`QUEUE_WINDOW_SECS`] as a duration.
pub const QUEUE_WINDOW: std::time::Duration = std::time::Duration::from_secs(QUEUE_WINDOW_SECS as u64);

/// How long an unclaimed turn for an agent of the owner's devices (or the
/// primary of someone else's chat) waits before its row shows: a live
/// device claims well within it, so a healthy room never flashes a row.
pub const OWNER_GRACE_SECS: i64 = ligus_core::dispatch::CLAIM_TIMEOUT.as_secs() as i64;

/// What this device knows about running the turn itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Here {
    /// Both dispatch gates say this device runs it (its placement and owner
    /// put it here, and the host policy allows it).
    pub eligible: bool,
    /// Whether this device could run the agent's own run spec: `Ok`, or why
    /// not (no key, a runtime it doesn't have). Only read when `eligible`,
    /// and for "Run here this time".
    pub resolves: Result<(), Why>,
}

/// The hosts as one device sees them.
pub struct HostsView<'a> {
    /// Every host record (former members' and revoked devices' too), so a
    /// gone host can say what happened.
    pub all: &'a [WorkspaceHost],
    /// Revoked device ids.
    pub revoked: &'a HashSet<Uuid>,
    /// Current member ids.
    pub members: &'a HashSet<String>,
    pub online: &'a dyn Fn(&WorkspaceHost) -> bool,
    /// A host's last signed status, by host id.
    pub status: &'a dyn Fn(&str) -> Option<HostStatus>,
}

/// A workspace's host records, revoked devices and members, read once for
/// many routes ([`HostsView`]).
#[derive(Debug, Clone, Default)]
pub struct HostFacts {
    pub all: Vec<WorkspaceHost>,
    pub revoked: HashSet<Uuid>,
    pub members: HashSet<String>,
}

impl HostFacts {
    /// From `workspace`'s config log as `svc` folds it.
    pub fn load(svc: &crate::ChatService, workspace: Uuid) -> Self {
        let cfg = svc.load_in(workspace, ligus_core::workspace_config_session_id(workspace)).ok().flatten();
        match cfg {
            Some(s) => Self {
                all: s.workspace_hosts.clone(),
                revoked: s.guards.revoked_devices.iter().copied().collect(),
                members: s.members.iter().map(|m| m.id.clone()).collect(),
            },
            None => Self::default(),
        }
    }

    /// The view [`route`] reads, with liveness and status from `presence`.
    pub fn view<'a>(&'a self, online: &'a dyn Fn(&WorkspaceHost) -> bool, status: &'a dyn Fn(&str) -> Option<HostStatus>) -> HostsView<'a> {
        HostsView { all: &self.all, revoked: &self.revoked, members: &self.members, online, status }
    }

    /// A host's label (its id's first 8 characters when it has none).
    pub fn label(&self, host: &str) -> String {
        self.all
            .iter()
            .find(|h| h.id == host)
            .map(|h| ligus_proto::names::clean_display_name(&h.label))
            .filter(|l| !l.trim().is_empty())
            .unwrap_or_else(|| format!("host {}", host.get(..8).unwrap_or(host)))
    }
}

/// Where the turn of `agent` (`None`: the chat's primary) started by a
/// message at `trigger_at` goes, at `now`.
pub fn route(agent: Option<&WorkspaceAgent>, here: &Here, hosts: &HostsView<'_>, trigger_at: Timestamp, now: Timestamp) -> TurnRoute {
    if here.eligible {
        return match &here.resolves {
            Ok(()) => TurnRoute::RunsHere,
            Err(why) => TurnRoute::Unrunnable(why.clone()),
        };
    }
    let waited = now.inner().unix_timestamp() - trigger_at.inner().unix_timestamp();
    let Some(agent) = agent else {
        // The primary answers on its chat's creator's devices.
        return TurnRoute::Queued { on: Vec::new(), since: trigger_at };
    };
    let judged = agent.as_judged();
    let host_id = judged.host_id.as_str();
    if host_id.is_empty() {
        // The owner's own devices; never anyone else's.
        return TurnRoute::Queued { on: Vec::new(), since: trigger_at };
    }
    let gone = |reason| TurnRoute::Unrunnable(Why::HostGone { host: host_id.to_string(), reason });
    let Some(host) = hosts.all.iter().find(|h| h.id == host_id) else {
        return gone(if agent.is_host_gone() { GoneReason::Left } else { GoneReason::Removed });
    };
    if Uuid::parse_str(host_id).is_ok_and(|d| hosts.revoked.contains(&d)) {
        return gone(GoneReason::Revoked);
    }
    if agent.is_host_gone() || (!host.owner_actor_id.is_empty() && !hosts.members.contains(&host.owner_actor_id)) {
        return gone(GoneReason::Left);
    }
    // The host's own word about this agent, from its last signed beat.
    if let Some(refusal) = (hosts.status)(host_id).and_then(|st| st.refusal_of(agent.id).cloned()) {
        return TurnRoute::Unrunnable(refusal.as_why(host_id, &agent.runtime_id));
    }
    if (hosts.online)(host) {
        return TurnRoute::RunsOn { host: host_id.to_string() };
    }
    if waited < QUEUE_WINDOW_SECS {
        TurnRoute::Queued { on: vec![host_id.to_string()], since: trigger_at }
    } else {
        gone(GoneReason::Offline)
    }
}

/// Whether a route's row shows yet: a queued turn on the owner's devices
/// (or the primary) only once it has waited past [`OWNER_GRACE_SECS`], so a
/// live device's claim lands first; every other non-running route at once.
pub fn row_due(route: &TurnRoute, trigger_at: Timestamp, now: Timestamp) -> bool {
    match route {
        TurnRoute::RunsHere | TurnRoute::RunsOn { .. } => false,
        TurnRoute::Queued { on, .. } if on.is_empty() => {
            now.inner().unix_timestamp() - trigger_at.inner().unix_timestamp() >= OWNER_GRACE_SECS
        }
        _ => true,
    }
}

/// The names a line uses.
pub struct Names<'a> {
    /// The agent's name without `@` ("Ligus" for the primary).
    pub agent: &'a str,
    /// The agent's owner (or the primary's chat creator), for "Ana's devices".
    pub owner: &'a str,
    /// A host's label by id.
    pub host: &'a dyn Fn(&str) -> String,
}

/// The reason code a reader keys on (`offline`, `removed`, ...,
/// `waiting` for a plain queue).
pub fn reason_code(route: &TurnRoute) -> &'static str {
    match route {
        TurnRoute::Queued { .. } => "waiting",
        TurnRoute::Unrunnable(Why::HostGone { reason, .. }) => reason.code(),
        TurnRoute::Unrunnable(w) => w.code(),
        TurnRoute::RunsHere | TurnRoute::RunsOn { .. } => "",
    }
}

/// The one line a chat shows for `route` (empty when it runs).
pub fn line(route: &TurnRoute, n: &Names<'_>) -> String {
    let who = if n.agent.eq_ignore_ascii_case("ligus") { "Ligus".to_string() } else { format!("@{}", n.agent.trim_start_matches('@')) };
    let owner = if n.owner.trim().is_empty() { "its owner".to_string() } else { format!("{}'s", n.owner.trim()) };
    let owners_devices = if n.owner.trim().is_empty() { "its owner's devices".to_string() } else { format!("{owner} devices") };
    match route {
        TurnRoute::RunsHere | TurnRoute::RunsOn { .. } => String::new(),
        TurnRoute::Queued { on, .. } => match on.first() {
            Some(h) => {
                let host = (n.host)(h);
                format!("{who} runs on {host}, which is offline. It will answer when {host} is back.")
            }
            None => format!("{who} answers from {owners_devices}. It will answer when one of them is online."),
        },
        TurnRoute::Unrunnable(why) => match why {
            Why::HostGone { host, reason } => {
                let host = (n.host)(host);
                match reason {
                    GoneReason::Offline => format!("{host} has been offline for a while. {who} will answer when it's back."),
                    GoneReason::Removed => format!("{host} was removed from the workspace. Choose where {who} runs."),
                    GoneReason::Revoked => format!("{host} was signed out of the workspace. Choose where {who} runs."),
                    GoneReason::Left => format!("{host}'s owner left the workspace. Choose where {who} runs."),
                }
            }
            Why::SourceNotOffered { host, source, detail } => {
                let host = (n.host)(host);
                let source = ligus_proto::names::clean_display_name(source);
                match detail.trim() {
                    "" => format!("{who} can't run on {host}: it doesn't have '{source}'."),
                    d => format!("{who} can't run on {host}: {}.", d.trim_end_matches('.')),
                }
            }
            Why::ModelNotOffered { host, model } => format!("{who} can't run on {}: it doesn't have the model '{model}'.", (n.host)(host)),
            Why::NotAllowedOnHost { host, detail } => {
                let host = (n.host)(host);
                match detail.trim() {
                    "" => format!("{who} can't run on {host}: it runs only its own person's agents."),
                    d => format!("{who} can't run on {host}: {}.", d.trim_end_matches('.')),
                }
            }
            Why::OwnerGone => format!("{who}'s owner left the workspace."),
            Why::NoKeyHere { source } => {
                let source = ligus_proto::names::clean_display_name(source);
                format!("{who} can't run here: this device has no key for '{source}'.")
            }
        },
    }
}

/// The route of a person's message that hasn't been taken yet: its
/// trigger's agent (`None`: the primary) and the route, or `None` when the
/// message starts no turn, was answered, is held by a device, or its row
/// isn't due yet ([`row_due`]). `here` says what this device knows about
/// running the turn of an agent (`None`: the primary).
pub struct PendingRoute {
    pub agent: Option<WorkspaceAgent>,
    pub route: TurnRoute,
}

/// [`PendingRoute`] for `trigger` in `session`.
pub fn pending_route(
    session: &ligus_core::ChatSession,
    trigger: &ligus_core::ChatMessage,
    links: &ligus_core::DeviceLinks,
    local_only: bool,
    here: &dyn Fn(Option<&WorkspaceAgent>) -> Here,
    hosts: &HostsView<'_>,
    now: Timestamp,
) -> Option<PendingRoute> {
    if trigger.role != ligus_core::MessageRole::User || trigger.superseded {
        return None;
    }
    if crate::turn_dispatch::user_turn_answered(session, trigger.id) {
        return None;
    }
    let agent = match crate::turn_dispatch::dispatch_target(&trigger.body, session, local_only, links)? {
        crate::turn_dispatch::DispatchTarget::Agent(id) => Some(session.workspace_agents.iter().find(|a| a.id == id)?.clone()),
        crate::turn_dispatch::DispatchTarget::Primary => None,
    };
    if session.turn(trigger.id, None).is_some_and(|t| t.state != ligus_core::turn::TurnState::Queued) {
        return None;
    }
    let at = trigger.appended_at();
    let route = route(agent.as_ref(), &here(agent.as_ref()), hosts, at, now);
    row_due(&route, at, now).then_some(PendingRoute { agent, route })
}

/// Whether "Run here this time" may run the turn of `agent` on this device
/// (workspace model v3, 5.3, D3). Under rules 3 only the agent's owner's own
/// devices may answer for it, so: an agent (never the primary), owned by
/// `my_person` (through device links), whose route doesn't already run it,
/// and that this device can run with the agent's own run spec. The turn is
/// still claimed through the dispatcher, so it runs once.
pub fn run_here_allowed(
    agent: Option<&WorkspaceAgent>,
    route: &TurnRoute,
    my_person: &str,
    links: &ligus_core::DeviceLinks,
    resolves: &Result<(), Why>,
) -> Result<(), String> {
    let Some(a) = agent else {
        return Err("Ligus answers on the chat's creator's devices".into());
    };
    if a.owner_actor_id.is_empty() || links.person(&a.owner_actor_id) != my_person {
        return Err(format!("@{} belongs to someone else; only their own devices run it", a.name));
    }
    match route {
        TurnRoute::RunsHere => return Err(format!("@{} already runs here", a.name)),
        TurnRoute::RunsOn { .. } => return Err(format!("@{}'s host is online and answers it", a.name)),
        TurnRoute::Unrunnable(Why::NoKeyHere { .. }) => return Err(format!("this device has no key for @{}'s runtime", a.name)),
        _ => {}
    }
    match resolves {
        Ok(()) => Ok(()),
        Err(Why::NoKeyHere { source }) => Err(format!("this device has no key for '{}'", ligus_proto::names::clean_display_name(source))),
        Err(_) => Err(format!("this device doesn't have @{}'s runtime '{}'", a.name, ligus_proto::names::clean_display_name(&a.runtime_id))),
    }
}

/// The row a reader shows for `route` (see [`ligus_proto::TurnRouteDto`]).
pub fn route_dto(route: &TurnRoute, names: &Names<'_>, run_here: bool, use_claude_code: bool) -> Option<ligus_proto::TurnRouteDto> {
    let state = route.state()?;
    let since = match route {
        TurnRoute::Queued { since, .. } => Some(since.inner().format(&time::format_description::well_known::Rfc3339).unwrap_or_default()),
        _ => None,
    };
    Some(ligus_proto::TurnRouteDto {
        state: state.to_string(),
        reason: reason_code(route).to_string(),
        text: line(route, names),
        host: route.host().map(|h| (names.host)(h)),
        since,
        run_here,
        use_claude_code,
    })
}

/// The outcome a reader shows for a finished reply, with its host by label.
pub fn outcome_dto(m: &ligus_core::ChatMessage, host_label: &dyn Fn(&str) -> String) -> Option<ligus_proto::ReplyOutcomeDto> {
    if m.is_streaming {
        return None;
    }
    let o = m.outcome.as_ref()?;
    let status = match o.status {
        ligus_core::outcome::ReplyStatus::Failed => "failed",
        ligus_core::outcome::ReplyStatus::Canceled => "canceled",
        _ => "ok",
    };
    let some = |s: &str| (!s.trim().is_empty()).then(|| s.to_string());
    Some(ligus_proto::ReplyOutcomeDto {
        status: status.to_string(),
        harness: some(&o.harness),
        model: some(&o.model),
        host: some(&o.host).map(|h| host_label(&h)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ligus_core::outcome::{AgentRefusal, RefusalKind};
    use ligus_core::{HostKind, HostStatus};

    fn host(id: &str, owner: &str) -> WorkspaceHost {
        let mut h = WorkspaceHost::new(id.to_string(), HostKind::Device, "build-1".to_string());
        h.owner_actor_id = owner.to_string();
        h
    }

    struct World {
        hosts: Vec<WorkspaceHost>,
        revoked: HashSet<Uuid>,
        members: HashSet<String>,
        online: bool,
        status: Option<HostStatus>,
    }

    impl World {
        fn new(host_id: &str) -> Self {
            Self {
                hosts: vec![host(host_id, "ana")],
                revoked: HashSet::new(),
                members: ["ana".to_string(), "ben".to_string()].into_iter().collect(),
                online: false,
                status: None,
            }
        }

        fn route(&self, agent: &WorkspaceAgent, here: &Here, waited: i64) -> TurnRoute {
            let online = |_: &WorkspaceHost| self.online;
            let status = |_: &str| self.status.clone();
            let view = HostsView { all: &self.hosts, revoked: &self.revoked, members: &self.members, online: &online, status: &status };
            let t = Timestamp::from_unix_seconds(1_800_000_000);
            route(Some(agent), here, &view, t, Timestamp::from_unix_seconds(1_800_000_000 + waited))
        }
    }

    fn away() -> Here {
        Here { eligible: false, resolves: Ok(()) }
    }

    fn agent_on(host_id: &str) -> WorkspaceAgent {
        let mut a = WorkspaceAgent::new("coder", "codex-pro");
        a.owner_actor_id = "ana".into();
        a.place(host_id);
        a
    }

    /// H3: an agent on an offline host is queued, then the host reads as
    /// offline past the window; it never runs here.
    #[test]
    fn an_offline_host_queues_then_reads_offline_and_never_runs_here() {
        let h = Uuid::new_v4().to_string();
        let w = World::new(&h);
        let a = agent_on(&h);
        assert_eq!(w.route(&a, &away(), 5), TurnRoute::Queued { on: vec![h.clone()], since: Timestamp::from_unix_seconds(1_800_000_000) });
        assert_eq!(w.route(&a, &away(), QUEUE_WINDOW_SECS + 1), TurnRoute::Unrunnable(Why::HostGone { host: h.clone(), reason: GoneReason::Offline }));
        let mut on = World::new(&h);
        on.online = true;
        assert_eq!(on.route(&a, &away(), 5), TurnRoute::RunsOn { host: h });
    }

    /// H13: a removed, revoked or departed host is gone, at once, and its
    /// agent never falls back to the owner's devices.
    #[test]
    fn a_gone_host_is_a_state_not_a_fallback() {
        let h = Uuid::new_v4();
        let a = agent_on(&h.to_string());
        let mut removed = World::new(&h.to_string());
        removed.hosts.clear();
        assert!(matches!(removed.route(&a, &away(), 0), TurnRoute::Unrunnable(Why::HostGone { reason: GoneReason::Removed, .. })));
        let mut revoked = World::new(&h.to_string());
        revoked.revoked.insert(h);
        assert!(matches!(revoked.route(&a, &away(), 0), TurnRoute::Unrunnable(Why::HostGone { reason: GoneReason::Revoked, .. })));
        let mut left = World::new(&h.to_string());
        left.members.remove("ana");
        assert!(matches!(left.route(&a, &away(), 0), TurnRoute::Unrunnable(Why::HostGone { reason: GoneReason::Left, .. })));
        // The fold's host_gone (rules 3 cleared host_id): still that host.
        let mut cleared = a.clone();
        cleared.host_id.clear();
        cleared.host_gone = true;
        assert!(matches!(left.route(&cleared, &away(), 0), TurnRoute::Unrunnable(Why::HostGone { reason: GoneReason::Left, .. })));
    }

    /// H2 and H16: the host's refusal (a source it can't resolve, no key, a
    /// desktop's policy) is the reason, even while it is online.
    #[test]
    fn a_hosts_refusal_is_the_reason() {
        let h = Uuid::new_v4().to_string();
        let a = agent_on(&h);
        for (kind, code) in [(RefusalKind::SourceNotOffered, "sourceNotOffered"), (RefusalKind::NoKey, "sourceNotOffered"), (RefusalKind::NotAllowed, "notAllowedOnHost")] {
            let mut w = World::new(&h);
            w.online = true;
            w.status = Some(HostStatus { refused_agents: vec![AgentRefusal { agent_id: a.id, kind, why: "no runtime 'codex-pro' here".into() }], ..Default::default() });
            match w.route(&a, &away(), 0) {
                TurnRoute::Unrunnable(why) => assert_eq!(why.code(), code),
                other => panic!("{other:?}"),
            }
        }
    }

    /// H2: this device would run it and can't: unrunnable with its reason,
    /// never a substitute.
    #[test]
    fn an_eligible_device_that_cant_resolve_says_why() {
        let w = World::new("h");
        let mut a = agent_on("");
        a.place("");
        let here = Here { eligible: true, resolves: Err(Why::NoKeyHere { source: "anthropic".into() }) };
        assert_eq!(w.route(&a, &here, 0), TurnRoute::Unrunnable(Why::NoKeyHere { source: "anthropic".into() }));
        assert_eq!(w.route(&a, &Here { eligible: true, resolves: Ok(()) }, 0), TurnRoute::RunsHere);
        // Another person's agent on their own devices: queued, never here.
        assert!(matches!(w.route(&a, &away(), 0), TurnRoute::Queued { on, .. } if on.is_empty()));
    }

    #[test]
    fn lines_are_one_calm_sentence() {
        let names = Names { agent: "coder", owner: "Ana", host: &|_| "build-1".to_string() };
        let q = TurnRoute::Queued { on: vec!["h".into()], since: Timestamp::epoch() };
        assert_eq!(line(&q, &names), "@coder runs on build-1, which is offline. It will answer when build-1 is back.");
        let owner = TurnRoute::Queued { on: vec![], since: Timestamp::epoch() };
        assert_eq!(line(&owner, &names), "@coder answers from Ana's devices. It will answer when one of them is online.");
        let gone = TurnRoute::Unrunnable(Why::HostGone { host: "h".into(), reason: GoneReason::Removed });
        assert_eq!(line(&gone, &names), "build-1 was removed from the workspace. Choose where @coder runs.");
        let src = TurnRoute::Unrunnable(Why::SourceNotOffered { host: "h".into(), source: "codex-pro".into(), detail: String::new() });
        assert_eq!(line(&src, &names), "@coder can't run on build-1: it doesn't have 'codex-pro'.");
        assert_eq!(line(&TurnRoute::RunsHere, &names), "");
        assert!(!row_due(&owner, Timestamp::from_unix_seconds(100), Timestamp::from_unix_seconds(101)));
        assert!(row_due(&owner, Timestamp::from_unix_seconds(100), Timestamp::from_unix_seconds(100 + OWNER_GRACE_SECS)));
        assert!(row_due(&q, Timestamp::from_unix_seconds(100), Timestamp::from_unix_seconds(100)));
    }
}
