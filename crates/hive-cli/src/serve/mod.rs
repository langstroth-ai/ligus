//! `ligus serve` — the home host for the iOS app.
//!
//! One process on an always-on box (the lab's SFF) that is a normal Hive
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
use hive_core::{workspace_config_session_id, ChatSession, HostKind, MessageRole, Timestamp, WorkspaceHost};
use hive_runtime::device_enroll::Standing;
use hive_runtime::{ChatService, EventStore};
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
pub mod dashboard_hive;
pub mod dms;
pub mod flow_api;
pub mod forges_api;
pub mod hub;
pub mod identity_api;
pub mod invite_code;
pub use invite_code::admission_code;
pub mod lab;
pub mod notify;
pub mod ntfy;
pub mod pair;
pub mod people;
pub mod presence_api;
pub mod proposal_file;
pub mod repos_api;
pub mod sets;
pub mod tunnel;
pub mod typing_api;

#[cfg(test)]
mod identity_tests;
#[cfg(test)]
mod tests;

/// How long `ligus serve` waits for a turn in flight on SIGTERM / Ctrl-C.
const STOP_GRACE: Duration = Duration::from_secs(15);

/// Replies and proposals created this long before serve started don't push.
const NOTIFY_HISTORY_MINUTES: i64 = 10;

/// Default listen address: loopback only. On the home host the systemd unit
/// passes the box's Tailscale IP.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8787";

/// Everything `ligus serve` is configured with besides the workspace [`Config`].
#[derive(Debug, Clone)]
pub struct ServeOptions {
    pub listen: SocketAddr,
    pub token_file: PathBuf,
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
    /// Apply an approved file-diff proposal to this box's bound checkout.
    pub apply_on_approve: bool,
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
        let ntfy = match (ntfy_url.or_else(|| env_opt("LIGUS_NTFY_URL")), ntfy_topic.or_else(|| env_opt("LIGUS_NTFY_TOPIC"))) {
            (Some(url), Some(topic)) => Some(ntfy::Ntfy { url, topic, token: env_opt("LIGUS_NTFY_TOKEN") }),
            (Some(_), None) | (None, Some(_)) => bail!("ntfy needs both a URL and a topic (--ntfy-url / --ntfy-topic)"),
            (None, None) => None,
        };
        Ok(Self {
            listen,
            token_file: token_file
                .or_else(|| env_opt("LIGUS_SERVE_TOKEN_FILE"))
                .map(PathBuf::from)
                .unwrap_or_else(|| cfg.data_dir.join("serve.token")),
            ntfy,
            public_url: public_url.or_else(|| env_opt("LIGUS_SERVE_PUBLIC_URL")),
            label: label
                .or_else(|| env_opt("LIGUS_SERVE_LABEL"))
                .or_else(hive_runtime::machine_hostname)
                .unwrap_or_else(|| "home host".into()),
            name: name.or_else(|| env_opt("LIGUS_SERVE_NAME")),
            pair,
            prometheus_url: env_opt("LIGUS_PROMETHEUS_URL").unwrap_or_else(|| "http://127.0.0.1:9090".into()),
            prometheus_configured: env_opt("LIGUS_PROMETHEUS_URL").is_some(),
            dashboard_file: dashboard::config_path(&cfg.data_dir),
            default_agent: env_opt("LIGUS_SERVE_DEFAULT_AGENT").map(|a| a.trim().trim_start_matches('@').to_string()),
            apply_on_approve: env_flag("LIGUS_SERVE_APPLY_ON_APPROVE", false),
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
    /// [`hive_runtime::host_presence`]), refreshed by `presence_loop`.
    pub presence: hive_runtime::HostPresence,
    /// The last successful relay sync round and the latest failure since.
    sync_status: Mutex<(Option<time::OffsetDateTime>, Option<String>)>,
    /// (chat, agent) turns this box's worker has queued or is running.
    busy: Mutex<HashSet<(Uuid, Uuid)>>,
    /// The dashboard's workspace snapshot, reused for a few seconds.
    hive_snapshot: Mutex<Option<(std::time::Instant, Arc<dashboard_hive::HiveSnapshot>)>>,
    /// MCP servers for `source = "mcp"` dashboard panels (`LIGUS_MCP_CONFIG`,
    /// read at start), or why there are none.
    pub dashboard_mcp: std::result::Result<hive_runtime::McpRegistry, String>,
    /// Repos from the phone: the GitHub backend, clone jobs, checkout locks.
    pub repos: repos_api::RepoState,
    /// Events waiting for a phone's passkey (B3 `presence/prepare`).
    pub presence_drafts: presence_api::Pending,
    /// Box admissions waiting for a phone's passkey and signature (Phase B).
    pub admission_drafts: admissions_api::Drafts,
    /// The box's direct messages (`dms.rs`).
    pub dms: dms::DmStore,
    /// Wakes the DM sync loop after a local write.
    pub dm_kick: Notify,
    /// One contact publish or drop-box read at a time.
    pub contact_lock: tokio::sync::Mutex<()>,
    /// DM invites waiting for the peer's contact id (`hive_runtime::dm_delivery`).
    pub held: hive_runtime::dm_delivery::HeldInvites,
    /// Wakes the held-invite loop (after a pull, or a new held invite).
    pub held_kick: Notify,
    /// The last contacts status (`contacts_api::status`).
    pub contact_state: Mutex<String>,
    /// Drop-box invites from the last listing (codes stay on the box).
    pub pending_codes: identity_api::PendingCodes,
    /// The phone's typing and who else is typing (`typing_api`).
    pub typing: typing_api::Typing,
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
pub fn hive_snapshot(
    svc: &ChatService,
    ws: Uuid,
    this_host_id: &str,
    this_host_label: &str,
    presence: &hive_runtime::HostPresence,
    busy_here: &HashSet<Uuid>,
    host: dashboard_hive::HostState,
) -> Result<dashboard_hive::HiveSnapshot> {
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
    Ok(dashboard_hive::HiveSnapshot::compute(&dashboard_hive::HiveInputs {
        now: now.inner(),
        offset: dashboard_hive::local_offset(),
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

impl dashboard::HiveProvider for Shared {
    fn snapshot(&self) -> Result<Arc<dashboard_hive::HiveSnapshot>> {
        let mut cached = self.hive_snapshot.lock().unwrap_or_else(|e| e.into_inner());
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
            dashboard_hive::RemoteState {
                connected: live.connected,
                push: t.push_available.load(Ordering::SeqCst),
                phones: phones.len(),
                phones_with_push: phones.iter().filter(|p| p.push_enabled).count(),
            }
        });
        let host = dashboard_hive::HostState {
            serving: true,
            relay_configured: self.cfg.relay_url.is_some(),
            last_sync_ok: last_ok,
            sync_error: err,
            remote,
            ntfy: self.opts.ntfy.is_some(),
        };
        let snap = {
            let svc = self.lock();
            Arc::new(hive_snapshot(&svc, self.ws, &self.host_id, &self.opts.label, &self.presence, &busy, host)?)
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
            hive: Some(self),
            mcp: self.dashboard_mcp.as_ref().map_err(String::as_str),
        }
    }

    fn sync_ok(&self) {
        *self.sync_status.lock().unwrap_or_else(|e| e.into_inner()) = (Some(time::OffsetDateTime::now_utc()), None);
    }

    fn sync_failed(&self, e: &str) {
        self.sync_status.lock().unwrap_or_else(|e| e.into_inner()).1 = Some(e.to_string());
    }
}

/// The MCP servers dashboard panels may use (see [`Shared::dashboard_mcp`]).
pub fn dashboard_mcp_registry() -> std::result::Result<hive_runtime::McpRegistry, String> {
    match crate::mcp_config::load_from_env() {
        Ok(specs) if specs.is_empty() => Err("no MCP servers are configured on this host (LIGUS_MCP_CONFIG)".into()),
        Ok(specs) => Ok(hive_runtime::McpRegistry::new(specs)),
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

    /// Re-read the token file; a rotated token takes effect at once.
    fn reload_token(&self) {
        let Ok(t) = std::fs::read_to_string(&self.opts.token_file) else { return };
        let t = t.trim();
        if t.len() >= 32 && *self.token.read().unwrap_or_else(|e| e.into_inner()) != t {
            *self.token.write().unwrap_or_else(|e| e.into_inner()) = t.to_string();
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
            (svc.author().clone(), hive_runtime::device_enroll::fingerprint(&svc.device_public_key()))
        };
        let mode = match self.standing() {
            // Never reached: a box enrolled as a person's device refuses to
            // start, and is paused if one appears while it runs.
            Standing::OwnerDevice(_) => "owner",
            Standing::Revoked => "revoked",
            Standing::Own => "member",
        };
        json!({ "mode": mode, "actorId": actor.id, "displayName": hive_proto::names::clean_display_name(&actor.display_name), "deviceFingerprint": fingerprint })
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

    /// Phone revocation is three-part (Phase A, item 6): a workspace
    /// `DeviceRevoked` of a phone's device (its own key never signs again;
    /// the fold refuses it) also revokes that phone's tunnel pairing here
    /// (the relay drops its sessions through the existing tunnel revocation,
    /// and pushes to it stop). Its langstroth-auth session is revoked by the same
    /// signed `DeviceRevoked` when organization sign-in lands (Phase D).
    /// Returns the pairings revoked.
    pub fn revoke_phones_of_revoked_devices(&self) -> Vec<String> {
        let Some(tunnel) = self.tunnel.as_ref() else { return Vec::new() };
        let revoked: std::collections::BTreeSet<Uuid> = {
            let svc = self.lock();
            match svc.store().load_config(self.ws, None, None) {
                Ok((_, t)) => t.current().devices.iter().filter(|(_, d)| d.revoked).map(|(id, _)| *id).collect(),
                Err(_) => return Vec::new(),
            }
        };
        let pids = tunnel.registry.pairings_of_revoked(&revoked);
        for pid in &pids {
            if let Err(e) = tunnel.revoke(pid) {
                eprintln!("serve: revoking a revoked phone's pairing failed: {e}");
            }
        }
        pids
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
            .filter(|m| m.role != MessageRole::System)
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
                        let Some(m) = s.messages.iter().find(|m| m.id == message) else { continue };
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
                        let Some(m) = s.messages.iter().find(|m| m.id == message) else { continue };
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
                        if !recruit_pushed && self.fresh(hive_core::Timestamp::from_unix_seconds(r.at_ms / 1000)) {
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
                        if self.fresh(hive_core::Timestamp::from_unix_seconds(a.ask.at_ms / 1000)) {
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
    /// checks `stopping` between turns). Returns whether the worker stopped in
    /// time; if not, the next start finishes the cut-off reply
    /// ([`worker::recover_interrupted_turns`]).
    pub async fn stop(&self, grace: Duration) -> bool {
        self.shutdown();
        let Some(worker) = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            return true;
        };
        let deadline = tokio::time::Instant::now() + grace;
        while !worker.is_finished() {
            if tokio::time::Instant::now() >= deadline {
                return false;
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
    hive_runtime::executor::configure_host(&cfg.data_dir, std::slice::from_ref(&opts.token_file), &[crate::repos::repos_dir(&cfg.data_dir)]);
    let token = pair::load_or_create_token(&opts.token_file)?;
    if let Err(e) = crate::purge_foreign_workspaces(&cfg) {
        eprintln!("serve: foreign-workspace cleanup skipped: {e}");
    }
    // Phase B: sync first, never found a workspace, join only as this box's
    // own service member through an admission (`crate::admit::prepare_box`).
    let mut cfg = cfg;
    let admission = match crate::env_opt(crate::admit::ADMISSION_FILE_ENV) {
        Some(path) => Some(crate::admit::read_code_file(std::path::Path::new(&path))?),
        None => None,
    };
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
                let identity = hive_runtime::IdentityStore::new(&cfg.data_dir, hive_runtime::secret_store::StoreKeyVault::for_data_dir(&cfg.data_dir));
                if let Err(e) = crate::adopt_box_name(&identity, &opts.label, hive_runtime::machine_hostname().as_deref()) {
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
                // A box still called "hive-cli" (enrolled under Hive 1.x), or by
                // the host name `ligus enroll` gave it, takes its label so the
                // phone's messages show the name the box was given.
                let identity = hive_runtime::IdentityStore::new(&cfg.data_dir, hive_runtime::secret_store::StoreKeyVault::for_data_dir(&cfg.data_dir));
                let was = svc.author().display_name.clone();
                match crate::adopt_box_name(&identity, &opts.label, hive_runtime::machine_hostname().as_deref()) {
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
        host_id,
        tunnel,
        svc: Mutex::new(svc),
        hub: hub::Hub::new(),
        differ: Mutex::new(hub::Differ::default()),
        local_streams: Mutex::new(HashSet::new()),
        read_marks: Mutex::new(load_read_marks(&cfg)),
        sync_kick: Notify::new(),
        worker_kick: Notify::new(),
        stopping: AtomicBool::new(false),
        #[cfg(test)]
        test_presence: AtomicBool::new(false),
        started_at: Timestamp::now(),
        http: hive_forge::http_client_builder().connect_timeout(Duration::from_secs(3)).build()?,
        dashboard_cache: dashboard::Cache::default(),
        rt: tokio::runtime::Handle::current(),
        revisions: Mutex::new(HashMap::new()),
        standing: Mutex::new(standing),
        presence: hive_runtime::HostPresence::new(),
        sync_status: Mutex::new((first_sync, None)),
        busy: Mutex::new(HashSet::new()),
        presence_drafts: presence_api::Pending::default(),
        admission_drafts: admissions_api::Drafts::default(),
        dms: dms::DmStore::load(&cfg.data_dir),
        dm_kick: Notify::new(),
        contact_lock: tokio::sync::Mutex::new(()),
        held: hive_runtime::dm_delivery::HeldInvites::for_data_dir(&cfg.data_dir),
        held_kick: Notify::new(),
        contact_state: Mutex::new("unknown".into()),
        pending_codes: identity_api::PendingCodes::default(),
        typing: typing_api::Typing::default(),
        hive_snapshot: Mutex::new(None),
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
    let mut rx_api = rx.clone();
    let mut tasks = vec![tokio::spawn(async move {
        let graceful = async move {
            let _ = rx_api.wait_for(|stop| *stop).await;
        };
        if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(graceful).await {
            eprintln!("serve: API stopped: {e}");
        }
    })];
    if shared.cfg.relay_url.is_some() {
        tasks.push(tokio::spawn(sync_loop(shared.clone(), rx.clone())));
        tasks.push(tokio::spawn(presence_loop(shared.clone(), rx.clone())));
        tasks.push(tokio::spawn(contact_loop(shared.clone(), rx.clone())));
    }
    tasks.push(tokio::spawn(dms::dm_sync_loop(shared.clone(), rx.clone())));
    tasks.push(tokio::spawn(contacts_api::held_loop(shared.clone(), rx.clone())));
    tasks.push(tokio::spawn(typing_api::typing_loop(shared.clone(), rx.clone())));
    tasks.push(tokio::spawn(token_watch(shared.clone(), rx.clone())));
    if let Some(t) = shared.tunnel.clone() {
        tasks.push(tokio::spawn(tunnel::pending_loop(t.clone(), rx.clone())));
        tasks.push(tokio::spawn(tunnel::control::run(t, api::router(shared.clone()), rx.clone(), Default::default())));
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
            _ = tokio::time::sleep(tunnel::PENDING_POLL) => shared.reload_token(),
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
        .and_then(|e| Ok((e, EventStore::open(shared.cfg.data_dir.join("hive.db"))?)))
    {
        Ok(v) => v,
        Err(e) => {
            eprintln!("sync: cannot start: {e}");
            return;
        }
    };
    let mut fails: u32 = 0;
    // The relay's push stream ("pull now" nudges), with its reconnect backoff.
    let mut stream: Option<hive_runtime::SseStream> = None;
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
                    Err(hive_runtime::SseConnectError::Unsupported) => stream_unsupported = true,
                    Err(e) => {
                        let delay = hive_runtime::next_backoff(stream_attempt);
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
                shared.sync_ok();
                if pulled > 0 {
                    shared.refresh_standing();
                    shared.revoke_phones_of_revoked_devices();
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
                let secs = crate::sync_backoff_secs(fails);
                eprintln!("sync: {e} — retrying in {secs}s");
                Duration::from_secs(secs)
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
                        let delay = hive_runtime::next_backoff(stream_attempt);
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
/// read everyone else's, every [`HOST_BEAT_EVERY_SECS`](hive_runtime::host_presence::HOST_BEAT_EVERY_SECS).
/// Replaces the durable heartbeat (a signed host upsert a minute, forever).
async fn presence_loop(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    use hive_runtime::host_presence::{fetch_host_beats, publish_host_beat, HOST_BEAT_EVERY_SECS};
    let (Ok(relay), Some(pass)) = (crate::relay_client(&shared.cfg), shared.cfg.key.clone()) else {
        return;
    };
    let key = hive_core::derive_workspace_key(&pass);
    let room = shared.cfg.room.clone();
    let mut coordinator = WorkspaceLease::default();
    loop {
        shared.presence.record(&shared.host_id, Timestamp::now().inner().unix_timestamp());
        // A box that can't write (revoked, awaiting approval) isn't serving work.
        let writable = shared.lock().write_block().is_none();
        // The one keyring (B1): beats seal at the authorized head.
        let ring = match crate::key_source(&shared.cfg) {
            Ok(src) => hive_runtime::keyring::KeyringEngine::from_store(shared.lock().store(), shared.ws, &src),
            Err(_) => hive_runtime::keyring::KeyringEngine::passphrase(shared.ws, key),
        };
        if writable {
            if let Err(e) = publish_host_beat(&relay, &room, &ring, &shared.host_id).await {
                eprintln!("presence: publishing this host's beat failed: {e}");
            }
            coordinator.tick(&shared, &relay, &room, &ring).await;
        }
        if let Ok(beats) = fetch_host_beats(&relay, &room, &ring).await {
            shared.presence.merge(beats);
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(HOST_BEAT_EVERY_SECS as u64)) => {}
            _ = stop.wait_for(|s| *s) => return,
        }
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
const WORKSPACE_LEASE_MS: u64 = 3 * hive_runtime::host_presence::HOST_BEAT_EVERY_SECS as u64 * 1000;

impl WorkspaceLease {
    async fn tick(&mut self, shared: &Arc<Shared>, relay: &hive_runtime::RelayClient, room: &str, ring: &hive_runtime::keyring::KeyringEngine) {
        use hive_core::lease::{lease_health, LeaseClaimReason, LeaseHealth, LeaseScope};
        use hive_runtime::lease_presence::{open_lease_beats, publish_lease_beat, LeaseBeat};
        let ws = shared.ws;
        let scope_id = ws.to_string();
        let config_id = hive_core::workspace_config_session_id(ws);
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
                    hive_runtime::envelope_verifier::build_roster(&svc.store().trust_envelopes(ws).unwrap_or_default())
                };
                use hive_runtime::envelope_verifier::DeviceKeyResolver;
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
            &hive_core::signal::GuardSignal::CoordinatorHeartbeat {
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
    fn ask_posted(&self, chat: Uuid, ask: &hive_core::flow::HumanAsk) {
        self.hub.emit("ask.pending", json!({ "ask": api::ask_dto(chat, ask) }));
    }
    /// The chat's bound checkout → its `origin` on one of this box's code
    /// hosts (and `upstream` for a fork), read-only.
    fn forge_for(&self, chat: Uuid) -> Option<hive_runtime::ligus_mcp::forge_tools::ForgeBinding> {
        let dir = crate::repos::RepoBindings::load(&self.cfg.data_dir).resolve(&crate::repos::repos_dir(&self.cfg.data_dir), chat)?;
        // The configured remote URLs, never git's insteadOf rewrites of them.
        let forges: Vec<_> = self.repos.forges(&self.cfg.data_dir).into_iter().map(|h| h.forge).collect();
        hive_runtime::ligus_mcp::forge_tools::binding_for_checkout(&dir, &forges)
    }
}

/// The worker runs on its own thread with a current-thread runtime: a turn's
/// tool executors (MCP clients, the lazy worktree) never have to be `Send`, and
/// a long turn never starves the API.
fn spawn_worker(shared: Arc<Shared>) -> Result<std::thread::JoinHandle<()>> {
    Ok(std::thread::Builder::new().name("hive-serve-worker".into()).spawn(move || {
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
            };
            let sink = ServeSink(shared.clone());
            let mut failed: HashSet<Uuid> = HashSet::new();
            while !shared.stopping() {
                if let Err(e) = worker::drain_tick(&ctx, &shared.svc, &mut failed, &sink).await {
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
        // if it can; a longer one is finished as interrupted on the next start.
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
