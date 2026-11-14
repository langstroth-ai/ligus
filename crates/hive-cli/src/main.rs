//! `ligus` — a headless Ligus client.
//!
//! Runs the same runtime the desktop app does (`hive-core` + `hive-runtime`),
//! wired to flags/env instead of a GUI, so a cloud-hosted agent (or a script)
//! can connect to a relay, sync a workspace's E2EE event log, and read/write
//! signed events without the full application. This is the "worker" client the
//! design spec §12 (D21–D23) calls for; v0 proves the path end-to-end:
//! connect → sync → read → post.
//!
//! Config (all via env; a persistent data dir keeps a stable identity):
//!   LIGUS_DATA_DIR            where identity + hive.db live (default: $HOME/.ligus)
//!   LIGUS_RELAY_URL           relay base URL (needed for `sync`/`send --push`)
//!   LIGUS_RELAY_ACCESS_TOKEN  relay entitlement token (hrt1/hex), provisioned out-of-band
//!   LIGUS_WORKSPACE           relay room / workspace (default: "default")
//!   LIGUS_WORKSPACE_KEY       E2EE passphrase; omit for a plaintext relay
//!
//! Commands:
//!   ligus whoami              print this client's identity (account/device ids)
//!   ligus chats               list chats in the local store
//!   ligus tail <chat-id>      print a chat's transcript (add --watch to follow)
//!   ligus send <chat-id> <…>  post a message (add --push to sync it out)
//!   ligus sync                run one sync round with the relay (add --watch to loop)
//!   ligus serve               home host for the iOS app: sync + agent worker +
//!                            HTTP/SSE API on a tailnet address (see serve/)

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use hive_core::{
    workspace_config_session_id, HostKind, MessageRole, ModelProviderKind,
    Timestamp, WorkspaceAgent, WorkspaceCredential, WorkspaceHost, WorkspaceRuntime,
};
use hive_runtime::{
    parse_mentions, pending_mentions, resolve_workspace_credential, turns_for, ChatService,
    ChatTurn, EventStore, IdentityStore, RelayClient, ResolvedRuntime, SyncEngine,
};
use hive_runtime::secret_store::StoreKeyVault;
use uuid::Uuid;

mod auth;
mod forge_cmd;
mod mcp_config;
mod reply;
mod repos;
mod serve;
mod worker;

#[derive(Clone)]
struct Config {
    data_dir: PathBuf,
    relay_url: Option<String>,
    token: Option<String>,
    room: String,
    key: Option<String>,
    /// Epoch keys an invite delivered (B1 newcomer bootstrap).
    epoch_keys: Vec<hive_runtime::keyring::DeliveredKey>,
}

/// Env lookup (blank = unset; `LIGUS_*` names only, see `hive_core::compat`).
fn env_opt(k: &str) -> Option<String> {
    hive_core::compat::env_opt(k)
}

/// `~/.ligus`, after moving a former `~/.hive` there once (see
/// `hive_core::compat::home_data_dir`). Exits if that move fails, rather than
/// starting fresh with a new device identity.
fn default_home_data_dir(home: &std::path::Path) -> PathBuf {
    match hive_core::compat::home_data_dir(home) {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

/// Persisted connection config (written by `ligus enroll`), read as a fallback
/// under the env vars so a headless box needn't re-export them each run.
/// `config.toml` holds only `relay_url` and `room`; the relay token, the
/// workspace passphrase and delivered epoch keys live in the OS secret store
/// (B2: scope `cli`, see [`FileConfig::load`] and [`FileConfig::save`]).
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct FileConfig {
    #[serde(default)]
    relay_url: Option<String>,
    #[serde(default, skip_serializing)]
    token: Option<String>,
    #[serde(default)]
    room: Option<String>,
    #[serde(default, skip_serializing)]
    key: Option<String>,
    /// Epoch keys the invite delivered (B1).
    #[serde(default, skip_serializing)]
    ek: Vec<hive_runtime::keyring::DeliveredKey>,
}

fn config_path(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("config.toml")
}

fn cli_secret(name: &str) -> hive_runtime::secret_store::SecretId {
    hive_runtime::secret_store::SecretId::new("cli", name)
}

impl FileConfig {
    /// `config.toml`, with its secrets from the store. Secrets still written
    /// in the file in the clear are dropped and the file rewritten without
    /// them (2.0 starts from scratch: re-run `ligus enroll`).
    fn load(data_dir: &std::path::Path) -> Self {
        let mut f: FileConfig = std::fs::read_to_string(config_path(data_dir))
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default();
        if f.token.is_some() || f.key.is_some() || !f.ek.is_empty() {
            eprintln!("{} held plaintext secrets; removed them (re-run `ligus enroll`)", config_path(data_dir).display());
            f.token = None;
            f.key = None;
            f.ek.clear();
            if let Ok(text) = toml::to_string_pretty(&f) {
                let _ = serve::tunnel::write_private(&config_path(data_dir), text.as_bytes());
            }
        }
        let store = hive_runtime::secret_store::for_data_dir(data_dir);
        let get = |n: &str| store.get_string(&cli_secret(n)).ok().flatten().map(|z| z.to_string()).filter(|v| !v.is_empty());
        f.token = get("token");
        f.key = get("key");
        f.ek = get("ek").and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default();
        if let Some(t) = &f.token {
            hive_runtime::secret_redact::register(t);
        }
        if let Some(k) = &f.key {
            hive_runtime::secret_redact::register(k);
        }
        f
    }

    /// Write `config.toml` (owner-only, no secrets) and the secrets to the store.
    fn save(&self, data_dir: &std::path::Path) -> Result<()> {
        let store = hive_runtime::secret_store::for_data_dir(data_dir);
        let put = |n: &str, v: Option<&str>| -> Result<()> {
            match v.filter(|v| !v.is_empty()) {
                Some(v) => store.put(&cli_secret(n), v.as_bytes())?,
                None => store.delete(&cli_secret(n))?,
            }
            Ok(())
        };
        put("token", self.token.as_deref())?;
        put("key", self.key.as_deref())?;
        let ek = (!self.ek.is_empty()).then(|| serde_json::to_string(&self.ek)).transpose()?;
        put("ek", ek.as_deref())?;
        serve::tunnel::write_private(&config_path(data_dir), toml::to_string_pretty(self)?.as_bytes())?;
        Ok(())
    }
}

fn load_file_config(data_dir: &std::path::Path) -> FileConfig {
    FileConfig::load(data_dir)
}

fn config() -> Config {
    let data_dir = env_opt("LIGUS_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| env_opt("HOME").map(|h| default_home_data_dir(std::path::Path::new(&h))))
        .unwrap_or_else(|| PathBuf::from("./hive-data"));
    // B2: secrets come from the OS secret store from here on.
    hive_runtime::secret_store::init_for_data_dir(&data_dir, None);
    // Env wins; the saved config.toml (from `ligus enroll`) fills the rest.
    let f = load_file_config(&data_dir);
    Config {
        data_dir,
        relay_url: env_opt("LIGUS_RELAY_URL").or(f.relay_url),
        token: env_opt("LIGUS_RELAY_ACCESS_TOKEN").or(f.token),
        room: env_opt("LIGUS_WORKSPACE").or(f.room).unwrap_or_else(|| "default".into()),
        key: env_opt("LIGUS_WORKSPACE_KEY").or(f.key),
        epoch_keys: f.ek,
    }
}

/// Decode a `hivews1:` workspace invite → (relay_url, room, key, invite id,
/// delivered epoch keys). Mirrors the app's encoder (base64url(json) of the
/// connection; snake_case fields).
#[allow(clippy::type_complexity)]
fn decode_hivews_invite_full(
    invite: &str,
) -> Result<(String, String, Option<String>, Option<Uuid>, Vec<hive_runtime::keyring::DeliveredKey>, Option<(String, String)>)> {
    use base64::Engine;
    #[derive(serde::Deserialize)]
    struct Conn {
        #[serde(default)]
        relay_url: String,
        #[serde(default)]
        room: String,
        #[serde(default)]
        key: Option<String>,
        /// The single-use invite an Admin issued with this code.
        #[serde(default)]
        inv: Option<String>,
        /// Every authorized epoch key the inviter held (B1).
        #[serde(default)]
        ek: Vec<hive_runtime::keyring::DeliveredKey>,
        /// The founder and their key fingerprint (the pin keys are decided on).
        #[serde(default)]
        founder_actor_id: Option<String>,
        #[serde(default)]
        fk: Option<String>,
    }
    let body = invite
        .trim()
        .strip_prefix("hivews1:")
        .ok_or_else(|| anyhow!("not a Ligus workspace invite (expected hivews1:…)"))?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(body.trim().as_bytes())
        .context("invalid invite encoding")?;
    let c: Conn = serde_json::from_slice(&bytes).context("invalid invite payload")?;
    if c.room.trim().is_empty() {
        bail!("invite is missing a room");
    }
    let pin = c.founder_actor_id.zip(c.fk);
    Ok((c.relay_url, c.room, c.key, c.inv.and_then(|i| Uuid::parse_str(i.trim()).ok()), c.ek, pin))
}

/// `ligus enroll <hivews1:…> [--token <access-token>]` — one-command bootstrap:
/// decode the invite, persist the connection to config.toml, and (with --token)
/// wire the relay identity. After this a headless box just runs `ligus worker`.
fn cmd_enroll(cfg: &Config, invite: &str, token: Option<String>) -> Result<()> {
    let (relay_url, room, key, inv, ek, pin) = decode_hivews_invite_full(invite)?;
    std::fs::create_dir_all(&cfg.data_dir)?;
    let mut f = load_file_config(&cfg.data_dir);
    f.relay_url = Some(relay_url.clone());
    f.room = Some(room.clone());
    f.key = key;
    f.ek = ek;
    if let Some(t) = token {
        f.token = Some(t);
    }
    // The workspace E2EE key and the relay token go to the secret store; the
    // file (owner-only) keeps the relay and room.
    f.save(&cfg.data_dir)?;
    // Keep the invite this box redeems when it enrolls itself, and the
    // founder pin the invite carries (device-local; keys and rosters are
    // only decided on a pinned fold).
    let store = EventStore::open(cfg.data_dir.join("hive.db"))?;
    if let Some(inv) = inv {
        store.set_join_invite(uuid_of_room(&room), inv)?;
    }
    if let Some((founder, fk)) = pin {
        store.set_workspace_pin(uuid_of_room(&room), &founder, &fk)?;
    }
    println!("Enrolled workspace '{room}' at {relay_url}.");
    println!("Saved to {}.", config_path(&cfg.data_dir).display());
    if f.token.is_none() {
        println!("No access token set — for a membership-enforcing relay, add one:");
        println!("  ligus enroll <invite> --token <hrt1-token>   (or export LIGUS_RELAY_ACCESS_TOKEN)");
    }
    println!("Next: `ligus sync` then `ligus worker` to bring the agent online.");
    Ok(())
}

/// `ligus join <invite-code>` — redeem a relay invite code to self-enroll this
/// box's identity in the configured workspace at the invite's role.
async fn cmd_join(cfg: &Config, code: &str) -> Result<()> {
    let relay_url = cfg
        .relay_url
        .clone()
        .ok_or_else(|| anyhow!("no relay — run `ligus enroll <invite>` or set LIGUS_RELAY_URL first"))?;
    let relay = RelayClient::new(&relay_url)
        .with_auth(cfg.token.clone())
        .with_auth_cell(auth::cell());
    match relay.join_via_invite(&cfg.room, code.trim(), "").await? {
        Some(role) => {
            println!("Joined workspace '{}' as {role}. Run `ligus sync`.", cfg.room);
            Ok(())
        }
        None => bail!("invite refused (invalid, expired, revoked, or used up)"),
    }
}

/// Open the local identity + event store and build a ChatService. Bootstraps a
/// fresh identity on first run (idempotent thereafter).
fn open_service(cfg: &Config) -> Result<ChatService> {
    std::fs::create_dir_all(&cfg.data_dir)?;
    let identity = IdentityStore::new(&cfg.data_dir, StoreKeyVault::for_data_dir(&cfg.data_dir));
    let stored = identity.bootstrap("hive-cli", "hive-cli", "hive-cli")?;
    let device_kp = identity.device_keypair(stored.device.id)?;
    let account_kp = identity.account_keypair(stored.account.id)?;
    let store = EventStore::open(cfg.data_dir.join("hive.db"))?;
    let mut svc = ChatService::new(store, stored.device.id, device_kp, account_kp, stored.account.actor());
    // Agents bound to this host sign their own output with keys this device
    // certifies (A3); the keys live with this host's other keys.
    svc.set_agent_vault(Box::new(StoreKeyVault::for_data_dir(&cfg.data_dir)));
    Ok(svc)
}

/// One-time cleanup: drop events of any workspace other than this config's
/// room that this box never wrote in — events older clients pushed into the
/// room regardless of their workspace (the sync engine now refuses both ways).
/// The database is backed up next to itself first; nothing is deleted unless
/// that backup succeeds. A no-op (no backup) once clean.
fn purge_foreign_workspaces(cfg: &Config) -> Result<()> {
    let db = cfg.data_dir.join("hive.db");
    if !db.exists() {
        return Ok(());
    }
    let identity = IdentityStore::new(&cfg.data_dir, StoreKeyVault::for_data_dir(&cfg.data_dir));
    let own_device = identity.bootstrap("hive-cli", "hive-cli", "hive-cli")?.device.id;
    let keep = HashSet::from([uuid_of_room(&cfg.room)]);
    let mut store = EventStore::open(&db)?;
    if store.foreign_workspaces(&keep, own_device)?.is_empty() {
        return Ok(());
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let backup = cfg.data_dir.join(format!("hive.db.bak-foreign-purge-{stamp}"));
    let dropped = store.purge_foreign_workspaces(&keep, own_device, Some(&backup))?;
    let events: usize = dropped.iter().map(|(_, n)| n).sum();
    println!(
        "hive: removed {events} event(s) of {} other workspace(s) that don't belong to this room (backup: {}).",
        dropped.len(),
        backup.display()
    );
    Ok(())
}

/// One sync round against the relay: push everything unpushed, then fetch +
/// ingest. Uses its own EventStore connection (shares the WAL file), as the app
/// does. Returns (pushed, pulled) counts.
async fn sync_once(cfg: &Config) -> Result<(usize, usize)> {
    let mut engine = sync_engine(cfg)?;
    let mut store = EventStore::open(cfg.data_dir.join("hive.db"))?;
    let (pushed, mut pulled) = sync_round(&mut engine, &mut store).await?;
    // A one-shot sync (a daemon's first sync, `ligus sync`, a worker tick) needs
    // the whole room before it acts: keep paging until caught up. Each round is
    // bounded and persists its cursor, so the cap only limits one call.
    if engine.is_behind() {
        pulled += engine.catch_up(&mut store, SYNC_ONCE_MAX_ROUNDS).await?;
    }
    Ok((pushed, pulled))
}

/// Pull rounds one `sync_once` may run (each up to
/// `hive_runtime::sync_engine::PULL_PAGES_PER_ROUND` pages of
/// `ENVELOPE_PAGE_LIMIT`): far beyond any real room, so it only stops a
/// runaway loop; the next call continues from the persisted cursor.
const SYNC_ONCE_MAX_ROUNDS: usize = 250;

/// The relay client this config syncs through.
fn relay_client(cfg: &Config) -> Result<RelayClient> {
    let relay_url = cfg
        .relay_url
        .clone()
        .ok_or_else(|| anyhow!("set LIGUS_RELAY_URL to sync"))?;
    Ok(RelayClient::new(&relay_url)
        .with_auth(cfg.token.clone())
        .with_auth_cell(auth::cell()))
}

/// This box's keyring for its room (B1), from its store's pinned config fold:
/// what blobs and liveness beats seal under and open with.
fn box_ring(cfg: &Config) -> Option<hive_runtime::keyring::KeyringEngine> {
    let src = key_source(cfg).ok()?;
    let store = EventStore::open(cfg.data_dir.join("hive.db")).ok()?;
    Some(hive_runtime::keyring::KeyringEngine::from_store(&store, hive_core::room_workspace_id(&cfg.room), &src))
}

/// A sync engine for this config. Kept across rounds (as `ligus serve` does),
/// it scans only rows stored since its last push instead of the whole store.
/// Every CLI host (serve, worker, `ligus sync`) builds its keyring the same
/// way as the desktop (B1): from the folded config log and this box's own
/// key-agreement key, so it reads and seals at rotated epochs.
fn sync_engine(cfg: &Config) -> Result<SyncEngine> {
    Ok(SyncEngine::new(relay_client(cfg)?, cfg.room.clone()).with_key_source(key_source(cfg)?))
}

/// What this box holds for the keyring (B1).
fn key_source(cfg: &Config) -> Result<hive_runtime::keyring::KeySource> {
    let identity = IdentityStore::new(&cfg.data_dir, StoreKeyVault::for_data_dir(&cfg.data_dir));
    let device = identity.bootstrap("hive-cli", "hive-cli", "hive-cli")?.device.id;
    let ka = identity.key_agreement(device)?;
    Ok(hive_runtime::keyring::KeySource::for_device(device, ka, cfg.key.as_deref(), &cfg.epoch_keys))
}

/// Publish this box's own key-agreement key on the workspace config log
/// (once; B1), so the next rotation seals the workspace key to it. Best
/// effort: a box whose config log hasn't synced, or that isn't certified
/// yet, tries again on its next start.
fn publish_key_agreement(cfg: &Config, svc: &mut ChatService) {
    let identity = IdentityStore::new(&cfg.data_dir, StoreKeyVault::for_data_dir(&cfg.data_dir));
    let Ok(ka) = identity.key_agreement(svc.device_id()) else { return };
    match svc.publish_key_agreement(uuid_of_room(&cfg.room), ka.public_key_bytes()) {
        Ok(true) => println!("published this box's key-agreement key for workspace key rotations"),
        Ok(false) => {}
        Err(e) => eprintln!("key-agreement key not published yet: {e}"),
    }
}

/// One push + pull round: (pushed, pulled).
async fn sync_round(engine: &mut SyncEngine, store: &mut EventStore) -> Result<(usize, usize)> {
    let to_push = engine.take_unpushed(store)?;
    let pushed = to_push.len();
    engine.push_envelopes(&to_push).await?;
    // One bounded round of relay pages; `engine.is_behind()` says another is due.
    let pulled = engine.pull_round(store).await?;
    Ok((pushed, pulled))
}

/// Resolve any `[Attached-blob: …]` references in agent turns to local files
/// (downloading + decrypting from the relay's blob channel and caching them), so
/// a headless agent can read attachments other members shared. No-op when no
/// relay/workspace key is configured or no turn carries a blob marker.
async fn materialize_turns(cfg: &Config, turns: &mut [ChatTurn]) {
    let (Some(relay_url), Some(_)) = (cfg.relay_url.clone(), cfg.key.as_ref()) else {
        return;
    };
    if !turns.iter().any(|t| t.content.contains("[Attached-blob:")) {
        return;
    }
    let relay = RelayClient::new(&relay_url)
        .with_auth(cfg.token.clone())
        .with_auth_cell(auth::cell());
    // The one keyring (B1): blobs sealed at any epoch this box holds open.
    let Some(ring) = box_ring(cfg) else { return };
    let cache = cfg.data_dir.join("attachments").join("blobs");
    for t in turns.iter_mut() {
        if t.content.contains("[Attached-blob:") {
            t.content = hive_runtime::attachment_blob::materialize_body(
                &t.content, &relay, &cfg.room, &ring, &cache,
            )
            .await;
        }
    }
}

fn cmd_whoami(cfg: &Config) -> Result<()> {
    let identity = IdentityStore::new(&cfg.data_dir, StoreKeyVault::for_data_dir(&cfg.data_dir));
    let stored = identity.bootstrap("hive-cli", "hive-cli", "hive-cli")?;
    println!("account : {}", stored.account.id);
    println!("device  : {}", stored.device.id);
    println!("name    : {}", stored.account.display_name);
    println!("data dir: {}", cfg.data_dir.display());
    println!("room    : {}", cfg.room);
    Ok(())
}

fn cmd_chats(cfg: &Config) -> Result<()> {
    let svc = open_service(cfg)?;
    let config_id = workspace_config_session_id(uuid_of_room(&cfg.room));
    let mut any = false;
    for id in svc.store().list_session_ids()? {
        // Never list the workspace-config log — it isn't a chat.
        if id == config_id {
            continue;
        }
        if let Some(s) = svc.load(id)? {
            // Skip config logs of other workspaces too (empty-title, has channels).
            if id == workspace_config_session_id(s.workspace_id) {
                continue;
            }
            any = true;
            let ch = if s.channel_id.is_empty() { String::new() } else { format!("  #{}", s.channel_id) };
            println!("{}  {:<32}  {} msgs{}", s.id, truncate(&s.title, 32), s.messages.len(), ch);
        }
    }
    if !any {
        println!("(no chats — run `ligus sync` to pull a workspace, or open the app to create one)");
    }
    Ok(())
}

/// `ligus proposals` — every proposal in the workspace's chats, newest first,
/// with its status and the votes cast on it (who, at what role).
fn cmd_proposals(cfg: &Config) -> Result<()> {
    let svc = open_service(cfg)?;
    let ws = uuid_of_room(&cfg.room);
    let mut rows = Vec::new();
    for id in svc.store().list_session_ids()? {
        if id == workspace_config_session_id(ws) {
            continue;
        }
        let Some(s) = svc.load(id)? else { continue };
        if s.workspace_id != ws || id == workspace_config_session_id(s.workspace_id) {
            continue;
        }
        for p in &s.proposals {
            let name_of = |actor: &str| {
                s.members
                    .iter()
                    .find(|m| m.id == actor)
                    .map(|m| m.actor.display_name.clone())
                    .unwrap_or_else(|| actor.to_string())
            };
            let votes: Vec<String> = p
                .approvals
                .iter()
                .map(|a| format!("{} {} ({:?})", if a.approved { "+" } else { "-" }, name_of(&a.actor_id), a.role))
                .collect();
            let status = format!("{:?}", p.status).to_lowercase();
            let mut line = format!("{}  {:<9} {}  [chat {}]", p.id, status, truncate(&p.title, 48), s.id);
            if !votes.is_empty() {
                line.push_str(&format!("\n    votes: {}", votes.join(", ")));
            }
            rows.push((p.created_at, line));
        }
    }
    if rows.is_empty() {
        println!("(no proposals)");
    }
    rows.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    for (_, line) in rows {
        println!("{line}");
    }
    Ok(())
}

fn print_transcript(svc: &ChatService, chat_id: Uuid, from: usize) -> Result<usize> {
    let Some(s) = svc.load(chat_id)? else {
        bail!("unknown chat {chat_id}");
    };
    for m in s.messages.iter().skip(from) {
        println!("{:>9}  {}", m.author, m.body);
    }
    Ok(s.messages.len())
}

fn cmd_tail(cfg: &Config, chat_id: Uuid, watch: bool) -> Result<()> {
    let svc = open_service(cfg)?;
    let mut seen = print_transcript(&svc, chat_id, 0)?;
    if !watch {
        return Ok(());
    }
    let rt = tokio::runtime::Runtime::new()?;
    loop {
        std::thread::sleep(Duration::from_secs(3));
        if cfg.relay_url.is_some() {
            let _ = rt.block_on(sync_once(cfg));
        }
        // Reopen to pick up ingested events (a fresh projection off the WAL file).
        let svc = open_service(cfg)?;
        seen = print_transcript(&svc, chat_id, seen)?;
    }
}

async fn cmd_send(cfg: &Config, chat_id: Uuid, body: String, push: bool) -> Result<()> {
    let mut svc = open_service(cfg)?;
    let Some(session) = svc.load(chat_id)? else {
        bail!("unknown chat {chat_id}");
    };
    svc.post_user_message(chat_id, session.workspace_id, body)?;
    println!("posted.");
    if push {
        let (pushed, pulled) = sync_once(cfg).await?;
        println!("synced: pushed {pushed}, pulled {pulled}");
    }
    Ok(())
}

async fn cmd_sync(cfg: &Config, watch: bool) -> Result<()> {
    loop {
        let (pushed, pulled) = sync_once(cfg).await?;
        println!("pushed {pushed}, pulled {pulled}");
        if !watch {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

fn cmd_new(cfg: &Config, title: String) -> Result<()> {
    let mut svc = open_service(cfg)?;
    let ws = uuid_of_room(&cfg.room);
    let session = svc.create_chat(title, ws, "")?;
    println!("{}", session.id);
    Ok(())
}

/// Build a runtime target from env (LIGUS_PROVIDER / LIGUS_MODEL / a key). This is
/// a PERSONAL credential today — spec §12.5's workspace-owned credential for a
/// detached worker doesn't exist yet.
fn resolve_runtime_from_env() -> Result<(ResolvedRuntime, String)> {
    let provider_str = env_opt("LIGUS_PROVIDER").unwrap_or_else(|| "anthropic".into());
    let model = env_opt("LIGUS_MODEL").unwrap_or_else(|| "claude-sonnet-4-5".into());
    let (provider, endpoint, key_env): (ModelProviderKind, String, &str) =
        match provider_str.to_lowercase().as_str() {
            "anthropic" => (ModelProviderKind::Anthropic, String::new(), "ANTHROPIC_API_KEY"),
            "openai" => (
                ModelProviderKind::OpenAI,
                "https://api.openai.com/v1/chat/completions".into(),
                "OPENAI_API_KEY",
            ),
            "openrouter" => (
                ModelProviderKind::OpenRouter,
                "https://openrouter.ai/api/v1/chat/completions".into(),
                "OPENROUTER_API_KEY",
            ),
            "ollama" => (
                ModelProviderKind::Ollama,
                env_opt("LIGUS_MODEL_BASE_URL")
                    .unwrap_or_else(|| "http://localhost:11434/v1/chat/completions".into()),
                "",
            ),
            other => bail!("unsupported LIGUS_PROVIDER '{other}' (anthropic|openai|openrouter|ollama)"),
        };
    let api_key = env_opt("LIGUS_PROVIDER_API_KEY")
        .or_else(|| (!key_env.is_empty()).then(|| env_opt(key_env)).flatten());
    let rt = ResolvedRuntime {
        provider,
        model: model.clone(),
        endpoint,
        api_key,
        args: Vec::new(),
        model_provider_id: None,
        model_base_url: None,
        context_window_tokens: None,
        keep_alive: None,
        think: None,
        tools: false,
        max_tool_rounds: None,
        allow_remote_file_tools: false,
        driver: Default::default(),
    };
    Ok((rt, format!("{provider_str}/{model}")))
}

fn provider_kind(s: &str) -> Result<ModelProviderKind> {
    Ok(match s.to_lowercase().as_str() {
        "anthropic" => ModelProviderKind::Anthropic,
        "openai" => ModelProviderKind::OpenAI,
        "openrouter" => ModelProviderKind::OpenRouter,
        "ollama" => ModelProviderKind::Ollama,
        // Any OpenAI-compatible server (a llama.cpp gateway): keyless when the
        // endpoint is local / LAN / tailnet, and runs the agent tool loop.
        "custom" => ModelProviderKind::Custom,
        // Subscription CLIs on the worker host (their own login; keyless).
        "claude-code" | "claudecode" => ModelProviderKind::ClaudeCode,
        "codex" => ModelProviderKind::Codex,
        "antigravity" | "agy" => ModelProviderKind::Antigravity,
        "copilot" => ModelProviderKind::Copilot,
        other => bail!("unknown provider '{other}' (anthropic|openai|openrouter|ollama|custom|claude-code|codex|antigravity|copilot)"),
    })
}

/// Define (or replace by id) a workspace-owned runtime. `--secret-ref <name>`
/// keeps the key off the synced log (worker env `LIGUS_WS_SECRET_<name>`);
/// `--secret <value>` carries it E2EE on the config log; neither = keyless.
#[allow(clippy::too_many_arguments)]
fn cmd_add_runtime(
    cfg: &Config,
    id: &str,
    name: &str,
    provider: &str,
    model: &str,
    endpoint: Option<String>,
    secret_ref: Option<String>,
    secret: Option<String>,
) -> Result<()> {
    let mut wr = WorkspaceRuntime::new(id, name, provider_kind(provider)?, model);
    wr.endpoint = endpoint.unwrap_or_default();
    wr.credential = match (secret_ref, secret) {
        (Some(r), _) => WorkspaceCredential::SecretRef { secret_ref: r },
        (None, Some(s)) => WorkspaceCredential::Inline { secret: s },
        (None, None) => WorkspaceCredential::None,
    };
    let mut svc = open_service(cfg)?;
    svc.add_workspace_runtime(uuid_of_room(&cfg.room), wr)?;
    println!("runtime '{id}' defined (sync to share it with the workspace).");
    Ok(())
}

fn cmd_rm_runtime(cfg: &Config, id: &str) -> Result<()> {
    let mut svc = open_service(cfg)?;
    svc.remove_workspace_runtime(uuid_of_room(&cfg.room), id)?;
    println!("removed.");
    Ok(())
}

/// Is `bin` an executable on PATH? (headless detection for `ligus setup`).
fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(bin).is_file()))
        .unwrap_or(false)
}

/// `ligus setup` — headless detect + configure. Reports what this box can already
/// reach and (with --apply) defines workspace runtimes for the *keyless* ones
/// (Ollama, claude/codex CLIs). Key-backed providers are printed as ready-to-run
/// commands rather than auto-wired, so a personal key is never inlined onto the
/// shared config log (workspace credentials use a secretRef the worker holds).
fn cmd_setup(cfg: &Config, apply: bool) -> Result<()> {
    let ollama = "127.0.0.1:11434"
        .parse()
        .ok()
        .map(|addr| {
            std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(300)).is_ok()
        })
        .unwrap_or(false);
    let claude = on_path("claude");
    let codex = on_path("codex");
    // `true`: also confirm agy's Google sign-in with `agy models` (read-only,
    // no quota) — this is an explicit, interactive command.
    let clis = hive_runtime::cli_detect::detect_cli_agents(&hive_runtime::cli_detect::DetectEnv::from_process(), true);

    println!("Detected on this box:");
    for (env_var, provider, model) in [
        ("ANTHROPIC_API_KEY", "anthropic", "claude-sonnet-4-6"),
        ("OPENAI_API_KEY", "openai", "gpt-4o"),
        ("OPENROUTER_API_KEY", "openrouter", "openai/gpt-4o-mini"),
    ] {
        if std::env::var(env_var).map(|v| !v.is_empty()).unwrap_or(false) {
            println!("  ✓ {env_var} present (key-backed)");
            println!(
                "      → ligus add-runtime {provider} \"{provider}\" {provider} {model} --secret-ref {provider}   (worker sets LIGUS_WS_SECRET_{provider})"
            );
        }
    }
    if ollama {
        println!("  ✓ Ollama (local, keyless)");
    }
    if claude {
        println!("  ✓ claude CLI (keyless)");
    }
    if codex {
        println!("  ✓ codex CLI (keyless)");
    }
    for c in &clis {
        if !c.installed {
            continue;
        }
        let version = c.version.as_deref().map(|v| format!(" {v}")).unwrap_or_default();
        if !c.supported {
            println!("  ! {}{version} — {}", c.label, c.note);
            continue;
        }
        use hive_runtime::cli_detect::LoginState;
        let login = match c.login {
            LoginState::LoggedIn => c.login_detail.clone(),
            LoginState::NotLoggedIn => format!("NOT signed in — run `{}` in a terminal on this box", c.login_command),
            LoginState::Unknown => format!("sign-in unknown — if a turn fails, run `{}` in a terminal on this box", c.login_command),
        };
        println!("  ✓ {} ({}){version}, keyless — {login}", c.label, c.binary);
    }

    if apply {
        let mut svc = open_service(cfg)?;
        let ws = uuid_of_room(&cfg.room);
        let mut n = 0;
        if ollama {
            let mut wr = WorkspaceRuntime::new("ollama", "Ollama (local)", ModelProviderKind::Ollama, "llama3.2");
            wr.endpoint = "http://localhost:11434".to_string();
            svc.add_workspace_runtime(ws, wr)?;
            n += 1;
        }
        if claude {
            svc.add_workspace_runtime(ws, WorkspaceRuntime::new("claude-code", "Claude Code", ModelProviderKind::ClaudeCode, ""))?;
            n += 1;
        }
        if codex {
            svc.add_workspace_runtime(ws, WorkspaceRuntime::new("codex", "Codex", ModelProviderKind::Codex, ""))?;
            n += 1;
        }
        for c in clis.iter().filter(|c| c.installed && c.supported) {
            if let Some(kind) = hive_runtime::cli_detect::provider_for(&c.id) {
                svc.add_workspace_runtime(ws, WorkspaceRuntime::new(c.id.as_str(), c.label.as_str(), kind, ""))?;
                n += 1;
            }
        }
        println!("\nDefined {n} keyless runtime(s). Run the add-runtime line(s) above for key-backed providers, then `ligus sync` to share.");
    } else {
        println!("\nRe-run `ligus setup --apply` to define the keyless runtimes automatically.");
    }
    Ok(())
}

/// TOML wrapper for `ligus config export/import` — the workspace's runtime set.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct WorkspaceRuntimesToml {
    #[serde(default, rename = "runtime")]
    runtimes: Vec<WorkspaceRuntime>,
}

/// `ligus config export` — print the workspace runtimes as portable TOML. Inline
/// secrets are NEVER exported: they're downgraded to a secretRef the importer
/// resolves from LIGUS_WS_SECRET_<ref>.
fn cmd_config_export(cfg: &Config) -> Result<()> {
    let svc = open_service(cfg)?;
    let mut runtimes = svc.list_workspace_runtimes(uuid_of_room(&cfg.room))?;
    for r in &mut runtimes {
        if matches!(r.credential, WorkspaceCredential::Inline { .. }) {
            r.credential = WorkspaceCredential::SecretRef { secret_ref: r.id.clone() };
        }
    }
    print!("{}", toml::to_string_pretty(&WorkspaceRuntimesToml { runtimes })?);
    Ok(())
}

/// `ligus config import <file>` — define workspace runtimes from a TOML file
/// (additive; upserts by id). `ligus sync` to share them with the team.
fn cmd_config_import(cfg: &Config, path: &str) -> Result<()> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    let doc: WorkspaceRuntimesToml = toml::from_str(&text).context("parsing config TOML")?;
    let mut svc = open_service(cfg)?;
    let ws = uuid_of_room(&cfg.room);
    let mut n = 0;
    for wr in doc.runtimes {
        let id = wr.id.clone();
        svc.add_workspace_runtime(ws, wr)?;
        println!("+ runtime {id}");
        n += 1;
    }
    println!("Imported {n} workspace runtime(s). `ligus sync` to share.");
    Ok(())
}

/// List the workspace-owned runtimes synced onto this box (spec §12.5).
fn cmd_runtimes(cfg: &Config) -> Result<()> {
    let svc = open_service(cfg)?;
    let rts = svc.list_workspace_runtimes(uuid_of_room(&cfg.room))?;
    if rts.is_empty() {
        println!("(no workspace runtimes — define one in the app, or `ligus sync` a workspace that has them)");
    }
    for r in rts {
        let cred = match &r.credential {
            WorkspaceCredential::SecretRef { secret_ref } => {
                let have = hive_core::compat::var(&WorkspaceCredential::env_var(secret_ref)).is_ok();
                format!("secretRef:{secret_ref}{}", if have { " ✓" } else { " (missing)" })
            }
            WorkspaceCredential::Inline { .. } => "inline".into(),
            WorkspaceCredential::None => "keyless".into(),
        };
        println!("{:<16} {:<22} {:?}/{}  [{cred}]", r.id, r.name, r.provider, r.model);
    }
    Ok(())
}

/// Resolve the runtime an agent should use: a workspace-owned runtime by id
/// (spec §12.5 — credential resolved via the worker's secret store, never a
/// personal key), else the personal env fallback.
fn resolve_agent_runtime(cfg: &Config, runtime_id: Option<&str>) -> Result<(ResolvedRuntime, String)> {
    let Some(id) = runtime_id else {
        return resolve_runtime_from_env();
    };
    let svc = open_service(cfg)?;
    let wr = svc
        .list_workspace_runtimes(uuid_of_room(&cfg.room))?
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| anyhow!("no workspace runtime '{id}' (run `ligus runtimes`, or define one in the app)"))?;
    let api_key = resolve_workspace_credential(&wr.credential);
    let rt = ResolvedRuntime {
        provider: wr.provider,
        model: wr.model.clone(),
        endpoint: wr.endpoint.clone(),
        api_key,
        args: Vec::new(),
        model_provider_id: None,
        model_base_url: None,
        context_window_tokens: None,
        keep_alive: None,
        think: None,
        tools: false,
        max_tool_rounds: None,
        allow_remote_file_tools: false,
        driver: Default::default(),
    };
    Ok((rt, format!("workspace:{}", wr.name)))
}

/// v1 — act as an agent. After each sync, reply (as the primary responder) to
/// any new user message that is un-mentioned or `@primary`, via the resolved
/// runtime (a workspace-owned one with `--runtime`, else the personal env), and
/// post + sync the reply back as signed events.
async fn cmd_agent(cfg: &Config, name: String, runtime_id: Option<String>) -> Result<()> {
    // §12.5: a detached agent must run on a WORKSPACE-owned credential, never a
    // personal key. `ligus agent` resolves a personal/local runtime, so it is a
    // local-dev convenience only — the production daemon is `ligus worker`, which
    // requires a workspace-owned runtime. Refuse to run unattended in production
    // unless explicitly opted in for dev.
    if hive_core::compat::var("LIGUS_ALLOW_PERSONAL_AGENT").is_err() {
        bail!(
            "`ligus agent` runs on a personal credential, which isn't workspace-owned (\u{a7}12.5) — \
             use `ligus worker` for production. For local dev only, set LIGUS_ALLOW_PERSONAL_AGENT=1."
        );
    }
    // Pull config first so a `--runtime` workspace runtime is available.
    if cfg.relay_url.is_some() {
        let _ = sync_once(cfg).await;
    }
    let (rt, rt_label) = resolve_agent_runtime(cfg, runtime_id.as_deref())?;
    if rt.api_key.is_none() && rt.provider != ModelProviderKind::Ollama {
        bail!("runtime '{rt_label}' has no resolvable key (set a provider key, or provision LIGUS_WS_SECRET_… for a workspace runtime)");
    }
    // Seed "seen" with existing messages so we don't reply to history.
    let mut seen: HashSet<Uuid> = HashSet::new();
    {
        let svc = open_service(cfg)?;
        for id in svc.store().list_session_ids()? {
            if let Some(s) = svc.load(id)? {
                seen.extend(s.messages.iter().map(|m| m.id));
            }
        }
    }
    hive_runtime::author_gate::warn_if_lowered(hive_runtime::author_gate::turn_writes(rt.provider, &rt.args));
    println!("agent @{name} online ({rt_label}); replying to @primary / un-mentioned turns. Ctrl-C to stop.");
    let host_label = env_opt("HOSTNAME").unwrap_or_else(|| "this worker".into());
    let system = format!(
        "You are @{name}, an agent in a Ligus workspace chat. Reply to the latest message concisely and helpfully.\n\n{}",
        hive_runtime::prompt::runtime_identity_block(&rt, &host_label)
    );
    loop {
        if cfg.relay_url.is_some() {
            if let Err(e) = sync_once(cfg).await {
                eprintln!("sync: {e}");
            }
        }
        let mut svc = open_service(cfg)?;
        for id in svc.store().list_session_ids()? {
            let Some(s) = svc.load(id)? else { continue };
            if id == workspace_config_session_id(s.workspace_id) {
                continue; // the config log isn't a chat
            }
            // Every new primary/un-mentioned user message, in order — not only
            // the chat's last: two posts in one sync ("hi" then "@coder …")
            // would otherwise drop the first.
            let triggers: Vec<hive_core::ChatMessage> = s
                .messages
                .iter()
                .filter(|m| m.role == MessageRole::User && !seen.contains(&m.id))
                .filter(|m| {
                    let t = parse_mentions(&m.body, &s);
                    t.primary || t.is_empty()
                })
                .filter(|m| {
                    // Already answered (by another `ligus agent`, or the desktop).
                    !s.messages.iter().any(|r| r.role == MessageRole::Assistant && r.reply_to_message_id == Some(m.id))
                })
                .cloned()
                .collect();
            seen.extend(s.messages.iter().map(|m| m.id));
            for last in triggers {
                // Non-Send store is held across .await — cmd_agent runs on a
                // current-thread runtime (see run()), so this is allowed.
                let mut turns = turns_for(&worker::through_trigger(&s, last.id), None, &name);
                materialize_turns(cfg, &mut turns).await;
                print!("↳ @{name} replying in {} … ", s.id);
                // H1 gate, the same as the worker's: at or above this host's
                // floor (Admin when the runtime can write, Contributor when
                // read-only; LIGUS_MCP_MIN_ROLE overrides) the turn runs as
                // configured. Below it an HTTP model gets no tools, a CLI runs
                // at its read-only tier, and one with none is refused.
                // Unresolved author → fail closed.
                // The author's role when they posted it (config roster at its frontier).
                let author_role = svc.author_role_at(s.id, last.id).ok().flatten();
                let mut turn_rt = rt.clone();
                let plan = hive_runtime::author_gate::gate_turn(turn_rt.provider, &mut turn_rt.args, author_role);
                let allow_mcp = plan.allows_tools();
                if let hive_runtime::author_gate::GatePlan::Refuse(reason) = &plan {
                    println!("refused: {reason}");
                    continue;
                }
                if !allow_mcp {
                    eprintln!("gating message {} — author role {author_role:?} below this host's floor", last.id);
                }
                // MCP tools when LIGUS_MCP_CONFIG provisions them and the gate allows,
                // else a plain stream.
                // The CLI child runs in this host's OS sandbox, in a fresh
                // scratch dir (never this process's cwd).
                let envelope = hive_core::sandbox::envelope_for_session(&s);
                let enforcement = hive_runtime::sandbox::enforce(turn_rt.provider, &envelope, &Vec::new());
                if let Some(reason) = enforcement.refuse {
                    println!("refused: {reason}");
                    continue;
                }
                turn_rt.args.extend(enforcement.extra_args);
                let allow_mcp = allow_mcp && enforcement.allow_run_command;
                let sandbox = hive_runtime::executor::TurnPolicy::new(hive_runtime::executor::HostKind::Agent, envelope, plan.clone());
                let reply = hive_runtime::executor::with_turn(sandbox, reply::generate_reply(&turn_rt, &system, &turns, allow_mcp, None)).await?;
                let mid = svc.begin_assistant_message(s.id, s.workspace_id, name.clone(), rt_label.clone(), None, Some(last.id))?;
                svc.complete_assistant_message(s.id, s.workspace_id, mid, reply)?;
                seen.insert(mid);
                println!("done");
            }
        }
        drop(svc);
        if cfg.relay_url.is_some() {
            let _ = sync_once(cfg).await;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

/// `ligus trust list | enable <digest> | enable-all`: the S1 decision for a
/// repository's runtimes and MCP servers on this device (the same table the
/// desktop's *Trust this workspace* writes, in this host's data dir).
fn cmd_trust(cfg: &Config, repo: &std::path::Path, action: Option<&str>, digest: Option<&str>) -> Result<()> {
    use hive_runtime::workspace_trust as wt;
    let store = wt::TrustStore::open(&cfg.data_dir)?;
    let rid = wt::root_id(repo);
    let mut items = wt::repo_items(repo);
    store.mark(&rid, &mut items);
    match action {
        Some("list") | None => {
            if items.is_empty() {
                println!("{} ships no runtimes or MCP servers", repo.display());
            }
            for i in &items {
                println!(
                    "{}  {:<9} {:<20} {}  ({})  {}",
                    &i.digest[..16],
                    i.kind.as_str(),
                    i.id,
                    if i.enabled { "trusted" } else { "INERT" },
                    i.source,
                    i.command_line
                );
            }
            Ok(())
        }
        Some("enable") => {
            let d = digest.ok_or_else(|| anyhow!("usage: ligus trust enable <digest> (from `ligus trust list`)"))?;
            let matching: Vec<&wt::TrustItem> = items.iter().filter(|i| i.digest.starts_with(d)).collect();
            let [item] = matching.as_slice() else { bail!("{} items match {d}: give more of the digest", matching.len()) };
            store.enable(&rid, item.kind, &item.digest, "ligus-cli")?;
            println!("trusted {} {}: {}", item.kind.as_str(), item.id, item.command_line);
            Ok(())
        }
        Some("enable-all") => {
            for i in &items {
                store.enable(&rid, i.kind, &i.digest, "ligus-cli")?;
                println!("trusted {} {}: {}", i.kind.as_str(), i.id, i.command_line);
            }
            Ok(())
        }
        Some(other) => bail!("unknown `ligus trust {other}`: use list|enable|enable-all"),
    }
}

/// Create a workspace agent (bound to a runtime; optionally a host + role).
fn cmd_add_agent(
    cfg: &Config,
    name: &str,
    runtime_id: &str,
    host: Option<String>,
    role: Option<String>,
) -> Result<()> {
    let ws = uuid_of_room(&cfg.room);
    let mut agent = WorkspaceAgent::new(name, runtime_id);
    agent.role = role.unwrap_or_default();
    agent.host_id = host.unwrap_or_default();
    let mut svc = open_service(cfg)?;
    agent.owner_actor_id = svc.author().id.clone();
    svc.add_agent(ws, ws, agent)?;
    println!("agent @{name} created on runtime '{runtime_id}' (sync to share).");
    Ok(())
}

fn cmd_agents(cfg: &Config) -> Result<()> {
    let svc = open_service(cfg)?;
    let ws = uuid_of_room(&cfg.room);
    let agents = svc.load(workspace_config_session_id(ws))?.map(|s| s.workspace_agents).unwrap_or_default();
    if agents.is_empty() {
        println!("(no agents — `ligus add-agent <name> <runtime-id>`)");
    }
    for a in agents {
        let host = if a.host_id.is_empty() { "owner-device".into() } else { a.host_id.clone() };
        println!("@{:<20} runtime={:<14} host={}", a.name, a.runtime_id, host);
    }
    Ok(())
}

// ── Hosts + worker daemon (spec §12.4) ──────────────────────────────────────

fn cmd_hosts(cfg: &Config) -> Result<()> {
    let svc = open_service(cfg)?;
    let me = svc.device_id().to_string();
    let hosts = svc.list_hosts(uuid_of_room(&cfg.room))?;
    if hosts.is_empty() {
        println!("(no hosts — `ligus register-worker` to register this box)");
    }
    for h in hosts {
        let mark = if h.id == me { "  ← this box" } else { "" };
        println!("{:<38} {:?}  {}{mark}", h.id, h.kind, h.label);
    }
    Ok(())
}

fn cmd_register_worker(cfg: &Config, label: Option<String>) -> Result<String> {
    let mut svc = open_service(cfg)?;
    let id = svc.device_id().to_string();
    let label = label.or_else(|| env_opt("HOSTNAME")).unwrap_or_else(|| "worker".into());
    svc.ensure_host(uuid_of_room(&cfg.room), WorkspaceHost::new(id.clone(), HostKind::Worker, label))?;
    Ok(id)
}

fn cmd_set_agent_host(cfg: &Config, agent_name: &str, host_id: &str) -> Result<()> {
    let mut svc = open_service(cfg)?;
    let ws = uuid_of_room(&cfg.room);
    let agents = svc
        .load(workspace_config_session_id(ws))?
        .map(|s| s.workspace_agents)
        .unwrap_or_default();
    let agent = agents
        .iter()
        .find(|a| a.name == agent_name)
        .ok_or_else(|| anyhow!("no agent '{agent_name}' in this workspace"))?;
    let aid = agent.id;
    svc.set_agent_host(ws, aid, host_id)?;
    println!("bound @{agent_name} to host {host_id} (sync to apply).");
    Ok(())
}

/// The worker daemon (spec §12.4-12.5). Registers this box as a worker host and
/// heartbeats its presence, then continuously drains **queued work** for the
/// agents bound to it: every unanswered `@mention` (backlog included, so it
/// catches up on work addressed while it was down, and picks up an agent freshly
/// reassigned to it) is answered via that agent's workspace runtime and synced
/// back. A failing turn is isolated and not retried in a tight loop. See
/// `worker.rs` for the turn itself (tool loop, worktrees, proposals).
async fn cmd_worker(cfg: &Config, label: Option<String>) -> Result<()> {
    let ws = uuid_of_room(&cfg.room);
    if let Err(e) = purge_foreign_workspaces(cfg) {
        eprintln!("worker: foreign-workspace cleanup skipped: {e}");
    }
    // Same label `cmd_register_worker` publishes, reused for the identity block
    // so an agent can say which worker it runs on.
    let host_label = label
        .clone()
        .or_else(|| env_opt("HOSTNAME"))
        .unwrap_or_else(|| "worker".into());
    let my_host = cmd_register_worker(cfg, label)?;
    if cfg.relay_url.is_some() {
        let _ = sync_once(cfg).await;
    }
    println!("worker {my_host} online — draining queued mentions for its agents. Ctrl-C to stop.");
    let mut svc = open_service(cfg)?;
    publish_key_agreement(cfg, &mut svc);
    // Finish replies a previous run was streaming when it stopped (see
    // `worker::recover_interrupted_turns`).
    match worker::recover_interrupted_turns(&mut svc) {
        Ok(done) if !done.is_empty() => println!("worker: finished {} reply(s) interrupted by the last stop", done.len()),
        Ok(_) => {}
        Err(e) => eprintln!("worker: interrupted-turn recovery failed: {e}"),
    }
    let svc = std::sync::Arc::new(std::sync::Mutex::new(svc));
    let ctx = worker::WorkerCtx {
        cfg,
        ws,
        my_host: &my_host,
        host_label: &host_label,
        settings: reply::ToolSettings::from_env(),
        ligus: Some(worker::ligus_server(svc.clone())),
    };
    // Mentions whose reply hard-failed, so we don't retry them every tick.
    let mut failed: HashSet<Uuid> = HashSet::new();
    // Exponential backoff on repeated relay-sync failures, so a relay outage
    // isn't hammered every 3s. Resets to the base cadence on the first success.
    let mut sync_fails: u32 = 0;
    // Liveness beat on the relay's presence channel (not the event log).
    let mut last_beat: Option<std::time::Instant> = None;
    let beat_every = Duration::from_secs(hive_runtime::host_presence::HOST_BEAT_EVERY_SECS as u64);
    loop {
        if last_beat.is_none_or(|t| t.elapsed() >= beat_every) {
            publish_beat(cfg, &my_host).await;
            last_beat = Some(std::time::Instant::now());
        }
        if cfg.relay_url.is_some() {
            match sync_once(cfg).await {
                Ok(_) => sync_fails = 0,
                Err(e) => {
                    sync_fails = sync_fails.saturating_add(1);
                    let secs = sync_backoff_secs(sync_fails);
                    eprintln!("sync: {e} — retrying in {secs}s");
                    tokio::time::sleep(Duration::from_secs(secs)).await;
                    continue;
                }
            }
        }
        // A transient store/DB error inside a tick must NOT kill the daemon —
        // log it and retry on the next tick (bare `?` here used to abort the loop).
        if let Err(e) = worker::drain_tick(&ctx, &svc, &mut failed, &worker::NoSink).await {
            eprintln!("worker: tick failed (retrying): {e}");
        }
        if cfg.relay_url.is_some() {
            let _ = sync_once(cfg).await;
        }
        // Sleep until the next tick, but wake immediately on a shutdown signal so
        // a redeploy/stop (SIGTERM) or Ctrl-C exits cleanly between ticks rather
        // than being SIGKILLed mid-poll.
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(3)) => {}
            _ = shutdown_signal() => {
                println!("worker: shutdown signal received — stopping.");
                return Ok(());
            }
        }
    }
}

/// Backoff after `fails` consecutive relay-sync failures: 6,12,24,48,96,120… s.
fn sync_backoff_secs(fails: u32) -> u64 {
    (3u64 << fails.min(5)).min(120)
}

/// Resolves when the process receives SIGTERM (orchestrator stop/redeploy) or
/// SIGINT (Ctrl-C), so the worker loop can exit gracefully.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Show queued work — unanswered agent mentions — and whether each agent's host
/// is online. A mention to an agent on an offline host is genuinely *queued*;
/// `ligus set-agent-host <agent> <this-worker>` reassigns it here to drain it.
fn cmd_queue(cfg: &Config) -> Result<()> {
    let svc = open_service(cfg)?;
    let ws = uuid_of_room(&cfg.room);
    let hosts = svc.list_hosts(ws)?;
    let presence = fetch_presence(cfg);
    let now = Timestamp::now();
    let mut any = false;
    for id in svc.store().list_session_ids()? {
        let Some(s) = svc.load(id)? else { continue };
        if id == workspace_config_session_id(s.workspace_id) {
            continue;
        }
        for pm in pending_mentions(&s) {
            any = true;
            let host_id = s
                .workspace_agents
                .iter()
                .find(|a| a.id == pm.agent_id)
                .map(|a| a.host_id.clone())
                .unwrap_or_default();
            let status = if host_id.is_empty() {
                "owner-device".to_string()
            } else {
                match hosts.iter().find(|h| h.id == host_id) {
                    Some(h) if presence.is_online(h, &now) => format!("{} · online", h.label),
                    Some(h) => format!("{} · OFFLINE → queued", h.label),
                    None => format!("{host_id} · unknown"),
                }
            };
            println!("@{:<18} in {}  [{status}]", pm.agent_name, s.id);
        }
    }
    if !any {
        println!("(nothing queued)");
    }
    Ok(())
}

/// Host liveness from the relay's presence channel, best effort (an empty
/// view without a relay, a key, or a reachable relay: the durable record then
/// decides, see [`hive_runtime::HostPresence::is_online`]).
fn fetch_presence(cfg: &Config) -> hive_runtime::HostPresence {
    let presence = hive_runtime::HostPresence::new();
    if let (Ok(relay), Some(_), Some(ring)) = (relay_client(cfg), cfg.key.as_deref(), box_ring(cfg)) {
        if let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
            let fetch = hive_runtime::host_presence::fetch_host_beats(&relay, &cfg.room, &ring);
            if let Ok(Ok(beats)) = rt.block_on(async { tokio::time::timeout(Duration::from_secs(5), fetch).await }) {
                presence.merge(beats);
            }
        }
    }
    presence
}

/// Publish this host's liveness beat (best effort; no relay or key = no-op).
async fn publish_beat(cfg: &Config, host_id: &str) {
    if let (Ok(relay), Some(_), Some(ring)) = (relay_client(cfg), cfg.key.as_deref(), box_ring(cfg)) {
        if let Err(e) = hive_runtime::host_presence::publish_host_beat(&relay, &cfg.room, &ring, host_id).await {
            eprintln!("presence: publishing this host's beat failed: {e}");
        }
    }
}

/// Deterministic workspace id for a relay room (mirrors the app's
/// `room_workspace_id`), so config-log filtering matches the app.
fn uuid_of_room(room: &str) -> Uuid {
    hive_core::room_workspace_id(room)
}

fn truncate(s: &str, n: usize) -> String {
    let t = if s.is_empty() { "(untitled)" } else { s };
    if t.chars().count() <= n {
        t.to_string()
    } else {
        format!("{}…", t.chars().take(n - 1).collect::<String>())
    }
}

fn usage() -> ! {
    eprintln!(
        "ligus — headless Ligus client\n\n\
         USAGE:\n  \
         ligus --version\n  \
         ligus whoami\n  \
         ligus chats\n  \
         ligus new [title]\n  \
         ligus tail <chat-id> [--watch]\n  \
         ligus send <chat-id> <message…> [--push]\n  \
         ligus sync [--watch]\n  \
         ligus runtimes                  list workspace-owned runtimes (spec §12.5)\n  \
         ligus add-runtime <id> <name> <provider> <model> [--endpoint <url>] [--secret-ref <name> | --secret <value>]\n                                       define a workspace-owned runtime\n  \
         ligus rm-runtime <id>           remove a workspace-owned runtime\n  \
         ligus agents                    list the workspace's agents\n  \
         ligus add-agent <name> <runtime-id> [--host <id>] [--role <role>]   add a workspace agent\n  \
         ligus agent [name] [--runtime <id>]   reply as the primary agent; --runtime uses a\n                                       workspace-owned credential (else a personal env key)\n  \
         ligus hosts                     list workspace hosts (devices + workers)\n  \
         ligus register-worker [--label <name>]   register this box as a worker host\n  \
         ligus set-agent-host <agent> <host-id>   bind an agent to a host\n  \
         ligus proposals                 list the workspace's proposals with status + votes\n  \
         ligus worker [--label <name>]   run the worker daemon (host detached agents, §12.4)\n  \
         ligus queue                     show queued work (unanswered mentions + host status)\n  \
         ligus serve [--listen ADDR] [--pair] [--token-file PATH] [--label NAME]\n                                       \
         run the home host for the iOS app (sync + worker + HTTP/SSE API);\n                                       \
         --remote-relay URL / LIGUS_REMOTE_RELAY also serves it through a relay\n  \
         ligus serve enroll [--new-device]   print the code that makes this box your own device\n                                       \
         (approve it on your desktop; --new-device after a revocation)\n  \
         ligus bind-repo <name|path> [--chat <id>] | --clear   bind a checkout for agent file tools\n  \
         ligus forge list|add <kind> <https://base> [--auth token|oauth|gh|none] [--client-id ID]|token <id> <read|write>|login <id>|test <id>|remove <id>\n                                       code-host connections (tokens on stdin, kept in the OS secret store)\n  \
         ligus repos                     show the repos dir and bindings\n  \
         ligus pair [--listen ADDR] [--public-url URL] [--relay-only]   show the phone pairing QR code\n  \
         ligus remote enable --relay URL [--reg-token T] | status | disable | reset\n                                       \
         phone remote access through a relay tunnel (off by default)\n  \
         ligus phones list | revoke <pid|name> [--keep-token]   phones paired over the relay\n  \
         ligus trust list | enable <digest> | enable-all [--repo PATH]   repo-supplied runtimes and MCP servers this device runs\n  \
         ligus facts export --jsonl [--since MS]   this device's measurement facts (ligus.facts.v1), for external observability tools\n  \
         ligus dashboard check [--file PATH]   validate dashboard.toml and evaluate it once\n  \
         ligus dashboard default         print the generated default dashboard as TOML\n  \
         ligus dashboard metrics         list the built-in hive metrics panels can show\n\n\
         Setup:\n  \
         ligus enroll <hivews1:invite> [--token <hrt1>]   one-shot: save the workspace connection\n  \
         ligus join <invite-code>        redeem a relay invite to self-enroll this box's identity\n  \
         ligus auth login <url> [--label NAME]   sign in through ligus-auth (device code); the relay\n                                       \
         token is then refreshed automatically | ligus auth status | ligus auth logout\n  \
         ligus setup [--apply]           detect providers; --apply defines keyless runtimes\n  \
         ligus config <export|import <file>>   round-trip the workspace runtime set\n\n\
         Config via env (override the saved config.toml from `enroll`):\n  \
         LIGUS_DATA_DIR, LIGUS_RELAY_URL, LIGUS_RELAY_ACCESS_TOKEN, LIGUS_WORKSPACE, LIGUS_WORKSPACE_KEY."
    );
    std::process::exit(2);
}

/// Whether the command line asks for help (`-h` / `--help` / `help` anywhere
/// before a `--` separator).
fn wants_help(args: &[String]) -> bool {
    args.first().is_some_and(|a| a == "help")
        || args.iter().take_while(|a| *a != "--").any(|a| a == "--help" || a == "-h")
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    // Inside the Linux agent sandbox this process is only the pre-exec helper.
    #[cfg(target_os = "linux")]
    hive_runtime::executor::linux::maybe_run_helper();
    // `ligus mcp-bridge`: inside an agent's sandbox, pipe its MCP stdio to
    // the turn's Ligus MCP socket (Track D). No config, no data dir.
    hive_runtime::ligus_mcp::bridge::maybe_run_bridge();
    // Before any thread starts: the only time Linux lets us read it.
    serve::dashboard_hive::init_local_offset();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(args.first().map(String::as_str), Some("--version" | "-V" | "version")) {
        println!("ligus {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let cfg = config();
    // Agent processes this host spawns never read its data dir (keys,
    // database, serve token); checked-out repos under it stay readable.
    hive_runtime::executor::configure_host(&cfg.data_dir, &[], &[repos::repos_dir(&cfg.data_dir)]);
    // Turn scratch dirs a killed turn or host left behind are swept (now and
    // every ten minutes) once older than the longest turn.
    hive_runtime::executor::start_turn_dir_sweeper(worker::turn_timeout());
    // Ligus's own secret env values are masked wherever agent output is
    // logged (relay tokens, the workspace key, provider keys passed in).
    hive_runtime::secret_redact::register_env_secrets();
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let rest = &args[args.len().min(1)..];
    // `hive <cmd> --help` prints usage instead of running the command (a bare
    // `ligus serve --help` used to start a real server on the default data dir).
    if wants_help(&args) {
        usage();
    }
    let has = |flag: &str| rest.iter().any(|a| a == flag);
    let flag_val = |flag: &str| {
        rest.iter()
            .position(|a| a == flag)
            .and_then(|i| rest.get(i + 1))
            .cloned()
    };
    // Positional args exclude flags and any value immediately after a known
    // value-flag (so `agent bot --runtime ws1` → positional = [bot]).
    let value_flags = [
        "--runtime",
        "--endpoint",
        "--secret-ref",
        "--secret",
        "--label",
        "--host",
        "--chat",
        "--listen",
        "--token-file",
        "--ntfy-url",
        "--ntfy-topic",
        "--public-url",
        "--name",
        "--role",
        "--relay",
        "--reg-token",
        "--remote-relay",
        "--file",
        "--repo",
        "--since",
        "--auth",
        "--client-id",
    ];
    let mut positional: Vec<&String> = Vec::new();
    let mut skip = false;
    for a in rest {
        if skip {
            skip = false;
            continue;
        }
        if a.starts_with("--") {
            if value_flags.contains(&a.as_str()) {
                skip = true;
            }
            continue;
        }
        positional.push(a);
    }

    // A current-thread runtime is all the CLI needs — it only ever `block_on`s
    // (never spawns), and keeps the non-Send EventStore on one thread.
    let rt = || -> Result<tokio::runtime::Runtime> {
        Ok(tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?)
    };

    // A ligus-auth sign-in (see `ligus auth`) supplies the relay token and
    // keeps it fresh in the background for the life of this process.
    if !matches!(cmd, "auth" | "") {
        auth::init(&cfg.data_dir);
    }

    match cmd {
        "auth" => auth::cmd_auth(&cfg.data_dir, &positional, flag_val("--label")),
        "whoami" => cmd_whoami(&cfg),
        "chats" => cmd_chats(&cfg),
        "new" => cmd_new(&cfg, positional.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ")),
        "tail" => {
            let id = positional
                .first()
                .ok_or_else(|| anyhow!("usage: ligus tail <chat-id> [--watch]"))?;
            cmd_tail(&cfg, Uuid::parse_str(id)?, has("--watch"))
        }
        "send" => {
            let id = positional
                .first()
                .ok_or_else(|| anyhow!("usage: ligus send <chat-id> <message…> [--push]"))?;
            let chat_id = Uuid::parse_str(id)?;
            let body = positional[1..]
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            if body.trim().is_empty() {
                bail!("message is empty");
            }
            rt()?.block_on(cmd_send(&cfg, chat_id, body, has("--push")))
        }
        "sync" => rt()?.block_on(cmd_sync(&cfg, has("--watch"))),
        "runtimes" => cmd_runtimes(&cfg),
        "add-runtime" => {
            if positional.len() < 4 {
                bail!("usage: ligus add-runtime <id> <name> <provider> <model> [--endpoint <url>] [--secret-ref <name> | --secret <value>]");
            }
            cmd_add_runtime(
                &cfg,
                positional[0],
                positional[1],
                positional[2],
                positional[3],
                flag_val("--endpoint"),
                flag_val("--secret-ref"),
                flag_val("--secret"),
            )
        }
        "rm-runtime" => {
            let id = positional.first().ok_or_else(|| anyhow!("usage: ligus rm-runtime <id>"))?;
            cmd_rm_runtime(&cfg, id)
        }
        "enroll" => {
            let invite = positional
                .first()
                .ok_or_else(|| anyhow!("usage: ligus enroll <hivews1:invite> [--token <access-token>]"))?;
            cmd_enroll(&cfg, invite, flag_val("--token"))
        }
        "join" => {
            let code = positional
                .first()
                .ok_or_else(|| anyhow!("usage: ligus join <invite-code>"))?;
            rt()?.block_on(cmd_join(&cfg, code))
        }
        "setup" => cmd_setup(&cfg, has("--apply")),
        "config" => match positional.first().map(|s| s.as_str()) {
            Some("export") => cmd_config_export(&cfg),
            Some("import") => {
                let path = positional
                    .get(1)
                    .ok_or_else(|| anyhow!("usage: ligus config import <file.toml>"))?;
                cmd_config_import(&cfg, path)
            }
            _ => bail!("usage: ligus config <export|import <file.toml>>"),
        },
        "agents" => cmd_agents(&cfg),
        "proposals" => cmd_proposals(&cfg),
        "add-agent" => {
            if positional.len() < 2 {
                bail!("usage: ligus add-agent <name> <runtime-id> [--host <id>] [--role <role>]");
            }
            cmd_add_agent(&cfg, positional[0], positional[1], flag_val("--host"), flag_val("--role"))
        }
        "hosts" => cmd_hosts(&cfg),
        "register-worker" => {
            let id = cmd_register_worker(&cfg, flag_val("--label"))?;
            println!("registered this box as worker host {id} (sync to share).");
            Ok(())
        }
        "set-agent-host" => {
            if positional.len() < 2 {
                bail!("usage: ligus set-agent-host <agent-name> <host-id>");
            }
            cmd_set_agent_host(&cfg, positional[0], positional[1])
        }
        "worker" => rt()?.block_on(cmd_worker(&cfg, flag_val("--label"))),
        "queue" => cmd_queue(&cfg),
        "serve" if positional.first().map(|s| s.as_str()) == Some("enroll") => {
            let opts = serve::ServeOptions::from_flags(
                &cfg,
                flag_val("--listen"),
                None,
                None,
                None,
                None,
                flag_val("--label"),
                None,
                false,
            )?;
            serve::cmd_enroll(&cfg, &opts, has("--new-device"))
        }
        "serve" => {
            let opts = serve::ServeOptions::from_flags(
                &cfg,
                flag_val("--listen"),
                flag_val("--token-file"),
                flag_val("--ntfy-url"),
                flag_val("--ntfy-topic"),
                flag_val("--public-url"),
                flag_val("--label"),
                flag_val("--name"),
                has("--pair"),
            )?;
            let mut opts = opts;
            opts.remote = serve::tunnel::RelaySettings::resolve(
                &cfg.data_dir,
                flag_val("--remote-relay"),
                flag_val("--reg-token"),
                has("--insecure-relay"),
            )?;
            serve::cmd_serve(cfg, opts)
        }
        "pair" => {
            let opts = serve::ServeOptions::from_flags(
                &cfg,
                flag_val("--listen"),
                flag_val("--token-file"),
                None,
                None,
                flag_val("--public-url"),
                None,
                None,
                true,
            )?;
            let mut opts = opts;
            opts.remote = serve::tunnel::RelaySettings::resolve(
                &cfg.data_dir,
                flag_val("--remote-relay"),
                flag_val("--reg-token"),
                has("--insecure-relay"),
            )?;
            serve::cmd_pair(&cfg.data_dir, &opts, has("--relay-only"))
        }
        "remote" => {
            use serve::tunnel::cli;
            match positional.first().map(|s| s.as_str()) {
                Some("enable") => cli::remote_enable(&cfg.data_dir, flag_val("--relay"), flag_val("--reg-token"), has("--insecure-relay")),
                Some("disable") => cli::remote_disable(&cfg.data_dir),
                Some("status") | None => cli::remote_status(&cfg.data_dir),
                Some("reset") => cli::remote_reset(&cfg.data_dir),
                Some(other) => bail!("unknown `ligus remote {other}`: use enable|status|disable|reset"),
            }
        }
        "facts" => {
            if positional.first().map(|s| s.as_str()) != Some("export") || !has("--jsonl") {
                bail!("usage: ligus facts export --jsonl [--since <unix ms>]");
            }
            let since: i64 = flag_val("--since").map(|v| v.parse()).transpose().map_err(|_| anyhow!("--since takes unix milliseconds"))?.unwrap_or(0);
            for line in open_service(&cfg)?.export_facts(since)? {
                println!("{line}");
            }
            Ok(())
        }
        "trust" => {
            let repo = flag_val("--repo").map(PathBuf::from).map(Ok).unwrap_or_else(std::env::current_dir)?;
            cmd_trust(&cfg, &repo, positional.first().map(|s| s.as_str()), positional.get(1).map(|s| s.as_str()))
        }
        "phones" => {
            use serve::tunnel::cli;
            match positional.first().map(|s| s.as_str()) {
                Some("list") | None => cli::phones_list(&cfg.data_dir),
                Some("revoke") => {
                    let token_file = flag_val("--token-file")
                        .or_else(|| env_opt("LIGUS_SERVE_TOKEN_FILE"))
                        .map(PathBuf::from)
                        .unwrap_or_else(|| cfg.data_dir.join("serve.token"));
                    cli::phones_revoke(&cfg.data_dir, positional.get(1).map(|s| s.as_str()), &token_file, has("--keep-token"))
                }
                Some(other) => bail!("unknown `ligus phones {other}`: use list|revoke"),
            }
        }
        "dashboard" => match positional.first().map(|s| s.as_str()) {
            Some("check") => serve::dashboard::cmd_check(&cfg, flag_val("--file")),
            Some("default") => {
                print!("{}", serve::dashboard::default_toml()?);
                Ok(())
            }
            Some("metrics") => {
                print!("{}", serve::dashboard::metrics_help());
                Ok(())
            }
            _ => bail!("usage: ligus dashboard <check [--file PATH] | default | metrics>"),
        },
        "bind-repo" => repos::cmd_bind_repo(
            &cfg.data_dir,
            positional.first().map(|s| s.as_str()),
            flag_val("--chat").as_deref(),
            has("--clear"),
        ),
        "repos" => repos::cmd_repos(&cfg.data_dir),
        "forge" => {
            let pos: Vec<&str> = positional.iter().map(|s| s.as_str()).collect();
            forge_cmd::cmd(&cfg.data_dir, &pos, flag_val("--auth"), flag_val("--client-id"))
        }
        "agent" => {
            let name = positional.first().map(|s| s.as_str()).unwrap_or("assistant").to_string();
            let runtime_id = flag_val("--runtime").or_else(|| env_opt("LIGUS_RUNTIME"));
            rt()?.block_on(cmd_agent(&cfg, name, runtime_id))
        }
        _ => usage(),
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    /// config.toml holds the workspace E2EE key and relay token.
    #[cfg(unix)]
    #[test]
    fn enroll_saves_the_connection_owner_only() {
        use base64::Engine;
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let cfg = Config {
            data_dir: tmp.path().join("data"),
            relay_url: None,
            token: None,
            room: "r".into(),
            key: None,
            epoch_keys: Vec::new(),
        };
        let body = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"relay_url":"https://relay.example","room":"r","key":"test-passphrase"}"#);
        cmd_enroll(&cfg, &format!("hivews1:{body}"), Some("hrt1-test".into())).unwrap();
        let mode = std::fs::metadata(config_path(&cfg.data_dir)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "config.toml is {mode:o}");
        let dir = std::fs::metadata(&cfg.data_dir).unwrap().permissions().mode() & 0o077;
        assert_eq!(dir, 0, "data dir is readable by others");
    }

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn help_flags_never_run_the_command() {
        assert!(wants_help(&argv(&["serve", "--help"])));
        assert!(wants_help(&argv(&["serve", "-h"])));
        assert!(wants_help(&argv(&["help"])));
        assert!(!wants_help(&argv(&["serve", "--pair"])));
        assert!(!wants_help(&argv(&["send", "c", "--", "--help"])));
    }

    #[test]
    fn custom_provider_is_accepted() {
        assert_eq!(provider_kind("custom").unwrap(), ModelProviderKind::Custom);
        assert_eq!(provider_kind("Custom").unwrap(), ModelProviderKind::Custom);
        assert_eq!(provider_kind("agy").unwrap(), ModelProviderKind::Antigravity);
        assert_eq!(provider_kind("copilot").unwrap(), ModelProviderKind::Copilot);
        assert!(provider_kind("nope").is_err());
    }
}
