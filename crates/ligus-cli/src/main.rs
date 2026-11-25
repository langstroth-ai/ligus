//! `ligus` — a headless Ligus client.
//!
//! Runs the same runtime the desktop app does (`ligus-core` + `ligus-runtime`),
//! wired to flags/env instead of a GUI, so a cloud-hosted agent (or a script)
//! can connect to a relay, sync a workspace's E2EE event log, and read/write
//! signed events without the full application. This is the "worker" client the
//! design spec §12 (D21–D23) calls for; v0 proves the path end-to-end:
//! connect → sync → read → post.
//!
//! Config (all via env; a persistent data dir keeps a stable identity):
//!   LIGUS_DATA_DIR            where identity + ligus.db live (default: $HOME/.ligus)
//!   LIGUS_RELAY_URL           relay base URL (needed for `sync`/`send --push`)
//!   LIGUS_RELAY_ACCESS_TOKEN  relay access token (hrt1/hex), provisioned out-of-band
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
use ligus_core::{
    workspace_config_session_id, HostKind, MessageRole, ModelProviderKind,
    Timestamp, WorkspaceAgent, WorkspaceCredential, WorkspaceHost, WorkspaceRuntime,
};
use ligus_runtime::{
    parse_mentions, pending_mentions, resolve_workspace_credential, turns_for, ChatService,
    ChatTurn, EventStore, IdentityStore, RelayClient, ResolvedRuntime, SyncEngine,
};
use ligus_runtime::display::shown;
use ligus_runtime::secret_store::StoreKeyVault;
use uuid::Uuid;

mod admit;
mod app_cmd;
mod auth;
mod checks;
mod cli_flags;
mod devices_cmd;
mod link_io;
mod forge_cmd;
mod gateway_check;
mod hardening;
mod help;
mod mcp_config;
mod problem;
mod memory_cmd;
mod selftest;
mod reply;
mod repos;
mod repos_cmd;
mod secret_arg;
mod serve;
mod setup_cmd;
mod worker;

#[derive(Clone)]
struct Config {
    data_dir: PathBuf,
    relay_url: Option<String>,
    token: Option<String>,
    room: String,
    key: Option<String>,
    /// Epoch keys an invite delivered (B1 newcomer bootstrap).
    epoch_keys: Vec<ligus_runtime::keyring::DeliveredKey>,
}

/// Env lookup (blank = unset; `LIGUS_*` names only, see `ligus_core::names`).
fn env_opt(k: &str) -> Option<String> {
    ligus_core::names::env_opt(k)
}

/// Create the data dir. Exits when it can't be created.
/// Owner-only (0700) from the start: `ligus.db` holds the decrypted history,
/// so it must not wait for a later private write to tighten the dir.
fn create_data_dir_or_exit(data_dir: &std::path::Path) {
    if let Err(e) = ligus_runtime::private_file::ensure_private_dir(data_dir) {
        eprintln!("ligus: could not set up {}: {e}", data_dir.display());
        std::process::exit(1);
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
    ek: Vec<ligus_runtime::keyring::DeliveredKey>,
}

fn config_path(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("config.toml")
}

/// The secret-store scope of this CLI's connection secrets.
const CLI_SCOPE: &str = "cli";
use ligus_runtime::connection_settings::ConnectionSecrets;

impl FileConfig {
    /// `config.toml`, with its secrets from the store. Secrets still written
    /// in the file in the clear are dropped and the file rewritten without
    /// them (0.1 starts from scratch: re-run `ligus enroll`).
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
        let store = ligus_runtime::secret_store::for_data_dir(data_dir);
        let secrets = ConnectionSecrets::load(store.as_ref(), CLI_SCOPE, "");
        (f.token, f.key, f.ek) = (secrets.token, secrets.key, secrets.ek);
        f
    }

    /// Write `config.toml` (owner-only, no secrets) and the secrets to the store.
    fn save(&self, data_dir: &std::path::Path) -> Result<()> {
        let store = ligus_runtime::secret_store::for_data_dir(data_dir);
        let secrets = ConnectionSecrets { token: self.token.clone(), key: self.key.clone(), ek: self.ek.clone() };
        secrets.save(store.as_ref(), CLI_SCOPE, "")?;
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
        .or_else(|| env_opt("HOME").map(|h| ligus_core::names::home_data_dir(std::path::Path::new(&h))))
        .unwrap_or_else(|| PathBuf::from("./ligus-data"));
    create_data_dir_or_exit(&data_dir);
    // B2: secrets come from the OS secret store from here on. `ligus doctor`
    // opens it with keychain prompts off: checking never asks.
    if std::env::args().skip(1).find(|a| !a.starts_with('-')).as_deref() == Some("doctor") {
        ligus_runtime::secret_store::without_prompts(|| ligus_runtime::secret_store::init_for_data_dir(&data_dir, None));
    } else {
        ligus_runtime::secret_store::init_for_data_dir(&data_dir, None);
    }
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

/// Before any command opens the store (identity model v2, 10.4 and 11):
/// data a newer Ligus wrote stops here with exit 3 (`ligus doctor` reports it
/// instead), and opening it once moves the redeem key seeds to the secret
/// store (M2). Then the sync state is keyed by relay and workspace (M1),
/// when this process has the store alone; otherwise on a later start.
fn prepare_store(cfg: &Config, command: &str) -> Result<()> {
    let db = cfg.data_dir.join("ligus.db");
    if !db.exists() {
        return Ok(());
    }
    let store = match EventStore::open(&db) {
        Ok(s) => s,
        // (The command opens it again and says why it can't.)
        Err(_) => return Ok(()),
    };
    if store.newer_schema().is_some() {
        if command == "doctor" {
            return Ok(());
        }
        return Err(newer_data_problem(cfg));
    }
    let conns = match sync_connections(cfg) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ligus: serve-dms.json is unreadable ({e}); the sync state stays as it is for now");
            return Ok(());
        }
    };
    match store.migrate_sync_tables(&conns) {
        Ok(ligus_runtime::event_store::SyncMigration::Migrated { backup: Some(b), reset, .. }) => {
            eprintln!("ligus: upgraded the local sync state (backup: {})", b.display());
            if !reset.is_empty() {
                eprintln!("ligus: {} room(s) named on more than one relay, or on none, sync again from the start", reset.len());
            }
        }
        Ok(_) => {}
        Err(e) => eprintln!("ligus: upgrading the local sync state failed, it runs again on the next start: {e}"),
    }
    Ok(())
}

/// Exit 3: the data dir holds data a newer Ligus wrote.
fn newer_data_problem(cfg: &Config) -> anyhow::Error {
    anyhow::Error::new(problem::Problem {
        exit: problem::EXIT_FIXABLE,
        code: "newer_data",
        message: ligus_runtime::event_store::NEWER_DATA_MESSAGE.to_string(),
        fix: Some(format!("install the newer Ligus, or point LIGUS_DATA_DIR at another data dir (this one is {})", cfg.data_dir.display())),
    })
}

/// This data dir's relay connections as (relay URL, room): its workspace
/// and each DM `ligus serve` holds.
fn sync_connections(cfg: &Config) -> Result<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> = cfg.relay_url.iter().map(|u| (u.clone(), cfg.room.clone())).collect();
    out.extend(serve::dms::DmStore::connections_checked(&cfg.data_dir)?);
    Ok(out)
}

/// Decode a `hivews1:` workspace invite → (relay_url, room, key, invite id,
/// delivered epoch keys, founder pin), with the codec every client shares
/// (`ligus_runtime::workspace_invite`).
#[allow(clippy::type_complexity)]
fn decode_ligusws_invite_full(
    invite: &str,
) -> Result<(String, String, Option<String>, Option<Uuid>, Vec<ligus_runtime::keyring::DeliveredKey>, Option<(String, String)>)> {
    let c = ligus_runtime::workspace_invite::decode_workspace_invite(invite).map_err(|e| anyhow!(e))?;
    let (inv, pin) = (c.invite_id(), c.founder_pin());
    Ok((c.relay_url, c.room, c.key, inv, c.ek, pin))
}

/// A relay URL a box can sync with: an http(s) address with a host.
use ligus_runtime::workspace_invite::usable_relay_url;

/// A relay URL as it's safe to print: scheme://host[:port] only. A URL can
/// carry user:password or a token in its path, query or fragment, and
/// doctor and status output is pasted into chats and issues.
fn relay_origin(url: &str) -> String {
    let url = url.trim();
    if let Ok(u) = reqwest::Url::parse(url) {
        if let Some(host) = u.host() {
            return match u.port() {
                Some(p) => format!("{}://{host}:{p}", u.scheme()),
                None => format!("{}://{host}", u.scheme()),
            };
        }
    }
    // Not a URL we can parse: keep the scheme, drop everything after the
    // authority and any userinfo inside it.
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    if scheme.is_empty() { host.to_string() } else { format!("{scheme}://{host}") }
}

/// Why this box can't sync, named before any HTTP is tried: no relay URL,
/// or one that isn't an http(s) address (an invite from a relayless
/// workspace once enrolled as `relay_url = ""`).
fn relay_url_problem(relay_url: Option<&str>) -> Option<String> {
    const FIX: &str = "re-enroll with an invite from a workspace that has one (set it in the desktop app under Settings → Team sync → Relay URL), or set LIGUS_RELAY_URL";
    match relay_url.map(str::trim) {
        None | Some("") => Some(format!("this box's workspace has no relay URL; {FIX}")),
        Some(r) if !usable_relay_url(r) => {
            Some(format!("this box's workspace has no relay URL it can use ({:?} is not an http(s) address); {FIX}", relay_origin(r)))
        }
        Some(_) => None,
    }
}

/// `ligus enroll - | --file PATH [--token - | @FILE]` — one-command bootstrap:
/// decode the invite, persist the connection to config.toml, and (with --token)
/// wire the relay identity. After this a headless box just runs `ligus worker`.
fn cmd_enroll(cfg: &Config, invite: &str, token: Option<String>) -> Result<()> {
    let (relay_url, room, key, inv, ek, pin) = decode_ligusws_invite_full(invite)?;
    // A box joins through the relay; an invite from a workspace with none
    // can't be used here. Refuse before writing anything.
    if !usable_relay_url(&relay_url) {
        bail!(
            "this invite can't be used: its workspace has no relay{}. In the desktop app, set one under Settings → Team sync (Relay URL), then copy a new invite.",
            if relay_url.trim().is_empty() { String::new() } else { format!(" (relay URL {:?} is not an http(s) address)", relay_url.trim()) }
        );
    }
    let relay_url = relay_url.trim().to_string();
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
    let store = EventStore::open(cfg.data_dir.join("ligus.db"))?;
    if let Some(inv) = inv {
        store.set_join_invite(uuid_of_room(&room), inv)?;
        // Its redeem key proves the redemption (`ligus_core::admission`).
        store.set_join_redeem(uuid_of_room(&room), ligus_runtime::box_admission::redeem_key_of(invite).as_ref())?;
    }
    if let Some((founder, fk)) = pin {
        store.set_workspace_pin(uuid_of_room(&room), &founder, &fk)?;
    }
    println!("Enrolled workspace '{room}' at {relay_url}.");
    println!("Saved to {}.", config_path(&cfg.data_dir).display());
    if f.token.is_none() {
        println!("No access token set — for a membership-enforcing relay, add one:");
        println!("  ligus enroll --file <invite-file> --token @<token-file>   (or export LIGUS_RELAY_ACCESS_TOKEN)");
    }
    println!("Next: `ligus sync`. (A serve or worker box doesn't enroll: it joins with `ligus admit request` and `ligus admit redeem`, see `ligus help box`.)");
    Ok(())
}

/// `ligus join <invite-code>` — redeem a relay invite code to self-enroll this
/// box's identity in the configured workspace at the invite's role.
async fn cmd_join(cfg: &Config, code: &str) -> Result<()> {
    if let Some(problem) = relay_url_problem(cfg.relay_url.as_deref()) {
        bail!(problem);
    }
    let relay_url = cfg.relay_url.as_deref().unwrap_or_default().trim();
    let relay = RelayClient::new(relay_url)
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

/// The name a new box identity starts with: its label (`--label` /
/// `LIGUS_SERVE_LABEL`), else the machine's host name.
pub(crate) fn box_name(label: Option<String>, hostname: Option<String>) -> String {
    label
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .or(hostname)
        .unwrap_or_else(|| "Ligus box".into())
}

/// This data dir's identity, created on first use with [`box_name`].
pub(crate) fn bootstrap_identity(identity: &IdentityStore<impl ligus_runtime::KeyVault>) -> Result<ligus_runtime::StoredIdentity> {
    let name = box_name(env_opt("LIGUS_SERVE_LABEL"), ligus_runtime::machine_hostname());
    Ok(identity.bootstrap(&name, &name, &name)?)
}

/// Give a box's identity its label `name` when it still carries a name
/// nobody chose: `auto` (the host name it was created with before a label was
/// known, e.g. by `ligus enroll`).
/// Returns the new name when it changed.
pub(crate) fn adopt_box_name(identity: &IdentityStore<impl ligus_runtime::KeyVault>, name: &str, auto: Option<&str>) -> Result<Option<String>> {
    let name = name.trim();
    let current = bootstrap_identity(identity)?;
    let unchosen = auto.is_some_and(|a| current.account.display_name == a);
    if !unchosen || name.is_empty() || name == current.account.display_name {
        return Ok(None);
    }
    identity.update_display_name(name)?;
    Ok(Some(name.to_string()))
}

/// Open the local identity + event store and build a ChatService. Bootstraps a
/// fresh identity on first run (idempotent thereafter).
fn open_service(cfg: &Config) -> Result<ChatService> {
    open_service_as(cfg, false)
}

/// [`open_service`]; `linking` lets `ligus devices redeem` open a linked
/// install in the workspace it is being linked into, before the link is
/// recorded. Otherwise a linked install opens only its linked workspaces:
/// anywhere else it would act as its old identity with a device key that is
/// the person's.
fn open_service_as(cfg: &Config, linking: bool) -> Result<ChatService> {
    std::fs::create_dir_all(&cfg.data_dir)?;
    let identity = IdentityStore::new(&cfg.data_dir, StoreKeyVault::for_data_dir(&cfg.data_dir));
    let stored = bootstrap_identity(&identity)?;
    let device_kp = identity.device_keypair(stored.device.id)?;
    let account_kp = identity.account_keypair(stored.account.id)?;
    let store = EventStore::open(cfg.data_dir.join("ligus.db"))?;
    let mut svc = ChatService::new(store, device_kp, account_kp, stored.actor());
    // A computer linked as a person's device ("Add a computer") acts as them
    // in the workspaces it was linked into, without their account key.
    let link = ligus_runtime::computer_link::LinkState::load(&cfg.data_dir)
        .map_err(|e| anyhow!("{} is unreadable ({e}): fix or remove it", ligus_runtime::computer_link::LinkState::path(&cfg.data_dir).display()))?;
    // Which person this computer is here, from the log (identity model v2,
    // 7.2): the device's own choice, `computer-link.json` only a cache. A
    // certificate someone else put on this device's key changes nothing.
    let ws = uuid_of_room(&cfg.room);
    if link.is_linked() {
        use ligus_runtime::device_binding::Binding;
        let binding = svc.binding(ws, &link, stored.account.id).unwrap_or(Binding::Pending { account: link.account_in(ws).unwrap_or_default() });
        let act_as = |svc: &mut ChatService, account: Uuid| {
            if let Some(person) = svc.person_actor(ws, account, link.person.as_ref()) {
                svc.act_as_owner_device(person);
            }
        };
        match binding {
            Binding::Linked { account } => act_as(&mut svc, account),
            Binding::NotLinkedHere if !linking => {
                let name = link.person.as_ref().map(|p| shown(&p.display_name)).unwrap_or_else(|| "a person".into());
                return Err(anyhow::Error::new(problem::Problem {
                    exit: problem::EXIT_REFUSED,
                    code: "linked_device",
                    message: format!("this computer is one of {name}'s devices and isn't linked into workspace '{}'", cfg.room),
                    fix: Some("add it to that workspace from your desktop (My devices > Add a computer), or use another LIGUS_DATA_DIR".into()),
                }));
            }
            Binding::NotLinkedHere | Binding::Own => {}
            other => {
                if let Some(account) = link.account_in(ws) {
                    act_as(&mut svc, account);
                }
                // Rules 3: a redeem that couldn't accept its certificate
                // (offline, interrupted) accepts it now.
                let healed = matches!(other, Binding::Pending { .. })
                    && svc.accept_device_certificate(ws).unwrap_or(false)
                    && svc.binding(ws, &link, stored.account.id).is_ok_and(|b| b.writable());
                if !linking && !healed {
                    svc.set_write_block(other.why_read_only());
                }
            }
        }
    }
    // Agents bound to this host sign their own output with keys this device
    // certifies (A3); the keys live with this host's other keys.
    svc.set_agent_vault(Box::new(StoreKeyVault::for_data_dir(&cfg.data_dir)));
    // Tests: a person's passkey prompts are answered by their test recovery
    // key (rules v2 governance needs presence; a test has no browser).
    #[cfg(test)]
    svc.enable_test_presence();
    Ok(svc)
}

/// `ligus facts export`: read-only, so it never creates a data dir, an
/// identity or a database. An observability tool polling a mistyped
/// `LIGUS_DATA_DIR` gets an error, not a fresh empty device.
fn cmd_facts_export(cfg: &Config, since: ligus_runtime::facts::Since, now_ms: i64) -> Result<Vec<String>> {
    let db = cfg.data_dir.join("ligus.db");
    if !db.is_file() {
        bail!("no Ligus database at {}: facts export reads an existing data dir and never creates one", db.display());
    }
    Ok(open_service(cfg)?.export_facts_since(since, now_ms)?)
}

/// One-time cleanup: drop events of any workspace other than this config's
/// room that this box never wrote in — events older clients pushed into the
/// room regardless of their workspace (the sync engine now refuses both ways).
/// The database is backed up next to itself first; nothing is deleted unless
/// that backup succeeds. A no-op (no backup) once clean.
/// The workspaces this data dir syncs: its room and the box's own direct
/// messages (`serve-dms.json`).
/// An unreadable `serve-dms.json` is an error (never "no DMs").
fn kept_workspaces(cfg: &Config) -> Result<HashSet<Uuid>> {
    let mut keep = HashSet::from([uuid_of_room(&cfg.room)]);
    keep.extend(serve::dms::DmStore::workspace_ids_checked(&cfg.data_dir)?);
    Ok(keep)
}

fn purge_foreign_workspaces(cfg: &Config) -> Result<()> {
    let db = cfg.data_dir.join("ligus.db");
    if !db.exists() {
        return Ok(());
    }
    let identity = IdentityStore::new(&cfg.data_dir, StoreKeyVault::for_data_dir(&cfg.data_dir));
    let own_device = bootstrap_identity(&identity)?.device.id;
    let keep = match kept_workspaces(cfg) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("ligus: serve-dms.json is unreadable ({e}); skipping the foreign-workspace cleanup so no direct message is deleted");
            return Ok(());
        }
    };
    let mut store = EventStore::open(&db)?;
    if store.foreign_workspaces(&keep, own_device)?.is_empty() {
        return Ok(());
    }
    let backup = ligus_runtime::event_store::purge_backup_path(&cfg.data_dir);
    let dropped = store.purge_foreign_workspaces(&keep, own_device, Some(&backup))?;
    let events: usize = dropped.iter().map(|(_, n)| n).sum();
    println!(
        "ligus: removed {events} event(s) of {} other workspace(s) that don't belong to this room (backup: {}).",
        dropped.len(),
        backup.display()
    );
    Ok(())
}

/// One sync round against the relay: push everything unpushed, then fetch +
/// ingest. Uses its own EventStore connection (shares the WAL file), as the app
/// does. Returns (pushed, pulled) counts.
/// Open the store from async code: opening can wait (up to the busy
/// timeout) for another connection's lock on the file, which must not hold
/// up a runtime worker thread.
async fn open_store(path: std::path::PathBuf) -> Result<EventStore> {
    Ok(tokio::task::spawn_blocking(move || EventStore::open(path)).await??)
}

/// [`open_store`] for a long-lived loop: while the file stays locked past
/// the busy timeout (the first open after an upgrade builds an index under
/// the write lock, a large store's takes a while), try again with a growing
/// pause instead of giving up, as long as `keep_trying` says so.
async fn open_store_patiently(path: std::path::PathBuf, what: &str, keep_trying: impl Fn() -> bool) -> Result<EventStore> {
    let mut pause = std::time::Duration::from_secs(1);
    loop {
        match open_store(path.clone()).await {
            Err(e) if store_busy(&e) && keep_trying() => {
                eprintln!("{what}: the store is busy ({e}); trying again in {}s", pause.as_secs());
                tokio::time::sleep(pause).await;
                pause = (pause * 2).min(std::time::Duration::from_secs(30));
            }
            r => return r,
        }
    }
}

/// Whether `e` is SQLite's "database is locked" (busy past the timeout).
fn store_busy(e: &anyhow::Error) -> bool {
    e.downcast_ref::<ligus_runtime::EventStoreError>().is_some_and(ligus_runtime::EventStoreError::is_busy)
}

async fn sync_once(cfg: &Config) -> Result<(usize, usize)> {
    let mut engine = sync_engine(cfg)?;
    let mut store = open_store(cfg.data_dir.join("ligus.db")).await?;
    let (pushed, mut pulled) = sync_round(&mut engine, &mut store).await?;
    // A one-shot sync (a daemon's first sync, `ligus sync`, a worker tick) needs
    // the whole room before it acts: keep paging until caught up. Each round is
    // bounded and persists its cursor, so the cap only limits one call.
    if engine.is_behind() {
        pulled += engine.catch_up(&mut store, SYNC_ONCE_MAX_ROUNDS).await?;
    }
    // Writes the relay refused (workspace full, a quota) or a truncated
    // room: the sync went through, but say why events still wait.
    if let Some(att) = engine.attention() {
        eprintln!("sync: {}", att.message());
    }
    Ok((pushed, pulled))
}

/// Pull rounds one `sync_once` may run (each up to
/// `ligus_runtime::sync_engine::PULL_PAGES_PER_ROUND` pages of
/// `ENVELOPE_PAGE_LIMIT`): far beyond any real room, so it only stops a
/// runaway loop; the next call continues from the persisted cursor.
const SYNC_ONCE_MAX_ROUNDS: usize = 250;

/// The relay client this config syncs through.
fn relay_client(cfg: &Config) -> Result<RelayClient> {
    if let Some(problem) = relay_url_problem(cfg.relay_url.as_deref()) {
        bail!(problem);
    }
    let relay_url = cfg.relay_url.as_deref().unwrap_or_default().trim();
    Ok(RelayClient::new(relay_url)
        .with_auth(cfg.token.clone())
        .with_auth_cell(auth::cell()))
}

/// This box's keyring for its room (B1), from its store's pinned config fold:
/// what blobs and liveness beats seal under and open with.
fn box_ring(cfg: &Config) -> Option<ligus_runtime::keyring::KeyringEngine> {
    let src = key_source(cfg).ok()?;
    let store = EventStore::open(cfg.data_dir.join("ligus.db")).ok()?;
    Some(ligus_runtime::keyring::KeyringEngine::from_store(&store, ligus_core::room_workspace_id(&cfg.room), &src))
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
fn key_source(cfg: &Config) -> Result<ligus_runtime::keyring::KeySource> {
    let identity = IdentityStore::new(&cfg.data_dir, StoreKeyVault::for_data_dir(&cfg.data_dir));
    let device = bootstrap_identity(&identity)?.device.id;
    let ka = identity.key_agreement(device)?;
    Ok(ligus_runtime::keyring::KeySource::for_device(device, ka, cfg.key.as_deref(), &cfg.epoch_keys))
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
    Ok(ligus_runtime::sync_service::sync_round(engine, store).await?)
}

/// Files shared over the relay reach a headless agent only as delimited
/// untrusted data (issue #118): a text file's verified content, anything else
/// its description, never a path. No-op when no turn carries a marker.
async fn materialize_turns(cfg: &Config, session: &ligus_core::ChatSession, head: &ligus_core::attachment::AttachmentRules, me: &str, turns: &mut [ChatTurn]) {
    // A local path resolves only from this box's own author's messages.
    let own = ligus_runtime::attachment_blob::own_path_markers(session, me);
    for t in turns.iter_mut() {
        t.content = ligus_runtime::attachment_blob::defuse_foreign_path_markers(&t.content, &own);
    }
    if !turns.iter().any(|t| t.content.contains("[Attachment: ")) {
        return;
    }
    let placed = ligus_runtime::attachment_blob::placed_attachments(session);
    let relay = match (cfg.relay_url.clone(), cfg.key.as_ref()) {
        (Some(url), Some(_)) => Some(RelayClient::new(&url).with_auth(cfg.token.clone()).with_auth_cell(auth::cell())),
        _ => None,
    };
    // The one keyring (B1): blobs sealed at any epoch this box holds open.
    let ring = box_ring(cfg).unwrap_or_default();
    let cache = ligus_runtime::attachment_blob::verified_cache_dir(&cfg.data_dir);
    for t in turns.iter_mut() {
        if t.content.contains("[Attachment: ") {
            t.content = ligus_runtime::attachment_blob::materialize_for_agent(&t.content, &placed, head, relay.as_ref(), &cfg.room, &ring, &cache).await;
        }
    }
}

fn cmd_whoami(cfg: &Config, json: bool) -> Result<()> {
    let identity = IdentityStore::new(&cfg.data_dir, StoreKeyVault::for_data_dir(&cfg.data_dir));
    let stored = bootstrap_identity(&identity)?;
    // On a linked computer, who it acts as is the person, per workspace
    // (identity model v2, 7.5); its own pre-link account is retired here.
    let link = ligus_runtime::computer_link::LinkState::load(&cfg.data_dir).unwrap_or_default();
    let ws = uuid_of_room(&cfg.room);
    let acting = open_service(cfg).ok().map(|svc| (svc.author().clone(), svc.write_block().map(str::to_string)));
    let linked = link.is_linked();
    if json {
        let person = acting.as_ref().filter(|_| linked).map(|(a, block)| {
            serde_json::json!({
                "workspace": ws,
                "account": a.account_id,
                "memberId": a.id,
                "name": shown(&a.display_name),
                "readOnly": block,
            })
        });
        println!(
            "{}",
            serde_json::json!({
                "account": acting.as_ref().and_then(|(a, _)| a.account_id).unwrap_or(stored.account.id),
                "device": stored.device.id,
                "name": acting.as_ref().map(|(a, _)| shown(&a.display_name)).unwrap_or_else(|| shown(&stored.account.display_name)),
                "dataDir": cfg.data_dir,
                "room": cfg.room,
                "person": person,
                "localAccount": { "id": stored.account.id, "retired": linked },
            })
        );
        return Ok(());
    }
    if let (true, Some((a, block))) = (linked, &acting) {
        println!("acts as : {} (account {}), one of their devices", shown(&a.display_name), a.account_id.map(|x| x.to_string()).unwrap_or_default());
        if let Some(b) = block {
            println!("here    : read-only: {b}");
        }
        println!("local   : {} (this install's own account, not used while linked)", stored.account.id);
        println!("device  : {}", stored.device.id);
        println!("data dir: {}", cfg.data_dir.display());
        println!("room    : {}", cfg.room);
        return Ok(());
    }
    println!("account : {}", stored.account.id);
    println!("device  : {}", stored.device.id);
    println!("name    : {}", shown(&stored.account.display_name));
    println!("data dir: {}", cfg.data_dir.display());
    println!("room    : {}", cfg.room);
    Ok(())
}

/// What `ligus status` reports, read-only: never creates an identity.
fn status_report(cfg: &Config) -> serde_json::Value {
    use serde_json::json;
    let db = cfg.data_dir.join("ligus.db");
    let ws = uuid_of_room(&cfg.room);
    let identity = IdentityStore::new(&cfg.data_dir, StoreKeyVault::for_data_dir(&cfg.data_dir));
    let stored = identity.load().ok().flatten();
    let relay_problem = relay_url_problem(cfg.relay_url.as_deref());
    // The local copy and membership, only from a store that exists.
    let (mut copy, mut member, mut standing, mut admission) = (false, false, None::<String>, false);
    if let (Some(_), true) = (&stored, db.is_file()) {
        if let Ok(svc) = open_service(cfg) {
            if let Ok(Some(c)) = svc.load_in(ws, workspace_config_session_id(ws)) {
                copy = true;
                member = c.members.iter().any(|m| m.id == svc.author().id);
            }
            standing = svc.device_standing(ws).ok().map(|s| match s {
                ligus_runtime::device_enroll::Standing::Own => "own".to_string(),
                ligus_runtime::device_enroll::Standing::OwnerDevice(_) => "owner-device".to_string(),
                ligus_runtime::device_enroll::Standing::Revoked => "revoked".to_string(),
            });
            admission = svc.store().join_invite(ws).ok().flatten().is_some();
        }
    }
    // A link waiting for the desktop's code (expired requests don't count;
    // status only reads, the devices commands clean them up).
    let now = ligus_core::flow::now_ms();
    let linking = ligus_runtime::computer_link::LinkState::load(&cfg.data_dir)
        .ok()
        .and_then(|s| s.pending_status(now))
        .map(|p| (p, link_io::pending_line(&p, now)));
    let tunnel = serve::tunnel::tunnel_dir(&cfg.data_dir);
    let remote = serve::tunnel::RelaySettings::file(&tunnel);
    let remote: Option<serve::tunnel::RelaySettings> = std::fs::read(&remote).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let live: Option<serve::tunnel::LiveStatus> = std::fs::read(tunnel.join("status.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let sandbox = ligus_runtime::executor::sandbox_status();
    // A box (serve, worker) joins only through an admission, never an
    // invite; a person's CLI device enrolls with an invite.
    let is_box = stored.as_ref().is_some_and(|s| s.service);
    let join = if is_box {
        "admit this box: ligus admit request, an Admin approves it (desktop: workspace menu > Admit a box), then ligus admit redeem <code> (ligus help box)"
    } else if stored.is_none() {
        "a serve or worker box: ligus admit request (ligus help box); a person's own CLI: ligus enroll --file ./invite (ligus help quickstart)"
    } else {
        "a person's own CLI: ligus enroll --file ./invite (ligus help quickstart); for a box use a fresh data dir and ligus admit request (ligus help box)"
    };
    let next = if relay_problem.is_some() || cfg.key.is_none() {
        join
    } else if is_box && !member {
        if admission {
            "start ligus serve or ligus worker: it syncs first, then joins with the kept admission"
        } else {
            join
        }
    } else if !copy {
        if is_box { "start ligus serve or ligus worker (it syncs first)" } else { "ligus sync" }
    } else {
        "ready"
    };
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "dataDir": cfg.data_dir,
        "database": db.is_file(),
        "identity": stored.as_ref().map(|s| json!({
            "account": s.account.id,
            "device": s.device.id,
            "name": shown(&s.account.display_name),
            "box": s.service,
        })),
        "workspace": {
            "room": cfg.room,
            "relayUrl": cfg.relay_url.as_deref().map(relay_origin),
            "relayProblem": relay_problem,
            "accessToken": cfg.token.is_some() || auth::cell().is_some(),
            "workspaceKey": cfg.key.is_some(),
            "localCopy": copy,
            "member": member,
            "deviceStanding": standing,
            "admissionKept": admission,
        },
        "serve": {
            "tokenFile": cfg.data_dir.join("serve.token").is_file(),
            "remoteRelay": remote.as_ref().map(|r| json!({ "url": relay_origin(&r.relay), "enabled": r.enabled })),
            "live": live.as_ref().map(|l| json!({
                "connected": l.connected,
                "status": l.status,
                "push": l.push,
                "lastError": l.last_error,
                "description": serve::tunnel::cli::describe_status(l, serve::tunnel::now()),
            })),
        },
        "sandbox": { "tier": sandbox.tier(), "summary": sandbox.summary },
        "linking": linking.map(|(p, line)| json!({
            "startedAt": p.created_at,
            "approveBy": p.approve_by,
            "expiresAt": p.expires_at,
            "pending": p.count,
            "description": line,
        })),
        "next": next,
    })
}

/// `ligus status`: the data dir, identity, workspace, relay, admission and
/// serve state at a glance.
fn cmd_status(cfg: &Config, json: bool) -> Result<()> {
    let r = status_report(cfg);
    if json {
        println!("{r}");
        return Ok(());
    }
    let yn = |v: &serde_json::Value| if v.as_bool().unwrap_or(false) { "yes" } else { "no" };
    let w = &r["workspace"];
    println!("ligus {}", env!("CARGO_PKG_VERSION"));
    println!("data dir      : {}{}", cfg.data_dir.display(), if r["database"].as_bool() == Some(true) { "" } else { " (no ligus.db yet)" });
    match r["identity"].as_object() {
        Some(i) => println!(
            "identity      : {} (account {}, device {}){}",
            i["name"].as_str().unwrap_or(""),
            i["account"].as_str().unwrap_or(""),
            i["device"].as_str().unwrap_or(""),
            if i["box"].as_bool() == Some(true) { ", a box (service member)" } else { "" }
        ),
        None => println!("identity      : none yet (created on first use)"),
    }
    println!("workspace     : {}", cfg.room);
    match w["relayProblem"].as_str() {
        Some(p) => println!("relay         : {p}"),
        None => println!("relay         : {}", w["relayUrl"].as_str().unwrap_or("")),
    }
    println!("access token  : {}", yn(&w["accessToken"]));
    println!("workspace key : {}", yn(&w["workspaceKey"]));
    println!("local copy    : {}", yn(&w["localCopy"]));
    println!("member        : {}{}", yn(&w["member"]), w["deviceStanding"].as_str().map(|s| format!(" (device standing: {s})")).unwrap_or_default());
    if w["admissionKept"].as_bool() == Some(true) {
        println!("admission     : kept; the next serve or worker start joins with it");
    }
    let sv = &r["serve"];
    println!("serve token   : {}", yn(&sv["tokenFile"]));
    match sv["remoteRelay"].as_object() {
        Some(rr) => println!(
            "remote access : {} ({})",
            rr["url"].as_str().unwrap_or(""),
            if rr["enabled"].as_bool() == Some(true) { "enabled" } else { "disabled" }
        ),
        None => println!("remote access : off"),
    }
    if let Some(d) = sv["live"]["description"].as_str() {
        println!("serve link    : {d}");
    }
    if let Some(d) = r["linking"]["description"].as_str() {
        println!("linking       : {d}");
    }
    println!("agent sandbox : {}", r["sandbox"]["tier"].as_str().unwrap_or(""));
    println!("next          : {}", r["next"].as_str().unwrap_or(""));
    Ok(())
}

fn cmd_chats(cfg: &Config, json: bool) -> Result<()> {
    let svc = open_service(cfg)?;
    let config_id = workspace_config_session_id(uuid_of_room(&cfg.room));
    let (mut rows, mut any) = (Vec::new(), false);
    for id in svc.store().list_session_ids()? {
        // Never list the workspace-config log — it isn't a chat.
        if id == config_id {
            continue;
        }
        if let Some(s) = svc.load_resolving(id)? {
            // Skip config logs of other workspaces too (empty-title, has channels).
            if id == workspace_config_session_id(s.workspace_id) {
                continue;
            }
            let n = s.messages.iter().filter(|m| !m.superseded).count();
            if json {
                rows.push(serde_json::json!({ "id": s.id, "title": shown(&s.title), "messages": n, "channel": s.channel_id }));
                continue;
            }
            any = true;
            let ch = if s.channel_id.is_empty() { String::new() } else { format!("  #{}", s.channel_id) };
            println!("{}  {:<32}  {} msgs{}", s.id, truncate(&shown(&s.title), 32), n, ch);
        }
    }
    if json {
        println!("{}", serde_json::Value::Array(rows));
    } else if !any {
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
        let Some(s) = svc.load_in(ws, id)? else { continue };
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
    let Some(s) = svc.load_resolving(chat_id)? else {
        return Err(problem::not_found(format!("unknown chat {chat_id} (ligus chats lists them)")));
    };
    for m in s.messages.iter().skip(from).filter(|m| !m.superseded) {
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

/// `ligus send`. With `wait`, it also pushes, then syncs every few seconds
/// until someone else (a person or an agent) posts in the chat, prints what
/// they posted, and exits 0; after `wait` without an answer it exits 7.
async fn cmd_send(cfg: &Config, chat_id: Uuid, body: String, push: bool, wait: Option<Duration>) -> Result<()> {
    let mut svc = open_service(cfg)?;
    let Some(session) = svc.load_resolving(chat_id)? else {
        return Err(problem::not_found(format!("unknown chat {chat_id} (ligus chats lists them)")));
    };
    svc.post_user_message(chat_id, session.workspace_id, body)?;
    println!("posted.");
    if push || wait.is_some() {
        let (pushed, pulled) = sync_once(cfg).await?;
        println!("synced: pushed {pushed}, pulled {pulled}");
    }
    let Some(wait) = wait else {
        return Ok(());
    };
    // Ours is the last message now; an answer is any later one by another author.
    let posted = svc.load_resolving(chat_id)?.map(|s| s.messages.clone()).unwrap_or_default();
    let (seen, me) = (posted.len(), posted.last().map(|m| m.author.clone()).unwrap_or_default());
    drop(svc);
    let deadline = std::time::Instant::now() + wait;
    loop {
        if let Some(s) = open_service(cfg)?.load_resolving(chat_id)? {
            let answers: Vec<_> = s.messages.iter().skip(seen).filter(|m| !m.superseded && m.author != me).collect();
            if !answers.is_empty() {
                for m in answers {
                    println!("{:>9}  {}", m.author, m.body);
                }
                return Ok(());
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(anyhow::Error::new(problem::Problem {
                exit: problem::EXIT_OFFLINE,
                code: "timeout",
                message: format!("no answer in chat {chat_id} within {}s", wait.as_secs()),
                fix: Some(format!("wait longer (--timeout 15m), or follow it with ligus tail {chat_id} --watch")),
            }));
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
        let _ = sync_once(cfg).await;
    }
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
    let founded = svc.load_in(ws, ligus_core::workspace_config_session_id(ws))?.is_none();
    let session = svc.create_chat(title, ws, "")?;
    println!("{}", session.id);
    // Phase A: the CLI can found a workspace (rules v2 from its seed) but has
    // no passkey ceremony. Until the founder registers a passkey and a
    // recovery key from the desktop app (or the phone, once it has its own
    // device key), nobody can be let in and nothing governed.
    if founded && !svc.workspace_secured(ws).unwrap_or(false) {
        eprintln!(
            "This workspace isn't secured yet: open it in the Ligus desktop app and create a passkey and a \
             recovery key (Settings → Passkeys & keys). Until then nobody can join it and its membership, \
             roles, devices and keys can't change."
        );
    }
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
        gateway: None,
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
    dry_run: bool,
) -> Result<()> {
    let mut wr = WorkspaceRuntime::new(id, name, provider_kind(provider)?, model);
    wr.endpoint = endpoint.unwrap_or_default();
    wr.credential = match (secret_ref, secret) {
        (Some(r), _) => WorkspaceCredential::SecretRef { secret_ref: r },
        (None, Some(s)) => WorkspaceCredential::Inline { secret: s },
        (None, None) => WorkspaceCredential::None,
    };
    if dry_run {
        let cred = match &wr.credential {
            WorkspaceCredential::SecretRef { secret_ref } => format!("the secret ref {secret_ref}"),
            WorkspaceCredential::Inline { .. } => "an inline key (stored encrypted in the workspace)".into(),
            WorkspaceCredential::None => "no key".into(),
        };
        println!("dry run: would define runtime '{id}' ({name}: {provider}, model {model}, {cred}); nothing was signed or written.");
        return Ok(());
    }
    let mut svc = open_service(cfg)?;
    svc.add_workspace_runtime(uuid_of_room(&cfg.room), wr)?;
    println!("runtime '{id}' defined (sync to share it with the workspace).");
    Ok(())
}

fn cmd_rm_runtime(cfg: &Config, id: &str, dry_run: bool) -> Result<()> {
    if dry_run {
        let svc = open_service(cfg)?;
        let ws = uuid_of_room(&cfg.room);
        let known = svc.list_workspace_runtimes(ws)?.iter().any(|r| r.id == id);
        if !known {
            return Err(problem::not_found(format!("no runtime '{id}' in this workspace (ligus runtimes lists them)")));
        }
        println!("dry run: would remove runtime '{id}'; nothing was signed or written.");
        return Ok(());
    }
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
    let clis = ligus_runtime::cli_detect::detect_cli_agents(&ligus_runtime::cli_detect::DetectEnv::from_process(), true);

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
        use ligus_runtime::cli_detect::LoginState;
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
            if let Some(kind) = ligus_runtime::cli_detect::provider_for(&c.id) {
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
fn cmd_runtimes(cfg: &Config, json: bool) -> Result<()> {
    let svc = open_service(cfg)?;
    let rts = svc.list_workspace_runtimes(uuid_of_room(&cfg.room))?;
    if json {
        let rows: Vec<serde_json::Value> = rts
            .iter()
            .map(|r| {
                let (kind, secret_ref, present) = match &r.credential {
                    WorkspaceCredential::SecretRef { secret_ref } => {
                        ("secretRef", Some(secret_ref.clone()), Some(ligus_core::names::var(&WorkspaceCredential::env_var(secret_ref)).is_ok()))
                    }
                    WorkspaceCredential::Inline { .. } => ("inline", None, None),
                    WorkspaceCredential::None => ("keyless", None, None),
                };
                serde_json::json!({
                    "id": r.id,
                    "name": shown(&r.name),
                    "provider": r.provider,
                    "model": r.model,
                    "endpoint": r.endpoint,
                    "credential": kind,
                    "secretRef": secret_ref,
                    "secretPresent": present,
                })
            })
            .collect();
        println!("{}", serde_json::Value::Array(rows));
        return Ok(());
    }
    if rts.is_empty() {
        println!("(no workspace runtimes — define one in the app, or `ligus sync` a workspace that has them)");
    }
    for r in rts {
        let cred = match &r.credential {
            WorkspaceCredential::SecretRef { secret_ref } => {
                let have = ligus_core::names::var(&WorkspaceCredential::env_var(secret_ref)).is_ok();
                format!("secretRef:{secret_ref}{}", if have { " ✓" } else { " (missing)" })
            }
            WorkspaceCredential::Inline { .. } => "inline".into(),
            WorkspaceCredential::None => "keyless".into(),
        };
        println!("{:<16} {:<22} {:?}/{}  [{cred}]", r.id, shown(&r.name), r.provider, r.model);
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
    Ok((workspace_runtime_resolved(&wr), format!("workspace:{}", wr.name)))
}

/// `ligus agent`'s runtime from a workspace runtime. A runtime that names a
/// model gateway goes through it (dispatch refuses it where this host
/// doesn't offer it), without the runtime's own key, as on serve and workers
/// ([`worker::agent_workspace_runtime`]).
fn workspace_runtime_resolved(wr: &ligus_core::WorkspaceRuntime) -> ResolvedRuntime {
    let gateway = Some(wr.gateway.trim()).filter(|g| !g.is_empty()).map(ligus_runtime::model_gateway::GatewaySelection::named);
    let api_key = if gateway.is_some() { None } else { resolve_workspace_credential(&wr.credential) };
    ResolvedRuntime {
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
        gateway,
    }
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
    if ligus_core::names::var("LIGUS_ALLOW_PERSONAL_AGENT").is_err() {
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
            if let Some(s) = svc.load_resolving(id)? {
                seen.extend(s.messages.iter().map(|m| m.id));
            }
        }
    }
    ligus_runtime::author_gate::warn_if_lowered(ligus_runtime::author_gate::turn_writes(rt.provider, &rt.args));
    println!("agent @{name} online ({rt_label}); replying to @primary / un-mentioned turns. Ctrl-C to stop.");
    let host_label = env_opt("HOSTNAME").unwrap_or_else(|| "this worker".into());
    let system = format!(
        "You are @{name}, an agent in a Ligus workspace chat. Reply to the latest message concisely and helpfully.\n\n{}",
        ligus_runtime::prompt::runtime_identity_block(&rt, &host_label)
    );
    loop {
        if cfg.relay_url.is_some() {
            if let Err(e) = sync_once(cfg).await {
                eprintln!("sync: {e}");
            }
        }
        let mut svc = open_service(cfg)?;
        for id in svc.store().list_session_ids()? {
            let Some(s) = svc.load_resolving(id)? else { continue };
            if id == workspace_config_session_id(s.workspace_id) {
                continue; // the config log isn't a chat
            }
            // Every new primary/un-mentioned user message, in order — not only
            // the chat's last: two posts in one sync ("hi" then "@coder …")
            // would otherwise drop the first.
            let triggers: Vec<ligus_core::ChatMessage> = s
                .messages
                .iter()
                .filter(|m| m.role == MessageRole::User && !seen.contains(&m.id))
                .filter(|m| {
                    let t = parse_mentions(&m.body, &s);
                    t.primary || t.is_empty()
                })
                .filter(|m| {
                    // Already answered (by another `ligus agent`, or the desktop).
                    !s.messages.iter().any(|r| r.role == MessageRole::Assistant && !r.superseded && r.reply_to_message_id == Some(m.id))
                })
                .cloned()
                .collect();
            seen.extend(s.messages.iter().map(|m| m.id));
            for last in triggers {
                // Agent access controls: this runs on the operator's own
                // credential, like a desktop's primary runtime, so in a shared
                // room it answers only the operator unless they opened it
                // (LIGUS_AGENT_OPEN_TO_MEMBERS=1). Refused: left for others.
                let open = ligus_core::names::var("LIGUS_AGENT_OPEN_TO_MEMBERS").is_ok_and(|v| v == "1");
                if !svc.primary_turn_access(s.workspace_id, s.id, last.id, open).map(|d| d.allowed()).unwrap_or(false) {
                    println!("↳ @{name}: not answering {} (another member's message; set LIGUS_AGENT_OPEN_TO_MEMBERS=1 to answer everyone)", last.id);
                    continue;
                }
                // Non-Send store is held across .await — cmd_agent runs on a
                // current-thread runtime (see run()), so this is allowed.
                let mut turns = turns_for(&worker::through_trigger(&s, last.id), None, &name);
                let head = svc.attachment_rules(s.workspace_id).unwrap_or_default();
                let me = svc.author().id.clone();
                materialize_turns(cfg, &s, &head, &me, &mut turns).await;
                print!("↳ @{name} replying in {} … ", s.id);
                // H1 gate, the same as the worker's: at or above this host's
                // floor (Admin when the runtime can write, Contributor when
                // read-only; LIGUS_MCP_MIN_ROLE overrides) the turn runs as
                // configured. Below it an HTTP model gets no tools, a CLI runs
                // at its read-only tier, and one with none is refused.
                // Unresolved author → fail closed.
                // The author's role when they posted it (config roster at its
                // frontier); an agent's message resolves through its chain to
                // the lowest of the humans behind it (the shared rule).
                let author_role = svc.trigger_author_role(s.id, last.id).ok().flatten();
                // Phase C: the shell is the Owner's (`ligus agent` has no
                // agent id to hold a grant).
                let shell_allowed = ligus_runtime::author_gate::shell_allowed(svc.trigger_shell_role(s.id, last.id).ok().flatten(), false);
                let mut turn_rt = rt.clone();
                let plan = ligus_runtime::author_gate::gate_turn(turn_rt.provider, &mut turn_rt.args, author_role);
                if let Err(reason) = ligus_runtime::author_gate::gate_shell(turn_rt.provider, &mut turn_rt.args, shell_allowed) {
                    println!("refused: {reason}");
                    continue;
                }
                let allow_mcp = plan.allows_tools();
                if let ligus_runtime::author_gate::GatePlan::Refuse(reason) = &plan {
                    println!("refused: {reason}");
                    continue;
                }
                if let Err(reason) = ligus_runtime::executor::turn_allowed(turn_rt.provider) {
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
                let envelope = ligus_core::sandbox::envelope_for_session(&s);
                let enforcement = ligus_runtime::sandbox::enforce(turn_rt.provider, &envelope, &Vec::new());
                if let Some(reason) = enforcement.refuse {
                    println!("refused: {reason}");
                    continue;
                }
                turn_rt.args.extend(enforcement.extra_args);
                let allow_mcp = allow_mcp && enforcement.allow_run_command;
                let mut sandbox = ligus_runtime::executor::TurnPolicy::new(ligus_runtime::executor::HostKind::Agent, envelope, plan.clone());
                sandbox.shell_allowed = shell_allowed;
                // The signed grants, when this host applies them
                // (`LIGUS_SANDBOX_GRANTS`; by default only the Owner's).
                sandbox.grants = svc.sandbox_grants_here(s.workspace_id, ligus_runtime::sandbox::grants_setting_from_env()).0.paths_for(None);
                // Who the turn answers (what the reply's `@you` means).
                let system = match ligus_runtime::prompt::requester_guidance(&s, Some(last.id)) {
                    g if g.is_empty() => system.clone(),
                    g => format!("{system}\n\n{g}"),
                };
                let reply = ligus_runtime::executor::with_turn(sandbox, reply::generate_reply(&turn_rt, &system, &turns, allow_mcp, None)).await?;
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
    use ligus_runtime::workspace_trust as wt;
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
    dry_run: bool,
) -> Result<()> {
    // The desktop refuses these too: a mention word ("ligus", "primary", the
    // legacy "hive", "all", ...) would shadow the default agent or a group.
    if ligus_core::agent::is_reserved_agent_name(name) {
        return Err(crate::help::usage_error(format!("'{name}' is a reserved mention name: pick another agent name")));
    }
    if dry_run {
        // The dry run checks what the real run needs: the runtime exists.
        let svc = open_service(cfg)?;
        let ws = uuid_of_room(&cfg.room);
        if !svc.list_workspace_runtimes(ws)?.iter().any(|r| r.id == runtime_id) {
            return Err(problem::not_found(format!("no runtime '{runtime_id}' in this workspace (ligus runtimes lists them)")));
        }
        let on = host.as_deref().map(|h| format!(" on host {h}")).unwrap_or_default();
        println!("dry run: would create agent @{name} on runtime '{runtime_id}'{on}; nothing was signed or written.");
        return Ok(());
    }
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

fn cmd_agents(cfg: &Config, json: bool) -> Result<()> {
    let svc = open_service(cfg)?;
    let ws = uuid_of_room(&cfg.room);
    let agents = svc.load_in(ws, workspace_config_session_id(ws))?.map(|s| s.workspace_agents).unwrap_or_default();
    if json {
        let rows: Vec<serde_json::Value> = agents
            .iter()
            .map(|a| {
                serde_json::json!({
                    "id": a.id,
                    "name": shown(&a.name),
                    "runtimeId": a.runtime_id,
                    "hostId": (!a.host_id.is_empty()).then_some(&a.host_id),
                    "role": shown(&a.role),
                })
            })
            .collect();
        println!("{}", serde_json::Value::Array(rows));
        return Ok(());
    }
    if agents.is_empty() {
        println!("(no agents — `ligus add-agent <name> <runtime-id>`)");
    }
    for a in agents {
        let host = if a.host_id.is_empty() { "owner-device".into() } else { a.host_id.clone() };
        println!("@{:<20} runtime={:<14} host={}", shown(&a.name), a.runtime_id, host);
    }
    Ok(())
}

// ── Hosts + worker daemon (spec §12.4) ──────────────────────────────────────

fn cmd_hosts(cfg: &Config, json: bool) -> Result<()> {
    let svc = open_service(cfg)?;
    let me = svc.device_id().to_string();
    let hosts = svc.list_hosts(uuid_of_room(&cfg.room))?;
    if json {
        let rows: Vec<serde_json::Value> = hosts
            .iter()
            .map(|h| {
                serde_json::json!({
                    "id": h.id,
                    "kind": h.kind,
                    "label": shown(&h.label),
                    "thisBox": h.id == me,
                    "modelGateways": h.model_gateways,
                })
            })
            .collect();
        println!("{}", serde_json::Value::Array(rows));
        return Ok(());
    }
    if hosts.is_empty() {
        println!("(no hosts — `ligus register-worker` to register this box)");
    }
    for h in hosts {
        let mark = if h.id == me { "  ← this box" } else { "" };
        println!("{:<38} {:?}  {}{mark}", h.id, h.kind, shown(&h.label));
    }
    Ok(())
}

/// `h` with the model gateways this host's operator lists (names only).
pub(crate) fn with_gateways(mut h: WorkspaceHost) -> WorkspaceHost {
    h.model_gateways = ligus_runtime::model_gateway::host().offers();
    h
}

fn cmd_register_worker(cfg: &Config, label: Option<String>) -> Result<String> {
    let mut svc = open_service(cfg)?;
    // Never seed a config log by registering: sync the workspace first.
    if svc.load_resolving(workspace_config_session_id(uuid_of_room(&cfg.room)))?.is_none() {
        bail!("this box has no copy of workspace '{}' yet: run `ligus sync` first", cfg.room);
    }
    let id = svc.device_id().to_string();
    let label = label.or_else(|| env_opt("HOSTNAME")).unwrap_or_else(|| "worker".into());
    svc.ensure_host(uuid_of_room(&cfg.room), crate::with_gateways(WorkspaceHost::new(id.clone(), HostKind::Worker, label)))?;
    Ok(id)
}

fn cmd_set_agent_host(cfg: &Config, agent_name: &str, host_id: &str, dry_run: bool) -> Result<()> {
    let mut svc = open_service(cfg)?;
    let ws = uuid_of_room(&cfg.room);
    let agents = svc
        .load_in(ws, workspace_config_session_id(ws))?
        .map(|s| s.workspace_agents)
        .unwrap_or_default();
    let agent = agents
        .iter()
        .find(|a| a.name == agent_name)
        .ok_or_else(|| problem::not_found(format!("no agent '{agent_name}' in this workspace (ligus agents lists them)")))?;
    let aid = agent.id;
    if dry_run {
        println!("dry run: would bind @{agent_name} ({aid}) to host {host_id}; nothing was signed or written.");
        return Ok(());
    }
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
async fn cmd_worker(cfg: &Config, label: Option<String>, health_listen: Option<String>) -> Result<()> {
    // Probes for an orchestrator (Kubernetes): live while the process serves,
    // ready once the worker has registered and opened its store.
    let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    if let Some(addr) = health_listen {
        worker::spawn_health(&addr, ready.clone()).await?;
    }
    if let Err(e) = purge_foreign_workspaces(cfg) {
        eprintln!("worker: foreign-workspace cleanup skipped: {e}");
    }
    // Same label the host registration publishes, reused for the identity
    // block so an agent can say which worker it runs on.
    let host_label = label
        .clone()
        .or_else(|| env_opt("HOSTNAME"))
        .unwrap_or_else(|| "worker".into());
    let admission = admit::admission_from_file(env_opt(admit::ADMISSION_FILE_ENV))?;
    let mut cfg = cfg.clone();
    let (mut svc, my_host) = worker_start(&mut cfg, &host_label, admission).await?;
    let cfg = &cfg;
    let ws = uuid_of_room(&cfg.room);
    println!("worker {my_host} online — draining queued mentions for its agents. Ctrl-C to stop.");
    // Finish replies a previous run was streaming when it stopped (see
    // `worker::recover_interrupted_turns`).
    match worker::recover_interrupted_turns(&mut svc) {
        Ok(done) if !done.is_empty() => println!("worker: finished {} reply(s) interrupted by the last stop", done.len()),
        Ok(_) => {}
        Err(e) => eprintln!("worker: interrupted-turn recovery failed: {e}"),
    }
    let svc = std::sync::Arc::new(std::sync::Mutex::new(svc));
    // Each turn's own stop signal: a shutdown mid-turn stops it once the
    // grace runs out.
    let stops = ligus_runtime::turn_engine::TurnStops::new();
    let ctx = worker::WorkerCtx {
        cfg,
        ws,
        my_host: &my_host,
        host_label: &host_label,
        settings: reply::ToolSettings::from_env(),
        ligus: Some(worker::ligus_server(svc.clone())),
        presence: None,
        stops: Some(stops.clone()),
    };
    // Registered, store open, interrupted turns recovered: ready for work.
    ready.store(true, std::sync::atomic::Ordering::Relaxed);
    // Mentions whose reply hard-failed (not retried every tick) and the
    // dispatcher's wait clock.
    let mut drain = worker::DrainState::default();
    // Exponential backoff on repeated relay-sync failures, so a relay outage
    // isn't hammered every 3s. Resets to the base cadence on the first success.
    let mut sync_fails: u32 = 0;
    // Liveness beat on the relay's presence channel (not the event log).
    let mut last_beat: Option<std::time::Instant> = None;
    let beat_every = Duration::from_secs(ligus_runtime::host_presence::HOST_BEAT_EVERY_SECS as u64);
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
        worker::apply_tick(&ctx, &svc);
        // A shutdown signal mid-turn: give the turn WORKER_STOP_GRACE to
        // finish, then stop it (it completes its reply and drops its
        // worktree) and exit.
        let tick = worker::drain_tick(&ctx, &svc, &mut drain, &worker::NoSink);
        tokio::pin!(tick);
        let ticked = tokio::select! {
            r = &mut tick => r,
            _ = shutdown_signal() => {
                println!("worker: shutdown signal received — letting the turn in flight finish.");
                if tokio::time::timeout(WORKER_STOP_GRACE, &mut tick).await.is_err() {
                    stops.close();
                    println!("worker: stopping the turn in flight.");
                    let _ = tokio::time::timeout(Duration::from_secs(3), &mut tick).await;
                }
                return Ok(());
            }
        };
        if let Err(e) = ticked {
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

/// A worker's start-up (Phase B): **sync first**, refuse without a copy of
/// the workspace (never seed a competing config log naming this box founder
/// or Owner), join only as this box's own service member through an
/// admission, then register its host, publish its key-agreement key and push.
/// Returns the service and this box's host id.
async fn worker_start(cfg: &mut Config, label: &str, admission: Option<String>) -> Result<(ChatService, String)> {
    let (mut svc, _) = admit::prepare_box(cfg, label, None, admission, |c| async move { sync_once(&c).await }).await?;
    let ws = uuid_of_room(&cfg.room);
    let id = svc.device_id().to_string();
    if svc.device_standing(ws)? == ligus_runtime::device_enroll::Standing::Revoked {
        bail!("this worker's device was revoked by a workspace Admin; start it from a fresh data dir and admit it again");
    }
    svc.ensure_host(ws, with_gateways(WorkspaceHost::new(id.clone(), HostKind::Worker, label.to_string())))
        .context("registering this box as a worker host")?;
    let device = svc.device_id();
    if let Err(e) = svc.set_device_label(ws, device, label) {
        eprintln!("worker: naming this box's device failed (it keeps working): {e}");
    }
    publish_key_agreement(cfg, &mut svc);
    if let Err(e) = sync_once(cfg).await {
        eprintln!("worker: pushing the registration failed (will retry): {e}");
    }
    Ok((svc, id))
}

/// Backoff after `fails` consecutive relay-sync failures: 6,12,24,48,96,120… s.
fn sync_backoff_secs(fails: u32) -> u64 {
    (3u64 << fails.min(5)).min(120)
}

/// Resolves when the process receives SIGTERM (orchestrator stop/redeploy) or
/// SIGINT (Ctrl-C), so the worker loop can exit gracefully.
/// How long `ligus worker` lets a turn in flight finish on SIGTERM / Ctrl-C
/// before it stops it (under a systemd unit's default 90 s stop timeout).
const WORKER_STOP_GRACE: Duration = Duration::from_secs(12);

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
        let Some(s) = svc.load_in(ws, id)? else { continue };
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
/// decides, see [`ligus_runtime::HostPresence::is_online`]).
fn fetch_presence(cfg: &Config) -> ligus_runtime::HostPresence {
    let presence = ligus_runtime::HostPresence::new();
    if let (Ok(relay), Some(_), Some(ring)) = (relay_client(cfg), cfg.key.as_deref(), box_ring(cfg)) {
        if let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
            let fetch = ligus_runtime::host_presence::fetch_host_beats(&relay, &cfg.room, &ring);
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
        if let Err(e) = ligus_runtime::host_presence::publish_host_beat(&relay, &cfg.room, &ring, host_id).await {
            eprintln!("presence: publishing this host's beat failed: {e}");
        }
    }
}

/// Deterministic workspace id for a relay room (mirrors the app's
/// `room_workspace_id`), so config-log filtering matches the app.
fn uuid_of_room(room: &str) -> Uuid {
    ligus_core::room_workspace_id(room)
}

fn truncate(s: &str, n: usize) -> String {
    let t = if s.is_empty() { "(untitled)" } else { s };
    if t.chars().count() <= n {
        t.to_string()
    } else {
        format!("{}…", t.chars().take(n - 1).collect::<String>())
    }
}

/// `ligus doctor --sandbox [--runtime <name>] [--agent <id>]`: what an agent
/// turn may reach on this host (Phase C: the home is default-deny), each path
/// with why, the shell rule, and the signed grants in force for this
/// workspace. The same lines as Settings, Sandbox on the desktop.
fn sandbox_policy_lines(cfg: &Config, runtime: Option<&str>, agent: Option<&str>) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    use ligus_core::ModelProviderKind as K;
    // The signed grants, and whether this host applies them.
    let setting = ligus_runtime::sandbox::grants_setting_from_env();
    let (signed, here) = signed_grants_here(cfg, setting);
    let grants = if here.apply { signed.clone() } else { ligus_core::sandbox::SandboxGrants::default() };
    let agent_id = agent.and_then(|a| Uuid::parse_str(a.trim()).ok());
    let kinds: Vec<Option<K>> = match runtime.map(|r| r.trim().to_ascii_lowercase()) {
        Some(r) => vec![match r.as_str() {
            "claude" | "claude-code" | "claudecode" => Some(K::ClaudeCode),
            "codex" => Some(K::Codex),
            "copilot" => Some(K::Copilot),
            "agy" | "antigravity" => Some(K::Antigravity),
            "aider" => Some(K::Aider),
            "pi" => Some(K::Pi),
            "hermes" => Some(K::Hermes),
            "commands" | "command" | "tool" => None,
            other => anyhow::bail!("unknown runtime {other:?} (claude, codex, copilot, agy, aider, pi, hermes, commands)"),
        }],
        None => vec![Some(K::ClaudeCode), Some(K::Codex), Some(K::Copilot), Some(K::Antigravity), Some(K::Aider), Some(K::Pi), Some(K::Hermes), None],
    };
    let paths = grants.paths_for(agent_id);
    let shell = agent_id.is_some_and(|id| grants.shell_granted(id));
    for k in kinds {
        let p = ligus_runtime::executor::effective_policy(k, &paths, shell);
        out.push(String::new());
        out.push(format!("{} (tier {}):", p.runtime, p.tier));
        for e in &p.entries {
            out.push(format!("  {:<10} {}  ({})", e.access, e.path, e.why));
        }
        out.push(format!("  shell      {}", p.shell));
    }
    out.push(String::new());
        out.push(format!("workspace sandbox grants on this host: {}", here.why));
    out.push(format!("  ({}=apply or {}=ignore sets it)", ligus_runtime::sandbox::GRANTS_ENV, ligus_runtime::sandbox::GRANTS_ENV));
    out.push(String::new());
        out.push(format!("signed grants for this workspace{}:", if here.apply { " (in force here)" } else { " (not applied here)" }));
    let grants = signed;
    if grants.is_empty() {
        out.push("  none (Settings, Sandbox on a desktop: Admin with a passkey; the shell grant is the Owner's)".into());
    }
    for p in &grants.workspace.read_only {
        out.push(format!("  every agent  read        {p}"));
    }
    for p in &grants.workspace.read_write {
        out.push(format!("  every agent  read-write  {p}"));
    }
    for (id, a) in &grants.agents {
        for p in &a.read_only {
            out.push(format!("  agent {id}  read        {p}"));
        }
        for p in &a.read_write {
            out.push(format!("  agent {id}  read-write  {p}"));
        }
        if a.shell {
            out.push(format!("  agent {id}  shell for every member's turns (an Owner's grant)"));
        }
    }
    Ok(out)
}

/// This host's workspace's signed sandbox grants and whether it applies
/// them (`LIGUS_SANDBOX_GRANTS`, else only on the Owner's own hosts).
fn signed_grants_here(cfg: &Config, setting: Option<bool>) -> (ligus_core::sandbox::SandboxGrants, ligus_runtime::sandbox::GrantsHere) {
    if cfg.room.is_empty() {
        return (Default::default(), ligus_runtime::sandbox::grants_here(setting, None, false));
    }
    match open_service(cfg) {
        Ok(svc) => {
            let ws = uuid_of_room(&cfg.room);
            (svc.sandbox_grants(ws), svc.sandbox_grants_here(ws, setting).1)
        }
        Err(_) => (Default::default(), ligus_runtime::sandbox::grants_here(setting, None, false)),
    }
}

/// What `ligus doctor` was asked to do.
#[derive(Default)]
struct DoctorOpts {
    require_confined: bool,
    expect_tier: Option<String>,
    turn_test: bool,
    /// `--sandbox`: the sandbox checks only, plus the per-runtime policy.
    sandbox: bool,
    /// `--gateways`: the model gateway checks only.
    gateways: bool,
    gateway: Option<String>,
    model: Option<String>,
    runtime: Option<String>,
    agent: Option<String>,
    json: bool,
    /// `--legacy-exit-codes`: the preflight failures exit 3, 4 and 5 as
    /// before 0.2 (for one release), instead of 1.
    legacy_exit_codes: bool,
}

impl DoctorOpts {
    /// A preflight flag narrows a bare doctor to the sandbox, as before
    /// (init containers and `helm test` run it that way).
    fn preflight(&self) -> bool {
        self.require_confined || self.expect_tier.is_some() || self.turn_test
    }
    fn sandbox_scope(&self) -> bool {
        self.sandbox || self.preflight() || !self.gateways
    }
    fn everything(&self) -> bool {
        !self.sandbox && !self.gateways && !self.preflight()
    }
    fn gateway_scope(&self) -> bool {
        self.gateways || self.everything()
    }
}

/// The fix for a host without an agent sandbox.
fn unconfined_fix() -> &'static str {
    if cfg!(windows) {
        "Windows has no agent sandbox: run CLI agents on a macOS or Linux host (ligus worker / ligus serve)"
    } else if cfg!(target_os = "linux") {
        "install bubblewrap (bwrap) with unprivileged user namespaces, or run Linux 6.7+ for Landlock (ligus help troubleshooting)"
    } else {
        "ligus help troubleshooting"
    }
}

/// `ligus doctor`: every check this host can make, as a `ligus.checks.v1`
/// report (checks.rs). Returns the exit code.
fn cmd_doctor(cfg: &Config, o: &DoctorOpts) -> Result<i32> {
    use checks::{Check, Report, Status};
    let mut r = Report::new("doctor");
    // A failed check exits 1; --legacy-exit-codes keeps the preflights' old
    // codes (3 no sandbox, 4 another tier, 5 the turn test) for one release.
    let mut code = 0;
    let legacy = |old: i32| if o.legacy_exit_codes { old } else { help::EXIT_ERROR };
    if o.sandbox_scope() {
        let s = ligus_runtime::executor::sandbox_status();
        let unconfined = s.kind == ligus_runtime::executor::ExecutorKind::Unconfined;
        let mut notes = s.notes.clone();
        let c = if unconfined {
            notes.push("CLI agent turns (any tier), run_command, stdio MCP servers and pre-flight tests are refused here; chat with an HTTP model (no local tools) still runs".into());
            let c = if o.require_confined || o.turn_test { Check::fail("sandbox.tier", "agent sandbox", s.tier()) } else { Check::warn("sandbox.tier", "agent sandbox", s.tier()) };
            c.fix(unconfined_fix())
        } else {
            Check::ok("sandbox.tier", "agent sandbox", s.tier())
        };
        let mut c = c.notes(notes);
        c.detail = format!("{}: {}", s.tier(), s.summary);
        r.add(c);
        if (o.require_confined || o.turn_test) && unconfined {
            eprintln!("ligus: no agent sandbox on this host: every agent turn that runs local programs would be refused");
            code = legacy(hardening::EXIT_UNCONFINED);
        }
        if let Some(want) = o.expect_tier.as_deref().map(|t| t.trim().to_ascii_lowercase()) {
            if want == s.tier() {
                r.add(Check::ok("sandbox.expected-tier", "expected tier", format!("{want}, as expected")));
            } else {
                r.add(
                    Check::fail("sandbox.expected-tier", "expected tier", format!("expected {want}, this host uses {}", s.tier()))
                        .fix("run on a node that has that tier, or change the expected tier (agentSandbox.tier on Kubernetes)"),
                );
                eprintln!("ligus: expected agent sandbox tier {want}, this host uses {}", s.tier());
                if code == 0 {
                    code = legacy(4);
                }
            }
        }
        if let Some(d) = hardening::dumpable() {
            r.add(if d {
                Check::warn("process.dumpable", "process", "dumpable: other processes of this user can read its memory").fix("run on Linux with prctl allowed (ligus help security)")
            } else {
                Check::ok("process.dumpable", "process", "not dumpable")
            });
        }
        let (signed, here) = signed_grants_here(cfg, ligus_runtime::sandbox::grants_setting_from_env());
        let n = signed.workspace.read_only.len() + signed.workspace.read_write.len() + signed.agents.len();
        r.add(Check::ok("sandbox.grants", "sandbox grants", format!("{} ({n} signed; ligus doctor --sandbox lists them)", here.why)));
        if o.turn_test && code == 0 {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            match rt.block_on(selftest::turn_test(&cfg.data_dir)) {
                Ok(t) if t.passed() => {
                    let mut lines = t.lines.clone();
                    lines.push(format!("passed (tier {})", t.tier));
                    r.add(Check::ok("sandbox.turn-test", "turn test", format!("passed (tier {})", t.tier)).notes(lines));
                }
                Ok(t) => {
                    eprintln!("ligus: turn test failed");
                    r.add(Check::fail("sandbox.turn-test", "turn test", "failed: the sandbox let something through or the command didn't run").fix("read the findings above; ligus help troubleshooting").notes(t.lines));
                    code = legacy(5);
                }
                Err(e) => {
                    eprintln!("ligus: turn test could not run: {e:#}");
                    r.add(Check::fail("sandbox.turn-test", "turn test", format!("could not run: {e:#}")).fix("ligus help troubleshooting"));
                    code = legacy(5);
                }
            }
        }
        if o.sandbox && code == 0 {
            let lines = sandbox_policy_lines(cfg, o.runtime.as_deref(), o.agent.as_deref())?;
            r.add(Check::ok("sandbox.policy", "sandbox policy", "what an agent turn may read and write here").notes(lines));
        }
    }
    if o.everything() {
        doctor_workspace_checks(cfg, &mut r);
    }
    if o.gateway_scope() {
        let ws = (!cfg.room.is_empty()).then(|| uuid_of_room(&cfg.room));
        let model = o.model.clone().unwrap_or_else(|| "ligus-gateway-check".into());
        let rt = rt_current()?;
        for c in rt.block_on(gateway_check::checks(ws, o.gateway.clone(), model, false)) {
            if c.status == Status::Fail {
                r.next("ligus gateway-check");
            }
            r.add(c);
        }
    }
    // Data a newer Ligus wrote: exit 3, as every other command does.
    if code == 0 && r.checks.iter().any(|c| c.id == "store.schema" && c.status == Status::Fail) {
        code = problem::EXIT_FIXABLE;
    }
    if code == 0 && !r.ok() {
        code = help::EXIT_ERROR;
    }
    if o.json {
        println!("{}", r.to_json(code));
    } else {
        let g = cli_flags::globals();
        print!("{}", doctor_text(&r, g.quiet, cli_flags::stdout_color()));
    }
    Ok(code)
}

/// The text form. The sandbox tier comes first as `agent sandbox: <tier>`
/// with its summary on the next line, and the turn test's findings as
/// `turn test: …` lines, as before: deploy scripts and CI read those lines.
///
/// `quiet` leaves out the checks that are ok or skipped; `color` colours a
/// failure red and a warning yellow.
fn doctor_text(r: &checks::Report, quiet: bool, color: bool) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    for c in &r.checks {
        match c.id.as_str() {
            "sandbox.tier" => {
                let (tier, summary) = c.detail.split_once(": ").unwrap_or((c.detail.as_str(), ""));
                let _ = writeln!(s, "agent sandbox: {tier}\n  {summary}");
                for n in &c.notes {
                    let _ = writeln!(s, "  - {n}");
                }
                if let (Some(f), false) = (&c.fix, c.status == checks::Status::Ok) {
                    let _ = writeln!(s, "  fix: {f}");
                }
            }
            "sandbox.turn-test" => {
                for n in &c.notes {
                    let _ = writeln!(s, "turn test: {n}");
                }
                if c.status != checks::Status::Ok {
                    s.push_str(&checks::Report::check_text_styled(&checks::Check { notes: Vec::new(), ..c.clone() }, color));
                }
            }
            "sandbox.policy" => {
                for n in &c.notes {
                    let _ = writeln!(s, "{n}");
                }
            }
            _ if quiet && matches!(c.status, checks::Status::Ok | checks::Status::Skip) => {}
            _ => s.push_str(&checks::Report::check_text_styled(c, color)),
        }
    }
    let _ = writeln!(s, "{}", r.summary());
    s
}

/// `secrets.keychain-access`: whether this build can use the Ligus keychain
/// without macOS asking (found without a prompt: every read runs with
/// keychain prompts off).
#[cfg(target_os = "macos")]
fn keychain_access_check(access: &ligus_runtime::secret_store::KeychainAccess) -> checks::Check {
    use checks::Check;
    use ligus_runtime::secret_store::KeychainAccess as A;
    const ID: &str = "secrets.keychain-access";
    const TITLE: &str = "keychain access";
    match access {
        A::NoKeychain => Check::skip(ID, TITLE, "no Ligus keychain in this data dir yet"),
        A::Trusted { untrusted } if untrusted.is_empty() => Check::ok(ID, TITLE, "this build reads the Ligus keychain without a prompt"),
        A::Trusted { untrusted } => Check::warn(
            ID,
            TITLE,
            format!("{} item(s) in the Ligus keychain were saved by a different Ligus build: {}", untrusted.len(), untrusted.join(", ")),
        )
        .fix(
            "quit and reopen a signed release of the Ligus app: it repairs their access by itself, without asking (the ligus CLI and \
             development builds don't; use the build that saved them). If macOS asks for the \"ligus\" keychain password, choose \
             Cancel: that password is random and only Ligus knows it",
        ),
        A::UnlockNotTrusted => Check::warn(
            ID,
            TITLE,
            "the keychain's unlock password (ai.ligus.secrets.unlock in the login keychain) exists, but this build isn't trusted to read it, \
             so macOS asks (it was saved by a different Ligus build)",
        )
        .fix(
            "run the build that uses this data dir in this Mac's desktop session (the Ligus app, or ligus with the same data dir in a \
             desktop terminal), enter your login password when macOS asks about ai.ligus.secrets.unlock and choose Always Allow; a \
             signed release of the Ligus app then repairs the item so it never asks again",
        ),
        A::UnlockMissing => Check::fail(ID, TITLE, "the Ligus keychain's unlock password is missing from the login keychain")
            .fix("the keychain in this data dir can't be opened without it (was the data dir copied from another Mac or user?): ligus help security"),
        A::Other(e) => Check::warn(ID, TITLE, format!("couldn't check the Ligus keychain without a prompt ({e})")).fix("ligus help security"),
    }
}

/// `ligus doctor`'s workspace checks: the secret store, the connection, the
/// relay (one request), and the events held aside.
/// The database's schema version (identity model v2, 10.4) and the backups
/// of it next to it, which hold redeem key seeds (10.3): listed, never
/// deleted.
fn store_checks(cfg: &Config) -> Vec<checks::Check> {
    use checks::Check;
    let db = cfg.data_dir.join("ligus.db");
    let mut out = Vec::new();
    if db.exists() {
        out.push(match EventStore::open(&db) {
            Ok(s) if s.newer_schema().is_some() => Check::fail("store.schema", "database", ligus_runtime::event_store::NEWER_DATA_MESSAGE)
                .fix(format!("install the newer Ligus, or point LIGUS_DATA_DIR at another data dir (this one is {})", cfg.data_dir.display())),
            Ok(s) => Check::ok("store.schema", "database", format!("schema version {}", s.schema_version().unwrap_or(0))),
            Err(e) => Check::warn("store.schema", "database", format!("couldn't open it: {e}")),
        });
    }
    let backups = ligus_runtime::event_store::seed_backups(&db);
    out.push(if backups.is_empty() {
        Check::ok("store.backups", "database backups", "none")
    } else {
        let paths: Vec<String> = backups.iter().map(|p| p.display().to_string()).collect();
        Check::warn("store.backups", "database backups", format!("{} backup(s) of the database hold invite and admission seeds: {}", paths.len(), paths.join(", ")))
            .fix("delete once you no longer need the backup")
            .notes(paths)
    });
    out
}

fn doctor_workspace_checks(cfg: &Config, r: &mut checks::Report) {
    use checks::Check;
    let backend = ligus_runtime::secret_store::for_data_dir(&cfg.data_dir).backend();
    let chosen = env_opt("LIGUS_SECRET_BACKEND").is_some();
    r.add(if backend == ligus_runtime::secret_store::Backend::Unavailable {
        Check::fail("secrets.backend", "secret store", "no usable secret store on this machine")
            .fix("unlock or install the OS secret store, or choose plain owner-only files with LIGUS_SECRET_BACKEND=file (ligus help security)")
    } else if backend.is_plain_file() && !chosen {
        Check::warn("secrets.backend", "secret store", "owner-only files under the data dir (the OS secret store wasn't usable)")
            .fix("ligus help security (LIGUS_SECRET_BACKEND)")
    } else {
        Check::ok("secrets.backend", "secret store", backend.as_str())
    });
    #[cfg(target_os = "macos")]
    if matches!(backend, ligus_runtime::secret_store::Backend::MacKeychain | ligus_runtime::secret_store::Backend::Unavailable) {
        // Never in a test binary unless it opted in: the probe reads the
        // login keychain (read-only, prompts off).
        let real = !ligus_runtime::executor::cli_home::in_test_binary() || ligus_runtime::executor::cli_home::test_keychain_opt_in();
        r.add(if real {
            keychain_access_check(&ligus_runtime::secret_store::MacKeychain::access(&cfg.data_dir))
        } else {
            Check::skip("secrets.keychain-access", "keychain access", "not probed in tests")
        });
    }

    for c in store_checks(cfg) {
        r.add(c);
    }

    let st = status_report(cfg);
    let w = &st["workspace"];
    let next = st["next"].as_str().unwrap_or("").to_string();
    let joined = w["relayProblem"].is_null() && w["workspaceKey"].as_bool() == Some(true);
    let c = if !joined {
        Check::warn("workspace.joined", "workspace", w["relayProblem"].as_str().unwrap_or("no workspace key on this device")).fix(next.clone())
    } else if w["deviceStanding"].as_str() == Some("revoked") {
        Check::fail("workspace.joined", "workspace", format!("{}: this device was revoked", cfg.room))
            .fix("an Admin re-admits it from a desktop; or join again from a fresh data dir")
    } else if next != "ready" {
        Check::warn("workspace.joined", "workspace", format!("{}: not synced or not a member yet", cfg.room)).fix(next.clone())
    } else {
        Check::ok("workspace.joined", "workspace", format!("{}: a member, local copy present", cfg.room))
    };
    // Only a plain command is a next step (the fix says the rest).
    if c.status != checks::Status::Ok && next.starts_with("ligus ") && !next.contains([':', ';', '(']) {
        r.next(next);
    }
    r.add(c);

    match cfg.relay_url.as_deref().filter(|u| usable_relay_url(u)) {
        None => r.add(Check::skip("relay.reachable", "relay", "no relay URL on this device")),
        Some(full) => {
            use ligus_runtime::relay_client::RelayProbe as P;
            // Only scheme://host[:port] is ever printed (see relay_origin).
            let url = relay_origin(full);
            let probe = match (rt_current(), relay_client(cfg)) {
                (Ok(rt), Ok(c)) => rt.block_on(c.with_request_timeout(Duration::from_secs(10)).probe(&cfg.room)),
                (Err(e), _) | (_, Err(e)) => P::Unreachable(format!("{e:#}")),
            };
            let relay_check = format!("langstroth-relay check --url {url}");
            let c = match probe {
                P::Ok => Check::ok("relay.reachable", "relay", format!("{url}: reachable, access accepted")),
                P::Unauthorized => Check::fail("relay.reachable", "relay", format!("{url} rejected this device's access token"))
                    .fix("sign in again (ligus auth login) or set a valid LIGUS_RELAY_ACCESS_TOKEN (ligus help env)"),
                P::HttpStatus(c) => {
                    Check::fail("relay.reachable", "relay", format!("{url} answered HTTP {c}")).fix(format!("on the relay host: {relay_check}"))
                }
                P::Unreachable(e) => {
                    Check::fail("relay.reachable", "relay", format!("{url}: {e}")).fix(format!("check the URL and this network; on the relay host: {relay_check}"))
                }
            };
            if c.status != checks::Status::Ok {
                r.next(relay_check);
            }
            r.add(c);
        }
    }

    // Events held aside until this device can judge them (a certificate or
    // admission not here yet, a disputed account key): they never stop sync.
    if let Ok(svc) = open_service(cfg) {
        if let Ok(h) = svc.store().held_stats(uuid_of_room(&cfg.room)) {
            let detail = format!(
                "{} from {} device(s), {} dropped by a cap ({} to fetch again from the relay), {} given up, {} expired (they apply on their own once what they wait for arrives)",
                h.held, h.signers, h.dropped, h.refetch, h.lost, h.expired
            );
            let notes = h.reasons.iter().map(|(reason, n)| format!("- {n}: {reason}"));
            r.add(if h.lost > 0 {
                Check::warn("sync.held", "held events", detail).fix("ligus help troubleshooting (held events)").notes(notes)
            } else {
                Check::ok("sync.held", "held events", detail).notes(notes)
            });
        }
    }
}

/// A current-thread runtime (the CLI only ever `block_on`s).
fn rt_current() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread().enable_all().build()?)
}

/// Whether the command line asks for help (`-h` / `--help` / `help` anywhere
/// before a `--` separator).
fn wants_help(args: &[String]) -> bool {
    args.first().is_some_and(|a| a == "help")
        || args.iter().take_while(|a| *a != "--").any(|a| a == "--help" || a == "-h")
}

fn main() {
    // reqwest 0.13 / rustls (and the tunnel's WebSockets): one process-wide
    // crypto provider (ring).
    ligus_forge::install_tls_provider();
    if let Err(e) = run() {
        let args = split_flag_values(std::env::args().skip(1).collect());
        let words: Vec<&str> = args.iter().take_while(|a| *a != "--").map(String::as_str).collect();
        let command = words.iter().find(|a| !a.starts_with('-')).copied().unwrap_or("");
        let r = problem::render(&e, command, &help::hint(&words));
        // Errors go to stderr; with --json the same error is also the one
        // JSON document on stdout.
        eprint!("{}", r.text);
        if wants_json(&words) {
            println!("{}", r.json);
        }
        std::process::exit(r.exit);
    }
}

/// `--json` or `-o json` on the command line.
fn wants_json(words: &[&str]) -> bool {
    words.iter().any(|a| *a == "--json") || words.windows(2).any(|w| matches!(w, ["-o" | "--output", "json"]))
}

fn run() -> Result<()> {
    // Inside the Linux agent sandbox this process is only the pre-exec helper.
    #[cfg(target_os = "linux")]
    ligus_runtime::executor::linux::maybe_run_helper();
    // `ligus mcp-bridge`: inside an agent's sandbox, pipe its MCP stdio to
    // the turn's Ligus MCP socket (Track D). No config, no data dir.
    ligus_runtime::ligus_mcp::bridge::maybe_run_bridge();
    // `ligus gateway-check`'s child: inside the agent sandbox, no config.
    gateway_check::maybe_run_probe();
    // Before any thread starts: the only time Linux lets us read it.
    serve::dashboard_ligus::init_local_offset();
    let args = cli_flags::extract(split_flag_values(std::env::args().skip(1).collect()))?;
    // Help never opens the data dir, the config or the secret store (a bare
    // `ligus serve --help` once started a real server on the default one).
    if args.first().map(String::as_str) == Some("help") {
        help::print_and_exit(&args[1..].iter().map(String::as_str).collect::<Vec<_>>(), true);
    }
    if wants_help(&args) {
        help::print_and_exit(&args.iter().take_while(|a| *a != "--").map(String::as_str).collect::<Vec<_>>(), false);
    }
    let Some(first) = args.first().map(String::as_str) else {
        eprint!("{}", help::overview());
        std::process::exit(help::EXIT_USAGE);
    };
    if matches!(first, "--version" | "-V" | "version") {
        println!("ligus {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    // An unknown command never creates a data dir.
    if !help::is_command(first) {
        return Err(help::usage_error(format!("unknown command `{first}`: `ligus help` lists the commands and topics")));
    }
    // `--dry-run` where it isn't supported would otherwise be ignored and the
    // command would really run (sign, write, sync): refuse it up front,
    // before the data dir, config or secrets are touched.
    dry_run_supported(&args)?;
    // `ligus app` only opens the desktop app: no data dir, config or secrets.
    if first == "app" {
        return app_cmd::cmd_app(&args[1..]);
    }
    // Also before any thread starts: `*_FILE` secrets and LIGUS_WS_SECRET_DIR
    // become this process's environment (see hardening.rs).
    let secret_paths = hardening::load_secret_files();
    // Long-running hosts hold the workspace key in memory: no other process
    // of this user may read their memory or environ.
    if matches!(args.first().map(String::as_str), Some("serve" | "worker" | "doctor")) {
        hardening::set_not_dumpable();
    }
    let cfg = config();
    // (`ligus doctor` never prompts: the seeds' move reads the store too.)
    if first == "doctor" {
        ligus_runtime::secret_store::without_prompts(|| prepare_store(&cfg, first))?;
    } else {
        prepare_store(&cfg, first)?;
    }
    // Agent processes this host spawns never read its data dir (keys,
    // database, serve token) or its secret files; checked-out repos under
    // the data dir stay readable.
    ligus_runtime::executor::configure_host(&cfg.data_dir, &secret_paths, &[repos::repos_dir(&cfg.data_dir)]);
    // The operator's model gateways (LIGUS_MODEL_GATEWAYS_FILE): their
    // credential files join the deny list. A listed file that can't be used
    // stops the host rather than running without the gateways.
    if let Err(e) = ligus_runtime::model_gateway::init_host(&cfg.data_dir) {
        eprintln!("ligus: {e}");
        std::process::exit(2);
    }
    // Turn scratch dirs a killed turn or host left behind are swept (now and
    // every ten minutes) once older than the longest turn.
    ligus_runtime::executor::start_turn_dir_sweeper(worker::turn_timeout());
    // Ligus's own secret env values are masked wherever agent output is
    // logged (relay tokens, the workspace key, provider keys passed in).
    ligus_runtime::secret_redact::register_env_secrets();
    // `ligus setup tool`'s servers, saved tokens and per-agent reach.
    mcp_config::set_context(&cfg.data_dir, uuid_of_room(&cfg.room));
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let rest = &args[args.len().min(1)..];
    let has = |flag: &str| rest.iter().any(|a| a == flag);
    let flag_val = |flag: &str| {
        rest.iter()
            .position(|a| a == flag)
            .and_then(|i| rest.get(i + 1))
            .cloned()
    };
    let positional = positional_args(rest);

    // A current-thread runtime is all the CLI needs — it only ever `block_on`s
    // (never spawns), and keeps the non-Send EventStore on one thread.
    let rt = || -> Result<tokio::runtime::Runtime> {
        Ok(tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?)
    };

    // A langstroth-auth sign-in (see `ligus auth`) supplies the relay token and
    // keeps it fresh in the background for the life of this process.
    if !matches!(cmd, "auth" | "") {
        auth::init(&cfg.data_dir);
    }

    match cmd {
        "auth" => auth::cmd_auth(&cfg.data_dir, &positional, flag_val("--label")),
        "whoami" => cmd_whoami(&cfg, has("--json")),
        "status" => cmd_status(&cfg, has("--json")),
        "doctor" => {
            let o = DoctorOpts {
                require_confined: has("--require-confined"),
                expect_tier: flag_val("--expect-tier"),
                turn_test: has("--turn-test"),
                sandbox: has("--sandbox"),
                gateways: has("--gateways") || flag_val("--gateway").is_some(),
                gateway: flag_val("--gateway"),
                model: flag_val("--model"),
                runtime: flag_val("--runtime"),
                agent: flag_val("--agent"),
                json: has("--json"),
                legacy_exit_codes: has("--legacy-exit-codes"),
            };
            let code = cmd_doctor(&cfg, &o)?;
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }
        "gateway-check" => {
            let ws = (!cfg.room.is_empty()).then(|| uuid_of_room(&cfg.room));
            let model = flag_val("--model").unwrap_or_else(|| "ligus-gateway-check".into());
            let code = rt()?.block_on(gateway_check::run(ws, flag_val("--gateway"), model, has("--legacy-exit-codes")))?;
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }
        "chats" => cmd_chats(&cfg, has("--json")),
        "new" => cmd_new(&cfg, positional.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ")),
        "tail" => {
            let id = positional
                .first()
                .ok_or_else(|| anyhow!("usage: ligus tail <chat-id> [--watch]"))?;
            cmd_tail(&cfg, chat_id_arg(id)?, has("--watch"))
        }
        "send" => {
            let id = positional
                .first()
                .ok_or_else(|| anyhow!("usage: ligus send <chat-id> <message…> [--push] [--wait [--timeout DURATION]]"))?;
            let chat_id = chat_id_arg(id)?;
            let body = positional[1..]
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            if body.trim().is_empty() {
                bail!("message is empty");
            }
            let wait = match (has("--wait"), flag_val("--timeout")) {
                (false, None) => None,
                (_, Some(t)) => Some(cli_flags::parse_duration(&t)?),
                (true, None) => Some(Duration::from_secs(300)),
            };
            rt()?.block_on(cmd_send(&cfg, chat_id, body, has("--push"), wait))
        }
        "sync" => rt()?.block_on(cmd_sync(&cfg, has("--watch"))),
        "runtimes" => cmd_runtimes(&cfg, has("--json")),
        "add-runtime" => {
            if positional.len() < 4 {
                bail!("usage: ligus add-runtime <id> <name> <provider> <model> [--endpoint <url>] [--secret-ref <name> | --secret - | @FILE]");
            }
            cmd_add_runtime(
                &cfg,
                positional[0],
                positional[1],
                positional[2],
                positional[3],
                flag_val("--endpoint"),
                flag_val("--secret-ref"),
                secret_arg::secret_flag_warn("--secret", flag_val("--secret"), &mut secret_arg::stdin_once())?,
                has("--dry-run"),
            )
        }
        "rm-runtime" => {
            let id = positional.first().ok_or_else(|| anyhow!("usage: ligus rm-runtime <id>"))?;
            cmd_rm_runtime(&cfg, id, has("--dry-run"))
        }
        "enroll" => {
            devices_cmd::refuse_if_linked(&cfg, "accept invites")?;
            let mut stdin = secret_arg::stdin_once();
            let (invite, on_argv) = enroll_invite(positional.first().map(|s| s.as_str()), flag_val("--file"), &mut stdin)?;
            if on_argv {
                eprintln!(
                    "warning: an invite carries the workspace key, and the command line leaks it (ps, shell history): \
                     next time pipe it to `ligus enroll -` or use `ligus enroll --file PATH`"
                );
            }
            let token = secret_arg::secret_flag_warn("--token", flag_val("--token"), &mut stdin)?;
            cmd_enroll(&cfg, &invite, token)
        }
        "join" => {
            devices_cmd::refuse_if_linked(&cfg, "accept invites")?;
            let code = positional
                .first()
                .ok_or_else(|| anyhow!("usage: ligus join <invite-code>"))?;
            rt()?.block_on(cmd_join(&cfg, code))
        }
        "setup" => setup_cmd::cmd(&cfg, rest),
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
        "agents" => cmd_agents(&cfg, has("--json")),
        "proposals" => cmd_proposals(&cfg),
        "add-agent" => {
            if positional.len() < 2 {
                bail!("usage: ligus add-agent <name> <runtime-id> [--host <id>] [--role <role>]");
            }
            cmd_add_agent(&cfg, positional[0], positional[1], flag_val("--host"), flag_val("--role"), has("--dry-run"))
        }
        "hosts" => cmd_hosts(&cfg, has("--json")),
        "register-worker" => {
            let id = cmd_register_worker(&cfg, flag_val("--label"))?;
            println!("registered this box as worker host {id} (sync to share).");
            Ok(())
        }
        "set-agent-host" => {
            if positional.len() < 2 {
                bail!("usage: ligus set-agent-host <agent-name> <host-id>");
            }
            cmd_set_agent_host(&cfg, positional[0], positional[1], has("--dry-run"))
        }
        "worker" => {
            devices_cmd::refuse_if_linked(&cfg, "run as a box")?;
            hardening::require_sandbox_or_exit("this worker");
            let health = flag_val("--health-listen").or_else(|| env_opt("LIGUS_WORKER_HEALTH_LISTEN"));
            rt()?.block_on(cmd_worker(&cfg, flag_val("--label"), health))
        }
        "queue" => cmd_queue(&cfg),
        "serve" if positional.first().map(|s| s.as_str()) == Some("enroll") => {
            bail!("`ligus serve enroll` was removed: {}", admit::OWNER_MODE_REMOVED)
        }
        "admit" => {
            if matches!(positional.first().map(|s| s.as_str()), Some("request" | "redeem")) {
                devices_cmd::refuse_if_linked(&cfg, "become a box")?;
            }
            admit::cmd_admit(&cfg, &positional, |f| flag_val(f))
        }
        "devices" => {
            let json = has("--json");
            let interactive = link_io::interactive();
            let mut ask = devices_cmd::confirm_on_tty;
            match positional.first().map(|s| s.as_str()) {
                Some("list") | None => devices_cmd::cmd_list(&cfg, json),
                Some("request") => {
                    let mut copier = link_io::copy;
                    let copy = has("--copy").then_some(&mut copier as devices_cmd::Copier<'_>);
                    devices_cmd::cmd_request(&cfg, flag_val("--label"), has("--new-device"), json, copy)
                }
                Some("approve") => rt()?.block_on(devices_cmd::cmd_approve(
                    &cfg,
                    positional.get(1).map(|s| s.as_str()),
                    flag_val("--code-file"),
                    flag_val("--fingerprint"),
                    flag_val("--recovery-words-file"),
                    flag_val("--out"),
                    json,
                    interactive.then_some(&mut ask as devices_cmd::Confirm<'_>),
                )),
                Some("redeem") => {
                    let mut cfg = cfg.clone();
                    let mut paste = devices_cmd::paste_at_tty;
                    rt()?.block_on(devices_cmd::cmd_redeem(
                        &mut cfg,
                        positional.get(1).map(|s| s.as_str()),
                        flag_val("--code-file"),
                        flag_val("--approver-fingerprint"),
                        json,
                        // No code given, at a terminal: a hidden paste or the clipboard.
                        (interactive && std::io::IsTerminal::is_terminal(&std::io::stdin()))
                            .then_some(&mut paste as devices_cmd::Paste<'_>),
                        interactive.then_some(&mut ask as devices_cmd::Confirm<'_>),
                    ))
                }
                Some("rename") => rt()?.block_on(devices_cmd::cmd_rename(
                    &cfg,
                    positional.get(1).map(|s| s.as_str()),
                    positional.get(2).map(|s| s.as_str()),
                    json,
                )),
                Some(other) => bail!("unknown `ligus devices {other}`: use list|request|approve|redeem|rename"),
            }
        }
        "recovery-key" => admit::cmd_recovery_key(&cfg, &positional, flag_val("--out")),
        "profile" => match positional.first().map(|s| s.as_str()) {
            Some("set") => rt()?.block_on(devices_cmd::cmd_profile_set(
                &cfg,
                flag_val("--name").as_deref(),
                flag_val("--avatar").as_deref(),
                has("--clear-avatar"),
                has("--json"),
            )),
            _ => bail!("use `ligus profile set [--name NAME] [--avatar FILE | --clear-avatar]`"),
        },
        "serve" => {
            devices_cmd::refuse_if_linked(&cfg, "run as a box")?;
            hardening::require_sandbox_or_exit("this serve host");
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
                secret_arg::secret_flag_warn("--reg-token", flag_val("--reg-token"), &mut secret_arg::stdin_once())?,
                has("--insecure-relay"),
            )?;
            serve::cmd_serve(cfg, opts)
        }
        "pair" => {
            devices_cmd::refuse_if_linked(&cfg, "run as a box")?;
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
                secret_arg::secret_flag_warn("--reg-token", flag_val("--reg-token"), &mut secret_arg::stdin_once())?,
                has("--insecure-relay"),
            )?;
            serve::cmd_pair(&cfg, &opts, has("--relay-only"))
        }
        "remote" => {
            use serve::tunnel::cli;
            match positional.first().map(|s| s.as_str()) {
                Some("enable") => cli::remote_enable(
                    &cfg.data_dir,
                    flag_val("--relay"),
                    secret_arg::secret_flag_warn("--reg-token", flag_val("--reg-token"), &mut secret_arg::stdin_once())?,
                    has("--insecure-relay"),
                ),
                Some("disable") => cli::remote_disable(&cfg.data_dir),
                Some("status") | None => cli::remote_status(&cfg.data_dir, has("--json")),
                Some("reset") => cli::remote_reset(&cfg.data_dir),
                Some("push-test") => cli::remote_push_test(&cfg.data_dir, flag_val("--phone").as_deref()),
                Some(other) => bail!("unknown `ligus remote {other}`: use enable|status|push-test|disable|reset"),
            }
        }
        "memory" => {
            let flags = memory_cmd::Flags {
                tags: ligus_core::memory::normalize_tags(&flag_val("--tag").map(|t| t.split(',').map(str::to_string).collect::<Vec<_>>()).unwrap_or_default()),
                title: flag_val("--title"),
                body: flag_val("--body"),
                note: flag_val("--note"),
                base: flag_val("--base"),
                json: has("--json"),
            };
            memory_cmd::run(&cfg, &positional, flags)
        }
        "task-state" => memory_cmd::task_state(&cfg, positional.first().map(|s| s.as_str()), has("--json")),
        "facts" => {
            if positional.first().map(|s| s.as_str()) != Some("export") || !has("--jsonl") {
                bail!("usage: ligus facts export --jsonl [--since <cursor | unix ms>]");
            }
            let since = match flag_val("--since") {
                None => ligus_runtime::facts::Since::UnixMs(0),
                Some(v) => ligus_runtime::facts::Since::parse(&v)
                    .ok_or_else(|| anyhow!("--since takes the cursor from a previous export's last line, or unix milliseconds"))?,
            };
            for line in cmd_facts_export(&cfg, since, ligus_core::flow::now_ms())? {
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
                Some("list") | None => cli::phones_list(&cfg.data_dir, has("--json")),
                Some("revoke") => {
                    let token_file = flag_val("--token-file")
                        .or_else(|| env_opt("LIGUS_SERVE_TOKEN_FILE"))
                        .map(PathBuf::from)
                        .unwrap_or_else(|| cfg.data_dir.join("serve.token"));
                    cli::phones_revoke(&cfg.data_dir, positional.get(1).map(|s| s.as_str()), &token_file, has("--keep-token"))
                }
                Some("certify") => {
                    // Without --fingerprint, only a person at a terminal can
                    // confirm it; non-interactive use must pass it.
                    let mut ask = admit::confirm_phone_fingerprint_on_tty;
                    let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin());
                    admit::cmd_phones_certify(
                        &cfg,
                        positional.get(1).map(|s| s.as_str()),
                        flag_val("--code-file"),
                        flag_val("--recovery-words-file"),
                        flag_val("--fingerprint"),
                        interactive.then_some(&mut ask as admit::ConfirmFingerprint<'_>),
                    )
                    .map(|_| ())
                }
                Some(other) => bail!("unknown `ligus phones {other}`: use list|revoke|certify"),
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
        "repos" => {
            let mut svc = open_service(&cfg)?;
            let ws = uuid_of_room(&cfg.room);
            let label = flag_val("--label").or_else(|| env_opt("LIGUS_HOST_LABEL")).or_else(|| env_opt("HOSTNAME")).unwrap_or_else(|| "this host".into());
            let pos: Vec<&str> = positional.iter().map(|s| s.as_str()).collect();
            repos_cmd::cmd(&cfg.data_dir, &mut svc, ws, &label, &pos, &|f: &str| flag_val(f).or_else(|| has(f).then(String::new)))
        }
        "forge" => {
            let pos: Vec<&str> = positional.iter().map(|s| s.as_str()).collect();
            forge_cmd::cmd(&cfg.data_dir, &pos, flag_val("--auth"), flag_val("--client-id"), has("--internal"))
        }
        "agent" => {
            let name = positional.first().map(|s| s.as_str()).unwrap_or("assistant").to_string();
            let runtime_id = flag_val("--runtime").or_else(|| env_opt("LIGUS_RUNTIME"));
            rt()?.block_on(cmd_agent(&cfg, name, runtime_id))
        }
        other => Err(help::usage_error(format!("unknown command `{other}`: `ligus help` lists the commands and topics"))),
    }
}

/// The invite `ligus enroll` uses: piped (`-`), from `--file PATH`, or (with a
/// warning, since it carries the workspace key) on the command line. The bool
/// is whether it came from the command line.
/// A chat id from the command line: a malformed one is a usage error.
fn chat_id_arg(s: &str) -> Result<Uuid> {
    Uuid::parse_str(s.trim()).map_err(|_| help::usage_error(format!("not a chat id: {s:?} (ligus chats lists the ids)")))
}

fn enroll_invite(arg: Option<&str>, file: Option<String>, stdin: &mut dyn FnMut() -> std::io::Result<String>) -> Result<(String, bool)> {
    match (arg, file) {
        (Some("-"), None) => Ok((stdin()?.trim().to_string(), false)),
        (Some(c), None) => Ok((c.trim().to_string(), true)),
        (None, Some(path)) => Ok((admit::read_code_file(std::path::Path::new(&path))?, false)),
        _ => bail!("usage: ligus enroll <hivews1:invite> | - | --file <path> [--token - | @FILE]"),
    }
}

/// Flags that take a value: the value right after one is not a positional
/// argument (so `agent bot --runtime ws1` gives positional `[bot]`). Every
/// flag read with `flag_val` must be listed here, or its value is taken as a
/// positional argument (a test checks this against the source).
const VALUE_FLAGS: &[&str] = &[
    "--approver-fingerprint",
    "--avatar",
    "--timeout",
    "--gateway",
    "--model",
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
    "--expect-tier",
    "--health-listen",
    "--tag",
    "--title",
    "--body",
    "--note",
    "--base",
    "--kind",
    "--branch",
    "--agent",
    "--code-file",
    "--fingerprint",
    "--out",
    "--phone",
    "--recovery-words-file",
    "--token",
    "--ttl",
    "--request",
    "--harness",
    "--path",
    "--id",
    "--command",
    "--url",
    "--reach",
    "--code",
];

/// The positional arguments: everything that is not a flag or a flag's value.
/// `--flag=value` becomes `--flag value`, so every command takes both
/// spellings. Arguments after a `--` separator are left as they are.
fn split_flag_values(args: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut it = args.into_iter();
    for a in it.by_ref() {
        if a == "--" {
            out.push(a);
            break;
        }
        match a.split_once('=') {
            Some((flag, value)) if flag.len() > 2 && flag.starts_with("--") => {
                out.push(flag.to_string());
                out.push(value.to_string());
            }
            _ => out.push(a),
        }
    }
    out.extend(it);
    out
}

/// The commands that take `--dry-run`.
const DRY_RUN_COMMANDS: &[&str] = &["add-runtime", "rm-runtime", "add-agent", "set-agent-host", "setup"];

/// A usage error when `--dry-run` (before any `--`) is given to a command
/// that has no dry run.
fn dry_run_supported(args: &[String]) -> Result<()> {
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let asked = args.iter().skip(1).take_while(|a| *a != "--").any(|a| a == "--dry-run");
    if asked && !DRY_RUN_COMMANDS.contains(&cmd) {
        return Err(help::usage_error(format!(
            "`ligus {cmd}` has no --dry-run (only {} do); nothing was run",
            DRY_RUN_COMMANDS.join(", ")
        )));
    }
    Ok(())
}

fn positional_args(rest: &[String]) -> Vec<&String> {
    let mut positional: Vec<&String> = Vec::new();
    let mut skip = false;
    for a in rest {
        if skip {
            skip = false;
            continue;
        }
        if a.starts_with("--") {
            if VALUE_FLAGS.contains(&a.as_str()) {
                skip = true;
            }
            continue;
        }
        positional.push(a);
    }
    positional
}

#[cfg(test)]
mod cli_tests {
    /// `--dry-run` on a command without one is a usage error (exit 2),
    /// never silently ignored; the four that have one, and text after
    /// `--`, pass.
    #[test]
    fn dry_run_only_where_supported() {
        for cmd in ["new", "send", "sync", "enroll", "serve", "worker", "admit", "doctor"] {
            let e = super::dry_run_supported(&argv(&[cmd, "x", "--dry-run"])).unwrap_err();
            assert!(crate::help::is_usage(&e), "{cmd}: {e:#}");
        }
        for cmd in super::DRY_RUN_COMMANDS {
            assert!(super::dry_run_supported(&argv(&[cmd, "a", "b", "--dry-run"])).is_ok(), "{cmd}");
        }
        assert!(super::dry_run_supported(&argv(&["send", "id", "--", "--dry-run"])).is_ok());
        assert!(super::dry_run_supported(&argv(&["new", "a chat"])).is_ok());
    }

    /// `--flag=value` and `--flag value` read the same; `=` inside a value,
    /// an empty value, positionals and anything after `--` are kept.
    #[test]
    fn flags_take_an_equals_sign_or_the_next_argument() {
        let split = |a: &[&str]| super::split_flag_values(argv(a));
        assert_eq!(split(&["worker", "--label=box", "--json"]), argv(&["worker", "--label", "box", "--json"]));
        assert_eq!(split(&["worker", "--label", "box"]), argv(&["worker", "--label", "box"]));
        assert_eq!(split(&["memory", "search", "--since=a=b"]), argv(&["memory", "search", "--since", "a=b"]));
        assert_eq!(split(&["serve", "--name="]), argv(&["serve", "--name", ""]));
        assert_eq!(split(&["send", "k=v", "-x=1"]), argv(&["send", "k=v", "-x=1"]));
        assert_eq!(split(&["mcp", "--", "--label=x"]), argv(&["mcp", "--", "--label=x"]));
        assert_eq!(split(&["mcp", "--=x"]), argv(&["mcp", "--=x"]));
        // Both spellings give the same positionals.
        let a = split(&["add-agent", "--host=sff", "coder", "claude-code"]);
        let b = argv(&["add-agent", "--host", "sff", "coder", "claude-code"]);
        assert_eq!(super::positional_args(&a[1..]), super::positional_args(&b[1..]));
    }

    /// The invite (which carries the workspace key) can be piped or read
    /// from a file instead of sitting in argv.
    #[test]
    fn enroll_takes_the_invite_from_stdin_or_a_file() {
        let no_stdin = &mut || -> std::io::Result<String> { panic!("stdin not read") };
        assert_eq!(super::enroll_invite(Some("-"), None, &mut || Ok("hivews1:x\n".into())).unwrap(), ("hivews1:x".to_string(), false));
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("invite");
        std::fs::write(&f, "hivews1:y\n").unwrap();
        assert_eq!(super::enroll_invite(None, Some(f.to_string_lossy().into_owned()), no_stdin).unwrap(), ("hivews1:y".to_string(), false));
        assert_eq!(super::enroll_invite(Some("hivews1:z"), None, no_stdin).unwrap(), ("hivews1:z".to_string(), true));
        assert!(super::enroll_invite(None, None, no_stdin).is_err());
    }

    /// A data dir the CLI creates (or finds group/world-open and owns) is
    /// owner-only from the start, not only once a later write tightens it:
    /// `ligus.db` holds the decrypted history.
    #[cfg(unix)]
    #[test]
    fn the_data_dir_is_owner_only_from_the_start() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("data");
        std::fs::create_dir(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        super::create_data_dir_or_exit(&d);
        assert_eq!(std::fs::metadata(&d).unwrap().permissions().mode() & 0o077, 0);
        let fresh = tmp.path().join("fresh").join("data");
        super::create_data_dir_or_exit(&fresh);
        assert_eq!(std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o077, 0);
    }

    /// `recovery-key create --out` never replaces an existing file: re-running
    /// it at the old words' path overwrote them, then deleted the file when
    /// the registration failed, so the registered key had no words anywhere.
    #[test]
    fn recovery_key_out_never_replaces_a_file() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = bare_cfg(tmp.path(), None);
        let out = tmp.path().join("words.txt");
        std::fs::write(&out, "old words").unwrap();
        let create = "create".to_string();
        let err = crate::admit::cmd_recovery_key(&cfg, &[&create], Some(out.to_string_lossy().into_owned()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "old words");
        assert!(!cfg.data_dir.exists(), "nothing else was set up");
    }

    /// Every flag read with `flag_val` is a value flag, so its value is never
    /// taken as a positional argument (`phones certify --code-file c …` once
    /// read `c` as the code and failed with a usage error).
    #[test]
    fn every_flag_val_flag_takes_a_value() {
        let src = include_str!("main.rs");
        let mut missing = Vec::new();
        for part in src.split("flag_val(\"").skip(1) {
            let flag = &part[..part.find('"').unwrap()];
            if flag.starts_with("--") && !super::VALUE_FLAGS.contains(&flag) {
                missing.push(flag.to_string());
            }
        }
        missing.sort();
        missing.dedup();
        assert!(missing.is_empty(), "value flags missing from VALUE_FLAGS: {missing:?}");
    }

    #[test]
    fn phones_certify_flags_are_not_positional() {
        let rest: Vec<String> = [
            "certify", "--code-file", "c.txt", "--recovery-words-file", "w.txt", "--fingerprint", "F",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let pos: Vec<&str> = super::positional_args(&rest).iter().map(|s| s.as_str()).collect();
        assert_eq!(pos, ["certify"]);
    }

    use super::*;

    /// `ligus agent` honors a workspace runtime's model gateway like serve and
    /// workers do: the turn goes through the gateway (or is refused where it
    /// isn't offered), and the runtime's own key is never sent along.
    #[test]
    fn ligus_agent_keeps_the_runtimes_gateway() {
        let mut wr = ligus_core::WorkspaceRuntime::new("gw", "Gateway", ligus_core::ModelProviderKind::Custom, "m");
        wr.endpoint = "https://api.example.com/v1/chat/completions".into();
        wr.credential = ligus_core::WorkspaceCredential::Inline { secret: "sk-RUNTIME-OWN".into() };
        wr.gateway = "litellm".into();
        let rt = workspace_runtime_resolved(&wr);
        assert_eq!(rt.gateway.as_ref().map(|g| g.name.as_str()), Some("litellm"));
        assert!(rt.api_key.is_none(), "the runtime's key rides along to a gateway turn");
        wr.gateway.clear();
        assert!(workspace_runtime_resolved(&wr).gateway.is_none());
    }

    /// A box enrolled with `ligus enroll` is named after its label or host.
    #[test]
    fn a_new_box_is_named_after_its_label_or_host() {
        assert_eq!(box_name(Some("mac-test".into()), Some("zaz-mbp".into())), "mac-test");
        assert_eq!(box_name(Some("  ".into()), Some("zaz-mbp".into())), "zaz-mbp");
        assert_eq!(box_name(None, None), "Ligus box");
        let tmp = tempfile::tempdir().unwrap();
        let identity = IdentityStore::new(tmp.path(), StoreKeyVault::for_data_dir(tmp.path()));
        let name = bootstrap_identity(&identity).unwrap().account.display_name;
        assert!(!name.trim().is_empty());
    }

    /// A box created under its host name (by `ligus enroll`) takes its label
    /// when it serves; a name someone chose is kept.
    #[test]
    fn an_unchosen_box_name_is_replaced_once() {
        let tmp = tempfile::tempdir().unwrap();
        let identity = IdentityStore::new(tmp.path(), StoreKeyVault::for_data_dir(tmp.path()));
        identity.bootstrap("zaz-mbp", "zaz-mbp", "zaz-mbp").unwrap();
        assert_eq!(adopt_box_name(&identity, "mac-test", Some("zaz-mbp")).unwrap().as_deref(), Some("mac-test"));
        assert_eq!(bootstrap_identity(&identity).unwrap().account.display_name, "mac-test");
        assert_eq!(adopt_box_name(&identity, "other", Some("zaz-mbp")).unwrap(), None, "a chosen name stays");
        assert_eq!(adopt_box_name(&identity, "other", None).unwrap(), None);
    }

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

    fn bare_cfg(dir: &std::path::Path, relay_url: Option<&str>) -> Config {
        Config {
            data_dir: dir.join("data"),
            relay_url: relay_url.map(str::to_string),
            token: None,
            room: "r".into(),
            key: None,
            epoch_keys: Vec::new(),
        }
    }

    /// An invite from a workspace with no relay can't be used by a box:
    /// enroll refuses it, says where to set one, and writes nothing.
    #[test]
    fn enroll_refuses_an_invite_without_a_usable_relay() {
        use base64::Engine;
        for relay in ["", "  ", "relay.example", "ftp://relay.example"] {
            let tmp = tempfile::tempdir().unwrap();
            let cfg = bare_cfg(tmp.path(), None);
            let json = serde_json::json!({"relay_url": relay, "room": "r", "key": "k"}).to_string();
            let body = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.as_bytes());
            let err = cmd_enroll(&cfg, &format!("hivews1:{body}"), None).unwrap_err().to_string();
            assert!(err.contains("Settings → Team sync"), "{relay:?}: {err}");
            assert!(!cfg.data_dir.exists(), "{relay:?}: enroll wrote {:?}", cfg.data_dir);
        }
        // An invite with no relay field at all gets the same answer, not a
        // serde "missing field" error.
        let tmp = tempfile::tempdir().unwrap();
        let cfg = bare_cfg(tmp.path(), None);
        let body = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"room":"r","key":"k"}"#);
        let err = cmd_enroll(&cfg, &format!("hivews1:{body}"), None).unwrap_err().to_string();
        assert!(err.contains("Settings → Team sync"), "{err}");
        assert!(!cfg.data_dir.exists());
    }

    /// A box enrolled from a relayless invite (older clients wrote
    /// relay_url = "") is told so by name, never "builder error".
    #[tokio::test]
    async fn sync_names_a_missing_relay_url() {
        for relay in [Some(""), Some("relay.example"), None] {
            let tmp = tempfile::tempdir().unwrap();
            let cfg = bare_cfg(tmp.path(), relay);
            let err = sync_once(&cfg).await.unwrap_err().to_string();
            assert!(err.contains("no relay URL"), "{relay:?}: {err}");
            assert!(!err.contains("builder error"), "{relay:?}: {err}");
        }
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
    fn app_command_parses_and_is_listed() {
        assert!(help::overview().contains("  app "));
        assert!(wants_help(&argv(&["app", "--help"])));
        assert!(!wants_help(&argv(&["app"])));
        // A stray argument is refused before anything is looked up or run.
        assert!(app_cmd::cmd_app(&argv(&["somewhere"])).is_err());
    }

    /// `ligus status` on a fresh data dir: reads only, never creates an
    /// identity, and names the next step.
    #[test]
    fn status_reads_without_creating_an_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = bare_cfg(tmp.path(), None);
        std::fs::create_dir_all(&cfg.data_dir).unwrap();
        let r = status_report(&cfg);
        assert!(r["identity"].is_null(), "{r}");
        assert_eq!(r["workspace"]["room"], "r");
        assert_eq!(r["workspace"]["localCopy"], false);
        assert!(r["workspace"]["relayProblem"].as_str().unwrap().contains("no relay URL"));
        assert!(IdentityStore::new(&cfg.data_dir, StoreKeyVault::for_data_dir(&cfg.data_dir)).load().unwrap().is_none());
        assert!(!cfg.data_dir.join("ligus.db").exists());
        // No identity yet: both ways in, the box's first.
        assert!(r["next"].as_str().unwrap().starts_with("a serve or worker box: ligus admit request"), "{r}");
        // A box is never told to enroll.
        admit::mark_box(&cfg).unwrap();
        let r = status_report(&cfg);
        assert!(r["next"].as_str().unwrap().starts_with("admit this box"), "{r}");
        assert!(!r["next"].as_str().unwrap().contains("enroll"), "{r}");

    }

    #[test]
    fn custom_provider_is_accepted() {
        assert_eq!(provider_kind("custom").unwrap(), ModelProviderKind::Custom);
        assert_eq!(provider_kind("Custom").unwrap(), ModelProviderKind::Custom);
        assert_eq!(provider_kind("agy").unwrap(), ModelProviderKind::Antigravity);
        assert_eq!(provider_kind("copilot").unwrap(), ModelProviderKind::Copilot);
        assert!(provider_kind("nope").is_err());
    }

    /// Scripts read `agent sandbox: <tier>` and the summary on the next line
    /// (deploy/local/ligus-local preflight), and `turn test: …` lines
    /// (the Kubernetes probe, CI): the checks' text form keeps them.
    #[test]
    fn doctor_text_keeps_the_lines_scripts_read() {
        use checks::{Check, Report};
        let mut r = Report::new("doctor");
        let mut c = Check::ok("sandbox.tier", "agent sandbox", "x").notes(["a caveat".to_string()]);
        c.detail = "landlock: Landlock (ABI 6): confined".into();
        r.add(c);
        r.add(Check::ok("sandbox.turn-test", "turn test", "passed (tier landlock)").notes([
            "agent sandbox: landlock".to_string(),
            "DENIED_ENVIRON_1".to_string(),
            "passed (tier landlock)".to_string(),
        ]));
        r.add(Check::warn("workspace.joined", "workspace", "not joined").fix("ligus help quickstart"));
        let t = doctor_text(&r, false, false);
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines[0], "agent sandbox: landlock");
        assert_eq!(lines[1], "  Landlock (ABI 6): confined");
        assert_eq!(lines[2], "  - a caveat");
        assert!(lines.contains(&"turn test: passed (tier landlock)"), "{t}");
        assert!(lines.contains(&"turn test: DENIED_ENVIRON_1"), "{t}");
        assert!(lines.contains(&"warn  workspace       not joined"), "{t}");
        assert!(lines.contains(&"      fix: ligus help quickstart"), "{t}");
        assert_eq!(*lines.last().unwrap(), "1 warning · next: nothing to do");
        // Quiet: the header and the turn test stay, ok checks go.
        r.add(Check::ok("secrets.backend", "secret store", "file"));
        let q = doctor_text(&r, true, false);
        assert!(q.starts_with("agent sandbox: landlock\n") && q.contains("turn test: passed") && q.contains("warn  workspace"), "{q}");
        assert!(!q.contains("secret store"), "{q}");
        assert!(doctor_text(&r, false, true).contains("\x1b[33mwarn"), "colour on request");
    }

    /// `secrets.keychain-access` says what was found and what to do,
    /// without ever saying to delete the keychain.
    #[cfg(target_os = "macos")]
    #[test]
    fn keychain_access_check_explains_each_case() {
        use ligus_runtime::secret_store::KeychainAccess as A;
        let s = |a: A| keychain_access_check(&a);
        assert_eq!(s(A::NoKeychain).status, checks::Status::Skip);
        assert_eq!(s(A::Trusted { untrusted: vec![] }).status, checks::Status::Ok);
        let c = s(A::UnlockNotTrusted);
        assert_eq!(c.status, checks::Status::Warn);
        assert!(c.id == "secrets.keychain-access" && c.detail.contains("isn't trusted"), "{}", c.detail);
        assert!(c.fix.as_deref().is_some_and(|f| f.contains("Always Allow") && f.contains("ai.ligus.secrets.unlock")));
        let c = s(A::Trusted { untrusted: vec!["seed:device".into()] });
        assert!(c.status == checks::Status::Warn && c.detail.contains("seed:device"), "{}", c.detail);
        assert!(c.fix.as_deref().is_some_and(|f| f.contains("choose Cancel")));
        assert_eq!(s(A::UnlockMissing).status, checks::Status::Fail);
        for a in [A::UnlockNotTrusted, A::UnlockMissing, A::Trusted { untrusted: vec!["x".into()] }, A::Other("e".into())] {
            let c = s(a);
            assert!(!format!("{} {:?}", c.detail, c.fix).to_ascii_lowercase().contains("delete"), "{}", c.detail);
        }
    }

    /// A bare doctor runs everything; --sandbox, --gateways and the
    /// preflight flags narrow it.
    #[test]
    fn doctor_scopes() {
        let all = DoctorOpts::default();
        assert!(all.everything() && all.sandbox_scope() && all.gateway_scope());
        let sb = DoctorOpts { sandbox: true, ..Default::default() };
        assert!(!sb.everything() && sb.sandbox_scope() && !sb.gateway_scope());
        let gw = DoctorOpts { gateways: true, ..Default::default() };
        assert!(!gw.everything() && !gw.sandbox_scope() && gw.gateway_scope());
        let pre = DoctorOpts { require_confined: true, ..Default::default() };
        assert!(!pre.everything() && pre.sandbox_scope() && !pre.gateway_scope(), "an init container's doctor checks only the sandbox");
        let both = DoctorOpts { sandbox: true, gateways: true, ..Default::default() };
        assert!(!both.everything() && both.sandbox_scope() && both.gateway_scope());
    }

    /// `ligus doctor --json` on a fresh device: one ligus.checks.v1 document,
    /// a warning for the missing workspace, no relay request.
    #[test]
    fn doctor_report_on_a_fresh_device() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = bare_cfg(tmp.path(), None);
        std::fs::create_dir_all(&cfg.data_dir).unwrap();
        let mut r = checks::Report::new("doctor");
        doctor_workspace_checks(&cfg, &mut r);
        let ids: Vec<&str> = r.checks.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"workspace.joined") && ids.contains(&"relay.reachable") && ids.contains(&"secrets.backend"), "{ids:?}");
        let relay = r.checks.iter().find(|c| c.id == "relay.reachable").unwrap();
        assert_eq!(relay.status, checks::Status::Skip);
        let ws = r.checks.iter().find(|c| c.id == "workspace.joined").unwrap();
        assert_eq!(ws.status, checks::Status::Warn);
        assert!(ws.fix.is_some());
        assert!(r.ok());
        assert!(r.next.iter().all(|n| n.starts_with("ligus ")), "{:?}", r.next);
        let d = r.to_json(0);
        assert_eq!(d["schema"], "ligus.checks.v1");
    }

    /// add-agent --dry-run checks the runtime exists, as its help says.
    #[test]
    fn add_agent_dry_run_needs_a_known_runtime() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = bare_cfg(tmp.path(), None);
        let e = cmd_add_agent(&cfg, "coder", "no-such-runtime", None, None, true).unwrap_err();
        assert!(format!("{e:#}").contains("no runtime 'no-such-runtime'"), "{e:#}");
        let svc = open_service(&cfg).unwrap();
        let ws = uuid_of_room(&cfg.room);
        let agents = svc.load_in(ws, workspace_config_session_id(ws)).unwrap().map(|s| s.workspace_agents).unwrap_or_default();
        assert!(agents.is_empty(), "a dry run created an agent");
    }

    /// A mention word is no agent name: `ligus`, `primary` and the legacy
    /// `hive` would shadow the default agent (a usage error, nothing signed).
    #[test]
    fn add_agent_refuses_the_default_agents_names() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = bare_cfg(tmp.path(), None);
        for name in ["ligus", "Hive", " primary ", "all"] {
            let e = cmd_add_agent(&cfg, name, "rt", None, None, false).unwrap_err();
            assert!(crate::help::is_usage(&e), "{name}: {e:#}");
        }
        let svc = open_service(&cfg).unwrap();
        let ws = uuid_of_room(&cfg.room);
        let agents = svc.load_in(ws, workspace_config_session_id(ws)).unwrap().map(|s| s.workspace_agents).unwrap_or_default();
        assert!(agents.is_empty());
    }

    /// Only scheme://host[:port] of a relay URL is ever printed.
    #[test]
    fn relay_origin_drops_credentials_path_and_query() {
        assert_eq!(relay_origin("https://u:p@relay.example/base?token=hrt1x#f"), "https://relay.example");
        assert_eq!(relay_origin(" http://127.0.0.1:8080/x "), "http://127.0.0.1:8080");
        assert_eq!(relay_origin("https://[::1]:9/a"), "https://[::1]:9");
        assert_eq!(relay_origin("relay.example/x?t=1"), "relay.example");
        assert_eq!(relay_origin("weird://a:b@h/x"), "weird://h");
        assert!(!relay_origin("http://u:p@:bad/?t=1").contains("p@"));
    }

    /// doctor against a relay that accepts the connection and never
    /// answers ends within the 10 s probe timeout, and neither the text nor
    /// the JSON report carries the URL's credentials, path or query.
    #[test]
    fn doctor_relay_probe_is_bounded_and_redacted() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for c in listener.incoming() {
                held.push(c);
            }
        });
        let tmp = tempfile::tempdir().unwrap();
        let url = format!("http://alice:s3cretpw@127.0.0.1:{port}/base/path?token=hrt1leak#frag");
        let cfg = bare_cfg(tmp.path(), Some(&url));
        std::fs::create_dir_all(&cfg.data_dir).unwrap();
        let mut r = checks::Report::new("doctor");
        let t0 = std::time::Instant::now();
        doctor_workspace_checks(&cfg, &mut r);
        assert!(t0.elapsed() < Duration::from_secs(20), "doctor hung for {:?}", t0.elapsed());
        let relay = r.checks.iter().find(|c| c.id == "relay.reachable").unwrap();
        assert_eq!(relay.status, checks::Status::Fail);
        let out = format!("{}\n{}\n{}", r.to_json(0), doctor_text(&r, false, false), status_report(&cfg));
        assert!(out.contains(&format!("http://127.0.0.1:{port}")), "{out}");
        for leak in ["alice", "s3cretpw", "hrt1leak", "token=", "/base/path", "frag"] {
            assert!(!out.contains(leak), "{leak} printed: {out}");
        }
    }
}
