//! What an agent runs and where it may run (workspace model v3, 5.2, 5.3
//! and 5.6): the sticky `run` and `placement` fields of a
//! [`WorkspaceAgent`], their link to the legacy `runtime_id` and `host_id`,
//! and the merge an upsert goes through in the fold.
//!
//! Both fields are additive and optional on the wire. An older client that
//! edits an agent sends the whole record without them, so the fold merges
//! every upsert with the record it replaces:
//!
//! 1. A sticky field absent from an upsert keeps its previous value.
//! 2. When the upsert's legacy field disagrees with the sticky field it
//!    shadows, **the legacy field wins** and the sticky field is reset to
//!    what the legacy field means. An older writer that moved an agent
//!    therefore moves it for everyone.
//! 3. Clearing is explicit (`placement: {kind: ownerDevices}`, `model: ""`).
//! 4. A removal, and a whole-list roster, start the record over.
//!
//! Authz judges the merged record ([`merge_upsert`]), never the payload
//! alone: any change of the effective host, placement, runtime or run
//! source is a move ([`WorkspaceAgent::moves_from`]).
//!
//! Under rules 3 a run spec is a rules-4 feature: `run` only mirrors the
//! runtime (`{source: "runtime:<id>"}` plus a display hint) and the
//! placement only mirrors `host_id` ([`RunRules::Mirror`]). The fold uses
//! that mode; [`RunRules::Full`] is the rules-4 merge, wired into the fold
//! with rules 4.

use base64::Engine as _;
use serde::{Deserialize, Deserializer, Serialize};

use crate::agent::WorkspaceAgent;

/// The prefix of a run source that names a workspace (or host-local)
/// runtime by id: `runtime:<id>`.
pub const RUNTIME_SOURCE_PREFIX: &str = "runtime:";
/// The prefix of a legacy id that encodes a full run spec:
/// `run:` + base64url of `{"s": source, "m": model}`.
pub const RUN_ID_PREFIX: &str = "run:";

/// What an agent asks for: a source a host offers by name, the harness that
/// drives it, and a model (`""` = the source's default).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRun {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub harness: String,
    #[serde(default)]
    pub source: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// What the writing device resolved a `runtime:<id>` source to (display
    /// only; never used to route or to authorize).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved: Option<ResolvedRun>,
}

/// A display hint: the harness and model a runtime resolved to on the
/// device that wrote it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedRun {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub harness: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
}

impl AgentRun {
    /// The run that mirrors runtime `id` (rules 3): `{source: "runtime:<id>"}`.
    pub fn mirror(runtime_id: &str, resolved: Option<ResolvedRun>) -> Self {
        Self { harness: String::new(), source: format!("{RUNTIME_SOURCE_PREFIX}{runtime_id}"), model: String::new(), resolved }
    }

    /// The runtime id a `runtime:<id>` source names.
    pub fn runtime_ref(&self) -> Option<&str> {
        self.source.strip_prefix(RUNTIME_SOURCE_PREFIX)
    }

    /// The harness, source and model, without the display hint: what
    /// routing and authz compare.
    fn spec(&self) -> (&str, &str, &str) {
        (&self.harness, &self.source, &self.model)
    }
}

/// Where an agent may run (5.3).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Placement {
    /// Any online device of the agent's owner (`host_id = ""`).
    OwnerDevices,
    /// Exactly that host (`host_id = id`).
    Host { host_id: String },
    /// Any online worker offering the source (rules 4 only).
    Pool,
    /// The chat creator's devices (the default agent only, rules 4).
    ChatCreator,
    /// A kind this build doesn't know (a later build's). Never routes
    /// anywhere; the fold resets it to what `host_id` means.
    #[serde(other)]
    Unknown,
}

impl Placement {
    /// The placement `host_id` means for an older writer: `""` is the
    /// owner's devices, anything else that host.
    pub fn mirror(host_id: &str) -> Self {
        if host_id.is_empty() {
            Placement::OwnerDevices
        } else {
            Placement::Host { host_id: host_id.to_string() }
        }
    }

    /// The host this placement names, if it names one.
    pub fn host(&self) -> Option<&str> {
        match self {
            Placement::Host { host_id } => Some(host_id),
            _ => None,
        }
    }
}

/// Which merge the fold applies to an upsert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunRules {
    /// Rules 3 and below: `run` and `placement` only mirror `runtime_id`
    /// and `host_id`.
    Mirror,
    /// Rules 4: full run specs, `pool` and `chatCreator` placements.
    Full,
}

/// The canonical JSON a full run spec's legacy id encodes.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpecKey {
    s: String,
    m: String,
}

/// The `runtime_id` older builds see for `run` (5.2): `runtime:<id>` gives
/// `<id>`; any other spec gives `run:` + base64url (no padding) of the
/// canonical JSON `{"s": source, "m": model}`. Exact and reversible
/// ([`from_legacy_id`]); model names with `:` or `/` round-trip.
pub fn legacy_id(run: &AgentRun) -> String {
    if let Some(id) = run.runtime_ref() {
        return id.to_string();
    }
    let key = serde_json::to_vec(&SpecKey { s: run.source.clone(), m: run.model.clone() }).unwrap_or_default();
    format!("{RUN_ID_PREFIX}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key))
}

/// The run a legacy `runtime_id` means: a `run:` id that decodes to a
/// canonical spec gives that spec (no harness: it isn't part of the id);
/// anything else is a runtime reference, `runtime:<id>`. Always
/// `legacy_id(&from_legacy_id(id)) == id`.
pub fn from_legacy_id(id: &str) -> AgentRun {
    if let Some(spec) = decode_spec(id) {
        return spec;
    }
    AgentRun::mirror(id, None)
}

fn decode_spec(id: &str) -> Option<AgentRun> {
    let b64 = id.strip_prefix(RUN_ID_PREFIX)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(b64).ok()?;
    let key: SpecKey = serde_json::from_slice(&bytes).ok()?;
    // A spec naming a runtime is written as that runtime's id, never as a
    // `run:` id.
    if key.s.starts_with(RUNTIME_SOURCE_PREFIX) {
        return None;
    }
    let run = AgentRun { harness: String::new(), source: key.s, model: key.m, resolved: None };
    // Only the canonical encoding counts, so the map is exact both ways.
    (legacy_id(&run) == id).then_some(run)
}

/// Whether `id` is a legacy id that encodes a full run spec.
pub fn is_full_spec_id(id: &str) -> bool {
    decode_spec(id).is_some()
}

/// `run` reconciled with the upsert's `runtime_id` (rule 2): kept when it
/// agrees, else reset to what `runtime_id` means.
fn reconcile_run(run: AgentRun, runtime_id: &str, rules: RunRules) -> AgentRun {
    match rules {
        RunRules::Mirror => {
            // Only the mirror, whatever the writer sent; its display hint is
            // kept only when it was written for this runtime.
            let hint = (run.runtime_ref() == Some(runtime_id)).then_some(run.resolved).flatten();
            AgentRun::mirror(runtime_id, hint)
        }
        RunRules::Full => {
            if legacy_id(&run) == runtime_id {
                return run;
            }
            let mut fresh = from_legacy_id(runtime_id);
            if fresh.source == run.source {
                fresh.harness = run.harness;
            }
            fresh
        }
    }
}

/// Whether `placement` agrees with the upsert's `host_id` (rule 2).
/// `explicit`: the upsert itself carries the placement (else it is the
/// previous record's); `prev_host`: the previous record's `host_id`.
fn placement_agrees(placement: &Placement, host_id: &str, explicit: bool, prev_host: Option<&str>, rules: RunRules) -> bool {
    match (placement, rules) {
        (Placement::OwnerDevices, _) => host_id.is_empty(),
        (Placement::Host { host_id: h }, _) => h == host_id && !h.is_empty(),
        // A pool writes its first host at write time, so any host agrees
        // with a pool the upsert names; one it only inherits agrees while
        // an older writer leaves `host_id` as it was.
        (Placement::Pool, RunRules::Full) => explicit || prev_host == Some(host_id),
        (Placement::ChatCreator, RunRules::Full) => host_id.is_empty(),
        _ => false,
    }
}

/// The record an upsert of `new` leaves in place of `prev` (rules 1 and 2
/// of 5.6). Its `runtime_id` and `host_id` are always the upsert's (older
/// builds read only those), and its `run` and `placement` always agree with
/// them. `host_gone` is the fold's, never the writer's, and starts clear.
pub fn merge_upsert(prev: Option<&WorkspaceAgent>, new: &WorkspaceAgent, rules: RunRules) -> WorkspaceAgent {
    let mut out = new.clone();
    out.host_gone = false;
    let run = new.run.clone().or_else(|| prev.and_then(|p| p.run.clone()));
    out.run = match run {
        Some(r) => Some(reconcile_run(r, &new.runtime_id, rules)),
        // Rules 4: a full-spec id with no run is that spec.
        None if rules == RunRules::Full && is_full_spec_id(&new.runtime_id) => Some(from_legacy_id(&new.runtime_id)),
        None => None,
    };
    let explicit = new.placement.is_some();
    let placement = new.placement.clone().or_else(|| prev.and_then(|p| p.placement.clone()));
    out.placement = placement.map(|p| {
        if placement_agrees(&p, &new.host_id, explicit, prev.map(|p| p.host_id.as_str()), rules) {
            p
        } else {
            Placement::mirror(&new.host_id)
        }
    });
    out
}

/// A record that starts over (a new id, or a whole-list roster's entry,
/// rule 4): [`merge_upsert`] with nothing before it.
pub fn normalize(new: &WorkspaceAgent, rules: RunRules) -> WorkspaceAgent {
    merge_upsert(None, new, rules)
}

/// A sticky field read leniently: a value this build can't decode is
/// [`Placement::Unknown`] (which the fold resets), never a failed event.
pub(crate) fn de_placement<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Placement>, D::Error> {
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(v.map(|v| serde_json::from_value(v).unwrap_or(Placement::Unknown)))
}

/// [`de_placement`] for `run`: an undecodable run is an empty one (which no
/// `runtime_id` agrees with, so the fold resets it).
pub(crate) fn de_run<'de, D: Deserializer<'de>>(d: D) -> Result<Option<AgentRun>, D::Error> {
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(v.map(|v| serde_json::from_value(v).unwrap_or_default()))
}

impl WorkspaceAgent {
    /// What this agent runs: its `run`, else what its `runtime_id` means.
    pub fn effective_run(&self) -> AgentRun {
        self.run.clone().unwrap_or_else(|| AgentRun::mirror(&self.runtime_id, None))
    }

    /// Where this agent may run: its `placement`, else what `host_id` means.
    /// For an agent whose host is gone this still names that host (see
    /// [`WorkspaceAgent::host_gone`]).
    pub fn effective_placement(&self) -> Placement {
        self.placement.clone().unwrap_or_else(|| Placement::mirror(&self.host_id))
    }

    /// Whether the agent is placed on a host that is gone (removed, or its
    /// owner left): it runs nowhere until someone places it again, and
    /// never falls back to the owner's devices on a placement-aware reader.
    /// The fold's state; also true for a placement whose host the legacy
    /// `host_id` no longer names (a seed taken after the fold cleared it).
    pub fn is_host_gone(&self) -> bool {
        self.host_gone || self.placement.as_ref().and_then(Placement::host).is_some_and(|h| h != self.host_id)
    }

    /// Whether `self` moves the agent compared with `old`: any change of
    /// its effective host, placement, runtime or run source (or harness).
    /// A model change within one source is not a move (rules 4); under
    /// rules 3 the model belongs to the runtime, so it is one there.
    pub fn moves_from(&self, old: &WorkspaceAgent) -> bool {
        let (h, s, _) = self.effective_run().spec_owned();
        let (oh, os, _) = old.effective_run().spec_owned();
        self.host_id != old.host_id
            || self.runtime_id != old.runtime_id
            || self.effective_placement() != old.effective_placement()
            || s != os
            || h != oh
    }

    /// The record a 0.5 writer sends for this agent, as read from the fold:
    /// an agent whose host is gone keeps naming that host (so a rename isn't
    /// a move), and `run` and `placement` mirror `runtime_id` and `host_id`
    /// (rules 3). `resolved` is the writer's display hint for its runtime.
    pub fn for_write(&self, resolved: Option<ResolvedRun>) -> WorkspaceAgent {
        let mut a = self.clone();
        if a.is_host_gone() {
            if let Some(h) = a.placement.as_ref().and_then(Placement::host) {
                a.host_id = h.to_string();
            }
        }
        a.host_gone = false;
        a.placement = Some(Placement::mirror(&a.host_id));
        let hint = resolved.or_else(|| a.run.as_ref().filter(|r| r.runtime_ref() == Some(a.runtime_id.as_str())).and_then(|r| r.resolved.clone()));
        a.run = Some(AgentRun::mirror(&a.runtime_id, hint));
        a
    }

    /// [`WorkspaceAgent::for_write`] under the rules in force: on rules 4
    /// an agent that carries a full run spec, or a `pool` or `chatCreator`
    /// placement, is written as it is, with `runtime_id = legacy_id(run)`
    /// and `host_id` the one its placement implies (a pool keeps its first
    /// host); everything else mirrors its legacy fields as on rules 3.
    pub fn for_write_at(&self, resolved: Option<ResolvedRun>, rules: RunRules) -> WorkspaceAgent {
        let full_run = self.run.as_ref().filter(|r| r.runtime_ref().is_none() && !r.source.is_empty());
        let full_place = matches!(self.placement, Some(Placement::Pool | Placement::ChatCreator));
        if rules != RunRules::Full || (full_run.is_none() && !full_place) {
            return self.for_write(resolved);
        }
        let mut a = self.for_write(resolved);
        if let Some(r) = full_run {
            let mut r = r.clone();
            r.resolved = None;
            a.runtime_id = legacy_id(&r);
            a.run = Some(r);
        }
        if let Some(p) = &self.placement {
            a.host_id = match p {
                Placement::Host { host_id } => host_id.clone(),
                Placement::Pool => self.host_id.clone(),
                _ => String::new(),
            };
            a.placement = Some(p.clone());
        }
        a
    }

    /// The record authz compares an upsert with: an agent whose host is
    /// gone is judged as still naming that host, as the fold sees it at the
    /// event's position (the clearing is a pass over the finished fold). So
    /// a device judging its own write against a finished fold agrees with
    /// every device's fold: a rename that keeps naming the host isn't a
    /// move, and one that writes `host_id = ""` is.
    pub fn as_judged(&self) -> std::borrow::Cow<'_, WorkspaceAgent> {
        match self.placement.as_ref().and_then(Placement::host) {
            Some(h) if self.is_host_gone() => {
                let mut a = self.clone();
                a.host_id = h.to_string();
                a.host_gone = false;
                std::borrow::Cow::Owned(a)
            }
            _ => std::borrow::Cow::Borrowed(self),
        }
    }

    /// Put the agent on `host_id` (`""` = its owner's devices): both the
    /// legacy field and the placement, and no longer gone.
    pub fn place(&mut self, host_id: &str) {
        self.host_id = host_id.to_string();
        self.placement = Some(Placement::mirror(host_id));
        self.host_gone = false;
    }
}

impl AgentRun {
    fn spec_owned(&self) -> (String, String, String) {
        let (h, s, m) = self.spec();
        (h.to_string(), s.to_string(), m.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn agent(runtime: &str, host: &str) -> WorkspaceAgent {
        let mut a = WorkspaceAgent::new("scout", runtime);
        a.host_id = host.into();
        a
    }

    #[test]
    fn legacy_id_examples() {
        let rt = AgentRun::mirror("ws-claude", None);
        assert_eq!(legacy_id(&rt), "ws-claude");
        assert_eq!(from_legacy_id("ws-claude"), rt);
        let spec = AgentRun { harness: "ollama".into(), source: "ollama".into(), model: "llama3:8b".into(), resolved: None };
        let id = legacy_id(&spec);
        assert!(id.starts_with(RUN_ID_PREFIX) && !id.contains('='), "{id}");
        let back = from_legacy_id(&id);
        assert_eq!((back.source.as_str(), back.model.as_str()), ("ollama", "llama3:8b"));
        // A `run:` id that isn't a canonical spec is a runtime id like any.
        assert_eq!(from_legacy_id("run:nope"), AgentRun::mirror("run:nope", None));
        assert_eq!(legacy_id(&from_legacy_id("run:nope")), "run:nope");
    }

    #[test]
    fn placement_wire_shape() {
        let p = Placement::Host { host_id: "dev-1".into() };
        assert_eq!(serde_json::to_value(&p).unwrap(), serde_json::json!({"kind": "host", "hostId": "dev-1"}));
        assert_eq!(serde_json::to_value(Placement::OwnerDevices).unwrap(), serde_json::json!({"kind": "ownerDevices"}));
        let later: Placement = serde_json::from_value(serde_json::json!({"kind": "region", "region": "eu"})).unwrap();
        assert_eq!(later, Placement::Unknown);
        let pool: Placement = serde_json::from_value(serde_json::json!({"kind": "pool", "extra": 1})).unwrap();
        assert_eq!(pool, Placement::Pool);
    }

    #[test]
    fn undecodable_sticky_fields_never_fail_the_record() {
        let json = r#"{"id":"00000000-0000-0000-0000-0000000000aa","name":"s","runtimeId":"rt","placement":5,"run":"x"}"#;
        let a: WorkspaceAgent = serde_json::from_str(json).unwrap();
        assert_eq!(a.placement, Some(Placement::Unknown));
        assert_eq!(a.run, Some(AgentRun::default()));
        let m = normalize(&a, RunRules::Mirror);
        assert_eq!(m.placement, Some(Placement::OwnerDevices));
        assert_eq!(m.run, Some(AgentRun::mirror("rt", None)));
    }

    #[test]
    fn host_gone_survives_a_write_back_and_place_clears_it() {
        let mut a = agent("rt", "");
        a.placement = Some(Placement::Host { host_id: "box-1".into() });
        a.host_gone = true;
        assert!(a.is_host_gone());
        let w = a.for_write(None);
        assert_eq!(w.host_id, "box-1", "a rename names the gone host, not the owner's devices");
        assert!(!w.host_gone);
        a.place("");
        assert!(!a.is_host_gone() && a.effective_placement() == Placement::OwnerDevices);
    }

    fn arb_model() -> impl Strategy<Value = String> {
        prop_oneof![
            Just(String::new()),
            Just("llama3:8b".to_string()),
            Just("org/model".to_string()),
            Just("gpt-5.3-codex".to_string()),
            Just("модель/😀:x".to_string()),
            "\\PC{0,16}",
        ]
    }

    fn arb_source() -> impl Strategy<Value = String> {
        prop_oneof![Just("codex".to_string()), Just("gw-home".to_string()), Just("ollama".to_string()), "[a-z0-9-]{1,12}", "\\PC{0,12}"]
    }

    fn arb_runtime_id() -> impl Strategy<Value = String> {
        prop_oneof![
            Just(String::new()),
            Just("ws-claude".to_string()),
            Just("rt-2".to_string()),
            Just("run:nope".to_string()),
            (arb_source(), arb_model()).prop_map(|(s, m)| legacy_id(&AgentRun { source: s, model: m, ..Default::default() })),
        ]
    }

    fn arb_host() -> impl Strategy<Value = String> {
        prop_oneof![Just(String::new()), Just("dev-a".to_string()), Just("box-1".to_string())]
    }

    fn arb_placement() -> impl Strategy<Value = Option<Placement>> {
        prop_oneof![
            Just(None),
            Just(Some(Placement::OwnerDevices)),
            arb_host().prop_map(|h| Some(Placement::Host { host_id: h })),
            Just(Some(Placement::Pool)),
            Just(Some(Placement::ChatCreator)),
            Just(Some(Placement::Unknown)),
        ]
    }

    fn arb_run() -> impl Strategy<Value = Option<AgentRun>> {
        prop_oneof![
            Just(None),
            arb_runtime_id().prop_map(|id| Some(AgentRun::mirror(&id, Some(ResolvedRun { harness: "claudeCode".into(), model: "opus".into() })))),
            (arb_source(), arb_model(), prop_oneof![Just(String::new()), Just("codex".to_string())])
                .prop_map(|(s, m, h)| Some(AgentRun { harness: h, source: s, model: m, resolved: None })),
        ]
    }

    fn arb_agent() -> impl Strategy<Value = WorkspaceAgent> {
        (arb_runtime_id(), arb_host(), arb_run(), arb_placement()).prop_map(|(rt, h, run, pl)| {
            let mut a = agent(&rt, &h);
            a.id = uuid::Uuid::from_u128(7);
            a.run = run;
            a.placement = pl;
            a
        })
    }

    fn arb_rules() -> impl Strategy<Value = RunRules> {
        prop_oneof![Just(RunRules::Mirror), Just(RunRules::Full)]
    }

    /// The record agrees with its legacy fields, as older builds read them.
    fn consistent(a: &WorkspaceAgent, rules: RunRules) {
        if let Some(r) = &a.run {
            assert_eq!(legacy_id(r), a.runtime_id, "run agrees with runtime_id");
            if rules == RunRules::Mirror {
                assert_eq!(r.runtime_ref(), Some(a.runtime_id.as_str()), "rules 3: run only mirrors");
                assert!(r.harness.is_empty() && r.model.is_empty());
            }
        }
        match (&a.placement, rules) {
            (None, _) => {}
            (Some(p), RunRules::Mirror) => assert_eq!(p, &Placement::mirror(&a.host_id), "rules 3: placement only mirrors"),
            (Some(Placement::OwnerDevices | Placement::ChatCreator), RunRules::Full) => assert!(a.host_id.is_empty()),
            (Some(Placement::Host { host_id }), RunRules::Full) => assert_eq!(host_id, &a.host_id),
            (Some(Placement::Pool), RunRules::Full) => {}
            (Some(Placement::Unknown), _) => panic!("an unknown placement never survives the merge"),
        }
    }

    proptest! {
        /// H14: `legacy_id` round-trips every model name, and `runtime:<id>`
        /// maps to `<id>`.
        #[test]
        fn legacy_id_round_trips(source in arb_source(), model in arb_model(), id in "\\PC{0,20}") {
            prop_assume!(!source.starts_with(RUNTIME_SOURCE_PREFIX));
            let spec = AgentRun { harness: String::new(), source, model, resolved: None };
            let lid = legacy_id(&spec);
            prop_assert!(lid.starts_with(RUN_ID_PREFIX));
            prop_assert_eq!(from_legacy_id(&lid), spec);
            prop_assert_eq!(legacy_id(&AgentRun::mirror(&id, None)), id.clone());
            // Every id means exactly one run, and that run's id is the id.
            prop_assert_eq!(legacy_id(&from_legacy_id(&id)), id);
        }

        /// Whatever the payload and the record before it, the merged record
        /// agrees with the legacy fields it carries, and they are the
        /// payload's (older builds read only those).
        #[test]
        fn the_merge_always_agrees_with_the_legacy_fields(prev in proptest::option::of(arb_agent()), new in arb_agent(), rules in arb_rules()) {
            let prev = prev.map(|p| merge_upsert(None, &p, rules));
            let m = merge_upsert(prev.as_ref(), &new, rules);
            prop_assert_eq!(&m.runtime_id, &new.runtime_id);
            prop_assert_eq!(&m.host_id, &new.host_id);
            consistent(&m, rules);
            // Rule 1: a sticky field the record had stays present.
            if prev.as_ref().is_some_and(|p| p.placement.is_some()) {
                prop_assert!(m.placement.is_some());
            }
            if prev.as_ref().is_some_and(|p| p.run.is_some()) {
                prop_assert!(m.run.is_some());
            }
        }

        /// An older writer's upsert (no sticky fields) never widens: it
        /// changes where the agent runs or what it runs only by changing a
        /// legacy field, which authz already judges as a move; one that
        /// keeps them keeps the sticky fields exactly.
        #[test]
        fn an_older_upsert_never_widens(prev in arb_agent(), runtime in arb_runtime_id(), host in arb_host(), keep in any::<bool>(), rules in arb_rules()) {
            let prev = merge_upsert(None, &prev, rules);
            let mut old = prev.clone();
            old.run = None;
            old.placement = None;
            old.name = "renamed".into();
            if !keep {
                old.runtime_id = runtime;
                old.host_id = host;
            }
            let m = merge_upsert(Some(&prev), &old, rules);
            if m.runtime_id == prev.runtime_id && m.host_id == prev.host_id {
                prop_assert_eq!(&m.run, &prev.run, "a rename keeps run");
                prop_assert_eq!(&m.placement, &prev.placement, "a rename keeps placement");
                prop_assert!(!m.moves_from(&prev));
            } else {
                prop_assert!(m.moves_from(&prev), "a legacy change is a move");
                // Legacy wins: the agent goes where the older writer put it.
                if m.host_id != prev.host_id {
                    prop_assert_eq!(m.effective_placement(), Placement::mirror(&m.host_id));
                }
                if m.runtime_id != prev.runtime_id {
                    prop_assert_eq!(legacy_id(&m.effective_run()), m.runtime_id.clone());
                }
            }
        }

        /// Merging twice is merging once: the fold's result is a fixed
        /// point, so re-reading and re-writing a record changes nothing.
        #[test]
        fn the_merge_is_idempotent(prev in proptest::option::of(arb_agent()), new in arb_agent(), rules in arb_rules()) {
            let prev = prev.map(|p| merge_upsert(None, &p, rules));
            let m = merge_upsert(prev.as_ref(), &new, rules);
            prop_assert_eq!(merge_upsert(Some(&m), &m, rules), m.clone());
            let w = m.for_write(None);
            prop_assert!(!merge_upsert(Some(&m), &w, RunRules::Mirror).moves_from(&m) || rules == RunRules::Full);
        }
    }
}
