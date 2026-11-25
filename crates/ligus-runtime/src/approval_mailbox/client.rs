//! The relay's mailbox routes (wire note 4).

use std::time::Duration;

use ligus_core::match_code::{self as mc, hex, put_cap, MailboxKeys};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{b64, system_clock, Clock};

/// The longest request blob the relay takes (8 KiB).
pub const MAX_REQUEST_BLOB: usize = 8 * 1024;
/// The longest message blob the relay takes (32 KiB).
pub const MAX_MSG_BLOB: usize = 32 * 1024;
/// The longest long-poll the relay holds.
pub const MAX_WAIT_SECS: u64 = 25;
/// The longest any one call may take, besides a long poll's wait.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Why a mailbox call failed. [`MailboxError::exit_code`] maps it onto the
/// shared exit-code table.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum MailboxError {
    /// The relay has no mailbox routes (or they are off): paste the request
    /// instead. Exit 3.
    #[error("this relay has no approval mailbox: paste the request instead")]
    NoRoutes,
    /// The relay couldn't be reached, timed out or is unavailable. Exit 7.
    #[error("the relay couldn't be reached: {0}")]
    Unreachable(String),
    /// No mailbox is registered at that id (`mailbox_unknown`). The relay
    /// keeps mailboxes in memory, so a member re-registers
    /// ([`MailboxClient::ensure_registered`]) when it sees this.
    #[error("the approval address isn't registered on this relay (it may be from before a key rotation): ask an Admin for the current address")]
    MailboxUnknown,
    /// The request is gone: expired, deleted or never put (`request_unknown`).
    #[error("the request is gone from the mailbox (expired or ended)")]
    RequestUnknown,
    /// The put cap was refused (the address was reset).
    #[error("the approval address was reset: ask an Admin for the current one")]
    BadCap,
    /// Member auth, a read key or the access token was refused.
    #[error("the relay refused this device's mailbox credentials")]
    Unauthorized,
    /// That request id already exists.
    #[error("that request id is taken")]
    RidExists,
    /// A register or reset that is behind the relay's counter.
    #[error("the approval address was reset by another member meanwhile: try again")]
    StaleCounter,
    /// 64 live requests already.
    #[error("the workspace's approval mailbox is full: try again later or paste the request")]
    MailboxFull,
    /// 10 messages for this request already.
    #[error("too many messages for this request")]
    TooManyMessages,
    /// Too many puts from this address.
    #[error("too many requests from this address: try again in {0} s")]
    RateLimited(u64),
    /// Over the relay's size cap.
    #[error("the message is larger than the relay takes")]
    TooLarge,
    #[error("the relay answered HTTP {0} {1}")]
    Status(u16, String),
    #[error("the relay's answer is malformed")]
    Malformed,
}

impl MailboxError {
    /// The shared exit code: 3 no routes (paste), 7 offline, 6 refused,
    /// 5 gone, 1 otherwise.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::NoRoutes => 3,
            Self::Unreachable(_) | Self::RateLimited(_) => 7,
            Self::Unauthorized | Self::BadCap => 6,
            Self::RequestUnknown | Self::MailboxUnknown => 5,
            _ => 1,
        }
    }

    /// A transient failure worth retrying with backoff.
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Unreachable(_) | Self::RateLimited(_))
    }
}

fn transport(e: reqwest::Error) -> MailboxError {
    MailboxError::Unreachable(if e.is_timeout() { "timed out".into() } else { e.without_url().to_string() })
}

/// How a call authenticates.
#[derive(Clone, Copy)]
pub enum MailboxAuth<'a> {
    /// A member: signed by the mailbox auth key (and the access token, if set).
    Member(&'a MailboxKeys),
    /// The requester: its read key for its own `rid`.
    Requester(&'a [u8; 16]),
}

/// What `PUT .../requests/{rid}` answers.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutAck {
    pub rid: String,
    pub put_at: i64,
    pub expires_at: i64,
}

/// One live request in a mailbox listing.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListedRequest {
    pub rid: String,
    pub blob: String,
    pub put_at: i64,
    pub expires_at: i64,
    #[serde(default)]
    pub msg_count: u32,
}

/// One message on a request.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MailboxMsg {
    pub seq: u64,
    pub blob: String,
    /// Routing hint (`members`, `requester`, `approver`): never trusted.
    pub to: String,
    /// What the relay saw (`member` or `requester`): a hint only.
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub at: i64,
}

#[derive(Deserialize)]
struct Counter {
    counter: u32,
}

/// A client of one relay's mailbox routes.
#[derive(Clone)]
pub struct MailboxClient {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
    clock: Clock,
}

impl std::fmt::Debug for MailboxClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MailboxClient").field("base", &self.base).field("token", &self.token.as_ref().map(|_| "..")).finish()
    }
}

impl MailboxClient {
    /// A client for the relay at `relay_url` (the address's `relay`).
    pub fn new(relay_url: &str) -> Self {
        let http = ligus_forge::http_client_builder()
            .connect_timeout(Duration::from_secs(10))
            .tcp_keepalive(Duration::from_secs(30))
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| ligus_forge::new_http_client());
        Self { http, base: relay_url.trim().trim_end_matches('/').to_string(), token: None, clock: system_clock() }
    }

    /// The relay access token members and puts send on a gated relay. Empty = none.
    pub fn with_auth(mut self, token: Option<String>) -> Self {
        self.token = token.map(|t| crate::relay_client::normalize_access_token(&t)).filter(|t| !t.is_empty());
        self
    }

    /// Another clock (tests).
    #[doc(hidden)]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    pub fn relay(&self) -> &str {
        &self.base
    }

    pub(crate) fn now(&self) -> i64 {
        (self.clock)()
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// The path the relay sees (the base URL's path prefix included).
    fn signed_path(&self, path: &str) -> String {
        let prefix = url::Url::parse(&self.base).map(|u| u.path().trim_end_matches('/').to_string()).unwrap_or_default();
        format!("{prefix}{path}")
    }

    fn bearer(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        }
    }

    fn authed(&self, rb: reqwest::RequestBuilder, auth: MailboxAuth<'_>, method: &str, path: &str, body: &[u8]) -> reqwest::RequestBuilder {
        match auth {
            MailboxAuth::Member(keys) => {
                let ts = self.now();
                let sig = keys.auth().sign(&mc::member_preimage(method, &self.signed_path(path), ts, body));
                self.bearer(rb).header("X-Ligus-Mailbox-Auth", format!("{ts}.{}", b64::enc(&sig)))
            }
            MailboxAuth::Requester(read_key) => rb.header("X-Ligus-Read-Key", b64::enc(read_key)),
        }
    }

    async fn send(&self, rb: reqwest::RequestBuilder) -> Result<reqwest::Response, MailboxError> {
        let resp = rb.send().await.map_err(transport)?;
        if resp.status().is_success() {
            return Ok(resp);
        }
        Err(problem(resp).await)
    }

    async fn json<T: serde::de::DeserializeOwned>(resp: reqwest::Response) -> Result<T, MailboxError> {
        let bytes = resp.bytes().await.map_err(transport)?;
        serde_json::from_slice(&bytes).map_err(|_| MailboxError::Malformed)
    }

    /// The mailbox's reset counter, or `None` when it isn't registered.
    pub async fn counter(&self, mailbox: &[u8; 16]) -> Result<Option<u32>, MailboxError> {
        let rb = self.http.get(self.url(&format!("/v1/mailboxes/{}", hex(mailbox)))).timeout(REQUEST_TIMEOUT);
        match self.send(rb).await {
            Ok(resp) => Ok(Some(Self::json::<Counter>(resp).await?.counter)),
            Err(MailboxError::MailboxUnknown) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Register (or reset) the mailbox of `epoch_key` at `counter`. Returns
    /// the relay's counter.
    pub async fn register(&self, epoch_key: &[u8; 32], counter: u32) -> Result<u32, MailboxError> {
        let keys = MailboxKeys::derive(epoch_key);
        let mailbox = keys.mailbox();
        let cap_hash: [u8; 32] = Sha256::digest(put_cap(epoch_key, counter)).into();
        let auth = keys.auth();
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Register {
            auth_pub: String,
            put_cap_hash: String,
            counter: u32,
            sig: String,
        }
        let body = Register {
            auth_pub: b64::enc(&auth.public_key_bytes()),
            put_cap_hash: b64::enc(&cap_hash),
            counter,
            sig: b64::enc(&auth.sign(&mc::register_preimage(&mailbox, &cap_hash, counter))),
        };
        let rb = self.bearer(self.http.put(self.url(&format!("/v1/mailboxes/{}", hex(&mailbox)))).json(&body).timeout(REQUEST_TIMEOUT));
        match self.send(rb).await {
            Ok(resp) => Ok(Self::json::<Counter>(resp).await?.counter),
            // A 404 from register is always "no routes" (wire note 4).
            Err(MailboxError::MailboxUnknown) => Err(MailboxError::NoRoutes),
            Err(e) => Err(e),
        }
    }

    /// The mailbox of `epoch_key`, registered: its current counter (the one
    /// [`super::ApprovalAddress::derive`] takes). Idempotent.
    pub async fn ensure_registered(&self, epoch_key: &[u8; 32]) -> Result<u32, MailboxError> {
        let mailbox = MailboxKeys::derive(epoch_key).mailbox();
        match self.counter(&mailbox).await? {
            Some(n) => Ok(n),
            None => match self.register(epoch_key, 0).await {
                // Registered by another member meanwhile.
                Err(MailboxError::StaleCounter) => self.counter(&mailbox).await?.ok_or(MailboxError::Malformed),
                r => r,
            },
        }
    }

    /// Reset Address: register the next counter, so the old put cap stops
    /// working. Returns the new counter. (Admins only, by convention.)
    pub async fn reset_address(&self, epoch_key: &[u8; 32]) -> Result<u32, MailboxError> {
        let mailbox = MailboxKeys::derive(epoch_key).mailbox();
        let next = match self.counter(&mailbox).await? {
            Some(n) => n.checked_add(1).ok_or(MailboxError::StaleCounter)?,
            None => 0,
        };
        self.register(epoch_key, next).await
    }

    /// Put a request (`blob` sealed to the address's seal key).
    pub async fn put_request(
        &self,
        mailbox: &[u8; 16],
        rid: &[u8; 16],
        put_cap: &[u8; 16],
        blob: &str,
        read_key: &[u8; 16],
    ) -> Result<PutAck, MailboxError> {
        if blob.len() > MAX_REQUEST_BLOB {
            return Err(MailboxError::TooLarge);
        }
        let body = serde_json::json!({ "blob": blob, "readKeyHash": b64::enc(&Sha256::digest(read_key)) });
        let rb = self
            .bearer(self.http.put(self.url(&format!("/v1/mailboxes/{}/requests/{}", hex(mailbox), hex(rid)))))
            .header("X-Ligus-Put-Cap", b64::enc(put_cap))
            .json(&body)
            .timeout(REQUEST_TIMEOUT);
        Self::json(self.send(rb).await?).await
    }

    /// The live requests (members only), oldest first.
    pub async fn list_requests(&self, keys: &MailboxKeys) -> Result<Vec<ListedRequest>, MailboxError> {
        let path = format!("/v1/mailboxes/{}/requests", hex(&keys.mailbox()));
        let rb = self.authed(self.http.get(self.url(&path)).timeout(REQUEST_TIMEOUT), MailboxAuth::Member(keys), "GET", &path, b"");
        #[derive(Deserialize)]
        struct List {
            requests: Vec<ListedRequest>,
        }
        Ok(Self::json::<List>(self.send(rb).await?).await?.requests)
    }

    /// Post a message on `rid`. `to` is the routing hint.
    pub async fn post_msg(&self, auth: MailboxAuth<'_>, mailbox: &[u8; 16], rid: &[u8; 16], blob: &str, to: &str) -> Result<u64, MailboxError> {
        if blob.len() > MAX_MSG_BLOB {
            return Err(MailboxError::TooLarge);
        }
        let path = format!("/v1/mailboxes/{}/requests/{}/msgs", hex(mailbox), hex(rid));
        let body = serde_json::to_vec(&serde_json::json!({ "blob": blob, "to": to })).expect("serialize");
        let rb = self.http.post(self.url(&path)).header("content-type", "application/json").body(body.clone()).timeout(REQUEST_TIMEOUT);
        let rb = self.authed(rb, auth, "POST", &path, &body);
        #[derive(Deserialize)]
        struct Seq {
            seq: u64,
        }
        Ok(Self::json::<Seq>(self.send(rb).await?).await?.seq)
    }

    /// The messages on `rid` after `after`, long-polling up to `wait_secs`
    /// (at most [`MAX_WAIT_SECS`]) while there are none.
    pub async fn get_msgs(
        &self,
        auth: MailboxAuth<'_>,
        mailbox: &[u8; 16],
        rid: &[u8; 16],
        after: u64,
        wait_secs: u64,
    ) -> Result<Vec<MailboxMsg>, MailboxError> {
        let wait = wait_secs.min(MAX_WAIT_SECS);
        let path = format!("/v1/mailboxes/{}/requests/{}/msgs", hex(mailbox), hex(rid));
        let rb = self.http.get(self.url(&format!("{path}?after={after}&wait={wait}"))).timeout(REQUEST_TIMEOUT + Duration::from_secs(wait));
        let rb = self.authed(rb, auth, "GET", &path, b"");
        #[derive(Deserialize)]
        struct Msgs {
            msgs: Vec<MailboxMsg>,
        }
        Ok(Self::json::<Msgs>(self.send(rb).await?).await?.msgs)
    }

    /// Delete `rid` and its messages (the requester, with its read key). A
    /// request already gone counts as deleted.
    pub async fn delete_request(&self, mailbox: &[u8; 16], rid: &[u8; 16], read_key: &[u8; 16]) -> Result<(), MailboxError> {
        let path = format!("/v1/mailboxes/{}/requests/{}", hex(mailbox), hex(rid));
        let rb = self.authed(self.http.delete(self.url(&path)).timeout(REQUEST_TIMEOUT), MailboxAuth::Requester(read_key), "DELETE", &path, b"");
        match self.send(rb).await {
            Ok(_) | Err(MailboxError::RequestUnknown) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Map a non-2xx answer (a problem document `{"error": code, ...}`).
async fn problem(resp: reqwest::Response) -> MailboxError {
    let status = resp.status().as_u16();
    let retry = resp.headers().get(reqwest::header::RETRY_AFTER).and_then(|h| h.to_str().ok()).and_then(|s| s.trim().parse::<u64>().ok());
    let text = resp.text().await.unwrap_or_default();
    let code = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get("error").or_else(|| v.get("code")).and_then(|c| c.as_str()).map(str::to_string))
        .unwrap_or_default();
    match (status, code.as_str()) {
        (404, "mailbox_unknown") => MailboxError::MailboxUnknown,
        (404, "request_unknown") => MailboxError::RequestUnknown,
        // A 404 without a mailbox problem document: the routes are off or absent.
        (404, _) => MailboxError::NoRoutes,
        (403, "bad_put_cap") | (403, "bad_cap") => MailboxError::BadCap,
        (401 | 403, _) => MailboxError::Unauthorized,
        (409, "stale_counter") => MailboxError::StaleCounter,
        (409, _) => MailboxError::RidExists,
        (413, _) => MailboxError::TooLarge,
        (429, "mailbox_full") | (507, "mailbox_full" | "mailboxes_full" | "mailbox_storage") => MailboxError::MailboxFull,
        (429, "too_many_messages") => MailboxError::TooManyMessages,
        (429, _) => MailboxError::RateLimited(retry.unwrap_or(60).min(3600)),
        (502..=504, _) => MailboxError::Unreachable(format!("HTTP {status}")),
        _ => MailboxError::Status(status, code.chars().filter(|c| c.is_ascii_graphic()).take(64).collect()),
    }
}


/// The workspace's current approval address on `client`'s relay: the newest
/// of `epochs`, its mailbox registered (idempotent). Not a secret.
pub async fn current_address(
    client: &MailboxClient,
    workspace_id: uuid::Uuid,
    founder_pin_hex: &str,
    epochs: &[super::EpochKey],
) -> Result<super::ApprovalAddress, MailboxError> {
    let e = epochs.iter().max_by_key(|e| e.epoch).ok_or(MailboxError::Malformed)?;
    let counter = client.ensure_registered(&e.key).await?;
    Ok(super::ApprovalAddress::derive(client.relay(), workspace_id, founder_pin_hex, e.epoch, &e.key, counter))
}
