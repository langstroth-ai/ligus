//! `ligus remote …` and `ligus phones …`: separate processes that hand work to a
//! running `ligus serve` through files in `tunnel/` (§16).

use std::path::Path;

use anyhow::{bail, Result};

use super::{
    ensure_private_dir, pending_dir, push_test_request_file, push_test_result_file, random_id, read_json, tunnel_dir, write_json,
    HostKeys, LiveStatus, PhoneHop, PushTestReport, PushTestRequest, Registry, RelaySettings, RevokeFile,
};

/// `ligus remote enable --relay <url> [--reg-token - | @FILE] [--insecure-relay]`.
pub fn remote_enable(data_dir: &Path, relay: Option<String>, reg_token: Option<String>, insecure: bool) -> Result<()> {
    let Some(relay) = relay else { bail!("usage: ligus remote enable --relay <https://relay.example.com> [--reg-token - | @FILE]") };
    let settings = RelaySettings::new(&relay, reg_token, insecure)?;
    let dir = tunnel_dir(data_dir);
    ensure_private_dir(&dir)?;
    let keys = HostKeys::load_or_create(&dir)?;
    write_json(&RelaySettings::file(&dir), &settings)?;
    println!("Remote access enabled through {} (host id {}).", settings.relay, keys.host_id());
    println!("Restart `ligus serve` to connect, then run `ligus pair` to pair a phone.");
    Ok(())
}

/// `ligus remote disable`: keeps keys and phones, so enabling again needs no re-pairing.
pub fn remote_disable(data_dir: &Path) -> Result<()> {
    let dir = tunnel_dir(data_dir);
    match read_json::<RelaySettings>(&RelaySettings::file(&dir))? {
        Some(mut s) => {
            s.enabled = false;
            write_json(&RelaySettings::file(&dir), &s)?;
            println!("Remote access disabled. Restart `ligus serve` to drop the relay connection.");
        }
        None => println!("Remote access is not enabled."),
    }
    Ok(())
}

/// `ligus remote status --json`: the same facts as one JSON document.
fn remote_status_json(data_dir: &Path) -> Result<serde_json::Value> {
    use serde_json::json;
    let dir = tunnel_dir(data_dir);
    let now = super::now();
    let relay = read_json::<RelaySettings>(&RelaySettings::file(&dir))?;
    let live = read_json::<LiveStatus>(&dir.join("status.json"))?;
    let phones = Registry::load(&dir)?.list();
    Ok(json!({
        "relay": relay.as_ref().map(|s| json!({ "url": s.relay, "enabled": s.enabled })),
        "hostId": HostKeys::load(&dir)?.map(|k| k.host_id().to_string()),
        "serve": live.as_ref().map(|st| json!({
            "connected": st.connected,
            "status": st.status,
            "push": st.push,
            "lastError": st.last_error,
            "connectedSince": st.connected_since,
            "description": describe_status(st, now),
        })),
        "phones": {
            "active": phones.iter().filter(|p| p.is_active()).count(),
            "revoked": phones.iter().filter(|p| !p.is_active()).count(),
            "list": phones.iter().filter(|p| p.is_active()).map(|p| json!({
                "pid": p.pid,
                "name": p.name,
                "pushEnabled": p.push_enabled,
                "pushPreviews": p.push_previews,
                "lastSeen": p.last_seen,
            })).collect::<Vec<_>>(),
        },
    }))
}

/// `ligus remote status [--json]`.
pub fn remote_status(data_dir: &Path, json: bool) -> Result<()> {
    if json {
        println!("{}", remote_status_json(data_dir)?);
        return Ok(());
    }
    let dir = tunnel_dir(data_dir);
    match read_json::<RelaySettings>(&RelaySettings::file(&dir))? {
        Some(s) => println!("relay:     {} ({})", s.relay, if s.enabled { "enabled" } else { "disabled" }),
        None => println!("relay:     not configured (ligus remote enable --relay <url>)"),
    }
    match HostKeys::load(&dir)? {
        Some(k) => println!("host id:   {}", k.host_id()),
        None => println!("host id:   (no keys yet)"),
    }
    if let Some(st) = read_json::<LiveStatus>(&dir.join("status.json"))? {
        println!("serve:     {}", describe_status(&st, super::now()));
    }
    let phones = Registry::load(&dir)?.list();
    println!("phones:    {} active, {} revoked", phones.iter().filter(|p| p.is_active()).count(), phones.iter().filter(|p| !p.is_active()).count());
    let now = super::now();
    for p in phones.iter().filter(|p| p.is_active()) {
        let push = match (p.push_enabled, p.push_previews) {
            (true, true) => "notifications on (previews on)",
            (true, false) => "notifications on (previews off)",
            (false, _) => "notifications OFF (the app hasn't turned them on for this host)",
        };
        let seen = p.last_seen.map(|t| format!(", last seen {} ago", short_age(now.saturating_sub(t)))).unwrap_or_default();
        println!("  {}: {push}{seen}", p.name);
    }
    Ok(())
}

/// `ligus remote push-test [--phone <pid|name>]`: asks the running `ligus
/// serve` to send a test notification to the paired phones, and prints what
/// happened at each hop (box → relay → APNs).
pub fn remote_push_test(data_dir: &Path, phone: Option<&str>) -> Result<()> {
    let dir = tunnel_dir(data_dir);
    // relay.json (`ligus remote enable`) or LIGUS_REMOTE_RELAY, as serve reads it.
    let relay = RelaySettings::resolve(data_dir, None, None, false).ok().flatten().map(|s| s.relay);
    let now = super::now();
    let st = read_json::<LiveStatus>(&dir.join("status.json"))?;
    let running = st.as_ref().is_some_and(|s| s.heartbeat > 0 && now.saturating_sub(s.heartbeat) <= 3 * super::STATUS_HEARTBEAT.as_secs());
    if !running {
        match relay {
            None => bail!("remote access isn't enabled here (ligus remote enable --relay <url>), so there is no push path to test"),
            Some(r) => bail!(
                "ligus serve doesn't seem to be running with remote access (relay {r}): start it, check `ligus remote status`, then run this again"
            ),
        }
    }
    let id = random_id();
    write_json(&push_test_request_file(&dir, &id), &PushTestRequest { phone: phone.map(str::to_string), exp: now + 60 })?;
    println!("Sending a test notification through {} …", relay.as_deref().unwrap_or("the relay"));
    let result = push_test_result_file(&dir, &id);
    let deadline = std::time::Instant::now() + super::PUSH_TEST_WAIT + super::PENDING_POLL + std::time::Duration::from_secs(5);
    let report = loop {
        if let Some(r) = read_json::<PushTestReport>(&result).ok().flatten() {
            let _ = std::fs::remove_file(&result);
            break r;
        }
        if std::time::Instant::now() >= deadline {
            let _ = std::fs::remove_file(push_test_request_file(&dir, &id));
            bail!("ligus serve didn't pick up the test (it checks every 2 s): is it running from this data dir ({})?", data_dir.display());
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    };
    let (text, ok) = describe_push_test(&report);
    print!("{text}");
    if !ok {
        bail!("the test notification didn't get through (see above)");
    }
    Ok(())
}

/// The push-test report as text, and whether every phone's push was
/// accepted by APNs (or at least by the relay, for an older relay).
pub(crate) fn describe_push_test(r: &PushTestReport) -> (String, bool) {
    use std::fmt::Write;
    let mut out = String::new();
    let mut ok = true;
    let _ = writeln!(out, "1. this box:  host id {}", r.host_id);
    if let Some(e) = &r.error {
        let _ = writeln!(out, "   {e}");
        return (out, false);
    }
    let _ = writeln!(
        out,
        "2. relay:     {} ({}, push {})",
        r.relay,
        if r.connected { "connected" } else { "NOT connected" },
        if r.push_available { "available" } else { "UNAVAILABLE" }
    );
    if !r.connected {
        let _ = writeln!(out, "   => ligus serve isn't connected to the relay: see `ligus remote status` and its log.");
        return (out, false);
    }
    if !r.push_available {
        let _ = writeln!(out, "   => the relay has no APNs configured (its LANGSTROTH_RELAY_APNS_* settings); it can't push to any phone.");
        ok = false;
    }
    if r.phones.is_empty() {
        let _ = writeln!(out, "3. phones:    none paired (ligus pair)");
        return (out, false);
    }
    for p in &r.phones {
        let (line, good) = describe_hop(p);
        let _ = write!(out, "{line}");
        ok &= good;
    }
    (out, ok)
}

fn describe_hop(p: &PhoneHop) -> (String, bool) {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out, "3. phone \"{}\" ({}):", p.name, p.pid);
    if !p.active {
        let _ = writeln!(out, "   revoked: no pushes");
        return (out, false);
    }
    let _ = writeln!(
        out,
        "   on this box: notifications {}{}; app open right now: {}",
        if p.push_enabled { "on" } else { "OFF" },
        if p.push_enabled { if p.previews { " (previews on)" } else { " (previews off)" } } else { "" },
        if p.app_open { "yes (a real push would be skipped while it is)" } else { "no" },
    );
    if !p.push_enabled {
        let _ = writeln!(
            out,
            "   => the phone never asked this box for pushes. In the app: Settings → Notifications → on (and allow notifications when iOS asks). Nothing was sent."
        );
        return (out, false);
    }
    if !p.sent {
        let _ = writeln!(out, "   box → relay: NOT sent (the relay connection dropped)");
        return (out, false);
    }
    let _ = writeln!(out, "   box → relay: sent (encrypted for this phone)");
    let Some(a) = &p.relay else {
        let _ = writeln!(out, "   relay → APNs: no answer from the relay within {} s", super::PUSH_TEST_WAIT.as_secs());
        return (out, false);
    };
    let detail = a.detail.as_deref().unwrap_or("");
    if a.ok {
        if detail.is_empty() {
            let _ = writeln!(out, "   relay → APNs: accepted (this relay doesn't report APNs details; check its `tunnel push` log)");
        } else {
            let _ = writeln!(out, "   relay → APNs: {detail}");
        }
        let _ = writeln!(
            out,
            "   => APNs accepted it. If the phone shows nothing: check iOS Settings → Notifications → Ligus, Focus, and that the app was opened once since installing (it stores the key the notification extension decrypts with)."
        );
        return (out, true);
    }
    let code = a.code.as_deref().unwrap_or("?");
    let _ = writeln!(out, "   relay → APNs: refused: {code}{}", if detail.is_empty() { String::new() } else { format!(" — {detail}") });
    let _ = writeln!(out, "   => {}", push_failure_advice(code, detail));
    (out, false)
}

/// What to do about a refused push, from the relay's code and APNs reason.
pub(crate) fn push_failure_advice(code: &str, detail: &str) -> &'static str {
    let has = |s: &str| detail.contains(s);
    match code {
        "no_push_token" => "the relay has no APNs token for this phone. Open the app (notifications allowed); it registers its token with the relay when it connects. If the relay restarted without durable storage, it forgot the token: opening the app fixes that too.",
        "push_unavailable" => "the relay has no APNs configured (LANGSTROTH_RELAY_APNS_*).",
        "rate_limited" => "the relay's push rate for this host is used up; try again in a minute.",
        "revoked" => "the relay has this phone as revoked; pair it again (ligus pair).",
        "push_failed" if has("not allowed") => "the phone's app (its bundle id) isn't one this relay pushes to: add it to LANGSTROTH_RELAY_APNS_TOPICS on the relay and restart it.",
        "push_failed" if has("BadDeviceToken") => "APNs says the token doesn't fit: usually the environment. A build run from Xcode has a sandbox (development) token; the relay must use LANGSTROTH_RELAY_APNS_ENV=auto or sandbox for it. Open the app again so it re-registers.",
        "push_failed" if has("DeviceTokenNotForTopic") || has("TopicDisallowed") => "APNs says the token belongs to another app than the topic used: set LANGSTROTH_RELAY_APNS_TOPIC / LANGSTROTH_RELAY_APNS_TOPICS to the bundle id of the build on the phone (and update the app so it reports its bundle id).",
        "push_failed" if has("InvalidProviderToken") || has("MissingProviderToken") || has("ExpiredProviderToken") => "APNs rejected the relay's key: check LANGSTROTH_RELAY_APNS_KEY_ID, LANGSTROTH_RELAY_APNS_TEAM_ID and the .p8 key (same Apple team as the app).",
        "push_failed" if has("Unregistered") || has(": 410") => "APNs says the app is no longer registered on that phone (uninstalled or reinstalled). Open the app so it registers a new token.",
        "push_failed" if has("unreachable") => "the relay couldn't reach APNs; check its network and try again.",
        "push_failed" => "APNs refused it or was unreachable; the relay's `tunnel push` log line has the APNs status and reason.",
        _ => "see the relay's log (`tunnel push` lines).",
    }
}

/// `secs` as a short age: `45s`, `12m`, `6h 33m`, `3d 4h`.
fn short_age(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h {}m", s / 3600, s % 3600 / 60),
        s => format!("{}d {}h", s / 86_400, s % 86_400 / 3600),
    }
}

/// The `serve:` line of `ligus remote status`. Connected shows how long the
/// relay link has been up, not when the file last changed (a link that stays
/// up never changes it, so "updated 6h ago" read like a stale file). A file
/// whose heartbeat stopped was left by a `ligus serve` that isn't running.
pub(crate) fn describe_status(st: &LiveStatus, now: u64) -> String {
    let detail = format!(
        "status {}, push {}",
        if st.status.is_empty() { "-" } else { st.status.as_str() },
        if st.push { "available" } else { "unavailable" }
    );
    let error = st.last_error.as_ref().map(|e| format!(", last error: {e}")).unwrap_or_default();
    let beat = (st.heartbeat > 0).then(|| now.saturating_sub(st.heartbeat));
    if let Some(b) = beat.filter(|b| *b > 3 * super::STATUS_HEARTBEAT.as_secs()) {
        let was = if st.connected { "connected" } else { "not connected" };
        return format!("unknown: ligus serve last reported {} ago ({was}, {detail}) — is it running?{error}", short_age(b));
    }
    if st.connected {
        match (st.connected_since, beat) {
            (Some(since), Some(b)) => {
                format!("connected for {} ({detail}, checked {} ago){error}", short_age(now.saturating_sub(since)), short_age(b))
            }
            // A status file from before connection age was recorded.
            _ => format!("connected ({detail}, last change {} ago){error}", short_age(now.saturating_sub(st.at))),
        }
    } else {
        format!("not connected ({detail}, since {} ago){error}", short_age(now.saturating_sub(st.at)))
    }
}

/// `ligus remote reset`: new host keys (new host id); every phone must re-pair.
pub fn remote_reset(data_dir: &Path) -> Result<()> {
    let dir = tunnel_dir(data_dir);
    ensure_private_dir(&dir)?;
    let keys = HostKeys::generate();
    keys.save(&dir)?;
    for f in [Registry::file(&dir), dir.join("status.json")] {
        let _ = std::fs::remove_file(f);
    }
    let _ = std::fs::remove_dir_all(pending_dir(&dir));
    println!("New host keys: host id {}. Every phone must pair again; restart `ligus serve`.", keys.host_id());
    Ok(())
}

/// A pairing's state for `ligus phones list`: `active`, `unverified` (an
/// active pairing from before pairings named their device: no "you"
/// notices), `unpaired` (ended here or by the phone) or `revoked`.
fn state(p: &super::PhoneRecord) -> &'static str {
    match (p.is_active(), p.device_id.is_some(), p.unpaired) {
        (true, true, _) => "active",
        (true, false, _) => "unverified",
        (false, _, Some(_)) => "unpaired",
        (false, _, None) => "revoked",
    }
}

/// `ligus phones list [--json]`.
pub fn phones_list(data_dir: &Path, json: bool) -> Result<()> {
    let phones = Registry::load(&tunnel_dir(data_dir))?.list();
    if json {
        let revocations = super::revocation::Ledger::load(&tunnel_dir(data_dir)).list();
        let v = serde_json::json!({
            "phones": phones.iter().map(|p| serde_json::json!({
                "pid": p.pid,
                "name": p.name,
                "state": state(p),
                "deviceId": p.device_id,
                "unpaired": p.unpaired.map(|u| u.as_str()),
                "pairedAt": p.enrolled_at,
                "lastSeen": p.last_seen,
                "pushEnabled": p.push_enabled,
            })).collect::<Vec<_>>(),
            "revocations": revocations.iter().map(|e| serde_json::json!({
                "name": e.status.name,
                "deviceId": e.status.device_id,
                "summary": e.status.summary(),
            })).collect::<Vec<_>>(),
        });
        println!("{v}");
        return Ok(());
    }
    if phones.is_empty() {
        println!("No paired phones. Run `ligus pair` with remote access enabled.");
        return Ok(());
    }
    println!("{:<24} {:<24} {:<10} {:<12} last seen", "PID", "NAME", "STATE", "PAIRED");
    for p in phones {
        let fmt_day = |t: u64| {
            time::OffsetDateTime::from_unix_timestamp(t as i64).map(|d| d.date().to_string()).unwrap_or_default()
        };
        println!(
            "{:<24} {:<24} {:<10} {:<12} {}",
            p.pid,
            p.name,
            state(&p),
            fmt_day(p.enrolled_at),
            p.last_seen.map(fmt_day).unwrap_or_else(|| "-".into())
        );
    }
    let revocations = super::revocation::Ledger::load(&tunnel_dir(data_dir)).list();
    if !revocations.is_empty() {
        println!("\nRevoked phone devices (each step; unfinished steps retry while `ligus serve` runs):");
        for e in revocations {
            let name = if e.status.name.is_empty() { "phone".to_string() } else { e.status.name.clone() };
            println!("  {name} (device {}): {}", e.status.device_id, e.status.summary());
            for (what, step) in [("tunnel", &e.status.tunnel), ("push", &e.status.push), ("session", &e.status.session)] {
                if !step.detail.is_empty() {
                    println!("    {what:<8} {}", step.detail);
                }
            }
        }
    }
    Ok(())
}

/// `ligus phones revoke <pid|name> [--keep-token]` (§13.2). Marks the phone
/// revoked in `phones.json` and leaves `pending/revoke-<pid>.json` for
/// `ligus serve` (end live sessions, tell the relay until it acks). Unless
/// `--keep-token`, it also rotates the direct-path bearer token.
pub fn phones_revoke(data_dir: &Path, who: Option<&str>, token_file: &Path, keep_token: bool) -> Result<()> {
    let Some(who) = who else { bail!("usage: ligus phones revoke <pid|name> [--keep-token]") };
    let dir = tunnel_dir(data_dir);
    let reg = Registry::load(&dir)?;
    let Some(rec) = reg.resolve(who) else { bail!("no phone '{who}' (see `ligus phones list`; names must be unique)") };
    reg.mark_unpaired(&rec.pid, super::phones::Unpaired::Box)?;
    let exp = reg.revoke(&rec.pid)?.unwrap_or(rec.cred_exp);
    write_json(
        &pending_dir(&dir).join(format!("revoke-{}.json", rec.pid)),
        &RevokeFile { pid: rec.pid.clone(), exp, unpaired: Some(super::phones::Unpaired::Box) },
    )?;
    println!("Revoked {} ({}). A running `ligus serve` ends its sessions and tells the relay within seconds.", rec.name, rec.pid);
    if rec.device_id.is_some() {
        println!("The phone's person is asked on their desktop whether to remove the device from their devices too.");
    }
    if keep_token {
        println!("The direct-path bearer token was kept (--keep-token).");
    } else if token_file.exists() {
        crate::serve::pair::rotate_token(token_file)?;
        println!(
            "Rotated the direct-path bearer token ({}): phones using the direct path must pair again (`ligus pair`).",
            token_file.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod push_test_tests {
    use super::super::RelayAck;
    use super::*;

    fn report(phones: Vec<PhoneHop>) -> PushTestReport {
        PushTestReport { relay: "https://relay.example".into(), host_id: "hid".into(), connected: true, push_available: true, phones, error: None }
    }

    fn hop(enabled: bool, relay: Option<RelayAck>) -> PhoneHop {
        PhoneHop { pid: "pid1".into(), name: "iPhone".into(), active: true, push_enabled: enabled, previews: true, sent: relay.is_some(), relay, ..Default::default() }
    }

    #[test]
    fn delivered_reads_as_each_hop_and_succeeds() {
        let detail = "apns sandbox topic com.zazulam.ligus.lab: 200 (apns-id X)";
        let (text, ok) = describe_push_test(&report(vec![hop(true, Some(RelayAck { ok: true, code: None, detail: Some(detail.into()) }))]));
        assert!(ok, "{text}");
        assert!(text.contains("relay:     https://relay.example (connected, push available)"), "{text}");
        assert!(text.contains("notifications on (previews on)"), "{text}");
        assert!(text.contains("box → relay: sent"), "{text}");
        assert!(text.contains(detail), "{text}");
    }

    #[test]
    fn each_failure_names_its_hop_and_the_fix() {
        let (text, ok) = describe_push_test(&report(vec![hop(false, None)]));
        assert!(!ok && text.contains("notifications OFF") && text.contains("Settings → Notifications"), "{text}");
        let refused = |code: &str, detail: &str| {
            describe_push_test(&report(vec![hop(true, Some(RelayAck { ok: false, code: Some(code.into()), detail: Some(detail.into()) }))]))
        };
        let (text, ok) = refused("no_push_token", "no push token");
        assert!(!ok && text.contains("refused: no_push_token") && text.contains("Open the app"), "{text}");
        let (text, _) = refused("push_failed", "apns production topic com.zazulam.ligus: 400 BadDeviceToken");
        assert!(text.contains("sandbox (development) token"), "{text}");
        let (text, _) = refused("push_failed", "apns sandbox topic com.x: topic not allowed on this relay");
        assert!(text.contains("LANGSTROTH_RELAY_APNS_TOPICS"), "{text}");
        let (text, _) = refused("push_failed", "apns sandbox topic com.x: 403 InvalidProviderToken");
        assert!(text.contains("LANGSTROTH_RELAY_APNS_KEY_ID"), "{text}");
        // No answer at all.
        let mut h = hop(true, None);
        h.sent = true;
        let (text, ok) = describe_push_test(&report(vec![h]));
        assert!(!ok && text.contains("no answer from the relay"), "{text}");
        // Relay down / no APNs.
        let mut r = report(vec![]);
        r.connected = false;
        assert!(describe_push_test(&r).0.contains("NOT connected"));
        let mut r = report(vec![hop(true, Some(RelayAck { ok: false, code: Some("push_unavailable".into()), detail: None }))]);
        r.push_available = false;
        let (text, ok) = describe_push_test(&r);
        assert!(!ok && text.contains("UNAVAILABLE"), "{text}");
    }
}

#[cfg(test)]
mod status_tests {
    use super::*;

    fn st(connected: bool) -> LiveStatus {
        LiveStatus { connected, status: "active".into(), push: true, ..Default::default() }
    }

    #[test]
    fn connected_shows_live_connection_age_not_last_change() {
        let now = 1_000_000;
        let mut s = st(true);
        s.at = now - 23_589;
        s.connected_since = Some(now - 23_589);
        s.heartbeat = now - 12;
        assert_eq!(describe_status(&s, now), "connected for 6h 33m (status active, push available, checked 12s ago)");
    }

    #[test]
    fn a_stopped_serve_is_not_reported_connected() {
        let now = 1_000_000;
        let mut s = st(true);
        s.at = now - 7200;
        s.connected_since = Some(now - 7200);
        s.heartbeat = now - 300;
        let line = describe_status(&s, now);
        assert!(line.starts_with("unknown: ligus serve last reported 5m ago (connected,"), "{line}");
        assert!(line.contains("is it running?"));
    }

    #[test]
    fn down_and_legacy_files_read_sensibly() {
        let now = 1_000_000;
        let mut down = st(false);
        down.push = false;
        down.at = now - 45;
        down.heartbeat = now - 5;
        down.last_error = Some("relay unreachable".into());
        assert_eq!(
            describe_status(&down, now),
            "not connected (status active, push unavailable, since 45s ago), last error: relay unreachable"
        );
        // Written by an older ligus serve: no heartbeat, no connection time.
        let mut legacy = st(true);
        legacy.at = now - 90_000;
        assert_eq!(describe_status(&legacy, now), "connected (status active, push available, last change 1d 1h ago)");
        assert_eq!(short_age(59), "59s");
        assert_eq!(short_age(3599), "59m");
    }
}
