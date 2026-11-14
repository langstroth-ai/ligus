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
/// `raisers` maps each active lockdown id to the signed author who raised it.
pub fn lockdown_within_bounds(fold: &ChatSession, chat: uuid::Uuid, lockdown_id: uuid::Uuid, raiser: &str) -> bool {
    let active = fold.guards.lockdowns_of(chat);
    if active.iter().any(|l| l.id == lockdown_id) {
        return true; // idempotent re-raise
    }
    if active.len() >= MAX_LOCKDOWNS_PER_CHAT {
        return false;
    }
    let mine = active
        .iter()
        .filter(|l| fold.authz.lockdown_raisers.get(&l.id).is_some_and(|r| r == raiser))
        .count();
    mine < MAX_LOCKDOWNS_PER_RAISER
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
        _ => true,
    }
}
