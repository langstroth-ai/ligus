//! Registered scratch repositories on a person's own device: create, bind,
//! the turn gate, keep, cleanup, and publish with a fake code host.

use std::sync::{Arc, Mutex};

use super::*;
use crate::repos::TurnCheckout;
use crate::EventStore;
use ligus_core::{ActorIdentity, ActorKind, SigningKeypair};

struct Env {
    t: tempfile::TempDir,
    svc: ChatService,
    ws: Uuid,
    settings: Settings,
}

fn env() -> Env {
    let account = Uuid::new_v4();
    let mut author = ActorIdentity::new(account.to_string(), "Ana", ActorKind::Human);
    author.account_id = Some(account);
    let mut svc = ChatService::new(
        EventStore::open_in_memory().unwrap(),
        SigningKeypair::generate().unwrap(),
        SigningKeypair::generate().unwrap(),
        author,
    );
    svc.enable_test_presence();
    let ws = Uuid::new_v4();
    svc.ensure_workspace_config(ws).unwrap();
    let t = tempfile::tempdir().unwrap();
    let settings = scratch::settings(svc.store(), &t.path().join("data/scratch"));
    Env {
        t,
        svc,
        ws,
        settings,
    }
}

fn gate(e: &Env, chat: Uuid) -> TurnCheckout {
    let s = e.svc.load(chat).unwrap().unwrap();
    let roster = e.svc.workspace_roster(e.ws).unwrap();
    crate::repos::turn_checkout(
        e.svc.store(),
        e.svc.device_id(),
        "laptop",
        &s,
        &roster,
        &Default::default(),
    )
}

#[test]
fn create_registers_binds_and_sets_the_chat() {
    let mut e = env();
    let chat = e.svc.create_chat("try", e.ws, "anthropic").unwrap();
    let (s, repo) = create(
        &mut e.svc,
        e.ws,
        Some(chat.id),
        &e.settings.clone(),
        &[],
        "Parser spike",
        "laptop",
    )
    .unwrap();
    assert!(repo.scratch, "flagged scratch on the workspace entry");
    assert_eq!(repo.canonical, format!("local:{}", s.root_commit));
    assert_eq!(repo.default_branch, "main");
    assert_eq!(s.repo_id, Some(repo.id));
    assert_eq!(s.chats, vec![chat.id]);
    assert_eq!(s.created_by, my_account(&e.svc));
    let roster = e.svc.workspace_roster(e.ws).unwrap();
    assert!(roster.repos[&repo.id].repo.scratch);
    let c = e.svc.load(chat.id).unwrap().unwrap();
    assert_eq!(c.repo.unwrap().repo_id, Some(repo.id));
    // The usual turn gate: ready, in this device's checkout, and the chat
    // branch is cut and pinned as for any repository.
    let TurnCheckout::Ready(r) = gate(&e, chat.id) else {
        panic!("{:?}", gate(&e, chat.id))
    };
    assert_eq!(r.path, s.path);
    let session = e.svc.load(chat.id).unwrap().unwrap();
    let p = crate::repos::prepare_chat(&r, &session, "laptop").unwrap();
    assert!(p.pin.is_some(), "the first turn pins a base");
    assert!(
        p.chat_worktree.starts_with(&s.path),
        "the chat's worktree lives inside the scratch repo"
    );

    // Another chat can use it too.
    let chat2 = e.svc.create_chat("two", e.ws, "anthropic").unwrap();
    use_in_chat(&mut e.svc, e.ws, s.id, chat2.id).unwrap();
    assert!(matches!(gate(&e, chat2.id), TurnCheckout::Ready(_)));
}

#[test]
fn a_member_who_cannot_add_repos_is_refused_before_anything_is_made() {
    let e = env();
    // A device whose person isn't an Admin of the workspace (here: not in
    // its roster at all).
    let mut stranger = ChatService::new(
        EventStore::open_in_memory().unwrap(),
        SigningKeypair::generate().unwrap(),
        SigningKeypair::generate().unwrap(),
        ActorIdentity::new("x", "X", ActorKind::Human),
    );
    let err = create(&mut stranger, e.ws, None, &e.settings, &[], "x", "box").unwrap_err();
    assert!(matches!(err, HostError::Refused(_)), "{err}");
    assert!(
        !e.settings.root.exists() || std::fs::read_dir(&e.settings.root).unwrap().count() == 0,
        "no folder was made"
    );
}

#[test]
fn keep_moves_rebinds_and_drops_the_flag() {
    let mut e = env();
    let chat = e.svc.create_chat("try", e.ws, "anthropic").unwrap();
    let (s, repo) = create(
        &mut e.svc,
        e.ws,
        Some(chat.id),
        &e.settings.clone(),
        &[],
        "Keeper",
        "laptop",
    )
    .unwrap();
    let projects = e.t.path().join("projects");
    std::fs::create_dir_all(&projects).unwrap();
    let (k, dropped) = keep(
        &mut e.svc,
        e.ws,
        s.id,
        &projects.join("keeper"),
        Some("Keeper app"),
        &[],
        "laptop",
    )
    .unwrap();
    assert!(dropped);
    assert_eq!(k.state, scratch::ScratchState::Kept);
    assert!(!s.path.exists());
    let roster = e.svc.workspace_roster(e.ws).unwrap();
    assert!(
        !roster.repos[&repo.id].repo.scratch,
        "the workspace no longer calls it scratch"
    );
    assert_eq!(roster.repos[&repo.id].repo.display_name, "Keeper app");
    let TurnCheckout::Ready(r) = gate(&e, chat.id) else {
        panic!()
    };
    assert_eq!(r.path, k.path, "the chat follows it to the new folder");
    // A kept repo is never cleaned up.
    let report = sweep(&mut e.svc, e.ws, &e.settings, "laptop", i64::MAX / 4);
    assert!(report.removed.is_empty());
    assert!(k.path.exists());
}

#[test]
fn cleanup_notices_then_leaves_the_chat_with_no_repository() {
    let mut e = env();
    let chat = e.svc.create_chat("try", e.ws, "anthropic").unwrap();
    let (s, repo) = create(
        &mut e.svc,
        e.ws,
        Some(chat.id),
        &e.settings.clone(),
        &[],
        "Old",
        "laptop",
    )
    .unwrap();
    let before = e.svc.load(chat.id).unwrap().unwrap().messages.len();
    let act = scratch::activity(&s, scratch::now());
    let r1 = sweep(
        &mut e.svc,
        e.ws,
        &e.settings,
        "laptop",
        act + 12 * 86_400 + 5,
    );
    assert_eq!(r1.noticed, vec![s.id]);
    assert!(r1.removed.is_empty());
    let msgs = e.svc.load(chat.id).unwrap().unwrap().messages;
    assert!(
        msgs.iter().any(|m| m.body.contains("will be removed on")),
        "the notice is in the chat"
    );
    let again = sweep(
        &mut e.svc,
        e.ws,
        &e.settings,
        "laptop",
        act + 12 * 86_400 + 10,
    );
    assert!(again.noticed.is_empty(), "once");
    let r2 = sweep(&mut e.svc, e.ws, &e.settings, "laptop", act + 15 * 86_400);
    assert_eq!(r2.removed, vec![s.id], "{r2:?}");
    assert!(!s.path.exists());
    let c = e.svc.load(chat.id).unwrap().unwrap();
    assert_eq!(c.repo.and_then(|r| r.repo_id), None, "No repository");
    assert!(c.messages.iter().any(|m| m.body.contains("was removed")));
    assert!(
        c.messages.len() >= before + 2,
        "history stays; the notes are added"
    );
    assert!(matches!(gate(&e, chat.id), TurnCheckout::NoRepo));
    // The workspace entry stays (its removal is an Admin's, with a passkey).
    assert!(e.svc.workspace_roster(e.ws).unwrap().repo_live(repo.id));
    assert!(
        e.svc
            .store()
            .repo_mappings(e.ws)
            .unwrap()
            .iter()
            .all(|m| m.repo_id != repo.id),
        "unbound here"
    );
}

#[test]
fn removed_scratch_repos_give_their_workspace_slot_back() {
    let mut e = env();
    let chat = e.svc.create_chat("try", e.ws, "anthropic").unwrap();
    let (s, repo) = create(
        &mut e.svc,
        e.ws,
        Some(chat.id),
        &e.settings.clone(),
        &[],
        "Gone",
        "laptop",
    )
    .unwrap();
    let act = scratch::activity(&s, scratch::now());
    sweep(&mut e.svc, e.ws, &e.settings, "laptop", act + 20 * 86_400);
    assert!(
        e.svc.workspace_roster(e.ws).unwrap().repo_live(repo.id),
        "the background cleanup signs nothing an Admin must confirm"
    );
    assert_eq!(scratch::retire_pending(e.svc.store(), e.ws).len(), 1);
    // An Admin's next scratch action retires it.
    create(
        &mut e.svc,
        e.ws,
        None,
        &e.settings.clone(),
        &[],
        "Next",
        "laptop",
    )
    .unwrap();
    assert!(!e.svc.workspace_roster(e.ws).unwrap().repo_live(repo.id));
    assert!(scratch::retire_pending(e.svc.store(), e.ws).is_empty());
}

// ── Publish with a fake code host ────────────────────────────────────────

struct Creds(bool);

#[async_trait::async_trait]
impl ligus_forge::CredentialSource for Creds {
    fn source(&self) -> ligus_forge::AuthSource {
        ligus_forge::AuthSource::None
    }
    async fn token(
        &self,
        _: ligus_forge::Purpose,
    ) -> ligus_forge::Result<Option<ligus_forge::Secret>> {
        Ok(None)
    }
    fn has_write(&self) -> bool {
        self.0
    }
    fn git_helper(&self) -> Option<String> {
        None
    }
}

struct FakeHost {
    id: ligus_forge::ForgeId,
    creds: Arc<dyn ligus_forge::CredentialSource>,
    remotes: std::path::PathBuf,
    created: Mutex<Vec<(String, String)>>,
}

fn nope<T>() -> ligus_forge::Result<T> {
    Err(ligus_forge::ForgeError::Unsupported("fake".into()))
}

#[async_trait::async_trait]
impl ligus_forge::ForgeReader for FakeHost {
    fn kind(&self) -> ligus_forge::ForgeKind {
        ligus_forge::ForgeKind::GitHub
    }
    fn id(&self) -> &ligus_forge::ForgeId {
        &self.id
    }
    async fn list_change_requests(
        &self,
        _: &ligus_forge::RepoRef,
        _: ligus_forge::StateFilter,
        _: u32,
    ) -> ligus_forge::Result<Vec<ligus_forge::ChangeRequest>> {
        nope()
    }
    async fn change_request(
        &self,
        _: &ligus_forge::RepoRef,
        _: &str,
    ) -> ligus_forge::Result<ligus_forge::ChangeRequest> {
        nope()
    }
    async fn review_comments(
        &self,
        _: &ligus_forge::RepoRef,
        _: &str,
    ) -> ligus_forge::Result<Vec<ligus_forge::ReviewComment>> {
        nope()
    }
    async fn issues(
        &self,
        _: &ligus_forge::RepoRef,
        _: ligus_forge::StateFilter,
        _: u32,
    ) -> ligus_forge::Result<Vec<ligus_forge::Issue>> {
        nope()
    }
    async fn issue(
        &self,
        _: &ligus_forge::RepoRef,
        _: &str,
    ) -> ligus_forge::Result<ligus_forge::Issue> {
        nope()
    }
    async fn ci_status(
        &self,
        _: &ligus_forge::RepoRef,
        _: &str,
    ) -> ligus_forge::Result<ligus_forge::CiStatus> {
        nope()
    }
    async fn file_at(
        &self,
        _: &ligus_forge::RepoRef,
        _: &str,
        _: &str,
    ) -> ligus_forge::Result<ligus_forge::FileAtRef> {
        nope()
    }
}

#[async_trait::async_trait]
impl ligus_forge::Forge for FakeHost {
    fn web_base(&self) -> &str {
        "https://github.com"
    }
    fn capabilities(&self) -> ligus_forge::ForgeCaps {
        ligus_forge::ForgeCaps {
            draft_cr: true,
            fork_api: true,
            ci_status: true,
            review_threads: true,
            issues: true,
            api_open_cr: true,
            device_flow: true,
        }
    }
    fn creds(&self) -> &Arc<dyn ligus_forge::CredentialSource> {
        &self.creds
    }
    async fn whoami(&self) -> ligus_forge::Result<Option<ligus_forge::ForgeUser>> {
        Ok(Some(ligus_forge::ForgeUser {
            id: "1".into(),
            login: "ana".into(),
            name: String::new(),
        }))
    }
    async fn list_repos(
        &self,
        _: &ligus_forge::RepoQuery,
        _: &str,
    ) -> ligus_forge::Result<Vec<ligus_forge::RepoSummary>> {
        Ok(vec![])
    }
    async fn owners(&self, login: &str) -> ligus_forge::Result<Vec<String>> {
        Ok(vec![login.into()])
    }
    async fn repo(
        &self,
        _: &ligus_forge::RepoRef,
    ) -> ligus_forge::Result<Option<ligus_forge::RepoMeta>> {
        Ok(None)
    }
    async fn create_fork(
        &self,
        _: &ligus_forge::RepoRef,
    ) -> ligus_forge::Result<ligus_forge::RepoRef> {
        panic!("never")
    }
    async fn create_private_repo(
        &self,
        owner: &str,
        _login: &str,
        name: &str,
    ) -> ligus_forge::Result<ligus_forge::RepoRef> {
        self.created
            .lock()
            .unwrap()
            .push((owner.into(), name.into()));
        let bare = self.remotes.join(owner).join(format!("{name}.git"));
        std::fs::create_dir_all(&bare).unwrap();
        crate::repos::git::git(&bare, &["init", "--bare", "-q"]).unwrap();
        Ok(ligus_forge::RepoRef::new(owner, name))
    }
    async fn open_change_request(
        &self,
        _: &ligus_forge::ChangeRequestDraft,
    ) -> ligus_forge::Result<ligus_forge::ChangeRequestOutcome> {
        nope()
    }
    fn clone_url(&self, r: &ligus_forge::RepoRef, _: ligus_forge::Transport) -> String {
        format!(
            "file://{}/{}/{}.git",
            self.remotes.display(),
            r.owner,
            r.name
        )
    }
    fn git_auth_args(&self) -> Vec<String> {
        Vec::new()
    }
}

#[tokio::test]
async fn publish_needs_the_exact_confirmation_and_pushes_privately() {
    let mut e = env();
    let chat = e.svc.create_chat("try", e.ws, "anthropic").unwrap();
    let (s, _) = create(
        &mut e.svc,
        e.ws,
        Some(chat.id),
        &e.settings.clone(),
        &[],
        "Pub",
        "laptop",
    )
    .unwrap();
    let projects = e.t.path().join("projects");
    std::fs::create_dir_all(&projects).unwrap();
    let (k, _) = keep(
        &mut e.svc,
        e.ws,
        s.id,
        &projects.join("pub"),
        None,
        &[],
        "laptop",
    )
    .unwrap();
    let host = FakeHost {
        id: ligus_forge::ForgeId::github_com(),
        creds: Arc::new(Creds(true)),
        remotes: e.t.path().join("remotes"),
        created: Mutex::new(vec![]),
    };
    // No confirmation, or a different one: nothing is created.
    for confirm in ["", "ana/other", "someone/pub"] {
        assert!(
            publish(&host, &k, "ana", "pub", confirm).await.is_err(),
            "{confirm}"
        );
    }
    assert!(publish(&host, &k, "ana", "../x", "ana/../x").await.is_err());
    assert!(
        host.created.lock().unwrap().is_empty(),
        "never without consent"
    );
    // Signed out: refused before any call.
    let signed_out = FakeHost {
        creds: Arc::new(Creds(false)),
        created: Mutex::new(vec![]),
        id: host.id.clone(),
        remotes: host.remotes.clone(),
    };
    assert!(publish(&signed_out, &k, "ana", "pub", "ana/pub")
        .await
        .is_err());
    assert!(signed_out.created.lock().unwrap().is_empty());

    // A config an agent could have planted (a helper command, a rewrite):
    // refused before anything is created.
    for (key, val) in [
        ("credential.helper", "!touch /tmp/pwned"),
        (
            "url.https://evil.example/.pushInsteadOf",
            "https://github.com/",
        ),
        ("include.path", "/tmp/x"),
    ] {
        crate::repos::git::git(&k.path, &["config", key, val]).unwrap();
        let err = publish(&host, &k, "ana", "pub", "ana/pub")
            .await
            .unwrap_err();
        assert!(err.contains("git config"), "{key}: {err}");
        crate::repos::git::git(&k.path, &["config", "--unset", key]).unwrap();
    }
    assert!(host.created.lock().unwrap().is_empty());

    // Only from the kept repository itself: a folder inside another
    // repository (git would find that one) is refused.
    let other = e.t.path().join("other");
    std::fs::create_dir_all(other.join("sub")).unwrap();
    crate::repos::git::git(&other, &["init", "-q"]).unwrap();
    let moved = ScratchRepo {
        path: other.join("sub"),
        ..k.clone()
    };
    assert!(publish(&host, &moved, "ana", "pub", "ana/pub")
        .await
        .unwrap_err()
        .contains("isn't the kept repository"));
    assert!(host.created.lock().unwrap().is_empty());

    let url = publish(&host, &k, "ana", "pub", "ana/pub").await.unwrap();
    assert_eq!(url, "https://github.com/ana/pub");
    assert_eq!(
        *host.created.lock().unwrap(),
        vec![("ana".to_string(), "pub".to_string())]
    );
    let bare = e.t.path().join("remotes/ana/pub.git");
    assert_eq!(
        crate::repos::git::git(&bare, &["rev-parse", "main"]).unwrap(),
        crate::repos::git::git(&k.path, &["rev-parse", "main"]).unwrap()
    );
    // Once it has an origin, it isn't published again.
    assert!(publish(&host, &k, "ana", "pub", "ana/pub").await.is_err());
    assert_eq!(host.created.lock().unwrap().len(), 1);
}
