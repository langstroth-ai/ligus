//! Asks folded with their envelope's provenance.
//!
//! [`FlowState::apply`] sees only a record; who signed it decides these
//! things here, all fold-local (nothing is refused, so every version accepts
//! the same events and keeps the same asks):
//!
//! * **An effect** (a permission prompt, the older `permission: true`, or a
//!   v2 `effect`) counts only when its signer is the asking turn's host:
//!   the device whose reply placeholder committed that turn (or an agent key
//!   it certified), with the agent bound to it, and the effect names that
//!   device. An outside prompt (`ligus hook permission`) counts when a
//!   person's own device raises it to that person alone. Anyone else's
//!   effect folds as a plain question, attributed to them.
//! * **A close** counts only from the ask's own signer, or the other half
//!   of its host pair (the host device, or an agent key it certified).
//! * **An answer to a grant** counts only from the host's person or the
//!   agent's owner, and with their passkey presence when the effect says
//!   so. Other answers stay in the log and count for nothing.
//! * **An answer to a v2 ask** counts only from a person its audience
//!   names (a member, or a role at the answer's own config frontier, which
//!   may be no older than the ask's), signed by a device of theirs, over
//!   the ask's digest ([`ask_digest`]). A quorum settles at the first
//!   position where one option has enough distinct people, and stays
//!   settled. The asker's closing record (`settledBy`) has the last word.
//!
//! Every input is the chat log in canonical order and the workspace roster
//! at each event's position, so every device decides the same. No wall
//! clock enters: expiry is a view ([`AskState::is_live`]).
//!
//! The v1 shadow ([`AskState::legacy_settled`]) is folded as builds before
//! these rules fold it, and only eviction (and so the ingest check of an
//! answer's `optionIndex`) reads it.

use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    agent_from_key, AskEffect, AskPoster, AskState, AskVote, FlowRecord, FlowState, HumanAnswered, HumanAsk, MAX_FOLDED_TEXT_CHARS,
    EFFECT_PERMISSION, EFFECT_SECRET, MAX_ASKS_PER_CHAT, MAX_AUDIENCE_PEOPLE, MAX_COUNTED_ANSWERS,
    MAX_EFFECT_DETAIL_BYTES, ORIGIN_OUTSIDE, PRIMARY,
};
use crate::identity::{ActorKind, CfgFrontier};
use crate::authz::RosterState;
use crate::events::SessionEventEnvelope;
use crate::session::ChatSession;

/// The digest a v2 ask seeded from a snapshot carries: no answer names it.
pub const SNAPSHOT_DIGEST: &str = "snapshot";

/// The signing context of [`ask_digest`]. Frozen: answers on the log name
/// digests made with it.
pub const ASK_DIGEST_CONTEXT: &[u8] = b"ligus-ask-digest-v1\0";

/// `SHA256("ligus-ask-digest-v1\0" ‖ askId (16 bytes) ‖ canonical payload)`
/// as lowercase hex: what an answer binds (`askDigest`). The payload is the
/// asking envelope's payload as signed ([`signed_payload`]), in its
/// canonical JSON form ([`canonical_json`]: keys sorted, no spaces, the
/// RFC 8785 string rules), so two encodings of one signed event (a copy
/// whose keys were reordered on the way) give one digest. Any change to
/// the question, the options, the audience or the effect gives another.
pub fn ask_digest(ask_id: Uuid, payload: &[u8]) -> String {
    let canonical = match serde_json::from_slice::<serde_json::Value>(payload) {
        Ok(v) => canonical_json(&v).into_bytes(),
        Err(_) => payload.to_vec(),
    };
    let mut h = Sha256::new();
    h.update(ASK_DIGEST_CONTEXT);
    h.update(ask_id.as_bytes());
    h.update(&canonical);
    crate::keys::hex(&h.finalize())
}

/// The canonical JSON of `v`: object keys sorted by their UTF-16 code
/// units, no insignificant whitespace, strings escaped as RFC 8785 says
/// (`\"`, `\\`, `\b \f \n \r \t`, other controls as lowercase `\u00xx`,
/// everything else as is), integers in decimal. The phone repositories
/// implement the same (golden vectors: `tests/golden/ask_digest_v1.json`).
pub fn canonical_json(v: &serde_json::Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

fn write_canonical(v: &serde_json::Value, out: &mut String) {
    use serde_json::Value;
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => write_string(s, out),
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(x, out);
            }
            out.push(']');
        }
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(k, out);
                out.push(':');
                write_canonical(&m[*k], out);
            }
            out.push('}');
        }
    }
}

fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The payload bytes `env`'s signature covers: the bytes it arrived with,
/// else this build's serialization (what it signed, for its own events).
pub fn signed_payload(env: &SessionEventEnvelope) -> Vec<u8> {
    match env.raw() {
        Some(r) => r.payload().as_bytes().to_vec(),
        None => serde_json::to_vec(&env.payload).unwrap_or_default(),
    }
}

/// The digest of the ask `env` posts, when it carries one.
pub fn envelope_ask_digest(env: &SessionEventEnvelope) -> Option<String> {
    match &env.payload {
        crate::events::SessionEvent::FlowRecorded { record: FlowRecord::HumanAsk(a) } => {
            Some(ask_digest(a.ask_id, &signed_payload(env)))
        }
        _ => None,
    }
}

/// What the fold knows about the envelope carrying an ask or an answer.
#[derive(Debug, Clone, Default)]
pub struct AskProvenance {
    /// The key that signed it (a device, or an agent key).
    pub signer: Option<Uuid>,
    /// The device that key resolves to.
    pub host: Option<Uuid>,
    /// The signer's actor.
    pub by: AskPoster,
    /// Nothing to judge it by (the device-local workspace, whose events are
    /// all this device's own, or a record folded without its envelope): it
    /// folds as it always has.
    pub legacy: bool,
    /// An ask: its signer is the asking turn's host.
    pub from_host: bool,
    /// An ask: its signer is the asking agent's own key.
    pub own_agent_key: bool,
    /// An ask: its effect (if any) counts.
    pub effect_ok: bool,
    /// An answer: its author may grant the ask's permission (the 0.3.1
    /// rule, for asks without the v2 rules).
    pub may_grant: bool,
    /// The event that carried it.
    pub event_id: Option<Uuid>,
    /// Its stamped config frontier.
    pub frontier: Option<CfgFrontier>,
    /// An ask: [`ask_digest`] of its envelope.
    pub digest: String,
    /// An answer: why it doesn't count (empty: it counts).
    pub why: String,
}

impl AskProvenance {
    /// A record without provenance: folded as before these rules.
    pub fn legacy() -> Self {
        Self {
            legacy: true,
            from_host: true,
            effect_ok: true,
            may_grant: true,
            ..Default::default()
        }
    }

    /// The provenance of `record` in `env`, judged at its position:
    /// `roster` is the workspace roster there, `session` the fold so far.
    /// `None` when `record` isn't an ask or an answer.
    pub fn of(
        record: &FlowRecord,
        env: &SessionEventEnvelope,
        session: &ChatSession,
        roster: &RosterState,
        trusted: bool,
    ) -> Option<Self> {
        if !matches!(
            record,
            FlowRecord::HumanAsk(_) | FlowRecord::HumanAnswered(_)
        ) {
            return None;
        }
        if trusted {
            let mut p = Self::legacy();
            p.event_id = Some(env.event_id);
            if let FlowRecord::HumanAsk(a) = record {
                if !session.flow.asks.contains_key(&a.ask_id) {
                    p.digest = ask_digest(a.ask_id, &signed_payload(env));
                }
            }
            return Some(p);
        }
        let stamp = env.actor_stamp.as_ref();
        let by = AskPoster {
            id: stamp.map(|s| s.actor.id.clone()).unwrap_or_default(),
            name: stamp
                .map(|s| s.actor.display_name.clone())
                .unwrap_or_default(),
        };
        let signer = env.signer_device_id;
        let key = signer.and_then(|k| roster.valid_agent_key(k));
        let host = key.map(|k| k.host_device_id).or(signer);
        let mut p = Self {
            signer,
            host,
            by,
            event_id: Some(env.event_id),
            frontier: stamp.and_then(|s| s.cfg_frontier),
            ..Default::default()
        };
        match record {
            FlowRecord::HumanAsk(a) => {
                let agent = agent_from_key(&a.agent_key);
                p.own_agent_key = key.is_some_and(|k| Some(k.agent_id) == agent);
                p.from_host = host.is_some_and(|h| {
                    turn_host_ok(
                        session,
                        roster,
                        a.turn_id,
                        &a.agent_key,
                        agent,
                        key.map(|k| k.agent_id),
                        h,
                    )
                });
                p.effect_ok = match &a.effect {
                    None => true,
                    Some(e) => {
                        effect_well_formed(e)
                            && ((p.from_host && e.host_device_id.is_some() && e.host_device_id == host)
                                || outside_ok(a, e, env, roster, &p))
                    }
                };
                if !session.flow.asks.contains_key(&a.ask_id) {
                    p.digest = ask_digest(a.ask_id, &signed_payload(env));
                }
            }
            FlowRecord::HumanAnswered(ans) => {
                let st = session.flow.asks.get(&ans.ask_id);
                p.may_grant = st.is_none_or(|st| {
                    !st.ask.permission || may_grant(st, roster, &p.by.id, p.signer)
                });
                if let Some(st) = st {
                    p.why = answer_why(st, ans, env, roster, &p).unwrap_or_default();
                }
            }
            _ => {}
        }
        Some(p)
    }
}

/// An effect a reader can show: a kind this build knows, a detail within
/// [`MAX_EFFECT_DETAIL_BYTES`], and a host named.
fn effect_well_formed(e: &AskEffect) -> bool {
    (e.kind == EFFECT_PERMISSION || e.kind == EFFECT_SECRET)
        && e.detail.len() <= MAX_EFFECT_DETAIL_BYTES
        && e.title.len() <= MAX_EFFECT_DETAIL_BYTES
        && e.host_device_id.is_some()
}

/// An outside prompt (section 8.2): a person's own device raises it, as
/// that person, to that person alone, naming that device as its host.
fn outside_ok(a: &HumanAsk, e: &AskEffect, env: &SessionEventEnvelope, roster: &RosterState, p: &AskProvenance) -> bool {
    let Some(device) = p.signer else { return false };
    let human = env
        .actor_stamp
        .as_ref()
        .is_some_and(|s| s.actor.kind == ActorKind::Human && !s.runtime_effect);
    // A device signing as itself, not an agent key.
    let own_device = roster.valid_agent_key(device).is_none() && p.host == Some(device);
    let account = roster.members.get(&p.by.id).and_then(|m| m.account_id);
    let holds = account.is_some_and(|acct| roster.account_holds_device(acct, device));
    human
        && own_device
        && holds
        && e.host_device_id == Some(device)
        && a.origin.as_ref().is_some_and(|o| o.via == ORIGIN_OUTSIDE)
        && a.audience.as_ref().and_then(|au| au.single()) == Some(p.by.id.as_str())
}

/// Why `ans` doesn't count for `st` (`None`: it counts).
fn answer_why(st: &AskState, ans: &HumanAnswered, env: &SessionEventEnvelope, roster: &RosterState, p: &AskProvenance) -> Option<String> {
    let grant = st.grant();
    if !st.is_v2() && grant.is_none() {
        // The older rule: any answer counts.
        return None;
    }
    if st.ask.permission && !st.is_v2() {
        return (!p.may_grant).then(|| "not the host's person or the agent's owner".into());
    }
    let author = p.by.id.as_str();
    let account = roster.members.get(author).and_then(|m| m.account_id);
    let Some(account) = account else { return Some("not a person in this workspace".into()) };
    if !p.signer.is_some_and(|d| roster.account_certified(account, d)) {
        return Some("not signed by a device of theirs".into());
    }
    if st.is_v2() {
        // A config position older than the ask's own can't keep a demoted
        // member eligible.
        if let Some(asked) = st.frontier {
            if p.frontier.is_none_or(|f| f < asked) {
                return Some("stamped before the ask".into());
            }
        }
        match st.ask.audience.as_ref() {
            Some(au) if au.kind == super::AUDIENCE_PEOPLE => {
                if au.member_ids.len() > MAX_AUDIENCE_PEOPLE || !au.member_ids.iter().any(|m| m == author) {
                    return Some("not asked".into());
                }
            }
            Some(au) if au.kind == super::AUDIENCE_ROOM => {
                let Some(floor) = au.floor() else { return Some("not asked".into()) };
                let floor = floor.rank().max(crate::WorkspaceRole::Contributor.rank());
                if roster.role_of(author).is_none_or(|r| r.rank() < floor) {
                    return Some("below the role it asks".into());
                }
            }
            Some(_) => return Some("not asked".into()),
            None => {
                if roster.role_of(author).is_none_or(|r| r.rank() < crate::WorkspaceRole::Contributor.rank()) {
                    return Some("below the role it asks".into());
                }
            }
        }
        if !st.digest.is_empty() && ans.ask_digest != st.digest {
            return Some(if ans.ask_digest.is_empty() { "answered on an older version".into() } else { "answers another question".into() });
        }
    }
    if let Some(e) = grant {
        if !may_grant(st, roster, author, p.signer) {
            return Some("not the host's person or the agent's owner".into());
        }
        if e.needs_presence() {
            let present = crate::authz::passkeys::verify_stamped(env, roster).ok().flatten();
            if present != Some(account) {
                return Some("no passkey".into());
            }
        }
    }
    None
}

/// Whether `host` runs the turn `turn_id` for `agent_key`: its reply
/// placeholder committed that turn on `host`, the reply is that agent's,
/// and the agent is bound to `host` (the chat's runtime: `host` is a
/// trusted device, signing as itself).
fn turn_host_ok(
    session: &ChatSession,
    roster: &RosterState,
    turn_id: Uuid,
    agent_key: &str,
    agent: Option<Uuid>,
    key_agent: Option<Uuid>,
    host: Uuid,
) -> bool {
    let committed = session
        .turn_dispatch
        .values()
        .any(|t| t.answer == Some(turn_id) && t.answered_by == Some(host));
    if !committed {
        return false;
    }
    let Some(reply) = session.messages.iter().find(|m| m.id == turn_id) else {
        return false;
    };
    match agent {
        Some(a) => {
            reply.responder_id == Some(a)
                && key_agent.is_none_or(|k| k == a)
                && roster.agent_bound_to(a, host)
        }
        None => {
            agent_key == PRIMARY
                && reply.responder_id.is_none()
                && key_agent.is_none()
                && roster.device_trusted(host)
        }
    }
}

/// Whether `author` may grant `st`'s permission: the asking agent's owner,
/// or the person of the host that asked (an account that holds that
/// device; one that only certified its key for itself holds nothing).
/// The answer's signing device must be certified for the author's account,
/// whatever its stamp names.
fn may_grant(st: &AskState, roster: &RosterState, author: &str, signer: Option<Uuid>) -> bool {
    if author.is_empty() {
        return false;
    }
    let Some(account) = roster.members.get(author).and_then(|m| m.account_id) else { return false };
    if !signer.is_some_and(|d| roster.account_certified(account, d)) {
        return false;
    }
    let account = Some(account);
    let owner = agent_from_key(&st.ask.agent_key)
        .and_then(|a| roster.agents.get(&a))
        .map(|a| a.owner.as_str());
    let is_owner = owner.is_some_and(|o| {
        o == author || account.is_some_and(|acct| Uuid::parse_str(o).ok() == Some(acct))
    });
    let host_person = st
        .host
        .zip(account)
        .is_some_and(|(h, acct)| roster.account_holds_device(acct, h));
    is_owner || host_person
}

impl FlowState {
    /// Asks a base snapshot carries: what the snapshot claims about who
    /// posted them isn't evidence, so none is a permission prompt or has a
    /// known signer, and eviction reads exactly what older builds read from
    /// the same snapshot (its answer and closed flags).
    pub fn reseat_snapshot_asks(&mut self) {
        for st in self.asks.values_mut() {
            // Eviction reads what 0.3 reads from the same snapshot: its
            // answer and closed flags (never fields 0.3 doesn't know).
            st.legacy_settled = st.answer.is_some() || st.ask.closed;
            st.ask.permission = false;
            st.ask.effect = None;
            st.signer = None;
            st.host = None;
            st.event_id = None;
            if st.is_v2() {
                // A snapshot's word about who answered a v2 ask isn't
                // evidence: its votes and settlement go, and no answer can
                // match its digest (there is no signed record to bind to).
                st.counted.clear();
                st.uncounted.clear();
                st.answered_by.clear();
                st.declined = false;
                st.answer = None;
                st.frontier = None;
                st.digest = SNAPSHOT_DIGEST.into();
                st.ask.closed = true;
            }
        }
    }

    /// Fold an ask or an answer with its provenance (see the module docs).
    pub fn apply_ask(&mut self, record: &FlowRecord, p: &AskProvenance) {
        match record {
            FlowRecord::HumanAsk(a) => match self.asks.get_mut(&a.ask_id) {
                // Only the asking turn may close it; the question is fixed.
                Some(st) if st.ask.turn_id == a.turn_id && st.ask.agent_key == a.agent_key => {
                    st.legacy_settled |= a.closed;
                    let unknown = st.signer.is_none() && st.host.is_none();
                    let pair = (p.signer.is_some() && p.signer == st.signer)
                        || (p.host.is_some() && p.host == st.host);
                    if p.legacy || unknown || pair {
                        st.ask.closed |= a.closed;
                        // The asker has the last word: what it acted on.
                        if let Some(e) = a.settled_by {
                            st.settle_on(e);
                            st.ask.closed = true;
                        }
                    }
                }
                Some(_) => {}
                None => {
                    if self.asks.len() >= MAX_ASKS_PER_CHAT {
                        if let Some(oldest) = self
                            .asks
                            .iter()
                            .filter(|(_, s)| s.evictable())
                            .min_by_key(|(_, s)| s.ask.at_ms)
                            .map(|(k, _)| *k)
                        {
                            self.asks.remove(&oldest);
                        }
                    }
                    let mut ask = a.clone();
                    ask.permission &= p.from_host;
                    if !p.effect_ok {
                        // An effect that doesn't count takes the older flag
                        // with it: the ask is a plain question.
                        ask.effect = None;
                        ask.permission = false;
                    }
                    let theirs = !p.legacy && !p.from_host && !p.own_agent_key;
                    self.asks.insert(
                        a.ask_id,
                        AskState {
                            ask,
                            answer: None,
                            posted_by: theirs.then(|| p.by.clone()),
                            signer: p.signer,
                            host: p.host,
                            legacy_settled: a.closed,
                            digest: p.digest.clone(),
                            frontier: p.frontier,
                            event_id: p.event_id,
                            ..Default::default()
                        },
                    );
                }
            },
            FlowRecord::HumanAnswered(ans) => {
                if let Some(st) = self.asks.get_mut(&ans.ask_id) {
                    st.legacy_settled = true;
                    st.answer_with(ans, p);
                }
            }
            _ => {}
        }
    }
}

impl AskState {
    /// Fold one answer (`p.why` empty: it counts).
    fn answer_with(&mut self, ans: &HumanAnswered, p: &AskProvenance) {
        let vote = AskVote {
            member_id: p.by.id.clone(),
            name: if p.by.name.is_empty() { ans.by.clone() } else { p.by.name.clone() },
            event_id: p.event_id,
            device_id: p.signer,
            option_index: ans.option_index,
            answer: ans.answer.chars().take(MAX_FOLDED_TEXT_CHARS).collect(),
            text: if self.ask.allow_text || !self.is_v2() { ans.text.chars().take(MAX_FOLDED_TEXT_CHARS).collect() } else { String::new() },
            decline: ans.decline,
            at_ms: ans.at_ms,
            why: String::new(),
        };
        let counts = p.legacy || (p.why.is_empty() && p.may_grant);
        if !counts {
            if self.uncounted.len() < MAX_COUNTED_ANSWERS {
                let why = if p.why.is_empty() { "not the host's person or the agent's owner".into() } else { p.why.clone() };
                self.uncounted.push(AskVote { why, text: String::new(), answer: String::new(), ..vote });
            }
            return;
        }
        if !self.is_v2() {
            // A closed or settled ask takes none.
            if !self.is_open() {
                return;
            }
            // First answer that counts, in canonical order (the older rule;
            // a "Not now" there is an answer like any other).
            self.answer = Some(ans.clone());
            self.counted.push(vote.clone());
            self.answered_by = vec![vote];
            return;
        }
        let has_option = ans.option_index.is_some_and(|i| (i as usize) < self.ask.options.len());
        let text_only = ans.option_index.is_none() && self.ask.allow_text && !ans.text.trim().is_empty();
        if !ans.decline && !has_option && !text_only {
            if self.uncounted.len() < MAX_COUNTED_ANSWERS {
                self.uncounted.push(AskVote { why: "no option chosen".into(), text: String::new(), answer: String::new(), ..vote });
            }
            return;
        }
        // Closed or declined: nothing more is kept.
        if self.ask.closed || self.declined {
            return;
        }
        // Each person's latest answer is their vote. One that arrives after
        // the ask settled is kept (the asker may have acted on it: see
        // `settledBy`) but settles nothing.
        let settled = self.answer.is_some();
        let known = self.counted.iter().position(|v| v.member_id == vote.member_id);
        match known {
            Some(i) => {
                self.counted.remove(i);
            }
            None if self.counted.len() >= MAX_COUNTED_ANSWERS => return,
            None => {}
        }
        self.counted.push(vote.clone());
        if settled {
            return;
        }
        if ans.decline {
            // "Not now" closes an ask put to one person; in a room it is
            // that person's abstention.
            if self.ask.audience.as_ref().and_then(|a| a.single()) == Some(vote.member_id.as_str()) {
                self.declined = true;
                self.answered_by = vec![vote];
            }
            return;
        }
        let q = self.quorum() as usize;
        let mut ans = ans.clone();
        ans.text = vote.text.clone();
        let ans = &ans;
        if text_only {
            if q == 1 {
                self.answer = Some(ans.clone());
                self.answered_by = vec![vote];
            }
            return;
        }
        let pick = ans.option_index;
        let voters: Vec<AskVote> = self.counted.iter().filter(|v| !v.decline && v.option_index == pick).cloned().collect();
        if voters.len() >= q {
            // Settled at the first position it is reached, and frozen.
            self.answer = Some(ans.clone());
            self.answered_by = voters;
        }
    }

    /// The asker acted on answer event `e`: show that one, when it counted.
    fn settle_on(&mut self, e: Uuid) {
        if self.answered_by.iter().any(|v| v.event_id == Some(e)) {
            return;
        }
        let Some(v) = self.counted.iter().find(|v| v.event_id == Some(e)).cloned() else { return };
        let pick = v.option_index;
        let voters: Vec<AskVote> = if v.decline {
            vec![v.clone()]
        } else {
            self.counted.iter().filter(|x| !x.decline && x.option_index == pick).cloned().collect()
        };
        // Only an answer that settles the ask on its own terms: a decline
        // by the one person asked, or an option with its quorum.
        let single = self.ask.audience.as_ref().and_then(|a| a.single()) == Some(v.member_id.as_str());
        if (v.decline && !single) || (!v.decline && pick.is_some() && voters.len() < self.quorum() as usize) {
            return;
        }
        if v.decline {
            self.declined = true;
            self.answer = None;
        } else {
            self.answer = Some(HumanAnswered {
                ask_id: self.ask.ask_id,
                answer: v.answer.clone(),
                option_index: v.option_index,
                by: v.name.clone(),
                at_ms: v.at_ms,
                text: v.text.clone(),
                ..Default::default()
            });
        }
        self.answered_by = voters;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::{HumanAnswered, HumanAsk};

    fn by(signer: u128) -> AskProvenance {
        let d = Uuid::from_u128(signer);
        AskProvenance {
            signer: Some(d),
            host: Some(d),
            by: AskPoster {
                id: format!("p{signer}"),
                name: String::new(),
            },
            from_host: true,
            may_grant: false,
            ..Default::default()
        }
    }

    /// Eviction picks the asks a build without these rules would: an ask
    /// someone else "closed" (or answered without the right to) is still
    /// open here, but it is settled for eviction, so both keep the same asks
    /// and judge a later answer's `optionIndex` alike.
    #[test]
    fn eviction_follows_what_every_version_folds() {
        let mut f = FlowState::default();
        let first = HumanAsk {
            ask_id: Uuid::from_u128(1),
            turn_id: Uuid::from_u128(100),
            agent_key: "a".into(),
            at_ms: 1,
            permission: true,
            ..Default::default()
        };
        f.apply_ask(&FlowRecord::HumanAsk(first.clone()), &by(7));
        f.apply_ask(
            &FlowRecord::HumanAsk(HumanAsk {
                closed: true,
                ..first.clone()
            }),
            &by(8),
        );
        assert!(f.asks[&first.ask_id].is_open() && f.asks[&first.ask_id].legacy_settled);
        // An ineligible answer to a permission ask: open here, settled for eviction.
        let second = HumanAsk {
            ask_id: Uuid::from_u128(2),
            at_ms: 2,
            ..first.clone()
        };
        f.apply_ask(&FlowRecord::HumanAsk(second.clone()), &by(7));
        f.apply_ask(
            &FlowRecord::HumanAnswered(HumanAnswered {
                ask_id: second.ask_id,
                answer: "yes".into(),
                ..Default::default()
            }),
            &by(8),
        );
        assert!(f.asks[&second.ask_id].is_open() && f.asks[&second.ask_id].legacy_settled);
        for i in 3..=MAX_ASKS_PER_CHAT as u128 {
            f.apply_ask(
                &FlowRecord::HumanAsk(HumanAsk {
                    ask_id: Uuid::from_u128(i),
                    at_ms: 10 + i as i64,
                    ..first.clone()
                }),
                &by(7),
            );
        }
        let mut legacy = FlowState::default();
        for r in [
            FlowRecord::HumanAsk(first.clone()),
            FlowRecord::HumanAsk(HumanAsk {
                closed: true,
                ..first.clone()
            }),
            FlowRecord::HumanAsk(second.clone()),
            FlowRecord::HumanAnswered(HumanAnswered {
                ask_id: second.ask_id,
                answer: "yes".into(),
                ..Default::default()
            }),
        ] {
            legacy.apply(&r);
        }
        for i in 3..=MAX_ASKS_PER_CHAT as u128 {
            legacy.apply(&FlowRecord::HumanAsk(HumanAsk {
                ask_id: Uuid::from_u128(i),
                at_ms: 10 + i as i64,
                ..first.clone()
            }));
        }
        for n in 0..2u128 {
            let next = FlowRecord::HumanAsk(HumanAsk {
                ask_id: Uuid::from_u128(1000 + n),
                at_ms: 9000,
                ..first.clone()
            });
            f.apply_ask(&next, &by(7));
            legacy.apply(&next);
            let keys = |s: &FlowState| s.asks.keys().copied().collect::<Vec<_>>();
            assert_eq!(keys(&f), keys(&legacy));
        }
        assert!(!f.asks.contains_key(&first.ask_id) && !f.asks.contains_key(&second.ask_id));
    }

    /// A permission flag from anyone but the turn's host folds off, and the
    /// ask is theirs; the agent's own key is never "someone else".
    #[test]
    fn a_permission_flag_needs_the_host() {
        let mut f = FlowState::default();
        let a = HumanAsk {
            ask_id: Uuid::from_u128(1),
            permission: true,
            ..Default::default()
        };
        f.apply_ask(
            &FlowRecord::HumanAsk(a.clone()),
            &AskProvenance {
                from_host: false,
                ..by(9)
            },
        );
        assert!(!f.asks[&a.ask_id].ask.permission);
        assert_eq!(
            f.asks[&a.ask_id].posted_by.as_ref().map(|p| p.id.as_str()),
            Some("p9")
        );
        let b = HumanAsk {
            ask_id: Uuid::from_u128(2),
            permission: true,
            ..Default::default()
        };
        f.apply_ask(
            &FlowRecord::HumanAsk(b.clone()),
            &AskProvenance {
                from_host: false,
                own_agent_key: true,
                ..by(9)
            },
        );
        assert!(!f.asks[&b.ask_id].ask.permission && f.asks[&b.ask_id].posted_by.is_none());
    }
}
