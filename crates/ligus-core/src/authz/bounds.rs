//! Size bounds that hold as fold rules (S5, S6): an event past a bound folds
//! as a no-op on every device alike.

use crate::flow::FlowRecord;
use crate::session::ChatSession;

/// S5: active lockdowns one raiser may hold on one chat.
pub const MAX_LOCKDOWNS_PER_RAISER: usize = 3;
/// S5: active lockdowns on one chat.
pub const MAX_LOCKDOWNS_PER_CHAT: usize = 16;

/// S6 bounds on workspace flow state.
pub const MAX_ZONES: usize = 256;
pub const MAX_MARKS_PER_CHAT: usize = 512;
pub const MAX_AGENT_PROFILES: usize = 256;
pub const MAX_RECRUITS_PER_PROPOSAL: usize = 16;
pub const MAX_ZONE_PATTERN_BYTES: usize = 256;
pub const MAX_ZONE_PATTERN_SEGMENTS: usize = 16;
pub const MAX_ZONE_PATTERN_GLOBSTARS: usize = 4;

/// Whether a `ChatLockdownRaised` by `raiser` on `chat` stays within S5.
/// Lockdowns count by the event that raised them (copies of one raise under
/// several signers count once), against the signed author who raised it
/// (`AuthzState::lockdown_raisers`). Every raise is a new lockdown: one
/// reusing an active lockdown's id is no re-raise of it.
pub fn lockdown_within_bounds(fold: &ChatSession, chat: uuid::Uuid, raiser: &str) -> bool {
    let raises: std::collections::BTreeSet<uuid::Uuid> =
        fold.guards.lockdowns_of(chat).iter().map(|l| l.raise_event_id.unwrap_or(l.id)).collect();
    if raises.len() >= MAX_LOCKDOWNS_PER_CHAT {
        return false;
    }
    let mine = raises.iter().filter(|r| fold.authz.lockdown_raisers.get(r).is_some_and(|a| a == raiser)).count();
    mine < MAX_LOCKDOWNS_PER_RAISER
}

/// Whether a `ChatLockdownCleared` names the raises it clears: at least one
/// (a clear that names none would clear nothing; one that cleared everything
/// would lift raises its author never saw), at most
/// [`MAX_LOCKDOWNS_PER_CHAT`] (all a chat can have active), each once.
pub fn clear_names_raises(raises: &[uuid::Uuid]) -> Result<(), &'static str> {
    if raises.is_empty() {
        return Err("a lockdown clear names the raises it clears");
    }
    if raises.len() > MAX_LOCKDOWNS_PER_CHAT {
        return Err("a lockdown clear names at most as many raises as a chat can hold");
    }
    let mut seen = std::collections::BTreeSet::new();
    if !raises.iter().all(|r| seen.insert(*r)) {
        return Err("a lockdown clear names each raise once");
    }
    Ok(())
}

/// Whether a zone pattern is within S6's size and complexity limits.
pub fn zone_pattern_ok(pattern: &str) -> bool {
    pattern.len() <= MAX_ZONE_PATTERN_BYTES
        && pattern.split('/').count() <= MAX_ZONE_PATTERN_SEGMENTS
        && pattern.split('/').filter(|s| *s == "**").count() <= MAX_ZONE_PATTERN_GLOBSTARS
}

/// Whether folding `record` keeps the flow state within S6.
pub fn flow_within_bounds(fold: &ChatSession, record: &FlowRecord) -> bool {
    let f = &fold.flow;
    match record {
        FlowRecord::Zone(z) => {
            zone_pattern_ok(&z.pattern) && (f.zones.iter().any(|x| x.id == z.id) || f.zones.len() < MAX_ZONES)
        }
        FlowRecord::Mark(m) => {
            f.marks.contains_key(&m.id)
                || f.marks.values().filter(|x| x.session_id == m.session_id).count() < MAX_MARKS_PER_CHAT
        }
        FlowRecord::AgentProfile(p) => f.profiles.contains_key(&p.agent_id) || f.profiles.len() < MAX_AGENT_PROFILES,
        FlowRecord::Recruit(r) => {
            f.recruits.iter().any(|x| x.id == r.id)
                || f.recruits.iter().filter(|x| x.reference == r.reference).count() < MAX_RECRUITS_PER_PROPOSAL
        }
        FlowRecord::HumanAsk(a) => {
            use crate::flow::{MAX_ASK_OPTIONS, MAX_ASK_OPTION_CHARS, MAX_ASK_QUESTION_CHARS};
            a.question.chars().count() <= MAX_ASK_QUESTION_CHARS
                && a.options.len() <= MAX_ASK_OPTIONS
                && a.options.iter().all(|o| o.chars().count() <= MAX_ASK_OPTION_CHARS)
        }
        FlowRecord::HumanAnswered(a) => {
            a.answer.chars().count() <= crate::flow::MAX_ANSWER_CHARS
                && a.option_index.is_none_or(|i| f.asks.get(&a.ask_id).is_some_and(|s| (i as usize) < s.ask.options.len()))
        }
        FlowRecord::TurnToolSummary(t) => t.calls.len() <= crate::flow::MAX_SUMMARY_TOOLS,
        _ => true,
    }
}
