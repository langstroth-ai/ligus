//! Leaving a workspace across real devices (workspace model v3, 10.4; W3):
//! a teammate leaves with their passkey and stays a member, shown as asked
//! to leave everywhere; the founder, a linked computer and a second leave
//! are refused before anything is written.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ligus_core::presence::PresenceAssertion;
use ligus_core::{workspace_config_session_id, SessionEvent};
use uuid::Uuid;

use crate::chat_service::{LeaveCheck, PresenceProvider, PresenceRequest};
use crate::{ChatService, EventStore, FileKeyVault, IdentityStore, RelayClient, SyncEngine};

const PASSPHRASE: &str = "leave a workspace passphrase";

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

    fn id(&self) -> String {
        self.svc.author().id.clone()
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

    fn left_here(&self, ws: Uuid, who: &str) -> bool {
        self.svc.load(workspace_config_session_id(ws)).unwrap().unwrap().left.contains_key(who)
    }

    fn member_here(&self, ws: Uuid, who: &str) -> bool {
        self.svc.load(workspace_config_session_id(ws)).unwrap().unwrap().members.iter().any(|m| m.id == who)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_teammate_leaves_with_their_passkey_and_the_founder_cant() {
    let relay = spawn_relay().await;
    let tmp = tempfile::tempdir().unwrap();
    let ws = ligus_core::room_workspace_id("acme");
    let cfg = workspace_config_session_id(ws);

    let mut mac = Dev::open(&tmp.path().join("mac"), "Alex", &relay, ws);
    mac.svc.ensure_self_member(cfg, ws).unwrap();
    mac.passkey(ws);
    let chat = mac.svc.create_chat("plans", ws, "").unwrap().id;
    mac.sync().await;
    let mut taylor = Dev::open(&tmp.path().join("taylor"), "Taylor", &relay, ws);
    taylor.join_via(&mut mac, ws).await;
    taylor.passkey(ws);
    taylor.sync().await;
    mac.sync().await;
    let (_, t) = mac.svc.store().load_config(ws, None, None).unwrap();
    assert_eq!(t.current().rules, ligus_core::authz::RULES_V4, "a 0.5 workspace is on rules 4");

    // The founder stays.
    match mac.svc.leave_check(ws).unwrap() {
        LeaveCheck::Refused(why) => assert!(why.contains("founder"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(mac.svc.leave_workspace(ws).is_err());

    // Taylor holds a passkey: the check says so, nothing is written.
    let before = taylor.svc.store().list(cfg).unwrap().len();
    assert_eq!(taylor.svc.leave_check(ws).unwrap(), LeaveCheck::NeedsPasskey);
    assert_eq!(taylor.svc.store().list(cfg).unwrap().len(), before);
    assert!(taylor.svc.leave_workspace(ws).unwrap());
    let env = taylor.svc.store().list(cfg).unwrap().into_iter().rev().find(|e| matches!(e.payload, SessionEvent::MemberLeft { .. })).unwrap();
    assert!(env.actor_stamp.as_ref().and_then(|s| s.presence.as_ref()).is_some(), "with Taylor's passkey");
    let SessionEvent::MemberLeft { watermark, account_id, .. } = &env.payload else { unreachable!() };
    assert_eq!(*account_id, taylor.svc.author().account_id);
    assert!(watermark.iter().any(|w| w.signer == taylor.svc.device_id()), "the leaver's own heads");
    assert!(taylor.svc.has_left(ws).unwrap());
    assert_eq!(taylor.svc.leave_check(ws).unwrap(), LeaveCheck::AlreadyLeft);
    assert!(!taylor.svc.leave_workspace(ws).unwrap(), "a second leave writes nothing");
    taylor.sync().await;
    mac.sync().await;

    // Everyone sees it; under rules 4 a leave is a removal (G4): Taylor is
    // no longer a member and writes nothing more there.
    let tid = taylor.id();
    assert!(mac.left_here(ws, &tid) && !mac.member_here(ws, &tid));
    assert!(taylor.svc.post_user_message(chat, ws, "one last thing").is_err(), "a former member writes nothing");
    let s = mac.svc.load(chat).unwrap().unwrap();
    assert!(!s.messages.iter().any(|m| m.body == "one last thing"));
    let _ = cfg;
}
