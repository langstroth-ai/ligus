//! Device-local agent usage (agent access controls): per agent, per member,
//! per UTC day, the turns this device or worker ran for the agent and the
//! estimated cost the runtime reported. Never synced, never in an event, a
//! beat or a relay body: each device that runs an agent counts its own
//! turns. Read by the daily budgets (`agent_access`) and the usage view.

use rusqlite::{params, Connection};
use uuid::Uuid;

use super::{EventStore, Result};

/// Rows older than this many days are pruned (the usage view shows a month).
const KEEP_DAYS: i64 = 400;

pub(super) fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS agent_usage (
            day          TEXT NOT NULL,
            workspace_id TEXT NOT NULL,
            agent_id     TEXT NOT NULL,
            member       TEXT NOT NULL,
            turns        INTEGER NOT NULL DEFAULT 0,
            cost_micros  INTEGER NOT NULL DEFAULT 0,
            uncosted     INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (day, agent_id, member)
        );
        CREATE INDEX IF NOT EXISTS idx_agent_usage_agent ON agent_usage (agent_id, day);
        "#,
    )
}

/// One member's use of one agent over a span of days.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentUsageRow {
    /// The member (empty: a turn no person could be traced to).
    pub member: String,
    pub turns: u64,
    /// Estimated USD, from the runtime's reported token usage.
    pub cost_usd: f64,
    /// Turns whose runtime reported no usage (not in `cost_usd`).
    pub uncosted: u64,
}

/// Today's UTC day, `YYYY-MM-DD`.
pub fn utc_day(at: time::OffsetDateTime) -> String {
    let d = at.to_offset(time::UtcOffset::UTC).date();
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

/// `YYYY-MM-DD` back to a date.
pub fn parse_day(day: &str) -> Option<time::Date> {
    let mut it = day.splitn(3, '-');
    let y: i32 = it.next()?.parse().ok()?;
    let m: u8 = it.next()?.parse().ok()?;
    let d: u8 = it.next()?.parse().ok()?;
    time::Date::from_calendar_date(y, time::Month::try_from(m).ok()?, d).ok()
}

impl EventStore {
    /// Count one turn of `agent` for `member` on `day`.
    pub fn record_agent_turn(&self, workspace_id: Uuid, agent: Uuid, member: &str, day: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO agent_usage (day, workspace_id, agent_id, member, turns) VALUES (?1, ?2, ?3, ?4, 1) \
             ON CONFLICT (day, agent_id, member) DO UPDATE SET turns = turns + 1",
            params![day, workspace_id.to_string(), agent.to_string(), member],
        )?;
        self.prune_agent_usage(day)?;
        Ok(())
    }

    /// Add a finished turn's estimated cost (`None`: the runtime reported no
    /// usage, counted as an uncosted turn).
    pub fn add_agent_cost(&self, workspace_id: Uuid, agent: Uuid, member: &str, day: &str, usd: Option<f64>) -> Result<()> {
        let (micros, uncosted) = match usd.filter(|u| u.is_finite() && *u >= 0.0) {
            Some(u) => ((u * 1_000_000.0).round().min(i64::MAX as f64) as i64, 0),
            None => (0, 1),
        };
        self.conn.execute(
            "INSERT INTO agent_usage (day, workspace_id, agent_id, member, cost_micros, uncosted) VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT (day, agent_id, member) DO UPDATE SET cost_micros = cost_micros + ?5, uncosted = uncosted + ?6",
            params![day, workspace_id.to_string(), agent.to_string(), member, micros, uncosted],
        )?;
        Ok(())
    }

    /// `agent`'s use per member on days `from..=to` (`YYYY-MM-DD`).
    pub fn agent_usage(&self, agent: Uuid, from: &str, to: &str) -> Result<Vec<AgentUsageRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT member, SUM(turns), SUM(cost_micros), SUM(uncosted) FROM agent_usage \
             WHERE agent_id = ?1 AND day >= ?2 AND day <= ?3 GROUP BY member ORDER BY member",
        )?;
        let rows = stmt
            .query_map(params![agent.to_string(), from, to], |r| {
                Ok(AgentUsageRow {
                    member: r.get(0)?,
                    turns: r.get::<_, i64>(1)?.max(0) as u64,
                    cost_usd: r.get::<_, i64>(2)?.max(0) as f64 / 1_000_000.0,
                    uncosted: r.get::<_, i64>(3)?.max(0) as u64,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn prune_agent_usage(&self, today: &str) -> Result<()> {
        let Some(d) = parse_day(today) else { return Ok(()) };
        let cutoff = d - time::Duration::days(KEEP_DAYS);
        let cutoff = format!("{:04}-{:02}-{:02}", cutoff.year(), u8::from(cutoff.month()), cutoff.day());
        self.conn.execute("DELETE FROM agent_usage WHERE day < ?1", params![cutoff])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turns_and_costs_add_up_per_member_and_day() {
        let store = EventStore::open_in_memory().unwrap();
        let (ws, a, b) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        store.record_agent_turn(ws, a, "mara", "2026-11-20").unwrap();
        store.record_agent_turn(ws, a, "mara", "2026-11-20").unwrap();
        store.add_agent_cost(ws, a, "mara", "2026-11-20", Some(0.25)).unwrap();
        store.add_agent_cost(ws, a, "mara", "2026-11-20", None).unwrap();
        store.record_agent_turn(ws, a, "carl", "2026-11-19").unwrap();
        store.record_agent_turn(ws, b, "mara", "2026-11-20").unwrap();
        let today = store.agent_usage(a, "2026-11-20", "2026-11-20").unwrap();
        assert_eq!(today, vec![AgentUsageRow { member: "mara".into(), turns: 2, cost_usd: 0.25, uncosted: 1 }]);
        let month = store.agent_usage(a, "2026-11-01", "2026-11-30").unwrap();
        assert_eq!(month.iter().map(|r| (r.member.as_str(), r.turns)).collect::<Vec<_>>(), vec![("carl", 1), ("mara", 2)]);
        // A new day starts from zero.
        assert!(store.agent_usage(a, "2026-11-21", "2026-11-21").unwrap().is_empty());
        // Old rows go.
        store.record_agent_turn(ws, a, "mara", "2028-01-01").unwrap();
        assert!(store.agent_usage(a, "2026-01-01", "2026-12-31").unwrap().is_empty());
    }

    #[test]
    fn utc_days() {
        let t = time::OffsetDateTime::from_unix_timestamp(1_795_000_000).unwrap();
        assert_eq!(utc_day(t), "2026-11-18");
        let local = t.to_offset(time::UtcOffset::from_hms(-10, 0, 0).unwrap());
        assert_eq!(utc_day(local), "2026-11-18", "a local offset doesn't move the UTC day");
    }
}
