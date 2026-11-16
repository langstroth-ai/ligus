//! People's presence (online + "typing in this chat") over the relay's
//! ephemeral presence channel.
//!
//! A person's device (the desktop, or a `ligus serve` box relaying its
//! phone's typing) overwrites one slot, `human:<device_id>`, every few seconds
//! while they type. The blob is never part of the event log.
//!
//! It used to be plaintext JSON (`actorId`, `name`, `sessionId`, `typing`), so
//! the relay could read who was typing in which chat. It is now sealed under
//! the keyring's seal key (the authorized head, B1) exactly like host and lease
//! beats ([`crate::host_presence`], [`crate::lease_presence`]): the same
//! `SealedEnvelope` (ChaCha20-Poly1305 under the workspace epoch key, no new
//! domain string). The relay sees only the slot name (`human:` and a device
//! id, which it already learns from the writer's auth) and ciphertext. The
//! `/v1/workspaces/{ws}/presence` route and its `{device_id, data}` body are
//! unchanged; `data` is opaque to the relay.
//!
//! Readers accept only blobs sealed at the authorized head, whose slot matches
//! the device they name, stamped no further than [`MAX_SKEW_MS`] ahead and no
//! older than [`PRESENCE_TTL_MS`]. Plaintext blobs (an older desktop) are
//! ignored: accepting them would let the relay inject "X is typing" with any
//! name. Beats are not signed: a current member could forge another member's
//! typing indicator, which is cosmetic (no authority rides on it); readers
//! prefer the display name from their own roster over the blob's.
//!
//! A beat never carries message text.

use std::collections::HashMap;

use hive_core::{open_symmetric, seal_symmetric_kid, SealedEnvelope};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::keyring::KeyringEngine;
use crate::relay_client::{RelayClient, RelayError};

/// A beat counts as live for this long after its stamp.
pub const PRESENCE_TTL_MS: i64 = 8_000;
/// How far ahead of a reader's clock a beat may be stamped and still count.
pub const MAX_SKEW_MS: i64 = 30_000;
/// The longest display name a beat may carry (characters, after cleaning).
pub const MAX_NAME_CHARS: usize = 64;

/// The presence slot one device's beat lives in.
pub fn human_presence_slot(device_id: &str) -> String {
    format!("human:{device_id}")
}

/// One device's presence: which person, and whether they're typing where.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HumanBeat {
    /// The person (actor id) the device speaks for: the device's own author,
    /// or for an enrolled box, its owner (as its messages are attributed).
    pub actor_id: String,
    /// Display name as the writer knows it (readers prefer their roster's).
    pub name: String,
    /// The writing device; must match the slot.
    pub device_id: String,
    /// The chat they're in ("" when only online).
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub typing: bool,
    /// Unix milliseconds.
    pub ts: i64,
}

/// Seal a beat under `ring`'s seal key, as a presence blob. `None` when this
/// device may not seal (no workspace key): presence is then not published at
/// all, never in the clear.
pub fn seal_human_beat(ring: &KeyringEngine, beat: &HumanBeat) -> Option<Value> {
    let k = ring.seal_key()?;
    let plain = serde_json::to_vec(beat).ok()?;
    let sealed = seal_symmetric_kid(&k.key, k.version, &k.kid, &plain).ok()?;
    Some(serde_json::json!({ "human": sealed }))
}

/// Open every live human beat in a presence listing (`slot` → blob). Skips
/// host/lease beats, plaintext blobs, blobs under another key or an epoch
/// older than the authorized head, beats in another device's slot, stale beats
/// and beats from too far in the future. Names come back cleaned.
pub fn open_human_beats(blobs: &HashMap<String, Value>, ring: &KeyringEngine, now_ms: i64) -> Vec<HumanBeat> {
    let mut out = Vec::new();
    let floor = ring.authorized_head();
    for (slot, blob) in blobs {
        let Some(sealed) = blob.get("human") else { continue };
        let Ok(sealed) = serde_json::from_value::<SealedEnvelope>(sealed.clone()) else { continue };
        if sealed.version < floor {
            continue;
        }
        let Some(plain) =
            ring.keys_for(sealed.version, sealed.kid.as_deref()).into_iter().find_map(|k| open_symmetric(k, &sealed).ok())
        else {
            continue;
        };
        let Ok(mut beat) = serde_json::from_slice::<HumanBeat>(&plain) else { continue };
        if beat.actor_id.is_empty() || *slot != human_presence_slot(&beat.device_id) {
            continue;
        }
        if beat.ts > now_ms.saturating_add(MAX_SKEW_MS) || now_ms.saturating_sub(beat.ts) > PRESENCE_TTL_MS {
            continue;
        }
        beat.ts = beat.ts.min(now_ms);
        beat.name = clean_display_name(&beat.name);
        out.push(beat);
    }
    out.sort_by(|a, b| a.actor_id.cmp(&b.actor_id).then(a.device_id.cmp(&b.device_id)));
    out
}

/// Who is typing in `session_id`, one entry per person (several devices of
/// one person collapse), excluding `me`. `(actor_id, name)`, sorted by name.
pub fn typing_in(beats: &[HumanBeat], session_id: &str, me: &str) -> Vec<(String, String)> {
    let mut seen: HashMap<&str, &str> = HashMap::new();
    for b in beats {
        if b.typing && b.session_id == session_id && b.actor_id != me {
            seen.entry(&b.actor_id).or_insert(&b.name);
        }
    }
    let mut out: Vec<(String, String)> = seen.into_iter().map(|(a, n)| (a.to_string(), n.to_string())).collect();
    out.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()).then(a.0.cmp(&b.0)));
    out
}

/// A display name safe to put in a label: control, format (bidi overrides,
/// zero-width) and line-break characters dropped, whitespace collapsed,
/// capped at [`MAX_NAME_CHARS`].
pub fn clean_display_name(raw: &str) -> String {
    let filtered: String = raw
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|c| !c.is_control() && !is_format_char(*c))
        .collect();
    let collapsed = filtered.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(MAX_NAME_CHARS).collect::<String>().trim_end().to_string()
}

/// Unicode "format" characters a name has no business carrying: bidi
/// embeddings/overrides/isolates, zero-width joiners and spaces, the BOM.
fn is_format_char(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{206F}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}')
}

/// Publish a beat to `room`'s presence channel (a no-op without a seal key).
pub async fn publish_human_beat(relay: &RelayClient, room: &str, ring: &KeyringEngine, beat: &HumanBeat) -> Result<(), RelayError> {
    let Some(blob) = seal_human_beat(ring, beat) else { return Ok(()) };
    relay.publish_presence(room, &human_presence_slot(&beat.device_id), &blob).await
}

/// Read every live human beat in `room`.
pub async fn fetch_human_beats(relay: &RelayClient, room: &str, ring: &KeyringEngine, now_ms: i64) -> Result<Vec<HumanBeat>, RelayError> {
    let blobs = relay.list_presence(room).await?;
    Ok(open_human_beats(&blobs, ring, now_ms))
}

/// Unix milliseconds now.
pub fn now_ms() -> i64 {
    (hive_core::Timestamp::now().inner().unix_timestamp_nanos() / 1_000_000) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> KeyringEngine {
        KeyringEngine::passphrase(uuid::Uuid::nil(), hive_core::derive_workspace_key(s))
    }

    fn beat(actor: &str, device: &str, session: &str, typing: bool, ts: i64) -> HumanBeat {
        HumanBeat { actor_id: actor.into(), name: format!("Name {actor}"), device_id: device.into(), session_id: session.into(), typing, ts }
    }

    #[test]
    fn round_trip_and_the_relay_sees_only_ciphertext() {
        let k = key("room secret");
        let b = beat("actor-q7", "dev-1", "chat-secret-id", true, 10_000);
        let blob = seal_human_beat(&k, &b).unwrap();
        let raw = serde_json::to_string(&blob).unwrap();
        for leak in ["actor-q7", "Name", "chat-secret-id", "typing", "actorId", "sessionId", "dev-1"] {
            assert!(!raw.contains(leak), "the relay could read {leak:?} in {raw}");
        }
        let mut blobs = HashMap::new();
        blobs.insert(human_presence_slot("dev-1"), blob);
        assert_eq!(open_human_beats(&blobs, &k, 10_500), vec![b]);
    }

    #[test]
    fn ignores_plaintext_foreign_keys_wrong_slots_and_stale_or_future_beats() {
        let k = key("room secret");
        let now = 100_000;
        let mut blobs = HashMap::new();
        // An older desktop's plaintext blob (or a relay forging one).
        blobs.insert("ada".into(), serde_json::json!({ "actorId": "ada", "name": "Ada", "sessionId": "s", "typing": true, "ts": now }));
        // Another workspace's key.
        blobs.insert(human_presence_slot("x"), seal_human_beat(&key("other"), &beat("x", "x", "s", true, now)).unwrap());
        // A beat claiming another device's slot.
        blobs.insert(human_presence_slot("victim"), seal_human_beat(&k, &beat("eve", "eve-dev", "s", true, now)).unwrap());
        // Stale and far-future.
        blobs.insert(human_presence_slot("old"), seal_human_beat(&k, &beat("old", "old", "s", true, now - PRESENCE_TTL_MS - 1)).unwrap());
        blobs.insert(human_presence_slot("fut"), seal_human_beat(&k, &beat("fut", "fut", "s", true, now + MAX_SKEW_MS + 1)).unwrap());
        // A host beat is not a human beat.
        blobs.insert("host:box".into(), crate::host_presence::seal_host_beat(&k, "box", 1).unwrap());
        // The one good beat (slightly in the future: clamped).
        blobs.insert(human_presence_slot("bob-dev"), seal_human_beat(&k, &beat("bob", "bob-dev", "s", true, now + 500)).unwrap());
        let got = open_human_beats(&blobs, &k, now);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].actor_id, "bob");
        assert_eq!(got[0].ts, now);
    }

    #[test]
    fn typing_collapses_devices_and_excludes_me_and_other_chats() {
        let beats = vec![
            beat("ada", "a1", "s", true, 1),
            beat("ada", "a2", "s", true, 1),
            beat("bob", "b1", "other", true, 1),
            beat("cy", "c1", "s", false, 1),
            beat("me", "m1", "s", true, 1),
        ];
        assert_eq!(typing_in(&beats, "s", "me"), vec![("ada".to_string(), "Name ada".to_string())]);
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_display_name("  Ada\u{202E}evil\n\tLove\u{200B}lace  "), "Adaevil Lovelace");
        assert_eq!(clean_display_name(&"x".repeat(200)).chars().count(), MAX_NAME_CHARS);
        assert_eq!(clean_display_name("\u{0007}"), "");
    }

    #[test]
    fn no_seal_key_publishes_nothing() {
        let empty = KeyringEngine::default();
        assert!(seal_human_beat(&empty, &beat("a", "d", "s", true, 1)).is_none());
    }

    #[tokio::test]
    async fn publish_and_fetch_through_the_relay() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, crate::test_relay::router()).await.unwrap() });
        let relay = RelayClient::new(format!("http://{addr}"));
        let k = key("room secret");
        let b = beat("ada", "d1", "s", true, now_ms());
        publish_human_beat(&relay, "room-a", &k, &b).await.unwrap();
        // What the relay stores is opaque.
        let raw = serde_json::to_string(&relay.list_presence("room-a").await.unwrap()).unwrap();
        assert!(!raw.contains("ada") && !raw.contains("\"s\""));
        let got = fetch_human_beats(&relay, "room-a", &k, now_ms()).await.unwrap();
        assert_eq!(got, vec![b]);
        assert!(fetch_human_beats(&relay, "room-b", &k, now_ms()).await.unwrap().is_empty());
    }
}
