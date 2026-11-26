//! Device-local held events (never synced, never folded): events ingest
//! could not judge yet, kept aside so they never pin the sync cursor.
//!
//! Ingest holds an event it can't decide on yet (a device whose certificate
//! for the account it claims hasn't arrived, an account not yet admitted
//! here, a device waiting for a dearth-mode approval, a disputed account
//! key). The relay cursor used to stop just before such an event so it was
//! fetched again, which let one event whose certificate or admission never
//! comes (a forged founder, a member signing as an account that never
//! certifies them, a removed member never re-admitted) stop a device from
//! syncing anything after it, for good.
//!
//! Now the cursor moves past it and the event waits here. It is judged
//! again, exactly as it would be on arrival, whenever this device's roster
//! may have changed (a certificate, an admission, a key registration
//! applied) and on a slow timer ([`HELD_RETRY_EVERY`]); applied then, it is
//! stored like any other event, so the fold (a function of the stored set)
//! is the same as if it had arrived after what it waited for.
//!
//! Bounded, whoever floods it: at most [`HELD_PER_SIGNER`] events per signing
//! device and workspace, [`HELD_PER_WORKSPACE`] events and
//! [`HELD_BYTES_PER_WORKSPACE`] bytes per workspace, [`HELD_TOTAL`] events
//! and [`HELD_BYTES_TOTAL`] bytes on the device, and none older than
//! [`HELD_TTL_MS`]. Past a cap the heaviest signer's newest event makes
//! room (first come stays: a flood can't push out what already waits), or
//! the new one is dropped. Dropped and expired events are counted for sync
//! diagnostics and `ligus doctor`.
//!
//! **A dropped event isn't lost.** The relay still has it: an event that came
//! from the relay and leaves the table (dropped by a cap, or evicted) keeps a
//! small row in `held_refetch` (its relay sequence, signer and the account it
//! claims; no body). Whenever the roster changes, a row whose signer now
//! signs for the account it claimed (what it waited for arrived) is marked
//! due, and the sync engine fetches those sequences from the relay again and
//! judges them like any fetched event ([`HELD_REFETCH_TRIES`] times at most,
//! so a flood can't make a device fetch the same events forever). A flood
//! from devices nobody certifies is never fetched again. At most
//! [`HELD_REFETCH_PER_WORKSPACE`] rows per workspace; past that (or the
//! tries), an event is given up on this device, counted as `lost`.

use rusqlite::Connection;
use uuid::Uuid;

use super::{EventStore, Result};
use ligus_core::SessionEventEnvelope;

/// Held events kept per signing device and workspace.
pub const HELD_PER_SIGNER: i64 = 1_000;
/// Held events kept per workspace.
pub const HELD_PER_WORKSPACE: i64 = 10_000;
/// Bytes of held events kept per workspace.
pub const HELD_BYTES_PER_WORKSPACE: i64 = 32 * 1024 * 1024;
/// Held events kept on the device (all workspaces).
pub const HELD_TOTAL: i64 = 50_000;
/// Bytes of held events kept on the device (all workspaces).
pub const HELD_BYTES_TOTAL: i64 = 128 * 1024 * 1024;
/// A held event older than this (30 days) is dropped.
pub const HELD_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// Dropped events per workspace remembered for fetching again (rows of a
/// relay sequence and two ids, no body).
pub const HELD_REFETCH_PER_WORKSPACE: i64 = 100_000;
/// How many times a dropped event is fetched again before it is given up.
pub const HELD_REFETCH_TRIES: i64 = 3;
/// How often held events are judged again without a roster change.
pub const HELD_RETRY_EVERY: std::time::Duration = std::time::Duration::from_secs(10 * 60);

pub(super) fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        -- Device-local: events ingest holds (see event_store/held.rs). Keyed
        -- by a digest of the exact envelope text, so a re-delivery is one row.
        CREATE TABLE IF NOT EXISTS held_events (
            workspace_id  TEXT NOT NULL,
            digest        TEXT NOT NULL,
            event_id      TEXT NOT NULL,
            signer        TEXT NOT NULL,
            reason        TEXT NOT NULL,
            envelope_json TEXT NOT NULL,
            sealed_epoch  INTEGER,
            bytes         INTEGER NOT NULL,
            held_ms       INTEGER NOT NULL,
            PRIMARY KEY (workspace_id, digest)
        );
        CREATE INDEX IF NOT EXISTS idx_held_events_signer ON held_events (workspace_id, signer);
        CREATE INDEX IF NOT EXISTS idx_held_events_age ON held_events (held_ms);
        -- Device-local: events dropped from held_events that came from the
        -- relay, to fetch again by sequence once what they waited for arrives
        -- (see event_store/held.rs). No body.
        CREATE TABLE IF NOT EXISTS held_refetch (
            workspace_id TEXT NOT NULL,
            seq          INTEGER NOT NULL,
            signer       TEXT NOT NULL,
            claimed      TEXT NOT NULL,
            attempts     INTEGER NOT NULL DEFAULT 0,
            due          INTEGER NOT NULL DEFAULT 0,
            noted_ms     INTEGER NOT NULL,
            PRIMARY KEY (workspace_id, seq)
        );
        CREATE INDEX IF NOT EXISTS idx_held_refetch_age ON held_refetch (noted_ms);
        -- Held events this device dropped (a cap, or the age limit), per
        -- workspace: what sync diagnostics report.
        CREATE TABLE IF NOT EXISTS held_dropped (
            workspace_id TEXT PRIMARY KEY,
            dropped      INTEGER NOT NULL DEFAULT 0,
            expired      INTEGER NOT NULL DEFAULT 0
        );
        "#,
    )?;
    // Columns added after the table first shipped on a branch build: the
    // relay sequence a held event came at (none over P2P) and the account
    // it claims, and the count of dropped events given up for good.
    for (table, col, ty) in [("held_events", "seq", "INTEGER"), ("held_events", "claimed", "TEXT NOT NULL DEFAULT ''"), ("held_dropped", "lost", "INTEGER NOT NULL DEFAULT 0")] {
        let has: bool = conn
            .prepare(&format!("SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1"))?
            .exists([col])?;
        if !has {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {col} {ty}"))?;
        }
    }
    conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_held_events_seq ON held_events (workspace_id, seq)")
}

/// The caps held events are kept under.
struct Caps {
    per_signer: i64,
    per_workspace: i64,
    bytes_per_workspace: i64,
    total: i64,
    bytes_total: i64,
    refetch_per_workspace: i64,
}

impl Caps {
    const DEFAULT: Caps = Caps {
        per_signer: HELD_PER_SIGNER,
        per_workspace: HELD_PER_WORKSPACE,
        bytes_per_workspace: HELD_BYTES_PER_WORKSPACE,
        total: HELD_TOTAL,
        bytes_total: HELD_BYTES_TOTAL,
        refetch_per_workspace: HELD_REFETCH_PER_WORKSPACE,
    };
}

/// One event to hold.
pub struct HoldItem<'a> {
    pub env: &'a SessionEventEnvelope,
    pub reason: &'a str,
    /// The relay sequence it came at (none over P2P): a dropped one is
    /// fetched again from there.
    pub seq: Option<u64>,
}

/// A dropped event to fetch again (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefetchRow {
    pub seq: u64,
    pub signer: Option<Uuid>,
    /// The account the event's stamp claims, if any.
    pub claimed: Option<Uuid>,
}

/// The account an event's stamp claims: its account id, or an actor id
/// that is one.
pub fn claimed_account(env: &SessionEventEnvelope) -> Option<Uuid> {
    let s = env.actor_stamp.as_ref()?;
    s.actor.account_id.or_else(|| Uuid::parse_str(&s.actor.id).ok())
}

/// One held event, as it arrived.
#[derive(Debug, Clone)]
pub struct HeldEvent {
    pub digest: String,
    pub env: SessionEventEnvelope,
    pub held_ms: i64,
}

/// What a workspace holds (sync diagnostics, `ligus doctor`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldStats {
    /// Events held now.
    pub held: i64,
    /// Distinct signing devices among them.
    pub signers: i64,
    /// Bytes they take.
    pub bytes: i64,
    /// Events dropped because a cap was reached.
    pub dropped: i64,
    /// Events dropped because they waited longer than 30 days.
    pub expired: i64,
    /// Dropped events waiting to be fetched again from the relay.
    pub refetch: i64,
    /// Dropped events given up for good (past the refetch rows' cap or
    /// tries): missing on this device.
    pub lost: i64,
    /// Each held reason with its count.
    pub reasons: Vec<(String, i64)>,
}

fn digest(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

impl EventStore {
    /// Hold `items` of `workspace` (see the module docs), at `now_ms`.
    /// Returns how many are held now (a re-delivery of one already held
    /// counts, and keeps its first time).
    pub fn hold_events(&self, workspace: Uuid, items: &[HoldItem<'_>], now_ms: i64) -> Result<usize> {
        self.hold_events_capped(workspace, items, now_ms, &Caps::DEFAULT)
    }

    fn hold_events_capped(&self, workspace: Uuid, items: &[HoldItem<'_>], now_ms: i64, caps: &Caps) -> Result<usize> {
        if items.is_empty() {
            return Ok(0);
        }
        let ws = workspace.to_string();
        let tx = super::write_tx(&self.conn)?;
        let mut held = 0;
        let mut dropped = 0i64;
        for item in items {
            let json = serde_json::to_string(item.env)?;
            let d = digest(&json);
            let exists: bool = tx
                .prepare_cached("SELECT 1 FROM held_events WHERE workspace_id = ?1 AND digest = ?2")?
                .exists(rusqlite::params![ws, d])?;
            if exists {
                held += 1;
                continue;
            }
            let signer = item.env.signer_device_id.map(|s| s.to_string()).unwrap_or_default();
            let claimed = claimed_account(item.env).map(|a| a.to_string()).unwrap_or_default();
            let bytes = json.len() as i64;
            if !make_room(&tx, &ws, &signer, bytes, caps, now_ms)? {
                dropped += 1;
                if let Some(seq) = item.seq {
                    note_refetch(&tx, &ws, seq, &signer, &claimed, now_ms, caps)?;
                }
                continue;
            }
            tx.execute(
                "INSERT INTO held_events (workspace_id, digest, event_id, signer, reason, envelope_json, sealed_epoch, bytes, held_ms, seq, claimed) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    ws,
                    d,
                    item.env.event_id.to_string(),
                    signer,
                    item.reason,
                    json,
                    item.env.sealed_epoch().map(i64::from),
                    bytes,
                    now_ms,
                    item.seq.map(|s| s as i64),
                    claimed
                ],
            )?;
            held += 1;
        }
        if dropped > 0 {
            note_dropped(&tx, &ws, dropped, 0)?;
        }
        tx.commit()?;
        Ok(held)
    }

    /// Every event `workspace` holds, oldest first.
    pub fn held_events(&self, workspace: Uuid) -> Result<Vec<HeldEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT digest, envelope_json, sealed_epoch, held_ms FROM held_events WHERE workspace_id = ?1 ORDER BY held_ms ASC, digest ASC",
        )?;
        let rows = stmt.query_map([workspace.to_string()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?, r.get::<_, i64>(3)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (digest, json, epoch, held_ms) = row?;
            if let Ok(mut env) = serde_json::from_str::<SessionEventEnvelope>(&json) {
                env.set_sealed_epoch(epoch.map(|e| e as u32));
                out.push(HeldEvent { digest, env, held_ms });
            }
        }
        Ok(out)
    }

    /// Every workspace with held events.
    pub fn held_workspaces(&self) -> Result<Vec<Uuid>> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT workspace_id FROM held_events ORDER BY workspace_id")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            if let Ok(id) = Uuid::parse_str(&r?) {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// A signature of what `workspace`'s trust roster is built from (its
    /// config log and its trust events anywhere, equivocating copies
    /// included): any new row changes it, so held events are due again.
    pub fn trust_rows_sig(&self, workspace: Uuid) -> Result<(i64, i64)> {
        let ws = workspace.to_string();
        let cfg = ligus_core::workspace_config_session_id(workspace).to_string();
        let q = "SELECT COUNT(*), COALESCE(MAX(row_id), 0) FROM events WHERE workspace_id = ?1 \
             AND (session_id = ?2 OR kind IN ('accountKeyRegistered','accountKeyRotated','deviceCertificateAdded','deviceRevoked',\
             'memberAdded','memberRemoved','sessionSnapshot','memberRoleChanged','dearthModeChanged','joinApproved',\
             'agentKeyCertified','agentUpserted','agentRosterUpdated','agentRemoved',\
             'workspaceRulesSet','passkeyRegistered','passkeyRemoved'))";
        let (n, max): (i64, i64) = self.conn.query_row(q, rusqlite::params![ws, cfg], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let eq: (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(MAX(row_id), 0) FROM equivocations WHERE workspace_id = ?1",
            [&ws],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((n + eq.0, max ^ eq.1.rotate_left(32)))
    }

    /// Forget held events (applied or refused since).
    pub fn release_held(&self, workspace: Uuid, digests: &[String]) -> Result<()> {
        if digests.is_empty() {
            return Ok(());
        }
        let ws = workspace.to_string();
        let tx = super::write_tx(&self.conn)?;
        for d in digests {
            // Judged now: no longer to be fetched again either.
            tx.execute(
                "DELETE FROM held_refetch WHERE workspace_id = ?1 AND seq = (SELECT seq FROM held_events WHERE workspace_id = ?1 AND digest = ?2)",
                rusqlite::params![ws, d],
            )?;
            tx.execute("DELETE FROM held_events WHERE workspace_id = ?1 AND digest = ?2", rusqlite::params![ws, d])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Dropped events of `workspace` that may be due for fetching again:
    /// not marked yet, tries left, and not held again meanwhile.
    pub fn held_refetch_candidates(&self, workspace: Uuid) -> Result<Vec<RefetchRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, signer, claimed FROM held_refetch r WHERE workspace_id = ?1 AND due = 0 AND attempts < ?2 \
             AND NOT EXISTS (SELECT 1 FROM held_events h WHERE h.workspace_id = r.workspace_id AND h.seq = r.seq) ORDER BY seq",
        )?;
        let rows = stmt.query_map(rusqlite::params![workspace.to_string(), HELD_REFETCH_TRIES], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, signer, claimed) = row?;
            out.push(RefetchRow { seq: seq as u64, signer: Uuid::parse_str(&signer).ok(), claimed: Uuid::parse_str(&claimed).ok() });
        }
        Ok(out)
    }

    /// Mark dropped events of `workspace` due for fetching again.
    pub fn mark_refetch_due(&self, workspace: Uuid, seqs: &[u64]) -> Result<()> {
        let ws = workspace.to_string();
        let tx = super::write_tx(&self.conn)?;
        for s in seqs {
            tx.execute("UPDATE held_refetch SET due = 1 WHERE workspace_id = ?1 AND seq = ?2", rusqlite::params![ws, *s as i64])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Up to `limit` relay sequences of `workspace` due for fetching again,
    /// lowest first.
    pub fn held_refetch_due(&self, workspace: Uuid, limit: usize) -> Result<Vec<u64>> {
        let mut stmt = self.conn.prepare("SELECT seq FROM held_refetch WHERE workspace_id = ?1 AND due = 1 ORDER BY seq LIMIT ?2")?;
        let rows = stmt.query_map(rusqlite::params![workspace.to_string(), limit as i64], |r| r.get::<_, i64>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?.into_iter().map(|s| s as u64).collect())
    }

    /// `seqs` of `workspace` are being fetched again: one try spent each,
    /// not due until the roster changes again.
    pub fn held_refetch_tried(&self, workspace: Uuid, seqs: &[u64]) -> Result<()> {
        let ws = workspace.to_string();
        let tx = super::write_tx(&self.conn)?;
        for s in seqs {
            tx.execute(
                "UPDATE held_refetch SET due = 0, attempts = attempts + 1 WHERE workspace_id = ?1 AND seq = ?2",
                rusqlite::params![ws, *s as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The event fetched again at `seq` was judged (applied or refused):
    /// nothing more to fetch.
    pub fn held_refetch_done(&self, workspace: Uuid, seq: u64) -> Result<()> {
        self.conn.execute("DELETE FROM held_refetch WHERE workspace_id = ?1 AND seq = ?2", rusqlite::params![workspace.to_string(), seq as i64])?;
        Ok(())
    }

    /// Give up the dropped events of `workspace` that used every try and
    /// aren't held again (counted as lost).
    pub fn prune_held_refetch(&self, workspace: Uuid) -> Result<usize> {
        let ws = workspace.to_string();
        let tx = super::write_tx(&self.conn)?;
        let n = tx.execute(
            "DELETE FROM held_refetch WHERE workspace_id = ?1 AND attempts >= ?2 AND due = 0 \
             AND NOT EXISTS (SELECT 1 FROM held_events h WHERE h.workspace_id = held_refetch.workspace_id AND h.seq = held_refetch.seq)",
            rusqlite::params![ws, HELD_REFETCH_TRIES],
        )?;
        if n > 0 {
            note_lost(&tx, &ws, n as i64)?;
        }
        tx.commit()?;
        Ok(n)
    }

    /// Drop held events older than [`HELD_TTL_MS`] at `now_ms`; returns how
    /// many went.
    pub fn expire_held(&self, now_ms: i64) -> Result<usize> {
        let cutoff = now_ms.saturating_sub(HELD_TTL_MS);
        let tx = super::write_tx(&self.conn)?;
        let per_ws: Vec<(String, i64)> = {
            let mut stmt = tx.prepare("SELECT workspace_id, COUNT(*) FROM held_events WHERE held_ms < ?1 GROUP BY workspace_id")?;
            let rows = stmt.query_map([cutoff], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let n = tx.execute("DELETE FROM held_events WHERE held_ms < ?1", [cutoff])?;
        for (ws, count) in per_ws {
            note_dropped(&tx, &ws, 0, count)?;
        }
        // A dropped event that waited as long: given up (the relay may have
        // expired it too).
        let stale: Vec<(String, i64)> = {
            let mut stmt = tx.prepare("SELECT workspace_id, COUNT(*) FROM held_refetch WHERE noted_ms < ?1 GROUP BY workspace_id")?;
            let rows = stmt.query_map([cutoff], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        tx.execute("DELETE FROM held_refetch WHERE noted_ms < ?1", [cutoff])?;
        for (ws, count) in stale {
            note_lost(&tx, &ws, count)?;
        }
        tx.commit()?;
        Ok(n)
    }

    /// What `workspace` holds and has dropped.
    pub fn held_stats(&self, workspace: Uuid) -> Result<HeldStats> {
        let ws = workspace.to_string();
        let (held, signers, bytes): (i64, i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COUNT(DISTINCT signer), COALESCE(SUM(bytes), 0) FROM held_events WHERE workspace_id = ?1",
            [&ws],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let (dropped, expired, lost): (i64, i64, i64) = self
            .conn
            .query_row("SELECT dropped, expired, lost FROM held_dropped WHERE workspace_id = ?1", [&ws], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap_or((0, 0, 0));
        let refetch: i64 = self.conn.query_row("SELECT COUNT(*) FROM held_refetch WHERE workspace_id = ?1", [&ws], |r| r.get(0))?;
        let mut stmt = self.conn.prepare(
            "SELECT reason, COUNT(*) FROM held_events WHERE workspace_id = ?1 GROUP BY reason ORDER BY COUNT(*) DESC, reason ASC",
        )?;
        let reasons = stmt
            .query_map([&ws], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(HeldStats { held, signers, bytes, dropped, expired, refetch, lost, reasons })
    }
}

fn note_lost(conn: &Connection, ws: &str, lost: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO held_dropped (workspace_id, lost) VALUES (?1, ?2) ON CONFLICT(workspace_id) DO UPDATE SET lost = lost + ?2",
        rusqlite::params![ws, lost],
    )?;
    Ok(())
}

/// Remember a dropped relay event (`seq`) of `ws` for fetching again, within
/// the cap (past it, lost). An event dropped again keeps its tries.
fn note_refetch(conn: &Connection, ws: &str, seq: u64, signer: &str, claimed: &str, now_ms: i64, caps: &Caps) -> rusqlite::Result<()> {
    let known: bool = conn
        .prepare_cached("SELECT 1 FROM held_refetch WHERE workspace_id = ?1 AND seq = ?2")?
        .exists(rusqlite::params![ws, seq as i64])?;
    if known {
        return Ok(());
    }
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM held_refetch WHERE workspace_id = ?1", [ws], |r| r.get(0))?;
    if n >= caps.refetch_per_workspace {
        return note_lost(conn, ws, 1);
    }
    conn.execute(
        "INSERT INTO held_refetch (workspace_id, seq, signer, claimed, noted_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![ws, seq as i64, signer, claimed, now_ms],
    )?;
    Ok(())
}

fn note_dropped(conn: &Connection, ws: &str, dropped: i64, expired: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO held_dropped (workspace_id, dropped, expired) VALUES (?1, ?2, ?3) \
         ON CONFLICT(workspace_id) DO UPDATE SET dropped = dropped + ?2, expired = expired + ?3",
        rusqlite::params![ws, dropped, expired],
    )?;
    Ok(())
}

/// Make room for one more held event of `signer` (`bytes` long) in `ws`:
/// within the per-signer cap, then the workspace's and the device's caps,
/// each by evicting the newest event of the signer holding the most (only a
/// signer strictly heavier than this one would become). `false`: no room,
/// drop the new one.
fn make_room(conn: &Connection, ws: &str, signer: &str, bytes: i64, caps: &Caps, now_ms: i64) -> rusqlite::Result<bool> {
    let mine: i64 = conn.query_row(
        "SELECT COUNT(*) FROM held_events WHERE workspace_id = ?1 AND signer = ?2",
        rusqlite::params![ws, signer],
        |r| r.get(0),
    )?;
    if mine >= caps.per_signer || bytes > caps.bytes_per_workspace {
        return Ok(false);
    }
    // The workspace's caps, then the device's.
    for scope_ws in [Some(ws), None] {
        loop {
            let (n, b): (i64, i64) = match scope_ws {
                Some(w) => conn.query_row(
                    "SELECT COUNT(*), COALESCE(SUM(bytes), 0) FROM held_events WHERE workspace_id = ?1",
                    [w],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?,
                None => conn.query_row("SELECT COUNT(*), COALESCE(SUM(bytes), 0) FROM held_events", [], |r| Ok((r.get(0)?, r.get(1)?)))?,
            };
            let (cap_n, cap_b) = match scope_ws {
                Some(_) => (caps.per_workspace, caps.bytes_per_workspace),
                None => (caps.total, caps.bytes_total),
            };
            if n < cap_n && b + bytes <= cap_b {
                break;
            }
            // The heaviest signer (most held), newest-held first on a tie.
            let heaviest: Option<(String, String, i64)> = match scope_ws {
                Some(w) => conn
                    .query_row(
                        "SELECT workspace_id, signer, COUNT(*) AS c FROM held_events WHERE workspace_id = ?1 \
                         GROUP BY signer ORDER BY c DESC, MAX(held_ms) DESC, signer DESC LIMIT 1",
                        [w],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .ok(),
                None => conn
                    .query_row(
                        "SELECT workspace_id, signer, COUNT(*) AS c FROM held_events \
                         GROUP BY workspace_id, signer ORDER BY c DESC, MAX(held_ms) DESC, signer DESC LIMIT 1",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .ok(),
            };
            let Some((hws, hsigner, count)) = heaviest else { return Ok(false) };
            // Only a signer strictly heavier than this one would become gives
            // way: first come stays.
            if count <= mine + 1 {
                return Ok(false);
            }
            let victim: (i64, Option<i64>, String) = conn.query_row(
                "SELECT rowid, seq, claimed FROM held_events WHERE workspace_id = ?1 AND signer = ?2 ORDER BY held_ms DESC, digest DESC LIMIT 1",
                rusqlite::params![hws, hsigner],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            conn.execute("DELETE FROM held_events WHERE rowid = ?1", [victim.0])?;
            note_dropped(conn, &hws, 1, 0)?;
            if let Some(seq) = victim.1 {
                note_refetch(conn, &hws, seq as u64, &hsigner, &victim.2, now_ms, caps)?;
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ligus_core::{SessionEvent, SessionEventEnvelope};

    fn env(signer: Uuid, n: u64) -> SessionEventEnvelope {
        let mut e = SessionEventEnvelope::new(Uuid::nil(), Uuid::nil(), n as i64, SessionEvent::SessionTitleChanged { title: format!("t{n}") });
        e.signer_device_id = Some(signer);
        e.signature = Some(vec![0; 64]);
        e
    }

    fn hold(store: &EventStore, ws: Uuid, envs: &[SessionEventEnvelope], now: i64, caps: &Caps) {
        let items: Vec<HoldItem<'_>> = envs.iter().map(|e| HoldItem { env: e, reason: "test", seq: None }).collect();
        store.hold_events_capped(ws, &items, now, caps).unwrap();
    }

    const SMALL: Caps = Caps { per_signer: 5, per_workspace: 12, bytes_per_workspace: 1 << 20, total: 20, bytes_total: 1 << 20, refetch_per_workspace: 30 };

    #[test]
    fn a_redelivery_is_one_row_and_keeps_its_first_time() {
        let store = EventStore::open_in_memory().unwrap();
        let ws = Uuid::new_v4();
        let e = env(Uuid::new_v4(), 1);
        hold(&store, ws, std::slice::from_ref(&e), 10, &SMALL);
        hold(&store, ws, std::slice::from_ref(&e), 20, &SMALL);
        let held = store.held_events(ws).unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].held_ms, 10);
        assert_eq!(held[0].env.event_id, e.event_id);
    }

    #[test]
    fn one_signer_is_capped_and_cant_push_out_others() {
        let store = EventStore::open_in_memory().unwrap();
        let ws = Uuid::new_v4();
        let honest = Uuid::new_v4();
        hold(&store, ws, &[env(honest, 0), env(honest, 1)], 1, &SMALL);
        let flooder = Uuid::new_v4();
        let flood: Vec<_> = (0..50).map(|n| env(flooder, n)).collect();
        hold(&store, ws, &flood, 2, &SMALL);
        let st = store.held_stats(ws).unwrap();
        assert_eq!(st.held, 2 + SMALL.per_signer);
        assert_eq!(st.dropped, 50 - SMALL.per_signer);
        assert_eq!(store.held_events(ws).unwrap().iter().filter(|h| h.env.signer_device_id == Some(honest)).count(), 2);
    }

    #[test]
    fn many_signers_fill_the_workspace_cap_first_come_stays() {
        let store = EventStore::open_in_memory().unwrap();
        let ws = Uuid::new_v4();
        let honest = Uuid::new_v4();
        hold(&store, ws, &[env(honest, 0)], 1, &SMALL);
        // A heavy flooder first, then many one-event signers.
        let heavy = Uuid::new_v4();
        hold(&store, ws, &(0..5).map(|n| env(heavy, n)).collect::<Vec<_>>(), 2, &SMALL);
        let many: Vec<_> = (0..40).map(|n| env(Uuid::new_v4(), n)).collect();
        hold(&store, ws, &many, 3, &SMALL);
        let st = store.held_stats(ws).unwrap();
        assert_eq!(st.held, SMALL.per_workspace, "the workspace cap holds");
        let held = store.held_events(ws).unwrap();
        assert!(held.iter().any(|h| h.env.signer_device_id == Some(honest)), "first come stays");
        // The heavy signer gave way to the newcomers down to one event each.
        assert!(held.iter().filter(|h| h.env.signer_device_id == Some(heavy)).count() <= 1);
        assert_eq!(st.dropped, 1 + 5 + 40 - SMALL.per_workspace);
    }

    #[test]
    fn the_device_cap_spans_workspaces() {
        let store = EventStore::open_in_memory().unwrap();
        for _ in 0..4 {
            let ws = Uuid::new_v4();
            let envs: Vec<_> = (0..10).map(|n| env(Uuid::new_v4(), n)).collect();
            hold(&store, ws, &envs, 1, &SMALL);
        }
        let total: i64 = store.conn.query_row("SELECT COUNT(*) FROM held_events", [], |r| r.get(0)).unwrap();
        assert_eq!(total, SMALL.total);
    }

    #[test]
    fn held_events_expire_after_thirty_days() {
        let store = EventStore::open_in_memory().unwrap();
        let ws = Uuid::new_v4();
        let now = 100 * 24 * 60 * 60 * 1000;
        hold(&store, ws, &[env(Uuid::new_v4(), 0)], now - HELD_TTL_MS - 1, &SMALL);
        hold(&store, ws, &[env(Uuid::new_v4(), 1)], now - HELD_TTL_MS + 1000, &SMALL);
        assert_eq!(store.expire_held(now).unwrap(), 1);
        let st = store.held_stats(ws).unwrap();
        assert_eq!((st.held, st.expired), (1, 1));
        assert_eq!(store.held_workspaces().unwrap(), vec![ws]);
    }

    #[test]
    fn the_sealed_epoch_survives_holding() {
        let store = EventStore::open_in_memory().unwrap();
        let ws = Uuid::new_v4();
        let mut e = env(Uuid::new_v4(), 0);
        e.set_sealed_epoch(Some(3));
        hold(&store, ws, &[e], 1, &SMALL);
        assert_eq!(store.held_events(ws).unwrap()[0].env.sealed_epoch(), Some(3));
    }
}
