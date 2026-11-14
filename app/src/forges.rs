//! Settings → Code hosts and the repo picker across forges, on the desktop
//! (IDENTITY-AND-FORGES 5, 6.2, 6.4).
//!
//! Connections live in `<app data>/forges.toml` (no tokens); tokens live in
//! this device's OS secret store (`forge:<id>:<purpose>`). Nothing here is
//! synced or sent to the relay. Kept out of `lib.rs`: the commands are
//! registered there, and `main.rs` runs [`maybe_run_git_credential`] so
//! this binary can be git's credential helper for its own clones.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use hive_forge::tokens::{sh_quote, AuthSource, CredentialSource, Purpose, TokenStore};
use hive_forge::{Connection, Connections, Forge, ForgeId, ForgeKind, RepoQuery, RepoRef, Transport};
use serde_json::{json, Value};
use tauri::State;

use crate::AppState;

fn store(data_dir: &Path) -> Arc<dyn TokenStore> {
    hive_runtime::forge_secrets::ForgeSecrets::for_data_dir(data_dir)
}

/// This binary as git's credential helper for a connection:
/// `'<exe>' forge git-credential '<id>' --data-dir '<dir>'` (git appends
/// the action). The token is never on the command line.
fn helper_command(id: &ForgeId, data_dir: &Path) -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!(
        "{} forge git-credential {} --data-dir {}",
        sh_quote(&exe.to_string_lossy()),
        sh_quote(id.as_str()),
        sh_quote(&data_dir.to_string_lossy())
    ))
}

fn credentials(c: &Connection, data_dir: &Path) -> Arc<dyn CredentialSource> {
    c.credentials_with_helper(store(data_dir), helper_command(&c.id, data_dir))
}

fn forge(c: &Connection, data_dir: &Path) -> Arc<dyn Forge> {
    c.build(credentials(c, data_dir))
}

/// `<exe> forge git-credential <id> --data-dir <dir> <action>`: answer git
/// from the secret store and exit. Run first thing in `main`.
pub fn maybe_run_git_credential() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 5 || args[0] != "forge" || args[1] != "git-credential" || args[3] != "--data-dir" {
        return;
    }
    let (id, dir, action) = (args[2].clone(), PathBuf::from(&args[4]), args.get(5).cloned().unwrap_or_else(|| "get".into()));
    let code = match run_helper(&id, &dir, &action) {
        Ok(()) => 0,
        Err(_) => 1,
    };
    std::process::exit(code);
}

fn run_helper(id: &str, dir: &Path, action: &str) -> Result<(), String> {
    use std::io::{Read, Write};
    if action != "get" {
        return Ok(());
    }
    let id = ForgeId::parse(id).map_err(|e| e.to_string())?;
    hive_runtime::secret_store::init_for_data_dir(dir, None);
    let mut input = String::new();
    std::io::stdin().take(64 * 1024).read_to_string(&mut input).map_err(|e| e.to_string())?;
    let all = Connections::load(dir);
    let Some(c) = all.get(&id) else { return Ok(()) };
    if !matches!(c.auth, AuthSource::Token | AuthSource::Oauth) {
        return Ok(());
    }
    let creds = c.credentials(store(dir), None);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
    let token = rt.block_on(creds.token(Purpose::Write)).ok().flatten();
    let user = hive_forge::tokens::git_username(c.kind, c.auth, c.login.as_deref());
    if let Some(a) = hive_forge::tokens::answer_git_credential(action, &input, &c.host(), &user, token.as_ref()) {
        std::io::stdout().write_all(a.as_bytes()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The chat's code host for the read-only agent tools: the workspace
/// folder's `origin` on one of this device's connections (and `upstream`
/// when it is a fork checkout).
pub(crate) fn binding_for_dir(data_dir: &Path, dir: &Path) -> Option<hive_runtime::ligus_mcp::forge_tools::ForgeBinding> {
    let out = |args: &[&str]| -> Option<String> {
        let o = hive_core::process_util::git().arg("-C").arg(dir).args(args).output().ok()?;
        o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let origin = out(&["remote", "get-url", "origin"])?;
    let all = Connections::load(data_dir);
    for c in &all.forges {
        let f = forge(c, data_dir);
        if let Some(repo) = f.repo_of_remote(&origin) {
            let upstream = out(&["remote", "get-url", "upstream"])
                .and_then(|u| f.repo_of_remote(&u))
                .filter(|u| !u.same(&repo));
            return Some(hive_runtime::ligus_mcp::forge_tools::ForgeBinding { reader: Arc::new(hive_forge::ReadOnly(f)), repo, upstream });
        }
    }
    None
}

// ── Commands ───────────────────────────────────────────────────────────────

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

async fn dto(c: &Connection, data_dir: &Path, check: bool) -> Value {
    let f = forge(c, data_dir);
    let present = store(data_dir).present(&c.id);
    let mut v = json!({
        "id": c.id.as_str(),
        "kind": c.kind.as_str(),
        "flavor": c.flavor,
        "displayName": c.display(),
        "baseUrl": c.base_url,
        "authSource": c.auth.as_str(),
        "hasRead": c.auth == AuthSource::Gh || present.contains(&Purpose::Read) || present.contains(&Purpose::Write),
        "hasWrite": f.has_write(),
        "user": c.login,
        "deviceFlow": f.capabilities().device_flow && hive_forge::device::endpoints(c).is_ok(),
        "openLabel": c.kind.open_label(),
        "holder": "this device",
    });
    if check {
        match tokio::time::timeout(Duration::from_secs(20), f.whoami()).await {
            Ok(Ok(Some(u))) => {
                v["available"] = json!(true);
                v["user"] = json!(u.login);
            }
            Ok(Ok(None)) => {
                v["available"] = json!(false);
                v["reason"] = json!(f.signed_out_reason());
            }
            Ok(Err(e)) => {
                v["available"] = json!(false);
                v["reason"] = json!(e.to_string());
            }
            Err(_) => {
                v["available"] = json!(false);
                v["reason"] = json!("the code host didn't answer in time");
            }
        }
    }
    v
}

/// Connections on this device (never a token). `check` signs in to each.
#[tauri::command]
pub async fn forges_list(state: State<'_, AppState>, check: Option<bool>) -> Result<Vec<Value>, String> {
    let dir = state.data_dir.clone();
    let mut out = Vec::new();
    for c in Connections::load(&dir).forges {
        out.push(dto(&c, &dir, check.unwrap_or(false)).await);
    }
    Ok(out)
}

/// Add a connection: `kind` is github, gitlab, forgejo, gitea or codeberg.
#[tauri::command]
pub async fn forges_add(state: State<'_, AppState>, kind: String, base_url: String, auth: Option<String>, client_id: Option<String>) -> Result<Value, String> {
    let lower = kind.trim().to_ascii_lowercase();
    let k = ForgeKind::parse(&lower).ok_or_else(|| format!("unknown code host kind '{kind}'"))?;
    let auth = match auth.as_deref() {
        Some("gh") => AuthSource::Gh,
        Some("none") => AuthSource::None,
        _ => AuthSource::Token,
    };
    let mut c = Connection::new(k, &base_url, auth).map_err(err)?;
    if c.flavor.is_none() && matches!(lower.as_str(), "forgejo" | "gitea" | "codeberg") {
        c.flavor = Some(lower);
    }
    c.client_id = client_id.map(|x| x.trim().to_string()).filter(|x| !x.is_empty());
    let dir = state.data_dir.clone();
    let mut all = Connections::load(&dir);
    if all.get(&c.id).is_some() {
        return Err(format!("{} is already connected", c.display()));
    }
    all.upsert(c.clone());
    all.save(&dir).map_err(err)?;
    Ok(dto(&c, &dir, false).await)
}

/// Remove a connection and its tokens.
#[tauri::command]
pub async fn forges_remove(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let id = ForgeId::parse(&id).map_err(err)?;
    let dir = state.data_dir.clone();
    let mut all = Connections::load(&dir);
    all.remove(&id);
    all.save(&dir).map_err(err)?;
    store(&dir).delete_all(&id).map_err(err)
}

/// Store a pasted token (read or write) in this device's secret store.
#[tauri::command]
pub async fn forges_set_token(state: State<'_, AppState>, id: String, purpose: String, token: String) -> Result<Value, String> {
    let id = ForgeId::parse(&id).map_err(err)?;
    let purpose = Purpose::parse(&purpose).ok_or("purpose is read or write")?;
    let t = token.trim();
    if t.is_empty() || t.len() > 4096 || t.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("that doesn't look like a token (one line, no spaces)".into());
    }
    let dir = state.data_dir.clone();
    let mut all = Connections::load(&dir);
    let c = all.get_mut(&id).ok_or("no such connection")?;
    if c.auth == AuthSource::Gh {
        return Err("this connection uses the gh login".into());
    }
    store(&dir).put(&id, purpose, &hive_forge::Secret::new(t)).map_err(err)?;
    if c.auth == AuthSource::None {
        c.auth = AuthSource::Token;
    }
    let c = c.clone();
    all.save(&dir).map_err(err)?;
    Ok(dto(&c, &dir, false).await)
}

/// Sign in to the connection and remember who as.
#[tauri::command]
pub async fn forges_test(state: State<'_, AppState>, id: String) -> Result<Value, String> {
    let id = ForgeId::parse(&id).map_err(err)?;
    let dir = state.data_dir.clone();
    let mut all = Connections::load(&dir);
    let c = all.get(&id).ok_or("no such connection")?.clone();
    let v = dto(&c, &dir, true).await;
    if let (Some(true), Some(user)) = (v["available"].as_bool(), v["user"].as_str()) {
        if let Some(x) = all.get_mut(&id) {
            x.login = Some(user.to_string());
        }
        all.save(&dir).map_err(err)?;
    }
    Ok(v)
}

struct DevicePending {
    id: ForgeId,
    state: &'static str,
    error: Option<String>,
    user_code: String,
    verification_uri: String,
    expires: Instant,
}

static DEVICE: LazyLock<Mutex<Option<DevicePending>>> = LazyLock::new(|| Mutex::new(None));

/// Start a device sign-in (GitHub App, GitLab app). The device code stays
/// in this process; the window shows the user code and opens the URI.
#[tauri::command]
pub async fn forges_device_start(state: State<'_, AppState>, id: String) -> Result<Value, String> {
    let id = ForgeId::parse(&id).map_err(err)?;
    let dir = state.data_dir.clone();
    let c = Connections::load(&dir).get(&id).ok_or("no such connection")?.clone();
    let ep = hive_forge::device::endpoints(&c).map_err(err)?;
    let start = hive_forge::device::start(&ep).await.map_err(err)?;
    *DEVICE.lock().unwrap_or_else(|e| e.into_inner()) = Some(DevicePending {
        id: id.clone(),
        state: "pending",
        error: None,
        user_code: start.user_code.clone(),
        verification_uri: start.verification_uri.clone(),
        expires: Instant::now() + Duration::from_secs(start.expires_in),
    });
    let out = json!({ "userCode": start.user_code, "verificationUri": start.verification_uri, "expiresIn": start.expires_in });
    tauri::async_runtime::spawn(async move {
        let set = |st: &'static str, e: Option<String>| {
            if let Some(p) = DEVICE.lock().unwrap_or_else(|e| e.into_inner()).as_mut().filter(|p| p.id == id) {
                p.state = st;
                p.error = e;
            }
        };
        let mut interval = start.interval;
        loop {
            tokio::time::sleep(Duration::from_secs(interval)).await;
            let expired = DEVICE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_none_or(|p| p.id != id || Instant::now() > p.expires);
            if expired {
                return set("expired", None);
            }
            match hive_forge::device::poll(&ep, &start.device_code).await {
                Ok(hive_forge::device::Poll::Pending) => {}
                Ok(hive_forge::device::Poll::SlowDown(i)) => interval = i,
                Ok(hive_forge::device::Poll::Denied) => return set("denied", None),
                Ok(hive_forge::device::Poll::Expired) => return set("expired", None),
                Ok(hive_forge::device::Poll::Done(t)) => {
                    let s = store(&dir);
                    let mut r = s.put(&id, Purpose::Write, &t.access).map_err(err);
                    if let (Ok(()), Some(rt)) = (&r, &t.refresh) {
                        r = s.put(&id, Purpose::Refresh, rt).map_err(err);
                    }
                    let mut all = Connections::load(&dir);
                    if let Some(c) = all.get_mut(&id) {
                        c.auth = AuthSource::Oauth;
                        c.token_expires_at = t.expires_in.map(|x| hive_forge::device::now_unix() + x);
                    }
                    let r = r.and_then(|()| all.save(&dir).map_err(err));
                    return match r {
                        Ok(()) => set("done", None),
                        Err(e) => set("error", Some(e)),
                    };
                }
                Err(e) => return set("error", Some(e.to_string())),
            }
        }
    });
    Ok(out)
}

#[tauri::command]
pub async fn forges_device_status() -> Result<Value, String> {
    let g = DEVICE.lock().unwrap_or_else(|e| e.into_inner());
    let p = g.as_ref().ok_or("no device sign-in in progress")?;
    Ok(json!({ "id": p.id.as_str(), "state": p.state, "error": p.error, "userCode": p.user_code, "verificationUri": p.verification_uri }))
}

/// One page of a connection's repositories, for the picker.
#[tauri::command]
pub async fn forges_repos(state: State<'_, AppState>, id: String, q: Option<String>, owner: Option<String>, page: Option<u32>) -> Result<Value, String> {
    let id = ForgeId::parse(&id).map_err(err)?;
    let dir = state.data_dir.clone();
    let c = Connections::load(&dir).get(&id).ok_or("no such connection")?.clone();
    let f = forge(&c, &dir);
    let me = f.whoami().await.map_err(err)?.ok_or_else(|| f.signed_out_reason().unwrap_or_else(|| "not signed in".into()))?;
    let query = RepoQuery {
        q: q.map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && s.len() <= 100),
        owner: owner.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        page: page.unwrap_or(1).clamp(1, 100),
        per_page: 30,
    };
    let owners = f.owners(&me.login).await.unwrap_or_else(|_| vec![me.login.clone()]);
    let repos = f.list_repos(&query, &me.login).await.map_err(err)?;
    Ok(json!({ "login": me.login, "owners": owners, "repos": repos, "hasMore": repos.len() >= 30 }))
}

/// Clone a repository (or, from a URL, any repository on a connected host)
/// into `parent/<name>`, through this device's credentials. Returns the
/// checkout path; the window then offers to use it as the workspace folder.
#[tauri::command]
pub async fn forges_clone(state: State<'_, AppState>, id: String, repo: String, parent: String) -> Result<String, String> {
    let id = ForgeId::parse(&id).map_err(err)?;
    let dir = state.data_dir.clone();
    let c = Connections::load(&dir).get(&id).ok_or("no such connection")?.clone();
    let spec = hive_forge::spec::parse_repo_spec(&repo, c.kind == ForgeKind::GitLab).map_err(err)?;
    if spec.host.as_deref().is_some_and(|h| !hive_forge::spec::same_host(h, &c.host())) {
        return Err(format!("that URL isn't on {}", c.display()));
    }
    let parent = PathBuf::from(parent.trim());
    if !parent.is_absolute() || !parent.is_dir() {
        return Err("pick an existing folder to clone into".into());
    }
    let dest = parent.join(&spec.repo.name);
    if dest.exists() {
        return Err(format!("{} already exists", dest.display()));
    }
    let f = forge(&c, &dir);
    let url = f.clone_url(&RepoRef::new(spec.repo.owner.clone(), spec.repo.name.clone()), Transport::Https);
    let auth = f.git_auth_args();
    let d = dest.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut cmd = hive_core::process_util::git();
        cmd.env("GIT_TERMINAL_PROMPT", "0").stdin(std::process::Stdio::null());
        cmd.args(&auth).args(["clone", "--no-recurse-submodules", "--", &url]).arg(&d);
        let o = cmd.output().map_err(err)?;
        if o.status.success() {
            Ok(d.to_string_lossy().into_owned())
        } else {
            let msg = String::from_utf8_lossy(&o.stderr).lines().filter(|l| !l.trim().is_empty()).last().unwrap_or("").to_string();
            Err(format!("git clone failed: {msg}"))
        }
    })
    .await
    .map_err(err)?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The desktop is git's helper with its own data dir; the token never
    /// appears in the helper command or the clone's git args.
    #[test]
    fn desktop_git_auth_never_carries_the_token() {
        let tmp = std::env::temp_dir().join(format!("ligus-forge-desk-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&tmp).unwrap();
        struct Rm(PathBuf);
        impl Drop for Rm {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = Rm(tmp);
        let dir = &dir;
        hive_runtime::secret_store::set_for_data_dir(&dir.0, Arc::new(hive_runtime::secret_store::File0600::new(&dir.0)));
        let c = Connection::new(ForgeKind::GitLab, "https://gitlab.example.com", AuthSource::Token).unwrap();
        store(&dir.0).put(&c.id, Purpose::Write, &hive_forge::Secret::new("glpat-desktop-0123456789")).unwrap();
        let args = forge(&c, &dir.0).git_auth_args();
        let joined = args.join(" ");
        assert!(joined.contains("forge git-credential 'gitlab:gitlab.example.com' --data-dir"), "{joined}");
        assert!(!joined.contains("glpat-desktop"));
    }
}
