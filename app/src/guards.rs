//! Desktop side of the workspace guards. This commit: coordinator leases for
//! workflow runs (the first user of `hive_core::lease`).
//!
//! * A device drives a run only while it holds the run's lease. Starting a run
//!   claims epoch 1; resuming or recovering reuses this device's epoch, or
//!   claims the next one only when the holder's heartbeat has lapsed, it
//!   released, or it handed the run to this device.
//! * The holder renews by publishing a signed heartbeat to the relay's presence
//!   channel (`hive_runtime::lease_presence`); the lease loop here publishes
//!   ours and reads everyone else's.
//! * Every run write carries the epoch (`WorkflowRun::lease_epoch`), so a
//!   device that kept driving after another took over (split brain) has its
//!   writes fenced in projection, and its driver stops at the next transition.
//! * Supersedure: the holder can hand a run to another online device on
//!   purpose (`hand_over_workflow_run`), which reserves the next epoch for it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use hive_core::lease::{
    lease_health, LeaseClaimReason, LeaseHealth, LeaseScope, LeaseState, DEFAULT_BEAT_EVERY_MS,
    DEFAULT_LEASE_MS,
};
use hive_core::signal::GuardSignal;
use hive_core::workflow::WorkflowRunStatus;
use hive_proto::RunLeaseDto;
use hive_runtime::lease_presence::{BeatTrust, LeaseBeat, LeaseBeats};
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

use crate::{map_err, AppState};

/// How long a successor waits after claiming before it drives, so a competing
/// claim from another device has a sync round to arrive and fence one of them.
const SUCCESSION_SETTLE: Duration = Duration::from_secs(8);
/// Lease loop tick (heartbeats are due every `DEFAULT_BEAT_EVERY_MS`).
const LEASE_TICK: Duration = Duration::from_secs(5);

/// A lease this device holds and is driving under.
#[derive(Debug, Clone)]
pub(crate) struct HeldLease {
    pub session_id: Uuid,
    pub workspace_id: Uuid,
    pub epoch: u64,
    pub beat: u64,
    pub last_beat: Option<std::time::Instant>,
}

/// Guard state the desktop keeps in memory.
#[derive(Default)]
pub(crate) struct GuardRuntime {
    /// Newest verified heartbeat per scope.
    pub lease_beats: LeaseBeats,
    /// Workflow-run leases this device holds, by run id.
    pub held_runs: Mutex<HashMap<Uuid, HeldLease>>,
    /// Whether the lease loop has read the relay's heartbeats at least once
    /// since launch. Until then another device's lease is never judged lapsed
    /// (we simply haven't heard from it yet).
    pub beats_fetched: AtomicBool,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn device_id(state: &AppState) -> Uuid {
    state.service.lock().unwrap().device_id()
}

/// Whether this device may take over a run lease held elsewhere, and why.
/// `initiator_here` staggers succession: the devices of whoever started the
/// run succeed after one lapsed lease, everyone else only after two, so the
/// likely successor usually claims alone (the bee result: staggered thresholds
/// make the colony's response smooth instead of everyone acting at once).
pub(crate) fn takeover_reason(
    lease: &LeaseState,
    me: Uuid,
    last_beat: Option<(Uuid, u64, i64)>,
    now_ms: i64,
    beats_fetched: bool,
    initiator_here: bool,
) -> Option<LeaseClaimReason> {
    if lease.handover_to == Some(me) {
        return Some(LeaseClaimReason::Handover);
    }
    if lease.handover_to.is_some() {
        // Reserved for someone else; only a second lapse (handled below via the
        // later epoch) lets anyone else in.
        return (beats_fetched && lapsed_for(lease, last_beat, now_ms, 2)).then_some(LeaseClaimReason::Lapsed);
    }
    match lease_health(lease, last_beat, now_ms) {
        LeaseHealth::Released => Some(LeaseClaimReason::Released),
        LeaseHealth::Lapsed if beats_fetched => {
            let leases = if initiator_here { 1 } else { 2 };
            lapsed_for(lease, last_beat, now_ms, leases).then_some(LeaseClaimReason::Lapsed)
        }
        _ => None,
    }
}

/// No beat for `leases` whole lease lengths.
fn lapsed_for(lease: &LeaseState, last_beat: Option<(Uuid, u64, i64)>, now_ms: i64, leases: i64) -> bool {
    let claimed = lease.current.claimed_at.inner().unix_timestamp() * 1000;
    let beat = match last_beat {
        Some((h, e, at)) if h == lease.current.holder && e == lease.current.epoch => at.max(claimed),
        _ => claimed,
    };
    now_ms - beat > lease.current.lease_ms as i64 * leases
}

/// Make sure this device holds the lease of `run_id` before driving it.
/// Returns the epoch to drive under and whether this was a succession (the
/// caller then waits [`SUCCESSION_SETTLE`] and re-checks).
pub(crate) fn acquire_run_lease(
    state: &AppState,
    session_id: Uuid,
    workspace_id: Uuid,
    run_id: Uuid,
) -> Result<(u64, bool), String> {
    let me = device_id(state);
    let scope_id = run_id.to_string();
    let (lease, initiator_here) = {
        let svc = state.service.lock().unwrap();
        let session = svc.load(session_id).map_err(map_err)?.ok_or("unknown session")?;
        let initiator = session
            .workflow_runs
            .iter()
            .find(|r| r.id == run_id)
            .map(|r| r.initiator_actor_id.clone())
            .unwrap_or_default();
        let lease = session
            .guards
            .leases
            .get(&hive_core::lease::lease_key(LeaseScope::WorkflowRun, &scope_id))
            .cloned();
        (lease, initiator == svc.author().id)
    };
    let (epoch, reason) = match &lease {
        None => (1, LeaseClaimReason::Initial),
        Some(l) if l.current.holder == me && !l.released => {
            remember(state, run_id, session_id, workspace_id, l.current.epoch);
            return Ok((l.current.epoch, false));
        }
        Some(l) => {
            let last = state.guards.lease_beats.last(LeaseScope::WorkflowRun, &scope_id);
            let fetched = state.guards.beats_fetched.load(Ordering::Relaxed) || !relay_configured(state);
            match takeover_reason(l, me, last, now_ms(), fetched, initiator_here) {
                // A pending handover reserved the next epoch for its successor;
                // anyone else (after the successor failed to show) skips it.
                Some(reason) => {
                    let skip = if l.handover_to.is_some_and(|s| s != me) { 2 } else { 1 };
                    (l.current.epoch + skip, reason)
                }
                None => {
                    return Err(format!(
                        "another device is driving this run (lease epoch {}, still beating)",
                        l.current.epoch
                    ))
                }
            }
        }
    };
    let after = {
        let mut svc = state.service.lock().unwrap();
        svc.claim_lease(session_id, workspace_id, LeaseScope::WorkflowRun, &scope_id, epoch, DEFAULT_LEASE_MS, reason)
            .map_err(map_err)?
    };
    match after {
        Some(l) if l.current.holder == me && l.current.epoch == epoch => {
            remember(state, run_id, session_id, workspace_id, epoch);
            Ok((epoch, reason != LeaseClaimReason::Initial))
        }
        Some(l) => {
            let _ = state.service.lock().unwrap().record_signal(
                workspace_id,
                Some(session_id),
                &GuardSignal::LeaseFenced {
                    scope: LeaseScope::WorkflowRun,
                    scope_id,
                    holder: me,
                    epoch,
                    current_epoch: l.current.epoch,
                },
            );
            Err("another device took this run first".into())
        }
        None => Err("lease claim was not recorded".into()),
    }
}

fn remember(state: &AppState, run_id: Uuid, session_id: Uuid, workspace_id: Uuid, epoch: u64) {
    let mut held = state.guards.held_runs.lock().unwrap();
    let entry = held.entry(run_id).or_insert(HeldLease { session_id, workspace_id, epoch, beat: 0, last_beat: None });
    if entry.epoch != epoch {
        *entry = HeldLease { session_id, workspace_id, epoch, beat: 0, last_beat: None };
    }
}

/// The epoch this device drives `run_id` under, if it holds it.
pub(crate) fn epoch_for(state: &AppState, run_id: Uuid) -> Option<u64> {
    state.guards.held_runs.lock().unwrap().get(&run_id).map(|h| h.epoch)
}

/// Forget a held run lease (its driver ended). The durable lease stays; a
/// finished run needs no coordinator, and an unfinished one lapses so another
/// device can succeed.
pub(crate) fn forget_run(state: &AppState, run_id: Uuid) {
    state.guards.held_runs.lock().unwrap().remove(&run_id);
}

/// Whether this device still holds the lease it drives `run_id` under. False
/// once a newer epoch has been folded (another device took over: split brain
/// fenced) or after this device released it (handover). Records a
/// `leaseFenced` signal the first time it turns false.
pub(crate) fn still_holds(state: &AppState, session_id: Uuid, run_id: Uuid) -> bool {
    let Some(mine) = epoch_for(state, run_id) else { return true };
    let me = device_id(state);
    let lease = state
        .service
        .lock()
        .unwrap()
        .lease(session_id, LeaseScope::WorkflowRun, &run_id.to_string())
        .ok()
        .flatten();
    let holds = lease
        .as_ref()
        .is_some_and(|l| l.current.holder == me && l.current.epoch == mine && !l.released);
    if !holds {
        if let Some(l) = lease {
            let ws = state.guards.held_runs.lock().unwrap().get(&run_id).map(|h| h.workspace_id);
            if let Some(ws) = ws {
                if l.current.epoch != mine {
                    let _ = state.service.lock().unwrap().record_signal(
                        ws,
                        Some(session_id),
                        &GuardSignal::LeaseFenced {
                            scope: LeaseScope::WorkflowRun,
                            scope_id: run_id.to_string(),
                            holder: me,
                            epoch: mine,
                            current_epoch: l.current.epoch,
                        },
                    );
                }
            }
        }
        forget_run(state, run_id);
    }
    holds
}

/// Called by the driver before it starts: claim (or confirm) the lease and,
/// after a succession, wait for competing claims to sync and re-check.
pub(crate) async fn lease_before_driving(app: &AppHandle, session_id: Uuid, workspace_id: Uuid, run_id: Uuid) -> bool {
    let state = app.state::<AppState>();
    match acquire_run_lease(&state, session_id, workspace_id, run_id) {
        Ok((_, false)) => true,
        Ok((_, true)) => {
            tokio::time::sleep(SUCCESSION_SETTLE).await;
            still_holds(&state, session_id, run_id)
        }
        Err(e) => {
            tracing::info!(target: "leases", "not driving run {run_id}: {e}");
            false
        }
    }
}

fn relay_configured(state: &AppState) -> bool {
    state.settings.lock().unwrap().relay_url.as_deref().is_some_and(|u| !u.trim().is_empty())
}

/// Whether startup/periodic recovery may (try to) drive `run` here: it has no
/// lease (pre-lease run), this device (`me`) holds it, or `takeover_reason`
/// allows it. Takes `me`/`my_actor` so it can run under the service lock.
pub(crate) fn recovery_allowed(
    state: &AppState,
    session: &hive_core::ChatSession,
    run_id: Uuid,
    me: Uuid,
    my_actor: &str,
    beats_fetched: bool,
) -> bool {
    let scope_id = run_id.to_string();
    let Some(lease) = session.guards.leases.get(&hive_core::lease::lease_key(LeaseScope::WorkflowRun, &scope_id)) else {
        return true;
    };
    if lease.current.holder == me && !lease.released {
        return true;
    }
    let initiator_here = session
        .workflow_runs
        .iter()
        .find(|r| r.id == run_id)
        .is_some_and(|r| r.initiator_actor_id == my_actor);
    let last = state.guards.lease_beats.last(LeaseScope::WorkflowRun, &scope_id);
    takeover_reason(lease, me, last, now_ms(), beats_fetched, initiator_here).is_some()
}

/// Whether another device's lapse can be judged yet (computed before taking
/// the service lock).
pub(crate) fn beats_known(state: &AppState) -> bool {
    state.guards.beats_fetched.load(Ordering::Relaxed) || !relay_configured(state)
}

/// The lease card for a run, as this device sees it.
/// `me` is this device's id (passed in: callers hold the service lock).
pub(crate) fn run_lease_dto(state: &AppState, session: &hive_core::ChatSession, run_id: Uuid, me: Uuid) -> Option<RunLeaseDto> {
    let scope_id = run_id.to_string();
    let lease = session.guards.leases.get(&hive_core::lease::lease_key(LeaseScope::WorkflowRun, &scope_id))?;
    let now = now_ms();
    let beat = state.guards.lease_beats.get(LeaseScope::WorkflowRun, &scope_id);
    let mut last = beat.as_ref().map(|(b, _)| (b.holder, b.epoch, b.at_ms));
    let mut verified = beat.as_ref().is_some_and(|(b, t)| {
        *t == BeatTrust::Verified && b.holder == lease.current.holder && b.epoch == lease.current.epoch
    });
    if lease.current.holder == me {
        // Our own lease: we know we're alive while the driver runs.
        if let Some(h) = state.guards.held_runs.lock().unwrap().get(&run_id) {
            if h.epoch == lease.current.epoch {
                last = Some((me, h.epoch, now));
                verified = true;
            }
        }
    }
    let claimed = lease.current.claimed_at.inner().unix_timestamp() * 1000;
    let beat_at = match last {
        Some((h, e, at)) if h == lease.current.holder && e == lease.current.epoch => at.max(claimed),
        _ => claimed,
    };
    let health = match lease_health(lease, last, now) {
        LeaseHealth::Live => "live",
        LeaseHealth::Degraded => "degraded",
        LeaseHealth::Lapsed => "lapsed",
        LeaseHealth::Released => "released",
    };
    let holder_label = session
        .workspace_hosts
        .iter()
        .find(|h| h.id == lease.current.holder.to_string())
        .map(|h| h.label.clone())
        .unwrap_or_default();
    Some(RunLeaseDto {
        holder: lease.current.holder.to_string(),
        holder_label,
        is_me: lease.current.holder == me,
        epoch: lease.current.epoch.min(u32::MAX as u64) as u32,
        lease_ms: lease.current.lease_ms.min(u32::MAX as u64) as u32,
        health: health.into(),
        last_beat_age_ms: (now - beat_at).clamp(0, u32::MAX as i64) as u32,
        verified,
        handover_to: lease.handover_to.map(|u| u.to_string()),
        fenced_claims: lease.fenced.len() as u32,
    })
}

/// Hand a run this device drives to another online device of the workspace
/// (supersedure): release the lease reserving the next epoch for the
/// successor; the local driver stops at its next transition, and the
/// successor's lease loop picks the run up straight away.
#[tauri::command]
pub(crate) fn hand_over_workflow_run(
    state: State<AppState>,
    session_id: String,
    run_id: String,
    successor: Option<String>,
) -> Result<String, String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let rid = Uuid::parse_str(&run_id).map_err(map_err)?;
    let held = state.guards.held_runs.lock().unwrap().get(&rid).cloned().ok_or("this device isn't driving that run")?;
    let me = device_id(&state);
    let successor = match successor {
        Some(s) => Uuid::parse_str(&s).map_err(map_err)?,
        None => elect_successor(&state, held.workspace_id, sid, rid, me).ok_or("no other device of this workspace is online to take it")?,
    };
    if successor == me {
        return Err("pick another device".into());
    }
    let mut svc = state.service.lock().unwrap();
    svc.release_lease(sid, held.workspace_id, LeaseScope::WorkflowRun, &rid.to_string(), held.epoch, Some(successor))
        .map_err(map_err)?;
    drop(svc);
    // Wake the driver so it notices it no longer holds the lease.
    if let Some(w) = state.run_wakers.lock().unwrap().get(&rid) {
        w.notify_waiters();
    }
    Ok(successor.to_string())
}

/// Pick a successor: an online device host other than this one, preferring a
/// device of the person who started the run.
fn elect_successor(state: &AppState, workspace_id: Uuid, session_id: Uuid, run_id: Uuid, me: Uuid) -> Option<Uuid> {
    let svc = state.service.lock().unwrap();
    let hosts = svc.list_hosts(workspace_id).ok()?;
    let initiator = svc
        .load(session_id)
        .ok()
        .flatten()
        .and_then(|s| s.workflow_runs.iter().find(|r| r.id == run_id).map(|r| r.initiator_actor_id.clone()))
        .unwrap_or_default();
    drop(svc);
    let now = hive_core::Timestamp::now();
    let mut candidates: Vec<_> = hosts
        .iter()
        .filter(|h| h.kind == hive_core::HostKind::Device && state.host_presence.is_online(h, &now))
        .filter_map(|h| Uuid::parse_str(&h.id).ok().filter(|id| *id != me).map(|id| (h.owner_actor_id != initiator, id)))
        .collect();
    candidates.sort();
    candidates.first().map(|(_, id)| *id)
}

/// Background loop: publish heartbeats for the leases this device holds, read
/// everyone else's, and run lease-aware recovery for interrupted runs (so a
/// lapsed holder is succeeded while this app is open, not only at launch).
pub(crate) async fn run_lease_loop(app: AppHandle) {
    let beat_every = Duration::from_millis(DEFAULT_BEAT_EVERY_MS);
    let mut tick = tokio::time::interval(LEASE_TICK);
    loop {
        tick.tick().await;
        let state = app.state::<AppState>();
        let conn = {
            let s = state.settings.lock().unwrap();
            s.relay_url
                .clone()
                .filter(|u| !u.trim().is_empty())
                .map(|u| (u, s.sync_room.clone(), s.workspace_key()))
        };
        let me = device_id(&state);
        // Our own beats: always recorded locally (signals), published when a
        // relay is configured.
        let due: Vec<(Uuid, HeldLease)> = {
            let mut held = state.guards.held_runs.lock().unwrap();
            held.iter_mut()
                .filter(|(_, h)| h.last_beat.is_none_or(|t| t.elapsed() >= beat_every))
                .map(|(id, h)| {
                    h.beat += 1;
                    h.last_beat = Some(std::time::Instant::now());
                    (*id, h.clone())
                })
                .collect()
        };
        for (run_id, h) in &due {
            let beat = LeaseBeat {
                scope: LeaseScope::WorkflowRun,
                scope_id: run_id.to_string(),
                holder: me,
                epoch: h.epoch,
                beat: h.beat,
                lease_ms: DEFAULT_LEASE_MS,
                at_ms: now_ms(),
                signature: Vec::new(),
            };
            let signed = state.service.lock().unwrap().sign_lease_beat(beat);
            state.guards.lease_beats.record(signed.clone(), BeatTrust::Verified);
            let _ = state.service.lock().unwrap().record_signal(
                h.workspace_id,
                Some(h.session_id),
                &GuardSignal::CoordinatorHeartbeat {
                    scope: LeaseScope::WorkflowRun,
                    scope_id: run_id.to_string(),
                    holder: me,
                    lease_ms: DEFAULT_LEASE_MS,
                    epoch: h.epoch,
                    beat: h.beat,
                },
            );
            if let Some((url, room, Some(key))) = &conn {
                if hive_core::room_workspace_id(room) == h.workspace_id {
                    let relay = state.relay_client(url);
                    if let Err(e) = hive_runtime::lease_presence::publish_lease_beat(&relay, room, key, &signed).await {
                        tracing::debug!(target: "leases", "lease beat publish failed: {e}");
                    }
                }
            }
        }
        // Everyone else's beats.
        if let Some((url, room, Some(key))) = &conn {
            let relay = state.relay_client(url);
            if let Ok(blobs) = relay.list_presence(room).await {
                let ws = hive_core::room_workspace_id(room);
                let resolver = {
                    let svc = state.service.lock().unwrap();
                    let trust = svc.store().trust_envelopes(ws).unwrap_or_default();
                    hive_runtime::envelope_verifier::build_roster(&trust)
                };
                use hive_runtime::envelope_verifier::DeviceKeyResolver;
                let beats = hive_runtime::lease_presence::open_lease_beats(&blobs, key, now_ms(), |d| {
                    if resolver.is_revoked(d) {
                        // A revoked device's beat can't renew anything: treat as
                        // certified with an impossible key so it fails.
                        return Some(vec![0u8; 32]);
                    }
                    resolver.public_key(d).map(<[u8]>::to_vec)
                });
                for (beat, trust) in beats {
                    if beat.holder == me {
                        continue;
                    }
                    let (scope, scope_id, holder, epoch, n) =
                        (beat.scope, beat.scope_id.clone(), beat.holder, beat.epoch, beat.beat);
                    if state.guards.lease_beats.record(beat, trust) {
                        let _ = state.service.lock().unwrap().record_signal(
                            ws,
                            None,
                            &GuardSignal::CoordinatorReceived { scope, scope_id, beat: n, holder, epoch, member: me },
                        );
                    }
                }
                state.guards.beats_fetched.store(true, Ordering::Relaxed);
            }
        }
        // Wake drivers whose lease may have been fenced by a synced claim.
        let held_ids: Vec<Uuid> = state.guards.held_runs.lock().unwrap().keys().copied().collect();
        for rid in held_ids {
            if let Some(w) = state.run_wakers.lock().unwrap().get(&rid) {
                w.notify_waiters();
            }
        }
        crate::recover_workflow_runs(&app);
    }
}

/// Device-local guard signals for the active workspace (content-free: lease
/// heartbeats and receipts, boundary decisions, hygiene catches, throttles),
/// oldest first after `after_row`. Never synced.
#[tauri::command]
pub(crate) fn list_guard_signals(
    state: State<AppState>,
    after_row: Option<i64>,
    kind: Option<String>,
    limit: Option<u32>,
) -> Result<Vec<hive_proto::GuardSignalDto>, String> {
    let ws = state.active_workspace_id();
    let svc = state.service.lock().unwrap();
    let rows = svc
        .list_signals(ws, after_row.unwrap_or(0), kind.as_deref(), limit.unwrap_or(500).min(5_000) as usize)
        .map_err(map_err)?;
    Ok(rows
        .into_iter()
        .map(|r| hive_proto::GuardSignalDto {
            row_id: r.row_id,
            at_ms: r.at_ms,
            kind: r.signal.kind_str().to_string(),
            session_id: r.session_id.map(|s| s.to_string()),
            attrs_json: serde_json::to_string(&r.signal).unwrap_or_default(),
        })
        .collect())
}

/// Whether a run is in a state a coordinator drives.
pub(crate) fn is_live_status(status: WorkflowRunStatus) -> bool {
    matches!(status, WorkflowRunStatus::Running | WorkflowRunStatus::AwaitingGate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::lease::{fold_claim, CoordinatorLease, LeaseTable};
    use hive_core::Timestamp;

    fn lease(holder: Uuid, claimed_s: i64) -> LeaseState {
        let mut t = LeaseTable::new();
        fold_claim(
            &mut t,
            &CoordinatorLease {
                scope: LeaseScope::WorkflowRun,
                scope_id: "r".into(),
                holder,
                epoch: 1,
                lease_ms: 60_000,
                reason: LeaseClaimReason::Initial,
                claimed_at: Timestamp::from_unix_seconds(claimed_s),
            },
        );
        t.into_values().next().unwrap()
    }

    #[test]
    fn succession_only_after_a_lapse_and_staggered() {
        let (holder, me) = (Uuid::new_v4(), Uuid::new_v4());
        let l = lease(holder, 1_000);
        let t0 = 1_000_000;
        // Live: never.
        assert_eq!(takeover_reason(&l, me, Some((holder, 1, t0 + 50_000)), t0 + 60_000, true, true), None);
        // Lapsed, but we haven't read the relay's beats yet: wait.
        assert_eq!(takeover_reason(&l, me, None, t0 + 70_000, false, true), None);
        // Lapsed once: the initiator's device succeeds, others wait another lease.
        assert_eq!(takeover_reason(&l, me, None, t0 + 70_000, true, true), Some(LeaseClaimReason::Lapsed));
        assert_eq!(takeover_reason(&l, me, None, t0 + 70_000, true, false), None);
        assert_eq!(takeover_reason(&l, me, None, t0 + 130_000, true, false), Some(LeaseClaimReason::Lapsed));
        // A beat from a stale epoch or another holder doesn't keep it alive.
        assert_eq!(takeover_reason(&l, me, Some((holder, 0, t0 + 69_000)), t0 + 70_000, true, true), Some(LeaseClaimReason::Lapsed));
    }

    #[test]
    fn handover_goes_to_the_named_successor_first() {
        let (holder, me, other) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut l = lease(holder, 1_000);
        l.released = true;
        l.handover_to = Some(me);
        assert_eq!(takeover_reason(&l, me, None, 1_000_001, false, false), Some(LeaseClaimReason::Handover));
        // Someone else waits until two leases pass with no claim by the successor.
        assert_eq!(takeover_reason(&l, other, None, 1_000_001, true, true), None);
        assert_eq!(takeover_reason(&l, other, None, 1_000_000 + 121_000, true, true), Some(LeaseClaimReason::Lapsed));
        // A plain release (no successor) is open to anyone at once.
        l.handover_to = None;
        assert_eq!(takeover_reason(&l, other, None, 1_000_001, false, false), Some(LeaseClaimReason::Released));
    }
}
