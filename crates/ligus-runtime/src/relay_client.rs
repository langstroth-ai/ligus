//! Relay transport client — ported from `RelayTransportClient.swift`. Pushes
//! signed (and, in production, E2EE-sealed) event envelopes to the relay and
//! fetches everything after a cursor, so two clients sharing a workspace
//! converge. The relay is content-blind; see `ligus-core::e2ee`.
//!
//! This is the relay-forwarding path. Direct P2P (STUN/hole-punch via the
//! rendezvous board) is a follow-up; relay forwarding already provides working
//! multiuser sync.

use ligus_core::SessionEventEnvelope;
use thiserror::Error;

pub use crate::relay_refusal::{RefusalClass, RelayRefusal};

#[derive(Debug, Error)]
pub enum RelayError {
    #[error("http error: {}", http_error_text(.0))]
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
    /// A `429` or `507` with its code and `Retry-After` (see
    /// [`crate::relay_refusal`]). A rate limit was already retried in place
    /// as long as its wait was short.
    #[error("{0}")]
    Refused(RelayRefusal),
    /// A local error (e.g. sealing an attachment before upload).
    #[error("{0}")]
    Message(String),
    /// The blob channel refused with a stable code (`blob_too_large`,
    /// `no_workspace`, …) and a message. A `429`/`507` is [`Self::Refused`]
    /// instead (a short rate limit was already retried in place).
    #[error("{message} (relay: HTTP {status} {code})")]
    BlobRefused { status: u16, code: String, message: String },
}

/// What the relay answers to a blob upload.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlobUploaded {
    pub id: String,
    pub size: u64,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub expires_at: i64,
}

/// A refusal's JSON body (`{"error","message"}`), or the bare status.
async fn refusal(status: u16, resp: reqwest::Response) -> RelayError {
    #[derive(serde::Deserialize)]
    struct Body {
        #[serde(default)]
        error: String,
        #[serde(default)]
        message: String,
    }
    let text = resp.text().await.unwrap_or_default();
    match serde_json::from_str::<Body>(&text) {
        Ok(b) if !b.error.is_empty() => {
            let message = match (b.error.as_str(), status) {
                ("blob_too_large", _) => "the file is larger than the relay accepts".to_string(),
                ("no_workspace", _) => "the relay holds nothing for this workspace yet; sync first".to_string(),
                (_, 507) => "the relay's storage for this workspace is full".to_string(),
                (_, 429) => "too many uploads; try again in a moment".to_string(),
                _ if !b.message.is_empty() => b.message.chars().take(200).filter(|c| !c.is_control()).collect(),
                _ => format!("refused ({})", b.error),
            };
            RelayError::BlobRefused { status, code: b.error.chars().take(64).filter(|c| c.is_ascii_graphic()).collect(), message }
        }
        _ => RelayError::Status(status),
    }
}

/// A `503` from the blob channel (a relay at capacity) as a refusal that
/// keeps its `Retry-After` (at least a few seconds), so the caller waits it.
fn unavailable(resp: &reqwest::Response) -> RelayError {
    RelayError::Refused(RelayRefusal {
        status: 503,
        code: "unavailable".into(),
        message: String::new(),
        retry_after: retry_after_header(resp).unwrap_or(std::time::Duration::from_secs(5)).max(std::time::Duration::from_secs(1)),
    })
}

/// A response's `Retry-After` in whole seconds, if it has one (capped as a
/// refusal's is).
pub(crate) fn retry_after_header(resp: &reqwest::Response) -> Option<std::time::Duration> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.trim().parse::<u64>().ok())
        .map(|s| std::time::Duration::from_secs(s).min(crate::relay_refusal::MAX_RETRY_AFTER))
}

/// A transport error's text. A builder error means the request URL itself
/// was unusable — almost always an empty or non-http(s) relay URL — so it
/// says that instead of reqwest's bare "builder error".
fn http_error_text(e: &reqwest::Error) -> String {
    if e.is_builder() {
        let url = e.url().map(|u| u.to_string()).unwrap_or_default();
        let shown = if url.is_empty() { String::new() } else { format!(" ({url})") };
        return format!("the relay URL{shown} is missing or not an http(s) address; set the workspace's Relay URL ({e})");
    }
    e.to_string()
}

/// The longest an envelope or keyring request (fetch a page, push one) may
/// take, body included. The client sets only a connect timeout (its SSE
/// streams stay open), so a relay or middlebox that accepts a request and
/// then stalls would otherwise hang the sync loop until a restart.
pub const ENVELOPE_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Decode a JSON response, mapping non-2xx to a typed error *before* attempting
/// to parse the body — so a plain-text 401/error page never surfaces as a
/// confusing "error decoding response body".
async fn json_ok<T: serde::de::DeserializeOwned>(resp: reqwest::Response) -> Result<T, RelayError> {
    let status = resp.status();
    if status.is_success() {
        Ok(resp.json().await?)
    } else {
        Err(status_error(resp).await)
    }
}

/// The error for a non-2xx response: a typed refusal for `429`/`507`,
/// `Unauthorized` for `401`/`403`, else the bare status.
async fn status_error(resp: reqwest::Response) -> RelayError {
    let code = resp.status().as_u16();
    match RelayRefusal::from_response(resp).await {
        Ok(r) => RelayError::Refused(r),
        Err(_) if matches!(code, 401 | 403) => RelayError::Unauthorized,
        Err(_) => RelayError::Status(code),
    }
}

/// The error for a refused roster call (members, claim, invites, join). The
/// roster routes answer a short plain-text reason (`forbidden`, `cannot remove
/// the last owner`, ...), so it is carried to the user with the status instead
/// of being read as a rejected access token: a 403 there usually means "not an
/// Admin of this workspace", not a bad token.
async fn roster_error(resp: reqwest::Response) -> RelayError {
    let code = resp.status().as_u16();
    if matches!(code, 429 | 507) {
        return status_error(resp).await;
    }
    let text = resp.text().await.unwrap_or_default();
    let reason: String = text.trim().chars().filter(|c| !c.is_control()).take(200).collect();
    if reason.is_empty() {
        RelayError::Message(format!("the relay refused (HTTP {code})"))
    } else {
        RelayError::Message(format!("the relay refused (HTTP {code}): {reason}"))
    }
}

impl RelayError {
    /// The relay's refusal, when this is one.
    pub fn refusal(&self) -> Option<&RelayRefusal> {
        match self {
            RelayError::Refused(r) => Some(r),
            _ => None,
        }
    }
}

/// The `Idempotency-Key` for pushing the envelope `event_id` from the client
/// holding `secret`: stable across that client's re-pushes (a re-sealed body
/// differs, the key doesn't), and derived so the relay learns no event id.
///
/// The relay dedups by this key across the whole room, for every writer, so
/// the key must not be computable from the event id alone: a co-member who
/// learned an id before this device pushed it (a P2P peer, a box relaying a
/// phone's event) could otherwise push junk under the key first and make the
/// relay answer this device's push with the junk's seq, swallowing the event.
/// `secret` is this client's ([`crate::SyncEngine`] draws one per engine). A
/// re-push from another client (or after a restart) is a separate write: at
/// worst a second copy of the same signed envelope, which every fold treats
/// as that envelope.
pub fn idempotency_key(secret: &[u8; 32], event_id: uuid::Uuid) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    // FROZEN context.
    h.update(b"ligus-push-idempotency-v1\0");
    h.update(secret);
    h.update(event_id.as_bytes());
    h.finalize()[..16].iter().map(|b| format!("{b:02x}")).collect()
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
pub(crate) fn normalize_access_token(raw: &str) -> String {
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
    /// Relay access token. Attached as `Authorization: Bearer` to
    /// every request; `None` for open/self-hosted relays.
    auth: Option<String>,
    /// A langstroth-auth access token kept fresh in the background. When it holds
    /// a value it is sent instead of `auth` (see `with_auth_cell`).
    auth_cell: Option<crate::ligus_auth::TokenCell>,
    /// [`ENVELOPE_REQUEST_TIMEOUT`] (shorter in tests).
    request_timeout: std::time::Duration,
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
        let http = ligus_forge::http_client_builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .pool_idle_timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| ligus_forge::new_http_client());
        Self {
            http,
            base: base.into().trim_end_matches('/').to_string(),
            auth: None,
            auth_cell: None,
            request_timeout: ENVELOPE_REQUEST_TIMEOUT,
        }
    }

    /// A shorter envelope request timeout (tests).
    #[doc(hidden)]
    pub fn with_request_timeout(mut self, t: std::time::Duration) -> Self {
        self.request_timeout = t;
        self
    }

    /// A client for a relay someone else chose (a DM invite's): like
    /// [`Self::new`], but it connects to public addresses only (resolved at
    /// connect time) and follows no redirect
    /// (`ligus_forge::public_only_client_builder`).
    pub fn public_only(base: impl Into<String>) -> Self {
        let http = ligus_forge::public_only_client_builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .pool_idle_timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| ligus_forge::new_http_client());
        Self { http, base: base.into().trim_end_matches('/').to_string(), auth: None, auth_cell: None, request_timeout: ENVELOPE_REQUEST_TIMEOUT }
    }

    /// Set the relay access token (for a token-gated relay). Empty = none.
    pub fn with_auth(mut self, token: Option<String>) -> Self {
        self.auth = token
            .map(|t| normalize_access_token(&t))
            .filter(|t| !t.is_empty());
        self
    }

    /// Read the bearer from a langstroth-auth token cell on every request, so a
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

    /// Attach the access-token bearer (if any) to a request. Nothing else: no
    /// forge (GitHub, GitLab, ...) token ever reaches a relay. Forge tokens
    /// live only in the OS secret store of the device doing git work and are
    /// used only between that device and the forge.
    pub(crate) fn authed(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        // A langstroth-auth token goes only to the relay its session is bound to
        // (E-5); any other relay gets the static token, if one is configured.
        let fresh = self.auth_cell.as_ref().and_then(|c| c.get_for(&self.base)).filter(|t| !t.is_empty());
        match fresh.as_ref().or(self.auth.as_ref()) {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        }
    }

    /// Send the request `make` builds (authed), retrying a rate-limit refusal
    /// in place: wait its `Retry-After`, then send the same request again, at
    /// most [`crate::relay_refusal::MAX_INLINE_RETRIES`] times and only while
    /// the wait is at most [`crate::relay_refusal::MAX_INLINE_WAIT`]. Any
    /// other `429`/`507` (or a rate limit past those bounds) is returned as
    /// [`RelayError::Refused`]; every other response comes back as is.
    pub(crate) async fn send_retrying(&self, make: impl Fn() -> reqwest::RequestBuilder) -> Result<reqwest::Response, RelayError> {
        use crate::relay_refusal::{MAX_INLINE_RETRIES, MAX_INLINE_WAIT};
        let mut tries = 0u32;
        loop {
            let resp = self.authed(make()).send().await?;
            let refusal = match RelayRefusal::from_response(resp).await {
                Err(resp) => return Ok(resp),
                Ok(r) => r,
            };
            if refusal.class() == RefusalClass::RateLimited && tries < MAX_INLINE_RETRIES && refusal.retry_after <= MAX_INLINE_WAIT {
                tries += 1;
                tracing::debug!(target: "sync", code = %refusal.code, wait = ?refusal.retry_after, "relay rate limit: retrying the same request");
                tokio::time::sleep(refusal.retry_after).await;
                continue;
            }
            return Err(RelayError::Refused(refusal));
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
        self.push_value_keyed(workspace, body, None).await
    }

    /// [`Self::push_value`] with an `Idempotency-Key` ([`idempotency_key`]),
    /// so this client's re-push of the same event (after a refusal or a lost
    /// answer) gets its original seq instead of a second copy. A rate limit
    /// is retried in place with the same body and key.
    pub async fn push_value_keyed(
        &self,
        workspace: &str,
        body: &serde_json::Value,
        key: Option<&str>,
    ) -> Result<u64, RelayError> {
        let url = format!("{}/v1/workspaces/{}/envelopes", self.base, workspace);
        let resp = self
            .send_retrying(|| {
                let rb = self.http.post(&url).json(body).timeout(self.request_timeout);
                match key {
                    Some(k) => rb.header("Idempotency-Key", k),
                    None => rb,
                }
            })
            .await?;
        let value: serde_json::Value = json_ok(resp).await?;
        Ok(value.get("seq").and_then(serde_json::Value::as_u64).unwrap_or(0))
    }

    /// Upload a sealed attachment to the workspace's blob channel
    /// (`POST /v1/workspaces/{id}/blobs`, issue #118). The relay is
    /// content-blind: `bytes` is ciphertext. The relay mints the blob id (a
    /// random 43-character token), so ids can't be guessed or enumerated.
    /// Its refusals (413 too large, 507 a quota, 429 a rate, 404 a relay
    /// without the channel or a workspace with nothing stored yet) come back
    /// as [`RelayError::Refused`] with the relay's code and message.
    pub async fn post_blob(&self, workspace: &str, bytes: Vec<u8>) -> Result<BlobUploaded, RelayError> {
        let url = format!("{}/v1/workspaces/{}/blobs", self.base, workspace);
        let resp = self
            .send_retrying(|| {
                self.http
                    .post(&url)
                    .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                    .body(bytes.clone())
            })
            .await?;
        let status = resp.status().as_u16();
        if resp.status().is_success() {
            return Ok(resp.json().await?);
        }
        if matches!(status, 401 | 403) {
            return Err(RelayError::Unauthorized);
        }
        if status == 503 {
            return Err(unavailable(&resp));
        }
        Err(refusal(status, resp).await)
    }

    /// Fetch a blob's ciphertext, at most `max_bytes` of it (a relay that
    /// sends more is refused before it is buffered). `Ok(None)` = the relay
    /// has no such blob (never uploaded, expired, deleted, or a relay
    /// without the channel).
    pub async fn get_blob(&self, workspace: &str, id: &str, max_bytes: u64) -> Result<Option<Vec<u8>>, RelayError> {
        use futures_util::StreamExt;
        let url = format!("{}/v1/workspaces/{}/blobs/{}", self.base, workspace, id);
        let resp = self.send_retrying(|| self.http.get(&url)).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if matches!(resp.status().as_u16(), 401 | 403) {
            return Err(RelayError::Unauthorized);
        }
        if resp.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
            return Err(unavailable(&resp));
        }
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            return Err(refusal(status, resp).await);
        }
        if resp.content_length().is_some_and(|n| n > max_bytes) {
            return Err(RelayError::Message("the relay sent a larger blob than the attachment names".into()));
        }
        let mut out = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if out.len() as u64 + chunk.len() as u64 > max_bytes {
                return Err(RelayError::Message("the relay sent a larger blob than the attachment names".into()));
            }
            out.extend_from_slice(&chunk);
        }
        Ok(Some(out))
    }

    /// Delete a blob (`DELETE /v1/workspaces/{id}/blobs/{blob}`). Idempotent:
    /// the relay answers 204 whether or not it held it. Issued only after a
    /// signed `AttachmentDeleted`.
    pub async fn delete_blob(&self, workspace: &str, id: &str) -> Result<(), RelayError> {
        let url = format!("{}/v1/workspaces/{}/blobs/{}", self.base, workspace, id);
        let resp = self.authed(self.http.delete(url)).send().await?;
        let status = resp.status().as_u16();
        if resp.status().is_success() || status == 404 {
            return Ok(());
        }
        if matches!(status, 401 | 403) {
            return Err(RelayError::Unauthorized);
        }
        Err(refusal(status, resp).await)
    }

    /// Cheap connectivity + auth check: an authed GET of the envelope list.
    /// Never errors — maps every failure to a `RelayProbe` the UI can render.
    pub async fn probe(&self, workspace: &str) -> RelayProbe {
        // limit=1: a probe only needs the status, never the room's history.
        let url = format!("{}/v1/workspaces/{}/envelopes?after=0&limit=1", self.base, workspace);
        // Bounded like every envelope request, so a relay that accepts and
        // never answers can't hang `ligus doctor` or the settings check.
        match self.authed(self.http.get(url).timeout(self.request_timeout)).send().await {
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
            // Without the URL (it can carry credentials or a token; the
            // caller names the relay itself), with the causes that say why.
            Err(e) => {
                let e = e.without_url();
                let mut msg = e.to_string();
                let mut src = std::error::Error::source(&e);
                while let Some(c) = src {
                    msg.push_str(&format!(": {c}"));
                    src = c.source();
                }
                RelayProbe::Unreachable(msg)
            }
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
        let resp = self.send_retrying(|| self.http.get(&url).timeout(self.request_timeout)).await?;
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
        // A row without a sequence, or at or below the one asked after, is
        // no row of this page (a hostile or broken relay): a seq of 0 would
        // collide in the held-refetch table and underflow its page math.
        let rows: Vec<(u64, serde_json::Value)> = rows
            .into_iter()
            .filter_map(|row| {
                let seq = row.get("seq").and_then(serde_json::Value::as_u64).filter(|&s| s > after)?;
                let body = row.get("body").cloned().unwrap_or(serde_json::Value::Null);
                Some((seq, body))
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
        let rows: Vec<serde_json::Value> = json_ok(self.send_retrying(|| self.http.get(&url).timeout(self.request_timeout)).await?).await?;
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
        let rows: Vec<serde_json::Value> = json_ok(self.send_retrying(|| self.http.get(&url).timeout(self.request_timeout)).await?).await?;
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
                // The relay's global stream cap: a plain `503` (no code),
                // perhaps with a `Retry-After`.
                503 => SseConnectError::Busy(retry_after_header(&resp)),
                c => match RelayRefusal::from_response(resp).await {
                    Ok(r) => SseConnectError::Refused(r),
                    Err(_) => SseConnectError::Status(c),
                },
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
        let resp = self.send_retrying(|| self.http.post(&url).json(&body)).await?;
        let value: serde_json::Value = json_ok(resp).await?;
        let code = value
            .get("code")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let expires_in = value.get("expires_in").and_then(serde_json::Value::as_u64).unwrap_or(0);
        Ok((code, expires_in))
    }

    /// Resolve a short pairing code back to its payload. `Ok(None)` if the code
    /// is unknown or expired. After a `pair_lockout` (too many wrong codes)
    /// nothing is sent to this relay until it ends: the lookup is refused
    /// here with the time left.
    pub async fn resolve_pairing(&self, code: &str) -> Result<Option<String>, RelayError> {
        use crate::relay_refusal::{note_pair_lockout, pair_lockout_left};
        if let Some(left) = pair_lockout_left(&self.base) {
            return Err(RelayError::Refused(RelayRefusal {
                status: 429,
                code: "pair_lockout".into(),
                message: String::new(),
                retry_after: left.max(std::time::Duration::from_secs(1)),
            }));
        }
        let url = format!("{}/v1/pair/{}", self.base, code.trim());
        let resp = match self.send_retrying(|| self.http.get(&url)).await {
            Err(RelayError::Refused(r)) => {
                if r.class() == RefusalClass::PairLockout {
                    note_pair_lockout(&self.base, &r);
                }
                return Err(RelayError::Refused(r));
            }
            other => other?,
        };
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
        rotation: &ligus_core::e2ee::WorkspaceKeyRotation,
    ) -> Result<(), RelayError> {
        let url = format!("{}/v1/workspaces/{}/keyring", self.base, workspace);
        let body = serde_json::to_value(rotation)?;
        // A refusal (a full keyring) is an error; any other answer is
        // ignored as before (the config log carries the rotation either way).
        self.send_retrying(|| self.http.post(&url).json(&body)).await?;
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
        let resp = self.send_retrying(|| self.http.post(&url).json(body)).await?;
        if !resp.status().is_success() {
            return Err(status_error(resp).await);
        }
        Ok(())
    }

    /// Every raw row of a workspace's keyring route (opaque JSON).
    pub async fn fetch_keyring_rows(&self, workspace: &str) -> Result<Vec<serde_json::Value>, RelayError> {
        let url = format!("{}/v1/workspaces/{}/keyring", self.base, workspace);
        json_ok(self.send_retrying(|| self.http.get(&url)).await?).await
    }

    /// Fetch all key rotations published to a workspace's keyring.
    pub async fn fetch_key_rotations(
        &self,
        workspace: &str,
    ) -> Result<Vec<ligus_core::e2ee::WorkspaceKeyRotation>, RelayError> {
        let url = format!("{}/v1/workspaces/{}/keyring", self.base, workspace);
        let rows: Vec<serde_json::Value> = json_ok(self.send_retrying(|| self.http.get(&url).timeout(self.request_timeout)).await?).await?;
        Ok(rows
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect())
    }

    /// Claim an unmanaged workspace, making the caller `Owner` and turning on
    /// server-side membership enforcement. `Ok(false)` if the relay doesn't
    /// support membership (open/self-host → 404) or it's already claimed
    /// (409). Any other refusal (an unidentified token, a server error) is an
    /// error carrying the relay's reason.
    pub async fn claim_membership(&self, workspace: &str) -> Result<bool, RelayError> {
        let url = format!("{}/v1/workspaces/{}/members/claim", self.base, workspace);
        let resp = self.authed(self.http.post(url)).send().await?;
        match resp.status().as_u16() {
            200..=299 => Ok(true),
            404 | 409 => Ok(false),
            _ => Err(roster_error(resp).await),
        }
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
        let resp = self.authed(self.http.post(url)).json(&body).send().await?;
        if !resp.status().is_success() {
            return Err(roster_error(resp).await);
        }
        Ok(())
    }

    /// Remove a member (caller must be `Admin`+). Pair with a key rotation so the
    /// ejected account also loses read access to new traffic. A refusal (not
    /// an Admin, the last Owner, membership off) is an error, so a caller never
    /// goes on to rotate as if the relay had removed them.
    pub async fn remove_member(&self, workspace: &str, account: &str) -> Result<(), RelayError> {
        let url = format!("{}/v1/workspaces/{}/members/{}", self.base, workspace, account);
        let resp = self.authed(self.http.delete(url)).send().await?;
        if !resp.status().is_success() {
            return Err(roster_error(resp).await);
        }
        Ok(())
    }

    /// Issue a revocable/expiring invite (caller must be `Admin`+). The raw
    /// `code` is returned ONCE. `Ok(None)` if the relay doesn't support invites
    /// (404); any other refusal (not an Admin, a server error) is an error
    /// carrying the relay's reason.
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
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(roster_error(resp).await);
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
        let resp = self.authed(self.http.delete(url)).send().await?;
        if !resp.status().is_success() {
            return Err(roster_error(resp).await);
        }
        Ok(())
    }

    /// Redeem an invite code to self-enroll in the workspace at its role.
    /// Returns the granted role, or `Ok(None)` if the code was refused
    /// (invalid/expired/revoked/used-up: 403) or invites aren't supported
    /// (404). Any other refusal (an unidentified token, a server error) is an
    /// error carrying the relay's reason.
    pub async fn join_via_invite(
        &self,
        workspace: &str,
        code: &str,
        login: &str,
    ) -> Result<Option<String>, RelayError> {
        let url = format!("{}/v1/workspaces/{}/join", self.base, workspace);
        let body = serde_json::json!({ "code": code, "login": login });
        let resp = self.authed(self.http.post(url)).json(&body).send().await?;
        if matches!(resp.status().as_u16(), 403 | 404) {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(roster_error(resp).await);
        }
        #[derive(serde::Deserialize)]
        struct JoinResp {
            role: String,
        }
        let jr: JoinResp = resp.json().await?;
        Ok(Some(jr.role))
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
    use ligus_core::{ChatMessage, MessageRole, SessionEvent, SessionEventEnvelope};
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

    /// Final review (area B): rows with no sequence, a zero one or one at
    /// or below the cursor are dropped, never read as seq 0.
    #[tokio::test]
    async fn final_review_fetch_page_drops_rows_without_a_usable_seq() {
        let router = axum::Router::new().fallback(|| async {
            axum::Json(serde_json::json!([{"body": {"a": 1}}, {"seq": 0, "body": {}}, {"seq": 2, "body": {}}, {"seq": 5, "body": {"b": 2}}]))
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let c = RelayClient::new(format!("http://{addr}"));
        let page = c.fetch_page("ws", 3, 10).await.unwrap();
        assert_eq!(page.rows.iter().map(|(s, _)| *s).collect::<Vec<_>>(), vec![5]);
        assert_eq!(page.next_after, 5);
    }

    /// Final review (area B): a relay that accepts a page request and never
    /// answers doesn't hang sync: the request times out.
    #[tokio::test]
    async fn final_review_a_stalled_relay_times_out() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let held = tokio::spawn(async move {
            let mut conns = Vec::new();
            loop {
                let (c, _) = listener.accept().await.unwrap();
                conns.push(c);
            }
        });
        let c = RelayClient::new(format!("http://{addr}")).with_request_timeout(std::time::Duration::from_millis(300));
        let r = tokio::time::timeout(std::time::Duration::from_secs(5), c.fetch_page("ws", 0, 10)).await;
        assert!(matches!(r, Ok(Err(_))), "a stalled relay hung the fetch: {:?}", r.map(|x| x.map(|p| p.rows.len())));
        held.abort();
    }

    /// A relay that accepts the probe and never answers: the probe ends
    /// with Unreachable within the request timeout instead of hanging.
    #[tokio::test]
    async fn probe_of_a_stalled_relay_times_out() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let held = tokio::spawn(async move {
            let mut conns = Vec::new();
            loop {
                let (c, _) = listener.accept().await.unwrap();
                conns.push(c);
            }
        });
        let c = RelayClient::new(format!("http://{addr}")).with_request_timeout(std::time::Duration::from_millis(300));
        let r = tokio::time::timeout(std::time::Duration::from_secs(5), c.probe("ws")).await;
        assert!(matches!(r, Ok(RelayProbe::Unreachable(_))), "a stalled relay hung the probe");
        held.abort();
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
        let _ = c.post_blob(&ws, vec![1, 2]).await;
        let _ = c.get_blob(&ws, "b", 10).await;
        let _ = c.delete_blob(&ws, "b").await;
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
        assert!(seen.len() >= 27, "every call reached the recorder ({})", seen.len());
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

    /// C12 `relay_errors_are_errors`: a roster call the relay refuses (403
    /// not an Admin, 409 the last Owner, 500) is an error carrying the
    /// relay's reason, never a silent success. Before this, `remove_member`
    /// answered `Ok(())` on a 403 and the desktop went on to edit its roster
    /// and rotate the key as if the relay had removed the member.
    #[tokio::test]
    async fn roster_helpers_surface_relay_refusals() {
        use axum::http::StatusCode;
        for (status, reason) in [
            (StatusCode::FORBIDDEN, "forbidden"),
            (StatusCode::CONFLICT, "cannot remove the last owner"),
            (StatusCode::INTERNAL_SERVER_ERROR, "store unavailable"),
        ] {
            let router = axum::Router::new().fallback(move || async move { (status, reason) });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            let c = RelayClient::new(format!("http://{addr}")).with_auth(Some("t".into()));
            let ws = Uuid::new_v4().to_string();
            let code = status.as_u16();
            let carries = |what: &str, e: RelayError| {
                let msg = e.to_string();
                assert!(msg.contains(&code.to_string()) && msg.contains(reason), "{what} on {code}: {msg}");
            };
            carries("remove_member", c.remove_member(&ws, "ligus:a").await.expect_err("remove_member refused"));
            carries("upsert_member", c.upsert_member(&ws, "ligus:a", "A", "viewer").await.expect_err("upsert_member refused"));
            carries("revoke_invite", c.revoke_invite(&ws, "i").await.expect_err("revoke_invite refused"));
            carries("create_invite", c.create_invite(&ws, "viewer", 60, 1).await.expect_err("create_invite refused"));
            match c.claim_membership(&ws).await {
                // 409 is "already claimed", a normal answer.
                Ok(false) if code == 409 => {}
                Ok(v) => panic!("claim_membership on {code} answered Ok({v})"),
                Err(e) => carries("claim_membership", e),
            }
            match c.join_via_invite(&ws, "code", "").await {
                // 403 is "that code was refused", a normal answer.
                Ok(None) if code == 403 => {}
                Ok(v) => panic!("join_via_invite on {code} answered Ok({v:?})"),
                Err(e) => carries("join_via_invite", e),
            }
        }
    }

    /// A relay without membership (404) is still "unsupported", not an error.
    #[tokio::test]
    async fn roster_helpers_treat_404_as_unsupported() {
        let router = axum::Router::new().fallback(|| async { (axum::http::StatusCode::NOT_FOUND, "404 page not found") });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let c = RelayClient::new(format!("http://{addr}"));
        let ws = Uuid::new_v4().to_string();
        assert!(!c.claim_membership(&ws).await.unwrap());
        assert!(c.create_invite(&ws, "viewer", 60, 1).await.unwrap().is_none());
        assert!(c.join_via_invite(&ws, "code", "").await.unwrap().is_none());
        assert!(c.list_members(&ws).await.unwrap().is_none());
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
        use ligus_core::e2ee::{KeyAgreementKeypair, WorkspaceKeyRotation};
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
        let opened = ligus_core::e2ee::open(&alice, &got[0].sealed["alice"]).unwrap();
        assert_eq!(opened, key_v2);
    }

    /// An empty or non-http relay URL makes reqwest fail with a bare
    /// "builder error"; the error names the URL problem instead.
    #[tokio::test]
    async fn a_relayless_client_names_the_url_problem() {
        for base in ["", "relay.example", "ftp://relay.example"] {
            let err = RelayClient::new(base).fetch_keyring_rows("w").await.unwrap_err().to_string();
            assert!(err.contains("relay URL"), "{base:?}: {err}");
        }
    }
}
