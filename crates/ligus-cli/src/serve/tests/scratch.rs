//! Scratch repositories on the box, from the phone: create and bind, the
//! turn runs there on the chat's branch, keep, publish (fake GitHub), the
//! cleanup with its notice, the path checks, the limits, and a desktop's
//! scratch repository seen from the box (mixed hosts).

use super::*;

const DAY: i64 = 86_400;

async fn scratch_for(h: &Harness, chat: &str) -> Value {
    h.send(
        reqwest::Method::POST,
        "/v1/repos/scratch",
        Some(json!({ "chatId": chat, "name": "Parser spike" })),
    )
    .await
}

async fn new_chat(h: &Harness, title: &str) -> String {
    let c = h
        .send(
            reqwest::Method::POST,
            "/v1/chats",
            Some(json!({ "title": title })),
        )
        .await;
    c["chat"]["id"].as_str().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn phone_starts_a_scratch_repo_and_the_turn_runs_in_it() {
    let (endpoint, model) = spawn_model(vec![vec![delta(json!({"content": "ok"}))]]).await;
    let mut h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;

    // The box owner's (like a clone): a phone that isn't the owner's, or
    // the bearer over the network, is refused.
    let chat = new_chat(&h, "spike").await;
    let (c, v) = status_of(
        &h,
        reqwest::Method::POST,
        "/v1/repos/scratch",
        Some(json!({ "chatId": chat })),
    )
    .await;
    assert_eq!(c, 403, "{v}");
    assert!(
        v["error"]
            .as_str()
            .unwrap_or("")
            .starts_with(super::super::repos_api::OWNER_REQUIRED),
        "{v}"
    );
    h.as_operator = true;

    let made = scratch_for(&h, &chat).await;
    let s = &made["scratch"];
    let folder = s["checkout"].as_str().unwrap().to_string();
    assert!(
        folder.starts_with("parser-spike-") && folder.len() == "parser-spike-".len() + 8,
        "{made}"
    );
    assert_eq!(s["state"], "live");
    assert_eq!(s["notice"], false);
    assert!(
        s["removeAt"].is_string(),
        "the cleanup is on by default: {s}"
    );
    assert_eq!(made["binding"]["repo"]["scratch"]["id"], s["id"], "{made}");
    assert_eq!(made["binding"]["repo"]["displayName"], "Parser spike");
    assert!(
        !made.to_string().contains(h._tmp.path().to_str().unwrap()),
        "no host paths: {made}"
    );

    // Listed first among the box's checkouts, tagged scratch; the host says
    // it makes them.
    let list = h.get("/v1/repos").await;
    assert_eq!(list["local"][0]["name"], json!(folder), "{list}");
    assert!(list["local"][0]["scratch"].is_object());
    assert_eq!(list["local"][0]["defaultBranch"], "main");
    assert_eq!(list["scratch"]["cleanupDays"], 14);
    assert_eq!(list["scratch"]["live"], 1);
    let details = h.get(&format!("/v1/chats/{chat}/details")).await;
    assert!(details["repo"]["repo"]["scratch"].is_object(), "{details}");
    let msgs = h
        .get(&format!("/v1/chats/{chat}/messages"))
        .await
        .to_string();
    assert!(msgs.contains("new scratch repository"), "{msgs}");

    // A turn runs in it, on the chat's own branch (proposals as usual).
    let host = this_host(&h).await;
    bind_here(&h, &host, &[h.agent_id]).await;
    h.send(
        reqwest::Method::POST,
        &format!("/v1/chats/{chat}/messages"),
        Some(json!({ "body": "@coder hi" })),
    )
    .await;
    let mut system = String::new();
    for _ in 0..300 {
        if let Some(b) = model.bodies.lock().unwrap().first() {
            system = b["messages"][0]["content"]
                .as_str()
                .unwrap_or("")
                .to_string();
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        system.contains(&format!("`{folder}`")) && system.contains(&format!("ligus/chat/{chat}")),
        "{system}"
    );

    // Another chat picks the same scratch repo by its folder name.
    let chat2 = new_chat(&h, "two").await;
    let b = h
        .send(
            reqwest::Method::PUT,
            &format!("/v1/chats/{chat2}/repo"),
            Some(json!({ "repo": folder })),
        )
        .await;
    assert!(b["repo"]["scratch"].is_object(), "{b}");
    let all = h.get("/v1/repos/scratch").await;
    assert_eq!(all["scratch"][0]["chats"], 2, "{all}");
    h.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn keep_then_publish_only_with_the_named_confirmation() {
    let (endpoint, _) = spawn_model(vec![]).await;
    let mut h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    h.as_operator = true;
    let remotes = h._tmp.path().join("remotes");
    std::fs::create_dir_all(&remotes).unwrap();
    let fake = Arc::new(FakeGitHub::new(&remotes, true));
    use_fake(&h, fake.clone());
    let chat = new_chat(&h, "keep").await;
    let made = scratch_for(&h, &chat).await;
    let id = made["scratch"]["id"].as_str().unwrap().to_string();

    // Publishing a live (unkept) one is refused.
    let (c, _) = status_of(
        &h,
        reqwest::Method::POST,
        &format!("/v1/repos/scratch/{id}/publish"),
        Some(json!({ "owner": "acme", "name": "spike", "confirm": "acme/spike" })),
    )
    .await;
    assert_eq!(c, 409);

    let kept = h
        .send(
            reqwest::Method::POST,
            &format!("/v1/repos/scratch/{id}/keep"),
            Some(json!({ "name": "spike" })),
        )
        .await;
    assert_eq!(kept["scratch"]["state"], "kept", "{kept}");
    assert_eq!(kept["scratch"]["checkout"], "spike");
    let b = h.get(&format!("/v1/chats/{chat}/repo")).await;
    assert_eq!(b["repo"]["name"], "spike", "the chat follows it: {b}");
    assert!(b["repo"].get("scratch").is_none(), "no longer scratch: {b}");
    let list = h.get("/v1/repos").await;
    let row = list["local"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "spike")
        .unwrap()
        .clone();
    assert!(
        row.get("scratch").is_none(),
        "an ordinary checkout of the box now: {row}"
    );
    // Keeping twice: not live any more.
    let (c, _) = status_of(
        &h,
        reqwest::Method::POST,
        &format!("/v1/repos/scratch/{id}/keep"),
        None,
    )
    .await;
    assert_eq!(c, 404);

    // No consent, a different one, or a name that isn't one: nothing made.
    for body in [
        json!({ "owner": "acme", "name": "spike" }),
        json!({ "owner": "acme", "name": "spike", "confirm": "acme/other" }),
        json!({ "owner": "acme", "name": "../spike", "confirm": "acme/../spike" }),
    ] {
        let (c, v) = status_of(
            &h,
            reqwest::Method::POST,
            &format!("/v1/repos/scratch/{id}/publish"),
            Some(body.clone()),
        )
        .await;
        assert_eq!(c, 409, "{body}: {v}");
    }
    assert!(
        fake.created.lock().unwrap().is_empty(),
        "never created without the confirmation"
    );
    let r = h
        .send(
            reqwest::Method::POST,
            &format!("/v1/repos/scratch/{id}/publish"),
            Some(json!({ "owner": "acme", "name": "spike", "confirm": "acme/spike" })),
        )
        .await;
    assert_eq!(r["url"], "https://github.com/acme/spike", "{r}");
    assert_eq!(r["private"], true);
    assert_eq!(
        *fake.created.lock().unwrap(),
        vec!["acme/spike".to_string()]
    );
    let pushed = crate::repos::git_out(&remotes.join("acme/spike.git"), &["rev-parse", "main"]);
    assert!(pushed.is_some_and(|s| s.len() == 40), "main was pushed");
    h.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cleanup_notices_then_leaves_the_chat_with_no_repository() {
    let (endpoint, _) = spawn_model(vec![]).await;
    let mut h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    h.as_operator = true;
    let chat = new_chat(&h, "old").await;
    let made = scratch_for(&h, &chat).await;
    let id: Uuid = made["scratch"]["id"].as_str().unwrap().parse().unwrap();
    let path = {
        let svc = h.handle.shared.lock();
        ligus_runtime::repos::scratch::list(svc.store(), h.handle.shared.ws)
            .into_iter()
            .find(|e| e.id == id)
            .unwrap()
            .path
    };
    let now = ligus_runtime::repos::scratch::now();
    // Inside the notice window: one note in the chat, the listing says so.
    let r = super::super::scratch_api::sweep_once(&h.handle.shared, now + 12 * DAY + 60);
    assert_eq!(r.noticed, vec![id]);
    let msgs = h
        .get(&format!("/v1/chats/{chat}/messages"))
        .await
        .to_string();
    assert!(msgs.contains("will be removed on"), "{msgs}");
    assert!(
        super::super::scratch_api::sweep_once(&h.handle.shared, now + 12 * DAY + 120)
            .noticed
            .is_empty(),
        "once"
    );
    // Past it: the folder is gone, the chat has no repository (not the
    // box's workspace default), and says why.
    let r = super::super::scratch_api::sweep_once(&h.handle.shared, now + 15 * DAY);
    assert_eq!(r.removed, vec![id], "{r:?}");
    assert!(!path.exists());
    let b = h.get(&format!("/v1/chats/{chat}/repo")).await;
    assert!(b["repo"].is_null(), "No repository: {b}");
    let msgs = h
        .get(&format!("/v1/chats/{chat}/messages"))
        .await
        .to_string();
    assert!(msgs.contains("was removed"), "{msgs}");
    let all = h.get("/v1/repos/scratch").await;
    assert_eq!(all["scratch"][0]["state"], "removed");
    // The workspace's own checkout (the harness's repo) is untouched.
    assert!(h.get("/v1/repos").await["workspaceDefault"].is_string());

    // Off: nothing is removed.
    h.send(
        reqwest::Method::PUT,
        "/v1/repos/scratch/settings",
        Some(json!({ "cleanupDays": 0 })),
    )
    .await;
    let chat2 = new_chat(&h, "kept around").await;
    scratch_for(&h, &chat2).await;
    let r = super::super::scratch_api::sweep_once(&h.handle.shared, now + 400 * DAY);
    assert!(r.removed.is_empty() && r.noticed.is_empty());
    let (c, _) = status_of(
        &h,
        reqwest::Method::PUT,
        "/v1/repos/scratch/settings",
        Some(json!({ "cleanupDays": 999 })),
    )
    .await;
    assert_eq!(c, 409);
    h.handle.shutdown();
}

/// Security pass: deletion never leaves the scratch root (a swapped
/// symlink, a forged id), and creation is capped.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removal_path_checks_and_limits() {
    let (endpoint, _) = spawn_model(vec![]).await;
    let mut h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    h.as_operator = true;
    let (c, _) = status_of(
        &h,
        reqwest::Method::DELETE,
        &format!("/v1/repos/scratch/{}", Uuid::new_v4()),
        None,
    )
    .await;
    assert_eq!(c, 404);
    let (c, _) = status_of(
        &h,
        reqwest::Method::DELETE,
        "/v1/repos/scratch/..%2F..%2Fetc",
        None,
    )
    .await;
    assert!(c == 400 || c == 404, "{c}");

    let chat = new_chat(&h, "swap").await;
    let made = scratch_for(&h, &chat).await;
    let id: Uuid = made["scratch"]["id"].as_str().unwrap().parse().unwrap();
    let entry = {
        let svc = h.handle.shared.lock();
        ligus_runtime::repos::scratch::list(svc.store(), h.handle.shared.ws)
            .into_iter()
            .find(|e| e.id == id)
            .unwrap()
    };
    let victim = h._tmp.path().join("precious");
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::write(victim.join("keep.txt"), "x").unwrap();
    #[cfg(unix)]
    {
        std::fs::remove_dir_all(&entry.path).unwrap();
        std::os::unix::fs::symlink(&victim, &entry.path).unwrap();
        let (c, v) = status_of(
            &h,
            reqwest::Method::DELETE,
            &format!("/v1/repos/scratch/{id}"),
            None,
        )
        .await;
        assert_eq!(c, 409, "{v}");
        assert!(
            victim.join("keep.txt").exists(),
            "the symlink's target is untouched"
        );
        let r = super::super::scratch_api::sweep_once(
            &h.handle.shared,
            ligus_runtime::repos::scratch::now() + 60 * DAY,
        );
        assert_eq!(r.failed.len(), 1, "the cleanup refuses it too: {r:?}");
        assert!(victim.join("keep.txt").exists());
    }

    // Limits: per host.
    {
        let svc = h.handle.shared.lock();
        ligus_runtime::repos::scratch::set_limits(svc.store(), Some(2), None).unwrap();
    }
    let chat2 = new_chat(&h, "two").await;
    scratch_for(&h, &chat2).await;
    let (c, v) = status_of(
        &h,
        reqwest::Method::POST,
        "/v1/repos/scratch",
        Some(json!({ "chatId": chat2 })),
    )
    .await;
    assert_eq!(c, 429, "{v}");
    let (c, _) = status_of(
        &h,
        reqwest::Method::POST,
        "/v1/repos/scratch",
        Some(json!({ "name": "x".repeat(200) })),
    )
    .await;
    assert_eq!(c, 400);
    h.handle.shutdown();
}

/// Mixed hosts: a scratch repository the desktop made is the desktop's; the
/// box lists it with its tag and never runs a turn there without a checkout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_desktop_scratch_repo_seen_from_the_box() {
    let (endpoint, model) = spawn_model(vec![vec![delta(json!({"content": "ok"}))]]).await;
    let mut h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    h.as_operator = true;
    let ws = h.handle.shared.ws;
    let root = h._tmp.path().join("desktop-scratch");
    let repo_id = {
        let mut desk = open_service(&h.desktop).unwrap();
        let st = ligus_runtime::repos::scratch::settings(desk.store(), &root);
        let (_, repo) = ligus_runtime::repos::scratch_host::create(
            &mut desk,
            ws,
            None,
            &st,
            &[],
            "Desk spike",
            "laptop",
        )
        .unwrap();
        repo.id
    };
    let mut seen = Value::Null;
    for _ in 0..200 {
        sync_once(&h.desktop).await.unwrap();
        h.handle.shared.sync_kick.notify_one();
        let v = h.get("/v1/workspace-repos").await;
        if let Some(r) = v["repos"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == repo_id.to_string())
        {
            seen = r.clone();
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(seen["scratch"], true, "{seen}");
    assert_eq!(
        seen["here"]["state"], "missing",
        "the box has no checkout of it"
    );
    let chat = new_chat(&h, "desk").await;
    let r = h
        .send(
            reqwest::Method::PUT,
            &format!("/v1/chats/{chat}/workspace-repo"),
            Some(json!({ "repoId": repo_id })),
        )
        .await;
    assert_eq!(r["scratch"], true, "{r}");
    let host = this_host(&h).await;
    bind_here(&h, &host, &[h.agent_id]).await;
    let before = model.bodies.lock().unwrap().len();
    h.send(
        reqwest::Method::POST,
        &format!("/v1/chats/{chat}/messages"),
        Some(json!({ "body": "@coder hi" })),
    )
    .await;
    let mut note = false;
    for _ in 0..100 {
        if h.get(&format!("/v1/chats/{chat}/messages"))
            .await
            .to_string()
            .contains("No checkout of Desk spike")
        {
            note = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(note, "refused with the reason");
    assert_eq!(model.bodies.lock().unwrap().len(), before, "no model call");
    // And the box never removes the desktop's folder: it isn't in the box's registry.
    let r = super::super::scratch_api::sweep_once(
        &h.handle.shared,
        ligus_runtime::repos::scratch::now() + 400 * DAY,
    );
    assert!(r.removed.is_empty());
    h.handle.shutdown();
}
