//! `ligus serve` — the home host for the iOS app.
//!
//! One process on an always-on box (the lab's SFF) that is a normal Ligus
//! device in the user's workspace — its E2EE keys never leave the box — and:
//!
//! 1. runs the relay sync loop (push local events, pull + verify peers'),
//! 2. runs the agent worker for the agents hosted here (`worker.rs`), with the
//!    offline harness's tool loop for local model runtimes,
//! 3. exposes a small JSON API + a Server-Sent Events stream to the phone,
//!    bound to a loopback/LAN/tailnet address and gated by a bearer token
//!    (`api.rs`; the contract is in `docs/ios-app.md`),
//! 4. pushes notifications through a self-hosted ntfy (`ntfy.rs`),
//! 5. optionally keeps a relay tunnel open so the phone reaches the API
//!    without a VPN (`tunnel/`, spec `docs/relay-tunnel.md`; off by default),
//! 6. serves the owner-defined dashboard (`dashboard.rs`).

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use ligus_core::{workspace_config_session_id, ChatSession, HostKind, MessageRole, Timestamp, WorkspaceHost};
use ligus_runtime::device_enroll::Standing;
use ligus_runtime::{ChatService, EventStore};
use serde_json::json;
use tokio::sync::{watch, Notify};
use uuid::Uuid;

use crate::reply::{env_flag, ToolSettings};
use crate::worker::{self, TurnSink, TurnState, WorkerCtx};
use crate::{env_opt, open_service, sync_once, uuid_of_room, Config};

pub mod admissions_api;
pub mod api;
pub mod appearance;
pub mod contacts_api;
pub mod dashboard;
pub mod dashboard_ligus;
pub mod dms;
pub mod drafts;
pub mod flow_api;
pub mod forges_api;
pub mod hub;
pub mod identity_api;
pub mod invite_code;
pub use invite_code::admission_code;
pub mod lab;
pub mod listener;
pub mod me_api;
pub mod memory_api;
pub mod notify;
pub mod ntfy;
pub mod pair;
pub mod people;
pub mod phone_actions_api;
pub mod phone_enroll_api;
pub mod presence_api;
pub mod proposal_file;
pub mod repos_api;
pub mod sets;
pub mod tunnel;
pub mod typing_api;
pub mod workspace_repos_api;

#[cfg(test)]
mod identity_tests;
#[cfg(test)]
mod tests;

/// How long `ligus serve` waits for a turn in flight on SIGTERM / Ctrl-C.
const STOP_GRACE: Duration = Duration::from_secs(12);
/// How long a turn stopped at shutdown gets to wind down (complete its
/// placeholder, drop its worktree).
const STOPPED_TURN_GRACE: Duration = Duration::from_secs(3);

/// Replies and proposals created this long before serve started don't push.
const NOTIFY_HISTORY_MINUTES: i64 = 10;

/// The operator's token file in the data dir (D5): the credential for the
/// box owner's routes and for secrets on a loopback listener. Separate from
/// `serve.token`, which every direct-path phone holds.
pub const OPERATOR_TOKEN_FILE: &str = "serve.operator.token";

/// Where `serve.operator.token` lives for a data dir.
pub fn operator_token_path(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join(OPERATOR_TOKEN_FILE)
}

/// How long a client may take to send a request's headers (also the
/// longest an idle keep-alive connection waits for its next request).
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a connection may go without a byte either way before it is
/// closed. Above the event stream's 15 s heartbeat and the longest git or
/// forge call a handler waits on.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Default listen address: loopback only. On the home host the systemd unit
/// passes the box's Tailscale IP.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8787";

/// Everything `ligus serve` is configured with besides the workspace [`Config`].
#[derive(Debug, Clone)]
pub struct ServeOptions {
    pub listen: SocketAddr,
    pub token_file: PathBuf,
    /// [`HEADER_READ_TIMEOUT`] (tests shorten it).
    pub header_read_timeout: Duration,
    /// [`IDLE_TIMEOUT`] (tests shorten it).
    pub idle_timeout: Duration,
    pub ntfy: Option<ntfy::Ntfy>,
    /// The URL the phone uses (pairing link); default `http://<listen>`.
    pub public_url: Option<String>,
    /// The host label other members see for this box.
    pub label: String,
    /// Display name this box's identity posts the phone's messages under.
    pub name: Option<String>,
    pub pair: bool,
    pub prometheus_url: String,
    /// Whether `LIGUS_PROMETHEUS_URL` was set: with no `dashboard.toml`, only
    /// then is the default (lab) dashboard generated.
    pub prometheus_configured: bool,
    /// `dashboard.toml` (`LIGUS_DASHBOARD_FILE`, default in the data dir).
    pub dashboard_file: PathBuf,
    /// `@mention` added to a phone message that names no agent.
    pub default_agent: Option<String>,
    /// Relay sync cadence.
    pub sync_every: Duration,
    /// Remote access through a relay tunnel (`None` = off, the default).
    pub remote: Option<tunnel::RelaySettings>,
    /// Where `GET /v1/appearance` finds the host's theme (`appearance.rs`).
    pub appearance: appearance::AppearanceConfig,
    /// Tests only: run the box as a person (it founds its workspace or
    /// joins with a person's invite), for the tests of person-only features
    /// (DMs, invites). A real box is always a service member.
    #[cfg(test)]
    pub test_person: bool,
}

impl ServeOptions {
    /// Flags first, then `LIGUS_SERVE_*` / `LIGUS_NTFY_*` env, then defaults.
    #[allow(clippy::too_many_arguments)]
    pub fn from_flags(
        cfg: &Config,
        listen: Option<String>,
        token_file: Option<String>,
        ntfy_url: Option<String>,
        ntfy_topic: Option<String>,
        public_url: Option<String>,
        label: Option<String>,
        name: Option<String>,
        pair: bool,
    ) -> Result<Self> {
        // Phase B: the box is its own service member; acting for a person
        // (`LIGUS_SERVE_OWNER`) is gone, and says so rather than being ignored.
        if env_opt("LIGUS_SERVE_OWNER").is_some() {
            bail!("LIGUS_SERVE_OWNER is no longer supported: {}", crate::admit::OWNER_MODE_REMOVED);
        }
        let listen_raw = listen.or_else(|| env_opt("LIGUS_SERVE_LISTEN")).unwrap_or_else(|| DEFAULT_LISTEN.into());
        let listen: SocketAddr = listen_raw
            .parse()
            .with_context(|| format!("--listen must be IP:PORT (got '{listen_raw}')"))?;
        if !pair::bind_allowed(listen.ip()) && !env_flag("LIGUS_SERVE_ALLOW_ANY_BIND", false) {
            bail!(
                "refusing to listen on {listen}: bind to loopback, a private LAN address or the box's Tailscale IP \
                 (set LIGUS_SERVE_ALLOW_ANY_BIND=1 to override)"
            );
        }
        let ntfy = ntfy::settings(
            ntfy_url.or_else(|| env_opt("LIGUS_NTFY_URL")),
            ntfy_topic.or_else(|| env_opt("LIGUS_NTFY_TOPIC")),
            env_opt("LIGUS_NTFY_TOKEN"),
            env_flag("LIGUS_NTFY_ALLOW_HTTP", false),
            env_flag("LIGUS_NTFY_TITLES", false),
        )?;
        Ok(Self {
            listen,
            token_file: token_file
                .or_else(|| env_opt("LIGUS_SERVE_TOKEN_FILE"))
                .map(PathBuf::from)
                .unwrap_or_else(|| cfg.data_dir.join("serve.token")),
            header_read_timeout: HEADER_READ_TIMEOUT,
            idle_timeout: IDLE_TIMEOUT,
            ntfy,
            public_url: public_url.or_else(|| env_opt("LIGUS_SERVE_PUBLIC_URL")),
            label: label
                .or_else(|| env_opt("LIGUS_SERVE_LABEL"))
                .or_else(ligus_runtime::machine_hostname)
                .unwrap_or_else(|| "home host".into()),
            name: name.or_else(|| env_opt("LIGUS_SERVE_NAME")),
            pair,
            prometheus_url: env_opt("LIGUS_PROMETHEUS_URL").unwrap_or_else(|| "http://127.0.0.1:9090".into()),
            prometheus_configured: env_opt("LIGUS_PROMETHEUS_URL").is_some(),
            dashboard_file: dashboard::config_path(&cfg.data_dir),
            default_agent: env_opt("LIGUS_SERVE_DEFAULT_AGENT").map(|a| a.trim().trim_start_matches('@').to_string()),
            sync_every: Duration::from_secs(3),
            remote: None,
            appearance: appearance::AppearanceConfig::from_env(),
            #[cfg(test)]
            test_person: false,
        })
    }

    /// The base URL the phone uses.
    pub fn server_url(&self) -> String {
        self.public_url.clone().unwrap_or_else(|| pair::default_server_url(self.listen))
    }
}

/// Tests only: the pre-0.1 start of a box that is a person (see
/// [`ServeOptions::test_person`]): sync, then found or join as itself.
#[cfg(test)]
async fn test_person_start(cfg: &Config) -> Result<(ChatService, bool)> {
    let synced = match &cfg.relay_url {
        Some(_) => sync_once(cfg).await.is_ok(),
        None => false,
    };
    Ok((open_service(cfg)?, synced))
}

/// Register this box as an always-on host of the workspace it joined as its
/// own service member ([`crate::admit::prepare_box`] made it a member).
fn register_own(svc: &mut ChatService, ws: Uuid, host_id: &str, opts: &ServeOptions) -> Result<()> {
    let host = crate::with_gateways(WorkspaceHost::new(host_id.to_string(), HostKind::Worker, opts.label.clone()));
    svc.ensure_host(ws, host).map(|_| ()).context("registering this box as a workspace host")
}

/// State shared by the API handlers, the sync loop and the worker thread.
pub struct Shared {
    pub cfg: Config,
    pub opts: ServeOptions,
    pub ws: Uuid,
    token: RwLock<String>,
    /// The operator's token (`serve.operator.token`, D5): the box owner's
    /// routes and secrets on a loopback listener. No phone ever gets it.
    operator_token: RwLock<String>,
    /// Open event streams per credential (`api::events`).
    pub sse_slots: Arc<api::SseSlots>,
    /// Bumped whenever the bearer token changes: direct-path streams opened
    /// with the old token end.
    pub token_gen: watch::Sender<u64>,
    /// How many times the revocation watch re-checked the roster.
    pub revocation_checks: std::sync::atomic::AtomicU64,
    /// Wakes the revocation watch at once: a sync round pulled events or
    /// this process wrote one, so a `DeviceRevoked` it folds in is enforced
    /// then, not at the next poll ([`revocation_watch`]).
    pub roster_kick: Notify,
    /// How many full passes the revocation watch has completed (each one
    /// after acting on what it saw), so tests can await "the watch has looked
    /// since X" instead of sleeping.
    pub revocation_passes: std::sync::atomic::AtomicU64,
    pub host_id: String,
    /// The relay tunnel, when remote access is enabled.
    pub tunnel: Option<Arc<tunnel::TunnelHost>>,
    pub svc: Mutex<ChatService>,
    pub hub: hub::Hub,
    differ: Mutex<hub::Differ>,
    /// Replies this box's worker is streaming: their tokens already went out
    /// as `message.delta`, so the differ doesn't repeat their checkpoints.
    local_streams: Mutex<HashSet<Uuid>>,
    /// chat id → id of the last message the phone has seen.
    read_marks: Mutex<HashMap<Uuid, Uuid>>,
    pub sync_kick: Notify,
    pub worker_kick: Notify,
    /// The worker's running turns, each with its own stop signal.
    pub turn_stops: Arc<ligus_runtime::turn_engine::TurnStops>,
    stopping: AtomicBool,
    /// Tests only: treat a bearer request as confirmed by the owner's
    /// presence (the HTTP tests have no phone or passkey).
    #[cfg(test)]
    pub test_presence: AtomicBool,
    /// When this process started (see [`Shared::fresh`]).
    started_at: Timestamp,
    pub http: reqwest::Client,
    /// Panel results, each kept for its `refresh`.
    pub dashboard_cache: dashboard::Cache,
    rt: tokio::runtime::Handle,
    /// "Send to agent to revise" requests awaiting the agent's new version,
    /// by the proposal they revise (`api::request_changes`). In memory: a
    /// restart before the revision lands leaves it unlinked.
    pub revisions: Mutex<HashMap<Uuid, api::PendingRevision>>,
    /// Whose device this box is (see [`Standing`]): its own identity, its
    /// owner's enrolled device, or revoked. Re-read after every pull.
    standing: Mutex<Standing>,
    /// Host liveness from the relay's presence channel (see
    /// [`ligus_runtime::host_presence`]), refreshed by `presence_loop`.
    pub presence: ligus_runtime::HostPresence,
    /// The last successful relay sync round and the latest failure since.
    sync_status: Mutex<(Option<time::OffsetDateTime>, Option<String>)>,
    /// (chat, agent) turns this box's worker has queued or is running.
    busy: Mutex<HashSet<(Uuid, Uuid)>>,
    /// The dashboard's workspace snapshot, reused for a few seconds.
    ligus_snapshot: Mutex<Option<(std::time::Instant, Arc<dashboard_ligus::LigusSnapshot>)>>,
    /// MCP servers for `source = "mcp"` dashboard panels (`LIGUS_MCP_CONFIG`,
    /// read at start), or why there are none.
    pub dashboard_mcp: std::result::Result<ligus_runtime::McpRegistry, String>,
    /// Repos from the phone: the GitHub backend, clone jobs, checkout locks.
    pub repos: repos_api::RepoState,
    /// Events waiting for a phone's passkey (B3 `presence/prepare`).
    pub presence_drafts: presence_api::Pending,
    /// Box admissions waiting for a phone's passkey and signature (Phase B).
    pub admission_drafts: admissions_api::Drafts,
    /// Phone actions waiting for the phone's signature (Phase B).
    pub phone_action_drafts: phone_actions_api::Drafts,
    /// The box's direct messages (`dms.rs`).
    pub dms: dms::DmStore,
    /// Wakes the DM sync loop after a local write.
    pub dm_kick: Notify,
    /// One contact publish or drop-box read at a time.
    pub contact_lock: tokio::sync::Mutex<()>,
    /// DM invites waiting for the peer's contact id (`ligus_runtime::dm_delivery`).
    pub held: ligus_runtime::dm_delivery::HeldInvites,
    /// Wakes the held-invite loop (after a pull, or a new held invite).
    pub held_kick: Notify,
    /// The last contacts status (`contacts_api::status`).
    pub contact_state: Mutex<String>,
    /// Drop-box invites from the last listing (codes stay on the box).
    pub pending_codes: identity_api::PendingCodes,
    /// The phone's typing and who else is typing (`typing_api`).
    pub typing: typing_api::Typing,
    /// Reverse-QR phone enrollment: requests relayed to desktops (`phone_enroll_api`).
    pub phone_enroll: phone_enroll_api::EnrollBox,
}

/// How long a dashboard snapshot of the workspace is reused.
const LIGUS_SNAPSHOT_TTL: Duration = Duration::from_secs(3);

/// The live chats of workspace `ws` (config log and archived chats excluded).
pub fn chats_of(svc: &ChatService, ws: Uuid) -> Result<Vec<ChatSession>> {
    let config = workspace_config_session_id(ws);
    let mut out = Vec::new();
    for id in svc.store().list_session_ids()? {
        if id == config {
            continue;
        }
        if let Ok(Some(s)) = svc.load(id) {
            if s.workspace_id == ws && !s.archived && id != workspace_config_session_id(s.workspace_id) {
                out.push(s);
            }
        }
    }
    Ok(out)
}

/// Gather a dashboard snapshot of workspace `ws` from the store.
#[allow(clippy::too_many_arguments)]
pub fn ligus_snapshot(
    svc: &ChatService,
    ws: Uuid,
    this_host_id: &str,
    this_host_label: &str,
    presence: &ligus_runtime::HostPresence,
    busy_here: &HashSet<Uuid>,
    host: dashboard_ligus::HostState,
) -> Result<dashboard_ligus::LigusSnapshot> {
    let chats = chats_of(svc, ws)?;
    let config_id = workspace_config_session_id(ws);
    let config = svc.load(config_id)?.unwrap_or_else(|| {
        let mut empty = ChatSession::new("", ws, "");
        empty.id = config_id;
        empty
    });
    let now = Timestamp::now();
    let hosts: Vec<(WorkspaceHost, bool)> = svc
        .list_hosts(ws)?
        .into_iter()
        .map(|h| {
            let on = h.id == this_host_id || presence.is_online(&h, &now);
            (h, on)
        })
        .collect();
    let me = svc.author().id.clone();
    Ok(dashboard_ligus::LigusSnapshot::compute(&dashboard_ligus::LigusInputs {
        now: now.inner(),
        offset: dashboard_ligus::local_offset(),
        me: &me,
        this_host_id,
        this_host_label,
        chats: &chats,
        config: &config,
        hosts: &hosts,
        busy_here,
        host,
    }))
}

impl dashboard::LigusProvider for Shared {
    fn snapshot(&self) -> Result<Arc<dashboard_ligus::LigusSnapshot>> {
        let mut cached = self.ligus_snapshot.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, snap)) = cached.as_ref() {
            if at.elapsed() < LIGUS_SNAPSHOT_TTL {
                return Ok(snap.clone());
            }
        }
        let busy: HashSet<Uuid> = self.busy.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(_, a)| *a).collect();
        let (last_ok, err) = self.sync_status.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let remote = self.tunnel.as_ref().map(|t| {
            let live = t.live_status();
            let phones: Vec<_> = t.registry.list().into_iter().filter(|p| p.is_active()).collect();
            dashboard_ligus::RemoteState {
                connected: live.connected,
                push: t.push_available.load(Ordering::SeqCst),
                phones: phones.len(),
                phones_with_push: phones.iter().filter(|p| p.push_enabled).count(),
            }
        });
        let host = dashboard_ligus::HostState {
            serving: true,
            relay_configured: self.cfg.relay_url.is_some(),
            last_sync_ok: last_ok,
            sync_error: err,
            remote,
            ntfy: self.opts.ntfy.is_some(),
        };
        let snap = {
            let svc = self.lock();
            Arc::new(ligus_snapshot(&svc, self.ws, &self.host_id, &self.opts.label, &self.presence, &busy, host)?)
        };
        *cached = Some((std::time::Instant::now(), snap.clone()));
        Ok(snap)
    }
}

impl Shared {
    /// The dashboard's sources: Prometheus/HTTP, this workspace, MCP.
    pub fn dashboard_sources(&self) -> dashboard::Sources<'_> {
        dashboard::Sources {
            http: &self.http,
            prometheus_url: &self.opts.prometheus_url,
            ligus: Some(self),
            mcp: self.dashboard_mcp.as_ref().map_err(String::as_str),
        }
    }

    fn sync_ok(&self) {
        *self.sync_status.lock().unwrap_or_else(|e| e.into_inner()) = (Some(time::OffsetDateTime::now_utc()), None);
    }

    fn sync_failed(&self, e: &str) {
        self.sync_status.lock().unwrap_or_else(|e| e.into_inner()).1 = Some(e.to_string());
    }

    /// The round worked but the relay refused this room's writes (or the
    /// room is truncated): synced, with the reason kept (logged once).
    fn sync_limited(&self, why: &str) {
        let mut st = self.sync_status.lock().unwrap_or_else(|e| e.into_inner());
        if st.1.as_deref() != Some(why) {
            eprintln!("sync: {why}");
        }
        *st = (Some(time::OffsetDateTime::now_utc()), Some(why.to_string()));
    }
}

/// The MCP servers dashboard panels may use (see [`Shared::dashboard_mcp`]).
pub fn dashboard_mcp_registry() -> std::result::Result<ligus_runtime::McpRegistry, String> {
    match crate::mcp_config::load_from_env() {
        Ok(specs) if specs.is_empty() => Err("no MCP servers are configured on this host (LIGUS_MCP_CONFIG)".into()),
        Ok(specs) => Ok(ligus_runtime::McpRegistry::new(specs)),
        Err(e) => Err(format!("LIGUS_MCP_CONFIG: {e:#}")),
    }
}

/// Why a revoked box refuses writes.
const REVOKED: &str = "this box's device was revoked by a workspace Admin; start it from a fresh data dir and admit it again (`ligus admit request`)";

/// The device-local serve state file (read marks).
fn state_path(cfg: &Config) -> PathBuf {
    cfg.data_dir.join("serve-state.json")
}

impl Shared {
    pub fn lock(&self) -> MutexGuard<'_, ChatService> {
        self.svc.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The current direct-path bearer token (`ligus phones revoke` rotates it).
    pub fn token(&self) -> String {
        self.token.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The operator's token (`serve.operator.token`).
    pub fn operator_token(&self) -> String {
        self.operator_token.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Re-read the operator token file (the operator replaced it).
    fn reload_operator_token(&self) {
        let Ok(t) = std::fs::read_to_string(operator_token_path(&self.cfg.data_dir)) else { return };
        let t = t.trim();
        if t.len() >= 32 && t != self.token() && *self.operator_token.read().unwrap_or_else(|e| e.into_inner()) != t {
            ligus_runtime::secret_redact::register(t);
            *self.operator_token.write().unwrap_or_else(|e| e.into_inner()) = t.to_string();
            println!("ligus serve: the operator token changed on disk; using the new one.");
        }
    }

    /// Re-read the token file; a rotated token takes effect at once.
    fn reload_token(&self) {
        let Ok(t) = std::fs::read_to_string(&self.opts.token_file) else { return };
        let t = t.trim();
        // Never the operator's: the phones would get it from the tunnel.
        if t.len() >= 32 && t != self.operator_token() && *self.token.read().unwrap_or_else(|e| e.into_inner()) != t {
            *self.token.write().unwrap_or_else(|e| e.into_inner()) = t.to_string();
            self.token_gen.send_modify(|g| *g += 1);
            println!("ligus serve: the bearer token changed on disk; using the new one.");
        }
    }

    pub fn stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// The workspace's live (non-archived) chats, config log excluded.
    pub fn workspace_chats(&self) -> Result<Vec<ChatSession>> {
        chats_of(&self.lock(), self.ws)
    }

    /// Whose device this box currently is.
    pub fn standing(&self) -> Standing {
        self.standing.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// `identity` for `GET /v1/health`: who the phone posts as.
    ///
    /// `mode`: `owner` (enrolled as the owner's device: the phone acts as the
    /// owner), `member` (its own workspace member, the pre-enrollment model),
    /// `pending` (`LIGUS_SERVE_OWNER` set, waiting for the owner's approval) or
    /// `revoked` (read-only until re-enrolled).
    pub fn identity_json(&self) -> serde_json::Value {
        let (actor, fingerprint) = {
            let svc = self.lock();
            (svc.author().clone(), ligus_runtime::device_enroll::fingerprint(&svc.device_public_key()))
        };
        let mode = match self.standing() {
            // Never reached: a box enrolled as a person's device refuses to
            // start, and is paused if one appears while it runs.
            Standing::OwnerDevice(_) => "owner",
            Standing::Revoked => "revoked",
            Standing::Own => "member",
        };
        json!({ "mode": mode, "actorId": actor.id, "displayName": ligus_proto::names::clean_display_name(&actor.display_name), "deviceFingerprint": fingerprint })
    }

    /// Re-read this box's standing after a pull, switching identity when the
    /// owner approves it (or revokes it) while it runs. Returns whether it changed.
    pub fn refresh_standing(&self) -> bool {
        let next = match self.lock().device_standing(self.ws, None) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("serve: reading this box's device standing failed: {e}");
                return false;
            }
        };
        let prev = self.standing();
        if next == prev {
            return false;
        }
        {
            let mut svc = self.lock();
            match (&prev, &next) {
                (_, Standing::Revoked) => {
                    svc.set_write_block(Some(REVOKED.into()));
                    eprintln!("ligus serve: {REVOKED}.");
                }
                // Phase B: a box never acts as a person. Someone certified
                // this box's device for their account while it ran: stop
                // writing rather than take on that identity.
                (_, Standing::OwnerDevice(owner)) => {
                    let why = format!("this box's device was certified as {}'s device; {}", owner.display_name, crate::admit::OWNER_MODE_REMOVED);
                    eprintln!("ligus serve: {why}");
                    svc.set_write_block(Some(why));
                }
                (Standing::OwnerDevice(_), Standing::Own) => {
                    // The owner's certificate no longer counts (the owner left the
                    // workspace): don't keep writing as them.
                    svc.set_write_block(Some("this box is no longer enrolled as its owner's device; restart ligus serve".into()));
                    eprintln!("ligus serve: no longer enrolled as the owner's device; writes are paused until a restart.");
                }
                _ => {}
            }
        }
        *self.standing.lock().unwrap_or_else(|e| e.into_inner()) = next;
        true
    }

    /// Phone revocation is three-part (Phase A, item 6; wired in Phase B): a
    /// workspace `DeviceRevoked` of a phone's device (its own key never signs
    /// again; the fold refuses it) also revokes every tunnel pairing bound to
    /// that device here, at once (live sessions end, new ones and pairings
    /// are refused, pushes stop), then tells the relay to drop the pairing and
    /// its push token and ends its langstroth-auth session, with per-step
    /// status ([`tunnel::revocation`]). Runs whenever the store changes,
    /// whatever wrote the event ([`revocation_watch`]). Returns the pairings
    /// revoked.
    pub fn revoke_phones_of_revoked_devices(&self) -> Vec<String> {
        let (revoked, revoked_phones): (std::collections::BTreeSet<Uuid>, std::collections::BTreeSet<Uuid>) = {
            let svc = self.lock();
            match svc.store().load_config(self.ws, None, None) {
                Ok((_, t)) => {
                    let devices = &t.current().devices;
                    (
                        devices.iter().filter(|(_, d)| d.revoked).map(|(id, _)| *id).collect(),
                        devices.iter().filter(|(_, d)| d.revoked && d.phone).map(|(id, _)| *id).collect(),
                    )
                }
                Err(_) => return Vec::new(),
            }
        };
        // A phone is a P-256 device on the roster, or any device a pairing
        // here is bound to.
        let mut revoked_phones = revoked_phones;
        if let Some(t) = self.tunnel.as_ref() {
            revoked_phones.extend(t.registry.list().into_iter().filter_map(|r| r.device_id).filter(|d| revoked.contains(d)));
        }
        // This box's own device revoked (Phase B: a box is its own service
        // member, and an Admin revokes its device like any other): it can no
        // longer act in the workspace, so nothing it holds stays usable. Every
        // pairing here is cut (a phone stops handing messages to a box that
        // was cut off, perhaps for being compromised), and the direct-path
        // token rotates once. Pairings made later are cut as they appear (the
        // watch re-checks whenever the registry changes).
        let own = Uuid::parse_str(&self.host_id).ok().filter(|d| revoked.contains(d));
        if let Some(d) = own {
            revoked_phones.insert(d);
        }
        self.rotate_token_for(&revoked_phones);
        let Some(tunnel) = self.tunnel.as_ref() else { return Vec::new() };
        let pids = match own {
            Some(_) => tunnel.registry.list().into_iter().filter(|r| r.is_active()).map(|r| r.pid).collect(),
            None => tunnel.registry.pairings_of_revoked(&revoked),
        };
        let mut by_device: std::collections::BTreeMap<Uuid, Vec<String>> = std::collections::BTreeMap::new();
        for pid in &pids {
            let bound = tunnel.registry.get(pid).and_then(|r| r.device_id).filter(|d| revoked.contains(d));
            if let Some(d) = bound.or(own) {
                by_device.entry(d).or_default().push(pid.clone());
            }
        }
        for (device, pids) in by_device {
            if let Err(e) = tunnel.revoke_device(device, &pids) {
                eprintln!("serve: revoking a revoked phone's pairing failed: {e}");
            }
        }
        pids
    }

    /// A revoked phone held the direct-path bearer token too (its pairing
    /// link carries it): rotate `serve.token` once for each newly revoked
    /// phone device (`serve-token-revocations.json` remembers which ones it
    /// already rotated for). Direct streams opened with the old token end
    /// ([`Self::token_gen`]); the remaining phones fetch the new one over
    /// their tunnel (`GET /v1/tunnel/direct`), or re-pair if they only use
    /// the direct path.
    fn rotate_token_for(&self, revoked_phones: &std::collections::BTreeSet<Uuid>) {
        // The sync loop and the revocation watch both get here: one rotation.
        static ROTATING: Mutex<()> = Mutex::new(());
        let _one = ROTATING.lock().unwrap_or_else(|e| e.into_inner());
        let file = self.cfg.data_dir.join("serve-token-revocations.json");
        let done: std::collections::BTreeSet<Uuid> =
            std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let fresh: Vec<Uuid> = revoked_phones.difference(&done).copied().collect();
        if fresh.is_empty() {
            return;
        }
        if let Err(e) = pair::rotate_token(&self.opts.token_file) {
            eprintln!("serve: rotating the bearer token after a phone revocation failed (retrying): {e:#}");
            return;
        }
        self.reload_token();
        let all: std::collections::BTreeSet<Uuid> = done.union(revoked_phones).copied().collect();
        if let Err(e) = crate::serve::tunnel::write_json(&file, &all) {
            eprintln!("serve: recording the bearer token rotation failed: {e:#}");
        }
        println!("ligus serve: rotated the bearer token because phone device(s) {fresh:?} were revoked; paired phones fetch the new one over the relay");
    }

    /// A workspace device as the tunnel sees it (for binding a pairing).
    pub fn device_lookup(&self, id: Uuid) -> tunnel::device_bind::DeviceLookup {
        use tunnel::device_bind::DeviceLookup;
        let svc = self.lock();
        match svc.store().load_config(self.ws, None, None) {
            Ok((_, t)) => match t.current().devices.get(&id) {
                None => DeviceLookup::Unknown,
                Some(d) if d.revoked => DeviceLookup::Revoked,
                Some(d) => DeviceLookup::Certified { public_key: d.public_key.clone(), phone: d.phone },
            },
            Err(_) => DeviceLookup::Unknown,
        }
    }

    /// The actor id this box writes as.
    pub fn my_actor_id(&self) -> String {
        self.lock().author().id.clone()
    }

    /// Unread count for `s`: messages after the phone's read mark that this
    /// box didn't write (system notes excluded). No mark = all of them.
    pub fn unread(&self, s: &ChatSession, me: &str) -> usize {
        let marks = self.read_marks.lock().unwrap_or_else(|e| e.into_inner());
        let start = match marks.get(&s.id) {
            Some(mark) => match s.messages.iter().position(|m| m.id == *mark) {
                Some(i) => i + 1,
                None => s.messages.len(),
            },
            None => 0,
        };
        s.messages[start.min(s.messages.len())..]
            .iter()
            .filter(|m| m.role != MessageRole::System && !m.superseded)
            .filter(|m| !(m.role == MessageRole::User && m.actor_identity.as_ref().is_some_and(|a| a.id == me)))
            .count()
    }

    /// Advance the read mark for `s` to `message` (never backwards).
    pub fn mark_read(&self, s: &ChatSession, message: Uuid) {
        let mut marks = self.read_marks.lock().unwrap_or_else(|e| e.into_inner());
        let new_idx = s.messages.iter().position(|m| m.id == message);
        let old_idx = marks.get(&s.id).and_then(|old| s.messages.iter().position(|m| m.id == *old));
        if new_idx.is_some() && new_idx >= old_idx {
            marks.insert(s.id, message);
            let body = json!({ "readMarks": &*marks });
            drop(marks);
            let _ = std::fs::write(state_path(&self.cfg), body.to_string());
        }
    }

    /// Diff the projection against the last look and publish the difference
    /// (SSE events, plus ntfy and relay pushes for what [`notify`] picks).
    ///
    /// The sync loop, the worker thread and API handlers all call this. The
    /// projection is loaded while holding the differ lock, so each diff runs on
    /// a snapshot at least as new as the one before it; loading first let a
    /// slow caller diff a stale snapshot after a newer one, rolling the differ
    /// back and re-emitting `message.done` (and its ntfy push).
    pub fn refresh(&self) {
        let (author, links, viewer) = {
            let svc = self.lock();
            (svc.author().clone(), svc.device_links(self.ws).unwrap_or_default(), api::Viewer::of(&svc))
        };
        let me = author.id.clone();
        let mut notices = Vec::new();
        let mut created = Vec::new();
        {
            let mut differ = self.differ.lock().unwrap_or_else(|e| e.into_inner());
            let sessions = match self.phone_chats() {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("serve: refresh failed: {e}");
                    return;
                }
            };
            let mut recruit_pushed = false;
            let mut phone_ctx: Option<api::PhoneChatCtx> = None;
            for change in differ.diff(&sessions) {
                use hub::Change;
                match change {
                    Change::MessageDelta { chat, message, from } => {
                        if self.local_streams.lock().unwrap_or_else(|e| e.into_inner()).contains(&message) {
                            continue;
                        }
                        let Some(s) = sessions.iter().find(|s| s.id == chat) else { continue };
                        let Some(m) = s.messages.iter().find(|m| m.id == message && !m.superseded) else { continue };
                        if let Some(text) = m.body.get(from..) {
                            self.hub.emit("message.delta", json!({ "chatId": chat, "messageId": message, "text": text }));
                        }
                    }
                    Change::MessageCreated { chat, message } | Change::MessageDone { chat, message } => {
                        let done = matches!(change, Change::MessageDone { .. });
                        if done {
                            self.local_streams.lock().unwrap_or_else(|e| e.into_inner()).remove(&message);
                        }
                        let Some(s) = sessions.iter().find(|s| s.id == chat) else { continue };
                        let Some(m) = s.messages.iter().find(|m| m.id == message && !m.superseded) else { continue };
                        let name = if done { "message.done" } else { "message.created" };
                        let me = notify::Me::in_chat(&author, s, &links);
                        let mut dto = api::message_dto(chat, m, viewer);
                        api::add_requester(&mut dto, s, m, &me);
                        self.hub.emit(name, hub::wrap("message", dto));
                        // Agent replies notify when done; other members'
                        // messages when they appear (only if they mention you).
                        if self.fresh(m.created_at) {
                            notices.extend(notify::for_message(s, m, done, &me));
                        }
                    }
                    Change::ProposalCreated { chat, proposal } => {
                        let Some(s) = sessions.iter().find(|s| s.id == chat) else { continue };
                        let Some(p) = s.proposals.iter().find(|p| p.id == proposal) else { continue };
                        created.push((chat, proposal));
                        self.hub.emit("proposal.created", hub::wrap("proposal", api::proposal_dto_as(s, p, &me)));
                        if self.fresh(p.created_at) {
                            notices.extend(notify::for_proposal(s, p));
                        }
                    }
                    Change::WorkflowApproval { chat, run, node } => {
                        let Some(s) = sessions.iter().find(|s| s.id == chat) else { continue };
                        let Some(r) = s.workflow_runs.iter().find(|r| r.id == run) else { continue };
                        if self.fresh(r.updated_at) {
                            notices.extend(notify::for_workflow_gate(s, r, &node));
                        }
                    }
                    Change::RecruitApprovers { chat, recruit } => {
                        let Some(s) = sessions.iter().find(|s| s.id == chat) else { continue };
                        let Some(r) = s.flow.recruits.iter().find(|r| r.id == recruit) else { continue };
                        // One recruit push per refresh: recruits are Contributor
                        // records any member or agent can write, every one points
                        // at the same waiting proposal, and ntfy has no rate limit.
                        if !recruit_pushed && self.fresh(ligus_core::Timestamp::from_unix_seconds(r.at_ms / 1000)) {
                            let me = notify::Me::in_chat(&author, s, &links);
                            if let Some(n) = notify::for_recruit(&sessions, s, r, &me) {
                                recruit_pushed = true;
                                notices.push(n);
                            }
                        }
                    }
                    Change::AskPending { chat, ask } => {
                        let Some(s) = sessions.iter().find(|s| s.id == chat) else { continue };
                        let Some(a) = s.flow.asks.get(&ask) else { continue };
                        self.hub.emit("ask.pending", json!({ "ask": api::ask_dto(chat, &a.ask) }));
                        if self.fresh(ligus_core::Timestamp::from_unix_seconds(a.ask.at_ms / 1000)) {
                            notices.extend(notify::for_ask(s, &a.ask));
                        }
                    }
                    Change::ProposalUpdated { chat, proposal } => {
                        let Some(s) = sessions.iter().find(|s| s.id == chat) else { continue };
                        let Some(p) = s.proposals.iter().find(|p| p.id == proposal) else { continue };
                        self.hub.emit("proposal.updated", hub::wrap("proposal", api::proposal_dto_as(s, p, &me)));
                    }
                    Change::ChatUpdated { chat } => {
                        let Some(s) = sessions.iter().find(|s| s.id == chat) else { continue };
                        let unread = self.unread(s, &me);
                        let ctx = phone_ctx.get_or_insert_with(|| api::PhoneChatCtx::new(self));
                        self.hub.emit("chat.updated", hub::wrap("chat", ctx.dto(self, s, unread)));
                    }
                }
            }
        }
        // A revision the agent just filed: link it to the version it revises
        // (a write of its own), then publish that change too.
        if self.link_revisions(&created) {
            self.sync_kick.notify_one();
            self.refresh();
        }
        // A DM never pushes (its text stays between the two people and
        // this box); only the workspace's chats do.
        notices.retain(|(n, _)| !self.is_dm_chat(n.chat_id));
        if let Some(t) = &self.tunnel {
            for (_, push) in &notices {
                t.push_all(push);
            }
        }
        if let Some(n) = self.opts.ntfy.clone() {
            for (notice, _) in notices {
                let n = n.clone();
                let client = self.http.clone();
                self.rt.spawn(async move { n.send(&client, &notice).await });
            }
        }
    }

    /// Whether something created at `at` is news worth a push: not history
    /// that synced in late (an outage, or a first sync that failed and seeded
    /// the differ from an incomplete store). The phone still lists it.
    fn fresh(&self, at: Timestamp) -> bool {
        at.inner() >= self.started_at.inner() - time::Duration::minutes(NOTIFY_HISTORY_MINUTES)
    }

    /// After a local write: tell the phone, wake the worker, push to the relay.
    pub fn after_write(&self) {
        self.refresh();
        self.roster_kick.notify_one();
        self.worker_kick.notify_one();
        self.sync_kick.notify_one();
        self.dm_kick.notify_one();
    }

    /// Whether `chat` belongs to one of the box's DMs.
    pub fn is_dm_chat(&self, chat: Uuid) -> bool {
        let ws = self.lock().store().workspace_of(chat).ok().flatten();
        ws.is_some_and(|w| w != self.ws && self.dms.get(w).is_some())
    }
}

/// The worker's live events → SSE, plus a refresh + push on store changes.
struct ServeSink(Arc<Shared>);

impl TurnSink for ServeSink {
    fn status(&self, chat: Uuid, agent: Uuid, state: TurnState, detail: Option<&str>) {
        {
            let mut busy = self.0.busy.lock().unwrap_or_else(|e| e.into_inner());
            match state {
                TurnState::Queued | TurnState::Running => busy.insert((chat, agent)),
                TurnState::Done | TurnState::Error => busy.remove(&(chat, agent)),
            };
        }
        let mut data = json!({ "chatId": chat, "agentId": agent, "state": state.as_str() });
        if let Some(d) = detail {
            data["detail"] = json!(d);
        }
        self.0.hub.emit("turn.status", data);
    }

    fn delta(&self, chat: Uuid, message: Uuid, text: &str) {
        self.0.local_streams.lock().unwrap_or_else(|e| e.into_inner()).insert(message);
        self.0.hub.emit("message.delta", json!({ "chatId": chat, "messageId": message, "text": text }));
    }

    fn changed(&self) {
        self.0.refresh();
        self.0.sync_kick.notify_one();
    }
}

/// A running server. Dropping it (or [`ServeHandle::shutdown`]) stops the API,
/// the sync loop and — between turns — the worker.
pub struct ServeHandle {
    pub addr: SocketAddr,
    pub shared: Arc<Shared>,
    shutdown: watch::Sender<bool>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// The background tasks (API, sync, presence, token watch, tunnel).
    /// Only tests wait for them (so a late round can't recreate a removed
    /// temp data dir); a real stop leaves them to the runtime's shutdown.
    #[cfg_attr(not(test), allow(dead_code))]
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl ServeHandle {
    pub fn shutdown(&self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        let _ = self.shutdown.send(true);
        self.shared.worker_kick.notify_one();
        self.shared.sync_kick.notify_one();
    }

    /// Stop, then give a turn in flight up to `grace` to finish (the worker
    /// checks `stopping` between turns). Past the grace the turn is stopped
    /// (its own stop signal), and gets [`STOPPED_TURN_GRACE`] to wind down.
    /// Returns whether the worker stopped in time; if not, the next start
    /// finishes the cut-off reply ([`worker::recover_interrupted_turns`]).
    pub async fn stop(&self, grace: Duration) -> bool {
        self.shutdown();
        let Some(worker) = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            return true;
        };
        let mut deadline = tokio::time::Instant::now() + grace;
        let mut stopped_turns = false;
        while !worker.is_finished() {
            if tokio::time::Instant::now() >= deadline {
                if stopped_turns {
                    return false;
                }
                stopped_turns = true;
                if self.shared.turn_stops.close() > 0 {
                    eprintln!("serve: stopping the turn in flight");
                }
                deadline = tokio::time::Instant::now() + STOPPED_TURN_GRACE;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let _ = worker.join();
        // The finished turn's last writes go out before we exit.
        if self.shared.cfg.relay_url.is_some() {
            let _ = tokio::time::timeout(Duration::from_secs(3), sync_once(&self.shared.cfg)).await;
        }
        #[cfg(test)]
        self.wait_stopped_blocking(grace);
        true
    }
}

impl Drop for ServeHandle {
    fn drop(&mut self) {
        self.shutdown();
        // Tests: the tasks and the worker finish before the test's temp dir
        // is removed (a late sync round would recreate the data dir there).
        #[cfg(test)]
        self.wait_stopped_blocking(Duration::from_secs(10));
    }
}

#[cfg(test)]
impl ServeHandle {
    fn wait_stopped_blocking(&self, grace: Duration) {
        let deadline = std::time::Instant::now() + grace;
        if let Some(w) = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take() {
            while !w.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            if w.is_finished() {
                let _ = w.join();
            }
        }
        let tasks: Vec<_> = std::mem::take(&mut *self.tasks.lock().unwrap_or_else(|e| e.into_inner()));
        let Ok(rt) = tokio::runtime::Handle::try_current() else { return };
        if rt.runtime_flavor() != tokio::runtime::RuntimeFlavor::MultiThread {
            return;
        }
        tokio::task::block_in_place(|| {
            rt.block_on(async {
                for t in tasks {
                    let left = deadline.saturating_duration_since(std::time::Instant::now());
                    let abort = t.abort_handle();
                    if tokio::time::timeout(left, t).await.is_err() {
                        abort.abort();
                    }
                }
            })
        });
    }
}

fn load_read_marks(cfg: &Config) -> HashMap<Uuid, Uuid> {
    std::fs::read_to_string(state_path(cfg))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| serde_json::from_value(v.get("readMarks")?.clone()).ok())
        .unwrap_or_default()
}

/// Start everything on the current (multi-threaded) tokio runtime and return
/// once the API is listening.
pub async fn start(cfg: Config, opts: ServeOptions) -> Result<ServeHandle> {
    // Agents on this box never read the serve token, wherever it lives.
    let operator_file = operator_token_path(&cfg.data_dir);
    ligus_runtime::executor::configure_host(
        &cfg.data_dir,
        &[opts.token_file.clone(), operator_file.clone()],
        &[crate::repos::repos_dir(&cfg.data_dir)],
    );
    let token = pair::load_or_create_token(&opts.token_file)?;
    let operator_token = pair::load_or_create_token(&operator_file)?;
    if pair::token_matches(&token, &operator_token) {
        bail!(
            "{} and {} hold the same token: the operator's must be its own (delete {} and start again)",
            opts.token_file.display(),
            operator_file.display(),
            operator_file.display()
        );
    }
    if let Err(e) = crate::purge_foreign_workspaces(&cfg) {
        eprintln!("serve: foreign-workspace cleanup skipped: {e}");
    }
    // Phase B: sync first, never found a workspace, join only as this box's
    // own service member through an admission (`crate::admit::prepare_box`).
    let mut cfg = cfg;
    let admission = crate::admit::admission_from_file(crate::env_opt(crate::admit::ADMISSION_FILE_ENV))?;
    #[cfg(test)]
    let person = opts.test_person;
    #[cfg(not(test))]
    let person = false;
    #[cfg(test)]
    let started = if person { Some(test_person_start(&cfg).await?) } else { None };
    #[cfg(not(test))]
    let started: Option<(ChatService, bool)> = None;
    let (mut svc, synced) = match started {
        Some(s) => s,
        None => {
            // The name the box joins under: its own, or its label replacing a
            // name nobody chose.
            crate::admit::mark_box(&cfg)?;
            if opts.name.is_none() {
                let identity = ligus_runtime::IdentityStore::new(&cfg.data_dir, ligus_runtime::secret_store::StoreKeyVault::for_data_dir(&cfg.data_dir));
                if let Err(e) = crate::adopt_box_name(&identity, &opts.label, ligus_runtime::machine_hostname().as_deref()) {
                    eprintln!("serve: renaming this box failed: {e}");
                }
            }
            crate::admit::prepare_box(&mut cfg, &opts.label, opts.name.as_deref(), admission, |c| async move { sync_once(&c).await }).await?
        }
    };
    let first_sync = synced.then(time::OffsetDateTime::now_utc);
    let ws = uuid_of_room(&cfg.room);
    let host_id = svc.device_id().to_string();
    let standing = svc.device_standing(ws, None).unwrap_or(Standing::Own);
    match &standing {
        Standing::Revoked => {
            svc.set_write_block(Some(REVOKED.into()));
            eprintln!("ligus serve: {REVOKED}.");
        }
        _ => {
            if let Some(name) = &opts.name {
                svc.set_author_display_name(name.clone());
            } else {
                // A box still called by the host name `ligus enroll` gave it
                // takes its label so the
                // phone's messages show the name the box was given.
                let identity = ligus_runtime::IdentityStore::new(&cfg.data_dir, ligus_runtime::secret_store::StoreKeyVault::for_data_dir(&cfg.data_dir));
                let was = svc.author().display_name.clone();
                match crate::adopt_box_name(&identity, &opts.label, ligus_runtime::machine_hostname().as_deref()) {
                    Ok(Some(name)) => {
                        println!("ligus serve: this box's name is now '{name}' (was '{was}').");
                        svc.set_author_display_name(name);
                    }
                    Ok(None) => {}
                    Err(e) => eprintln!("serve: renaming this box failed: {e}"),
                }
            }
        }
    }
    // A turn cut off by the last stop would otherwise stay "streaming" forever
    // and never be answered (its placeholder counts as the reply).
    if standing != Standing::Revoked {
        match worker::recover_interrupted_turns(&mut svc) {
            Ok(done) if !done.is_empty() => eprintln!("serve: finished {} reply(s) interrupted by the last stop", done.len()),
            Ok(_) => {}
            Err(e) => eprintln!("serve: interrupted-turn recovery failed: {e}"),
        }
        if person {
            let _ = svc.ensure_self_member(workspace_config_session_id(ws), ws);
        }
        register_own(&mut svc, ws, &host_id, &opts)?;
        crate::publish_key_agreement(&cfg, &mut svc);
    }
    let tunnel = match &opts.remote {
        Some(settings) => Some(tunnel::TunnelHost::open(&cfg.data_dir, settings.clone()).context("opening the relay tunnel state")?),
        None => None,
    };
    let shared = Arc::new(Shared {
        ws,
        token: RwLock::new(token),
        operator_token: RwLock::new(operator_token),
        sse_slots: Default::default(),
        token_gen: watch::Sender::new(0),
        revocation_checks: Default::default(),
        roster_kick: Notify::new(),
        revocation_passes: Default::default(),
        host_id,
        tunnel,
        svc: Mutex::new(svc),
        hub: hub::Hub::new(),
        differ: Mutex::new(hub::Differ::default()),
        local_streams: Mutex::new(HashSet::new()),
        read_marks: Mutex::new(load_read_marks(&cfg)),
        sync_kick: Notify::new(),
        worker_kick: Notify::new(),
        turn_stops: ligus_runtime::turn_engine::TurnStops::new(),
        stopping: AtomicBool::new(false),
        #[cfg(test)]
        test_presence: AtomicBool::new(false),
        started_at: Timestamp::now(),
        http: ligus_forge::http_client_builder().connect_timeout(Duration::from_secs(3)).build()?,
        dashboard_cache: dashboard::Cache::default(),
        rt: tokio::runtime::Handle::current(),
        revisions: Mutex::new(HashMap::new()),
        standing: Mutex::new(standing),
        presence: ligus_runtime::HostPresence::new(),
        sync_status: Mutex::new((first_sync, None)),
        busy: Mutex::new(HashSet::new()),
        presence_drafts: presence_api::Pending::default(),
        admission_drafts: admissions_api::Drafts::default(),
        phone_action_drafts: phone_actions_api::Drafts::default(),
        dms: dms::DmStore::load(&cfg.data_dir),
        dm_kick: Notify::new(),
        contact_lock: tokio::sync::Mutex::new(()),
        held: ligus_runtime::dm_delivery::HeldInvites::for_data_dir(&cfg.data_dir),
        held_kick: Notify::new(),
        contact_state: Mutex::new("unknown".into()),
        pending_codes: identity_api::PendingCodes::default(),
        typing: typing_api::Typing::default(),
        phone_enroll: Default::default(),
        ligus_snapshot: Mutex::new(None),
        dashboard_mcp: dashboard_mcp_registry(),
        repos: repos_api::RepoState::default(),
        cfg,
        opts,
    });
    // Seed the differ and mark every existing chat read, so a restart neither
    // replays history as events nor shows it all as unread.
    {
        let chats = shared.phone_chats()?;
        shared.differ.lock().unwrap_or_else(|e| e.into_inner()).seed(&chats);
        let mut marks = shared.read_marks.lock().unwrap_or_else(|e| e.into_inner());
        for s in &chats {
            if let Some(last) = s.messages.last() {
                marks.entry(s.id).or_insert(last.id);
            }
        }
    }

    let (shutdown, rx) = watch::channel(false);
    let listener = tokio::net::TcpListener::bind(shared.opts.listen)
        .await
        .with_context(|| format!("binding {}", shared.opts.listen))?;
    let addr = listener.local_addr()?;
    let app = api::router(shared.clone());
    let (header_read, idle) = (shared.opts.header_read_timeout, shared.opts.idle_timeout);
    let mut tasks = vec![tokio::spawn(listener::serve(listener, app, header_read, idle, rx.clone()))];
    if shared.cfg.relay_url.is_some() {
        tasks.push(tokio::spawn(sync_loop(shared.clone(), rx.clone())));
        tasks.push(tokio::spawn(presence_loop(shared.clone(), rx.clone())));
        tasks.push(tokio::spawn(contact_loop(shared.clone(), rx.clone())));
        tasks.push(tokio::spawn(phone_enroll_api::relay_loop(shared.clone(), rx.clone())));
    }
    tasks.push(tokio::spawn(dms::dm_sync_loop(shared.clone(), rx.clone())));
    tasks.push(tokio::spawn(contacts_api::held_loop(shared.clone(), rx.clone())));
    tasks.push(tokio::spawn(typing_api::typing_loop(shared.clone(), rx.clone())));
    tasks.push(tokio::spawn(token_watch(shared.clone(), rx.clone())));
    if let Some(t) = shared.tunnel.clone() {
        let weak = Arc::downgrade(&shared);
        t.set_device_lookup(Arc::new(move |id| match weak.upgrade() {
            Some(s) => s.device_lookup(id),
            None => tunnel::device_bind::DeviceLookup::Unknown,
        }));
        tasks.push(tokio::spawn(revocation_watch(shared.clone(), rx.clone())));
        tasks.push(tokio::spawn(tunnel::revocation::run(t.clone(), rx.clone())));
        tasks.push(tokio::spawn(tunnel::pending_loop(t.clone(), rx.clone())));
        // E3b: an uncertified phone gets the add-a-phone routes only.
        let routers = tunnel::session::Routers { api: api::router(shared.clone()), provisional: Some(phone_enroll_api::provisional_router(shared.clone())) };
        tasks.push(tokio::spawn(tunnel::control::run(t, routers, rx.clone(), Default::default())));
    }
    let worker = spawn_worker(shared.clone())?;
    shared.sync_kick.notify_one();
    Ok(ServeHandle { addr, shared, shutdown, worker: Mutex::new(Some(worker)), tasks: Mutex::new(tasks) })
}

/// How often the box refreshes its contact record (relays keep one 30 days).
const CONTACT_EVERY: Duration = Duration::from_secs(15 * 60);

/// Publish the box's contact record and announce its id (`contacts_api`).
async fn contact_loop(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    loop {
        let on = contacts_api::ensure_contact(&shared).await;
        shared.set_contact_status(contacts_api::status(&shared, on));
        tokio::select! {
            _ = tokio::time::sleep(CONTACT_EVERY) => {}
            _ = stop.wait_for(|s| *s) => return,
        }
    }
}

/// Pick up a token rotated by `ligus phones revoke` (§13.2 step 4).
async fn token_watch(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(tunnel::PENDING_POLL) => {
                shared.reload_operator_token();
                shared.reload_token();
            }
            _ = stop.wait_for(|s| *s) => return,
        }
    }
}

/// How often [`revocation_watch`] looks at the store when nothing in this
/// process woke it (another process writing the same `ligus.db`).
pub const REVOCATION_WATCH: Duration = Duration::from_millis(500);

/// Enforce phone revocations whatever path the `DeviceRevoked` arrived by (a
/// sync round, an API write, the phone itself, another process writing the
/// same `ligus.db`): whenever the store gained config-log rows, re-check the
/// roster and cut the pairings of revoked devices. A sync round that pulled
/// events and every local write wake it at once ([`Shared::roster_kick`]), so
/// a revocation they bring in is cut as soon as it is stored, not at the next
/// poll; a write by another process is seen within [`REVOCATION_WATCH`].
async fn revocation_watch(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    let config_id = workspace_config_session_id(shared.ws);
    // (events row, equivocations row, registry generation) last looked at.
    let mut seen: Option<(i64, i64, u64)> = None;
    loop {
        let gen = shared.tunnel.as_ref().map(|t| t.registry.generation()).unwrap_or(0);
        let now = {
            let svc = shared.lock();
            let st = svc.store();
            match (st.max_row_id(), st.max_equivocation_row_id()) {
                (Ok(r), Ok(q)) => {
                    // Only the config log carries revocations: chat traffic
                    // (a busy workspace) never re-folds or rescans anything.
                    let check = match seen {
                        None => true,
                        Some((r0, q0, g0)) => g0 != gen || ((r, q) != (r0, q0) && st.session_rows_after(config_id, r0, q0).unwrap_or(true)),
                    };
                    Some(((r, q, gen), check))
                }
                _ => None,
            }
        };
        if let Some((at, check)) = now {
            seen = Some(at);
            if check {
                shared.revocation_checks.fetch_add(1, Ordering::Relaxed);
                shared.revoke_phones_of_revoked_devices();
            }
        }
        shared.revocation_passes.fetch_add(1, Ordering::SeqCst);
        tokio::select! {
            _ = tokio::time::sleep(REVOCATION_WATCH) => {}
            _ = shared.roster_kick.notified() => {}
            _ = stop.wait_for(|s| *s) => return,
        }
    }
}

/// While the relay's push stream is open, a full round this often anyway
/// catches a nudge the stream dropped (the desktop's heartbeat, too).
const SYNC_SAFETY_POLL: Duration = Duration::from_secs(20);

/// A push stream up at least this long was healthy: its drop resets the
/// reconnect backoff.
const STREAM_STABLE_AFTER: Duration = Duration::from_secs(30);

/// Relay sync: immediately after a local write, when the relay's push stream
/// says something new arrived, and every few seconds while that stream is down.
///
/// The engine and store live as long as the loop, so a round only scans what
/// was stored since the last one.
async fn sync_loop(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    let (mut engine, mut store) = match crate::sync_engine(&shared.cfg)
        .and_then(|e| Ok((e, EventStore::open(shared.cfg.data_dir.join("ligus.db"))?)))
    {
        Ok(v) => v,
        Err(e) => {
            eprintln!("sync: cannot start: {e}");
            return;
        }
    };
    let mut fails: u32 = 0;
    // The relay's push stream ("pull now" nudges), with its reconnect backoff.
    let mut stream: Option<ligus_runtime::SseStream> = None;
    let mut stream_since = tokio::time::Instant::now();
    let mut stream_attempt: u32 = 0;
    let mut next_stream_try = tokio::time::Instant::now();
    let mut stream_unsupported = false;
    while !shared.stopping() {
        // Subscribe before pulling: whatever lands from here on queues a nudge.
        if stream.is_none() && !stream_unsupported && tokio::time::Instant::now() >= next_stream_try {
            match crate::relay_client(&shared.cfg) {
                Ok(c) => match c.open_event_stream(&shared.cfg.room).await {
                    Ok(s) => {
                        stream_since = tokio::time::Instant::now();
                        stream = Some(s);
                    }
                    Err(ligus_runtime::SseConnectError::Unsupported) => stream_unsupported = true,
                    Err(e) => {
                        // A refusal (`sse_streams`) waits at least its Retry-After.
                        let delay = ligus_runtime::next_backoff(stream_attempt).max(e.retry_after().unwrap_or_default());
                        stream_attempt = stream_attempt.saturating_add(1);
                        next_stream_try = tokio::time::Instant::now() + delay;
                        eprintln!("sync: no relay push stream ({e}); polling, retry in {}s", delay.as_secs());
                    }
                },
                Err(_) => stream_unsupported = true,
            }
        }
        let wait = match crate::sync_round(&mut engine, &mut store).await {
            Ok((_, pulled)) => {
                fails = 0;
                // Writes refused by the relay (workspace full, organisation
                // quota, full relay, a client quota) or a truncated room:
                // the round worked, but the health says why writes wait.
                match engine.attention() {
                    Some(att) => shared.sync_limited(&att.message()),
                    None => shared.sync_ok(),
                }
                if pulled > 0 {
                    // A pulled DeviceRevoked is enforced now, not at the
                    // revocation watch's next poll.
                    shared.roster_kick.notify_one();
                    shared.refresh_standing();
                    shared.refresh();
                    shared.worker_kick.notify_one();
                    // A peer's contact id may have arrived: deliver what waits.
                    shared.held_kick.notify_one();
                }
                if engine.is_behind() {
                    // Mid catch-up (a large room): next round of pages now.
                    Duration::ZERO
                } else if stream.is_some() {
                    SYNC_SAFETY_POLL
                } else {
                    shared.opts.sync_every
                }
            }
            Err(_) if shared.stopping() => return,
            Err(e) => {
                fails = fails.saturating_add(1);
                shared.sync_failed(&format!("{e:#}"));
                // The relay's Retry-After (a refused read) wins over our own
                // backoff when it is longer.
                let asked = e.downcast_ref::<ligus_runtime::SyncError>().and_then(|e| e.retry_after()).unwrap_or_default();
                let wait = Duration::from_secs(crate::sync_backoff_secs(fails)).max(asked);
                eprintln!("sync: {e} — retrying in {}s", wait.as_secs());
                wait
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = shared.sync_kick.notified(), if fails == 0 => {}
            nudge = async { stream.as_mut().expect("guarded").recv().await }, if stream.is_some() => {
                if nudge.is_none() {
                    // Dropped. One that stayed up a while reconnects at once; a
                    // flapping one backs off further each time (polling meanwhile).
                    stream = None;
                    if stream_since.elapsed() >= STREAM_STABLE_AFTER {
                        stream_attempt = 0;
                        next_stream_try = tokio::time::Instant::now();
                    } else {
                        let delay = ligus_runtime::next_backoff(stream_attempt);
                        stream_attempt = stream_attempt.saturating_add(1);
                        next_stream_try = tokio::time::Instant::now() + delay;
                    }
                }
            }
            _ = stop.wait_for(|s| *s) => return,
        }
    }
}

/// Host liveness: publish this box's beat to the relay's presence channel and
/// read everyone else's, every [`HOST_BEAT_EVERY_SECS`](ligus_runtime::host_presence::HOST_BEAT_EVERY_SECS).
/// Replaces the durable heartbeat (a signed host upsert a minute, forever).
async fn presence_loop(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    use ligus_runtime::host_presence::{fetch_beats, publish_beat, HOST_BEAT_EVERY_SECS};
    let (Ok(relay), Some(pass)) = (crate::relay_client(&shared.cfg), shared.cfg.key.clone()) else {
        return;
    };
    let key = ligus_core::derive_workspace_key(&pass);
    let room = shared.cfg.room.clone();
    let mut coordinator = WorkspaceLease::default();
    let mut retry_seen = Timestamp::now().inner().unix_timestamp();
    // Whether the last pairing report named any phone (an empty one is sent
    // once after that, so My devices stops showing a pairing that ended).
    let mut paired_reported = false;
    let mut known_repos: Option<Vec<uuid::Uuid>> = None;
    loop {
        shared.presence.record(&shared.host_id, Timestamp::now().inner().unix_timestamp());
        // When the workspace's repos change (and at start): map the legacy
        // bindings and look for checkouts in the repos dir (found ones stay
        // unconfirmed until someone says "use it").
        let live: Vec<uuid::Uuid> = shared.lock().workspace_roster(shared.ws).map(|r| ligus_runtime::repos::live_repos(&r).iter().map(|x| x.id).collect()).unwrap_or_default();
        if known_repos.as_ref() != Some(&live) {
            known_repos = Some(live);
            let sh = shared.clone();
            // In the background: the beat never waits on a scan.
            drop(tokio::task::spawn_blocking(move || repos_refresh(&sh)));
        }
        let coverage = if live_empty(&known_repos) { Vec::new() } else { repo_coverage(&shared) };
        // A box that can't write (revoked, awaiting approval) isn't serving work.
        let writable = shared.lock().write_block().is_none();
        // The one keyring (B1): beats seal at the authorized head.
        let ring = match crate::key_source(&shared.cfg) {
            Ok(src) => ligus_runtime::keyring::KeyringEngine::from_store(shared.lock().store(), shared.ws, &src),
            Err(_) => ligus_runtime::keyring::KeyringEngine::passphrase(shared.ws, key),
        };
        if writable {
            let beat = shared.lock().signed_beat(shared.ws, coverage);
            if let Err(e) = publish_beat(&relay, &room, &ring, &beat).await {
                eprintln!("presence: publishing this host's beat failed: {e}");
            }
            coordinator.tick(&shared, &relay, &room, &ring).await;
        }
        if let Ok(beats) = fetch_beats(&relay, &room, &ring).await {
            let verified = shared.lock().beat_verifier(shared.ws);
            shared.presence.merge_beats(beats, verified);
        }
        if shared.tunnel.is_some() {
            revocation_presence(&shared, &relay, &room, &ring, &mut retry_seen).await;
            pairing_presence(&shared, &relay, &room, &ring, &mut paired_reported).await;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(HOST_BEAT_EVERY_SECS as u64)) => {}
            _ = stop.wait_for(|s| *s) => return,
        }
    }
}

fn live_empty(known: &Option<Vec<uuid::Uuid>>) -> bool {
    known.as_ref().is_none_or(|v| v.is_empty())
}

/// Map legacy bindings and scan the repos dir (and the box's scan roots)
/// for the workspace's repos. Device-local only: nothing is published.
pub(crate) fn repos_refresh(shared: &Shared) {
    // Git runs without the service lock (the revocation watch and the API
    // keep it); only reads and writes of the store take it.
    let Ok((roster, roots, aliases)) = ({
        let svc = shared.lock();
        svc.workspace_roster(shared.ws).map_err(|e| e.to_string()).and_then(|r| {
            let roots = svc.store().scan_roots().map_err(|e| e.to_string())?;
            let aliases = ligus_runtime::repos::aliases(svc.store(), shared.ws, Some(&r));
            Ok((r, roots, aliases))
        })
    }) else {
        return;
    };
    let repos = ligus_runtime::repos::live_repos(&roster);
    if repos.is_empty() {
        return;
    }
    let mut all_roots = roots;
    all_roots.push(crate::repos::repos_dir(&shared.cfg.data_dir));
    let found = ligus_runtime::repos::scan::scan(&all_roots, &repos, &aliases);
    let svc = shared.lock();
    if let Err(e) = ligus_runtime::repos::record_found(svc.store(), shared.ws, &found) {
        eprintln!("repos: recording found checkouts failed: {e}");
    }
    if let Err(e) = crate::repos_cmd::migrate_bindings(&shared.cfg.data_dir, &svc, shared.ws, &shared.opts.label) {
        eprintln!("repos: migrating bindings failed: {e}");
    }
}

/// This box's coverage of the workspace's repos for its beat (on by
/// default for a box; `ligus repos coverage off` turns it off).
pub(crate) fn repo_coverage(shared: &Shared) -> Vec<ligus_core::workspace_repo::CoverageEntry> {
    let (maps, aliases, repos) = {
        let svc = shared.lock();
        if !ligus_runtime::repos::coverage_reporting(svc.store(), shared.ws, true) {
            return Vec::new();
        }
        let Ok(roster) = svc.workspace_roster(shared.ws) else { return Vec::new() };
        (
            svc.store().repo_mappings(shared.ws).unwrap_or_default(),
            ligus_runtime::repos::aliases(svc.store(), shared.ws, Some(&roster)),
            ligus_runtime::repos::live_repos(&roster),
        )
    };
    ligus_runtime::repos::coverage_from(&maps, &aliases, &repos, &ligus_runtime::repos::cloning())
}

/// Publish this box's phone-revocation status for the desktops, and take the
/// retry requests they left (sealed presence slots; display and "try now"
/// only, see [`ligus_runtime::revocation_status`]).
async fn revocation_presence(
    shared: &Arc<Shared>,
    relay: &ligus_runtime::RelayClient,
    room: &str,
    ring: &ligus_runtime::keyring::KeyringEngine,
    retry_seen: &mut i64,
) {
    use ligus_runtime::revocation_status as rs;
    let Some(t) = shared.tunnel.as_ref() else { return };
    // Under this box's workspace device id (its host id), signed with its key.
    let report = t.ledger.report(&shared.host_id);
    if !report.devices.is_empty() {
        let report = shared.lock().sign_revocation_report(report);
        if let Err(e) = rs::publish_report(relay, room, ring, &report).await {
            eprintln!("presence: publishing the revocation status failed: {e}");
        }
    }
    if let Ok(blobs) = relay.list_presence(room).await {
        let now = Timestamp::now().inner().unix_timestamp();
        let ws = shared.ws;
        let retries = {
            let svc = shared.lock();
            rs::open_retries(&blobs, ring, *retry_seen, now, |d| svc.retry_requester_key(ws, d))
        };
        for r in retries {
            *retry_seen = (*retry_seen).max(r.at);
            if let Ok(d) = Uuid::parse_str(&r.device_id) {
                t.retry_revocation(d);
            }
        }
    }
}

/// Publish which phones this box has paired (sealed, signed by this box's
/// device key) so My devices on the person's desktop can say "paired with
/// <this box>, seen 2 min ago" next to a phone. Display only.
async fn pairing_presence(
    shared: &Arc<Shared>,
    relay: &ligus_runtime::RelayClient,
    room: &str,
    ring: &ligus_runtime::keyring::KeyringEngine,
    reported: &mut bool,
) {
    let Some(t) = shared.tunnel.as_ref() else { return };
    let report = t.registry.pairing_report(&shared.host_id, Timestamp::now().inner().unix_timestamp());
    if report.phones.is_empty() && !*reported {
        return;
    }
    let named = !report.phones.is_empty();
    let report = shared.lock().sign_pairing_report(report);
    match ligus_runtime::revocation_status::publish_pairing_report(relay, room, ring, &report).await {
        Ok(()) => *reported = named,
        Err(e) => eprintln!("presence: publishing the paired phones failed: {e}"),
    }
}

/// This box as the workspace's coordinator: it holds the workspace-scope
/// coordinator lease on the config log and renews it with signed heartbeats on
/// the presence channel. A second serve box on the same workspace leaves it
/// alone while the holder beats, and takes over (next epoch) only after the
/// holder's heartbeat has lapsed for two leases.
#[derive(Default)]
struct WorkspaceLease {
    epoch: Option<u64>,
    beat: u64,
}

/// Workspace leases renew on the host beat cadence, so the lease spans three.
const WORKSPACE_LEASE_MS: u64 = 3 * ligus_runtime::host_presence::HOST_BEAT_EVERY_SECS as u64 * 1000;

impl WorkspaceLease {
    async fn tick(&mut self, shared: &Arc<Shared>, relay: &ligus_runtime::RelayClient, room: &str, ring: &ligus_runtime::keyring::KeyringEngine) {
        use ligus_core::lease::{lease_health, LeaseClaimReason, LeaseHealth, LeaseScope};
        use ligus_runtime::lease_presence::{open_lease_beats, publish_lease_beat, LeaseBeat};
        let ws = shared.ws;
        let scope_id = ws.to_string();
        let config_id = ligus_core::workspace_config_session_id(ws);
        let now_ms = (Timestamp::now().inner().unix_timestamp_nanos() / 1_000_000) as i64;
        let (me, lease) = {
            let svc = shared.lock();
            (svc.device_id(), svc.lease(config_id, LeaseScope::Workspace, &scope_id).ok().flatten())
        };
        // The holder's newest verified beat, from the presence channel.
        let last = match relay.list_presence(room).await {
            Ok(blobs) => {
                let roster = {
                    let svc = shared.lock();
                    ligus_runtime::envelope_verifier::build_roster(&svc.store().trust_envelopes(ws).unwrap_or_default())
                };
                use ligus_runtime::envelope_verifier::DeviceKeyResolver;
                open_lease_beats(&blobs, ring, now_ms, |d| roster.public_key(d).map(<[u8]>::to_vec))
                    .into_iter()
                    .filter(|(b, _)| b.scope == LeaseScope::Workspace && b.scope_id == scope_id)
                    .map(|(b, _)| (b.holder, b.epoch, b.at_ms))
                    .next()
            }
            Err(_) => None,
        };
        let want = match &lease {
            None => Some((1, LeaseClaimReason::Initial)),
            Some(l) if l.current.holder == me && !l.released => {
                self.epoch = Some(l.current.epoch);
                None
            }
            Some(l) => match lease_health(l, last, now_ms) {
                LeaseHealth::Released => Some((l.current.epoch + 1, LeaseClaimReason::Released)),
                LeaseHealth::Lapsed if last.is_none_or(|(_, _, at)| now_ms - at > 2 * l.current.lease_ms as i64) => {
                    Some((l.current.epoch + 1, LeaseClaimReason::Lapsed))
                }
                _ => {
                    self.epoch = None;
                    None
                }
            },
        };
        if let Some((epoch, reason)) = want {
            let mut svc = shared.lock();
            match svc.claim_lease(config_id, ws, LeaseScope::Workspace, &scope_id, epoch, WORKSPACE_LEASE_MS, reason) {
                Ok(Some(l)) if l.current.holder == me && l.current.epoch == epoch => self.epoch = Some(epoch),
                _ => self.epoch = None,
            }
        }
        let Some(epoch) = self.epoch else { return };
        self.beat += 1;
        let beat = shared.lock().sign_lease_beat(LeaseBeat {
            scope: LeaseScope::Workspace,
            scope_id: scope_id.clone(),
            holder: me,
            epoch,
            beat: self.beat,
            lease_ms: WORKSPACE_LEASE_MS,
            at_ms: now_ms,
            signature: Vec::new(),
        });
        if let Err(e) = publish_lease_beat(relay, room, ring, &beat).await {
            eprintln!("presence: publishing the coordinator heartbeat failed: {e}");
            return;
        }
        let _ = shared.lock().record_signal(
            ws,
            None,
            &ligus_core::signal::GuardSignal::CoordinatorHeartbeat {
                scope: LeaseScope::Workspace,
                scope_id,
                holder: me,
                lease_ms: WORKSPACE_LEASE_MS,
                epoch,
                beat: self.beat,
            },
        );
    }
}

/// The serve box's chat service, for its Ligus MCP server (Track D).
impl worker::ServiceSource for Shared {
    fn service(&self) -> &Mutex<ChatService> {
        &self.svc
    }
    fn kick(&self) {
        // A hand-off queued a turn: drain now instead of on the next tick.
        self.worker_kick.notify_one();
    }
    fn ask_posted(&self, chat: Uuid, ask: &ligus_core::flow::HumanAsk) {
        self.hub.emit("ask.pending", json!({ "ask": api::ask_dto(chat, ask) }));
    }
    /// The chat's bound checkout → its `origin` on one of this box's code
    /// hosts (and `upstream` for a fork), read-only.
    fn forge_for(&self, chat: Uuid) -> Option<ligus_runtime::ligus_mcp::forge_tools::ForgeBinding> {
        let dir = crate::repos::RepoBindings::load(&self.cfg.data_dir).resolve(&crate::repos::repos_dir(&self.cfg.data_dir), chat)?;
        // The configured remote URLs, never git's insteadOf rewrites of them.
        let forges: Vec<_> = self.repos.forges(&self.cfg.data_dir).into_iter().map(|h| h.forge).collect();
        ligus_runtime::ligus_mcp::forge_tools::binding_for_checkout(&dir, &forges)
    }
}

/// The worker runs on its own thread with a current-thread runtime: a turn's
/// tool executors (MCP clients, the lazy worktree) never have to be `Send`, and
/// a long turn never starves the API.
fn spawn_worker(shared: Arc<Shared>) -> Result<std::thread::JoinHandle<()>> {
    // A generous stack: a turn's futures run on this thread's own stack
    // (Windows' default overflowed in a hosted agent's tool turn).
    Ok(std::thread::Builder::new().name("ligus-serve-worker".into()).stack_size(16 * 1024 * 1024).spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("serve: worker runtime failed: {e}");
                return;
            }
        };
        rt.block_on(async move {
            let ctx = WorkerCtx {
                cfg: &shared.cfg,
                ws: shared.ws,
                my_host: &shared.host_id,
                host_label: &shared.opts.label,
                settings: ToolSettings::from_env(),
                ligus: Some(worker::ligus_server(shared.clone())),
                presence: Some(&shared.presence),
                stops: Some(shared.turn_stops.clone()),
            };
            let sink = ServeSink(shared.clone());
            let mut drain = worker::DrainState::default();
            while !shared.stopping() {
                if !worker::apply_tick(&ctx, &shared.svc).is_empty() {
                    shared.after_write();
                }
                if let Err(e) = worker::drain_tick(&ctx, &shared.svc, &mut drain, &sink).await {
                    eprintln!("worker: tick failed (retrying): {e}");
                }
                tokio::select! {
                    _ = shared.worker_kick.notified() => {}
                    _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                }
            }
        });
    })?)
}

/// Print the pairing QR code and link. With remote access enabled the link is
/// v2 (§4): it also opens a one-time enrollment window (10 min) that a
/// running `ligus serve` announces to the relay. `direct` = `None` prints a
/// relay-only link (no `url`/`token`).
pub fn print_pairing(data_dir: &std::path::Path, direct: Option<(&str, &str)>, remote: Option<&tunnel::RelaySettings>) -> Result<()> {
    let link = match remote {
        Some(r) => {
            r.validate()?;
            let dir = tunnel::tunnel_dir(data_dir);
            let Some(keys) = tunnel::HostKeys::load(&dir)? else {
                bail!(
                    "remote access is enabled but there are no host keys in {}: run `ligus remote enable` or start `ligus serve` first",
                    dir.display()
                );
            };
            let (secret, exp) = tunnel::create_window(&dir, tunnel::PAIR_WINDOW_SECS)?;
            tunnel::pairing_link_v2(direct, &keys, &r.relay, &secret.ps, exp)
        }
        None => {
            let Some((url, token)) = direct else { bail!("nothing to pair: no direct URL and remote access is off") };
            pair::pairing_url(url, token)
        }
    };
    println!("\nScan with the iPhone camera (or the Ligus app) to pair:\n");
    print!("{}", pair::render_qr(&link)?);
    println!("\n{link}\n");
    if direct.is_some() {
        println!("This link carries the API token: share it only with your own phone.");
    }
    if remote.is_some() {
        println!("The remote pairing secret in it works once, for 10 minutes, while `ligus serve` is running.");
    }
    Ok(())
}

/// `ligus serve` — run until SIGTERM / Ctrl-C.
pub fn cmd_serve(cfg: Config, opts: ServeOptions) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        let server_url = opts.server_url();
        let pair_now = opts.pair;
        let handle = start(cfg, opts).await?;
        println!(
            "ligus serve: listening on http://{} — workspace '{}', host {} ('{}'). Ctrl-C to stop.",
            handle.addr, handle.shared.cfg.room, handle.shared.host_id, handle.shared.opts.label
        );
        if let Some(t) = &handle.shared.tunnel {
            println!("ligus serve: remote access through {} (host id {}).", t.settings.relay, t.host_id());
        }
        if pair_now {
            let token = handle.shared.token();
            let remote = handle.shared.opts.remote.as_ref();
            match pair::pair_plan(&server_url, remote.is_some(), false) {
                Ok(plan) => {
                    if plan == pair::PairPlan::RelayOnly {
                        println!("(The direct URL {server_url} is loopback; printing a relay-only link.)");
                    }
                    let direct = (plan == pair::PairPlan::Direct).then_some((server_url.as_str(), token.as_str()));
                    print_pairing(&handle.shared.cfg.data_dir, direct, remote)?;
                    if let Some(hint) = pair::member_hint(&handle.shared.standing()) {
                        println!("{hint}");
                    }
                }
                // Keep serving: only the link is withheld.
                Err(why) => eprintln!("ligus serve: {why}"),
            }
        } else {
            println!("Run `ligus pair` to show the phone pairing code.");
        }
        crate::shutdown_signal().await;
        println!("ligus serve: stopping.");
        // Under the unit's TimeoutStopSec (20 s): let a turn in flight finish
        // if it can; a longer one is stopped, and one that won't stop is
        // finished as interrupted on the next start.
        if !handle.stop(STOP_GRACE).await {
            eprintln!("ligus serve: a turn was still running; it will be marked interrupted on the next start.");
        }
        Ok(())
    })
}

/// `ligus pair` — show the pairing code for a configured (possibly already
/// running) `ligus serve`, without starting one.
pub fn cmd_pair(cfg: &Config, opts: &ServeOptions, relay_only: bool) -> Result<()> {
    let data_dir = cfg.data_dir.as_path();
    let url = opts.server_url();
    let remote = opts.remote.as_ref();
    match pair::pair_plan(&url, remote.is_some(), relay_only).map_err(anyhow::Error::msg)? {
        pair::PairPlan::RelayOnly => {
            if !relay_only {
                println!("(The direct URL {url} is loopback; printing a relay-only link.)");
            }
            print_pairing(data_dir, None, remote)?;
        }
        pair::PairPlan::Direct => {
            let token = pair::load_existing_token(&opts.token_file)?;
            print_pairing(data_dir, Some((&url, &token)), remote)?;
        }
    }
    // Best effort: a box whose store can't be read yet just gets no hint.
    let standing = open_service(cfg).ok().and_then(|svc| svc.device_standing(uuid_of_room(&cfg.room), None).ok());
    if let Some(hint) = standing.as_ref().and_then(pair::member_hint) {
        println!("{hint}");
    }
    Ok(())
}
