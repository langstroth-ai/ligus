//! Short link codes (Ligus 0.5): the device a person already uses shows
//! `K7Q-4MX` for its workspace's approval address, and their new device
//! types it instead of pasting the address. The derivation is
//! [`ligus_core::link_code`]; the relay routes are
//! `/v1/mailboxes/{mailbox}/codes/{code}` (a member posts, checks and
//! withdraws) and `/v1/link-codes/{code}` (anyone resolves, once).
//!
//! * [`LinkCodeOffer`]: the showing side. [`LinkCodeOffer::post`] registers
//!   the mailbox, seals the address and posts a fresh code;
//!   [`LinkCodeOffer::check`] says whether it is still live, was used, or
//!   expired, and posts it again (the same code) when the relay restarted.
//! * [`resolve`]: the typing side: the approval address behind a code,
//!   checked to be for the mailbox and relay it came from.
//!
//! A relay without the routes answers [`MailboxError::NoRoutes`]: the
//! caller falls back to the pasted address.

use ligus_core::link_code::{ShortCode, Stretched};
use ligus_core::match_code::{hex, MailboxKeys};
use serde::Deserialize;
use uuid::Uuid;

use super::client::{problem_of, problem_parts, transport, REQUEST_TIMEOUT};
use super::{current_address, ApprovalAddress, EpochKey, MailboxAuth, MailboxClient, MailboxError};

/// How long a fresh code lives (the relay's longest).
pub const LINK_CODE_TTL_SECS: u64 = (ligus_core::link_code::TTL_MS / 1000) as u64;
/// The shortest life a code is posted again with after a restart.
const MIN_REPOST_SECS: i64 = 60;
/// Fresh codes tried when one is taken.
const TRIES: usize = 3;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodeInfo {
    expires_at: i64,
    #[serde(default)]
    used: bool,
}

impl MailboxClient {
    /// Post (or post again) a code for the mailbox of `keys`.
    async fn put_link_code(&self, keys: &MailboxKeys, lookup: &str, blob: &str, ttl_secs: u64) -> Result<CodeInfo, MailboxError> {
        let path = format!("/v1/mailboxes/{}/codes/{lookup}", hex(&keys.mailbox()));
        let body = serde_json::to_vec(&serde_json::json!({ "blob": blob, "ttlSecs": ttl_secs })).expect("serialize");
        let rb = self.http.put(self.url(&path)).header("content-type", "application/json").body(body.clone()).timeout(REQUEST_TIMEOUT);
        let rb = self.authed(rb, MailboxAuth::Member(keys), "PUT", &path, &body);
        Self::json(self.send(rb).await?).await
    }

    /// Whether the code is live (and used).
    async fn link_code_status(&self, keys: &MailboxKeys, lookup: &str) -> Result<CodeInfo, MailboxError> {
        let path = format!("/v1/mailboxes/{}/codes/{lookup}", hex(&keys.mailbox()));
        let rb = self.authed(self.http.get(self.url(&path)).timeout(REQUEST_TIMEOUT), MailboxAuth::Member(keys), "GET", &path, b"");
        Self::json(self.send(rb).await?).await
    }

    async fn delete_link_code(&self, keys: &MailboxKeys, lookup: &str) -> Result<(), MailboxError> {
        let path = format!("/v1/mailboxes/{}/codes/{lookup}", hex(&keys.mailbox()));
        let rb = self.authed(self.http.delete(self.url(&path)).timeout(REQUEST_TIMEOUT), MailboxAuth::Member(keys), "DELETE", &path, b"");
        self.send(rb).await.map(|_| ())
    }

    /// `POST /v1/link-codes/{lookup}`: `(mailbox, blob)`, once.
    async fn resolve_link_code(&self, lookup: &str) -> Result<(String, String), MailboxError> {
        let rb = self.bearer(self.http.post(self.url(&format!("/v1/link-codes/{lookup}"))).timeout(REQUEST_TIMEOUT));
        let resp = rb.send().await.map_err(transport)?;
        if !resp.status().is_success() {
            let (status, code, retry) = problem_parts(resp).await;
            // An older token-gated relay answers an unknown route at its
            // access gate, without a problem document.
            if matches!(status, 401 | 403 | 405) && code.is_empty() {
                return Err(MailboxError::NoRoutes);
            }
            return Err(problem_of(status, &code, retry));
        }
        #[derive(Deserialize)]
        struct Resolved {
            mailbox: String,
            blob: String,
        }
        let r: Resolved = Self::json(resp).await?;
        Ok((r.mailbox, r.blob))
    }
}

/// What [`LinkCodeOffer::check`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeState {
    /// Live, waiting to be typed.
    Live,
    /// The relay had forgotten it (it restarted): posted again, the same code.
    Reposted,
    /// The new device typed it: its request should arrive in the mailbox.
    Used,
    /// It lived out its 10 minutes unused.
    Expired,
}

/// A code this device shows for its workspace's approval address.
pub struct LinkCodeOffer {
    code: ShortCode,
    stretched: Stretched,
    keys: MailboxKeys,
    relay: String,
    workspace_id: Uuid,
    expires_at: i64,
    used: bool,
}

impl std::fmt::Debug for LinkCodeOffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkCodeOffer").field("relay", &self.relay).field("expires_at", &self.expires_at).finish_non_exhaustive()
    }
}

fn newest(epochs: &[EpochKey]) -> Result<&EpochKey, MailboxError> {
    epochs.iter().max_by_key(|e| e.epoch).ok_or(MailboxError::Malformed)
}

impl LinkCodeOffer {
    /// Register the workspace's mailbox (idempotent), seal its current
    /// approval address under a fresh code, and post it. A taken code is
    /// replaced by another.
    pub async fn post(client: &MailboxClient, workspace_id: Uuid, founder_pin_hex: &str, epochs: &[EpochKey]) -> Result<Self, MailboxError> {
        let address = current_address(client, workspace_id, founder_pin_hex, epochs).await?;
        let keys = MailboxKeys::derive(&newest(epochs)?.key);
        let mut last = MailboxError::CodeTaken;
        for _ in 0..TRIES {
            let code = ShortCode::generate();
            let stretched = stretch(&code, client.relay()).await;
            let blob = stretched.seal(&address.mailbox, &address.encode());
            match client.put_link_code(&keys, &stretched.lookup_hex(), &blob, LINK_CODE_TTL_SECS).await {
                Ok(info) => {
                    return Ok(Self { code, stretched, keys, relay: client.relay().to_string(), workspace_id, expires_at: info.expires_at, used: info.used })
                }
                Err(MailboxError::CodeTaken) => last = MailboxError::CodeTaken,
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }

    /// The code as typed (`K7Q4MX`); `.shown()` is `K7Q-4MX`.
    pub fn code(&self) -> &ShortCode {
        &self.code
    }

    /// The relay the code is on (the new device types it for this relay).
    pub fn relay(&self) -> &str {
        &self.relay
    }

    pub fn workspace_id(&self) -> Uuid {
        self.workspace_id
    }

    /// Unix ms the code stops working.
    pub fn expires_at(&self) -> i64 {
        self.expires_at
    }

    /// Whether the code is still live, was used or expired. When the relay
    /// forgot it (a restart: `code_unknown` or `mailbox_unknown` before its
    /// expiry), the mailbox is registered again and the same code posted
    /// again for the rest of its life: [`CodeState::Reposted`].
    pub async fn check(&mut self, client: &MailboxClient, founder_pin_hex: &str, epochs: &[EpochKey]) -> Result<CodeState, MailboxError> {
        if self.used {
            return Ok(CodeState::Used);
        }
        let now = client.now();
        if now >= self.expires_at {
            return Ok(CodeState::Expired);
        }
        match client.link_code_status(&self.keys, &self.stretched.lookup_hex()).await {
            Ok(info) if info.used => {
                self.used = true;
                Ok(CodeState::Used)
            }
            Ok(info) => {
                self.expires_at = info.expires_at;
                Ok(CodeState::Live)
            }
            Err(MailboxError::CodeUnknown | MailboxError::MailboxUnknown) => {
                let left = (self.expires_at - now) / 1000;
                if left < MIN_REPOST_SECS {
                    return Ok(CodeState::Expired);
                }
                let address = current_address(client, self.workspace_id, founder_pin_hex, epochs).await?;
                let blob = self.stretched.seal(&address.mailbox, &address.encode());
                let info = client.put_link_code(&self.keys, &self.stretched.lookup_hex(), &blob, left as u64).await?;
                self.expires_at = info.expires_at;
                Ok(CodeState::Reposted)
            }
            Err(e) => Err(e),
        }
    }

    /// Take the code back (the screen closed). Gone already counts.
    pub async fn withdraw(&self, client: &MailboxClient) -> Result<(), MailboxError> {
        match client.delete_link_code(&self.keys, &self.stretched.lookup_hex()).await {
            Ok(()) | Err(MailboxError::MailboxUnknown) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Argon2id takes a moment (64 MiB): off the async workers.
async fn stretch(code: &ShortCode, relay: &str) -> Stretched {
    let (c, r) = (code.clone(), relay.to_string());
    match tokio::task::spawn_blocking(move || c.stretch(&r)).await {
        Ok(s) => s,
        Err(_) => code.stretch(relay),
    }
}

/// The approval address behind `code` on `client`'s relay. The code is used
/// up. The address must open with the code, name the mailbox the relay
/// returned and point at this same relay. `allow_insecure_relay` as for
/// [`ApprovalAddress::decode_with`].
pub async fn resolve(client: &MailboxClient, code: &ShortCode, allow_insecure_relay: bool) -> Result<ApprovalAddress, MailboxError> {
    let stretched = stretch(code, client.relay()).await;
    let (mailbox, blob) = client.resolve_link_code(&stretched.lookup_hex()).await?;
    let mb = super::hex_fixed::<16>(&mailbox).ok_or(MailboxError::Malformed)?;
    let text = stretched.open(&mb, &blob).ok_or(MailboxError::Malformed)?;
    let address = ApprovalAddress::decode_with(&text, allow_insecure_relay).map_err(|_| MailboxError::Malformed)?;
    if address.mailbox != mb || ligus_core::link_code::relay_salt(&address.relay) != ligus_core::link_code::relay_salt(client.relay()) {
        return Err(MailboxError::Malformed);
    }
    Ok(address)
}

/// Whether a requester's poll or put failed because the relay forgot its
/// request or mailbox (a restart): post the request again.
pub fn relay_forgot(e: &MailboxError) -> bool {
    matches!(e, MailboxError::RequestUnknown | MailboxError::MailboxUnknown)
}
