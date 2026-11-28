//! "Set up Ligus": one checklist on every surface (workspace model v3,
//! 17.5). The desktop's Settings > Set up, `ligus setup`, lt's setup screen
//! and the phone's Health all show the rows [`checklist`] makes from the same
//! [`Facts`], so the same state reads the same everywhere (S4).
//!
//! - One row per item, each with a state and at most one action. Something
//!   never chosen reads grey **Not set up** ([`RowState::NotSetUp`]), never
//!   red; something chosen but needing a look is [`RowState::Attention`].
//! - **Clean up** gathers the leftovers ([`Leftover`]): members who left or
//!   were retired, devices revoked, idle or never finished, hosts of former
//!   members, old agent names that no longer mean anything, phones a box
//!   unpaired, and (desktop only) the old relay roster line. Each has one
//!   action: **Remove** (its own passkey ceremony, after a confirm) or
//!   **Remove from view** (this device only; nothing is written). Nothing is
//!   removed without the person confirming it first.
//! - Facts are cheap and local ([`gather`]): the fold, this device's store
//!   and what the surface already knows about hosts (beats). No network.
//!
//! The rows are data; each surface maps an [`Action`] id to its own control
//! (a pane, a command, a key) and nothing else.

use serde::{Deserialize, Serialize};

mod gather;
pub mod hidden;

/// The shared fixture every surface renders (S4): facts and the rows they
/// give, in `tests/fixtures/setup-checklist`. Exposed so lt's tests (which
/// get this crate as a git dependency) render the very same rows.
#[doc(hidden)]
pub mod fixture {
    pub const NEW_PERSON_FACTS: &str = include_str!("../../../tests/fixtures/setup-checklist/new-person.facts.json");
    pub const NEW_PERSON_CHECKLIST: &str = include_str!("../../../tests/fixtures/setup-checklist/new-person.checklist.json");
    pub const SETTLED_TEAM_FACTS: &str = include_str!("../../../tests/fixtures/setup-checklist/settled-team.facts.json");
    pub const SETTLED_TEAM_CHECKLIST: &str = include_str!("../../../tests/fixtures/setup-checklist/settled-team.checklist.json");
}
#[cfg(test)]
mod tests;

pub use gather::{gather, Live, Viewer};

/// The schema name of the checklist document (`ligus setup --json`'s
/// `checklist`, the desktop's `setup_checklist`, the box's `GET /v1/setup`).
pub const SCHEMA: &str = "ligus.setup.v1";

// ── Facts ───────────────────────────────────────────────────────────────────

/// What the rows are made from: plain data, so every surface (and a fixture)
/// can hand the same facts to [`checklist`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Facts {
    /// The workspace's name.
    pub workspace: String,
    /// Your role here (`Owner`, `Admin`, `Contributor`, `Viewer`); empty when
    /// not a member.
    pub role: String,
    /// The workspace rules version in force.
    pub rules: u32,
    /// The newest rules version this build can switch to.
    pub rules_supported: u32,
    /// Your profile name; empty when you never set one.
    pub name: String,
    /// Your profile has a picture.
    pub picture: bool,
    /// Your passkeys (not counting the recovery key).
    pub passkeys: u32,
    /// You saved a recovery key.
    pub recovery_key: bool,
    /// This device's name as the workspace shows it; empty when unnamed.
    pub this_device: String,
    /// How this device is called in the rows ("this Mac", "this terminal").
    pub this_device_word: String,
    /// Your other computers, terminals and boxes of your own (not phones).
    pub devices: Vec<DeviceItem>,
    /// Your phones.
    pub phones: Vec<PhoneItem>,
    /// The workspace's always-on boxes and workers.
    pub boxes: Vec<BoxItem>,
    /// The workspace's agents and where each runs.
    pub agents: Vec<AgentItem>,
    /// Leftovers for Clean up.
    pub leftovers: Vec<Leftover>,
    /// Leftover keys ([`Leftover::key`]) this device removed from view.
    pub hidden: Vec<String>,
}

/// One of your devices.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DeviceItem {
    pub id: String,
    pub label: String,
    /// `pending`, `active`, `idle` or `revoked`
    /// ([`crate::device_lifecycle::DeviceState::as_str`]).
    pub state: String,
}

/// One of your phones.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PhoneItem {
    pub id: String,
    pub label: String,
    /// As for [`DeviceItem::state`].
    pub state: String,
    /// Push is on (`Some(true)`) or off; `None` when this surface can't tell
    /// (only the box a phone is paired with, and the phone, know).
    pub push: Option<bool>,
}

/// An always-on box or worker.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BoxItem {
    pub id: String,
    pub label: String,
    pub online: bool,
    /// Its Ligus build from its beat (empty: not reported).
    pub build: String,
    /// A newer release it could upgrade to (empty: none known).
    pub upgrade_to: String,
}

/// One agent and where it answers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentItem {
    /// Its name, without the `@`.
    pub name: String,
    /// Where it runs, as people read it ("build-1", "this Mac", "Ana's
    /// devices").
    pub runs_on: String,
    /// It would answer a message now (its host is here or online, and the
    /// host hasn't said it won't run it).
    pub answers: bool,
    /// Why not, in a few words, when it doesn't ("offline", "host removed").
    pub why: String,
}

/// What kind of leftover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LeftoverKind {
    /// A member who asked to leave (still listed under rules 3).
    MemberLeft,
    /// A member retired into their person (a duplicate).
    MemberRetired,
    /// One of your devices approved here that never wrote anything.
    DevicePending,
    /// One of your devices with no signature and no beat for 30 days.
    DeviceIdle,
    /// One of your devices that was revoked (still listed).
    DeviceRevoked,
    /// A host record of a former member, a revoked device or a box that left.
    FormerHost,
    /// An agent whose name no longer means anything (a roster agent named
    /// after the default agent, or the Hive builds' `hive`).
    LegacyAgent,
    /// One of your phones a box unpaired, or that forgot a box, while it
    /// stays your device.
    UnpairedPhone,
    /// The People pane's old relay roster line (desktop only).
    RelayRoster,
}

impl LeftoverKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LeftoverKind::MemberLeft => "memberLeft",
            LeftoverKind::MemberRetired => "memberRetired",
            LeftoverKind::DevicePending => "devicePending",
            LeftoverKind::DeviceIdle => "deviceIdle",
            LeftoverKind::DeviceRevoked => "deviceRevoked",
            LeftoverKind::FormerHost => "formerHost",
            LeftoverKind::LegacyAgent => "legacyAgent",
            LeftoverKind::UnpairedPhone => "unpairedPhone",
            LeftoverKind::RelayRoster => "relayRoster",
        }
    }

    /// "1 former member", "2 devices never finished".
    fn count_words(self, n: usize) -> String {
        let (one, many) = match self {
            LeftoverKind::MemberLeft => ("member who asked to leave", "members who asked to leave"),
            LeftoverKind::MemberRetired => ("retired member", "retired members"),
            LeftoverKind::DevicePending => ("device never finished", "devices never finished"),
            LeftoverKind::DeviceIdle => ("idle device", "idle devices"),
            LeftoverKind::DeviceRevoked => ("revoked device", "revoked devices"),
            LeftoverKind::FormerHost => ("former member's host", "former members' hosts"),
            LeftoverKind::LegacyAgent => ("old agent name", "old agent names"),
            LeftoverKind::UnpairedPhone => ("unpaired phone", "unpaired phones"),
            LeftoverKind::RelayRoster => ("relay roster line", "relay roster lines"),
        };
        format!("{n} {}", if n == 1 { one } else { many })
    }
}

/// One leftover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Leftover {
    pub kind: LeftoverKind,
    /// What the action acts on: a member id, a device id, a host id, an
    /// agent id (a phone: its device id; the relay line: the workspace id).
    pub id: String,
    /// Its name as people read it.
    pub label: String,
    /// One line ("asked to leave 24 Nov", "approved 3 Oct, never used").
    #[serde(default)]
    pub detail: String,
    /// You may remove it from the workspace (your role, or it is yours);
    /// otherwise only Remove from view is offered.
    #[serde(default)]
    pub can_remove: bool,
}

impl Leftover {
    /// The key a "Remove from view" is remembered under: `kind:id`.
    pub fn key(&self) -> String {
        format!("{}:{}", self.kind.as_str(), self.id)
    }
}

// ── The checklist ───────────────────────────────────────────────────────────

/// The rows, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RowId {
    You,
    Passkey,
    Device,
    Devices,
    Phone,
    Box,
    Agents,
    Workspace,
    Cleanup,
}

/// A row's state. There is no red: a never-chosen item is grey Not set up,
/// and anything chosen that needs a look is amber.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RowState {
    /// ✓
    Done,
    /// ○ grey: never chosen (some rows are optional).
    NotSetUp,
    /// • amber: chosen, and something wants a look.
    Attention,
}

/// A row's one action: a stable id each surface maps to its own control,
/// and its words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Action {
    /// `profile`, `passkey`, `recovery`, `deviceName`, `addComputer`,
    /// `devices`, `addPhone`, `push`, `addBox`, `upgradeBox`, `addAgent`,
    /// `agents`, `upgradeRules`, `cleanup`.
    pub id: String,
    pub label: String,
}

fn action(id: &str, label: &str) -> Option<Action> {
    Some(Action { id: id.into(), label: label.into() })
}

/// One line of the checklist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub id: RowId,
    pub title: String,
    pub state: RowState,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub action: Option<Action>,
}

/// What a Clean up item's one action does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CleanupActionId {
    /// Remove it from the workspace: its own event, with a passkey.
    Remove,
    /// Hide it on this device only; nothing is written.
    RemoveFromView,
}

/// A Clean up item's one action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupAction {
    pub id: CleanupActionId,
    pub label: String,
    /// It asks for your passkey.
    pub passkey: bool,
    /// The question asked before anything happens.
    pub confirm: String,
}

/// One Clean up item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupItem {
    /// `kind:id`, also the Remove from view key.
    pub key: String,
    pub kind: LeftoverKind,
    pub id: String,
    pub label: String,
    pub detail: String,
    pub action: CleanupAction,
}

/// The checklist document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Checklist {
    pub schema: String,
    pub rows: Vec<Row>,
    pub cleanup: Vec<CleanupItem>,
}

impl Checklist {
    pub fn row(&self, id: RowId) -> Option<&Row> {
        self.rows.iter().find(|r| r.id == id)
    }
}

/// "a, b and c".
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// `items` joined with ", ", the first `max` only, then "+N".
fn short_list(items: &[String], max: usize) -> String {
    if items.len() <= max {
        return items.join(", ");
    }
    format!("{}, +{}", items[..max].join(", "), items.len() - max)
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn is_admin(role: &str) -> bool {
    matches!(role, "Owner" | "Admin")
}

/// The rows for `facts` (pure: the same facts give the same rows on every
/// surface).
pub fn checklist(facts: &Facts) -> Checklist {
    let not_set_up = "Not set up".to_string();
    let this_word = if facts.this_device_word.trim().is_empty() { "this device".to_string() } else { facts.this_device_word.clone() };
    let mut rows = Vec::new();

    // You: a name (and a picture).
    let name = facts.name.trim();
    rows.push(if name.is_empty() {
        Row { id: RowId::You, title: "You".into(), state: RowState::NotSetUp, detail: not_set_up.clone(), action: action("profile", "Set your name") }
    } else {
        let detail = if facts.picture { format!("{name} · picture") } else { name.to_string() };
        Row { id: RowId::You, title: "You".into(), state: RowState::Done, detail, action: None }
    });

    // Passkey and recovery key.
    rows.push(match (facts.passkeys, facts.recovery_key) {
        (0, false) => {
            Row { id: RowId::Passkey, title: "Passkey".into(), state: RowState::NotSetUp, detail: not_set_up.clone(), action: action("passkey", "Add a passkey") }
        }
        (0, true) => Row {
            id: RowId::Passkey,
            title: "Passkey".into(),
            state: RowState::Attention,
            detail: "recovery key only".into(),
            action: action("passkey", "Add a passkey"),
        },
        (n, false) => Row {
            id: RowId::Passkey,
            title: "Passkey".into(),
            state: RowState::Attention,
            detail: format!("{} · no recovery key", plural(n as usize, "passkey", "passkeys")),
            action: action("recovery", "Save a recovery key"),
        },
        (n, true) => Row {
            id: RowId::Passkey,
            title: "Passkey".into(),
            state: RowState::Done,
            detail: format!("{} · recovery key", plural(n as usize, "passkey", "passkeys")),
            action: None,
        },
    });

    // This device's name.
    let here = facts.this_device.trim();
    rows.push(if here.is_empty() {
        Row { id: RowId::Device, title: "This device".into(), state: RowState::NotSetUp, detail: "Not named".into(), action: action("deviceName", "Name it") }
    } else {
        Row { id: RowId::Device, title: "This device".into(), state: RowState::Done, detail: here.to_string(), action: None }
    });

    // Your other devices (revoked ones are Clean up's).
    let live: Vec<&DeviceItem> = facts.devices.iter().filter(|d| d.state != "revoked").collect();
    rows.push(if live.is_empty() {
        Row { id: RowId::Devices, title: "My devices".into(), state: RowState::NotSetUp, detail: not_set_up.clone(), action: action("addComputer", "Add a computer") }
    } else {
        let mut names = vec![this_word.clone()];
        names.extend(live.iter().map(|d| d.label.clone()));
        let pending = live.iter().filter(|d| d.state == "pending").count();
        let idle = live.iter().filter(|d| d.state == "idle").count();
        let mut notes = Vec::new();
        if pending > 0 {
            notes.push(format!("{pending} approved, never used"));
        }
        if idle > 0 {
            notes.push(format!("{idle} idle"));
        }
        let list = short_list(&names, 3);
        if notes.is_empty() {
            Row { id: RowId::Devices, title: "My devices".into(), state: RowState::Done, detail: list, action: None }
        } else {
            Row {
                id: RowId::Devices,
                title: "My devices".into(),
                state: RowState::Attention,
                detail: format!("{list} · {}", notes.join(" · ")),
                action: action("devices", "Review"),
            }
        }
    });

    // Phone and push.
    let phones: Vec<&PhoneItem> = facts.phones.iter().filter(|p| p.state != "revoked").collect();
    rows.push(if phones.is_empty() {
        Row { id: RowId::Phone, title: "Phone".into(), state: RowState::NotSetUp, detail: not_set_up.clone(), action: action("addPhone", "Add a phone") }
    } else {
        let names: Vec<String> = phones.iter().map(|p| p.label.clone()).collect();
        let list = short_list(&names, 2);
        let push_off = phones.iter().any(|p| p.push == Some(false));
        let push_on = phones.iter().any(|p| p.push == Some(true));
        if push_off && !push_on {
            Row { id: RowId::Phone, title: "Phone".into(), state: RowState::Attention, detail: format!("{list} · push off"), action: action("push", "Turn on push") }
        } else if push_on {
            Row { id: RowId::Phone, title: "Phone".into(), state: RowState::Done, detail: format!("{list} · push on"), action: None }
        } else {
            Row { id: RowId::Phone, title: "Phone".into(), state: RowState::Done, detail: list, action: None }
        }
    });

    // An always-on box (optional) and its update.
    rows.push(match facts.boxes.as_slice() {
        [] => Row {
            id: RowId::Box,
            title: "Box".into(),
            state: RowState::NotSetUp,
            detail: "Not set up (optional)".into(),
            action: action("addBox", "Add a box"),
        },
        boxes => {
            let shown: Vec<String> = boxes
                .iter()
                .map(|b| {
                    let mut s = b.label.clone();
                    if !b.build.is_empty() {
                        s.push_str(&format!(" · {}", b.build));
                        if !b.upgrade_to.is_empty() {
                            s.push_str(&format!(" ({} out)", b.upgrade_to));
                        }
                    }
                    if !b.online {
                        s.push_str(" · offline");
                    }
                    s
                })
                .collect();
            let detail = short_list(&shown, 2);
            if boxes.iter().any(|b| !b.upgrade_to.is_empty()) {
                Row { id: RowId::Box, title: "Box".into(), state: RowState::Done, detail, action: action("upgradeBox", "How to upgrade") }
            } else if boxes.iter().all(|b| !b.online) {
                Row { id: RowId::Box, title: "Box".into(), state: RowState::Attention, detail, action: None }
            } else {
                Row { id: RowId::Box, title: "Box".into(), state: RowState::Done, detail, action: None }
            }
        }
    });

    // An agent that answers, and where.
    rows.push(if facts.agents.is_empty() {
        Row { id: RowId::Agents, title: "Agents".into(), state: RowState::NotSetUp, detail: not_set_up.clone(), action: action("addAgent", "Add an agent") }
    } else {
        let shown: Vec<String> = facts
            .agents
            .iter()
            .map(|a| {
                let mut s = format!("@{} on {}", a.name, a.runs_on);
                if !a.answers && !a.why.is_empty() {
                    s.push_str(&format!(" ({})", a.why));
                }
                s
            })
            .collect();
        let detail = short_list(&shown, 2).replace(", ", " · ");
        if facts.agents.iter().any(|a| a.answers) {
            Row { id: RowId::Agents, title: "Agents".into(), state: RowState::Done, detail, action: None }
        } else {
            Row { id: RowId::Agents, title: "Agents".into(), state: RowState::Attention, detail, action: action("agents", "Review") }
        }
    });

    // The workspace and its rules.
    let ws = if facts.workspace.trim().is_empty() { "This workspace".to_string() } else { facts.workspace.trim().to_string() };
    let upgrade = facts.role == "Owner" && facts.rules_supported > facts.rules && facts.rules > 0;
    rows.push(if upgrade {
        Row {
            id: RowId::Workspace,
            title: "Workspace".into(),
            state: RowState::Done,
            detail: format!("{ws} · rules {} · Upgrade available", facts.rules),
            action: action("upgradeRules", "Upgrade"),
        }
    } else {
        Row { id: RowId::Workspace, title: "Workspace".into(), state: RowState::Done, detail: format!("{ws} · rules {}", facts.rules), action: None }
    });

    // Clean up.
    let cleanup = cleanup_items(facts);
    rows.push(if cleanup.is_empty() {
        Row { id: RowId::Cleanup, title: "Clean up".into(), state: RowState::Done, detail: "Nothing to clean up".into(), action: None }
    } else {
        let mut counts: Vec<(LeftoverKind, usize)> = Vec::new();
        for i in &cleanup {
            match counts.iter_mut().find(|(k, _)| *k == i.kind) {
                Some((_, n)) => *n += 1,
                None => counts.push((i.kind, 1)),
            }
        }
        counts.sort_by_key(|(k, _)| *k);
        let words: Vec<String> = counts.iter().map(|(k, n)| k.count_words(*n)).collect();
        let detail = if words.len() > 2 {
            let shown: usize = counts[..2].iter().map(|(_, n)| n).sum();
            format!("{} · +{} more", words[..2].join(" · "), cleanup.len() - shown)
        } else {
            words.join(" · ")
        };
        Row { id: RowId::Cleanup, title: "Clean up".into(), state: RowState::Attention, detail, action: action("cleanup", "Clean up") }
    });

    Checklist { schema: SCHEMA.into(), rows, cleanup }
}

/// The Clean up items for `facts`: the leftovers not removed from view, in
/// kind order, each with its one action and the question asked first.
pub fn cleanup_items(facts: &Facts) -> Vec<CleanupItem> {
    let admin = is_admin(&facts.role);
    let ws = if facts.workspace.trim().is_empty() { "the workspace".to_string() } else { facts.workspace.trim().to_string() };
    let mut out: Vec<CleanupItem> = facts
        .leftovers
        .iter()
        .filter(|l| !facts.hidden.contains(&l.key()))
        .map(|l| {
            let name = if l.label.trim().is_empty() { "this".to_string() } else { l.label.trim().to_string() };
            let hide = CleanupAction {
                id: CleanupActionId::RemoveFromView,
                label: "Remove from view".into(),
                passkey: false,
                confirm: format!("Remove {name} from view on this device? Nothing changes in {ws}, and other devices still show it."),
            };
            let remove = |confirm: String| CleanupAction { id: CleanupActionId::Remove, label: "Remove".into(), passkey: true, confirm };
            let action = match l.kind {
                LeftoverKind::MemberLeft if admin && l.can_remove => {
                    remove(format!("Remove {name} from {ws}? They asked to leave; this takes them off the member list and rotates the workspace key. It asks for your passkey."))
                }
                LeftoverKind::DevicePending | LeftoverKind::DeviceIdle if l.can_remove => {
                    remove(format!("Remove {name} from your devices? It stops counting as yours in {ws}, and it can't be undone. It asks for your passkey."))
                }
                LeftoverKind::UnpairedPhone if l.can_remove => {
                    remove(format!("Remove the phone {name} from your devices? It can't be undone; add it again to use it. It asks for your passkey."))
                }
                LeftoverKind::FormerHost if l.can_remove => {
                    remove(format!("Remove the host {name} from {ws}? Agents placed on it stay where they are and keep saying the host is gone. It asks for your passkey."))
                }
                LeftoverKind::LegacyAgent if l.can_remove => {
                    remove(format!("Remove the agent @{name} from {ws}? It can't be undone. It asks for your passkey."))
                }
                _ => hide,
            };
            CleanupItem { key: l.key(), kind: l.kind, id: l.id.clone(), label: l.label.clone(), detail: l.detail.clone(), action }
        })
        .collect();
    out.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.label.cmp(&b.label)).then_with(|| a.id.cmp(&b.id)));
    out
}

/// The mark a row's state reads as in text: ✓ done, ○ not set up, • attention.
pub fn mark(state: RowState) -> &'static str {
    match state {
        RowState::Done => "✓",
        RowState::NotSetUp => "○",
        RowState::Attention => "•",
    }
}

/// The checklist as text, one line per row (`ligus setup`, lt): the mark,
/// the title, the detail and, when the row has an action, what `how` says
/// to do for it (a command, a key). Clean up's items follow under it.
pub fn render_text(list: &Checklist, how: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    for r in &list.rows {
        let act = r.action.as_ref().and_then(|a| how(&a.id).map(|h| format!("{}: {h}", a.label)));
        let line = match act {
            Some(a) => format!("{} {:<12} {:<48} {a}", mark(r.state), r.title, r.detail),
            None => format!("{} {:<12} {}", mark(r.state), r.title, r.detail),
        };
        out.push_str(line.trim_end());
        out.push('\n');
    }
    if !list.cleanup.is_empty() {
        out.push('\n');
        out.push_str("Clean up\n");
        for i in &list.cleanup {
            let detail = if i.detail.is_empty() { String::new() } else { format!(" · {}", i.detail) };
            out.push_str(&format!("  {}{detail}  [{}]  {}\n", i.label, i.action.label, i.key));
        }
    }
    out
}

/// The kinds of leftover, as their words, for a summary ("2 idle devices").
pub fn leftover_summary(items: &[CleanupItem]) -> String {
    let mut kinds: Vec<LeftoverKind> = items.iter().map(|i| i.kind).collect();
    kinds.sort();
    kinds.dedup();
    and_list(&kinds.iter().map(|k| k.count_words(items.iter().filter(|i| i.kind == *k).count())).collect::<Vec<_>>())
}
