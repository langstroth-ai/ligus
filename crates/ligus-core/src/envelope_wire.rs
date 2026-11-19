//! Wire form of [`SessionEventEnvelope`]: received bytes are kept.
//!
//! A received envelope remembers the exact JSON text it arrived as. The four
//! signed JSON fields (`scope`, `timestamp`, `actorStamp`, `payload`) are
//! verified over those bytes (see [`crate::crypto::envelope_preimage`]), so a
//! field or event kind this build does not know still verifies, is stored and
//! is re-emitted unchanged to newer peers. Serializing a received envelope
//! writes the received text back verbatim (unless an unsigned routing field was
//! changed in memory, in which case the envelope is rebuilt field by field with
//! the signed fields and unknown keys still copied byte for byte).
//!
//! Invariant: code that changes a *signed* field of a received envelope in
//! memory must call [`SessionEventEnvelope::clear_raw`] first; otherwise the
//! stale received bytes win. No production path does (they only re-stamp
//! envelopes they author).
//!
//! Unknown top-level keys are kept but are **not** covered by the v3
//! signature (it signs only the four fields above plus the routing ids); a
//! future build must not trust a new top-level key without a new preimage.

use std::fmt;
use std::ops::Range;

use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;
use uuid::Uuid;

use crate::events::{EventScope, SessionEvent, SessionEventEnvelope};
use crate::identity::ActorStamp;
use crate::time_util::Timestamp;

/// The bytes an envelope arrived as, and where its signed fields sit in them.
#[derive(Clone)]
pub struct RawSigned {
    whole: Box<RawValue>,
    scope: Option<Range<usize>>,
    timestamp: Option<Range<usize>>,
    actor_stamp: Option<Range<usize>>,
    payload: Range<usize>,
    /// Top-level keys this build does not know, in received order.
    extra: Vec<(String, Range<usize>)>,
    /// The unsigned routing fields as parsed, to notice an in-memory change.
    routing: Routing,
}

#[derive(Clone, PartialEq)]
struct Routing {
    id: Uuid,
    event_id: Uuid,
    session_id: Uuid,
    workspace_id: Uuid,
    sequence: i64,
    lamport: u64,
    signer_device_id: Option<Uuid>,
    signature: Option<Vec<u8>>,
}

impl Routing {
    fn of(env: &SessionEventEnvelope) -> Self {
        Self {
            id: env.id,
            event_id: env.event_id,
            session_id: env.session_id,
            workspace_id: env.workspace_id,
            sequence: env.sequence,
            lamport: env.lamport,
            signer_device_id: env.signer_device_id,
            signature: env.signature.clone(),
        }
    }
}

impl fmt::Debug for RawSigned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawSigned").field("bytes", &self.whole.get().len()).field("extra", &self.extra_keys()).finish()
    }
}

impl RawSigned {
    fn slice(&self, r: &Range<usize>) -> &str {
        &self.whole.get()[r.clone()]
    }
    /// The whole envelope exactly as received.
    pub fn text(&self) -> &str {
        self.whole.get()
    }
    pub fn scope(&self) -> Option<&str> {
        self.scope.as_ref().map(|r| self.slice(r))
    }
    pub fn timestamp(&self) -> Option<&str> {
        self.timestamp.as_ref().map(|r| self.slice(r))
    }
    pub fn actor_stamp(&self) -> Option<&str> {
        self.actor_stamp.as_ref().map(|r| self.slice(r))
    }
    pub fn payload(&self) -> &str {
        self.slice(&self.payload)
    }
    /// Top-level keys this build does not know.
    pub fn extra_keys(&self) -> Vec<&str> {
        self.extra.iter().map(|(k, _)| k.as_str()).collect()
    }
    /// The raw JSON of an unknown top-level key.
    pub fn extra(&self, key: &str) -> Option<&str> {
        self.extra.iter().find(|(k, _)| k == key).map(|(_, r)| self.slice(r))
    }
}

const KNOWN: [&str; 12] = [
    "id",
    "eventId",
    "sessionId",
    "workspaceId",
    "sequence",
    "lamport",
    "timestamp",
    "actorStamp",
    "payload",
    "scope",
    "signerDeviceId",
    "signature",
];

/// Most top-level keys an envelope may carry (known and unknown).
pub const MAX_TOP_LEVEL_KEYS: usize = 64;

/// Top-level keys of one envelope object, borrowed from its text.
struct Fields<'a>(Vec<(&'a str, &'a RawValue)>);

impl<'de: 'a, 'a> Deserialize<'de> for Fields<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<'a>(std::marker::PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = Fields<'a>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an event envelope object")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut out: Vec<(&'a str, &'a RawValue)> = Vec::new();
                while let Some(key) = map.next_key::<&'de str>()? {
                    // Bounded: twelve known keys plus room for future ones. A
                    // key-stuffed envelope would otherwise cost quadratic time
                    // here on every load of its log.
                    if out.len() >= MAX_TOP_LEVEL_KEYS {
                        return Err(de::Error::custom("too many envelope keys"));
                    }
                    if out.iter().any(|(k, _)| *k == key) {
                        // Two values for one key would let the verifier and the
                        // fold read different ones.
                        return Err(de::Error::custom(format!("duplicate envelope key `{key}`")));
                    }
                    let value: &'de RawValue = map.next_value()?;
                    out.push((key, value));
                }
                Ok(Fields(out))
            }
        }
        d.deserialize_map(V(std::marker::PhantomData))
    }
}

fn offset(whole: &str, part: &str) -> Range<usize> {
    let start = part.as_ptr() as usize - whole.as_ptr() as usize;
    start..start + part.len()
}

fn parse<T: serde::de::DeserializeOwned, E: de::Error>(key: &str, raw: &RawValue) -> Result<T, E> {
    serde_json::from_str(raw.get()).map_err(|e| E::custom(format!("envelope `{key}`: {e}")))
}

impl<'de> Deserialize<'de> for SessionEventEnvelope {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let whole = Box::<RawValue>::deserialize(d)?;
        from_raw(whole).map_err(de::Error::custom)
    }
}

fn from_raw(whole: Box<RawValue>) -> Result<SessionEventEnvelope, serde_json::Error> {
    use serde::de::Error as _;
    let text = whole.get();
    let Fields(fields) = serde_json::from_str::<Fields<'_>>(text)?;
    let get = |key: &str| fields.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
    let need = |key: &str| get(key).ok_or_else(|| serde_json::Error::missing_field(leak(key)));

    let id: Uuid = parse("id", need("id")?)?;
    let event_id: Uuid = parse("eventId", need("eventId")?)?;
    let session_id: Uuid = parse("sessionId", need("sessionId")?)?;
    let workspace_id: Uuid = parse("workspaceId", need("workspaceId")?)?;
    let sequence: i64 = parse("sequence", need("sequence")?)?;
    let lamport: u64 = match get("lamport") {
        Some(v) => parse("lamport", v)?,
        None => 0,
    };
    let timestamp: Timestamp = match get("timestamp") {
        Some(v) => parse("timestamp", v)?,
        // Legacy: signed over the defaulted value, as before.
        None => Timestamp::default(),
    };
    let actor_stamp: Option<ActorStamp> = match get("actorStamp") {
        Some(v) => parse("actorStamp", v)?,
        None => None,
    };
    let payload_raw = need("payload")?;
    // A kind this build knows but cannot parse (a newer build widened it) is
    // inert here, like an unknown kind; the bytes are kept either way.
    let payload: SessionEvent = serde_json::from_str(payload_raw.get()).unwrap_or(SessionEvent::Unknown);
    let scope: EventScope = match get("scope") {
        Some(v) => parse("scope", v)?,
        None => EventScope::Session,
    };
    let signer_device_id: Option<Uuid> = match get("signerDeviceId") {
        Some(v) => parse("signerDeviceId", v)?,
        None => None,
    };
    let signature: Option<Vec<u8>> = match get("signature") {
        Some(v) => parse("signature", v)?,
        None => None,
    };

    let range = |key: &str| get(key).map(|v| offset(text, v.get()));
    let scope_r = range("scope");
    let timestamp_r = range("timestamp");
    let actor_r = range("actorStamp");
    let payload_r = offset(text, payload_raw.get());
    let extra: Vec<(String, Range<usize>)> = fields
        .iter()
        .filter(|(k, _)| !KNOWN.contains(k))
        .map(|(k, v)| (k.to_string(), offset(text, v.get())))
        .collect();

    let mut env = SessionEventEnvelope {
        id,
        event_id,
        session_id,
        workspace_id,
        sequence,
        lamport,
        timestamp,
        actor_stamp,
        payload,
        scope,
        signer_device_id,
        signature,
        raw: None,
        sealed_epoch: None,
    };
    let routing = Routing::of(&env);
    env.raw = Some(RawSigned {
        whole,
        scope: scope_r,
        timestamp: timestamp_r,
        actor_stamp: actor_r,
        payload: payload_r,
        extra,
        routing,
    });
    Ok(env)
}

/// `missing_field` wants a `&'static str`; the keys are the fixed names above.
fn leak(key: &str) -> &'static str {
    KNOWN.iter().find(|k| **k == key).copied().unwrap_or("field")
}

/// A JSON fragment written verbatim (a received signed field or unknown key).
struct Verbatim<'a>(&'a str);

impl Serialize for Verbatim<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let raw: &RawValue = serde_json::from_str(self.0).map_err(serde::ser::Error::custom)?;
        raw.serialize(s)
    }
}

impl Serialize for SessionEventEnvelope {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if let Some(raw) = &self.raw {
            if raw.routing == Routing::of(self) {
                // Untouched since it arrived: the exact received text.
                return Verbatim(raw.text()).serialize(s);
            }
        }
        let raw = self.raw.as_ref();
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("id", &self.id)?;
        m.serialize_entry("eventId", &self.event_id)?;
        m.serialize_entry("sessionId", &self.session_id)?;
        m.serialize_entry("workspaceId", &self.workspace_id)?;
        m.serialize_entry("sequence", &self.sequence)?;
        m.serialize_entry("lamport", &self.lamport)?;
        match raw.and_then(RawSigned::timestamp) {
            Some(t) => m.serialize_entry("timestamp", &Verbatim(t))?,
            None => m.serialize_entry("timestamp", &self.timestamp)?,
        }
        match raw.and_then(RawSigned::actor_stamp) {
            Some(a) => m.serialize_entry("actorStamp", &Verbatim(a))?,
            None => {
                if let Some(a) = &self.actor_stamp {
                    m.serialize_entry("actorStamp", a)?;
                }
            }
        }
        match raw {
            Some(r) => m.serialize_entry("payload", &Verbatim(r.payload()))?,
            None => m.serialize_entry("payload", &self.payload)?,
        }
        match raw.and_then(RawSigned::scope) {
            Some(sc) => m.serialize_entry("scope", &Verbatim(sc))?,
            None => m.serialize_entry("scope", &self.scope)?,
        }
        if let Some(d) = &self.signer_device_id {
            m.serialize_entry("signerDeviceId", d)?;
        }
        if let Some(sig) = &self.signature {
            m.serialize_entry("signature", sig)?;
        }
        if let Some(r) = raw {
            for (k, range) in &r.extra {
                m.serialize_entry(k, &Verbatim(r.slice(range)))?;
            }
        }
        m.end()
    }
}

impl SessionEventEnvelope {
    /// The bytes this envelope was received as, if it was parsed rather than
    /// authored here.
    pub fn raw(&self) -> Option<&RawSigned> {
        self.raw.as_ref()
    }

    /// Forget the received bytes, so serialization and the signature preimage
    /// use the typed fields. Required before changing a signed field of a
    /// received envelope in memory.
    pub fn clear_raw(&mut self) {
        self.raw = None;
    }

    /// The relay key epoch this device received the envelope sealed under
    /// (Phase 0, B1), recorded at ingest. Device-local and unsigned: never
    /// serialized, `None` for events authored here or received unsealed.
    pub fn sealed_epoch(&self) -> Option<u32> {
        self.sealed_epoch
    }

    pub fn set_sealed_epoch(&mut self, epoch: Option<u32>) {
        self.sealed_epoch = epoch;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_keys_are_rejected() {
        let env = SessionEventEnvelope::new(Uuid::nil(), Uuid::nil(), 1, SessionEvent::MessageRemoved { message_id: Uuid::nil() });
        let json = serde_json::to_string(&env).unwrap();
        let dup = json.replacen("{", "{\"payload\":{\"kind\":\"unknown\"},", 1);
        assert!(serde_json::from_str::<SessionEventEnvelope>(&dup).is_err());
    }

    /// Hardening A-8: an envelope stuffed with unknown top-level keys (a few
    /// hundred KB from any relay poster) must not cost quadratic time to
    /// parse; past a small bound of keys it is refused outright.
    #[test]
    fn many_top_level_keys_are_refused_fast() {
        let env = SessionEventEnvelope::new(Uuid::nil(), Uuid::nil(), 1, SessionEvent::MessageRemoved { message_id: Uuid::nil() });
        let json = serde_json::to_string(&env).unwrap();
        let mut pad = String::from("{");
        for i in 0..60_000 {
            pad.push_str(&format!("\"k{i}\":0,"));
        }
        let stuffed = json.replacen("{", &pad, 1);
        let t = std::time::Instant::now();
        let r = serde_json::from_str::<SessionEventEnvelope>(&stuffed);
        let took = t.elapsed();
        assert!(r.is_err(), "an envelope with 60k unknown keys was accepted");
        assert!(took < std::time::Duration::from_millis(500), "parsing took {took:?}");
    }

    #[test]
    fn received_text_round_trips_exactly() {
        let env = SessionEventEnvelope::new(Uuid::nil(), Uuid::nil(), 1, SessionEvent::MessageRemoved { message_id: Uuid::nil() });
        let json = serde_json::to_string(&env).unwrap();
        let odd = json.replacen("{", "{ \"zFuture\" : [1, {\"a\":2}] ,", 1);
        let back: SessionEventEnvelope = serde_json::from_str(&odd).unwrap();
        assert_eq!(serde_json::to_string(&back).unwrap(), odd);
        assert_eq!(back.raw().unwrap().extra_keys(), vec!["zFuture"]);
        // A changed routing field rebuilds the object but keeps the unknown key.
        let mut moved = back.clone();
        moved.sequence = 9;
        let re = serde_json::to_string(&moved).unwrap();
        assert!(re.contains("\"zFuture\":[1, {\"a\":2}]"), "{re}");
        assert!(re.contains("\"sequence\":9"));
        // Also through serde_json::Value.
        let v = serde_json::to_value(&back).unwrap();
        assert_eq!(v["zFuture"][1]["a"], 2);
    }
}
