use std::path::{Path, PathBuf};
use std::sync::Arc;

use ligus_core::presence::PresenceAssertion;
use ligus_core::{workspace_config_session_id, WorkspaceAgent};
use uuid::Uuid;

use super::*;
use crate::chat_service::{PresenceProvider, PresenceRequest};
use crate::{ChatService, EventStore, FileKeyVault, IdentityStore, RelayClient, SyncEngine};

/// The shared fixture every surface renders (S4): the facts, and the rows
/// they must give. `LIGUS_BLESS=1` rewrites the rows after a deliberate
/// change (the desktop's, lt's and the phone's tests read the same file).
fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/setup-checklist")
}

fn load_facts(name: &str) -> Facts {
    let text = std::fs::read_to_string(fixture_dir().join(format!("{name}.facts.json"))).unwrap();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn the_shared_fixture_gives_the_same_rows() {
    for name in ["new-person", "settled-team"] {
        let facts = load_facts(name);
        let list = checklist(&facts);
        let path = fixture_dir().join(format!("{name}.checklist.json"));
        let got = serde_json::to_string_pretty(&list).unwrap() + "\n";
        if std::env::var("LIGUS_BLESS").is_ok() {
            std::fs::write(&path, &got).unwrap();
        }
        let want = std::fs::read_to_string(&path).unwrap();
        assert_eq!(got, want, "{name}: the rows changed; run with LIGUS_BLESS=1 if on purpose, and update every surface");
        // Text for the CLI and lt follows the same rows.
        let text_path = fixture_dir().join(format!("{name}.txt"));
        let text = render_text(&list, &|id| Some(format!("ligus {id}")));
        if std::env::var("LIGUS_BLESS").is_ok() {
            std::fs::write(&text_path, &text).unwrap();
        }
        assert_eq!(text, std::fs::read_to_string(&text_path).unwrap(), "{name}: text");
    }
}

#[test]
fn the_exported_fixture_is_the_files() {
    let pairs = [
        (fixture::NEW_PERSON_FACTS, fixture::NEW_PERSON_CHECKLIST, "new-person"),
        (fixture::SETTLED_TEAM_FACTS, fixture::SETTLED_TEAM_CHECKLIST, "settled-team"),
    ];
    for (facts, rows, name) in pairs {
        let f: Facts = serde_json::from_str(facts).unwrap();
        let want: Checklist = serde_json::from_str(rows).unwrap();
        assert_eq!(checklist(&f), want, "{name}");
    }
}

#[test]
fn never_chosen_reads_not_set_up_and_nothing_is_red() {
    let list = checklist(&load_facts("new-person"));
    for id in [RowId::You, RowId::Passkey, RowId::Devices, RowId::Phone, RowId::Box, RowId::Agents] {
        let r = list.row(id).unwrap();
        assert_eq!(r.state, RowState::NotSetUp, "{id:?}");
        assert!(r.detail.starts_with("Not set up"), "{id:?}: {}", r.detail);
        assert!(r.action.is_some(), "{id:?} has its one action");
    }
    assert_eq!(list.row(RowId::Cleanup).unwrap().state, RowState::Done);
    let json = serde_json::to_string(&list).unwrap();
    for word in ["fail", "error", "red", "\"off\""] {
        assert!(!json.to_lowercase().contains(word), "{word} in {json}");
    }
}

#[test]
fn every_row_has_at_most_one_action_and_cleanup_always_confirms() {
    let list = checklist(&load_facts("settled-team"));
    assert_eq!(list.rows.len(), 9);
    for i in &list.cleanup {
        assert!(!i.action.confirm.is_empty(), "{}", i.key);
        assert_eq!(i.action.passkey, i.action.id == CleanupActionId::Remove, "{}", i.key);
        if i.action.id == CleanupActionId::RemoveFromView {
            assert!(i.action.confirm.contains("on this device"), "{}", i.action.confirm);
        }
    }
}

#[test]
fn a_hidden_leftover_leaves_clean_up() {
    let mut facts = load_facts("settled-team");
    let before = checklist(&facts).cleanup.len();
    let key = facts.leftovers[0].key();
    facts.hidden.push(key.clone());
    let after = checklist(&facts);
    assert_eq!(after.cleanup.len(), before - 1);
    assert!(after.cleanup.iter().all(|i| i.key != key));
}

#[test]
fn only_admins_remove_members_and_hosts() {
    let mut facts = load_facts("settled-team");
    facts.role = "Contributor".into();
    for i in checklist(&facts).cleanup {
        if matches!(i.kind, LeftoverKind::MemberLeft) {
            assert_eq!(i.action.id, CleanupActionId::RemoveFromView, "{}", i.key);
        }
    }
    // A leftover the gatherer said can't be removed is only hidden.
    let mut facts = load_facts("settled-team");
    for l in &mut facts.leftovers {
        l.can_remove = false;
    }
    assert!(checklist(&facts).cleanup.iter().all(|i| i.action.id == CleanupActionId::RemoveFromView));
}

#[test]
fn rules_upgrade_is_for_owners_only() {
    let mut facts = load_facts("settled-team");
    facts.rules = 3;
    facts.rules_supported = 4;
    facts.role = "Owner".into();
    let r = checklist(&facts).row(RowId::Workspace).cloned().unwrap();
    assert!(r.detail.contains("Upgrade available") && r.action.as_ref().is_some_and(|a| a.id == "upgradeRules"), "{r:?}");
    facts.role = "Admin".into();
    let r = checklist(&facts).row(RowId::Workspace).cloned().unwrap();
    assert!(!r.detail.contains("Upgrade") && r.action.is_none(), "{r:?}");
    facts.rules = 4;
    facts.role = "Owner".into();
    assert_eq!(checklist(&facts).row(RowId::Workspace).unwrap().detail, "Team · rules 4");
}

/// Names and labels come from other members' events: the text a terminal
/// prints never carries their control characters (an escape sequence in an
/// agent's name can't recolour or rewrite the screen).
#[test]
fn text_never_carries_control_characters() {
    let mut facts = load_facts("settled-team");
    facts.agents[0].name = "co\u{1b}[31mder\u{7}".into();
    facts.leftovers[0].label = "Tay\u{1b}]0;owned\u{7}lor".into();
    facts.boxes[0].label = "box\r\n-1".into();
    let text = render_text(&checklist(&facts), &|_| None);
    assert!(!text.chars().any(|c| c.is_control() && c != '\n'), "{text:?}");
    assert!(text.contains("@co[31mder") && text.contains("Tay]0;ownedlor"), "{text}");
}

#[test]
fn upgrade_to_says_nothing_rather_than_guess() {
    assert_eq!(gather::upgrade_to("0.4.1", Some("v0.5.0")), "0.5.0");
    assert_eq!(gather::upgrade_to("0.5.0", Some("0.5.0")), "");
    assert_eq!(gather::upgrade_to("0.5.0-dev", Some("0.6.0")), "");
    assert_eq!(gather::upgrade_to("", Some("0.6.0")), "");
    assert_eq!(gather::upgrade_to("0.4.1", None), "");
}

#[test]
fn hidden_keys_round_trip_and_an_unreadable_file_is_kept() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(hidden::load(tmp.path()).is_empty());
    hidden::set(tmp.path(), "memberLeft:abc", false).unwrap();
    hidden::set(tmp.path(), "deviceRevoked:def", false).unwrap();
    assert_eq!(hidden::load(tmp.path()), vec!["memberLeft:abc".to_string(), "deviceRevoked:def".to_string()]);
    hidden::set(tmp.path(), "memberLeft:abc", true).unwrap();
    assert_eq!(hidden::load(tmp.path()), vec!["deviceRevoked:def".to_string()]);
    assert!(hidden::set(tmp.path(), "no colon", false).is_err());
    std::fs::write(tmp.path().join(hidden::FILE), b"{not json").unwrap();
    assert!(hidden::set(tmp.path(), "memberLeft:x", false).is_err(), "never written over");
    assert_eq!(std::fs::read(tmp.path().join(hidden::FILE)).unwrap(), b"{not json");
    assert!(hidden::load(tmp.path()).is_empty(), "unreadable hides nothing");
}

// ── gather, over real devices ───────────────────────────────────────────────

const PASSPHRASE: &str = "set up ligus passphrase";

async fn spawn_relay() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, crate::test_relay::router()).await.unwrap() });
    format!("http://{addr}")
}

struct TestPasskey(Uuid);

impl PresenceProvider for TestPasskey {
    fn assert(&self, req: &PresenceRequest) -> Result<PresenceAssertion, String> {
        Ok(ligus_core::testkit::test_recovery_key(self.0).assert(&req.challenge))
    }
}

struct Dev {
    svc: ChatService,
    dir: PathBuf,
    engine: SyncEngine,
}

impl Dev {
    fn open(dir: &Path, name: &str, relay: &str, ws: Uuid) -> Self {
        std::fs::create_dir_all(dir).unwrap();
        let ids = IdentityStore::new(dir, FileKeyVault::new(dir));
        let me = ids.bootstrap(name, &name.to_lowercase(), name).unwrap();
        let svc = ChatService::new(
            EventStore::open(dir.join("ligus.db")).unwrap(),
            ids.device_keypair(me.device.id).unwrap(),
            ids.account_keypair(me.account.id).unwrap(),
            me.account.actor(),
        );
        let key = ligus_core::derive_workspace_key(PASSPHRASE);
        let engine = SyncEngine::new(RelayClient::new(relay), ws.to_string()).with_room_workspace(ws).with_key(key);
        Self { svc, dir: dir.to_path_buf(), engine }
    }

    async fn sync(&mut self) {
        let mut store = EventStore::open(self.dir.join("ligus.db")).unwrap();
        self.engine.push_new(&store).await.unwrap();
        self.engine.pull(&mut store).await.unwrap();
    }

    fn passkey(&mut self, ws: Uuid) {
        let acct = self.svc.author().account_id.unwrap();
        let key = ligus_core::testkit::test_recovery_key(acct);
        self.svc.register_passkey(ws, key.credential(acct, "Test recovery key"), &|req| Ok(key.assert(&req.challenge))).unwrap();
        self.svc.set_presence_provider(Some(Arc::new(TestPasskey(acct))));
    }

    async fn join_via(&mut self, admin: &mut Dev, ws: Uuid) {
        let invite = admin.svc.issue_invite(ws).unwrap();
        admin.sync().await;
        self.sync().await;
        self.svc.store().set_join_invite(ws, invite).unwrap();
        self.svc.store().set_join_redeem(ws, admin.svc.take_invite_redeem_seed(ws, invite).unwrap().as_ref()).unwrap();
        self.svc.ensure_self_member(workspace_config_session_id(ws), ws).unwrap();
        self.svc.publish_identity(workspace_config_session_id(ws), ws).unwrap();
    }

    fn facts(&self, ws: Uuid) -> Facts {
        let live = Live { this_device_word: "this Mac".into(), workspace_name: "Acme".into(), ..Live::default() };
        gather(&self.svc, ws, &Viewer::author(&self.svc), &live)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gather_reads_the_fold_and_clean_up_follows_it() {
    let relay = spawn_relay().await;
    let tmp = tempfile::tempdir().unwrap();
    let ws = ligus_core::room_workspace_id("acme-setup");
    let cfg = workspace_config_session_id(ws);

    let mut mac = Dev::open(&tmp.path().join("mac"), "Alex", &relay, ws);
    mac.svc.ensure_self_member(cfg, ws).unwrap();

    // A new workspace: the founder, nothing else.
    let f = mac.facts(ws);
    assert_eq!((f.workspace.as_str(), f.role.as_str()), ("Acme", "Owner"));
    assert!(f.rules >= ligus_core::authz::RULES_GENESIS, "{f:?}");
    assert_eq!((f.passkeys, f.recovery_key), (0, false));
    assert!(f.agents.is_empty() && f.boxes.is_empty() && f.phones.is_empty() && f.devices.is_empty());
    let list = checklist(&f);
    assert_eq!(list.row(RowId::Passkey).unwrap().state, RowState::NotSetUp);
    assert_eq!(list.row(RowId::Agents).unwrap().state, RowState::NotSetUp);
    assert_eq!(list.row(RowId::Cleanup).unwrap().detail, "Nothing to clean up");

    mac.passkey(ws);
    mac.svc.set_profile(ws, Some("Alex"), None, false).unwrap();
    // One agent of yours, and one named like the Hive builds' default agent.
    let mut coder = WorkspaceAgent::new("coder", "claude-code");
    coder.owner_actor_id = mac.svc.author().id.clone();
    mac.svc.add_agent(ws, ws, coder).unwrap();
    let mut old = WorkspaceAgent::new("hive", "claude-code");
    old.owner_actor_id = mac.svc.author().id.clone();
    let old_id = old.id;
    mac.svc.add_agent(ws, ws, old).unwrap();
    mac.sync().await;

    // A teammate joins and asks to leave.
    let mut taylor = Dev::open(&tmp.path().join("taylor"), "Taylor", &relay, ws);
    taylor.join_via(&mut mac, ws).await;
    taylor.passkey(ws);
    // Taylor's always-on box, a host record of their own.
    let taylor_box = taylor.svc.device_id().to_string();
    taylor.svc.register_host(ws, ligus_core::WorkspaceHost::new(taylor_box.clone(), ligus_core::HostKind::Worker, "taylor-box")).unwrap();
    taylor.sync().await;
    mac.sync().await;
    let f = mac.facts(ws);
    assert!(f.boxes.iter().any(|b| b.id == taylor_box && b.label == "taylor-box"), "{:?}", f.boxes);
    assert!(!is_former_host(&f, &taylor_box), "a member's live host is never a leftover");
    assert!(taylor.svc.leave_workspace(ws).unwrap());
    taylor.sync().await;
    mac.sync().await;

    let f = mac.facts(ws);
    assert_eq!(f.name, "Alex");
    assert!(f.passkeys + u32::from(f.recovery_key) >= 1, "{f:?}");
    assert_eq!(f.agents.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), vec!["coder"]);
    assert!(f.agents[0].answers && f.agents[0].runs_on == "your devices", "{:?}", f.agents[0]);
    let tid = taylor.svc.author().id.clone();
    let left = f.leftovers.iter().find(|l| l.kind == LeftoverKind::MemberLeft).expect("Taylor asked to leave");
    assert_eq!((left.id.as_str(), left.label.as_str(), left.can_remove), (tid.as_str(), "Taylor", true));
    assert!(left.detail.starts_with("asked to leave "), "{}", left.detail);
    let legacy = f.leftovers.iter().find(|l| l.kind == LeftoverKind::LegacyAgent).expect("the old @hive");
    assert_eq!((legacy.id.clone(), legacy.can_remove), (old_id.to_string(), true));

    let list = checklist(&f);
    let row = list.row(RowId::Cleanup).unwrap();
    assert_eq!(row.state, RowState::Attention);
    assert!(row.detail.contains("1 member who asked to leave") && row.detail.contains("1 old agent name"), "{}", row.detail);
    let item = list.cleanup.iter().find(|i| i.kind == LeftoverKind::MemberLeft).unwrap();
    assert_eq!(item.action.id, CleanupActionId::Remove);
    assert!(item.action.confirm.contains("Taylor") && item.action.confirm.contains("passkey"), "{}", item.action.confirm);

    // Taylor's own checklist never offers to remove anyone.
    let tf = taylor.facts(ws);
    assert_eq!(tf.role, "Contributor");
    assert!(checklist(&tf).cleanup.iter().all(|i| i.action.id == CleanupActionId::RemoveFromView || i.kind == LeftoverKind::DevicePending || i.kind == LeftoverKind::DeviceIdle));

    // Removing them through the existing ceremony clears the row.
    mac.svc.remove_member(cfg, ws, tid).unwrap();
    mac.svc.remove_agent(ws, ws, old_id).unwrap();
    let f = mac.facts(ws);
    assert!(f.leftovers.iter().all(|l| l.kind != LeftoverKind::MemberLeft && l.kind != LeftoverKind::LegacyAgent), "{:?}", f.leftovers);
    // On rules 3 the fold makes a former member's host inert: it is no box
    // any more, and there is nothing left over to remove.
    assert!(f.rules >= ligus_core::authz::RULES_V3 || is_former_host(&f, &taylor_box), "{:?}", f.leftovers);
    assert!(f.boxes.iter().all(|b| b.id != taylor_box), "{:?}", f.boxes);
    assert_eq!(checklist(&f).row(RowId::Cleanup).unwrap().state, RowState::Done, "{:?}", f.leftovers);
}
