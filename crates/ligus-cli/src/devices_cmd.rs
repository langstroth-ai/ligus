//! `ligus devices`: link this computer as one of your own devices ("Add a
//! computer"), instead of joining with an invite as a separate member. See
//! `ligus_runtime::computer_link` and `docs/design/link-a-computer.md`.
//!
//! * `request` (on the new computer): print its link request and fingerprint.
//! * `approve` (a person's headless identity; prefer the desktop): certify the
//!   request's device as yours with your recovery key and print the grant.
//! * `redeem` (on the new computer): check the grant, connect, pull, and act
//!   as the person from then on.
//! * `list`: your devices here, and where this one stands.

use std::path::Path;

use anyhow::{anyhow, Result};
use ligus_core::workspace_config_session_id;
use ligus_runtime::computer_link::{Grant, LinkRequest, LinkState, OpenedGrant};
use ligus_runtime::display::shown;
use ligus_runtime::secret_store::StoreKeyVault;
use ligus_runtime::IdentityStore;
use uuid::Uuid;

use crate::checks::{Check, Report};
use crate::problem::{Problem, EXIT_ERROR, EXIT_FIXABLE, EXIT_HUMAN, EXIT_OFFLINE, EXIT_REFUSED};
use crate::{bootstrap_identity, load_file_config, uuid_of_room, Config};

fn identity(cfg: &Config) -> IdentityStore<StoreKeyVault> {
    IdentityStore::new(&cfg.data_dir, StoreKeyVault::for_data_dir(&cfg.data_dir))
}

fn problem(exit: i32, code: &'static str, message: impl Into<String>, fix: Option<&str>) -> anyhow::Error {
    anyhow::Error::new(Problem { exit, code, message: message.into(), fix: fix.map(str::to_string) })
}

/// A name from a code, cut to what a prompt line can show.
fn clamp(s: &str) -> String {
    let mut out: String = s.chars().take(64).collect();
    if out.len() < s.len() {
        out.push('…');
    }
    out
}

/// Two fingerprints are the same whether typed with dashes, spaces (as a
/// copy from the desktop gives them) or in lower case.
pub fn same_fingerprint(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_uppercase();
    let (a, b) = (norm(a), norm(b));
    !a.is_empty() && a == b
}

fn now_ms() -> i64 {
    ligus_core::flow::now_ms()
}

fn link_state(cfg: &Config) -> Result<LinkState> {
    LinkState::load(&cfg.data_dir).map_err(|e| anyhow!("{} is unreadable ({e}): fix or remove it", LinkState::path(&cfg.data_dir).display()))
}

/// Refuse what a linked install never does: found a workspace, accept an
/// invite, become a box. `what` names the command.
pub fn refuse_if_linked(cfg: &Config, what: &str) -> Result<()> {
    if let Some(p) = link_state(cfg)?.person {
        return Err(problem(
            EXIT_REFUSED,
            "linked_device",
            format!("this computer is one of {}'s devices: it doesn't {what}", shown(&p.display_name)),
            Some("do it on the desktop that holds your account key, then add this computer there (My devices > Add a computer); or use another LIGUS_DATA_DIR"),
        ));
    }
    Ok(())
}

fn print(report: &Report, code: i32, json: bool, extra: serde_json::Value, text: &str) {
    if json {
        let mut v = report.to_json(code);
        if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
            o.insert("result".into(), serde_json::Value::Object(e.clone()));
        }
        println!("{v}");
    } else {
        print!("{text}");
    }
}

/// Copies a code to the clipboard: the tool's name, `None` when none worked
/// ([`crate::link_io::copy`]).
pub type Copier<'a> = &'a mut dyn FnMut(&str) -> Option<&'static str>;

/// `ligus devices request [--label NAME] [--new-device] [--copy]`: the code
/// alone on stdout (so `| pbcopy` copies just it); the fingerprint and the
/// next steps on stderr. `copy` copies it instead (`copier`: the clipboard
/// tool, injectable for tests).
pub fn cmd_request(
    cfg: &Config,
    label: Option<String>,
    new_device: bool,
    json: bool,
    copy: Option<Copier<'_>>,
) -> Result<()> {
    let ids = identity(cfg);
    if ids.load()?.is_some_and(|s| s.service) {
        return Err(problem(
            EXIT_REFUSED,
            "box",
            "this data dir is a box (a service member): a box joins as its own member",
            Some("on a box run `ligus admit request`; to link a person's computer use another LIGUS_DATA_DIR"),
        ));
    }
    let existed = ids.load()?.is_some();
    let stored = bootstrap_identity(&ids)?;
    let mut state = link_state(cfg)?;
    let ws = uuid_of_room(&cfg.room);
    if let Some(p) = state.linked_in(ws) {
        let mut r = Report::new("devices request");
        r.add(Check::ok("devices.linked", "linked", format!("this computer is already one of {}'s devices in '{}'", shown(&p.display_name), cfg.room)));
        if json {
            print(&r, 0, json, serde_json::json!({ "linked": true }), "");
        } else {
            eprintln!("This computer is already one of {}'s devices in '{}'.", shown(&p.display_name), cfg.room);
        }
        return Ok(());
    }
    // An install that has been connected (it may have written as its own
    // member) asks with a new device key, switched to only on redeem.
    let connected = load_file_config(&cfg.data_dir).room.is_some() || (existed && cfg.data_dir.join("ligus.db").exists());
    let fresh_key = new_device || connected;
    let now = now_ms();
    state.prune_expired(now);
    let (device_id, device_kp) = if fresh_key {
        match state.latest_pending(now).filter(|p| p.device_id != stored.device.id) {
            Some(p) => (p.device_id, ids.device_keypair(p.device_id)?),
            None => ids.prepare_device()?,
        }
    } else {
        (stored.device.id, ids.device_keypair(stored.device.id)?)
    };
    let ka = ids.key_agreement(device_id)?;
    let account_kp = ids.account_keypair(stored.account.id)?;
    let previous = fresh_key.then_some((stored.account.id, &account_kp));
    let host = ligus_runtime::machine_hostname().unwrap_or_else(|| "this computer".into());
    let label = label.map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).unwrap_or_else(|| format!("ligus on {host}"));
    let request = LinkRequest::new(&device_kp, &ka.public_key_bytes(), &label, now, previous);
    state.add_pending(&request, now);
    state.save(&cfg.data_dir)?;
    let code = request.encode();
    let fingerprint = request.fingerprint();
    let approve_by = request.created_at.saturating_add(ligus_runtime::computer_link::REQUEST_TTL_MS);
    let mut r = Report::new("devices request");
    r.add(Check::ok("devices.request", "request", format!("fingerprint {fingerprint}")));
    r.add(
        Check::warn("devices.approve", "approve", "a person approves it on their desktop with a passkey")
            .fix("desktop: Settings > Profile > My devices > Add a computer; paste the request, compare the fingerprint"),
    );
    r.next("ligus devices redeem");
    if json {
        let extra = serde_json::json!({
            "code": code, "request": code, "fingerprint": fingerprint, "label": request.label,
            "expiresAt": approve_by, "newDevice": fresh_key,
        });
        print(&r, 0, true, extra, "");
        return Ok(());
    }
    let asked = copy.is_some();
    match copy.and_then(|c| c(&code)) {
        Some(_) => eprintln!("Copied this computer's link request (it holds no secret)."),
        None => {
            if asked {
                eprintln!("ligus: couldn't reach the clipboard (pbcopy, wl-copy, xclip, xsel or clip.exe): the request is below");
            }
            println!("{code}");
        }
    }
    eprintln!(
        "Fingerprint: {fingerprint}  (label \"{}\")\n\
         On your desktop: Settings > Profile > My devices > Add a computer; paste the request, compare the fingerprint, confirm with your passkey (by {}).\n\
         Then here: ligus devices redeem   (it reads the desktop's code from the clipboard or a hidden paste){}",
        request.label,
        crate::link_io::hh_mm(approve_by),
        if fresh_key { "\nThis install was already connected, so it asks with a new device key (used only once you redeem)." } else { "" }
    );
    Ok(())
}

/// The workspace connection this CLI identity seals into a grant (no
/// invite).
fn connection(cfg: &Config, svc: &ligus_runtime::ChatService) -> Result<String> {
    let relay = cfg
        .relay_url
        .clone()
        .filter(|r| crate::usable_relay_url(r))
        .ok_or_else(|| problem(EXIT_FIXABLE, "no_relay", "this workspace has no relay URL", Some("set LIGUS_RELAY_URL or re-enroll")))?;
    let ws = uuid_of_room(&cfg.room);
    let pin = svc.store().workspace_pin(ws)?.ok_or_else(|| anyhow!("this identity has no founder pin for '{}'", cfg.room))?;
    let mut conn = ligus_runtime::workspace_invite::WorkspaceConn {
        name: cfg.room.clone(),
        relay_url: relay,
        room: cfg.room.clone(),
        key: cfg.key.clone(),
        icon: None,
        dm_account: None,
        founder_actor_id: Some(pin.founder_account.to_string()),
        fk: Some(pin.fk.iter().map(|b| format!("{b:02x}")).collect()),
        inv: None,
        ek: Vec::new(),
        sig: None,
    };
    if let Ok(src) = crate::key_source(cfg) {
        conn.ek = svc.delivered_epoch_keys(ws, &src).unwrap_or_default();
    }
    Ok(ligus_runtime::workspace_invite::encode_workspace_invite(&conn))
}

/// Asks the person at a terminal whether `what` matches; `Ok(true)` is yes.
pub type Confirm<'a> = &'a mut dyn FnMut(&str) -> Result<bool>;

/// `y`/`yes` typed at the terminal after printing `prompt` on stderr (from
/// the controlling terminal when stdin carried a code).
pub fn confirm_on_tty(prompt: &str) -> Result<bool> {
    crate::link_io::confirm(prompt)
}

/// Reads the desktop's code from the person at the terminal (a hidden
/// paste, or the clipboard): [`crate::link_io::read_pasted`].
pub type Paste<'a> = &'a mut dyn FnMut() -> Result<String>;

/// The paste reader at a real terminal.
pub fn paste_at_tty() -> Result<String> {
    crate::link_io::read_pasted(&mut crate::link_io::read_hidden, &mut crate::link_io::paste)
}

fn read_code(arg: Option<&str>, file: Option<String>, usage: &str) -> Result<String> {
    let code = match (arg, file) {
        (Some("-"), None) => {
            let mut s = String::new();
            let limit = ligus_runtime::computer_link::MAX_CODE_LEN as u64 + 1;
            std::io::Read::read_to_string(&mut std::io::Read::take(std::io::stdin(), limit), &mut s)?;
            s
        }
        (Some(c), None) => c.to_string(),
        (None, Some(path)) => crate::admit::read_code_file(Path::new(&path))?,
        _ => return Err(crate::help::usage_error(usage.to_string())),
    };
    Ok(code.trim().to_string())
}

/// `ligus devices approve`: a person's headless "Add a computer".
#[allow(clippy::too_many_arguments)]
pub async fn cmd_approve(
    cfg: &Config,
    code: Option<&str>,
    code_file: Option<String>,
    fingerprint: Option<String>,
    words: Option<String>,
    out: Option<String>,
    json: bool,
    confirm: Option<Confirm<'_>>,
) -> Result<()> {
    let usage = "usage: ligus devices approve <liguslink1:request> | --code-file PATH --fingerprint XXXX-XXXX-XXXX-XXXX-XXXX --recovery-words-file PATH [--out PATH]";
    let text = read_code(code, code_file, usage)?;
    let request = LinkRequest::decode(&text).map_err(|e| anyhow!(e))?;
    match (fingerprint, confirm) {
        (Some(f), _) if same_fingerprint(&f, &request.fingerprint()) => {}
        (Some(f), _) => {
            return Err(problem(
                EXIT_ERROR,
                "fingerprint_mismatch",
                format!("the request's fingerprint is {}, not {}: don't approve it", request.fingerprint(), f.trim()),
                Some("make a new request on the computer you are using and compare again"),
            ))
        }
        (None, Some(ask)) => {
            let prompt = format!("Computer '{}'\nFingerprint: {}\nDoes the computer's screen show this same fingerprint?", shown(&request.label), request.fingerprint());
            if !ask(&prompt)? {
                return Err(problem(EXIT_ERROR, "not_confirmed", "not approved: the fingerprint wasn't confirmed", None));
            }
        }
        (None, None) => {
            return Err(problem(
                EXIT_HUMAN,
                "fingerprint_needed",
                "a person compares the fingerprint with the computer's screen before approving",
                Some("pass --fingerprint XXXX-XXXX-XXXX-XXXX-XXXX as the computer shows it, or run this at a terminal"),
            ))
        }
    }
    let Some(words) = words else {
        return Err(problem(
            EXIT_HUMAN,
            "presence_needed",
            "adding a computer needs your passkey or recovery key",
            Some("use the desktop (Settings > Profile > My devices > Add a computer), or pass --recovery-words-file in your own terminal"),
        ));
    };
    if identity(cfg).load()?.is_some_and(|s| s.service) {
        return Err(problem(EXIT_REFUSED, "box", "this data dir is a box: it has no devices of its own to add", None));
    }
    let mut svc = crate::open_service(cfg)?;
    let key = crate::secret_arg::recovery_key_from_file(Path::new(&words))?;
    svc.set_presence_provider(Some(std::sync::Arc::new(crate::admit::RecoveryPresence(key))));
    let ws = uuid_of_room(&cfg.room);
    let conn = connection(cfg, &svc)?;
    let grant = svc.issue_computer_link(ws, &request, &conn, now_ms())?;
    drop(svc);
    // The certificate must reach the relay before the computer redeems.
    let pushed = crate::sync_once(cfg).await;
    let mut r = Report::new("devices approve");
    r.add(Check::ok("devices.certified", "certified", format!("'{}' ({}) is one of your devices", shown(&request.label), request.fingerprint())));
    match &pushed {
        Ok(_) => r.add(Check::ok("devices.synced", "synced", "the certificate is on the relay")),
        Err(e) => r.add(Check::warn("devices.synced", "synced", format!("not on the relay yet: {e}")).fix("run `ligus sync` before the computer redeems")),
    }
    r.next("on the computer: ligus devices redeem --code-file <grant-file>");
    let text = match &out {
        Some(path) => {
            crate::admit::write_out(path, grant.as_bytes())?;
            format!("Wrote the grant to {path}. Paste it on the computer within 30 minutes: `ligus devices redeem --code-file {path}`.\n")
        }
        None => format!("{grant}\n"),
    };
    let extra = match &out {
        Some(path) => serde_json::json!({ "grantFile": path }),
        None => serde_json::json!({ "grant": grant }),
    };
    print(&r, 0, json, extra, &text);
    Ok(())
}

/// Find the pending request `code` answers and open it with that request's
/// key-agreement key: (the grant, the device it names).
fn open_for_pending(cfg: &Config, state: &LinkState, code: &str) -> Result<(OpenedGrant, Uuid)> {
    let ids = identity(cfg);
    let mut last = None;
    for p in &state.pending {
        let Ok(ka) = ids.key_agreement(p.device_id) else { continue };
        match Grant::open(code, &ka) {
            Ok(g) => return Ok((g, p.device_id)),
            Err(e) => last = Some(e),
        }
    }
    Err(match last {
        Some(ligus_runtime::computer_link::LinkError::NotForThisDevice) | None => problem(
            EXIT_ERROR,
            "not_pending",
            "this grant doesn't answer a request made on this computer (or it was used already)",
            Some("run `ligus devices request` here and add the computer again on your desktop"),
        ),
        Some(e) => anyhow!(e),
    })
}

/// The desktop's code from what was given: the argument, `-` (stdin), a
/// file, or with none of them the person's paste or clipboard (`paste`, at
/// a terminal). Whatever surrounds the code (a whole copied command,
/// quotes, blank lines) is dropped; the text is never shown back.
fn read_grant(code: Option<&str>, code_file: Option<String>, paste: Option<Paste<'_>>, usage: &str) -> Result<String> {
    let (text, from) = match (code, code_file, paste) {
        (None, None, Some(p)) => return p(),
        (None, None, None) => {
            return Err(problem(
                EXIT_HUMAN,
                "code_needed",
                "redeem needs the code your desktop showed",
                Some("run this at a terminal (it reads the clipboard or a hidden paste), or pipe the code in: ligus devices redeem - (or --code-file PATH)"),
            ))
        }
        (Some("-"), None, _) => (read_code(Some("-"), None, usage)?, "the input"),
        (c, f, _) => (read_code(c, f, usage)?, "the code given"),
    };
    crate::link_io::extract_grant(&text).ok_or_else(|| anyhow!(crate::link_io::no_grant_message(&text, from)))
}

/// `ligus devices redeem`.
pub async fn cmd_redeem(
    cfg: &mut Config,
    code: Option<&str>,
    code_file: Option<String>,
    approver_fingerprint: Option<String>,
    json: bool,
    paste: Option<Paste<'_>>,
    confirm: Option<Confirm<'_>>,
) -> Result<()> {
    let usage = "usage: ligus devices redeem [<ligusgrant1:code> | - | --code-file PATH] [--approver-fingerprint XXXX-…]";
    let text = read_grant(code, code_file, paste, usage)?;
    let ids = identity(cfg);
    let Some(stored) = ids.load()? else {
        return Err(problem(EXIT_ERROR, "no_request", "this computer hasn't made a link request", Some("run `ligus devices request` first")));
    };
    if stored.service {
        return Err(problem(EXIT_REFUSED, "box", "this data dir is a box: a box joins as its own member (ligus admit)", None));
    }
    let mut state = link_state(cfg)?;
    if state.prune_expired(now_ms()) {
        state.save(&cfg.data_dir)?;
    }
    let (grant, device_id) = open_for_pending(cfg, &state, &text)?;
    let now = now_ms();
    if state.pending_for(&grant.request, now).is_none() {
        return Err(problem(EXIT_ERROR, "not_pending", "the request this grant answers expired or was used", Some("run `ligus devices request` again")));
    }
    let device_key = ids.device_keypair(device_id)?.public_key_bytes();
    let person = grant.check(&device_key, now).map_err(|e| anyhow!(e))?;
    if let Some(p) = &state.person {
        if p.account_id != person.account_id {
            return Err(problem(EXIT_REFUSED, "other_person", format!("this computer is already one of {}'s devices", shown(&p.display_name)), Some("use another LIGUS_DATA_DIR")));
        }
    }
    let saved = load_file_config(&cfg.data_dir);
    if saved.room.as_deref().is_some_and(|r| r != grant.conn.room) {
        return Err(problem(
            EXIT_REFUSED,
            "other_workspace",
            "this data dir is connected to another workspace",
            Some("use another LIGUS_DATA_DIR for this computer's link"),
        ));
    }
    // The person checks who approved it before anything is saved: a request
    // is public, so anyone could answer it from their own workspace.
    let shown_ws = clamp(&shown(if grant.conn.name.trim().is_empty() { &grant.conn.room } else { &grant.conn.name }));
    match (approver_fingerprint, confirm) {
        (Some(f), _) if same_fingerprint(&f, &grant.approver_fingerprint()) => {}
        (Some(f), _) => {
            return Err(problem(
                EXIT_ERROR,
                "approver_mismatch",
                format!("this grant was approved by device {}, not {}: don't use it", grant.approver_fingerprint(), f.trim()),
                Some("only redeem a grant your own desktop showed you"),
            ))
        }
        (None, Some(ask)) => {
            let prompt = format!(
                "Approved by {}'s desktop · fingerprint {} · for {shown_ws}. Is that your desktop?",
                clamp(&shown(&person.display_name)),
                grant.approver_fingerprint()
            );
            if !ask(&prompt)? {
                return Err(problem(EXIT_ERROR, "not_confirmed", "not linked: the approving desktop wasn't confirmed", None));
            }
        }
        (None, None) => {
            return Err(problem(
                EXIT_HUMAN,
                "confirm_approver",
                format!(
                    "a person confirms who approved this: {}'s desktop {} for workspace '{shown_ws}'",
                    clamp(&shown(&person.display_name)),
                    grant.approver_fingerprint()
                ),
                Some("pass --approver-fingerprint XXXX-XXXX-XXXX-XXXX-XXXX as your desktop shows it, or run this at a terminal"),
            ))
        }
    }
    let (founder, fk) = grant.conn.founder_pin().ok_or_else(|| anyhow!("the grant has no founder pin"))?;
    if !crate::usable_relay_url(&grant.conn.relay_url) && cfg.relay_url.as_deref().is_none_or(|r| !crate::usable_relay_url(r)) {
        return Err(problem(EXIT_FIXABLE, "no_relay", "the workspace has no relay this computer can use", Some("set LIGUS_RELAY_URL")));
    }
    // The device the request named becomes this install's. Until the link
    // is recorded nothing is written to the workspace; if it fails, the
    // install goes back to its old device, so the new key never signs as the
    // install's own member (it would become a bridge to the person).
    let old_device = stored.device.id;
    if device_id != old_device {
        ids.adopt_device(device_id)?;
    }
    let linked = finish_redeem(cfg, &grant, &mut state, founder, fk).await;
    let person = match linked {
        Ok(p) => p,
        Err(e) => {
            if device_id != old_device {
                ids.adopt_device(old_device)?;
            }
            return Err(e);
        }
    };
    let mut r = Report::new("devices redeem");
    r.add(Check::ok("devices.linked", "linked", format!("this computer is one of {}'s devices in '{}'", clamp(&shown(&person.display_name)), cfg.room)));
    r.next("ligus chats");
    let text = format!(
        "This computer is now one of {}'s devices in workspace '{shown_ws}' (approved by desktop {}).\n\
         It acts as them here; votes ask for their passkey, and account changes stay on the desktop.\n",
        clamp(&shown(&person.display_name)),
        grant.approver_fingerprint()
    );
    print(&r, 0, json, serde_json::json!({ "person": person.id, "workspace": cfg.room, "approver": grant.approver_fingerprint() }), &text);
    Ok(())
}

/// The writes of a redeem, once the person confirmed: the connection, the
/// pull, the check, the link record, the key-agreement key.
async fn finish_redeem(cfg: &mut Config, grant: &OpenedGrant, state: &mut LinkState, founder: String, fk: String) -> Result<ligus_core::ActorIdentity> {
    // The connection lives in memory until the link is recorded (a failed
    // redeem must not leave this data dir connected as its own
    // identity with the device key the desktop certified for the person).
    let before = cfg.clone();
    if crate::env_opt("LIGUS_RELAY_URL").is_none() {
        cfg.relay_url = Some(grant.conn.relay_url.trim().to_string());
    }
    cfg.room = grant.conn.room.clone();
    cfg.key = grant.conn.key.clone();
    cfg.epoch_keys = grant.conn.ek.clone();
    let ws = grant.workspace_id;
    let had_pin = {
        let store = ligus_runtime::EventStore::open(cfg.data_dir.join("ligus.db"))?;
        let had = store.workspace_pin(ws)?.is_some();
        store.set_workspace_pin(ws, &founder, &fk)?;
        had
    };
    let linked = async {
        crate::sync_once(cfg).await?;
        let mut svc = crate::open_service_as(cfg, true)?;
        svc.set_trusted_founder(Some(founder.clone()));
        let person = match svc.check_computer_link(grant, now_ms()) {
            Ok(p) => p,
            Err(e) if e.to_string().contains(ligus_runtime::computer_link::NOT_ON_RELAY_YET) => {
                return Err(problem(EXIT_OFFLINE, "not_yet", e.to_string(), Some("run this again in a few seconds (the grant stays valid for 30 minutes)")));
            }
            Err(e) => return Err(e.into()),
        };
        Ok((svc, person))
    }
    .await;
    let (mut svc, person) = match linked {
        Ok(v) => v,
        Err(e) => {
            if !had_pin {
                if let Ok(store) = ligus_runtime::EventStore::open(cfg.data_dir.join("ligus.db")) {
                    let _ = store.forget_workspace(ws);
                }
            }
            *cfg = before;
            return Err(e);
        }
    };
    state.record_link(&person, ws).map_err(|e| anyhow!(e))?;
    state.save(&cfg.data_dir)?;
    let mut f = load_file_config(&cfg.data_dir);
    f.relay_url = cfg.relay_url.clone();
    f.room = Some(cfg.room.clone());
    f.key = cfg.key.clone();
    f.ek = cfg.epoch_keys.clone();
    f.save(&cfg.data_dir)?;
    svc.act_as_owner_device(person.clone());
    crate::publish_key_agreement(cfg, &mut svc);
    drop(svc);
    let _ = crate::sync_once(cfg).await;
    Ok(person)
}

/// The link state with expired requests cleaned up (and saved).
pub fn pruned_link_state(cfg: &Config) -> Result<LinkState> {
    let mut state = link_state(cfg)?;
    if state.prune_expired(now_ms()) {
        state.save(&cfg.data_dir)?;
    }
    Ok(state)
}

/// `ligus devices list`.
pub fn cmd_list(cfg: &Config, json: bool) -> Result<()> {
    let state = pruned_link_state(cfg)?;
    let svc = crate::open_service(cfg)?;
    let ws = uuid_of_room(&cfg.room);
    let here = svc.device_id();
    let standing = svc.device_standing(ws, state.linked_in(ws).map(|p| p.id.as_str()))?;
    let me = match (&standing, state.linked_in(ws)) {
        (ligus_runtime::device_enroll::Standing::Revoked, _) => "revoked: this computer's device was revoked here".to_string(),
        (_, Some(p)) => format!("one of {}'s devices", shown(&p.display_name)),
        _ => "its own identity".to_string(),
    };
    let devices = svc.own_devices(ws)?;
    let mut seen = std::collections::HashSet::new();
    let rows: Vec<_> = devices.into_iter().filter(|d| seen.insert(d.device_id)).collect();
    let has_config = svc.load_in(ws, workspace_config_session_id(ws))?.is_some();
    if json {
        let mut r = Report::new("devices list");
        r.add(Check::ok("devices.this", "this computer", me.clone()));
        let extra = serde_json::json!({
            "thisDevice": here,
            "standing": me,
            "pending": state.pending.iter().map(|p| serde_json::json!({
                "deviceId": p.device_id, "createdAt": p.created_at,
                "approveBy": p.created_at.saturating_add(ligus_runtime::computer_link::REQUEST_TTL_MS),
                "expiresAt": p.created_at.saturating_add(LinkState::PENDING_TTL_MS),
            })).collect::<Vec<_>>(),
            "devices": rows.iter().map(|d| serde_json::json!({
                "deviceId": d.device_id, "fingerprint": d.fingerprint, "revoked": d.revoked, "thisDevice": d.device_id == here,
            })).collect::<Vec<_>>(),
        });
        print(&r, 0, true, extra, "");
        return Ok(());
    }
    println!("this computer: {me} (device {here})");
    if !has_config {
        println!("no copy of workspace '{}' here yet (ligus sync)", cfg.room);
    }
    for d in rows {
        println!(
            "  {}  {}{}{}",
            d.device_id,
            d.fingerprint,
            if d.device_id == here { "  this device" } else { "" },
            if d.revoked { "  revoked" } else { "" }
        );
    }
    if let Some(p) = state.pending_status(now_ms()) {
        println!("{}{}", crate::link_io::pending_line(&p, now_ms()), if p.count > 1 { format!(" ({} requests)", p.count) } else { String::new() });
    }
    Ok(())
}
