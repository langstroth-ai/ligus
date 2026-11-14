//! Relay transport client — ported from `RelayTransportClient.swift`. Pushes
//! signed (and, in production, E2EE-sealed) event envelopes to the relay and
//! fetches everything after a cursor, so two clients sharing a workspace
//! converge. The relay is content-blind; see `hive-core::e2ee`.
//!
//! This is the relay-forwarding path. Direct P2P (STUN/hole-punch via the
//! rendezvous board) is a follow-up; relay forwarding already provides working
//! multiuser sync.

use hive_core::SessionEventEnvelope;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RelayError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// The relay rejected the access token (401/403). Surfaced distinctly so the
    /// sync loop can show an actionable message instead of a JSON-decode error
    /// (a plain-text 401 body would otherwise fail to parse and read as garbage).
    #[error("relay rejected the access token — set a valid one in Settings → Team")]
    Unauthorized,
    /// The relay returned an unexpected non-2xx status.
    #[error("relay returned HTTP {0}")]
    Status(u16),
    /// A local error (e.g. sealing an attachment before upload).
    #[error("{0}")]
    Message(String),
}

/// Decode a JSON response, mapping non-2xx to a typed error *before* attempting
/// to parse the body — so a plain-text 401/error page never surfaces as a
/// confusing "error decoding response body".
async fn json_ok<T: serde::de::DeserializeOwned>(resp: reqwest::Response) -> Result<T, RelayError> {
    let status = resp.status();
    if status.is_success() {
        Ok(resp.json().await?)
    } else if matches!(status.as_u16(), 401 | 403) {
        Err(RelayError::Unauthorized)
    } else {
        Err(RelayError::Status(status.as_u16()))
    }
}

/// Envelopes per page a client asks the relay for. The relay also caps a page
/// by size (4 MiB), so a room of large bodies returns fewer.
pub const ENVELOPE_PAGE_LIMIT: u32 = 500;

/// One page of `GET /v1/workspaces/{id}/envelopes`.
#[derive(Debug, Clone, Default)]
pub struct EnvelopePage {
    /// `(server seq, opaque body)`, oldest first.
    pub rows: Vec<(u64, serde_json::Value)>,
    /// The relay holds envelopes past this page.
    pub more: bool,
    /// The `after` cursor for the next page.
    pub next_after: u64,
}

/// Outcome of a cheap connectivity/auth probe. Every failure maps to a variant
/// so the UI can tell "wrong URL / relay down" apart from "bad token".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayProbe {
    /// Reachable and the token (if any) was accepted.
    Ok,
    /// Reached the relay but it rejected the access token (401/403).
    Unauthorized,
    /// Reached an HTTP server but it returned an unexpected status.
    HttpStatus(u16),
    /// Could not reach the relay at all (DNS/connect/timeout/TLS).
    Unreachable(String),
}

/// Accept either a bare relay token or the issued `name:token` form and return
/// just the value the relay expects as the Bearer. The `name:` prefix is only a
/// server-side lookup key (e.g. `LIGUS_RELAY_USER_TOKENS`); sending it verbatim
/// 401s. Relay tokens are colon-free (hex, or `hrt1.<b64>.<b64>`), so a leading
/// `identifier:` can be dropped safely; anything that doesn't look like that is
/// returned unchanged.
fn normalize_access_token(raw: &str) -> String {
    let t = raw.trim();
    if let Some((prefix, rest)) = t.split_once(':') {
        let prefix_ok = !prefix.is_empty()
            && prefix
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
        let rest_ok = !rest.is_empty() && !rest.contains(':') && !rest.contains('/');
        if prefix_ok && rest_ok {
            return rest.to_string();
        }
    }
    t.to_string()
}

/// A workspace member as returned by a membership-enforcing relay.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberEntry {
    pub account: String,
    pub login: String,
    /// `owner | admin | contributor | viewer`.
    pub role: String,
    #[serde(default)]
    pub added_by: String,
    #[serde(default)]
    pub added_at: u64,
}

/// Invite metadata from a membership-enforcing relay (never the code).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InviteEntry {
    pub id: String,
    pub role: String,
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub expires_at: i64,
    #[serde(default)]
    pub max_uses: i64,
    #[serde(default)]
    pub uses: i64,
    #[serde(default)]
    pub revoked: bool,
}

/// A freshly-issued invite. `code` is shown ONCE (only its hash is stored).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IssuedInvite {
    pub id: String,
    pub code: String,
    pub role: String,
    #[serde(default)]
    pub expires_at: i64,
    #[serde(default)]
    pub max_uses: i64,
}

/// One issued access token's metadata, as returned by the relay admin API
/// (never the raw value or hash).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayTokenEntry {
    pub id: String,
    pub user_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub last_used: u64,
    #[serde(default)]
    pub revoked_at: Option<u64>,
}

/// A relay access user plus their tokens, as returned by `GET /v1/admin/users`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayUserEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub login: String,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub tokens: Vec<RelayTokenEntry>,
}

/// A freshly issued token — the `raw` value is shown ONCE and never retrievable
/// again.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IssuedRelayToken {
    pub user: RelayUserEntry,
    pub token: RelayTokenEntry,
    pub raw: String,
}

/// One envelope as returned by the relay, with its server sequence.
#[derive(Debug, Clone)]
pub struct FetchedEnvelope {
    pub seq: u64,
    pub envelope: SessionEventEnvelope,
}

/// HTTP client for a single relay endpoint (e.g. `https://relay.example`).
#[derive(Debug, Clone)]
pub struct RelayClient {
    http: reqwest::Client,
    base: String,
    /// Relay access token (entitlement). Attached as `Authorization: Bearer` to
    /// every request; `None` for open/self-hosted relays.
    auth: Option<String>,
    /// A ligus-auth access token kept fresh in the background. When it holds
    /// a value it is sent instead of `auth` (see `with_auth_cell`).
    auth_cell: Option<crate::ligus_auth::TokenCell>,
}

impl RelayClient {
    pub fn new(base: impl Into<String>) -> Self {
        // The default reqwest client keeps idle keep-alive connections in its pool
        // with no TCP keepalive probing, so a connection that dies while the
        // machine sleeps (or during a network blip) sits in the pool and gets
        // reused — every request then fails with "error sending request" until the
        // process restarts and rebuilds the pool. TCP keepalive lets a dead socket
        // be detected and dropped, and a short idle timeout evicts stale ones, so
        // the next request opens a fresh connection and sync self-heals without a
        // restart. connect_timeout bounds a wedged connect (SSE streams stay
        // untimed — no per-request timeout, which would sever a long-lived stream).
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .pool_idle_timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            base: base.into().trim_end_matches('/').to_string(),
            auth: None,
            auth_cell: None,
        }
    }

    /// Set the relay access token (for a gated/paid hosted relay). Empty = none.
    pub fn with_auth(mut self, token: Option<String>) -> Self {
        self.auth = token
            .map(|t| normalize_access_token(&t))
            .filter(|t| !t.is_empty());
        self
    }

    /// Read the bearer from a ligus-auth token cell on every request, so a
    /// long-lived client picks up refreshed tokens. An empty cell falls back
    /// to the static token from `with_auth`.
    pub fn with_auth_cell(mut self, cell: Option<crate::ligus_auth::TokenCell>) -> Self {
        self.auth_cell = cell;
        self
    }

    /// The relay base URL (no trailing slash).
    pub(crate) fn base_url(&self) -> &str {
        &self.base
    }

    pub(crate) fn http_client(&self) -> &reqwest::Client {
        &self.http
    }

    /// Attach the entitlement bearer (if any) to a request. Nothing else: no
    /// forge (GitHub, GitLab, ...) token ever reaches a relay. Forge tokens
    /// live only in the OS secret store of the device doing git work and are
    /// used only between that device and the forge.
    pub(crate) fn authed(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        // A ligus-auth token goes only to the relay its session is bound to
        // (E-5); any other relay gets the static token, if one is configured.
        let fresh = self.auth_cell.as_ref().and_then(|c| c.get_for(&self.base)).filter(|t| !t.is_empty());
        match fresh.as_ref().or(self.auth.as_ref()) {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        }
    }

    /// Push an envelope to a workspace; returns the relay-assigned sequence.
    pub async fn push(
        &self,
        workspace: &str,
        envelope: &SessionEventEnvelope,
    ) -> Result<u64, RelayError> {
        let body = serde_json::to_value(envelope)?;
        self.push_value(workspace, &body).await
    }

    /// Push an arbitrary opaque body (e.g. a sealed envelope). The relay is
    /// content-blind; with E2EE this is ciphertext.
    pub async fn push_value(
        &self,
        workspace: &str,
        body: &serde_json::Value,
    ) -> Result<u64, RelayError> {
        let url = format!("{}/v1/workspaces/{}/envelopes", self.base, workspace);
        let resp = self.authed(self.http.post(url)).json(body).send().await?;
        let value: serde_json::Value = json_ok(resp).await?;
        Ok(value.get("seq").and_then(serde_json::Value::as_u64).unwrap_or(0))
    }

    /// Upload an opaque file blob (a sealed attachment) to a workspace. The relay
    /// is content-blind: `bytes` is ciphertext the caller sealed with the
    /// workspace key. `id` is a client-chosen opaque key (a UUID). Only the
    /// enterprise relay serves this route; an open relay returns 404/405.
    pub async fn put_blob(
        &self,
        workspace: &str,
        id: &str,
        bytes: Vec<u8>,
    ) -> Result<(), RelayError> {
        let url = format!("{}/v1/workspaces/{}/blobs/{}", self.base, workspace, id);
        let resp = self
            .authed(self.http.put(url))
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(bytes)
            .send()
            .await?;
        resp.error_for_status()?; // non-2xx (401/403/413/404) → error
        Ok(())
    }

    /// Fetch an opaque file blob's ciphertext. `Ok(None)` = the relay returned 404
    /// (never uploaded, aged out, or an open relay without the blob route).
    pub async fn get_blob(
        &self,
        workspace: &str,
        id: &str,
    ) -> Result<Option<Vec<u8>>, RelayError> {
        let url = format!("{}/v1/workspaces/{}/blobs/{}", self.base, workspace, id);
        let resp = self.authed(self.http.get(url)).send().await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let resp = resp.error_for_status()?;
        Ok(Some(resp.bytes().await?.to_vec()))
    }

    /// Cheap connectivity + auth check: an authed GET of the envelope list.
    /// Never errors — maps every failure to a `RelayProbe` the UI can render.
    pub async fn probe(&self, workspace: &str) -> RelayProbe {
        // limit=1: a probe only needs the status, never the room's history.
        let url = format!("{}/v1/workspaces/{}/envelopes?after=0&limit=1", self.base, workspace);
        match self.authed(self.http.get(url)).send().await {
            Ok(resp) => {
                let code = resp.status().as_u16();
                if resp.status().is_success() {
                    RelayProbe::Ok
                } else if code == 401 || code == 403 {
                    RelayProbe::Unauthorized
                } else {
                    RelayProbe::HttpStatus(code)
                }
            }
            Err(e) => RelayProbe::Unreachable(e.to_string()),
        }
    }

    /// Fetch one page of raw opaque bodies past `after` (at most `limit`; the
    /// relay may return fewer, bounded by its own page cap and byte budget).
    ///
    /// The relay marks a partial page with `X-Hive-More: true` and the cursor to
    /// continue from with `X-Hive-Next-After`. A relay that predates paging sends
    /// neither and returns everything, so `more` is false and the page is the
    /// whole tail — the caller is caught up either way.
    pub async fn fetch_page(
        &self,
        workspace: &str,
        after: u64,
        limit: u32,
    ) -> Result<EnvelopePage, RelayError> {
        let url = format!(
            "{}/v1/workspaces/{}/envelopes?after={}&limit={}",
            self.base, workspace, after, limit
        );
        let resp = self.authed(self.http.get(url)).send().await?;
        let header = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .map(str::to_owned)
        };
        let more = header("x-hive-more").is_some_and(|v| v.eq_ignore_ascii_case("true") || v == "1");
        let next_header = header("x-hive-next-after").and_then(|v| v.parse::<u64>().ok());
        let rows: Vec<serde_json::Value> = json_ok(resp).await?;
        let rows: Vec<(u64, serde_json::Value)> = rows
            .into_iter()
            .map(|row| {
                let seq = row.get("seq").and_then(serde_json::Value::as_u64).unwrap_or(0);
                let body = row.get("body").cloned().unwrap_or(serde_json::Value::Null);
                (seq, body)
            })
            .collect();
        // Continue past the highest seq actually received; the header only
        // matters for an (unexpected) empty page that still says "more".
        let last = rows.iter().map(|(seq, _)| *seq).max().unwrap_or(after);
        let next_after = last.max(next_header.unwrap_or(after)).max(after);
        // A page that claims "more" but can't move the cursor would loop
        // forever; treat it as caught up and let the next round retry.
        let more = more && next_after > after;
        Ok(EnvelopePage { rows, more, next_after })
    }

    /// Fetch raw opaque bodies past `after`, with their server sequences.
    pub async fn fetch_values(
        &self,
        workspace: &str,
        after: u64,
    ) -> Result<Vec<(u64, serde_json::Value)>, RelayError> {
        let url = format!(
            "{}/v1/workspaces/{}/envelopes?after={}",
            self.base, workspace, after
        );
        let rows: Vec<serde_json::Value> = json_ok(self.authed(self.http.get(url)).send().await?).await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let seq = row.get("seq").and_then(serde_json::Value::as_u64).unwrap_or(0);
                let body = row.get("body").cloned().unwrap_or(serde_json::Value::Null);
                (seq, body)
            })
            .collect())
    }

    /// Fetch every envelope on a workspace with server sequence > `after`.
    pub async fn fetch(
        &self,
        workspace: &str,
        after: u64,
    ) -> Result<Vec<FetchedEnvelope>, RelayError> {
        let url = format!(
            "{}/v1/workspaces/{}/envelopes?after={}",
            self.base, workspace, after
        );
        let rows: Vec<serde_json::Value> = json_ok(self.authed(self.http.get(url)).send().await?).await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let seq = row.get("seq").and_then(serde_json::Value::as_u64).unwrap_or(0);
            if let Some(body) = row.get("body") {
                let envelope: SessionEventEnvelope = serde_json::from_value(body.clone())?;
                out.push(FetchedEnvelope { seq, envelope });
            }
        }
        Ok(out)
    }

    /// Open the relay's live push stream for a workspace:
    /// `GET /v1/workspaces/{id}/events` as `text/event-stream`, authed with the
    /// same bearer as every other read. The relay sends a
    /// `data: {"seq":N}` nudge whenever a new envelope is appended; each nudge is
    /// forwarded over the returned [`SseStream`] so the sync loop can trigger an
    /// immediate pull instead of waiting for its poll. The stream is a wake-up
    /// only — event bodies still come through the unchanged fetch/decode path, so
    /// this transport stays content-blind.
    ///
    /// A background task drives `reqwest`'s `bytes_stream()` through an
    /// [`SseDecoder`] (robust to lines split across chunks) and pushes nudges
    /// into the channel; it exits (closing the stream → `recv` yields `None`)
    /// when the body ends or errors, so the caller reconnects with backoff.
    /// Dropping the [`SseStream`] tears the task and HTTP connection down.
    ///
    /// Errors distinguish an older relay without the endpoint (404 →
    /// [`SseConnectError::Unsupported`], degrade to polling) from auth/transport
    /// failures, so the caller can degrade gracefully rather than hard-fail.
    pub async fn open_event_stream(
        &self,
        workspace: &str,
    ) -> Result<crate::sse::SseStream, crate::sse::SseConnectError> {
        use crate::sse::{SseConnectError, SseDecoder, SseStream};
        let url = format!("{}/v1/workspaces/{}/events", self.base, workspace);
        let resp = self
            .authed(self.http.get(url))
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send()
            .await
            .map_err(|e| SseConnectError::Transport(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(match status.as_u16() {
                404 => SseConnectError::Unsupported,
                401 | 403 => SseConnectError::Unauthorized,
                c => SseConnectError::Status(c),
            });
        }
        // A small buffer: nudges are cheap and coalesce naturally (any pending
        // nudge triggers a full pull), so a slow consumer just drops back to one
        // pull-per-drain rather than growing unboundedly.
        let (tx, rx) = tokio::sync::mpsc::channel::<u64>(64);
        tokio::spawn(async move {
            use futures_util::StreamExt;
            let mut decoder = SseDecoder::new();
            let mut body = resp.bytes_stream();
            while let Some(chunk) = body.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    // A read error ends the stream; the caller reconnects.
                    Err(_) => break,
                };
                for data in decoder.feed(&chunk) {
                    // Any `data:` frame is a "pull now" nudge; a missing/garbled
                    // seq still wakes the loop (seq 0 is a valid nudge marker).
                    let seq = crate::sse::parse_seq(&data).unwrap_or(0);
                    if tx.send(seq).await.is_err() {
                        // Receiver (SseStream) dropped → nobody's listening; stop.
                        return;
                    }
                }
            }
        });
        Ok(SseStream::new(rx))
    }

    /// Upsert this device's ephemeral presence (online + typing) for a
    /// workspace. Presence is not part of the event log — it's transient
    /// metadata keyed by `device_id` and overwritten on each ping.
    pub async fn publish_presence(
        &self,
        workspace: &str,
        device_id: &str,
        data: &serde_json::Value,
    ) -> Result<(), RelayError> {
        let url = format!("{}/v1/workspaces/{}/presence", self.base, workspace);
        let body = serde_json::json!({ "device_id": device_id, "data": data });
        self.authed(self.http.post(url)).json(&body).send().await?;
        Ok(())
    }

    /// Store an opaque payload (a peer friend code or workspace invite) behind a
    /// short, human-typeable pairing code; returns `(code, expires_in_secs)`.
    /// The relay only brokers the handoff — it never sees plaintext traffic.
    pub async fn publish_pairing(
        &self,
        payload: &str,
        ttl_secs: Option<u64>,
    ) -> Result<(String, u64), RelayError> {
        let url = format!("{}/v1/pair", self.base);
        let body = serde_json::json!({ "payload": payload, "ttl_secs": ttl_secs });
        let resp = self.authed(self.http.post(url)).json(&body).send().await?;
        let value: serde_json::Value = resp.json().await?;
        let code = value
            .get("code")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let expires_in = value.get("expires_in").and_then(serde_json::Value::as_u64).unwrap_or(0);
        Ok((code, expires_in))
    }

    /// Resolve a short pairing code back to its payload. `Ok(None)` if the code
    /// is unknown or expired.
    pub async fn resolve_pairing(&self, code: &str) -> Result<Option<String>, RelayError> {
        let url = format!("{}/v1/pair/{}", self.base, code.trim());
        let resp = self.authed(self.http.get(url)).send().await?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        let value: serde_json::Value = resp.json().await?;
        Ok(value
            .get("payload")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string))
    }

    /// Publish a workspace-key rotation (sealed per-device) to the keyring. The
    /// relay stores it opaquely; only recipients can open their sealed blob.
    pub async fn publish_key_rotation(
        &self,
        workspace: &str,
        rotation: &hive_core::e2ee::WorkspaceKeyRotation,
    ) -> Result<(), RelayError> {
        let url = format!("{}/v1/workspaces/{}/keyring", self.base, workspace);
        let body = serde_json::to_value(rotation)?;
        self.authed(self.http.post(url)).json(&body).send().await?;
        Ok(())
    }

    /// Publish a signed rotation body (B1) to the keyring route, for devices
    /// whose config log is behind. Same `/v1` route and opaque body as before;
    /// the body holds only sealed blobs, versions and the key commitment (not
    /// the signed envelope, so the relay learns nothing about its author). A
    /// full keyring (507) is an error the caller may ignore: the config log
    /// carries the rotation either way.
    pub async fn publish_keyring_row(&self, workspace: &str, body: &serde_json::Value) -> Result<(), RelayError> {
        let url = format!("{}/v1/workspaces/{}/keyring", self.base, workspace);
        let resp = self.authed(self.http.post(url)).json(body).send().await?;
        if !resp.status().is_success() {
            return Err(RelayError::Status(resp.status().as_u16()));
        }
        Ok(())
    }

    /// Every raw row of a workspace's keyring route (opaque JSON).
    pub async fn fetch_keyring_rows(&self, workspace: &str) -> Result<Vec<serde_json::Value>, RelayError> {
        let url = format!("{}/v1/workspaces/{}/keyring", self.base, workspace);
        json_ok(self.authed(self.http.get(url)).send().await?).await
    }

    /// Fetch all key rotations published to a workspace's keyring.
    pub async fn fetch_key_rotations(
        &self,
        workspace: &str,
    ) -> Result<Vec<hive_core::e2ee::WorkspaceKeyRotation>, RelayError> {
        let url = format!("{}/v1/workspaces/{}/keyring", self.base, workspace);
        let rows: Vec<serde_json::Value> = json_ok(self.authed(self.http.get(url)).send().await?).await?;
        Ok(rows
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect())
    }

    /// Claim an unmanaged workspace, making the caller `Owner` and turning on
    /// server-side membership enforcement. `Ok(false)` if the relay doesn't
    /// support membership (open/self-host → 404) or it's already claimed.
    pub async fn claim_membership(&self, workspace: &str) -> Result<bool, RelayError> {
        let url = format!("{}/v1/workspaces/{}/members/claim", self.base, workspace);
        let resp = self.authed(self.http.post(url)).send().await?;
        Ok(resp.status().is_success())
    }

    /// List a workspace's members (membership-enforcing relays only). `Ok(None)`
    /// if the workspace is unmanaged or the relay doesn't support membership.
    pub async fn list_members(&self, workspace: &str) -> Result<Option<Vec<MemberEntry>>, RelayError> {
        let url = format!("{}/v1/workspaces/{}/members", self.base, workspace);
        let resp = self.authed(self.http.get(url)).send().await?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        Ok(Some(resp.json().await?))
    }

    /// Add a member or change their role (caller must be `Admin`+).
    pub async fn upsert_member(
        &self,
        workspace: &str,
        account: &str,
        login: &str,
        role: &str,
    ) -> Result<(), RelayError> {
        let url = format!("{}/v1/workspaces/{}/members", self.base, workspace);
        let body = serde_json::json!({ "account": account, "login": login, "role": role });
        self.authed(self.http.post(url)).json(&body).send().await?;
        Ok(())
    }

    /// Remove a member (caller must be `Admin`+). Pair with a key rotation so the
    /// ejected account also loses read access to new traffic.
    pub async fn remove_member(&self, workspace: &str, account: &str) -> Result<(), RelayError> {
        let url = format!("{}/v1/workspaces/{}/members/{}", self.base, workspace, account);
        self.authed(self.http.delete(url)).send().await?;
        Ok(())
    }

    /// Issue a revocable/expiring invite (caller must be `Admin`+). The raw
    /// `code` is returned ONCE. `Ok(None)` if the relay doesn't support invites
    /// or the caller isn't authorized.
    pub async fn create_invite(
        &self,
        workspace: &str,
        role: &str,
        ttl_secs: i64,
        max_uses: i64,
    ) -> Result<Option<IssuedInvite>, RelayError> {
        let url = format!("{}/v1/workspaces/{}/invites", self.base, workspace);
        let body = serde_json::json!({ "role": role, "ttlSecs": ttl_secs, "maxUses": max_uses });
        let resp = self.authed(self.http.post(url)).json(&body).send().await?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        Ok(Some(resp.json().await?))
    }

    /// List a workspace's invites (metadata only; never the codes). `Ok(None)`
    /// if unsupported or unauthorized.
    pub async fn list_invites(&self, workspace: &str) -> Result<Option<Vec<InviteEntry>>, RelayError> {
        let url = format!("{}/v1/workspaces/{}/invites", self.base, workspace);
        let resp = self.authed(self.http.get(url)).send().await?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        Ok(Some(resp.json().await?))
    }

    /// Revoke an invite by id (caller must be `Admin`+).
    pub async fn revoke_invite(&self, workspace: &str, invite_id: &str) -> Result<(), RelayError> {
        let url = format!("{}/v1/workspaces/{}/invites/{}", self.base, workspace, invite_id);
        self.authed(self.http.delete(url)).send().await?;
        Ok(())
    }

    /// Redeem an invite code to self-enroll in the workspace at its role.
    /// Returns the granted role, or `Ok(None)` if the code was refused
    /// (invalid/expired/revoked/used-up) or invites aren't supported.
    pub async fn join_via_invite(
        &self,
        workspace: &str,
        code: &str,
        login: &str,
    ) -> Result<Option<String>, RelayError> {
        let url = format!("{}/v1/workspaces/{}/join", self.base, workspace);
        let body = serde_json::json!({ "code": code, "login": login });
        let resp = self.authed(self.http.post(url)).json(&body).send().await?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        #[derive(serde::Deserialize)]
        struct JoinResp {
            role: String,
        }
        let jr: JoinResp = resp.json().await?;
        Ok(Some(jr.role))
    }

    // ── Relay access user/token administration (enterprise relay) ──────────
    // Gated server-side by the relay's admin authorizer, authenticated by the
    // relay bearer. `Ok(None)` when the relay has no admin API (open/OSS) or
    // the caller isn't an admin.

    /// List relay access users + their tokens. `Ok(None)` if the admin API is
    /// unavailable or the caller isn't authorized.
    pub async fn admin_list_users(&self) -> Result<Option<Vec<RelayUserEntry>>, RelayError> {
        let url = format!("{}/v1/admin/users", self.base);
        let resp = self.authed(self.http.get(url)).send().await?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        Ok(Some(resp.json().await?))
    }

    /// Create a user and issue their first token. The returned `raw` token is
    /// shown once. Returns the HTTP status text on failure.
    pub async fn admin_create_user(
        &self,
        name: &str,
        login: &str,
        label: &str,
    ) -> Result<IssuedRelayToken, RelayError> {
        let url = format!("{}/v1/admin/users", self.base);
        let body = serde_json::json!({ "name": name, "login": login, "label": label });
        let resp = self.authed(self.http.post(url)).json(&body).send().await?;
        Ok(resp.error_for_status()?.json().await?)
    }

    /// Issue an additional token for an existing user (raw shown once).
    pub async fn admin_issue_token(
        &self,
        user_id: &str,
        label: &str,
    ) -> Result<IssuedRelayToken, RelayError> {
        let url = format!("{}/v1/admin/users/{}/tokens", self.base, user_id);
        let body = serde_json::json!({ "label": label });
        let resp = self.authed(self.http.post(url)).json(&body).send().await?;
        Ok(resp.error_for_status()?.json().await?)
    }

    /// Revoke a single token immediately.
    pub async fn admin_revoke_token(&self, token_id: &str) -> Result<(), RelayError> {
        let url = format!("{}/v1/admin/tokens/{}", self.base, token_id);
        self.authed(self.http.delete(url)).send().await?.error_for_status()?;
        Ok(())
    }

    /// Enable or disable a user (disabling kills all their tokens at once).
    pub async fn admin_set_user_disabled(
        &self,
        user_id: &str,
        disabled: bool,
    ) -> Result<(), RelayError> {
        let url = format!("{}/v1/admin/users/{}/disabled", self.base, user_id);
        let body = serde_json::json!({ "disabled": disabled });
        self.authed(self.http.post(url)).json(&body).send().await?.error_for_status()?;
        Ok(())
    }

    /// Read everyone's presence blobs for a workspace (`device_id` → data).
    pub async fn list_presence(
        &self,
        workspace: &str,
    ) -> Result<std::collections::HashMap<String, serde_json::Value>, RelayError> {
        let url = format!("{}/v1/workspaces/{}/presence", self.base, workspace);
        let map = self.authed(self.http.get(url)).send().await?.json().await?;
        Ok(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::{ChatMessage, MessageRole, SessionEvent, SessionEventEnvelope};
    use uuid::Uuid;

    #[test]
    fn normalizes_name_prefixed_tokens() {
        // The issued `name:token` form → bare token.
        assert_eq!(
            normalize_access_token("alice:0123456789abcdef0123456789abcdef01234567"),
            "0123456789abcdef0123456789abcdef01234567"
        );
        // A bare token is untouched.
        assert_eq!(normalize_access_token("abcdef123456"), "abcdef123456");
        // hrt1 tokens contain dots but no colons → untouched.
        assert_eq!(normalize_access_token("hrt1.abc.def"), "hrt1.abc.def");
        // Leading/trailing whitespace trimmed.
        assert_eq!(normalize_access_token("  bob:tok  "), "tok");
        // A mispasted URL is not treated as name:token (rest has '/').
        assert_eq!(normalize_access_token("https://relay.example"), "https://relay.example");
    }

    async fn spawn_relay() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, crate::test_relay::router()).await.unwrap();
        });
        format!("http://{addr}")
    }

    /// S7 (`relay_client_never_sends_forge_headers`): every relay call is
    /// recorded and none carries a forge token header, and the only
    /// `Authorization` the relay ever sees is the relay bearer.
    #[tokio::test]
    async fn relay_client_never_sends_forge_headers() {
        use std::sync::{Arc, Mutex};
        type Seen = Arc<Mutex<Vec<(String, Vec<(String, String)>)>>>;
        let seen: Seen = Arc::default();
        let rec = seen.clone();
        let router = axum::Router::new().fallback(move |req: axum::extract::Request| {
            let rec = rec.clone();
            async move {
                let headers = req
                    .headers()
                    .iter()
                    .map(|(k, v)| (k.as_str().to_ascii_lowercase(), v.to_str().unwrap_or("").to_string()))
                    .collect();
                rec.lock().unwrap().push((format!("{} {}", req.method(), req.uri().path()), headers));
                axum::Json(serde_json::json!([]))
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let c = RelayClient::new(format!("http://{addr}")).with_auth(Some("relay-bearer-1".into()));
        let ws = Uuid::new_v4().to_string();
        let env = SessionEventEnvelope::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            1,
            SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "A", "hi") },
        );
        let _ = c.push(&ws, &env).await;
        let _ = c.fetch_page(&ws, 0, 10).await;
        let _ = c.fetch_values(&ws, 0).await;
        let _ = c.fetch(&ws, 0).await;
        let _ = c.probe(&ws).await;
        let _ = c.put_blob(&ws, "b", vec![1, 2]).await;
        let _ = c.get_blob(&ws, "b").await;
        let _ = c.publish_presence(&ws, "d", &serde_json::json!({})).await;
        let _ = c.list_presence(&ws).await;
        let _ = c.publish_pairing("x", None).await;
        let _ = c.resolve_pairing("ABC").await;
        let _ = c.publish_keyring_row(&ws, &serde_json::json!({})).await;
        let _ = c.fetch_keyring_rows(&ws).await;
        let _ = c.claim_membership(&ws).await;
        let _ = c.list_members(&ws).await;
        let _ = c.upsert_member(&ws, "ligus:a", "A", "viewer").await;
        let _ = c.remove_member(&ws, "ligus:a").await;
        let _ = c.create_invite(&ws, "viewer", 60, 1).await;
        let _ = c.list_invites(&ws).await;
        let _ = c.revoke_invite(&ws, "i").await;
        let _ = c.join_via_invite(&ws, "code", "").await;
        let _ = c.admin_list_users().await;
        let _ = c.admin_revoke_token("t").await;
        let _ = c.open_event_stream(&ws).await;
        let cid = crate::contacts::new_cid();
        let key = crate::contacts::new_write_key();
        let _ = c.contact_put(&cid, &key, &serde_json::json!({}), true).await;
        let _ = c.contact_get(&cid).await;
        let _ = c.contact_drop(&cid, &serde_json::json!({})).await;
        let _ = c.contact_drops(&cid, &key, 0).await;
        let _ = c.contact_drops_ack(&cid, &key, 1).await;
        let _ = c.contact_delete(&cid, &key).await;
        let seen = seen.lock().unwrap();
        assert!(seen.len() >= 29, "every call reached the recorder ({})", seen.len());
        for (route, headers) in seen.iter() {
            for (name, value) in headers {
                assert!(
                    !["github", "gitlab", "gitea", "forgejo", "forge-token", "codeberg", "srht"].iter().any(|f| name.contains(f)),
                    "{route} sent a forge header {name}"
                );
                if name == "authorization" {
                    assert_eq!(value, "Bearer relay-bearer-1", "{route}: only the relay bearer");
                }
            }
        }
    }

    #[tokio::test]
    async fn two_clients_sync_through_the_relay() {
        let base = spawn_relay().await;
        let workspace = Uuid::new_v4().to_string();

        // Client A authors and pushes two envelopes.
        let client_a = RelayClient::new(&base);
        let sid = Uuid::new_v4();
        let wid = Uuid::new_v4();
        for (i, body) in ["hello", "from A"].iter().enumerate() {
            let env = SessionEventEnvelope::new(
                sid,
                wid,
                (i + 1) as i64,
                SessionEvent::MessageAppended {
                    message: ChatMessage::new(MessageRole::User, "A", *body),
                },
            );
            client_a.push(&workspace, &env).await.unwrap();
        }

        // Client B fetches from the start and sees both, in order.
        let client_b = RelayClient::new(&base);
        let fetched = client_b.fetch(&workspace, 0).await.unwrap();
        assert_eq!(fetched.len(), 2);
        assert_eq!(fetched[0].seq, 1);
        assert_eq!(fetched[1].seq, 2);
        match &fetched[1].envelope.payload {
            SessionEvent::MessageAppended { message } => assert_eq!(message.body, "from A"),
            _ => panic!("unexpected payload"),
        }

        // Incremental fetch after the cursor returns nothing new.
        assert!(client_b.fetch(&workspace, 2).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn pairing_code_round_trips_through_relay() {
        let base = spawn_relay().await;
        let sharer = RelayClient::new(&base);
        let joiner = RelayClient::new(&base);

        let payload = "hive_AbC123example";
        let (code, ttl) = sharer.publish_pairing(payload, Some(600)).await.unwrap();
        assert!(!code.is_empty());
        assert_eq!(ttl, 600);

        // Joiner resolves it (case/separator tolerant on the relay side).
        let got = joiner.resolve_pairing(&code.to_lowercase()).await.unwrap();
        assert_eq!(got.as_deref(), Some(payload));

        // Unknown code → None, not an error.
        assert_eq!(joiner.resolve_pairing("ZZZZZZ").await.unwrap(), None);
    }

    #[tokio::test]
    async fn key_rotations_publish_and_fetch() {
        use hive_core::e2ee::{KeyAgreementKeypair, WorkspaceKeyRotation};
        let base = spawn_relay().await;
        let owner = RelayClient::new(&base);
        let member = RelayClient::new(&base);
        let workspace = Uuid::new_v4().to_string();

        // Owner seals a v2 key to one remaining device and publishes it.
        let alice = KeyAgreementKeypair::generate().unwrap();
        let key_v2 = [7u8; 32];
        let rot = WorkspaceKeyRotation::seal_for_devices(
            2,
            &key_v2,
            &[("alice".into(), alice.public_key_bytes().to_vec())],
        )
        .unwrap();
        owner.publish_key_rotation(&workspace, &rot).await.unwrap();

        // Member fetches it and opens its sealed blob back to the new key.
        let got = member.fetch_key_rotations(&workspace).await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].version, 2);
        let opened = hive_core::e2ee::open(&alice, &got[0].sealed["alice"]).unwrap();
        assert_eq!(opened, key_v2);
    }
}
