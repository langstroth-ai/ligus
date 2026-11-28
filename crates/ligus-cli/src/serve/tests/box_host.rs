//! The box as a host (workspace model v3, 10.1, 5.10): it writes its label
//! only when `--label` changed, its first beat carries its build and offers,
//! and after `ligus serve leave` it writes nothing of its own any more.

use super::*;

struct World {
    ws: Uuid,
    desktop: Config,
    boxc: Config,
    dir: PathBuf,
    _tmp: tempfile::TempDir,
}

/// A desktop (founder) and a box admitted to its workspace, not started.
async fn world() -> World {
    world_on(None).await
}

/// [`world`], founded on `rules` (this build's default when `None`).
async fn world_on(rules: Option<u32>) -> World {
    let tmp = tempfile::tempdir().unwrap();
    let relay = spawn(test_relay::router()).await;
    let room = format!("serve-host-{}", Uuid::new_v4());
    let ws = crate::uuid_of_room(&room);
    let desktop = config(&tmp.path().join("desktop"), &relay, &room);
    let code = {
        let mut svc = open_service(&desktop).unwrap();
        if let Some(r) = rules {
            svc.set_founding_rules(r);
        }
        svc.ensure_self_member(workspace_config_session_id(ws), ws).unwrap();
        let (inv, seed) = svc.issue_box_admission(ws, 3_600_000, None).unwrap();
        crate::serve::admission_code(&desktop, &svc, inv, &seed).unwrap()
    };
    sync_once(&desktop).await.unwrap();
    let dir = tmp.path().join("serve");
    std::fs::create_dir_all(&dir).unwrap();
    let mut boxc = config(&dir, &relay, &room);
    crate::admit::redeem(&mut boxc, &code).unwrap();
    World { ws, desktop, boxc, dir, _tmp: tmp }
}

impl World {
    fn opts(&self, label: &str, given: bool) -> ServeOptions {
        ServeOptions { token_file: self.dir.join("serve.token"), dashboard_file: self.dir.join("dashboard.toml"), label_given: given, ..box_opts(label) }
    }

    async fn start(&self, label: &str, given: bool) -> ServeHandle {
        start(self.boxc.clone(), self.opts(label, given)).await.unwrap()
    }

    /// The box's synced device name and its host record's label, as the
    /// box itself has them.
    fn names(&self, h: &ServeHandle) -> (Option<String>, Option<String>) {
        let svc = h.shared.lock();
        let device = svc.device_id();
        let synced = svc.device_label_of(self.ws, device, svc.author().account_id.unwrap());
        let host = svc.list_hosts(self.ws).unwrap().into_iter().find(|x| x.id == device.to_string()).map(|x| x.label);
        (synced, host)
    }
}

async fn stop(h: ServeHandle) {
    h.stop(Duration::from_secs(5)).await;
}

#[test]
fn the_label_is_written_only_when_it_changed() {
    // First start: whatever the box is called, once.
    assert_eq!(label_to_write(false, "sff", None, None), Some("sff"));
    assert_eq!(label_to_write(true, "sff", None, None), Some("sff"));
    // A hostname is never written again.
    assert_eq!(label_to_write(false, "sff", Some("sff"), Some(("Kitchen box", false))), None);
    assert_eq!(label_to_write(false, "new-hostname", Some("sff"), Some(("sff", true))), None);
    // The same --label after a desktop rename: the rename stays.
    assert_eq!(label_to_write(true, "sff", Some("sff"), Some(("Kitchen box", false))), None);
    // A new --label renames.
    assert_eq!(label_to_write(true, "attic", Some("sff"), Some(("Kitchen box", false))), Some("attic"));
    // An upgrade (no record yet): a name set elsewhere is kept, one this
    // box set counts as its last.
    assert_eq!(label_to_write(true, "sff", None, Some(("Kitchen box", false))), None);
    assert_eq!(label_to_write(true, "sff", None, Some(("sff", true))), None);
    assert_eq!(label_to_write(true, "attic", None, Some(("sff", true))), Some("attic"));
    assert_eq!(label_to_write(true, "  ", None, None), None);
}

/// U6: a rename of the box made elsewhere survives the box's restart with
/// the same `--label`; a new `--label` changes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rename_survives_a_restart_and_a_new_label_renames() {
    let w = world().await;
    let h = w.start("sff", true).await;
    assert_eq!(w.names(&h), (Some("sff".into()), Some("sff".into())));
    assert_eq!(std::fs::read_to_string(w.dir.join(LAST_SELF_LABEL_FILE)).unwrap(), "sff");
    // Renamed (a synced DeviceLabelSet for the box's device).
    {
        let mut svc = h.shared.lock();
        let device = svc.device_id();
        assert!(svc.set_device_label(w.ws, device, "Kitchen box").unwrap());
    }
    stop(h).await;

    // Same label: the rename stays, and the host record follows it.
    let h = w.start("sff", true).await;
    assert_eq!(w.names(&h), (Some("Kitchen box".into()), Some("Kitchen box".into())));
    assert_eq!(h.shared.opts.label, "Kitchen box", "the box goes by its synced name");
    let rows = h.shared.lock().store().max_row_id().unwrap();
    stop(h).await;

    // Again: nothing written for the label or the record.
    let h = w.start("sff", true).await;
    assert_eq!(w.names(&h), (Some("Kitchen box".into()), Some("Kitchen box".into())));
    assert_eq!(h.shared.lock().store().max_row_id().unwrap(), rows, "a restart wrote an event");
    stop(h).await;

    // A hostname start (no --label) never renames either.
    let h = w.start("some-hostname", false).await;
    assert_eq!(w.names(&h).0.as_deref(), Some("Kitchen box"));
    stop(h).await;

    // A new --label does.
    let h = w.start("attic", true).await;
    assert_eq!(w.names(&h), (Some("attic".into()), Some("attic".into())));
    assert_eq!(std::fs::read_to_string(w.dir.join(LAST_SELF_LABEL_FILE)).unwrap(), "attic");
    stop(h).await;
    let _ = &w.desktop;
}

/// The beat this box publishes, as a member of the workspace reads it.
async fn own_beat(h: &ServeHandle) -> Option<ligus_core::workspace_repo::HostBeat> {
    let shared = &h.shared;
    let relay = crate::relay_client(&shared.cfg).unwrap();
    let ring = match crate::key_source(&shared.cfg) {
        Ok(src) => ligus_runtime::keyring::KeyringEngine::from_store(shared.lock().store(), shared.ws, &src),
        Err(_) => ligus_runtime::keyring::KeyringEngine::passphrase(shared.ws, ligus_core::derive_workspace_key(shared.cfg.key.as_deref().unwrap_or_default())),
    };
    let beats = ligus_runtime::host_presence::fetch_beats(&relay, &shared.cfg.room, &ring).await.ok()?;
    beats.get(&shared.host_id).cloned()
}

/// The first beat carries the box's signed status with its build version,
/// and its host record lists its offers (A4's box side).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_first_beat_carries_the_build_and_the_record_its_offers() {
    let w = world().await;
    let h = w.start("sff", true).await;
    let mut beat = None;
    for _ in 0..100 {
        h.shared.beat_kick.notify_one();
        if let Some(b) = own_beat(&h).await.filter(|b| !b.status.is_empty()) {
            beat = Some(b);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let beat = beat.expect("a beat with a status");
    let roster = h.shared.lock().workspace_roster(w.ws).unwrap();
    let status = beat.status_verified(w.ws, &roster).expect("the status verifies for the box");
    assert_eq!(status.build, env!("CARGO_PKG_VERSION"));
    let host = h.shared.lock().list_hosts(w.ws).unwrap().into_iter().find(|x| x.id == h.shared.host_id).unwrap();
    assert!(host.offers.is_some(), "the record lists its offers");
    stop(h).await;
}

/// The time of the box's first beat.
async fn first_beat(h: &ServeHandle) -> i64 {
    for _ in 0..100 {
        h.shared.beat_kick.notify_one();
        if let Some(b) = own_beat(h).await {
            return b.at;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("a first beat")
}

/// The box's beat times over a few kicks.
async fn beats_after(h: &ServeHandle) -> Vec<Option<i64>> {
    let mut after = Vec::new();
    for _ in 0..5 {
        h.shared.beat_kick.notify_one();
        tokio::time::sleep(Duration::from_millis(300)).await;
        after.push(own_beat(h).await.map(|b| b.at));
    }
    after
}

/// W3 (box leave): after `ligus serve leave` the box publishes no more
/// beats, and a restart writes no host record or label.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_box_that_left_stops_beating_and_writes_nothing_of_its_own() {
    // Under rules 3 a leave is a request: the box stays on the roster until
    // an Admin removes it. Rules 4 (a leave removes it) is the next test.
    let w = world_on(Some(ligus_core::authz::RULES_V3)).await;
    let h = w.start("sff", true).await;
    let first = first_beat(&h).await;
    crate::leave_cmd::cmd_serve_leave(&w.boxc, true, true).await.unwrap();
    assert!(h.shared.lock().has_left(w.ws).unwrap());
    // Past the second the beat's time would move by.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let after = beats_after(&h).await;
    // One beat may have been in flight when the leave landed.
    let last = after.last().cloned().flatten();
    assert!(last.is_none_or(|t| t <= first + 2), "the box kept beating after it left: {first} then {after:?}");
    let rows = h.shared.lock().store().max_row_id().unwrap();
    stop(h).await;

    // A restart, even with a new label: nothing of its own is written.
    let h = w.start("attic", true).await;
    assert_eq!(h.shared.lock().store().max_row_id().unwrap(), rows, "a box that left wrote an event");
    assert_eq!(w.names(&h).0.as_deref(), Some("sff"));
    stop(h).await;
}

/// Rules 4: leaving removes the box, so a restart after `ligus serve leave`
/// keeps it out. It starts, sees that it left, writes nothing, publishes no
/// beat, and says how to rejoin (only an Admin's re-add lifts the removal).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn under_rules_4_a_box_that_left_stays_out_after_a_restart() {
    let w = world().await;
    let h = w.start("sff", true).await;
    {
        let svc = h.shared.lock();
        let rules = svc.load_in(w.ws, workspace_config_session_id(w.ws)).unwrap().unwrap().authz.rules;
        assert!(rules >= ligus_core::authz::RULES_V4, "this build founds on rules 4 (got {rules})");
    }
    let first = first_beat(&h).await;
    crate::leave_cmd::cmd_serve_leave(&w.boxc, true, true).await.unwrap();
    let rows = h.shared.lock().store().max_row_id().unwrap();
    stop(h).await;

    // A restart, even with a new label: it starts, out of the workspace.
    let h = w.start("attic", true).await;
    {
        let svc = h.shared.lock();
        assert!(svc.has_left(w.ws).unwrap());
        assert!(crate::admit::left_and_out(&svc, w.ws).unwrap(), "the leave removed the box");
        assert!(svc.write_block().is_some(), "a box that left writes nothing");
        assert_eq!(svc.store().max_row_id().unwrap(), rows, "a box that left wrote an event on start");
    }
    assert_eq!(w.names(&h).0.as_deref(), Some("sff"), "a box that left renamed itself");
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let after = beats_after(&h).await;
    assert!(after.iter().flatten().all(|t| *t <= first + 2), "a box that left beat after a restart: {first} then {after:?}");
    assert_eq!(h.shared.lock().store().max_row_id().unwrap(), rows, "a box that left wrote an event");
    stop(h).await;

    assert_eq!(
        crate::admit::left_line("home"),
        "This box left home. To rejoin: ligus admit request (an Admin approves)."
    );
}
