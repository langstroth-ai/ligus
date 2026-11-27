//! The passkey hand-off from the CLI (workspace model v3, 17.3): a write the
//! workspace's rules hold for your passkey is sent, drafted exactly as it
//! will be stored, to your own desktop, which shows it with a code and
//! confirms it with the passkey. The CLI prints
//!
//! ```text
//! Confirm on your desktop: Define workspace runtime “coder” (ClaudeCode, model “sonnet”) (code K7Q4 M9X2)
//! ```
//!
//! and waits (Ctrl-C cancels, `--timeout`, default 10 minutes), then writes
//! the change with the passkey's assertion. `--no-wait` posts it and exits 4;
//! `ligus handoff wait <id>` finishes it later, `ligus handoff cancel <id>`
//! ends it. A relay without the approval mailbox keeps today's answer (do it
//! in the desktop app, or pass `--recovery-words-file`).
//!
//! * [`HandoffPresence`]: the `PresenceProvider` `ligus setup` installs.
//! * `ligus handoff list|wait|cancel`: the pending hand-offs of this data dir.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use ligus_core::authz::PRESENCE_REQUIRED;
use ligus_core::presence::PresenceAssertion;
use ligus_core::SessionEventEnvelope;
use ligus_runtime::approval_mailbox::{
    self as am, MailboxClient, MailboxError, PresenceAsk, PresenceOutcome, PresencePendingStore, PresenceRequester, RequesterError, Signer,
};
use ligus_runtime::chat_service::{PresenceProvider, PresenceRequest};
use ligus_runtime::secret_store::StoreKeyVault;
use ligus_runtime::IdentityStore;
use uuid::Uuid;

use crate::checks::{Check, Report};
use crate::problem::{Problem, EXIT_ERROR, EXIT_HUMAN, EXIT_NOT_FOUND, EXIT_OFFLINE};
use crate::{uuid_of_room, Config};

/// How long the CLI waits for your desktop by default.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// How a hand-off ended (what `ligus setup` reports after the flow).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandoffEnd {
    /// Confirmed on the desktop; the write went ahead.
    Approved,
    /// Posted with `--no-wait`: finish with `ligus handoff wait <rid>`.
    Pending { rid: String, code: String, summary: String },
    /// No one confirmed within `--timeout` (the request was cancelled).
    TimedOut,
    /// The relay has no approval mailbox, or the change is too large for it:
    /// today's answer stands (the desktop app, or a recovery-words file).
    Unavailable(String),
    /// The person declined on their desktop (Not me), or ended it there.
    Declined,
    /// Cancelled here (Ctrl-C).
    Cancelled,
    /// Fifteen minutes passed.
    Expired,
    /// Anything else (the relay unreachable, …).
    Failed(String),
}

/// What the hand-off needs, read before the flow runs (the provider runs
/// while the flow holds the service).
#[derive(Clone)]
struct Ctx {
    data_dir: std::path::PathBuf,
    ws: Uuid,
    relay: String,
    token: Option<String>,
    founder_pin: String,
    epochs: Vec<am::EpochKey>,
    /// This account's own devices here (who may answer).
    mine: HashSet<Uuid>,
    device_seed: [u8; 32],
    label: String,
}

/// The CLI's passkey: a hand-off to the person's own desktop.
pub struct HandoffPresence {
    ctx: Ctx,
    /// `None`: `--no-wait`.
    timeout: Option<Duration>,
    end: Arc<Mutex<Option<HandoffEnd>>>,
}

fn device_signer(seed: &[u8; 32]) -> Signer {
    Signer::Ed25519(ligus_core::crypto::SigningKeypair::from_seed(seed).expect("32-byte seed"))
}

fn client(relay: &str, token: Option<String>) -> MailboxClient {
    MailboxClient::new(relay).with_auth(token)
}

impl HandoffPresence {
    /// The hand-off for `cfg`'s workspace, or `None` when it can't run here
    /// (no relay, no key, no account): the flow then keeps today's answer.
    pub fn for_service(cfg: &Config, svc: &ligus_runtime::ChatService, label: &str, timeout: Option<Duration>) -> Option<(Self, Arc<Mutex<Option<HandoffEnd>>>)> {
        let ws = uuid_of_room(&cfg.room);
        let relay = cfg.relay_url.clone().filter(|r| crate::usable_relay_url(r))?;
        svc.author().account_id?;
        let founder_pin = svc.founder_pin_hex(ws).ok().flatten()?;
        let epochs = svc.approval_epoch_keys(ws, &crate::key_source(cfg).ok()?).ok()?;
        if epochs.is_empty() {
            return None;
        }
        let mine = svc.my_device_ids(ws).ok()?.into_iter().collect();
        let ids = IdentityStore::new(&cfg.data_dir, StoreKeyVault::for_data_dir(&cfg.data_dir));
        let device_seed = ids.device_keypair(svc.device_id()).ok()?.seed_bytes();
        let end = Arc::new(Mutex::new(None));
        let ctx = Ctx { data_dir: cfg.data_dir.clone(), ws, relay, token: cfg.token.clone(), founder_pin, epochs, mine, device_seed, label: label.to_string() };
        Some((Self { ctx, timeout, end: end.clone() }, end))
    }

    fn finish(&self, end: HandoffEnd) {
        *self.end.lock().unwrap() = Some(end);
    }
}

impl PresenceProvider for HandoffPresence {
    fn assert(&self, _req: &PresenceRequest) -> std::result::Result<PresenceAssertion, String> {
        Err(PRESENCE_REQUIRED.into())
    }

    fn assert_draft(&self, req: &PresenceRequest, draft: &SessionEventEnvelope) -> std::result::Result<PresenceAssertion, String> {
        let ctx = self.ctx.clone();
        let draft = draft.clone();
        let summary = req.summary.clone();
        let timeout = self.timeout;
        let out = ligus_runtime::setup::service::block_on(async move { hand_off(ctx, draft, summary, timeout).await });
        let (end, result) = out.unwrap_or_else(|| (HandoffEnd::Failed("the hand-off stopped".into()), Err("the hand-off stopped".into())));
        self.finish(end);
        result
    }
}

/// Where today's answer applies, the words the flow shows.
fn unavailable(why: &str) -> String {
    format!("{PRESENCE_REQUIRED}: {why}")
}

async fn hand_off(
    ctx: Ctx,
    draft: SessionEventEnvelope,
    summary: String,
    timeout: Option<Duration>,
) -> (HandoffEnd, std::result::Result<PresenceAssertion, String>) {
    let client = client(&ctx.relay, ctx.token.clone());
    let address = match am::current_address(&client, ctx.ws, &ctx.founder_pin, &ctx.epochs).await {
        Ok(a) => a,
        Err(MailboxError::NoRoutes) => {
            return (HandoffEnd::Unavailable("this relay has no approval mailbox".into()), Err(unavailable("this relay has no approval mailbox")))
        }
        Err(e) => return (HandoffEnd::Failed(e.to_string()), Err(format!("couldn't reach your desktop through the relay: {e}"))),
    };
    let signer = device_signer(&ctx.device_seed);
    let ask = match PresenceAsk::new(&ctx.label, &signer.public_key(), &draft) {
        Ok(a) => a,
        Err(e) => return (HandoffEnd::Unavailable(e.to_string()), Err(unavailable(&e.to_string()))),
    };
    let mut req = match PresenceRequester::post(client, &address, signer, ask, &summary, Some(&ctx.data_dir)).await {
        Ok(r) => r,
        Err(RequesterError::Mailbox(MailboxError::NoRoutes)) => {
            return (HandoffEnd::Unavailable("this relay has no approval mailbox".into()), Err(unavailable("this relay has no approval mailbox")))
        }
        Err(RequesterError::Mailbox(MailboxError::TooLarge)) => {
            return (HandoffEnd::Unavailable("the change is too large to send".into()), Err(unavailable("the change is too large to send to your desktop")))
        }
        Err(e) => return (HandoffEnd::Failed(e.to_string()), Err(e.to_string())),
    };
    let code = req.code().shown.clone();
    eprintln!("Confirm on your desktop: {summary} (code {code})");
    let Some(timeout) = timeout else {
        let rid = req.rid_hex();
        return (
            HandoffEnd::Pending { rid: rid.clone(), code: code.clone(), summary },
            Err(format!("{PRESENCE_REQUIRED}: waiting for your desktop (code {code}); finish with: ligus handoff wait {rid}")),
        );
    };
    eprintln!("Waiting for your desktop (Ctrl-C cancels)…");
    let mine = ctx.mine.clone();
    let (end, result) = wait(&mut req, &mine, timeout).await;
    let _ = req.settle().await;
    (end, result)
}

/// Wait for the answer until `timeout`; cancel on Ctrl-C or the timeout.
async fn wait(req: &mut PresenceRequester, mine: &HashSet<Uuid>, timeout: Duration) -> (HandoffEnd, std::result::Result<PresenceAssertion, String>) {
    let deadline = Instant::now() + timeout;
    let is_mine = |d: Uuid| mine.contains(&d);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            let _ = req.cancel().await;
            return (HandoffEnd::TimedOut, Err(format!("{PRESENCE_REQUIRED}: no one confirmed it on your desktop in time (nothing was written)")));
        }
        let polled = tokio::select! {
            r = req.poll(left.as_secs().clamp(1, am::MAX_WAIT_SECS), &is_mine) => r,
            _ = tokio::signal::ctrl_c() => {
                let _ = req.cancel().await;
                return (HandoffEnd::Cancelled, Err("cancelled here: nothing was written".into()));
            }
        };
        match polled {
            Ok(None) => {}
            Ok(Some(PresenceOutcome::Approved { assertion, .. })) => {
                eprintln!("Confirmed on your desktop.");
                return (HandoffEnd::Approved, Ok(assertion));
            }
            Ok(Some(PresenceOutcome::Declined { .. })) | Ok(Some(PresenceOutcome::Aborted { .. })) => {
                return (HandoffEnd::Declined, Err("declined on your desktop: nothing was written".into()))
            }
            Ok(Some(PresenceOutcome::Expired)) => {
                return (HandoffEnd::Expired, Err(format!("{PRESENCE_REQUIRED}: the request expired (15 minutes): nothing was written")))
            }
            Err(RequesterError::Mailbox(m)) if m.retryable() => tokio::time::sleep(Duration::from_secs(3).min(left)).await,
            Err(e) => return (HandoffEnd::Failed(e.to_string()), Err(e.to_string())),
        }
    }
}

/// The `next` lines a flow ends with after a hand-off that didn't finish.
pub fn next_lines(end: &HandoffEnd, rerun: &str) -> Option<Vec<String>> {
    match end {
        HandoffEnd::Pending { rid, code, summary } => Some(vec![
            format!("confirm on your desktop: {summary} (code {code})"),
            format!("ligus handoff wait {rid}"),
            format!("then run this again: {rerun}"),
        ]),
        HandoffEnd::TimedOut | HandoffEnd::Expired => Some(vec![format!("run this again and confirm on your desktop: {rerun}")]),
        _ => None,
    }
}

// ---- ligus handoff list|wait|cancel ----

fn problem(exit: i32, code: &'static str, message: impl Into<String>, fix: Option<&str>) -> anyhow::Error {
    anyhow::Error::new(Problem { exit, code, message: message.into(), fix: fix.map(str::to_string) })
}

fn print(r: &Report, json: bool, result: serde_json::Value, text: &str) {
    if json {
        let mut v = r.to_json(0);
        if let Some(o) = v.as_object_mut() {
            o.insert("result".into(), result);
        }
        println!("{v}");
    } else {
        print!("{text}");
    }
}

/// `ligus handoff list [--json]`.
pub fn cmd_list(cfg: &Config, json: bool) -> Result<()> {
    let live = PresencePendingStore::new(&cfg.data_dir).live(ligus_core::flow::now_ms())?;
    let mut r = Report::new("handoff list");
    let mut text = String::new();
    for e in &live {
        r.add(Check::warn("handoff.pending", "waiting for your desktop", format!("{} (code {})", e.summary, e.code)).fix(format!("ligus handoff wait {}", e.rid)));
        text.push_str(&format!("{}  code {}  {}\n", e.rid, e.code, e.summary));
    }
    if live.is_empty() {
        text.push_str("No hand-offs are waiting.\n");
    }
    let rows: Vec<serde_json::Value> = live.iter().map(|e| serde_json::json!({ "id": e.rid, "code": e.code, "summary": e.summary, "createdAt": e.created_at })).collect();
    print(&r, json, serde_json::json!({ "pending": rows }), &text);
    Ok(())
}

fn pick(cfg: &Config, rid: Option<&str>) -> Result<am::PresencePending> {
    let live = PresencePendingStore::new(&cfg.data_dir).live(ligus_core::flow::now_ms())?;
    match rid {
        Some(id) => live.into_iter().find(|e| e.rid == id.trim()).ok_or_else(|| {
            problem(EXIT_NOT_FOUND, "no_handoff", "no hand-off with that id is waiting here (it expires after 15 minutes)", Some("ligus handoff list"))
        }),
        None => match live.len() {
            1 => Ok(live.into_iter().next().expect("one")),
            0 => Err(problem(EXIT_NOT_FOUND, "no_handoff", "no hand-off is waiting here", None)),
            _ => Err(crate::help::usage_error("several hand-offs are waiting: name one (ligus handoff list)")),
        },
    }
}

fn resume(cfg: &Config, e: &am::PresencePending) -> Result<(ligus_runtime::ChatService, PresenceRequester, HashSet<Uuid>)> {
    let svc = crate::open_service(cfg)?;
    let ids = IdentityStore::new(&cfg.data_dir, StoreKeyVault::for_data_dir(&cfg.data_dir));
    let signer = Signer::Ed25519(ids.device_keypair(svc.device_id())?);
    let req = PresenceRequester::resume(client(&e.relay, cfg.token.clone()), e, signer, Some(&cfg.data_dir)).map_err(|err| anyhow::anyhow!(err.to_string()))?;
    let mine = svc.my_device_ids(e.workspace_id)?.into_iter().collect();
    Ok((svc, req, mine))
}

/// `ligus handoff wait [<id>] [--timeout 10m] [--json]`: wait for your
/// desktop, then write the change with its passkey confirmation.
pub async fn cmd_wait(cfg: &Config, rid: Option<&str>, timeout: Duration, json: bool) -> Result<()> {
    let e = pick(cfg, rid)?;
    let (mut svc, mut req, mine) = resume(cfg, &e)?;
    eprintln!("Confirm on your desktop: {} (code {})", e.summary, e.code);
    eprintln!("Waiting for your desktop (Ctrl-C cancels)…");
    let (end, result) = wait(&mut req, &mine, timeout).await;
    let _ = req.settle().await;
    let mut r = Report::new("handoff wait");
    let assertion = match (end, result) {
        (HandoffEnd::Approved, Ok(a)) => a,
        (HandoffEnd::TimedOut, Err(m)) | (HandoffEnd::Expired, Err(m)) => {
            return Err(problem(EXIT_HUMAN, "not_confirmed", m.replace(&format!("{PRESENCE_REQUIRED}: "), ""), Some("run the command again and confirm on your desktop")))
        }
        (HandoffEnd::Failed(m), _) => return Err(problem(EXIT_OFFLINE, "relay", m, Some("check the network and the relay, then retry"))),
        (_, Err(m)) => return Err(problem(EXIT_ERROR, "declined", m, None)),
        (_, Ok(_)) => unreachable!("only an approval carries an assertion"),
    };
    let draft = svc.presence_draft_of(req.draft().clone())?;
    let env = svc.commit_presence(draft, assertion).map_err(|err| {
        problem(EXIT_ERROR, "not_written", format!("confirmed, but it couldn't be written: {err}"), Some("run the command again (it drafts the change anew)"))
    })?;
    r.add(Check::ok("handoff.written", "written", format!("{} (event {})", e.summary, env.event_id)));
    r.next("run the command that asked again: it skips what is done");
    print(&r, json, serde_json::json!({ "id": e.rid, "eventId": env.event_id, "summary": e.summary }), &format!("Confirmed on your desktop and written: {}\n", e.summary));
    Ok(())
}

/// `ligus handoff cancel [<id>]`: end a waiting hand-off (your desktop drops it).
pub async fn cmd_cancel(cfg: &Config, rid: Option<&str>, json: bool) -> Result<()> {
    let e = pick(cfg, rid)?;
    let (_svc, mut req, _) = resume(cfg, &e)?;
    let _ = req.cancel().await;
    req.settle().await.map_err(|err| anyhow::anyhow!(err.to_string()))?;
    let mut r = Report::new("handoff cancel");
    r.add(Check::ok("handoff.cancelled", "cancelled", e.summary.clone()));
    print(&r, json, serde_json::json!({ "id": e.rid }), &format!("Cancelled: {}\n", e.summary));
    Ok(())
}
