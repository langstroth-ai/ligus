//! The approval address (`ligusaddr1:`, wire note section 2).

use ligus_core::match_code::{hex, put_cap, MailboxKeys};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{b64, hex_fixed};

/// Prefix of an approval address.
pub const ADDRESS_PREFIX: &str = "ligusaddr1:";
/// The longest address read.
const MAX_ADDRESS_LEN: usize = 4096;

/// Why an address was refused. The text is what a person reads.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum AddressError {
    #[error("not a Ligus approval address (expected {ADDRESS_PREFIX}...)")]
    NotAnAddress,
    #[error("the approval address is damaged or incomplete")]
    Malformed,
    #[error("approval address version {0} isn't supported here; update Ligus")]
    Version(u32),
    #[error("the approval address's relay isn't an https:// address")]
    InsecureRelay,
}

/// Where a workspace's approval requests go (design note 5.1). Not a secret
/// and grants nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalAddress {
    /// The relay base URL (no trailing slash).
    pub relay: String,
    pub workspace_id: Uuid,
    /// `FounderPin::fingerprint_hex` of the founder's account key (32 lowercase hex).
    pub founder_pin: String,
    /// The key epoch the mailbox keys were derived from.
    pub epoch: u32,
    pub mailbox: [u8; 16],
    /// The X25519 public key requests are sealed to.
    pub seal_key: [u8; 32],
    /// The put capability at the relay's current counter.
    pub put_cap: [u8; 16],
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Wire {
    v: u32,
    relay: String,
    workspace_id: Uuid,
    founder_pin: String,
    epoch: u32,
    mailbox: String,
    seal_key: String,
    put_cap: String,
}

impl ApprovalAddress {
    /// The address every member derives from epoch `epoch`'s 32-byte key and
    /// the relay's reset `counter` ([`super::MailboxClient::ensure_registered`]).
    pub fn derive(relay: &str, workspace_id: Uuid, founder_pin_hex: &str, epoch: u32, epoch_key: &[u8; 32], counter: u32) -> Self {
        let keys = MailboxKeys::derive(epoch_key);
        Self {
            relay: relay.trim().trim_end_matches('/').to_string(),
            workspace_id,
            founder_pin: founder_pin_hex.trim().to_ascii_lowercase(),
            epoch,
            mailbox: keys.mailbox(),
            seal_key: keys.seal().public_key_bytes(),
            put_cap: put_cap(epoch_key, counter),
        }
    }

    /// The mailbox id as it appears in URLs (32 hex).
    pub fn mailbox_hex(&self) -> String {
        hex(&self.mailbox)
    }

    /// `ligusaddr1:<b64u(JSON)>`.
    pub fn encode(&self) -> String {
        let w = Wire {
            v: 1,
            relay: self.relay.clone(),
            workspace_id: self.workspace_id,
            founder_pin: self.founder_pin.clone(),
            epoch: self.epoch,
            mailbox: self.mailbox_hex(),
            seal_key: b64::enc(&self.seal_key),
            put_cap: b64::enc(&self.put_cap),
        };
        format!("{ADDRESS_PREFIX}{}", b64::enc(&serde_json::to_vec(&w).expect("serialize an address")))
    }

    /// Parse an address, refusing a non-`https` relay.
    pub fn decode(text: &str) -> Result<Self, AddressError> {
        Self::decode_with(text, false)
    }

    /// [`Self::decode`], also accepting an `http://` relay (tests, and builds
    /// that allow insecure relays).
    pub fn decode_with(text: &str, allow_insecure_relay: bool) -> Result<Self, AddressError> {
        let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.len() > MAX_ADDRESS_LEN {
            return Err(AddressError::Malformed);
        }
        let body = compact.strip_prefix(ADDRESS_PREFIX).ok_or(AddressError::NotAnAddress)?;
        let bytes = b64::dec(body).ok_or(AddressError::Malformed)?;
        let w: Wire = serde_json::from_slice(&bytes).map_err(|_| AddressError::Malformed)?;
        if w.v != 1 {
            return Err(AddressError::Version(w.v));
        }
        let url = url::Url::parse(w.relay.trim()).map_err(|_| AddressError::Malformed)?;
        if url.host_str().is_none_or(str::is_empty) {
            return Err(AddressError::Malformed);
        }
        match url.scheme() {
            "https" => {}
            "http" if allow_insecure_relay => {}
            "http" => return Err(AddressError::InsecureRelay),
            _ => return Err(AddressError::Malformed),
        }
        let founder_pin = w.founder_pin.to_ascii_lowercase();
        hex_fixed::<16>(&founder_pin).ok_or(AddressError::Malformed)?;
        Ok(Self {
            relay: w.relay.trim().trim_end_matches('/').to_string(),
            workspace_id: w.workspace_id,
            founder_pin,
            epoch: w.epoch,
            mailbox: hex_fixed::<16>(&w.mailbox.to_ascii_lowercase()).ok_or(AddressError::Malformed)?,
            seal_key: b64::fixed::<32>(&w.seal_key).ok_or(AddressError::Malformed)?,
            put_cap: b64::fixed::<16>(&w.put_cap).ok_or(AddressError::Malformed)?,
        })
    }
}
