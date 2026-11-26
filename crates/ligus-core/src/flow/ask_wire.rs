//! How [`HumanAsk`] and [`HumanAnswered`] decode, so that 0.3 and 0.4 decode
//! exactly the same payloads (`docs/design/agent-questions.md` 5, 11).
//!
//! A derived decoder refuses a duplicated field. The fields 0.3 knows keep
//! that rule (0.3 refuses their duplicates too), but a duplicated field 0.4
//! added must not make 0.4 refuse what 0.3 accepts (0.3 ignores unknown
//! keys, duplicates included): those take their first value, and a value
//! this build can't read folds as absent.

use serde::de::{DeserializeOwned, Error as _, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};
use uuid::Uuid;

use super::{HumanAnswered, HumanAsk};

/// Every key and value of a map, duplicates kept, in order.
struct Entries(Vec<(String, Value)>);

impl<'de> Deserialize<'de> for Entries {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Entries;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a map")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Entries, A::Error> {
                let mut out = Vec::new();
                while let Some((k, v)) = m.next_entry::<String, Value>()? {
                    out.push((k, v));
                }
                Ok(Entries(out))
            }
        }
        d.deserialize_map(V)
    }
}

/// Split `entries` into the 0.3 fields (a duplicate is an error, as 0.3's
/// derived decoder makes it) and the rest (first value wins).
fn split<E: serde::de::Error>(
    entries: Entries,
    old: &[&'static str],
) -> Result<(Map<String, Value>, Map<String, Value>), E> {
    let mut known = Map::new();
    let mut rest = Map::new();
    for (k, v) in entries.0 {
        if let Some(name) = old.iter().find(|n| **n == k) {
            if known.contains_key(&k) {
                return Err(E::duplicate_field(name));
            }
            known.insert(k, v);
        } else if !rest.contains_key(&k) {
            rest.insert(k, v);
        }
    }
    Ok((known, rest))
}

/// A field 0.4 added: its first value, or its default when this build
/// can't read it.
fn lenient<T: DeserializeOwned + Default>(rest: &Map<String, Value>, key: &str) -> T {
    rest.get(key)
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

/// [`HumanAsk`] exactly as 0.3 decodes it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AskV1 {
    ask_id: Uuid,
    turn_id: Uuid,
    agent_key: String,
    question: String,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    expires_at_ms: i64,
    #[serde(default)]
    closed: bool,
    at_ms: i64,
    #[serde(default)]
    permission: bool,
}

const ASK_V1: [&str; 9] = [
    "askId",
    "turnId",
    "agentKey",
    "question",
    "options",
    "expiresAtMs",
    "closed",
    "atMs",
    "permission",
];

impl<'de> Deserialize<'de> for HumanAsk {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let (known, rest) = split::<D::Error>(Entries::deserialize(d)?, &ASK_V1)?;
        let b: AskV1 = serde_json::from_value(Value::Object(known)).map_err(D::Error::custom)?;
        Ok(HumanAsk {
            ask_id: b.ask_id,
            turn_id: b.turn_id,
            agent_key: b.agent_key,
            question: b.question,
            options: b.options,
            expires_at_ms: b.expires_at_ms,
            closed: b.closed,
            at_ms: b.at_ms,
            permission: b.permission,
            v: lenient(&rest, "v"),
            audience: lenient(&rest, "audience"),
            quorum: lenient(&rest, "quorum"),
            allow_text: lenient(&rest, "allowText"),
            effect: lenient(&rest, "effect"),
            origin: lenient(&rest, "origin"),
            settled_by: lenient(&rest, "settledBy"),
        })
    }
}

/// [`HumanAnswered`] exactly as 0.3 decodes it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AnsweredV1 {
    ask_id: Uuid,
    answer: String,
    #[serde(default)]
    option_index: Option<u32>,
    #[serde(default)]
    by: String,
    at_ms: i64,
}

const ANSWERED_V1: [&str; 5] = ["askId", "answer", "optionIndex", "by", "atMs"];

impl<'de> Deserialize<'de> for HumanAnswered {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let (known, rest) = split::<D::Error>(Entries::deserialize(d)?, &ANSWERED_V1)?;
        let b: AnsweredV1 =
            serde_json::from_value(Value::Object(known)).map_err(D::Error::custom)?;
        Ok(HumanAnswered {
            ask_id: b.ask_id,
            answer: b.answer,
            option_index: b.option_index,
            by: b.by,
            at_ms: b.at_ms,
            text: lenient(&rest, "text"),
            ask_digest: lenient(&rest, "askDigest"),
            decline: lenient(&rest, "decline"),
        })
    }
}
