//! Direct messages on the box (IDENTITY-AND-FORGES §4).
//!
//! `ligus serve` is a device in one workspace; a DM is a workspace of its
//! own (a random `dm-…` room, never derived from anything public). The box
//! keeps the DMs it founded or joined here, device-local:
//! `serve-dms.json` (owner-only) holds the room, relay, the other person's
//! account and the founder pin; each DM's passphrase and delivered epoch
//! keys live in the secret store (scope `cli`), like the workspace's own.
//!
//! - Opening a DM with a co-member founds it here (only on a box that holds
//!   its account's key; a box acting for its owner can't found one) and
//!   hands back a signed invite, also dropped into their contact drop box
//!   when they published one (`contacts_api`).
//! - Joining one takes a signed DM invite addressed to this box's account
//!   from a co-member whose key matches (or a stranger whose safety code the
//!   person compared): the rule is `hive_runtime::dm::accept_dm_invite`.
//! - Each DM syncs in its own loop with its own engine, on its own relay;
//!   the workspace token and relay sign-in are used only on the box's own
//!   relay, never on a relay an invite named.
//! - Agents never run in a DM: the worker serves the box's workspace only.
//!   DM messages don't push.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use hive_core::{workspace_config_session_id, ChatSession};
use hive_runtime::keyring::DeliveredKey;
use hive_runtime::secret_store::SecretId;
use hive_runtime::{EventStore, RelayClient, SyncEngine};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use uuid::Uuid;

use super::Shared;
use crate::Config;

/// One DM this box holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DmConn {
    /// The other person's name when the DM was made (shown until their
    /// roster entry syncs).
    #[serde(default)]
    pub name: String,
    pub relay_url: String,
    pub room: String,
    /// The other person's account id.
    pub peer: String,
    /// Who founded the DM (this box's account, or the other person's).
    pub founder_actor_id: String,
    pub fk: String,
}

impl DmConn {
    pub fn workspace_id(&self) -> Uuid {
        hive_core::room_workspace_id(&self.room)
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct DmFile {
    #[serde(default)]
    v: u32,
    #[serde(default)]
    dms: Vec<DmConn>,
    /// Rooms the simultaneous-open rule archived (read-only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    archived: Vec<String>,
}

/// The DMs on this box.
pub struct DmStore {
    data_dir: PathBuf,
    file: Mutex<DmFile>,
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("serve-dms.json")
}

fn key_id(room: &str) -> SecretId {
    SecretId::new("cli", format!("dmkey-{room}"))
}

fn ek_id(room: &str) -> SecretId {
    SecretId::new("cli", format!("dmek-{room}"))
}

/// The most DMs one box keeps (each one syncs in the background).
pub const MAX_DMS: usize = 200;

impl DmStore {
    pub fn load(data_dir: &Path) -> Self {
        let file = std::fs::read_to_string(path(data_dir))
            .ok()
            .and_then(|t| serde_json::from_str::<DmFile>(&t).ok())
            .unwrap_or_default();
        Self { data_dir: data_dir.to_path_buf(), file: Mutex::new(file) }
    }

    /// The DM rooms' workspaces as `serve-dms.json` lists them, or an error
    /// when the file exists but can't be read: a caller deciding what to
    /// delete (the foreign-workspace cleanup) must not take an unreadable
    /// list as "no DMs".
    pub fn workspace_ids_checked(data_dir: &Path) -> Result<Vec<Uuid>> {
        let f = match std::fs::read_to_string(path(data_dir)) {
            Ok(t) => serde_json::from_str::<DmFile>(&t)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => DmFile::default(),
            Err(e) => return Err(e.into()),
        };
        Ok(f.dms.iter().map(DmConn::workspace_id).collect())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, DmFile> {
        self.file.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self, f: &DmFile) -> Result<()> {
        super::tunnel::write_private(&path(&self.data_dir), serde_json::to_string_pretty(f)?.as_bytes())
    }

    pub fn list(&self) -> Vec<DmConn> {
        self.lock().dms.clone()
    }

    pub fn get(&self, workspace_id: Uuid) -> Option<DmConn> {
        self.lock().dms.iter().find(|d| d.workspace_id() == workspace_id).cloned()
    }

    pub fn holds_room(&self, room: &str) -> bool {
        self.lock().dms.iter().any(|d| d.room == room)
    }

    pub fn workspace_ids(&self) -> Vec<Uuid> {
        self.lock().dms.iter().map(DmConn::workspace_id).collect()
    }

    pub fn archived(&self) -> Vec<String> {
        self.lock().archived.clone()
    }

    pub fn is_archived(&self, workspace_id: Uuid) -> bool {
        let f = self.lock();
        f.dms.iter().any(|d| d.workspace_id() == workspace_id && f.archived.contains(&d.room))
    }

    /// Add a DM and keep its secrets. Refuses a room this box already holds.
    pub fn add(&self, conn: DmConn, key: &str, ek: &[DeliveredKey]) -> Result<()> {
        let store = hive_runtime::secret_store::for_data_dir(&self.data_dir);
        let mut f = self.lock();
        if f.dms.iter().any(|d| d.room == conn.room) {
            anyhow::bail!("this box already holds that conversation");
        }
        if f.dms.len() >= MAX_DMS {
            anyhow::bail!("this box already keeps {MAX_DMS} direct messages");
        }
        store.put(&key_id(&conn.room), key.as_bytes())?;
        hive_runtime::secret_redact::register(key);
        if !ek.is_empty() {
            store.put(&ek_id(&conn.room), serde_json::to_string(ek)?.as_bytes())?;
        }
        f.dms.push(conn);
        f.v = 1;
        let snapshot = f.clone();
        self.save(&snapshot)
    }

    /// Apply the simultaneous-open rule's archive set (only rooms this box
    /// holds). Returns whether it changed.
    pub fn set_archived(&self, archived: Vec<String>) -> bool {
        let mut f = self.lock();
        let mut next: Vec<String> = archived.into_iter().filter(|r| f.dms.iter().any(|d| &d.room == r)).collect();
        next.sort();
        next.dedup();
        let mut cur = f.archived.clone();
        cur.sort();
        if cur == next {
            return false;
        }
        f.archived = next;
        let snapshot = f.clone();
        if let Err(e) = self.save(&snapshot) {
            eprintln!("serve: saving the direct messages failed: {e}");
        }
        true
    }

    /// The sync config for one DM: its relay and room, its passphrase and
    /// epoch keys. The workspace's relay token goes only to the box's own
    /// relay.
    pub fn config_for(&self, base: &Config, conn: &DmConn) -> Config {
        let store = hive_runtime::secret_store::for_data_dir(&self.data_dir);
        let get = |id: &SecretId| store.get_string(id).ok().flatten().map(|z| z.to_string()).filter(|v| !v.is_empty());
        let key = get(&key_id(&conn.room));
        if let Some(k) = &key {
            hive_runtime::secret_redact::register(k);
        }
        let ek: Vec<DeliveredKey> = get(&ek_id(&conn.room)).and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default();
        let own_relay = base.relay_url.as_deref().is_some_and(|r| same_relay(r, &conn.relay_url));
        Config {
            data_dir: base.data_dir.clone(),
            relay_url: Some(conn.relay_url.clone()),
            token: if own_relay { base.token.clone() } else { None },
            room: conn.room.clone(),
            key,
            epoch_keys: ek,
        }
    }
}

/// Two relay base URLs name the same relay (case and a trailing slash
/// ignored).
pub fn same_relay(a: &str, b: &str) -> bool {
    a.trim().trim_end_matches('/').eq_ignore_ascii_case(b.trim().trim_end_matches('/'))
}

/// A client for `relay` with the box's sign-in only when it is the box's own
/// relay: a relay a DM invite named never sees the workspace token, and is
/// reached only at public addresses, checked when each connection is made
/// (so a name re-pointed inside the box's network after the invite was
/// checked is refused; [`dm_relay_allowed`] refuses internal literals).
pub fn relay_client_for(base: &Config, relay: &str) -> RelayClient {
    let own = base.relay_url.as_deref().is_some_and(|r| same_relay(r, relay));
    if own {
        RelayClient::new(relay).with_auth(base.token.clone()).with_auth_cell(crate::auth::cell())
    } else {
        RelayClient::public_only(relay)
    }
}

/// A DM relay URL the box will sync with: http(s), no credentials.
pub fn valid_relay_url(raw: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(raw.trim()) else { return false };
    matches!(u.scheme(), "http" | "https") && u.host_str().is_some() && u.username().is_empty() && u.password().is_none()
}

/// Whether the box will sync a DM through `relay`, which the invite's sender
/// chose: its own relay (whatever its address), or a public `https` relay.
/// A relay on a loopback, private, link-local or local-name address would
/// turn an invite into a way to make the box send requests inside its
/// network (and a plain-http one to expose the DM's traffic).
pub fn dm_relay_allowed(own_relay: Option<&str>, relay: &str) -> bool {
    if !valid_relay_url(relay) {
        return false;
    }
    if own_relay.is_some_and(|o| same_relay(o, relay)) {
        return true;
    }
    let Ok(u) = reqwest::Url::parse(relay.trim()) else { return false };
    if u.scheme() != "https" {
        return false;
    }
    let Some(host) = u.host_str() else { return false };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match host.parse::<std::net::IpAddr>() {
        Ok(ip) => hive_forge::http::is_public_ip(ip),
        Err(_) => {
            let d = host.trim_end_matches('.').to_ascii_lowercase();
            d.contains('.') && d != "localhost" && ![".local", ".internal", ".lan", ".home.arpa", ".localhost"].iter().any(|s| d.ends_with(s))
        }
    }
}

impl Shared {
    /// The live chats of the box's DMs (each DM's own chats), archived DMs
    /// included (read-only).
    pub fn dm_chats(&self) -> Vec<ChatSession> {
        let ids = self.dms.workspace_ids();
        if ids.is_empty() {
            return Vec::new();
        }
        let svc = self.lock();
        let mut out = Vec::new();
        for ws in ids {
            if let Ok(list) = super::chats_of(&svc, ws) {
                out.extend(list);
            }
        }
        out
    }

    /// Every chat the phone lists: the workspace's and the DMs'.
    pub fn phone_chats(&self) -> Result<Vec<ChatSession>> {
        let mut all = self.workspace_chats()?;
        all.extend(self.dm_chats());
        Ok(all)
    }

    /// Owner decision D12 over the box's DMs: for every peer with more than
    /// one founded DM, keep the one with the smaller `(founded_at, room)`
    /// and archive the rest. Recomputed every time.
    pub fn resolve_concurrent_dms(&self) {
        let dms = self.dms.list();
        let mut by_peer: BTreeMap<String, Vec<DmConn>> = BTreeMap::new();
        for d in dms {
            by_peer.entry(d.peer.clone()).or_default().push(d);
        }
        let mut archived = self.dms.archived();
        let before = archived.clone();
        {
            let svc = self.lock();
            for list in by_peer.values().filter(|v| v.len() > 1) {
                let candidates: Vec<hive_runtime::dm::DmCandidate> = list
                    .iter()
                    .filter(|d| svc.founder_verified(d.workspace_id()).unwrap_or(false))
                    .map(|d| hive_runtime::dm::DmCandidate {
                        room: d.room.clone(),
                        created_at: svc.store().config_founded_at(d.workspace_id()).ok().flatten(),
                    })
                    .filter(|c| c.created_at.is_some())
                    .collect();
                if let Some(r) = hive_runtime::dm::resolve_concurrent(&candidates) {
                    hive_runtime::dm::apply_resolution(&mut archived, &r);
                }
            }
        }
        if archived != before {
            self.dms.set_archived(archived);
        }
    }

    /// After a DM synced: join its roster once the founder's log is here
    /// (redeeming the invite), and publish this box's key-agreement key on
    /// it. Idempotent.
    pub fn settle_dm(&self, conn: &DmConn) {
        let ws = conn.workspace_id();
        let mut svc = self.lock();
        if svc.write_block().is_some() {
            return;
        }
        if let Err(e) = svc.ensure_self_member(workspace_config_session_id(ws), ws) {
            eprintln!("serve: joining a direct message's roster failed: {e}");
            return;
        }
        let identity = hive_runtime::IdentityStore::new(
            &self.cfg.data_dir,
            hive_runtime::secret_store::StoreKeyVault::for_data_dir(&self.cfg.data_dir),
        );
        if let Ok(ka) = identity.key_agreement(svc.device_id()) {
            let _ = svc.publish_key_agreement(ws, ka.public_key_bytes());
        }
    }
}

/// How often the DMs sync without a nudge.
const DM_SYNC_EVERY: Duration = Duration::from_secs(4);

/// Sync every DM: right after a local write (`dm_kick`) and every few
/// seconds. One engine per room, kept for the life of the loop.
pub async fn dm_sync_loop(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    let mut engines: HashMap<String, SyncEngine> = HashMap::new();
    let mut store = match EventStore::open(shared.cfg.data_dir.join("hive.db")) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("dm sync: cannot open the store: {e}");
            return;
        }
    };
    let mut fails: HashMap<String, u32> = HashMap::new();
    let mut skip_until: HashMap<String, tokio::time::Instant> = HashMap::new();
    while !shared.stopping() {
        let dms = shared.dms.list();
        let mut changed = false;
        for conn in &dms {
            if skip_until.get(&conn.room).is_some_and(|t| tokio::time::Instant::now() < *t) {
                continue;
            }
            if !engines.contains_key(&conn.room) {
                let cfg = shared.dms.config_for(&shared.cfg, conn);
                let client = relay_client_for(&shared.cfg, &conn.relay_url);
                let src = match crate::key_source(&cfg) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("dm sync: no key source for a direct message: {e}");
                        continue;
                    }
                };
                engines.insert(conn.room.clone(), SyncEngine::new(client, conn.room.clone()).with_key_source(src));
            }
            let Some(engine) = engines.get_mut(&conn.room) else { continue };
            match crate::sync_round(engine, &mut store).await {
                Ok((pushed, pulled)) => {
                    fails.remove(&conn.room);
                    if pulled > 0 || pushed > 0 {
                        shared.settle_dm(conn);
                        changed = true;
                    }
                }
                Err(e) => {
                    let n = fails.entry(conn.room.clone()).or_default();
                    *n = n.saturating_add(1);
                    let secs = crate::sync_backoff_secs(*n);
                    if *n == 1 || *n % 10 == 0 {
                        eprintln!("dm sync: a direct message's relay failed ({e}); retrying in {secs}s");
                    }
                    skip_until.insert(conn.room.clone(), tokio::time::Instant::now() + Duration::from_secs(secs));
                }
            }
        }
        engines.retain(|room, _| dms.iter().any(|d| &d.room == room));
        if changed {
            shared.resolve_concurrent_dms();
            shared.refresh();
            shared.hub.emit("dms.changed", serde_json::json!({}));
        }
        tokio::select! {
            _ = tokio::time::sleep(DM_SYNC_EVERY) => {}
            _ = shared.dm_kick.notified() => {}
            _ = stop.wait_for(|s| *s) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_matching_and_validation() {
        assert!(same_relay("https://Relay.example/", "https://relay.example"));
        assert!(!same_relay("https://relay.example", "https://relay.example.evil"));
        assert!(valid_relay_url("http://127.0.0.1:8443"));
        assert!(valid_relay_url("https://relay.example"));
        assert!(!valid_relay_url("https://user:pw@relay.example"));
        assert!(!valid_relay_url("file:///etc/passwd"));
        assert!(!valid_relay_url("not a url"));
        // A DM's relay: the box's own (any address), else public https.
        let own = Some("http://127.0.0.1:8443");
        assert!(dm_relay_allowed(own, "http://127.0.0.1:8443/"));
        assert!(dm_relay_allowed(own, "https://relay.example.org"));
        for bad in [
            "http://relay.example.org",
            "https://127.0.0.1:9",
            "https://10.0.0.5",
            "https://169.254.169.254",
            "https://100.100.100.100",
            "https://[::1]",
            "https://[fd00::1]",
            "https://[::ffff:10.0.0.1]",
            "https://localhost",
            "https://printer.local",
            "https://box.internal",
            "https://intranet",
            "https://u:p@relay.example.org",
            // Literals skip the resolver: every non-public form is refused.
            "https://0.0.0.1",
            "https://198.18.0.1",
            "https://224.0.0.1",
            "https://240.0.0.1",
            "https://[64:ff9b::a00:1]",
            "https://[::a00:1]",
            "https://[fec0::1]",
            "https://[ff02::1]",
        ] {
            assert!(!dm_relay_allowed(own, bad), "{bad}");
        }
    }

    /// A DM relay the invite's sender chose connects only to public
    /// addresses, at connect time: a public-looking name that resolves (or
    /// is re-pointed after the invite was checked) to a loopback or private
    /// address is never reached. The box's own relay is unaffected.
    #[tokio::test]
    async fn a_foreign_dm_relay_never_connects_to_an_internal_address() {
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let h = hits.clone();
        let app = axum::Router::new().fallback(move || {
            let h = h.clone();
            async move {
                h.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                (axum::http::StatusCode::NOT_FOUND, "")
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let cid = hive_runtime::contacts::new_cid();
        let base = Config {
            data_dir: std::env::temp_dir(),
            relay_url: Some(format!("http://127.0.0.1:{port}")),
            token: None,
            room: "main".into(),
            key: None,
            epoch_keys: Vec::new(),
        };
        // The box's own relay: reached.
        assert!(relay_client_for(&base, &format!("http://127.0.0.1:{port}")).contact_get(&cid).await.is_ok());
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        // Another relay whose name resolves to loopback: refused before any
        // request leaves the box.
        let foreign = format!("http://localhost:{port}");
        assert!(relay_client_for(&base, &foreign).contact_get(&cid).await.is_err());
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1, "the internal address was reached");
    }

    #[test]
    fn store_keeps_secrets_out_of_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let s = DmStore::load(tmp.path());
        let conn = DmConn {
            name: "Ada".into(),
            relay_url: "http://127.0.0.1:1".into(),
            room: hive_runtime::dm::new_room(),
            peer: Uuid::new_v4().to_string(),
            founder_actor_id: Uuid::new_v4().to_string(),
            fk: "00".repeat(16),
        };
        s.add(conn.clone(), "secret-passphrase", &[]).unwrap();
        assert!(s.add(conn.clone(), "again", &[]).is_err(), "one room once");
        let text = std::fs::read_to_string(path(tmp.path())).unwrap();
        assert!(!text.contains("secret-passphrase"));
        let back = DmStore::load(tmp.path());
        assert_eq!(back.list(), vec![conn.clone()]);
        let base = Config {
            data_dir: tmp.path().to_path_buf(),
            relay_url: Some("https://box-relay.example".into()),
            token: Some("hrt1-secret".into()),
            room: "main".into(),
            key: None,
            epoch_keys: Vec::new(),
        };
        let cfg = back.config_for(&base, &conn);
        assert_eq!(cfg.key.as_deref(), Some("secret-passphrase"));
        assert_eq!(cfg.token, None, "another relay never gets the workspace token");
        let same = DmConn { relay_url: "https://box-relay.example/".into(), room: hive_runtime::dm::new_room(), ..conn };
        back.add(same.clone(), "k2", &[]).unwrap();
        assert_eq!(back.config_for(&base, &same).token.as_deref(), Some("hrt1-secret"));
        assert!(back.set_archived(vec![same.room.clone(), "not-ours".into()]));
        assert_eq!(back.archived(), vec![same.room.clone()]);
        assert!(back.is_archived(same.workspace_id()));
    }
}
