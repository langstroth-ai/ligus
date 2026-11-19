//! Desktop side of the workspace guards: coordinator leases for workflow runs
//! and the sandbox envelope every agent turn runs in.
//!
//! ## Coordinator leases
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
    /// The last hygiene sweep's results (in memory; a restart sweeps again).
    pub hygiene: Mutex<hive_proto::HygieneReportDto>,
    /// Last plan-window observation per runtime kind.
    pub plan_windows: Mutex<HashMap<hive_core::ModelProviderKind, hive_core::throttle::PlanWindow>>,
    /// Work the throttles hold back right now, by key (for the UI).
    pub deferred: Mutex<HashMap<String, hive_proto::DeferredWorkDto>>,
    /// When the lease loop next runs the hygiene sweep.
    pub last_sweep: Mutex<Option<std::time::Instant>>,
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
            s.relay_url.clone().filter(|u| !u.trim().is_empty()).map(|u| {
                let ws_conn = s.workspaces.iter().find(|w| w.room == s.sync_room).cloned();
                (u, s.sync_room.clone(), s.workspace_key(), ws_conn)
            })
        };
        // The one keyring (B1): lease beats seal at the authorized head, and
        // only beats sealed there are read.
        let conn = conn.map(|(u, room, key, ws_conn)| {
            let ring = key.is_some().then(|| crate::workspace_ring(&state, &room, ws_conn.as_ref()));
            (u, room, ring)
        });
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
            if let Some((url, room, Some(ring))) = &conn {
                if hive_core::room_workspace_id(room) == h.workspace_id {
                    let relay = state.relay_client(url);
                    if let Err(e) = hive_runtime::lease_presence::publish_lease_beat(&relay, room, ring, &signed).await {
                        tracing::debug!(target: "leases", "lease beat publish failed: {e}");
                    }
                }
            }
        }
        // Everyone else's beats.
        if let Some((url, room, Some(ring))) = &conn {
            let relay = state.relay_client(url);
            if let Ok(blobs) = relay.list_presence(room).await {
                let ws = hive_core::room_workspace_id(room);
                let resolver = {
                    let svc = state.service.lock().unwrap();
                    let trust = svc.store().trust_envelopes(ws).unwrap_or_default();
                    hive_runtime::envelope_verifier::build_roster(&trust)
                };
                use hive_runtime::envelope_verifier::DeviceKeyResolver;
                let beats = hive_runtime::lease_presence::open_lease_beats(&blobs, ring, now_ms(), |d| {
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
        // Hygiene: first a few minutes after launch, then every few hours.
        let due = {
            let now = std::time::Instant::now();
            let mut next = state.guards.last_sweep.lock().unwrap();
            let at = *next.get_or_insert(now + HYGIENE_FIRST_AFTER);
            if now >= at {
                *next = Some(now + HYGIENE_EVERY);
                true
            } else {
                false
            }
        };
        if due {
            let app2 = app.clone();
            let _ = tauri::async_runtime::spawn_blocking(move || {
                let state = app2.state::<AppState>();
                run_hygiene_sweep(&state);
            })
            .await;
        }
    }
}

const HYGIENE_EVERY: Duration = Duration::from_secs(6 * 3600);
const HYGIENE_FIRST_AFTER: Duration = Duration::from_secs(180);

// ---------------------------------------------------------------------------
// Hygienic removal
// ---------------------------------------------------------------------------

/// Sweep the active workspace's bound repository: remove crash-leftover turn
/// worktrees (saving their edits), remove checkouts of gone or archived chats
/// (keeping their branches), and mark open proposals whose diff no longer
/// applies as stale (taking them out of the inbox).
pub(crate) fn run_hygiene_sweep(state: &AppState) -> hive_proto::HygieneReportDto {
    use hive_core::signal::{HygieneKind, WorktreeState};
    use hive_runtime::hygiene::{self, ChatStatus, SweepAction};
    let ws = state.active_workspace_id();
    let repo = state.workspace_repo_root();
    let mut report = hive_proto::HygieneReportDto { ran_at: crate::rfc3339(hive_core::Timestamp::now()), ..Default::default() };
    // Every checkout this device manages for the workspace (5.7): the bound
    // folder, and each confirmed checkout of a workspace repo, with the repo
    // it holds (a proposal is judged only against its own repo's checkout).
    let mut roots: Vec<(std::path::PathBuf, Option<Uuid>)> = Vec::new();
    {
        let svc = state.service.lock().unwrap();
        for m in svc.store().repo_mappings(ws).unwrap_or_default().into_iter().filter(|m| m.confirmed) {
            if hive_runtime::git_worktree::is_git_repo(&m.path) && !roots.iter().any(|(p, _)| *p == m.path) {
                roots.push((m.path, Some(m.repo_id)));
            }
        }
    }
    let bound = std::path::PathBuf::from(&repo);
    let bound_canon = hive_runtime::repos::git::canonical(&bound);
    if !repo.trim().is_empty() && hive_runtime::git_worktree::is_git_repo(&bound) && !roots.iter().any(|(p, _)| *p == bound_canon) {
        roots.push((bound, None));
    }
    if roots.is_empty() {
        *state.guards.hygiene.lock().unwrap() = report.clone();
        return report;
    }
    report.repo_bound = true;
    // The workspace's chats, their streaming turns and open diff proposals.
    let mut chats = std::collections::HashMap::new();
    let mut live_turns = std::collections::HashSet::new();
    let mut open_diffs: Vec<(Uuid, Uuid, String, String, Option<Uuid>)> = Vec::new();
    let device = {
        let svc = state.service.lock().unwrap();
        let config_id = hive_core::workspace_config_session_id(ws);
        for id in svc.store().list_session_ids().unwrap_or_default() {
            if id == config_id {
                continue;
            }
            let Ok(Some(s)) = svc.load(id) else { continue };
            if s.workspace_id != ws {
                continue;
            }
            chats.insert(hygiene::slug(&id.to_string()), if s.archived { ChatStatus::Archived } else { ChatStatus::Live });
            for m in s.messages.iter().filter(|m| m.is_streaming) {
                live_turns.insert(hygiene::slug(&m.id.to_string()));
            }
            if s.archived {
                continue;
            }
            for p in &s.proposals {
                if p.status == hive_core::ProposalStatus::Open
                    && !p.dismissed
                    && p.superseded_by.is_none()
                    && !s.guards.stale_proposals.contains(&p.id)
                {
                    if let Some(d) = &p.diff {
                        open_diffs.push((id, p.id, p.title.clone(), d.clone(), p.repo_id));
                    }
                }
            }
        }
        svc.device_id()
    };
    let secret = device.as_bytes().to_vec();
    let mut signals = Vec::new();
    let legacy_root = roots.iter().find(|(_, r)| r.is_none()).map(|(p, _)| p.clone()).or_else(|| roots.first().map(|(p, _)| p.clone()));
    for (root, root_repo) in &roots {
        let root = root.clone();
        let entries = hygiene::list_worktrees(&root);
        // Only worktrees under this workspace's `.ligus/worktrees` are ours; a
        // Hive 1.x `.hive/worktrees` checkout is never swept.
        let plan = match hive_core::compat::workspace_state_path(&root, "worktrees") {
            Ok(owned) => hygiene::plan_sweep(&owned, &entries, &live_turns, &chats, &hive_runtime::git_worktree::is_in_use),
            Err(e) => {
                tracing::warn!(target: "worktree", "hygiene sweep skipped: {e}");
                Vec::new()
            }
        };
        for action in plan {
            if !hygiene::apply(&root, &action) {
                continue;
            }
            match &action {
                SweepAction::RemoveTurnWorktree { .. } => {
                    report.turn_worktrees_removed += 1;
                    signals.push(GuardSignal::WorktreeState { state: WorktreeState::Abandoned, repo_key: None });
                }
                SweepAction::RemoveChatCheckout { chat_slug, .. } => {
                    report.chat_checkouts_removed += 1;
                    signals.push(GuardSignal::WorktreeState {
                        state: WorktreeState::Removed,
                        repo_key: Some(hygiene::opaque_key(&secret, chat_slug)),
                    });
                }
                // A gone or archived chat's reuse pool: a hygienic removal, never
                // an abandoned turn (parking and reuse record nothing).
                SweepAction::RemoveParkedWorktree { chat_slug, .. } => {
                    report.parked_worktrees_removed += 1;
                    signals.push(GuardSignal::WorktreeState {
                        state: WorktreeState::Removed,
                        repo_key: Some(hygiene::opaque_key(&secret, chat_slug)),
                    });
                }
            }
        }
        hygiene::prune(&root);
        // Stale proposals: check each open diff against its chat's branch checkout
        // (or the repository's HEAD when the chat has none).
        let remaining = hygiene::list_worktrees(&root);
        for (sid, pid, title, diff, p_repo) in open_diffs.iter().cloned() {
            // A repo's proposals against that repo's checkout; one without a
            // repo against the bound folder.
            let mine = match p_repo {
                Some(r) => *root_repo == Some(r),
                None => legacy_root.as_ref() == Some(&root),
            };
            if !mine {
                continue;
            }
            let branch = format!("hive/chat/{}", hygiene::slug(&sid.to_string()));
            let dir = remaining
                .iter()
                .find(|e| e.branch.as_deref() == Some(branch.as_str()))
                .map(|e| e.path.clone())
                .unwrap_or_else(|| root.clone());
            if hygiene::patch_applies(&dir, &diff) {
                continue;
            }
            let mut svc = state.service.lock().unwrap();
            if svc.mark_proposal_stale(sid, ws, pid).is_ok() {
                signals.push(GuardSignal::Hygiene { hygiene_kind: HygieneKind::ProposalStale, count: 1 });
                report.stale_proposals.push(hive_proto::StaleProposalDto {
                    session_id: sid.to_string(),
                    proposal_id: pid.to_string(),
                    title,
                });
            }
        }
    }
    {
        let svc = state.service.lock().unwrap();
        for sig in &signals {
            let _ = svc.record_signal(ws, None, sig);
        }
    }
    *state.guards.hygiene.lock().unwrap() = report.clone();
    report
}

/// Run the hygiene sweep now (Settings → Guards).
#[tauri::command]
pub(crate) async fn run_hygiene_now(app: AppHandle) -> Result<hive_proto::HygieneReportDto, String> {
    let report = tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        run_hygiene_sweep(&state)
    })
    .await
    .map_err(map_err)?;
    Ok(report)
}

/// The last hygiene sweep's results.
#[tauri::command]
pub(crate) fn get_hygiene_report(state: State<AppState>) -> hive_proto::HygieneReportDto {
    state.guards.hygiene.lock().unwrap().clone()
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

// ---------------------------------------------------------------------------
// Sandbox envelope
// ---------------------------------------------------------------------------

/// The envelope a turn in `session` runs in. The session carries the
/// workspace's guard state (overlaid from the config log by `ChatService::load`).
pub(crate) fn turn_envelope(session: &hive_core::ChatSession) -> hive_core::sandbox::SandboxEnvelope {
    hive_core::sandbox::envelope_for_session(session)
}

/// The configured MCP servers the envelope's egress policy lets a turn reach.
/// Each refused (enabled) server is recorded as a content-free egress decision.
pub(crate) fn filter_mcp_servers(
    state: &AppState,
    servers: Vec<hive_runtime::McpServerSpec>,
    egress: &hive_core::sandbox::EgressPolicy,
) -> Vec<hive_runtime::McpServerSpec> {
    if egress.is_open() {
        return servers;
    }
    let ws = state.active_workspace_id();
    servers
        .into_iter()
        .filter(|s| {
            let ok = egress.allows_mcp(&s.id);
            if !ok && s.enabled {
                if let Ok(svc) = state.service.try_lock() {
                    let _ = svc.record_signal(ws, None, &egress_decision(&s.id, "envelope_egress"));
                }
            }
            ok
        })
        .collect()
}

fn egress_decision(source: &str, reason: &str) -> GuardSignal {
    GuardSignal::GuardDecision {
        gate: hive_core::signal::GuardGate::Egress,
        verdict: hive_core::signal::GuardVerdict::Reject,
        subject: hive_core::signal::GuardSubject::Nestmate,
        source: source.to_string(),
        reason: reason.to_string(),
    }
}

/// Claude Code `mcpServers` entries for the servers the envelope allows.
/// Authenticated HTTP servers are left out: their token would have to ride
/// the command line, where any local process can read it.
fn claude_mcp_entries(state: &AppState, egress: &hive_core::sandbox::EgressPolicy, shell: bool) -> Vec<(String, serde_json::Value)> {
    // Every turn runs Claude Code with a strict MCP config (open egress
    // too), so the chat's servers ride it; none while egress is closed.
    if matches!(egress, hive_core::sandbox::EgressPolicy::Closed) {
        return Vec::new();
    }
    filter_mcp_servers(state, state.combined_mcp_servers(), egress)
        .into_iter()
        .filter(|s| s.enabled && s.auth.is_none())
        .filter(|s| shell || mcp_known_shell_free(state, &s.id))
        .map(|s| {
            let v = match &s.transport {
                hive_runtime::McpTransport::Stdio { command, args } => serde_json::json!({ "command": command, "args": args }),
                hive_runtime::McpTransport::Http { url } => serde_json::json!({ "type": "http", "url": url }),
            };
            (s.id.clone(), v)
        })
        .collect()
}

/// How the turn's runtime honors the envelope (extra flags, or a refusal).
/// `shell`: whether the turn has the shell; without it, MCP servers that
/// offer one (or that this device hasn't seen list their tools) stay out.
pub(crate) fn enforcement_for(
    state: &AppState,
    provider: hive_core::ModelProviderKind,
    envelope: &hive_core::sandbox::SandboxEnvelope,
    shell: bool,
) -> hive_runtime::sandbox::Enforcement {
    let entries = if provider == hive_core::ModelProviderKind::ClaudeCode {
        claude_mcp_entries(state, &envelope.egress, shell)
    } else {
        Vec::new()
    };
    hive_runtime::sandbox::enforce(provider, envelope, &entries)
}

// ---------------------------------------------------------------------------
// MCP servers that offer a shell
// ---------------------------------------------------------------------------

/// Remember which tools each server listed run shell commands
/// (`hive_runtime::is_shell_tool`).
pub(crate) fn record_mcp_tools(state: &AppState, tagged: &[(String, hive_runtime::McpTool)]) {
    let mut by_server: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for (server, tool) in tagged {
        let e = by_server.entry(server.clone()).or_default();
        if hive_runtime::is_shell_tool(&tool.name) {
            e.push(tool.name.clone());
        }
    }
    let mut m = state.mcp_shell_tools.lock().unwrap();
    for (server, shells) in by_server {
        m.insert(server, shells);
    }
}

/// Whether this device has seen `id` list its tools, none of them a shell.
/// A turn without the shell gets only such servers (fail closed: one that
/// never listed is left out).
pub(crate) fn mcp_known_shell_free(state: &AppState, id: &str) -> bool {
    id == hive_runtime::executor::LIGUS_MCP_ID || state.mcp_shell_tools.lock().unwrap().get(id).is_some_and(|t| t.is_empty())
}

/// List the tools of the enabled servers this turn may reach that this
/// device hasn't listed yet (each bounded), so `mcp_known_shell_free` can
/// judge them.
pub(crate) async fn probe_mcp_shells(state: &AppState, egress: &hive_core::sandbox::EgressPolicy) {
    state.ensure_fresh_mcp_tokens().await;
    const RETRY: std::time::Duration = std::time::Duration::from_secs(600);
    let unknown: Vec<hive_runtime::McpServerSpec> = filter_mcp_servers(state, state.combined_mcp_servers(), egress)
        .into_iter()
        .filter(|s| s.enabled && !state.mcp_shell_tools.lock().unwrap().contains_key(&s.id))
        .filter(|s| state.mcp_probe_failed.lock().unwrap().get(&s.id).is_none_or(|t| t.elapsed() > RETRY))
        .collect();
    for s in unknown {
        let id = s.id.clone();
        let registry = hive_runtime::McpRegistry::new(vec![s]);
        match tokio::time::timeout(std::time::Duration::from_secs(15), registry.list_tools(&id)).await {
            Ok(Ok(tools)) => {
                let tagged: Vec<(String, hive_runtime::McpTool)> = tools.into_iter().map(|t| (id.clone(), t)).collect();
                if tagged.is_empty() {
                    state.mcp_shell_tools.lock().unwrap().insert(id.clone(), Vec::new());
                }
                record_mcp_tools(state, &tagged);
                state.mcp_probe_failed.lock().unwrap().remove(&id);
            }
            _ => {
                state.mcp_probe_failed.lock().unwrap().insert(id, std::time::Instant::now());
            }
        }
    }
}

/// The OS-sandbox policy for one turn on this desktop: the envelope, the
/// author gate's plan, and the endpoints of the MCP servers the envelope
/// lets this chat reach (their hosts join the egress allow-list).
pub(crate) fn turn_sandbox_policy(
    state: &AppState,
    session: &hive_core::ChatSession,
    envelope: &hive_core::sandbox::SandboxEnvelope,
    gate: &hive_runtime::author_gate::GatePlan,
    isolated: bool,
) -> hive_runtime::executor::TurnPolicy {
    let mut policy = hive_runtime::executor::TurnPolicy::new(hive_runtime::executor::HostKind::Desktop, envelope.clone(), gate.clone());
    policy.worktree_is_isolated = isolated;
    // Opt-in egress presets (device-local). The plan applies them only under
    // an open envelope, so dearth mode and a lockdown override them.
    policy.egress_presets = chat_egress_presets(state, session.workspace_id, session.id);
    policy.cli_user_config = workspace_cli_user_config(state, session.workspace_id);
    for s in state.combined_mcp_servers().into_iter().filter(|s| s.enabled && envelope.egress.allows_mcp(&s.id)) {
        if let hive_runtime::McpTransport::Http { url } = &s.transport {
            policy.mcp_hosts.extend(hive_runtime::egress::HostPort::from_url(url));
        }
        policy.mcp_ids.push(s.id.clone());
    }
    policy
}

// ---------------------------------------------------------------------------
// Egress presets
// ---------------------------------------------------------------------------

/// The egress presets a chat opted into on this device: the chat's own
/// setting when it has one, else its workspace's. Off by default.
pub(crate) fn chat_egress_presets(state: &AppState, workspace_id: Uuid, session_id: Uuid) -> Vec<hive_runtime::egress::EgressPreset> {
    let s = state.settings.lock().unwrap();
    effective_egress_presets(&s.egress_presets_by_workspace, &s.egress_presets_by_chat, workspace_id, session_id)
}

pub(crate) fn effective_egress_presets(
    by_workspace: &std::collections::HashMap<String, Vec<String>>,
    by_chat: &std::collections::HashMap<String, Vec<String>>,
    workspace_id: Uuid,
    session_id: Uuid,
) -> Vec<hive_runtime::egress::EgressPreset> {
    let names = by_chat.get(&session_id.to_string()).or_else(|| by_workspace.get(&workspace_id.to_string()));
    names.map(|n| hive_runtime::egress::EgressPreset::parse_list(n.iter().map(String::as_str))).unwrap_or_default()
}

fn preset_names(presets: &[hive_runtime::egress::EgressPreset]) -> Vec<String> {
    presets.iter().map(|p| p.name().to_string()).collect()
}

/// Settings → Guards → Network presets: what exists, what the active
/// workspace and (optionally) one chat have on, and whether a guard is
/// overriding them right now.
#[tauri::command]
pub(crate) fn get_egress_presets(state: State<AppState>, session_id: Option<String>) -> Result<hive_proto::EgressPresetsDto, String> {
    let ws = state.active_workspace_id();
    let sid = session_id.as_deref().filter(|s| !s.is_empty()).map(Uuid::parse_str).transpose().map_err(map_err)?;
    let session = match sid {
        Some(id) => state.service.lock().unwrap().load(id).map_err(map_err)?,
        None => None,
    };
    let (workspace, chat) = {
        let s = state.settings.lock().unwrap();
        let known = |n: &Vec<String>| preset_names(&hive_runtime::egress::EgressPreset::parse_list(n.iter().map(String::as_str)));
        let workspace = s.egress_presets_by_workspace.get(&ws.to_string()).map(known).unwrap_or_default();
        let chat = sid.and_then(|id| s.egress_presets_by_chat.get(&id.to_string())).map(known);
        (workspace, chat)
    };
    let host = preset_names(&hive_runtime::executor::host_paths().egress_presets);
    let restricted_by = session
        .as_ref()
        .map(|s| {
            let env = hive_core::sandbox::envelope_for_session(s);
            if env.egress.is_open() {
                String::new()
            } else if env.tightened_by.contains(&hive_core::sandbox::TightenedBy::Lockdown) {
                "lockdown".to_string()
            } else {
                "dearth".to_string()
            }
        })
        .unwrap_or_default();
    Ok(hive_proto::EgressPresetsDto {
        available: hive_runtime::egress::EgressPreset::ALL
            .iter()
            .map(|p| hive_proto::EgressPresetDto {
                name: p.name().into(),
                label: p.label().into(),
                hosts: p.host_names().iter().map(|h| h.to_string()).collect(),
            })
            .collect(),
        workspace,
        chat,
        host,
        restricted_by,
    })
}

/// Whether `workspace_id`'s CLI agents run with the user's own CLI setup.
pub(crate) fn workspace_cli_user_config(state: &AppState, workspace_id: Uuid) -> bool {
    state.settings.lock().unwrap().cli_user_config_workspaces.contains(&workspace_id.to_string())
}

/// Whether the active workspace's CLI agents use the user's own CLI setup
/// (Claude Code: user settings, hooks, plugins, skills). Device-local.
#[tauri::command]
pub(crate) fn get_workspace_cli_user_config(state: State<AppState>) -> bool {
    workspace_cli_user_config(&state, state.active_workspace_id())
}

/// Opt the active workspace's CLI agents into (or out of) the user's own
/// CLI setup on this device. The tools that run work outside the turn stay
/// off either way, and the opt-in is ignored while egress is restricted.
#[tauri::command]
pub(crate) fn set_workspace_cli_user_config(state: State<AppState>, enabled: bool) -> Result<(), String> {
    let ws = state.active_workspace_id().to_string();
    let mut s = state.settings.lock().unwrap();
    if enabled {
        s.cli_user_config_workspaces.insert(ws);
    } else {
        s.cli_user_config_workspaces.remove(&ws);
    }
    crate::save_settings(&state.data_dir, &s);
    Ok(())
}

/// Turn presets on or off for the active workspace (device-local).
#[tauri::command]
pub(crate) fn set_workspace_egress_presets(state: State<AppState>, presets: Vec<String>) -> Result<(), String> {
    let parsed = parse_preset_names(&presets)?;
    let ws = state.active_workspace_id().to_string();
    let mut s = state.settings.lock().unwrap();
    if parsed.is_empty() {
        s.egress_presets_by_workspace.remove(&ws);
    } else {
        s.egress_presets_by_workspace.insert(ws, preset_names(&parsed));
    }
    crate::save_settings(&state.data_dir, &s);
    Ok(())
}

/// Override the workspace's presets for one chat (`None` = follow the
/// workspace again; an empty list = off for this chat).
#[tauri::command]
pub(crate) fn set_chat_egress_presets(state: State<AppState>, session_id: String, presets: Option<Vec<String>>) -> Result<(), String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?.to_string();
    let parsed = presets.as_deref().map(parse_preset_names).transpose()?;
    let mut s = state.settings.lock().unwrap();
    match parsed {
        None => {
            s.egress_presets_by_chat.remove(&sid);
        }
        Some(p) => {
            s.egress_presets_by_chat.insert(sid, preset_names(&p));
        }
    }
    crate::save_settings(&state.data_dir, &s);
    Ok(())
}

/// Known preset names, or an error naming the unknown one.
fn parse_preset_names(names: &[String]) -> Result<Vec<hive_runtime::egress::EgressPreset>, String> {
    for n in names {
        if hive_runtime::egress::EgressPreset::parse(n).is_none() {
            return Err(format!("unknown egress preset '{n}'"));
        }
    }
    Ok(hive_runtime::egress::EgressPreset::parse_list(names.iter().map(String::as_str)))
}

/// Endpoints the turn's egress proxy refused: one content-free decision per
/// endpoint count (no host names: a name can itself carry data) and a note
/// in the chat so members see why a tool call failed.
pub(crate) fn record_sandbox_egress_denials(state: &AppState, workspace_id: Uuid, session_id: Uuid, denied: &[hive_runtime::egress::HostPort]) {
    if denied.is_empty() {
        return;
    }
    let hosts: Vec<String> = denied.iter().map(ToString::to_string).collect();
    tracing::info!(target: "sandbox", refused = %hosts.join(", "), "egress proxy refused endpoints");
    let mut svc = state.service.lock().unwrap();
    let _ = svc.record_signal(workspace_id, Some(session_id), &egress_decision("sandbox_proxy", "endpoint_not_allowed"));
    // Known optional hosts (a CLI's telemetry, the claude.ai connectors
    // proxy) are blocked on purpose and don't affect the turn: no notice.
    if let Some(note) = hive_runtime::egress::refused_notice(denied) {
        let _ = svc.post_system_note(session_id, workspace_id, note);
    }
}

/// A turn refused because its runtime can't be confined: tell the chat (so
/// every member sees why nobody answered) and record the decision.
pub(crate) fn record_egress_refusal(state: &AppState, workspace_id: Uuid, session_id: Uuid, provider: hive_core::ModelProviderKind) {
    let mut svc = state.service.lock().unwrap();
    let _ = svc.record_signal(workspace_id, Some(session_id), &egress_decision(&format!("{provider:?}"), "runtime_unconfinable"));
    let _ = svc.post_system_note(
        session_id,
        workspace_id,
        format!("A {provider:?} turn was not run: this chat's sandbox has network egress restricted and {provider:?} can't be confined to it. Claude Code, Codex and API models can."),
    );
}

// ---------------------------------------------------------------------------
// Sandbox: the effective policy and the signed grants (Phase C)
// ---------------------------------------------------------------------------

/// One agent of the workspace, for the Sandbox section.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SandboxAgentDto {
    pub id: String,
    pub name: String,
    pub runtime: String,
}

/// Settings, Sandbox: what agent turns may reach on this device and why,
/// the signed grants in force, and who may change them.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SandboxPolicyDto {
    /// The effective policy for the chosen agent (or, with none, for every
    /// runtime the workspace's agents use, plus commands).
    pub policies: Vec<hive_runtime::executor::EffectivePolicy>,
    pub grants: hive_core::sandbox::SandboxGrants,
    pub agents: Vec<SandboxAgentDto>,
    /// Admin or above: may change the path grants (with a passkey).
    pub can_edit_paths: bool,
    /// The Owner: may grant or withdraw an agent's shell (with a passkey).
    pub can_edit_shell: bool,
    /// The workspace is the device-local one (its only person is the device
    /// owner, who always has the shell).
    pub local: bool,
    /// Whether this device applies the grants (`policies` show what applies
    /// here), its default, its own choice, and why.
    pub applies_here: bool,
    pub applies_by_default: bool,
    pub applies_setting: Option<bool>,
    pub applies_why: String,
    /// The signed grants changed since this device's user last looked.
    pub changed: bool,
}

/// This device's choice for `workspace_id`'s signed grants (`None`: the
/// default, `hive_runtime::sandbox::grants_here`).
pub(crate) fn grants_setting(state: &AppState, workspace_id: Uuid) -> Option<bool> {
    state.settings.lock().unwrap().sandbox_grants_here.get(&workspace_id.to_string()).copied()
}

/// Apply (or not, or back to the default with `None`) the active
/// workspace's signed sandbox grants on this device.
#[tauri::command]
pub(crate) fn set_sandbox_grants_here(state: State<AppState>, apply: Option<bool>) -> Result<(), String> {
    let ws = state.active_workspace_id().to_string();
    let mut s = state.settings.lock().unwrap();
    match apply {
        Some(v) => s.sandbox_grants_here.insert(ws, v),
        None => s.sandbox_grants_here.remove(&ws),
    };
    crate::save_settings(&state.data_dir, &s);
    Ok(())
}

/// The user has seen the active workspace's signed grants as they are now
/// (the change notice goes away until they change again).
#[tauri::command]
pub(crate) fn ack_sandbox_grants(state: State<AppState>) -> Result<(), String> {
    let ws = state.active_workspace_id();
    let fp = hive_runtime::sandbox::grants_fingerprint(&state.service.lock().unwrap().sandbox_grants(ws));
    let mut s = state.settings.lock().unwrap();
    s.sandbox_grants_seen.insert(ws.to_string(), fp);
    crate::save_settings(&state.data_dir, &s);
    Ok(())
}

/// The effective sandbox policy and the signed grants for the active
/// workspace; `agent_id` narrows it to one agent (its runtime, its grants).
#[tauri::command]
pub(crate) fn get_sandbox_policy(state: State<AppState>, agent_id: Option<String>) -> Result<SandboxPolicyDto, String> {
    let ws = state.active_workspace_id();
    let setting = grants_setting(&state, ws);
    let (grants, here, agents_cfg, my_role, local) = {
        let svc = state.service.lock().unwrap();
        let me = svc.author().id.clone();
        let config = svc.load(hive_core::workspace_config_session_id(ws)).map_err(map_err)?;
        let my_role = config.as_ref().map(|c| c.role_of(&me)).unwrap_or(hive_core::WorkspaceRole::Owner);
        let agents = config.as_ref().map(|c| c.workspace_agents.clone()).unwrap_or_default();
        (svc.sandbox_grants(ws), svc.sandbox_grants_here(ws, setting).1, agents, my_role, svc.is_local_workspace(ws))
    };
    let fp = hive_runtime::sandbox::grants_fingerprint(&grants);
    let seen = state.settings.lock().unwrap().sandbox_grants_seen.get(&ws.to_string()).cloned().unwrap_or_default();
    // The policies show what applies on this device.
    let applied = if here.apply { grants.clone() } else { hive_core::sandbox::SandboxGrants::default() };
    let agents: Vec<SandboxAgentDto> = agents_cfg
        .iter()
        .map(|a| SandboxAgentDto { id: a.id.to_string(), name: a.name.clone(), runtime: format!("{:?}", state.resolve_runtime(&a.runtime_id).provider) })
        .collect();
    let chosen = agent_id.as_deref().filter(|s| !s.is_empty()).map(Uuid::parse_str).transpose().map_err(map_err)?;
    let mut policies = Vec::new();
    match chosen.and_then(|id| agents_cfg.iter().find(|a| a.id == id)) {
        Some(a) => {
            let kind = state.resolve_runtime(&a.runtime_id).provider;
            policies.push(hive_runtime::executor::effective_policy(Some(kind), &applied.paths_for(Some(a.id)), applied.shell_granted(a.id)));
        }
        None => {
            let mut kinds: Vec<hive_core::ModelProviderKind> = agents_cfg.iter().map(|a| state.resolve_runtime(&a.runtime_id).provider).collect();
            kinds.sort_by_key(|k| format!("{k:?}"));
            kinds.dedup();
            for k in kinds {
                policies.push(hive_runtime::executor::effective_policy(Some(k), &applied.paths_for(None), false));
            }
            policies.push(hive_runtime::executor::effective_policy(None, &applied.paths_for(None), false));
        }
    }
    Ok(SandboxPolicyDto {
        policies,
        grants,
        agents,
        can_edit_paths: my_role.rank() >= hive_core::WorkspaceRole::Admin.rank(),
        can_edit_shell: my_role == hive_core::WorkspaceRole::Owner,
        local,
        applies_here: here.apply,
        applies_by_default: here.default,
        applies_setting: here.setting,
        applies_why: here.why,
        changed: fp != seen,
    })
}

/// Replace the active workspace's sandbox grants (signed on the config log;
/// the passkey sheet confirms it). Admin + presence; changing an agent's
/// shell grant is the Owner's.
#[tauri::command(async)]
pub(crate) fn set_sandbox_grants(app: AppHandle, state: State<AppState>, grants: hive_core::sandbox::SandboxGrants) -> Result<(), String> {
    let ws = state.active_workspace_id();
    let clean = |v: Vec<String>| -> Vec<String> {
        let mut v: Vec<String> = v.into_iter().map(|s| s.trim().trim_end_matches('/').to_string()).filter(|s| !s.is_empty()).collect();
        v.sort();
        v.dedup();
        v
    };
    let mut g = hive_core::sandbox::SandboxGrants::default();
    g.workspace.read_only = clean(grants.workspace.read_only);
    g.workspace.read_write = clean(grants.workspace.read_write);
    for (id, a) in grants.agents {
        let a = hive_core::sandbox::AgentGrant { read_only: clean(a.read_only), read_write: clean(a.read_write), shell: a.shell };
        if a != hive_core::sandbox::AgentGrant::default() {
            g.agents.insert(id, a);
        }
    }
    let fp = hive_runtime::sandbox::grants_fingerprint(&g);
    state.service.lock().unwrap().set_sandbox_grants(ws, g).map_err(map_err)?;
    // Your own change needs no notice.
    {
        let mut s = state.settings.lock().unwrap();
        s.sandbox_grants_seen.insert(ws.to_string(), fp);
        crate::save_settings(&state.data_dir, &s);
    }
    let _ = tauri::Emitter::emit(&app, "workspace://synced", 1);
    Ok(())
}

// ---------------------------------------------------------------------------
// Dearth mode
// ---------------------------------------------------------------------------

/// Dearth mode for the active workspace, with pending joins and what it does
/// to each entrance.
#[tauri::command]
pub(crate) fn get_dearth_state(state: State<AppState>) -> Result<hive_proto::DearthStateDto, String> {
    let ws = state.active_workspace_id();
    let servers = state.combined_mcp_servers();
    let runtimes = state.combined_runtimes();
    let svc = state.service.lock().unwrap();
    let me = svc.author().id.clone();
    let config = svc.load(hive_core::workspace_config_session_id(ws)).map_err(map_err)?;
    let guards = config.as_ref().map(|c| c.guards.clone()).unwrap_or_default();
    let members = config.as_ref().map(|c| c.members.clone()).unwrap_or_default();
    let my_role = config.as_ref().map(|c| c.role_of(&me)).unwrap_or(hive_core::WorkspaceRole::Owner);
    let can_manage = my_role.rank() >= hive_core::WorkspaceRole::Admin.rank() || config.is_none();
    let name_of = |id: &str| members.iter().find(|m| m.id == id).map(|m| hive_proto::names::clean_display_name(&m.actor.display_name)).unwrap_or_else(|| id.to_string());
    let mut pending: Vec<hive_proto::PendingJoinDto> = guards
        .pending_members
        .iter()
        .map(|p| hive_proto::PendingJoinDto {
            subject: p.member.id.clone(),
            kind: "member".into(),
            name: hive_proto::names::clean_display_name(&p.member.actor.display_name),
            added_by: name_of(&p.added_by),
            can_approve: can_manage && p.added_by != me,
        })
        .collect();
    let my_account = svc.author().account_id;
    for (device, account) in svc.pending_devices(ws).map_err(map_err)? {
        let owner = members.iter().find(|m| m.actor.account_id == Some(account));
        pending.push(hive_proto::PendingJoinDto {
            subject: device.to_string(),
            kind: "device".into(),
            name: format!("Device {}", &device.to_string()[..8]),
            added_by: owner.map(|m| hive_proto::names::clean_display_name(&m.actor.display_name)).unwrap_or_default(),
            can_approve: can_manage && my_account != Some(account),
        });
    }
    let d = guards.dearth.clone();
    let on = guards.dearth_on().is_some();
    let egress = d
        .as_ref()
        .filter(|d| d.enabled)
        .map(|d| hive_core::sandbox::EgressPolicy::AllowList { mcp_servers: d.allowed_mcp_servers.clone() })
        .unwrap_or(hive_core::sandbox::EgressPolicy::Open);
    let mut entrances: Vec<hive_proto::EntranceDto> = servers
        .iter()
        .filter(|s| s.enabled)
        .map(|s| hive_proto::EntranceDto {
            name: s.id.clone(),
            kind: "mcp".into(),
            state: if !on { "open" } else if egress.allows_mcp(&s.id) { "allowed" } else { "closed" }.into(),
        })
        .collect();
    let closed = if on { "closed" } else { "open" };
    entrances.push(hive_proto::EntranceDto { name: "Web fetch and search (Claude Code)".into(), kind: "web".into(), state: closed.into() });
    entrances.push(hive_proto::EntranceDto { name: "Shell (Bash, run_command)".into(), kind: "shell".into(), state: closed.into() });
    let mut seen = std::collections::HashSet::new();
    for rt in runtimes.iter() {
        if hive_runtime::sandbox::confinable(rt.provider_kind) || !seen.insert(rt.provider_kind) {
            continue;
        }
        entrances.push(hive_proto::EntranceDto {
            name: format!("{:?} agents", rt.provider_kind),
            kind: "runtime".into(),
            state: if on { "refused" } else { "open" }.into(),
        });
    }
    Ok(hive_proto::DearthStateDto {
        enabled: on,
        reason: d.as_ref().map(|d| d.reason.clone()).unwrap_or_default(),
        brood_paths: d
            .as_ref()
            .map(|d| d.brood_paths.clone())
            .unwrap_or_else(|| hive_core::guard::DEFAULT_BROOD_PATHS.iter().map(|s| s.to_string()).collect()),
        allowed_mcp_servers: d.as_ref().map(|d| d.allowed_mcp_servers.clone()).unwrap_or_default(),
        set_by: d.as_ref().map(|d| name_of(&d.set_by)).unwrap_or_default(),
        set_at: d.as_ref().map(|d| crate::rfc3339(d.set_at)).unwrap_or_default(),
        can_manage,
        default_brood_paths: hive_core::guard::DEFAULT_BROOD_PATHS.iter().map(|s| s.to_string()).collect(),
        pending,
        entrances,
    })
}

/// Switch dearth mode on or off for the active workspace (Admin+).
#[tauri::command(async)]
pub(crate) fn set_dearth_mode(
    app: AppHandle,
    state: State<AppState>,
    enabled: bool,
    reason: String,
    brood_paths: Vec<String>,
    allowed_mcp_servers: Vec<String>,
) -> Result<(), String> {
    let ws = state.active_workspace_id();
    let clean = |v: Vec<String>| -> Vec<String> {
        v.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
    };
    let dearth = hive_core::guard::DearthMode {
        enabled,
        reason: reason.trim().chars().take(200).collect(),
        brood_paths: clean(brood_paths),
        allowed_mcp_servers: clean(allowed_mcp_servers),
        admitted_devices: Vec::new(),
        set_by: String::new(),
        set_at: hive_core::Timestamp::now(),
    };
    state.service.lock().unwrap().set_dearth_mode(ws, dearth).map_err(map_err)?;
    let _ = tauri::Emitter::emit(&app, "workspace://synced", 1);
    Ok(())
}

/// Approve a member or device waiting on dearth mode's second approval.
#[tauri::command(async)]
pub(crate) fn approve_join(app: AppHandle, state: State<AppState>, subject: String) -> Result<(), String> {
    let ws = state.active_workspace_id();
    let mut svc = state.service.lock().unwrap();
    svc.approve_join(ws, subject.trim()).map_err(map_err)?;
    let _ = svc.record_signal(
        ws,
        None,
        &GuardSignal::GuardDecision {
            gate: hive_core::signal::GuardGate::DeviceJoin,
            verdict: hive_core::signal::GuardVerdict::Accept,
            subject: hive_core::signal::GuardSubject::Unknown,
            source: "approval".into(),
            reason: "second_approval".into(),
        },
    );
    drop(svc);
    let _ = tauri::Emitter::emit(&app, "workspace://synced", 1);
    Ok(())
}

// ---------------------------------------------------------------------------
// Self-medication: chat lockdown
// ---------------------------------------------------------------------------

/// A chat's lockdown as the chat header shows it.
#[tauri::command]
pub(crate) fn get_chat_lockdown(state: State<AppState>, session_id: String) -> Result<hive_proto::ChatLockdownDto, String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let svc = state.service.lock().unwrap();
    let me = svc.author().id.clone();
    let session = svc.load(sid).map_err(map_err)?.ok_or("unknown chat")?;
    let view = hive_runtime::lockdown_view::chat_lockdown(&session, sid, &me);
    let signals = view
        .signals
        .iter()
        .map(|l| hive_proto::LockdownSignalDto {
            signal: serde_json::to_value(l.signal).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default(),
            detail: l.detail.clone(),
            raised_at: crate::rfc3339(l.raised_at),
            message_id: l.message_id.map(|m| m.to_string()),
            raise_event_id: l.raise_event_id.map(|r| r.to_string()),
        })
        .collect::<Vec<_>>();
    Ok(hive_proto::ChatLockdownDto {
        active: !signals.is_empty(),
        signals,
        can_clear: view.can_clear,
    })
}

/// Clear the lockdown signals of this chat the person was shown, each named
/// by its `raiseEventId` (a human member, Contributor+; re-checked on every
/// device's ingest). A signal raised since, which they weren't shown, stays.
#[tauri::command(async)]
pub(crate) fn clear_chat_lockdown(app: AppHandle, state: State<AppState>, session_id: String, raise_ids: Vec<String>) -> Result<(), String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let raises = raise_ids.iter().map(|r| Uuid::parse_str(r)).collect::<Result<Vec<_>, _>>().map_err(map_err)?;
    let mut svc = state.service.lock().unwrap();
    let ws = svc.load(sid).map_err(map_err)?.ok_or("unknown chat")?.workspace_id;
    svc.clear_lockdown(ws, sid, raises).map_err(map_err)?;
    drop(svc);
    let _ = tauri::Emitter::emit(&app, "workspace://synced", 1);
    Ok(())
}

/// Lock this chat down by hand (anyone may tighten).
#[tauri::command]
pub(crate) fn lock_chat(app: AppHandle, state: State<AppState>, session_id: String) -> Result<(), String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let mut svc = state.service.lock().unwrap();
    let ws = svc.load(sid).map_err(map_err)?.ok_or("unknown chat")?.workspace_id;
    svc.self_medicate(ws, sid, None, &[(hive_core::guard::LockdownSignal::Manual, "manual".into())]).map_err(map_err)?;
    drop(svc);
    let _ = tauri::Emitter::emit(&app, "workspace://synced", 1);
    Ok(())
}

// ---------------------------------------------------------------------------
// Staggered throttles (protect the brood)
// ---------------------------------------------------------------------------

fn now_secs() -> i64 {
    now_ms() / 1000
}

/// Assumed reset when a runtime says "limit reached" without saying when.
const UNKNOWN_RESET_SECS: i64 = 3600;

/// Record a runtime's plan-window report (Claude Code rate-limit events).
pub(crate) fn record_plan_window(state: &AppState, provider: hive_core::ModelProviderKind, mut window: hive_core::throttle::PlanWindow) {
    if window.observed_at == 0 {
        window.observed_at = now_secs();
    }
    state.guards.plan_windows.lock().unwrap().insert(provider, window);
}

/// A failed turn whose error says the subscription limit was hit counts as a
/// rejected window (for the CLIs that only report it as an error).
pub(crate) fn record_usage_error(state: &AppState, provider: hive_core::ModelProviderKind, message: &str) {
    use hive_runtime::provider::cli_agent::{classify_failure, CliFailure};
    if !provider.is_cli_agent() || classify_failure(message) != CliFailure::UsageLimit {
        return;
    }
    let now = now_secs();
    record_plan_window(
        state,
        provider,
        hive_core::throttle::PlanWindow {
            status: hive_core::throttle::WindowStatus::Rejected,
            utilization_pct: Some(100),
            resets_at: Some(now + UNKNOWN_RESET_SECS),
            window: None,
            observed_at: now,
        },
    );
}

fn decide(
    state: &AppState,
    provider: hive_core::ModelProviderKind,
    priority: hive_core::throttle::TurnPriority,
    agent_key: &str,
) -> hive_core::throttle::ThrottleDecision {
    let window = state.guards.plan_windows.lock().unwrap().get(&provider).cloned();
    hive_core::throttle::ThrottlePolicy::default().decide(priority, agent_key, window.as_ref(), now_secs())
}

/// Note (or clear) a deferral for the UI, recording a throttle signal when it
/// starts.
fn note_deferral(
    state: &AppState,
    workspace_id: Uuid,
    key: String,
    what: String,
    provider: hive_core::ModelProviderKind,
    priority: hive_core::throttle::TurnPriority,
    decision: hive_core::throttle::ThrottleDecision,
) {
    let mut deferred = state.guards.deferred.lock().unwrap();
    match decision {
        hive_core::throttle::ThrottleDecision::Run => {
            deferred.remove(&key);
        }
        hive_core::throttle::ThrottleDecision::Defer { utilization_pct, threshold_pct, .. } => {
            let fresh = !deferred.contains_key(&key);
            deferred.insert(
                key,
                hive_proto::DeferredWorkDto {
                    priority: priority.as_str().into(),
                    what,
                    runtime: format!("{provider:?}"),
                    utilization_pct,
                    threshold_pct,
                },
            );
            drop(deferred);
            if fresh {
                if let Ok(svc) = state.service.try_lock() {
                    let _ = svc.record_signal(
                        workspace_id,
                        None,
                        &GuardSignal::Throttle {
                            runtime_id: format!("{provider:?}"),
                            priority: priority.as_str().into(),
                            utilization_pct,
                            threshold_pct,
                            deferred: true,
                        },
                    );
                }
            }
        }
    }
}

/// Wait while a workflow stage's runtime is past this agent's threshold.
/// Returns an error if the run is canceled while waiting.
pub(crate) async fn wait_for_capacity(
    app: &AppHandle,
    provider: hive_core::ModelProviderKind,
    priority: hive_core::throttle::TurnPriority,
    agent_key: &str,
    run_id: Option<Uuid>,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let ws = state.active_workspace_id();
    let key = format!("{}:{agent_key}", run_id.map(|r| r.to_string()).unwrap_or_default());
    loop {
        let d = decide(&state, provider, priority, agent_key);
        note_deferral(&state, ws, key.clone(), format!("workflow stage ({agent_key})"), provider, priority, d);
        let hive_core::throttle::ThrottleDecision::Defer { resets_at, .. } = d else { return Ok(()) };
        if let Some(rid) = run_id {
            if state.canceled_runs.lock().unwrap().contains(&rid) {
                state.guards.deferred.lock().unwrap().remove(&key);
                return Err("canceled while waiting for plan-window capacity".into());
            }
        }
        // Re-check every minute, or at the reset if that's sooner.
        let wait = resets_at.map(|r| (r - now_secs()).clamp(5, 60)).unwrap_or(60) as u64;
        tokio::time::sleep(Duration::from_secs(wait)).await;
    }
}

/// Whether a due schedule should wait (background tier).
pub(crate) fn defer_background(app: &AppHandle, runtime_id: &str, schedule_id: &str) -> bool {
    let state = app.state::<AppState>();
    let rid = if runtime_id.is_empty() { state.current_default_runtime_id() } else { runtime_id.to_string() };
    let provider = state.resolve_runtime(&rid).provider;
    let d = decide(&state, provider, hive_core::throttle::TurnPriority::Background, schedule_id);
    note_deferral(
        &state,
        state.active_workspace_id(),
        format!("schedule:{schedule_id}"),
        "scheduled run".into(),
        provider,
        hive_core::throttle::TurnPriority::Background,
        d,
    );
    matches!(d, hive_core::throttle::ThrottleDecision::Defer { .. })
}

/// For an agent-to-agent hand-off: the note to post when it must pause.
pub(crate) fn cascade_paused(
    state: &AppState,
    workspace_id: Uuid,
    _session_id: Uuid,
    provider: hive_core::ModelProviderKind,
    agent_key: &str,
    agent_name: &str,
) -> Option<String> {
    let d = decide(state, provider, hive_core::throttle::TurnPriority::Workflow, agent_key);
    if let hive_core::throttle::ThrottleDecision::Defer { utilization_pct, threshold_pct, .. } = d {
        note_deferral(
            state,
            workspace_id,
            format!("cascade:{agent_key}"),
            format!("hand-off to {agent_name}"),
            provider,
            hive_core::throttle::TurnPriority::Workflow,
            d,
        );
        // A hand-off isn't retried on its own; it's recorded and then cleared.
        state.guards.deferred.lock().unwrap().remove(&format!("cascade:{agent_key}"));
        return Some(format!(
            "Paused the hand-off to {agent_name}: its runtime's plan window is at {utilization_pct}% (agent hand-offs pause at {threshold_pct}% to keep room for your own requests). Mention it again to continue."
        ));
    }
    None
}

/// Plan windows, thresholds and what's being held back.
#[tauri::command]
pub(crate) fn get_throttle_state(state: State<AppState>) -> hive_proto::ThrottleStateDto {
    let p = hive_core::throttle::ThrottlePolicy::default();
    let now = now_secs();
    let mut windows: Vec<hive_proto::PlanWindowDto> = state
        .guards
        .plan_windows
        .lock()
        .unwrap()
        .iter()
        .map(|(k, w)| hive_proto::PlanWindowDto {
            runtime: format!("{k:?}"),
            status: match w.status {
                hive_core::throttle::WindowStatus::Allowed => "allowed",
                hive_core::throttle::WindowStatus::Warning => "warning",
                hive_core::throttle::WindowStatus::Rejected => "rejected",
            }
            .into(),
            effective_pct: w.effective_pct(now),
            reported: w.utilization_pct.is_some(),
            window: w.window.clone().unwrap_or_default(),
            resets_at: w
                .resets_at
                .map(|r| crate::rfc3339(hive_core::Timestamp::from_unix_seconds(r)))
                .unwrap_or_default(),
            age_secs: (now - w.observed_at).clamp(0, u32::MAX as i64) as u32,
        })
        .collect();
    windows.sort_by(|a, b| a.runtime.cmp(&b.runtime));
    let mut deferred: Vec<_> = state.guards.deferred.lock().unwrap().values().cloned().collect();
    deferred.sort_by(|a, b| a.what.cmp(&b.what));
    hive_proto::ThrottleStateDto {
        background_pct: p.background_pct,
        workflow_pct: p.workflow_pct,
        stagger_pct: p.stagger_pct,
        windows,
        deferred,
    }
}

/// Record which MCP tools a turn was offered, per server, and how many the
/// cap or a name collision dropped (content-free).
pub(crate) fn record_tools_selected(state: &AppState, workspace_id: Uuid, session_id: Uuid, per_server: &[(String, u32, u32)]) {
    let svc = state.service.lock().unwrap();
    for (server, offered, dropped) in per_server {
        let _ = svc.record_signal(
            workspace_id,
            Some(session_id),
            &GuardSignal::ToolsSelected {
                server: server.clone(),
                offered: *offered,
                dropped_cap: *dropped,
                dropped_collision: 0,
                dropped_policy: 0,
            },
        );
    }
}

// ---------------------------------------------------------------------------
// Patch-leaving rule
// ---------------------------------------------------------------------------

/// Record a tool loop's measured progress (and whether it was made to check
/// in) as content-free signals.
pub(crate) fn record_progress(
    state: &AppState,
    workspace_id: Uuid,
    session_id: Uuid,
    progress: &hive_runtime::patch_leaving::ProgressTracker,
) {
    use hive_core::signal::ProgressKind;
    let (tests, writes) = progress.units();
    let svc = state.service.lock().unwrap();
    for (units, kind) in [(tests, ProgressKind::TestsPassing), (writes, ProgressKind::FilesWritten)] {
        if units > 0.0 {
            let _ = svc.record_signal(workspace_id, Some(session_id), &GuardSignal::Progress { units, progress_kind: Some(kind) });
        }
    }
    if progress.checked_in() {
        tracing::info!(target: "guards", chat = %session_id, "patch-leaving rule: agent checked in after a stall");
    }
}

// ---------------------------------------------------------------------------
// Nuc workspaces: grow on occupancy, not up front
// ---------------------------------------------------------------------------

/// What the workspace's occupancy looks like right now.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Occupancy {
    pub agents: usize,
    pub worker_hosts: usize,
    /// Mentions waiting for an agent whose worker host is offline.
    pub queued_on_offline_hosts: usize,
    /// Mentions waiting for an agent that runs on its owner's device.
    pub queued_on_devices: usize,
    /// Work the plan-window throttles are holding back.
    pub deferred: usize,
    /// A subscription window is past the workflow threshold.
    pub hot_window: bool,
    /// Agent replies in the last 7 days, and the busiest agent's share.
    pub agent_replies_7d: usize,
    pub busiest_share: f64,
}

/// Suggestions from occupancy, like adding a box to a hive when about seven of
/// ten frames are drawn: a little ahead of demand, never before it.
pub(crate) fn suggest_growth(o: &Occupancy) -> Vec<hive_proto::GrowthSuggestionDto> {
    let mut out = Vec::new();
    let s = |id: &str, kind: &str, title: &str, reason: String| hive_proto::GrowthSuggestionDto {
        id: id.into(),
        kind: kind.into(),
        title: title.into(),
        reason,
    };
    if o.queued_on_offline_hosts >= 2 || (o.worker_hosts == 0 && o.queued_on_devices >= 3) {
        out.push(s(
            "always-on-host",
            "host",
            "Add an always-on host",
            format!(
                "{} request{} waiting for an agent whose machine is offline. A `ligus serve` box answers while laptops sleep.",
                o.queued_on_offline_hosts + o.queued_on_devices,
                if o.queued_on_offline_hosts + o.queued_on_devices == 1 { " is" } else { "s are" }
            ),
        ));
    }
    if o.hot_window && o.deferred > 0 {
        out.push(s(
            "worker-capacity",
            "worker",
            "Add a worker with its own capacity",
            format!(
                "Agents are pausing near a plan-window limit ({} item{} held back). A worker with a local model or another subscription takes background work off this window.",
                o.deferred,
                if o.deferred == 1 { "" } else { "s" }
            ),
        ));
    }
    if o.agents <= 2 && o.agent_replies_7d >= 40 && o.busiest_share >= 0.8 {
        out.push(s(
            "second-agent",
            "agent",
            "Add another agent",
            format!(
                "One agent wrote {:.0}% of the last {} agent replies. A second agent with its own role spreads the load and lets them review each other.",
                o.busiest_share * 100.0,
                o.agent_replies_7d
            ),
        ));
    }
    out
}

fn gather_occupancy(state: &AppState) -> Occupancy {
    let ws = state.active_workspace_id();
    let me = state.local_actor_id();
    let known = state.known_room_ids();
    let deferred = state.guards.deferred.lock().unwrap().len();
    let policy = hive_core::throttle::ThrottlePolicy::default();
    let now = now_secs();
    let hot_window = state
        .guards
        .plan_windows
        .lock()
        .unwrap()
        .values()
        .any(|w| w.effective_pct(now) >= policy.workflow_pct);
    let svc = state.service.lock().unwrap();
    let hosts = svc.list_hosts(ws).unwrap_or_default();
    let ts_now = hive_core::Timestamp::now();
    let mut o = Occupancy {
        worker_hosts: hosts.iter().filter(|h| h.kind == hive_core::HostKind::Worker).count(),
        deferred,
        hot_window,
        ..Default::default()
    };
    let week_ago = ts_now.inner().unix_timestamp() - 7 * 86_400;
    let mut by_agent: HashMap<String, usize> = HashMap::new();
    let config_id = hive_core::workspace_config_session_id(ws);
    for id in svc.store().list_session_ids().unwrap_or_default() {
        if id == config_id {
            continue;
        }
        let Ok(Some(s)) = svc.load(id) else { continue };
        if !crate::session_in_workspace(s.workspace_id, &s.creator_actor_id, ws, &me, &known) {
            continue;
        }
        o.agents = o.agents.max(s.workspace_agents.len());
        for pm in hive_runtime::chat_service::pending_mentions(&s) {
            let host = s.workspace_agents.iter().find(|a| a.id == pm.agent_id).map(|a| a.host_id.clone()).unwrap_or_default();
            if host.is_empty() {
                o.queued_on_devices += 1;
            } else if !hosts.iter().any(|h| h.id == host && state.host_presence.is_online(h, &ts_now)) {
                o.queued_on_offline_hosts += 1;
            }
        }
        for m in &s.messages {
            if matches!(m.role, hive_core::MessageRole::Assistant | hive_core::MessageRole::Agent)
                && m.created_at.inner().unix_timestamp() >= week_ago
            {
                *by_agent.entry(m.author.clone()).or_default() += 1;
            }
        }
    }
    o.agent_replies_7d = by_agent.values().sum();
    if o.agent_replies_7d > 0 {
        o.busiest_share = *by_agent.values().max().unwrap_or(&0) as f64 / o.agent_replies_7d as f64;
    }
    o
}

/// Growth suggestions for the active workspace (nuc workspaces start small).
#[tauri::command]
pub(crate) fn growth_suggestions(state: State<AppState>) -> Vec<hive_proto::GrowthSuggestionDto> {
    suggest_growth(&gather_occupancy(&state))
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
    fn growth_is_suggested_from_occupancy_only() {
        // A quiet nuc: nothing to suggest.
        assert!(suggest_growth(&Occupancy { agents: 1, ..Default::default() }).is_empty());
        let host = suggest_growth(&Occupancy { queued_on_offline_hosts: 2, worker_hosts: 1, ..Default::default() });
        assert_eq!(host[0].id, "always-on-host");
        let worker = suggest_growth(&Occupancy { hot_window: true, deferred: 3, ..Default::default() });
        assert_eq!(worker[0].kind, "worker");
        // A hot window with nothing held back isn't a reason yet.
        assert!(suggest_growth(&Occupancy { hot_window: true, ..Default::default() }).is_empty());
        let agent = suggest_growth(&Occupancy { agents: 1, agent_replies_7d: 50, busiest_share: 0.9, ..Default::default() });
        assert_eq!(agent[0].id, "second-agent");
        assert!(suggest_growth(&Occupancy { agents: 1, agent_replies_7d: 10, busiest_share: 1.0, ..Default::default() }).is_empty());
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

#[cfg(test)]
mod egress_preset_settings_tests {
    use super::*;
    use hive_runtime::egress::EgressPreset;

    /// Off by default; the chat's own setting wins over the workspace's, and
    /// an empty chat list switches the preset off for that chat.
    #[test]
    fn chat_setting_overrides_workspace_and_default_is_off() {
        let (ws, chat, other) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        let mut by_ws = std::collections::HashMap::new();
        let mut by_chat = std::collections::HashMap::new();
        assert!(effective_egress_presets(&by_ws, &by_chat, ws, chat).is_empty());
        by_ws.insert(ws.to_string(), vec!["package-registries".to_string()]);
        assert_eq!(effective_egress_presets(&by_ws, &by_chat, ws, chat), vec![EgressPreset::PackageRegistries]);
        by_chat.insert(chat.to_string(), Vec::new());
        assert!(effective_egress_presets(&by_ws, &by_chat, ws, chat).is_empty());
        assert_eq!(effective_egress_presets(&by_ws, &by_chat, ws, other), vec![EgressPreset::PackageRegistries]);
        by_ws.clear();
        by_chat.insert(chat.to_string(), vec!["package-registries".into(), "bogus".into()]);
        assert_eq!(effective_egress_presets(&by_ws, &by_chat, ws, chat), vec![EgressPreset::PackageRegistries]);
        assert!(parse_preset_names(&["bogus".to_string()]).is_err());
    }
}
