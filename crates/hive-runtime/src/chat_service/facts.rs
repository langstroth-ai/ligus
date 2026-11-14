//! P0.5: per-turn measurement records (turn tags, touches, progress, edge
//! waits, route stops) are **device-local facts**, not signed log events.
//! [`ChatService::record_flow`] routes them here; [`ChatService::load`]
//! overlays a session's facts onto its folded flow state, so every reader
//! (routing, scores, the board, the phone's Flow view) works unchanged on
//! this device. What syncs instead is one bounded
//! [`hive_core::flow::AgentOutcomeSummary`] per agent, per UTC day, per host
//! ([`ChatService::emit_daily_summaries`]). Route decisions stay in the log:
//! other devices follow the first decision per trigger.

use hive_core::flow::{FlowRecord, TurnTag};
use uuid::Uuid;

use super::{ChatService, Result};

/// Fact kind of the "summary emitted" marker (one per agent, day, host).
const SUMMARY_MARK: &str = "summaryEmitted";

impl ChatService {
    /// Store a measurement record as a device-local fact on `session_id`.
    pub(super) fn record_fact(&self, session_id: Uuid, workspace_id: Uuid, record: &FlowRecord) -> Result<()> {
        let at_ms = crate::facts::record_at_ms(record).unwrap_or_else(hive_core::flow::now_ms);
        let json = serde_json::to_string(record).unwrap_or_default();
        self.store.record_fact(workspace_id, session_id, at_ms, record.type_str(), &json)?;
        Ok(())
    }

    /// Fold this device's facts for `session` onto its flow state.
    pub(super) fn overlay_facts(&self, session: &mut hive_core::ChatSession) {
        self.overlay_facts_on(session.id, &mut session.flow);
    }

    /// Fold this device's facts for `session_id` onto `flow`.
    pub(super) fn overlay_facts_on(&self, session_id: Uuid, flow: &mut hive_core::flow::FlowState) {
        let Ok(rows) = self.store.session_facts(session_id) else { return };
        for (_, _, kind, json) in rows {
            if kind == SUMMARY_MARK {
                continue;
            }
            if let Ok(record) = serde_json::from_str::<FlowRecord>(&json) {
                if record.is_measurement() {
                    flow.apply(&record);
                }
            }
        }
    }

    /// Emit this host's daily outcome summaries for every complete UTC day
    /// (before today) that has turn facts and wasn't summarized yet: at most
    /// one per agent, per day, per host. Returns how many were emitted.
    pub fn emit_daily_summaries(&mut self, workspace_id: Uuid, host_id: &str, now_ms: i64) -> Result<usize> {
        let today = hive_core::flow::utc_day(now_ms);
        let rows = self.store.facts_since(today - 14 * 86_400_000)?;
        let ws = workspace_id.to_string();
        let mut tags: Vec<(Uuid, TurnTag)> = Vec::new();
        let mut done: std::collections::BTreeSet<(String, i64)> = Default::default();
        for (_, w, sid, _, kind, json) in rows {
            if w != ws {
                continue;
            }
            if kind == SUMMARY_MARK {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) {
                    done.insert((v["agent"].as_str().unwrap_or_default().to_string(), v["day"].as_i64().unwrap_or_default()));
                }
                continue;
            }
            if let Ok(FlowRecord::TurnTagged(t)) = serde_json::from_str::<FlowRecord>(&json) {
                if t.host_id.is_empty() || t.host_id == host_id {
                    tags.push((Uuid::parse_str(&sid).unwrap_or_default(), t));
                }
            }
        }
        let mut todo: std::collections::BTreeSet<(String, i64)> = Default::default();
        for (_, t) in &tags {
            let day = hive_core::flow::utc_day(t.at_ms);
            if day < today && !done.contains(&(t.agent.clone(), day)) {
                todo.insert((t.agent.clone(), day));
            }
        }
        let mut emitted = 0;
        for (agent, day) in todo {
            let day_tags: Vec<TurnTag> = tags.iter().filter(|(_, t)| t.agent == agent).map(|(_, t)| t.clone()).collect();
            let chats: std::collections::BTreeSet<Uuid> = tags.iter().filter(|(_, t)| t.agent == agent).map(|(s, _)| *s).collect();
            let sessions: Vec<hive_core::ChatSession> = chats.iter().filter_map(|c| self.load(*c).ok().flatten()).collect();
            let proposals: Vec<&hive_core::ActionProposal> = sessions.iter().flat_map(|s| s.proposals.iter()).collect();
            let sum = hive_core::flow::score::summarize_day(&agent, host_id, day, &day_tags, &proposals);
            if sum.turns == 0 {
                continue;
            }
            self.record_flow(Uuid::nil(), workspace_id, FlowRecord::AgentOutcomeSummary(sum))?;
            let mark = serde_json::json!({ "agent": agent, "day": day }).to_string();
            self.store.record_fact(workspace_id, hive_core::workspace_config_session_id(workspace_id), day, SUMMARY_MARK, &mark)?;
            emitted += 1;
        }
        Ok(emitted)
    }

    /// This device's facts since `since_ms`, as `ligus.facts.v1` JSON lines.
    pub fn export_facts(&self, since_ms: i64) -> Result<Vec<String>> {
        let rows = self.store.facts_since(since_ms)?;
        let mut out = Vec::new();
        for (row, ws, sid, at_ms, kind, json) in rows {
            if kind == SUMMARY_MARK {
                continue;
            }
            if kind == crate::facts::TOOL_CALL_KIND {
                out.extend(crate::facts::tool_call_line(row, &ws, &sid, at_ms, &json));
                continue;
            }
            let Ok(record) = serde_json::from_str::<FlowRecord>(&json) else { continue };
            if let Some(v) = crate::facts::export_line(row, &ws, &sid, at_ms, &record) {
                out.push(v);
            }
        }
        Ok(out)
    }
}
