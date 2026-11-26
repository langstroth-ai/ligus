//! Code-match approval from the CLI (`docs/design/code-match-approval.md`
//! 7.4; bytes in `docs/design/code-match-wire.md`):
//!
//! * `ligus admit address [--reset]`: the workspace's approval address
//!   (`ligusaddr1:`), registering its mailbox. Not a secret.
//! * `ligus admit request --to ligusaddr1:…`: a box asks through the mailbox,
//!   shows the code, and joins only after checks 1 to 5 (the approval must be
//!   on the real workspace log, signed by an Admin's device with a passkey).
//! * `ligus devices request --to ligusaddr1:…`: a computer asks, confirms the
//!   code at its terminal, acknowledges the desktop's offer, and links after
//!   checks 1 to 6 ("Link this computer as Ana?").
//! * `ligus approvals list` and `ligus approvals approve <rid> --code …`: the
//!   headless approver (a person's own CLI identity, like `ligus admit issue`).
//!
//! The paste path (`ligus admit request`, `ligus devices request` without
//! `--to`) is unchanged, and `--to` prints its text straight away, so an older
//! desktop can approve without the mailbox.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use ligus_core::match_code::MatchKind;
use ligus_core::{workspace_config_session_id, WorkspaceRole};
use ligus_runtime::approval_mailbox::LogApprovalCheck as _;
use ligus_runtime::approval_mailbox::{
    self as am, AbortReason, ApprovalAddress, ApproverError, ApproverIdentity, ApproverPerson,
    ApproverSession, CheckedGrant, GrantTarget, LogVerdict, MailboxClient, MailboxError, Notice,
    NoticeKind, Notices, Outcome, RequestSpec, RequestSubject, Requester, RequesterError,
    RequesterEvent, SeenSet, Signer,
};
use ligus_runtime::display::shown;
use ligus_runtime::secret_store::StoreKeyVault;
use ligus_runtime::IdentityStore;
use uuid::Uuid;

use crate::checks::{Check, Report};
use crate::problem::{
    Problem, EXIT_ERROR, EXIT_FIXABLE, EXIT_HUMAN, EXIT_OFFLINE, EXIT_REFUSED, EXIT_USAGE,
};
use crate::{uuid_of_room, Config};

/// How long `--to` waits for an approver by default.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(15 * 60);
/// How long a requester retries check 5 while the approval syncs.
const LOG_RETRY: Duration = Duration::from_millis(am::LOG_CHECK_RETRY_MS as u64);

/// Asks the person at a terminal; `Ok(true)` is yes.
pub type Ask<'a> = &'a mut (dyn FnMut(&str) -> Result<bool> + Send);

fn problem(
    exit: i32,
    code: &'static str,
    message: impl Into<String>,
    fix: Option<&str>,
) -> anyhow::Error {
    anyhow::Error::new(Problem {
        exit,
        code,
        message: message.into(),
        fix: fix.map(str::to_string),
    })
}

fn identity(cfg: &Config) -> IdentityStore<StoreKeyVault> {
    IdentityStore::new(&cfg.data_dir, StoreKeyVault::for_data_dir(&cfg.data_dir))
}

fn now_ms() -> i64 {
    ligus_core::flow::now_ms()
}

/// A mailbox error as the shared exit table reads it. `paste` is the paste
/// command a relay without the routes falls back to.
fn mailbox_problem(e: MailboxError, paste: &str) -> anyhow::Error {
    let fix_paste =
        format!("this relay has no approval mailbox: use the paste path instead ({paste})");
    match e {
        MailboxError::NoRoutes => problem(
            EXIT_FIXABLE,
            "no_mailbox_routes",
            e.to_string(),
            Some(&fix_paste),
        ),
        MailboxError::Unreachable(_) | MailboxError::RateLimited(_) => problem(
            EXIT_OFFLINE,
            "relay_unreachable",
            e.to_string(),
            Some("check the network and the relay, then retry"),
        ),
        MailboxError::BadCap | MailboxError::MailboxUnknown => problem(
            EXIT_HUMAN,
            "address_stale",
            e.to_string(),
            Some("ask an Admin for the current approval address (ligus admit address)"),
        ),
        MailboxError::Unauthorized => problem(
            EXIT_REFUSED,
            "refused",
            e.to_string(),
            Some("check the relay access token"),
        ),
        MailboxError::MailboxFull => problem(
            EXIT_OFFLINE,
            "mailbox_full",
            e.to_string(),
            Some(&fix_paste),
        ),
        MailboxError::RequestUnknown => problem(
            EXIT_ERROR,
            "request_gone",
            e.to_string(),
            Some("start again"),
        ),
        other => problem(EXIT_ERROR, "mailbox", other.to_string(), None),
    }
}

fn requester_problem(e: RequesterError, paste: &str) -> anyhow::Error {
    match e {
        RequesterError::Mailbox(m) => mailbox_problem(m, paste),
        RequesterError::TooManyDead => problem(
            EXIT_ERROR,
            "too_many_dead",
            e.to_string(),
            Some("check the device and the network, then start again"),
        ),
        RequesterError::BadSpec(_) => problem(EXIT_USAGE, "bad_request", e.to_string(), None),
        RequesterError::Io(_) => problem(EXIT_ERROR, "io", e.to_string(), None),
    }
}

fn approver_problem(e: ApproverError) -> anyhow::Error {
    match e {
        ApproverError::Mailbox(m) => mailbox_problem(
            m,
            "ligus admit issue --request … or ligus devices approve …",
        ),
        ApproverError::DidntFinish => problem(
            EXIT_ERROR,
            "didnt_finish",
            e.to_string(),
            Some("start again from the computer"),
        ),
        other => problem(EXIT_ERROR, "approval", other.to_string(), None),
    }
}

/// Parse `--to`. An `http://` relay is taken for a loopback host, or with
/// `--insecure-relay` (tests); anything else needs `https://`.
pub fn parse_address(text: &str, insecure: bool) -> Result<ApprovalAddress> {
    let a = ApprovalAddress::decode_with(text, true)
        .map_err(|e| problem(EXIT_USAGE, "bad_address", e.to_string(), None))?;
    let url = reqwest::Url::parse(&a.relay).map_err(|_| {
        problem(
            EXIT_USAGE,
            "bad_address",
            "the approval address is damaged",
            None,
        )
    })?;
    let host = url
        .host_str()
        .unwrap_or_default()
        .trim_start_matches('[')
        .trim_end_matches(']');
    let loopback = host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    if url.scheme() == "http" && !loopback && !insecure {
        return Err(problem(
            EXIT_USAGE,
            "insecure_relay",
            "the approval address's relay isn't an https:// address",
            Some("ask for the address of an https relay"),
        ));
    }
    Ok(a)
}

fn client_for(cfg: &Config, relay: &str) -> MailboxClient {
    let c = MailboxClient::new(relay).with_auth(cfg.token.clone());
    #[cfg(test)]
    {
        let dir = cfg.data_dir.clone();
        c.with_clock(std::sync::Arc::new(move || {
            ligus_core::flow::now_ms() + CLOCK_SKEW.lock().unwrap().get(&dir).copied().unwrap_or(0)
        }))
    }
    #[cfg(not(test))]
    c
}

/// Tests: a data dir's mailbox clock runs this many ms ahead.
#[cfg(test)]
pub(crate) static CLOCK_SKEW: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, i64>>,
> = std::sync::LazyLock::new(Default::default);

/// Whether this process runs with an operator's relay override.
fn relay_override() -> bool {
    crate::env_opt("LIGUS_RELAY_URL").is_some()
}

/// Everything these commands print, kept for the tests' secret scan.
#[cfg(test)]
pub(crate) static PRINTED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn keep(_text: &str) {
    #[cfg(test)]
    PRINTED.lock().unwrap().push(_text.to_string());
}

/// A line on stderr (progress, the code).
fn note(text: &str) {
    keep(text);
    eprintln!("{text}");
}

/// Write a line for the person: stderr with `--json` (stdout carries the
/// one document), stdout otherwise.
fn say(json: bool, text: &str) {
    keep(text);
    if json {
        eprintln!("{text}");
    } else {
        println!("{text}");
    }
}

fn print_report(r: &Report, json: bool, result: serde_json::Value, text: &str) {
    if json {
        let mut v = r.to_json(0);
        if let Some(o) = v.as_object_mut() {
            o.insert("result".into(), result);
        }
        keep(&v.to_string());
        println!("{v}");
    } else if !text.is_empty() {
        keep(text);
        print!("{text}");
    }
}

// ---- The address ----

/// The workspace keys, founder pin and relay this member uses for the mailbox.
fn member_context(
    cfg: &Config,
) -> Result<(
    ligus_runtime::ChatService,
    Uuid,
    String,
    Vec<am::EpochKey>,
    MailboxClient,
)> {
    let svc = crate::open_service(cfg)?;
    let ws = uuid_of_room(&cfg.room);
    let relay = cfg
        .relay_url
        .clone()
        .filter(|r| crate::usable_relay_url(r))
        .ok_or_else(|| {
            problem(
                EXIT_FIXABLE,
                "no_relay",
                "this workspace has no relay URL",
                Some("set LIGUS_RELAY_URL (ligus help env)"),
            )
        })?;
    let pin = svc.founder_pin_hex(ws)?.ok_or_else(|| {
        problem(
            EXIT_FIXABLE,
            "no_founder_pin",
            format!("this identity has no founder pin for '{}'", cfg.room),
            Some("join the workspace from an invite first"),
        )
    })?;
    let src = crate::key_source(cfg)?;
    let epochs = svc.approval_epoch_keys(ws, &src)?;
    if epochs.is_empty() {
        return Err(problem(
            EXIT_FIXABLE,
            "no_key",
            "this device holds no workspace key here",
            Some("sync first (ligus sync)"),
        ));
    }
    Ok((svc, ws, pin, epochs, client_for(cfg, &relay)))
}

/// `ligus admit address [--reset] [--json]`.
pub async fn cmd_address(cfg: &Config, reset: bool, json: bool) -> Result<()> {
    let (svc, ws, pin, epochs, client) = member_context(cfg)?;
    let paste = "ligus admit request (paste the request into the desktop)";
    if reset {
        let role = svc
            .load_in(ws, workspace_config_session_id(ws))?
            .map(|c| c.role_of(&svc.author().id))
            .unwrap_or(WorkspaceRole::Viewer);
        if role.rank() < WorkspaceRole::Admin.rank() {
            return Err(problem(
                EXIT_REFUSED,
                "not_admin",
                "only an Admin resets the approval address",
                Some("ask an Admin"),
            ));
        }
        let newest = epochs.iter().max_by_key(|e| e.epoch).expect("not empty");
        client
            .reset_address(&newest.key)
            .await
            .map_err(|e| mailbox_problem(e, paste))?;
    }
    let address = am::current_address(&client, ws, &pin, &epochs)
        .await
        .map_err(|e| mailbox_problem(e, paste))?;
    let text = address.encode();
    let mut r = Report::new("admit address");
    r.add(Check::ok(
        "approval.address",
        "approval address",
        format!("epoch {}, relay {}", address.epoch, address.relay),
    ));
    if reset {
        r.add(Check::ok(
            "approval.reset",
            "reset",
            "the previous address no longer takes requests",
        ));
    }
    r.next(format!("on the box: ligus admit request --to {text}"));
    print_report(
        &r,
        json,
        serde_json::json!({ "address": text, "relay": address.relay, "epoch": address.epoch, "mailbox": address.mailbox_hex(), "reset": reset }),
        &format!("{text}\n"),
    );
    Ok(())
}

// ---- The box requester ----

/// What `--to` waits with.
pub struct ToOptions {
    pub to: String,
    pub label: String,
    pub wait: Duration,
    pub insecure: bool,
    pub json: bool,
    /// How long check 5 is retried while the approval syncs.
    pub log_retry: Duration,
    /// Hears the code as it is printed (an embedding UI, tests).
    pub code_tx: Option<std::sync::mpsc::Sender<String>>,
}

impl ToOptions {
    pub fn new(to: String, label: String, wait: Duration, insecure: bool, json: bool) -> Self {
        Self {
            to,
            label,
            wait,
            insecure,
            json,
            log_retry: LOG_RETRY,
            code_tx: None,
        }
    }
}

/// `ligus admit request --to ligusaddr1:…`: the box's side.
pub async fn cmd_admit_request_to(
    cfg: &Config,
    o: ToOptions,
    mut ask: Option<Ask<'_>>,
) -> Result<()> {
    let address = parse_address(&o.to, o.insecure)?;
    let join = crate::admit::join_request(cfg, &o.label)?;
    let paste_text = join.encode();
    let paste = "ligus admit request, then paste the request into the Admin's desktop (workspace menu > Admit a box)";
    say(
        o.json,
        &format!(
            "Join request (an older desktop can paste it: workspace menu > Admit a box):\n\n  {paste_text}\n\nFingerprint: {}  (label \"{}\")\n",
            join.fingerprint(),
            shown(&join.label)
        ),
    );
    let ids = identity(cfg);
    let device = ids.device_keypair(join.device_id)?;
    let ka = ids.key_agreement(join.device_id)?;
    let spec = RequestSpec {
        kind: MatchKind::Box,
        code: paste_text.clone(),
        signer: Signer::Ed25519(device),
        ka: Some(ids.key_agreement(join.device_id)?),
    };
    let client = client_for(cfg, &address.relay);
    let mut req = Requester::post(client, &address, spec, Some(&cfg.data_dir))
        .await
        .map_err(|e| requester_problem(e, paste))?;
    req.allow_relay_override(relay_override());
    let mut r = Report::new("admit request");
    r.add(Check::ok(
        "approval.posted",
        "posted",
        format!(
            "request {} in the workspace's approval mailbox",
            req.rid_hex()
        ),
    ));
    note("Waiting for an approver…");
    let outcome = wait_box(cfg, &mut req, &ka, &join, &o, &mut ask, &mut r).await;
    let _ = req.settle().await;
    let (room, approver) = outcome?;
    r.next("ligus serve  (or ligus worker): the box joins as its own member");
    print_report(
        &r,
        o.json,
        serde_json::json!({ "rid": req.rid_hex(), "fingerprint": join.fingerprint(), "request": paste_text, "workspace": room, "approver": approver }),
        &format!("Admitted by {approver}. Kept the admission for workspace '{room}': start `ligus serve` or `ligus worker` and it joins as its own member.\n"),
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn wait_box(
    cfg: &Config,
    req: &mut Requester,
    ka: &ligus_core::KeyAgreementKeypair,
    join: &ligus_runtime::box_admission::JoinRequest,
    o: &ToOptions,
    ask: &mut Option<Ask<'_>>,
    r: &mut Report,
) -> Result<(String, String)> {
    let deadline = Instant::now() + o.wait;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            let _ = req.abort(AbortReason::Cancelled).await;
            return Err(problem(
                EXIT_HUMAN,
                "no_approver",
                "no approver answered within --wait",
                Some("ask an Admin to approve it in their Inbox, or paste the join request printed above (workspace menu > Admit a box)"),
            ));
        }
        let events = match req.poll(left.as_secs().min(am::MAX_WAIT_SECS)).await {
            Ok(e) => e,
            Err(RequesterError::Mailbox(m)) if m.retryable() => {
                tokio::time::sleep(Duration::from_secs(3).min(left)).await;
                continue;
            }
            Err(e) => return Err(requester_problem(e, "ligus admit request")),
        };
        for ev in events {
            match ev {
                RequesterEvent::Opened { code, claimed } => {
                    note(&format!(
                        "Code: {}  (the approver's screen must show the same)",
                        code.shown
                    ));
                    note(&format!(
                        "Says it's {}'s device {} (checked after approval)",
                        shown(&claimed.name),
                        claimed.fingerprint
                    ));
                    r.add(Check::ok("match-code", "match code", code.shown.clone()));
                    if let Some(tx) = &o.code_tx {
                        let _ = tx.send(code.shown.clone());
                    }
                    if let Some(ask) = ask.as_mut() {
                        if !ask("Does the approver's screen show this code?")? {
                            let _ = req.abort(AbortReason::Mismatch).await;
                            return Err(problem(
                                EXIT_ERROR,
                                "mismatch",
                                "the codes don't match: nothing was granted",
                                Some(MISMATCH_FIX),
                            ));
                        }
                    }
                }
                RequesterEvent::Result(res) if res.outcome == Outcome::Approved => {
                    let checked = req
                        .check_grant(&res, GrantTarget::Box { ka }, now_ms())
                        .map_err(|e| {
                            problem(
                                EXIT_ERROR,
                                "grant_refused",
                                e.to_string(),
                                Some("start again"),
                            )
                        })?;
                    r.add(Check::ok(
                        "approval.grant",
                        "grant",
                        "sealed to this box, for the address's workspace and founder",
                    ));
                    let name = box_check5(cfg, req, &checked, join, o.log_retry).await?;
                    r.add(Check::ok(
                        "approval.log",
                        "on the workspace log",
                        format!("approved by {name}'s device with a passkey"),
                    ));
                    let grant = res.grant.as_deref().unwrap_or_default();
                    let mut cfg = cfg.clone();
                    let kept = crate::admit::redeem(&mut cfg, grant)?;
                    r.add(Check::ok(
                        "admit.kept",
                        "admission kept",
                        format!("workspace '{}'", kept.room),
                    ));
                    return Ok((kept.room, name));
                }
                RequesterEvent::Result(res) => return Err(ended(res.outcome)),
                RequesterEvent::Aborted { reason, .. } => return Err(aborted(reason)),
                RequesterEvent::CodeExpired | RequesterEvent::RequestExpired => {
                    return Err(problem(
                        EXIT_ERROR,
                        "expired",
                        "the request expired before it was approved",
                        Some("start again"),
                    ))
                }
                RequesterEvent::Offered { .. } => {}
            }
        }
    }
}

const MISMATCH_FIX: &str = "check the device you're adding, start again on a trusted network, and tell an Admin if it happens twice";

fn ended(outcome: Outcome) -> anyhow::Error {
    match outcome {
        Outcome::Mismatch => problem(
            EXIT_ERROR,
            "mismatch",
            "the approver said the codes don't match: nothing was granted",
            Some(MISMATCH_FIX),
        ),
        Outcome::Declined => problem(
            EXIT_ERROR,
            "declined",
            "the approver declined the request",
            None,
        ),
        _ => problem(
            EXIT_ERROR,
            "expired",
            "the code expired before it was approved",
            Some("start again"),
        ),
    }
}

fn aborted(reason: AbortReason) -> anyhow::Error {
    match reason {
        AbortReason::Mismatch => problem(
            EXIT_ERROR,
            "mismatch",
            "the request was ended: the codes didn't match (reported, not verified)",
            Some(MISMATCH_FIX),
        ),
        AbortReason::Contested => problem(
            EXIT_ERROR,
            "contested",
            "an Admin ended the request: a device that can't approve it opened it",
            Some(MISMATCH_FIX),
        ),
        AbortReason::Cancelled => problem(
            EXIT_ERROR,
            "cancelled",
            "the request was cancelled",
            Some("start again"),
        ),
    }
}

/// Check 5 for a box: pull the workspace the admission names (with its
/// founder pin, in memory: nothing is kept yet) and find the Admin's
/// admission on the log, signed by the device that showed the code. Retries
/// for [`LOG_RETRY`] while it syncs. Returns the verified approver's name.
async fn box_check5(
    cfg: &Config,
    req: &Requester,
    checked: &CheckedGrant,
    join: &ligus_runtime::box_admission::JoinRequest,
    budget: Duration,
) -> Result<String> {
    let CheckedGrant::Box { conn, .. } = checked else {
        return Err(anyhow!("not a box admission"));
    };
    let mut probe = cfg.clone();
    // Check 5 reads the log on the approval address's relay (check 3 made
    // the grant name the same one), unless the operator overrides it.
    if !relay_override() {
        probe.relay_url = Some(req.address().relay.clone());
    }
    probe.room = conn.room.clone();
    if probe.key.is_none() {
        probe.key = conn.key.clone();
    }
    for k in &conn.ek {
        if !probe.epoch_keys.iter().any(|h| h.v == k.v) {
            probe.epoch_keys.push(k.clone());
        }
    }
    let ws = conn.id();
    let (founder, fk) = conn
        .founder_pin()
        .ok_or_else(|| anyhow!("the admission has no founder pin"))?;
    let had_pin = {
        let store = ligus_runtime::EventStore::open(probe.data_dir.join("ligus.db"))?;
        let had = store.workspace_pin(ws)?.is_some();
        store.set_workspace_pin(ws, &founder, &fk)?;
        had
    };
    let device_key = join.device_public_key.clone();
    let verdict = am::retry_log_check(
        || {
            let probe = probe.clone();
            let device_key = device_key.clone();
            async move {
                if let Err(e) = crate::sync_once(&probe).await {
                    return LogVerdict::NotYet(format!("sync: {e}"));
                }
                let svc = match crate::open_service(&probe) {
                    Ok(s) => s,
                    Err(e) => return LogVerdict::NotYet(e.to_string()),
                };
                let Some(la) = req.log_approval(checked, &device_key, now_ms()) else {
                    return LogVerdict::Refused("no open".into());
                };
                svc.check_approval_on_log(&la)
            }
        },
        budget,
        Duration::from_secs(1),
    )
    .await;
    match verdict {
        LogVerdict::Verified { name, .. } => Ok(name),
        other => {
            if !had_pin {
                if let Ok(store) = ligus_runtime::EventStore::open(probe.data_dir.join("ligus.db"))
                {
                    let _ = store.forget_workspace(ws);
                }
            }
            let why = match other {
                LogVerdict::Refused(w) | LogVerdict::NotYet(w) => w,
                LogVerdict::Verified { .. } => unreachable!(),
            };
            Err(problem(
                EXIT_ERROR,
                "not_on_log",
                format!(
                    "the approval wasn't found on the workspace log ({why}): nothing was redeemed"
                ),
                Some("ask the Admin to check their desktop is online and synced, then start again"),
            ))
        }
    }
}

// ---- The computer requester ----

/// `ligus devices request --to ligusaddr1:…`: the computer's side. A person
/// confirms the code and, at the end, who they link as: without a terminal
/// it exits 4.
pub async fn cmd_devices_request_to(
    cfg: &Config,
    o: ToOptions,
    new_device: bool,
    ask: Option<Ask<'_>>,
) -> Result<()> {
    let address = parse_address(&o.to, o.insecure)?;
    let ids = identity(cfg);
    if ids.load()?.is_some_and(|s| s.service) {
        return Err(problem(
            EXIT_REFUSED,
            "box",
            "this data dir is a box: a box joins with `ligus admit request --to`",
            None,
        ));
    }
    let Some(ask) = ask else {
        return Err(problem(
            EXIT_HUMAN,
            "terminal_needed",
            "a computer is linked by a person at it: run this in a terminal to confirm the code and who you link as",
            Some("run `ligus devices request --to …` in a terminal, or use the paste path (ligus devices request, then ligus devices redeem)"),
        ));
    };
    let label = (!o.label.trim().is_empty()).then(|| o.label.clone());
    let (request, device_id, _) = crate::devices_cmd::make_link_request(cfg, label, new_device)?;
    let paste_text = request.encode();
    let paste = "ligus devices request, then paste the request into your desktop (My devices > Add a computer)";
    say(
        o.json,
        &format!(
            "Link request (an older desktop can paste it: Settings > Profile > My devices > Add a computer):\n\n  {paste_text}\n\nFingerprint: {}  (label \"{}\")\n",
            request.fingerprint(),
            shown(&request.label)
        ),
    );
    let stored = ids.load()?.ok_or_else(|| anyhow!("no identity"))?;
    let ka = ids.key_agreement(device_id)?;
    let device_key = ids.device_keypair(device_id)?.public_key_bytes();
    let spec = RequestSpec {
        kind: MatchKind::Computer,
        code: paste_text.clone(),
        signer: Signer::Ed25519(ids.device_keypair(device_id)?),
        ka: Some(ids.key_agreement(device_id)?),
    };
    let client = client_for(cfg, &address.relay);
    let mut req = Requester::post(client, &address, spec, Some(&cfg.data_dir))
        .await
        .map_err(|e| requester_problem(e, paste))?;
    req.allow_relay_override(relay_override());
    let mut r = Report::new("devices request");
    r.add(Check::ok(
        "approval.posted",
        "posted",
        format!(
            "request {} in the workspace's approval mailbox",
            req.rid_hex()
        ),
    ));
    note("Waiting for an approver…");
    let mut cfg = cfg.clone();
    let outcome = wait_computer(
        &mut cfg,
        &mut req,
        &request,
        &ka,
        &device_key,
        device_id,
        stored.device.id,
        &o,
        ask,
        &mut r,
    )
    .await;
    let _ = req.settle().await;
    let person = outcome?;
    r.next("ligus chats");
    print_report(
        &r,
        o.json,
        serde_json::json!({ "rid": req.rid_hex(), "fingerprint": request.fingerprint(), "request": paste_text, "person": person.id, "workspace": cfg.room }),
        &format!(
            "This computer is now one of {}'s devices in workspace '{}'.\n",
            shown(&person.display_name),
            cfg.room
        ),
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn wait_computer(
    cfg: &mut Config,
    req: &mut Requester,
    request: &ligus_runtime::computer_link::LinkRequest,
    ka: &ligus_core::KeyAgreementKeypair,
    device_key: &[u8],
    device_id: Uuid,
    old_device: Uuid,
    o: &ToOptions,
    ask: Ask<'_>,
    r: &mut Report,
) -> Result<ligus_core::ActorIdentity> {
    let deadline = Instant::now() + o.wait;
    let mut confirmed = false;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            let _ = req.abort(AbortReason::Cancelled).await;
            return Err(problem(
                EXIT_HUMAN,
                "no_approver",
                "no approver answered within --wait",
                Some("approve it on your desktop (Inbox), or paste the link request printed above (My devices > Add a computer)"),
            ));
        }
        let events = match req.poll(left.as_secs().min(am::MAX_WAIT_SECS)).await {
            Ok(e) => e,
            Err(RequesterError::Mailbox(m)) if m.retryable() => {
                tokio::time::sleep(Duration::from_secs(3).min(left)).await;
                continue;
            }
            Err(e) => return Err(requester_problem(e, "ligus devices request")),
        };
        for ev in events {
            match ev {
                RequesterEvent::Opened { code, claimed } => {
                    note(&format!(
                        "Code: {}  (the approver's screen must show the same)",
                        code.shown
                    ));
                    note(&format!(
                        "Says it's {}'s device {} (checked after approval)",
                        shown(&claimed.name),
                        claimed.fingerprint
                    ));
                    r.add(Check::ok("match-code", "match code", code.shown.clone()));
                    if let Some(tx) = &o.code_tx {
                        let _ = tx.send(code.shown.clone());
                    }
                    if !ask("Does your desktop show this code?")? {
                        let _ = req.abort(AbortReason::Mismatch).await;
                        return Err(problem(
                            EXIT_ERROR,
                            "mismatch",
                            "the codes don't match: nothing was granted",
                            Some(MISMATCH_FIX),
                        ));
                    }
                    confirmed = true;
                }
                RequesterEvent::Offered { .. } if confirmed => {
                    req.ack()
                        .await
                        .map_err(|e| requester_problem(e, "ligus devices request"))?;
                }
                RequesterEvent::Offered { .. } => {}
                RequesterEvent::Result(res) if res.outcome == Outcome::Approved => {
                    let checked = req
                        .check_grant(
                            &res,
                            GrantTarget::Computer {
                                request,
                                ka,
                                device_key,
                            },
                            now_ms(),
                        )
                        .map_err(|e| {
                            problem(
                                EXIT_ERROR,
                                "grant_refused",
                                e.to_string(),
                                Some("start again"),
                            )
                        })?;
                    let CheckedGrant::Computer { grant, .. } = checked else {
                        return Err(anyhow!("not a computer grant"));
                    };
                    r.add(Check::ok("approval.grant", "grant", "from the device that showed the code, for the address's workspace and founder"));
                    let person =
                        link_after_check5(cfg, &grant, device_id, old_device, ask, o.log_retry)
                            .await?;
                    r.add(Check::ok(
                        "approval.log",
                        "on the workspace log",
                        format!("certified as {}'s device", shown(&person.display_name)),
                    ));
                    return Ok(person);
                }
                RequesterEvent::Result(res) => return Err(ended(res.outcome)),
                RequesterEvent::Aborted { reason, .. } => return Err(aborted(reason)),
                RequesterEvent::CodeExpired | RequesterEvent::RequestExpired => {
                    return Err(problem(
                        EXIT_ERROR,
                        "expired",
                        "the request expired before it was approved",
                        Some("start again"),
                    ))
                }
            }
        }
    }
}

/// Check 5 (the certificate on the log, retried while it syncs) and check 6
/// (the person confirms who they link as, verified), then the link.
async fn link_after_check5(
    cfg: &mut Config,
    grant: &ligus_runtime::computer_link::OpenedGrant,
    device_id: Uuid,
    old_device: Uuid,
    ask: Ask<'_>,
    budget: Duration,
) -> Result<ligus_core::ActorIdentity> {
    let start = Instant::now();
    loop {
        let mut state = ligus_runtime::computer_link::LinkState::load(&cfg.data_dir)?;
        let mut confirm = |p: &ligus_core::ActorIdentity| {
            ask(&format!(
                "Link this computer as {} (verified on the workspace log)?",
                shown(&p.display_name)
            ))
        };
        match crate::devices_cmd::link_with_grant(
            cfg,
            grant,
            device_id,
            old_device,
            &mut state,
            Some(&mut confirm),
        )
        .await
        {
            Ok(p) => return Ok(p),
            Err(e)
                if e.downcast_ref::<Problem>()
                    .is_some_and(|p| p.code == "not_yet")
                    && start.elapsed() < budget =>
            {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(e)
                if e.downcast_ref::<Problem>()
                    .is_some_and(|p| p.code == "not_yet") =>
            {
                return Err(problem(
                    EXIT_ERROR,
                    "not_on_log",
                    "the approval wasn't found on the workspace log: nothing was linked",
                    Some("make sure your desktop is online and synced, then start again"),
                ))
            }
            Err(e) => return Err(e),
        }
    }
}

// ---- The headless approver ----

/// A refusal of [`ApproverSession::ready_to_commit`]: nothing was written.
fn commit_problem(e: ApproverError) -> anyhow::Error {
    match e {
        ApproverError::CodeExpired => problem(
            EXIT_ERROR,
            "expired",
            "the code expired (5 minutes after it appeared): nothing was written",
            Some("start again from the requesting device"),
        ),
        ApproverError::Aborted(r) => problem(
            EXIT_ERROR,
            "ended",
            format!("the request was ended ({r:?}): nothing was written"),
            None,
        ),
        other => approver_problem(other),
    }
}

/// `ligus approvals list [--json]`.
pub async fn cmd_list(cfg: &Config, json: bool) -> Result<()> {
    let (svc, ws, _, epochs, client) = member_context(cfg)?;
    let paste = "paste the request instead";
    let mut listing = am::list_live(&client, &epochs)
        .await
        .map_err(|e| mailbox_problem(e, paste))?;
    if !listing.unregistered.is_empty() {
        // The relay forgets mailboxes on a restart: register them again.
        for e in epochs
            .iter()
            .filter(|e| listing.unregistered.contains(&e.epoch))
        {
            client
                .ensure_registered(&e.key)
                .await
                .map_err(|e| mailbox_problem(e, paste))?;
        }
        listing = am::list_live(&client, &epochs)
            .await
            .map_err(|e| mailbox_problem(e, paste))?;
    }
    let classes = svc.approval_classifier(ws)?;
    let me = svc.device_id();
    let rows: Vec<serde_json::Value> = listing
        .live
        .iter()
        .filter(|l| l.ended.is_none())
        .map(|l| {
            let view = match am::claim_view(l, me, &classes) {
                am::ClaimView::Unclaimed => serde_json::json!({ "state": "unclaimed" }),
                am::ClaimView::Mine => serde_json::json!({ "state": "mine" }),
                am::ClaimView::ClaimedBy { name, device_id } => serde_json::json!({ "state": "claimed", "by": name, "deviceId": device_id }),
                am::ClaimView::Contested { by } => {
                    serde_json::json!({ "state": "contested", "by": by.as_ref().map(|b| b.0.clone()), "deviceId": by.map(|b| b.1) })
                }
            };
            serde_json::json!({
                "rid": l.rid_hex(),
                "kind": l.kind().as_str(),
                "label": l.label(),
                "fingerprint": l.fingerprint(),
                "createdAt": l.created_at(),
                "putAt": l.put_at,
                "expiresAt": l.expires_at,
                "epoch": l.epoch,
                "relay": l.relay,
                "claim": view,
            })
        })
        .collect();
    let mut r = Report::new("approvals list");
    r.add(Check::ok(
        "approvals.listed",
        "listed",
        format!("{} live request(s)", rows.len()),
    ));
    if !rows.is_empty() {
        r.next("ligus approvals approve <rid> --recovery-words-file PATH  (at a terminal: type the code the requesting device shows)");
    }
    let mut text = String::new();
    for row in &rows {
        text.push_str(&format!(
            "{}  {:<8}  {}  fingerprint {}  ({})\n",
            row["rid"].as_str().unwrap_or_default(),
            row["kind"].as_str().unwrap_or_default(),
            row["label"].as_str().unwrap_or_default(),
            row["fingerprint"].as_str().unwrap_or_default(),
            row["claim"]["state"].as_str().unwrap_or_default()
        ));
    }
    if rows.is_empty() {
        text.push_str("No live approval requests.\n");
    }
    print_report(&r, json, serde_json::json!({ "approvals": rows }), &text);
    Ok(())
}

/// What `ligus approvals approve` takes.
pub struct ApproveOptions {
    pub rid: String,
    pub code: Option<String>,
    pub words: Option<String>,
    pub json: bool,
    /// End a contested request (`--end`) instead of approving it.
    pub end: bool,
}

/// `ligus approvals approve <rid> [--code K7Q4M9X2] --recovery-words-file PATH`:
/// a person's own step. The request is opened first (that is when the
/// requesting device shows its code); the person then types the code it
/// shows (`type_code`, a terminal prompt), or writes it to stdin with
/// `--code -`. It must equal what this device computes; the CLI never
/// offers it.
pub async fn cmd_approve(
    cfg: &Config,
    o: ApproveOptions,
    mut type_code: Option<&mut dyn FnMut() -> Result<String>>,
) -> Result<()> {
    if o.code.is_none() && type_code.is_none() && !o.end {
        return Err(problem(
            EXIT_HUMAN,
            "code_needed",
            "a person compares the code with the requesting device's screen",
            Some("run this at a terminal and type the code the requesting device shows, or pass --code"),
        ));
    }
    // The code exists only once this run has opened the request (it picks a
    // fresh nonce), so it is never known up front: `--code -` reads it from
    // stdin after the open, for a person typing into a pipe.
    if o.code.as_deref().is_some_and(|c| c.trim() != "-") {
        return Err(problem(
            EXIT_USAGE,
            "code_up_front",
            "the code appears on the requesting device only after this command opens the request, so it can't be given in advance",
            Some("run this at a terminal and type the code when asked, or pass --code - and write it to stdin once the requesting device shows it"),
        ));
    }
    let words = o.words.clone();
    if words.is_none() && !o.end {
        return Err(problem(
            EXIT_HUMAN,
            "presence_needed",
            "approving needs your passkey or recovery key",
            Some("use the desktop, or pass --recovery-words-file in your own terminal"),
        ));
    }
    if identity(cfg).load()?.is_some_and(|s| s.service) {
        return Err(problem(
            EXIT_REFUSED,
            "box",
            "this data dir is a box: an Admin approves from their own identity",
            None,
        ));
    }
    let (mut svc, ws, _, epochs, client) = member_context(cfg)?;
    let listing = am::list_live(&client, &epochs)
        .await
        .map_err(|e| mailbox_problem(e, "paste the request instead"))?;
    let rid = o.rid.trim().to_ascii_lowercase();
    let live = listing
        .live
        .into_iter()
        .find(|l| l.rid_hex() == rid)
        .ok_or_else(|| crate::problem::not_found(format!("no live approval request {rid}")))?;
    if matches!(live.request.subject, RequestSubject::Phone(_)) {
        return Err(problem(
            EXIT_REFUSED,
            "phone",
            "add a phone on your desktop (My devices > Add a phone) or with ligus phones certify",
            None,
        ));
    }
    let epoch = epochs
        .iter()
        .find(|e| e.epoch == live.epoch)
        .cloned()
        .ok_or_else(|| anyhow!("no key for epoch {}", live.epoch))?;
    let ids = identity(cfg);
    // Who is checking it already (design 6.4): another approver's request is
    // theirs; a contested one is ended, never approved.
    match am::claim_view(&live, svc.device_id(), &svc.approval_classifier(ws)?) {
        am::ClaimView::ClaimedBy { name, .. } => {
            return Err(problem(
                EXIT_REFUSED,
                "claimed",
                format!("{} is checking this request", shown(&name)),
                Some("leave it to them, or ask them to end it"),
            ));
        }
        am::ClaimView::Contested { by } => {
            let who = by
                .map(|(n, _)| format!(" (by {})", shown(&n)))
                .unwrap_or_default();
            note(&format!("Opened by a device that can't approve this{who}. It may be an attempt to intercept it."));
            if o.end {
                let signer = Signer::Ed25519(ids.device_keypair(svc.device_id())?);
                am::end_request(
                    &client,
                    &epoch,
                    &live.request,
                    &signer,
                    AbortReason::Contested,
                )
                .await
                .map_err(approver_problem)?;
                let mut r = Report::new("approvals approve");
                r.add(Check::ok(
                    "approval.ended",
                    "ended",
                    "the request was ended; nothing was granted",
                ));
                print_report(
                    &r,
                    o.json,
                    serde_json::json!({ "rid": rid, "ended": true }),
                    "Ended the request. Nothing was granted.\n",
                );
                return Ok(());
            }
            return Err(problem(
                EXIT_HUMAN,
                "contested",
                "this request is contested: it won't be approved",
                Some("end it with --end, and ask the requester to start again"),
            ));
        }
        _ if o.end => {
            return Err(problem(
                EXIT_USAGE,
                "not_contested",
                "--end ends a contested request; this one isn't",
                None,
            ));
        }
        _ => {}
    }
    let me = ApproverIdentity {
        signer: Signer::Ed25519(ids.device_keypair(svc.device_id())?),
        device_name: format!("{}'s CLI", svc.author().display_name),
        person: ApproverPerson {
            account_id: svc.author().account_id.unwrap_or_default(),
            name: svc.author().display_name.clone(),
        },
    };
    let mut seen = SeenSet::load(&cfg.data_dir)?;
    let mut session = ApproverSession::begin(client.clone(), &epoch, &live.request, me, &mut seen)
        .await
        .map_err(approver_problem)?;
    let notices = Notices::new(&cfg.data_dir);
    let notice = |kind: NoticeKind| Notice {
        kind,
        workspace_id: ws,
        rid: rid.clone(),
        request_kind: live.kind(),
        label: live.label(),
        at: now_ms(),
        by: None,
    };
    let start = Instant::now();
    let code = loop {
        match session.await_reveal(10).await {
            Ok(Some(c)) => break c,
            Ok(None) if start.elapsed() < Duration::from_secs(120) => continue,
            Ok(None) => {
                let _ = session.expire().await;
                return Err(problem(
                    EXIT_OFFLINE,
                    "no_reveal",
                    "the requesting device didn't answer",
                    Some("start again from the requesting device"),
                ));
            }
            Err(ApproverError::RevealMismatch) => {
                let _ = notices.record(notice(NoticeKind::Mismatch));
                return Err(problem(
                    EXIT_ERROR,
                    "mismatch",
                    "the requester's answer didn't match its commitment: nothing was granted",
                    Some(MISMATCH_FIX),
                ));
            }
            Err(e) => return Err(approver_problem(e)),
        }
    };
    note("Opened: the requesting device now shows a code.");
    let typed = match (&o.code, type_code.as_mut()) {
        (None, Some(t)) => t()?,
        (Some(_), _) => {
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            line.trim().to_string()
        }
        (None, None) => unreachable!("checked above"),
    };
    let mut r = Report::new("approvals approve");
    if !code.matches_typed(&typed) {
        let _ = session.mismatch().await;
        let _ = notices.record(notice(NoticeKind::Mismatch));
        return Err(problem(
            EXIT_ERROR,
            "mismatch",
            "that isn't the code this device computed: nothing was granted",
            Some(MISMATCH_FIX),
        ));
    }
    r.add(Check::ok("match-code", "match code", code.shown.clone()));
    // The code may have expired while the person typed, or the request
    // been ended: check before anything is signed.
    session.ready_to_commit().await.map_err(commit_problem)?;
    let key = crate::secret_arg::recovery_key_from_file(Path::new(&words.unwrap_or_default()))?;
    svc.set_presence_provider(Some(std::sync::Arc::new(crate::admit::RecoveryPresence(
        key,
    ))));
    let ctx = ligus_runtime::chat_service::MatchContext {
        label: live.label(),
        code: code.shown.clone(),
    };
    match &live.request.subject {
        RequestSubject::Box(join) => {
            session.ready_to_commit().await.map_err(commit_problem)?;
            svc.set_match_context(Some(ctx));
            let (inv, mut seed) = svc.issue_box_admission(
                ws,
                ligus_runtime::box_admission::DEFAULT_TTL_MS,
                Some(join),
            )?;
            let invite = crate::serve::admission_code(cfg, &svc, inv, &seed);
            zeroize::Zeroize::zeroize(&mut seed);
            let sealed = join.seal(&invite?).map_err(|e| anyhow!(e))?;
            drop(svc);
            publish(cfg, &mut r).await;
            session
                .finish_approved(Some(&sealed))
                .await
                .map_err(approver_problem)?;
            r.add(Check::ok(
                "approval.sent",
                "admission sent",
                "sealed to the box, through the mailbox",
            ));
        }
        RequestSubject::Computer(link) => {
            let conn = crate::devices_cmd::connection(cfg, &svc)?;
            let grant = svc.prepare_computer_link(ws, link, &conn, now_ms())?;
            session.offer(grant).await.map_err(approver_problem)?;
            loop {
                match session.await_ack(10).await {
                    Ok(true) => break,
                    Ok(false) => continue,
                    Err(ApproverError::DidntFinish) => {
                        let _ = notices.record(notice(NoticeKind::DidntFinish));
                        return Err(approver_problem(ApproverError::DidntFinish));
                    }
                    Err(e) => return Err(approver_problem(e)),
                }
            }
            session.ready_to_commit().await.map_err(|e| {
                if e == ApproverError::DidntFinish {
                    let _ = notices.record(notice(NoticeKind::DidntFinish));
                }
                commit_problem(e)
            })?;
            svc.set_match_context(Some(ctx));
            svc.publish_computer_link(ws, link, &conn, now_ms())?;
            drop(svc);
            publish(cfg, &mut r).await;
            session
                .finish_approved(None)
                .await
                .map_err(approver_problem)?;
            r.add(Check::ok(
                "approval.sent",
                "grant sent",
                "sealed to the computer, after it acknowledged",
            ));
        }
        RequestSubject::Phone(_) => unreachable!("refused above"),
    }
    print_report(
        &r,
        o.json,
        serde_json::json!({ "rid": rid, "kind": live.kind().as_str(), "label": live.label() }),
        &format!(
            "Approved {} \"{}\" (code {}).\n",
            live.kind().as_str(),
            live.label(),
            code.shown
        ),
    );
    Ok(())
}

/// Push the approval to the relay before the grant goes out (the requester
/// looks for it on the log).
async fn publish(cfg: &Config, r: &mut Report) {
    match crate::sync_once(cfg).await {
        Ok(_) => r.add(Check::ok(
            "approval.published",
            "published",
            "the approval is on the relay",
        )),
        Err(e) => r.add(
            Check::warn(
                "approval.published",
                "published",
                format!("not on the relay yet: {e}"),
            )
            .fix("run `ligus sync`"),
        ),
    }
}

/// Ask the person at a terminal for the code the requesting device shows.
pub fn type_code_on_tty() -> Result<String> {
    use std::io::Write;
    let mut err = std::io::stderr();
    write!(err, "Type the code the requesting device shows: ")?;
    err.flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}
