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
    devices_cmd::cmd_request(&comp, Some("laptop".into()), false, true, None).unwrap();
    let state = ligus_runtime::computer_link::LinkState::load(&comp_dir).unwrap();
    let request = state.latest_pending(ligus_core::flow::now_ms()).unwrap().code.clone();
    let fp = ligus_runtime::computer_link::LinkRequest::decode(&request).unwrap().fingerprint();

    // Approve: refused without the fingerprint (a person compares it).
    let words = words_file(tmp.path(), alex.account_id.unwrap());
    let e = devices_cmd::cmd_approve(&desk, Some(&request), None, None, Some(words.clone()), None, true, None, None).await.unwrap_err();
    assert_eq!(crate::problem::classify(&e).0, crate::problem::EXIT_HUMAN);
    let out = tmp.path().join("grant");
    devices_cmd::cmd_approve(&desk, Some(&request), None, Some(fp), Some(words), Some(out.display().to_string()), true, None, None)
        .await
        .unwrap();

    // Redeem: refused until the person confirms the approving desktop.
    let e = devices_cmd::cmd_redeem(&mut comp, None, Some(out.display().to_string()), None, true, None, None).await.unwrap_err();
    assert_eq!(crate::problem::classify(&e).0, crate::problem::EXIT_HUMAN, "{e}");
    let e = devices_cmd::cmd_redeem(&mut comp, None, Some(out.display().to_string()), Some("0000-0000-0000-0000-0000".into()), true, None, None)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("don't use it"), "{e}");
    let approver = {
        let svc = open_service(&desk).unwrap();
        ligus_runtime::device_enroll::fingerprint(&svc.device_public_key())
    };
    // No code away from a terminal: a person must act.
    let e = devices_cmd::cmd_redeem(&mut comp, None, None, None, true, None, None).await.unwrap_err();
    assert_eq!(crate::problem::classify(&e).0, crate::problem::EXIT_HUMAN, "{e}");
    // At a terminal: Enter reads the clipboard, which holds the whole
    // copied command; the grant is pulled out of it.
    let grant = std::fs::read_to_string(&out).unwrap();
    let copied = format!("lt --plain link-redeem '{}' --approver-fingerprint {approver}\n", grant.trim());
    let from_clipboard = |c: String| {
        move || crate::link_io::read_pasted(&mut |_| Ok("\n".to_string()), &mut || Some(c.clone()))
    };
    // A no at the confirm links nothing.
    let mut paste = from_clipboard(copied.clone());
    let mut no = |_: &str| Ok(false);
    let e = devices_cmd::cmd_redeem(&mut comp, None, None, None, true, Some(&mut paste), Some(&mut no)).await.unwrap_err();
    assert!(e.to_string().contains("wasn't confirmed"), "{e}");
    assert_eq!(comp.room, "default");
    // A clipboard with this computer's own request instead says so.
    let mut paste = from_clipboard(request.clone());
    let e = devices_cmd::cmd_redeem(&mut comp, None, None, None, true, Some(&mut paste), None).await.unwrap_err();
    assert!(e.to_string().contains("this computer's request"), "{e}");
    // A yes links it; the prompt names the approving desktop.
    let mut asked = String::new();
    let mut paste = from_clipboard(copied);
    let mut yes = |p: &str| {
        asked = p.to_string();
        Ok(true)
    };
    devices_cmd::cmd_redeem(&mut comp, None, None, None, true, Some(&mut paste), Some(&mut yes)).await.unwrap();
    assert!(asked.contains(&format!("fingerprint {approver}")) && asked.ends_with("Is that your desktop?"), "{asked}");
    assert!(!asked.contains(grant.trim()), "the prompt never shows the grant");
    assert_eq!(comp.room, room);
    let state = ligus_runtime::computer_link::LinkState::load(&comp_dir).unwrap();
    assert_eq!(state.pending_status(ligus_core::flow::now_ms()), None, "nothing waits once linked");

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
    let e = devices_cmd::cmd_redeem(&mut again, None, Some(out.display().to_string()), Some(approver), true, None, None).await.unwrap_err();
    assert!(e.to_string().contains("doesn't answer a request"), "{e}");
    let e = devices_cmd::refuse_if_linked(&comp, "accept invites").unwrap_err();
    assert_eq!(crate::problem::classify(&e).0, crate::problem::EXIT_REFUSED);
    devices_cmd::cmd_list(&comp, true).unwrap();
    // Anywhere else it would be its old identity under the person's device
    // key: refused.
    let mut elsewhere = comp.clone();
    elsewhere.room = "another-room".into();
    let e = open_service(&elsewhere).err().unwrap();
    assert_eq!(crate::problem::classify(&e).0, crate::problem::EXIT_REFUSED, "{e}");
}
