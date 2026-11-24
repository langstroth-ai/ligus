//! `ligus devices`: a person's CLI identity approves another computer headless
//! (`approve`, with their recovery key) and the computer links (`request`,
//! `redeem`), synced through the in-process test relay; then the computer
//! posts as the person, and its refusals (enroll, a second redeem, an
//! unconfirmed approver) hold.

use super::*;
use crate::devices_cmd;

fn words_file(dir: &Path, account: Uuid) -> String {
    let path = dir.join("recovery-words");
    let words = ligus_core::testkit::test_recovery_key(account).words();
    ligus_runtime::private_file::write_private_atomic(&path, words.as_bytes()).unwrap();
    path.display().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_computer_links_as_the_persons_device_through_the_cli() {
    let tmp = tempfile::tempdir().unwrap();
    let relay = spawn(test_relay::router()).await;
    let room = format!("link-{}", Uuid::new_v4());
    let ws = crate::uuid_of_room(&room);
    let config_id = workspace_config_session_id(ws);
    // The person's identity founds the workspace (and its recovery key).
    let desk = config(&tmp.path().join("desk"), &relay, &room);
    let (alex, chat) = {
        let mut svc = open_service(&desk).unwrap();
        svc.ensure_self_member(config_id, ws).unwrap();
        let acct = svc.author().account_id.unwrap();
        let key = ligus_core::testkit::test_recovery_key(acct);
        svc.register_passkey(ws, key.credential(acct, "Recovery key"), &|req| Ok(key.assert(&req.challenge))).unwrap();
        let chat = svc.create_chat("plans", ws, "").unwrap().id;
        (svc.author().clone(), chat)
    };
    tokio::time::timeout(REQ, sync_once(&desk)).await.unwrap().unwrap();

    // The computer: a fresh data dir, no connection yet.
    let comp_dir = tmp.path().join("laptop");
    std::fs::create_dir_all(&comp_dir).unwrap();
    let mut comp =
        Config { data_dir: comp_dir.clone(), relay_url: None, token: None, room: "default".into(), key: None, epoch_keys: Vec::new() };
    devices_cmd::cmd_request(&comp, Some("laptop".into()), false, true).unwrap();
    let state = ligus_runtime::computer_link::LinkState::load(&comp_dir).unwrap();
    let request = state.latest_pending(ligus_core::flow::now_ms()).unwrap().code.clone();
    let fp = ligus_runtime::computer_link::LinkRequest::decode(&request).unwrap().fingerprint();

    // Approve: refused without the fingerprint (a person compares it).
    let words = words_file(tmp.path(), alex.account_id.unwrap());
    let e = devices_cmd::cmd_approve(&desk, Some(&request), None, None, Some(words.clone()), None, true, None).await.unwrap_err();
    assert_eq!(crate::problem::classify(&e).0, crate::problem::EXIT_HUMAN);
    let out = tmp.path().join("grant");
    devices_cmd::cmd_approve(&desk, Some(&request), None, Some(fp), Some(words), Some(out.display().to_string()), true, None)
        .await
        .unwrap();

    // Redeem: refused until the person confirms the approving desktop.
    let e = devices_cmd::cmd_redeem(&mut comp, None, Some(out.display().to_string()), None, true, None).await.unwrap_err();
    assert_eq!(crate::problem::classify(&e).0, crate::problem::EXIT_HUMAN, "{e}");
    let e = devices_cmd::cmd_redeem(&mut comp, None, Some(out.display().to_string()), Some("0000-0000-0000-0000-0000".into()), true, None)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("don't use it"), "{e}");
    let approver = {
        let svc = open_service(&desk).unwrap();
        ligus_runtime::device_enroll::fingerprint(&svc.device_public_key())
    };
    devices_cmd::cmd_redeem(&mut comp, None, Some(out.display().to_string()), Some(approver.clone()), true, None).await.unwrap();
    assert_eq!(comp.room, room);

    // The computer acts as Alex.
    {
        let mut svc = open_service(&comp).unwrap();
        assert_eq!(svc.author().id, alex.id);
        assert!(svc.is_delegated());
        svc.post_user_message(chat, ws, "from the laptop").unwrap();
    }
    tokio::time::timeout(REQ, sync_once(&comp)).await.unwrap().unwrap();
    tokio::time::timeout(REQ, sync_once(&desk)).await.unwrap().unwrap();
    let svc = open_service(&desk).unwrap();
    let s = svc.load_in(ws, chat).unwrap().unwrap();
    assert!(s.messages.iter().any(|m| m.body == "from the laptop" && m.actor_identity.as_ref().is_some_and(|a| a.id == alex.id)));
    assert!(svc.own_devices(ws).unwrap().iter().any(|d| !d.revoked && d.device_id != svc.device_id()));
    drop(svc);

    // Single use; and a linked computer accepts no invites.
    let mut again = comp.clone();
    let e = devices_cmd::cmd_redeem(&mut again, None, Some(out.display().to_string()), Some(approver), true, None).await.unwrap_err();
    assert!(e.to_string().contains("doesn't answer a request"), "{e}");
    let e = devices_cmd::refuse_if_linked(&comp, "accept invites").unwrap_err();
    assert_eq!(crate::problem::classify(&e).0, crate::problem::EXIT_REFUSED);
    devices_cmd::cmd_list(&comp, true).unwrap();
}
