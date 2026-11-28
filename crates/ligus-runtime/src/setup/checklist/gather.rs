//! The checklist's [`Facts`] from what this device already holds: the
//! workspace's fold, its trust envelopes, this store and what the surface
//! knows about hosts (beats). Cheap and local: no network.

use std::collections::{BTreeMap, HashSet};

use ligus_core::{workspace_config_session_id, HostKind, HostStatus, Timestamp, WorkspaceHost, WorkspaceRole};
use uuid::Uuid;

use super::{AgentItem, BoxItem, DeviceItem, Facts, Leftover, LeftoverKind, PhoneItem};
use crate::device_lifecycle::{state_of, DeviceFacts, DeviceState};
use crate::ChatService;

/// Whose checklist it is: this device's author (desktop, CLI, lt), or the
/// person behind a phone (a box answering `GET /v1/setup`).
#[derive(Debug, Clone)]
pub struct Viewer {
    /// The person's member (actor) id.
    pub actor_id: String,
    pub account: Option<Uuid>,
    /// The device the checklist is for ("This device").
    pub device: Uuid,
}

impl Viewer {
    /// This device's author, on this device.
    pub fn author(svc: &ChatService) -> Self {
        Self { actor_id: svc.author().id.clone(), account: svc.author().account_id, device: svc.device_id() }
    }
}

/// What the surface knows that the fold doesn't.
pub struct Live<'a> {
    /// A host is online now (the desktop's beats; elsewhere its record's
    /// `last_seen` within five minutes).
    pub online: &'a dyn Fn(&WorkspaceHost) -> bool,
    /// A host's last signed status, by host id (its build, its refusals).
    pub status: &'a dyn Fn(&str) -> Option<HostStatus>,
    /// The newest release this surface knows, for a box's "(0.5.0 out)".
    pub latest_release: Option<String>,
    /// How this device is called ("this Mac", "this terminal", "this box").
    pub this_device_word: String,
    /// The workspace's name as this surface shows it (empty: the fold's).
    pub workspace_name: String,
    /// Push per phone device, when this surface knows it (a box: its own
    /// pairings).
    pub push: &'a dyn Fn(Uuid) -> Option<bool>,
    /// Leftover keys this device removed from view.
    pub hidden: Vec<String>,
    pub now: Timestamp,
}

impl Live<'_> {
    /// Online when the record's own `last_seen` is under five minutes old.
    pub fn record_online(h: &WorkspaceHost) -> bool {
        (Timestamp::now().inner() - h.last_seen.inner()).whole_seconds() < 300
    }
}

fn no_status(_: &str) -> Option<HostStatus> {
    None
}

fn no_push(_: Uuid) -> Option<bool> {
    None
}

impl Default for Live<'static> {
    fn default() -> Self {
        Live {
            online: &Live::record_online,
            status: &no_status,
            latest_release: None,
            this_device_word: String::new(),
            workspace_name: String::new(),
            push: &no_push,
            hidden: Vec::new(),
            now: Timestamp::now(),
        }
    }
}

/// "0.5.0" when `latest` is a strictly newer plain release than `build`
/// (both `MAJOR.MINOR.PATCH`, a leading `v` allowed), else empty: a
/// pre-release, a dev build or anything odd says nothing.
pub fn upgrade_to(build: &str, latest: Option<&str>) -> String {
    fn plain(v: &str) -> Option<(u64, u64, u64)> {
        let core = v.trim().trim_start_matches(['v', 'V']);
        let parts: Vec<&str> = core.split('.').collect();
        if parts.len() != 3 || parts.iter().any(|p| p.is_empty() || p.len() > 9 || !p.bytes().all(|b| b.is_ascii_digit())) {
            return None;
        }
        Some((parts[0].parse().ok()?, parts[1].parse().ok()?, parts[2].parse().ok()?))
    }
    match (plain(build), latest.and_then(plain)) {
        (Some(have), Some(next)) if next > have => format!("{}.{}.{}", next.0, next.1, next.2),
        _ => String::new(),
    }
}

fn shown(s: &str) -> String {
    ligus_proto::names::clean_display_name(s)
}

fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// "24 Nov".
fn day(t: Timestamp) -> String {
    let d = t.inner().date();
    let month = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][usize::from(u8::from(d.month())) - 1];
    format!("{} {month}", d.day())
}

/// The checklist's facts for `viewer` in workspace `ws`.
pub fn gather(svc: &ChatService, ws: Uuid, viewer: &Viewer, live: &Live<'_>) -> Facts {
    let cfg_id = workspace_config_session_id(ws);
    let config = svc.load_in(ws, cfg_id).ok().flatten();
    let roster = svc.store().load_config(ws, None, None).ok().map(|(_, t)| t.current().clone());
    let mut f = Facts {
        this_device_word: live.this_device_word.clone(),
        rules_supported: ligus_core::authz::RULES_SUPPORTED,
        hidden: live.hidden.clone(),
        ..Default::default()
    };
    let Some(config) = config else {
        f.workspace = live.workspace_name.clone();
        return f;
    };
    f.workspace = if live.workspace_name.trim().is_empty() { shown(&config.title) } else { live.workspace_name.clone() };
    f.rules = if config.authz.rules > 0 { config.authz.rules } else { roster.as_ref().map(|r| r.rules).unwrap_or(ligus_core::authz::RULES_GENESIS) };
    let member = config.members.iter().find(|m| m.id == viewer.actor_id);
    let role = member.map(|m| m.role);
    f.role = role.map(|r| crate::setup::role_name(r).to_string()).unwrap_or_default();
    let admin = matches!(role, Some(WorkspaceRole::Owner | WorkspaceRole::Admin));

    // You: the profile (the member's own name when no profile was written).
    if let Some(account) = viewer.account {
        let profile = config.profiles.get(&account);
        f.name = profile.and_then(|p| p.display_name.clone()).map(|n| shown(&n)).filter(|n| !n.trim().is_empty()).unwrap_or_default();
        f.picture = profile.is_some_and(|p| p.avatar.is_some() && !p.avatar_cleared);
        let keys = svc.passkeys_of(ws, account).unwrap_or_default();
        f.passkeys = keys.iter().filter(|(_, recovery)| !recovery).count() as u32;
        f.recovery_key = keys.iter().any(|(_, recovery)| *recovery);
    }
    if f.name.is_empty() {
        f.name = member.map(|m| shown(&m.actor.display_name)).filter(|n| !n.trim().is_empty()).unwrap_or_default();
    }

    // Devices: this one's synced name, then the others of the account.
    let labels = |d: Uuid| -> Option<String> {
        let account = viewer.account?;
        config.device_labels.get(&d).and_then(|m| m.get(&account)).map(|l| shown(l)).filter(|l| !l.trim().is_empty())
    };
    let hosts: Vec<WorkspaceHost> = config.workspace_hosts.clone();
    let host_label = |d: Uuid| hosts.iter().find(|h| h.id == d.to_string()).map(|h| shown(&h.label)).filter(|l| !l.trim().is_empty());
    f.this_device = labels(viewer.device).or_else(|| host_label(viewer.device)).unwrap_or_default();
    let phone_ids: HashSet<Uuid> = roster.as_ref().map(|r| r.devices.iter().filter(|(_, d)| d.phone).map(|(id, _)| *id).collect()).unwrap_or_default();
    let mine = match viewer.account {
        Some(a) => svc.store().trust_envelopes(ws).map(|t| crate::device_enroll::devices_of(&t, a)).unwrap_or_default(),
        None => Vec::new(),
    };
    let author_is_viewer = svc.author().account_id.is_some() && svc.author().account_id == viewer.account;
    for d in &mine {
        if d.device_id == viewer.device {
            continue;
        }
        let phone = phone_ids.contains(&d.device_id);
        // Only this device's author can tell "approved, never used" (its own
        // account's certificates); a box answering for a phone leaves it out.
        let never_written = !phone && !d.revoked && author_is_viewer && svc.never_written(ws, d.device_id).ok().flatten().is_some();
        let host = hosts.iter().find(|h| h.id == d.device_id.to_string());
        let facts = DeviceFacts {
            revoked: d.revoked,
            never_written,
            this_device: false,
            last_signed: svc.store().newest_signed_at(ws, d.device_id).ok().flatten(),
            last_beat: host.map(|h| if (live.online)(h) { live.now } else { h.last_seen }),
            certified_at: d.certified_at,
        };
        let state = state_of(&facts, live.now);
        let label = labels(d.device_id).or_else(|| host_label(d.device_id)).unwrap_or_else(|| {
            let kind = if phone { "Phone" } else { "Device" };
            format!("{kind} {}", short(&d.device_id.to_string()))
        });
        if phone {
            f.phones.push(PhoneItem { id: d.device_id.to_string(), label: label.clone(), state: state.as_str().into(), push: (live.push)(d.device_id) });
        } else {
            f.devices.push(DeviceItem { id: d.device_id.to_string(), label: label.clone(), state: state.as_str().into() });
        }
        let (kind, detail) = match state {
            DeviceState::Pending => (Some(LeftoverKind::DevicePending), "approved, never used".to_string()),
            DeviceState::Idle => (Some(LeftoverKind::DeviceIdle), "no activity for 30 days".to_string()),
            DeviceState::Revoked => (Some(LeftoverKind::DeviceRevoked), "revoked".to_string()),
            DeviceState::Active => (None, String::new()),
        };
        // An idle phone stays a phone (a box's pairing is its beat, which
        // only the box and the desktop's page see): only computers here.
        if let Some(kind) = kind.filter(|k| !(phone && *k == LeftoverKind::DeviceIdle)) {
            f.leftovers.push(Leftover { kind, id: d.device_id.to_string(), label, detail, can_remove: kind != LeftoverKind::DeviceRevoked });
        }
    }

    // Members who left or were retired.
    let names = |id: &str| -> String {
        config
            .members
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.actor.account_id.map(|a| crate::profiles::name_of(&config, a, &m.actor.display_name)).unwrap_or_else(|| m.actor.display_name.clone()))
            .map(|n| shown(&n))
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| format!("member {}", short(id)))
    };
    for (id, left) in &config.left {
        if *id == viewer.actor_id || !config.members.iter().any(|m| &m.id == id) {
            continue;
        }
        f.leftovers.push(Leftover { kind: LeftoverKind::MemberLeft, id: id.clone(), label: names(id), detail: format!("asked to leave {}", day(left.at)), can_remove: admin });
    }
    for id in config.retired.keys() {
        if config.members.iter().any(|m| &m.id == id) {
            f.leftovers.push(Leftover { kind: LeftoverKind::MemberRetired, id: id.clone(), label: names(id), detail: "retired into their person".into(), can_remove: false });
        }
    }

    // Hosts: boxes and workers for the Box row; records of former members,
    // revoked devices and boxes that left for Clean up.
    let members: HashSet<&str> = config.members.iter().map(|m| m.id.as_str()).collect();
    let revoked: HashSet<Uuid> = config.guards.revoked_devices.iter().copied().collect();
    let links = svc.device_links(ws).unwrap_or_default();
    let person_of = |actor: &str| links.person(actor).to_string();
    for h in &hosts {
        let gone_owner = !h.owner_actor_id.is_empty() && !members.contains(h.owner_actor_id.as_str()) && !members.contains(person_of(&h.owner_actor_id).as_str());
        let gone_device = Uuid::parse_str(&h.id).is_ok_and(|d| revoked.contains(&d));
        let left_box = config.left.contains_key(&h.owner_actor_id);
        let label = if h.label.trim().is_empty() { format!("host {}", short(&h.id)) } else { shown(&h.label) };
        if gone_owner || gone_device {
            let why = if gone_device { "its device was revoked" } else { "its owner isn't a member" };
            f.leftovers.push(Leftover { kind: LeftoverKind::FormerHost, id: h.id.clone(), label, detail: why.into(), can_remove: admin });
            continue;
        }
        if h.kind == HostKind::Worker && !left_box {
            let build = (live.status)(&h.id).map(|s| s.build).unwrap_or_default();
            f.boxes.push(BoxItem {
                id: h.id.clone(),
                label,
                online: (live.online)(h),
                upgrade_to: upgrade_to(&build, live.latest_release.as_deref()),
                build,
            });
        }
    }

    // Agents and where they answer; old names for Clean up.
    let route_facts = crate::turn_route::HostFacts::load(svc, ws);
    let online = |h: &WorkspaceHost| (live.online)(h);
    let status = |id: &str| (live.status)(id);
    let view = route_facts.view(&online, &status);
    let owner_names: BTreeMap<&str, String> = config.members.iter().map(|m| (m.id.as_str(), names(&m.id))).collect();
    for a in &config.workspace_agents {
        if ligus_core::agent::names_primary(&a.name) || a.name.trim().eq_ignore_ascii_case(ligus_core::agent::LEGACY_PRIMARY_HANDLE) {
            let own = !a.owner_actor_id.is_empty() && (a.owner_actor_id == viewer.actor_id || person_of(&a.owner_actor_id) == viewer.actor_id);
            f.leftovers.push(Leftover {
                kind: LeftoverKind::LegacyAgent,
                id: a.id.to_string(),
                label: shown(a.name.trim()),
                detail: "an old name: @ligus is the default agent".into(),
                can_remove: admin || own,
            });
            continue;
        }
        let judged = a.as_judged();
        let host_id = judged.host_id.clone();
        let (runs_on, answers, why) = if host_id.is_empty() {
            let owner = person_of(&a.owner_actor_id);
            let mine = !a.owner_actor_id.is_empty() && (a.owner_actor_id == viewer.actor_id || owner == viewer.actor_id);
            if mine {
                ("your devices".to_string(), true, String::new())
            } else if a.owner_actor_id.is_empty() || members.contains(a.owner_actor_id.as_str()) || members.contains(owner.as_str()) {
                let who = owner_names.get(a.owner_actor_id.as_str()).or_else(|| owner_names.get(owner.as_str())).cloned().unwrap_or_else(|| "its owner".into());
                (format!("{who}'s devices"), true, String::new())
            } else {
                ("its owner's devices".to_string(), false, "owner left".to_string())
            }
        } else {
            let label = if host_id == viewer.device.to_string() && !live.this_device_word.is_empty() { live.this_device_word.clone() } else { route_facts.label(&host_id) };
            let here = crate::turn_route::Here { eligible: host_id == viewer.device.to_string(), resolves: Ok(()) };
            match crate::turn_route::route(Some(a), &here, &view, live.now, live.now) {
                ligus_core::outcome::TurnRoute::RunsHere | ligus_core::outcome::TurnRoute::RunsOn { .. } => (label, true, String::new()),
                ligus_core::outcome::TurnRoute::Queued { .. } => (label, false, "offline".into()),
                ligus_core::outcome::TurnRoute::Unrunnable(why) => (label, false, why_words(&why)),
            }
        };
        f.agents.push(AgentItem { name: shown(a.name.trim()), runs_on, answers, why });
    }
    f
}

/// A few words for why an agent won't answer.
fn why_words(why: &ligus_core::outcome::Why) -> String {
    use ligus_core::outcome::{GoneReason, Why};
    match why {
        Why::HostGone { reason: GoneReason::Offline, .. } => "offline".into(),
        Why::HostGone { reason: GoneReason::Revoked, .. } => "host revoked".into(),
        Why::HostGone { reason: GoneReason::Left, .. } => "host left".into(),
        Why::HostGone { .. } => "host removed".into(),
        Why::SourceNotOffered { .. } => "not offered there".into(),
        Why::ModelNotOffered { .. } => "model not offered".into(),
        Why::NotAllowedOnHost { .. } => "not allowed there".into(),
        Why::OwnerGone => "owner left".into(),
        Why::NoKeyHere { .. } => "no key".into(),
    }
}
