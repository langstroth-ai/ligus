//! TestIdPCannotGrantMembership (PHASE0-SPECS E.5), client side.
//!
//! A ligus-auth token with `workspaces:[W]` gives a person relay transport to
//! W, and nothing else: no workspace key and no signed `MemberAdded`. Here an
//! outsider with exactly that (the test relay is open, standing in for a
//! relay that admits the token) writes everything they can into W: a chat
//! message, a config log founding themselves as Owner, and the same again
//! without encryption. A member's device pulls the room and none of it folds:
//! the roster is unchanged and the outsider's posts never appear. Relay
//! reachability is not membership.

use hive_core::{
    workspace_config_session_id, ActorIdentity, ActorKind, SigningKeypair, WorkspaceMember,
    WorkspaceRole,
};
use uuid::Uuid;

use crate::{ChatService, EventStore, RelayClient, SyncEngine};

fn service(path: &std::path::Path, id: &str) -> ChatService {
    let store = EventStore::open(path).unwrap();
    let author = ActorIdentity::new(id, id, ActorKind::Human);
    ChatService::new(
        store,
        Uuid::new_v4(),
        SigningKeypair::generate().unwrap(),
        SigningKeypair::generate().unwrap(),
        author,
    )
}

async fn spawn_relay() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, crate::test_relay::router())
            .await
            .unwrap()
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn idp_relay_access_cannot_grant_membership() {
    let relay = spawn_relay().await;
    let room = "ws-home";
    let wid = hive_core::room_workspace_id(room);
    let key = hive_core::derive_workspace_key("the family passphrase");
    let dir = tempfile::tempdir().unwrap();

    // Ana founds W and posts.
    let ana_db = dir.path().join("ana.db");
    let chat_id = {
        let mut ana = service(&ana_db, "ana");
        let chat = ana.create_chat("Home", wid, "anthropic").unwrap();
        ana.ensure_workspace_config(wid).unwrap();
        ana.post_user_message(chat.id, wid, "hello from ana")
            .unwrap();
        chat.id
    };
    let mut ana_sync = SyncEngine::new(RelayClient::new(&relay), room).with_key(key);
    let pushed = ana_sync
        .push_new(&EventStore::open(&ana_db).unwrap())
        .await
        .unwrap();
    assert!(pushed > 0);

    // Eve: relay access to W only (what an IdP group grant gives her).
    let eve_db = dir.path().join("eve.db");
    {
        let mut eve = service(&eve_db, "eve");
        let chat = eve.create_chat("Takeover", wid, "anthropic").unwrap();
        eve.ensure_workspace_config(wid).unwrap();
        let owner = WorkspaceMember {
            id: "eve".into(),
            actor: ActorIdentity::new("eve", "Eve", ActorKind::Human),
            role: WorkspaceRole::Owner,
            title: String::new(),
            index: 0,
            joined_at: hive_core::Timestamp::now(),
        };
        let _ = eve.add_member(workspace_config_session_id(wid), wid, owner);
        eve.post_user_message(chat.id, wid, "eve was here").unwrap();
        eve.post_user_message(chat_id, wid, "eve in ana's chat")
            .unwrap();
    }
    let eve_store = EventStore::open(&eve_db).unwrap();
    // With a guessed key, and with no key at all.
    let guessed = hive_core::derive_workspace_key("a guess");
    let sent = SyncEngine::new(RelayClient::new(&relay), room)
        .with_key(guessed)
        .push_new(&eve_store)
        .await
        .unwrap();
    assert!(
        sent > 0,
        "the relay forwards eve's envelopes (transport is granted)"
    );
    let _ = SyncEngine::new(RelayClient::new(&relay), room)
        .push_new(&eve_store)
        .await;

    // Ana pulls everything the relay forwards.
    let mut ana_store = EventStore::open(&ana_db).unwrap();
    for _ in 0..5 {
        let _ = ana_sync.pull_round(&mut ana_store).await;
    }
    let ana = service(&ana_db, "ana-reader");
    let cfg = ana
        .load(workspace_config_session_id(wid))
        .unwrap()
        .expect("config log");
    let ids: Vec<&str> = cfg.members.iter().map(|m| m.id.as_str()).collect();
    assert!(
        !ids.contains(&"eve"),
        "relay access must not make eve a member: {ids:?}"
    );
    let chat = ana.load(chat_id).unwrap().unwrap();
    assert!(
        chat.messages.iter().any(|m| m.body == "hello from ana"),
        "ana's own post is there"
    );
    assert!(
        chat.messages.iter().all(|m| !m.body.contains("eve")),
        "eve's posts must not fold into ana's chat"
    );
}
