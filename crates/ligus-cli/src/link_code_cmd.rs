//! Short link codes from the CLI (Ligus 0.5):
//!
//! * `ligus devices link [--code] [--wait D] [--json]`: the device the person
//!   already uses shows "Link a device: code K7Q-4MX" for its workspace's
//!   approval address, keeps it posted (again after a relay restart) and
//!   says when the new device used it. The request then comes to the Inbox.
//! * `ligus devices request --code K7Q-4MX [--relay URL]`: the new device
//!   types it; the relay resolves it to the approval address, and the code
//!   match goes on exactly as with `--to`.
//!
//! A relay without the code routes falls back to the approval address.

use std::time::{Duration, Instant};

use anyhow::Result;
use ligus_core::link_code::{relay_salt, ShortCode};
use ligus_runtime::approval_mailbox::{self as am, CodeState, LinkCodeOffer, MailboxError};

use crate::approvals_cmd::{self as ac, Ask, ToOptions};
use crate::checks::{Check, Report};
use crate::problem::{Problem, EXIT_ERROR, EXIT_FIXABLE, EXIT_HUMAN, EXIT_NOT_FOUND, EXIT_OFFLINE, EXIT_REFUSED, EXIT_USAGE};
use crate::Config;

/// How often the showing side checks its code.
#[cfg(not(test))]
const CHECK_EVERY: Duration = Duration::from_secs(3);
#[cfg(test)]
const CHECK_EVERY: Duration = Duration::from_millis(200);

fn problem(exit: i32, code: &'static str, message: impl Into<String>, fix: Option<&str>) -> anyhow::Error {
    anyhow::Error::new(Problem { exit, code, message: message.into(), fix: fix.map(str::to_string) })
}

/// The `--relay` a new device types its code for: the flag, else this data
/// dir's relay, else the community relay.
pub fn code_relay(cfg: &Config, flag: Option<String>) -> String {
    flag.filter(|r| !r.trim().is_empty())
        .or_else(|| cfg.relay_url.clone().filter(|r| crate::usable_relay_url(r)))
        .unwrap_or_else(|| ligus_core::COMMUNITY_RELAY_URL.to_string())
        .trim()
        .trim_end_matches('/')
        .to_string()
}

/// Whether `relay` is the community relay (the new device needs no `--relay`).
fn is_community(relay: &str) -> bool {
    relay_salt(relay) == relay_salt(ligus_core::COMMUNITY_RELAY_URL)
}

/// What the new device runs to type `code` (with `--relay` off the community relay).
fn request_command(code: &ShortCode, relay: &str) -> String {
    if is_community(relay) {
        format!("ligus devices request --code {}", code.shown())
    } else {
        format!("ligus devices request --code {} --relay {relay}", code.shown())
    }
}

/// `ligus devices link`: show a code until the new device uses it.
pub async fn cmd_link(cfg: &Config, wait: Duration, json: bool) -> Result<()> {
    link(cfg, wait, json, None).await
}

/// [`cmd_link`], telling `code_tx` the code as it is shown (tests).
pub(crate) async fn link(cfg: &Config, wait: Duration, json: bool, code_tx: Option<std::sync::mpsc::Sender<String>>) -> Result<()> {
    if ligus_runtime::IdentityStore::new(&cfg.data_dir, ligus_runtime::secret_store::StoreKeyVault::for_data_dir(&cfg.data_dir))
        .load()?
        .is_some_and(|s| s.service)
    {
        return Err(problem(EXIT_REFUSED, "box", "this data dir is a box: a box can't link a person's devices", Some("run `ligus devices link` on your own computer, or use My devices > Add a computer on your desktop")));
    }
    let (_svc, ws, pin, epochs, client) = ac::member_context(cfg)?;
    let offer = match LinkCodeOffer::post(&client, ws, &pin, &epochs).await {
        Ok(o) => o,
        Err(MailboxError::NoRoutes) => {
            // No code routes: the approval address still works when the
            // relay has a mailbox (it isn't a secret, so the fix carries it).
            return Err(match am::current_address(&client, ws, &pin, &epochs).await {
                Ok(a) => problem(
                    EXIT_FIXABLE,
                    "no_code_routes",
                    "this relay has no short codes yet",
                    Some(&format!("on the new device, use the approval address instead: ligus devices request --to {}", a.encode())),
                ),
                Err(_) => problem(
                    EXIT_FIXABLE,
                    "no_mailbox_routes",
                    "this relay has no approval mailbox",
                    Some("on the new device run `ligus devices request` and paste its request into your desktop (My devices > Add a computer)"),
                ),
            });
        }
        Err(e) => return Err(ac::mailbox_problem(e, "ligus admit address, then ligus devices request --to on the new device")),
    };
    let code = offer.code().clone();
    let relay = offer.relay().to_string();
    let typed = request_command(&code, &relay);
    ac::say(json, &format!("Link a device: code {}", code.shown()));
    if let Some(tx) = &code_tx {
        let _ = tx.send(code.shown());
    }
    ac::say(
        json,
        &format!(
            "On the new device, choose \"I already use Ligus\" and type the code{}, or run: {typed}\nThe code works for 10 minutes, once. Then approve the request in your desktop's Inbox (or: ligus approvals list).",
            if is_community(&relay) { String::new() } else { format!(" (relay {relay})") }
        ),
    );
    let mut r = Report::new("devices link");
    r.add(Check::ok("link.code", "short code", format!("{} on {relay}", code.shown())));
    let result = |used: bool, expires_at: i64| serde_json::json!({ "code": code.as_str(), "shown": code.shown(), "relay": relay, "used": used, "expiresAt": expires_at, "workspace": cfg.room });
    let mut offer = offer;
    let deadline = Instant::now() + wait;
    let mut trouble = false;
    loop {
        if Instant::now() >= deadline {
            return Err(problem(
                EXIT_HUMAN,
                "waiting",
                format!("the new device hasn't typed the code {} yet", code.shown()),
                Some(&format!("on the new device: {typed} (the code works until it expires; nothing posts it again if the relay restarts)")),
            ));
        }
        match offer.check(&client, &pin, &epochs).await {
            Ok(CodeState::Live) => trouble = false,
            Ok(CodeState::Reposted) => {
                trouble = false;
                ac::note("The relay restarted; posted the code again. It's the same code.");
            }
            Ok(CodeState::Used) => {
                r.add(Check::ok("link.used", "used", "the new device has the code; its request comes to the Inbox"));
                r.next("approve it in your desktop's Inbox, or: ligus approvals list, then ligus approvals approve <rid>");
                ac::print_report(&r, json, result(true, offer.expires_at()), "The new device has the code. Approve its request in your desktop's Inbox (or: ligus approvals list).\n");
                return Ok(());
            }
            Ok(CodeState::Expired) => {
                return Err(problem(EXIT_HUMAN, "expired", "the code expired before the new device typed it", Some("run `ligus devices link` again for a new code")));
            }
            Err(e) if e.retryable() => {
                if !trouble {
                    ac::note(&format!("Can't reach the relay right now ({e}). Still trying…"));
                }
                trouble = true;
            }
            Err(e) => return Err(ac::mailbox_problem(e, "ligus admit address, then ligus devices request --to on the new device")),
        }
        tokio::time::sleep(CHECK_EVERY.min(deadline.saturating_duration_since(Instant::now()))).await;
    }
}

/// `ligus devices request --code`: the approval address behind the code.
pub async fn resolve_code(cfg: &Config, text: &str, relay: &str) -> Result<am::ApprovalAddress> {
    let code = ShortCode::parse(text).ok_or_else(|| {
        problem(EXIT_USAGE, "bad_code", "that isn't a short code (6 letters and digits, like K7Q-4MX)", Some("type the code your other device shows (ligus devices link, or My devices > Add a computer)"))
    })?;
    let client = am::MailboxClient::new(relay).with_auth(cfg.token.clone());
    match am::resolve_link_code(&client, &code, true).await {
        Ok(a) => Ok(a),
        Err(MailboxError::NoRoutes) => Err(problem(
            EXIT_FIXABLE,
            "no_code_routes",
            format!("the relay {relay} has no short codes"),
            Some("pass --relay with your other device's relay, or ask it for its approval address and run ligus devices request --to <address>"),
        )),
        Err(MailboxError::CodeUnknown) => Err(problem(
            EXIT_NOT_FOUND,
            "code_unknown",
            format!("no device is showing {} on {relay} (mistyped, expired or already used)", code.shown()),
            Some("check the code, or get a new one on your other device; a self-hosted relay needs --relay"),
        )),
        Err(e @ (MailboxError::Unreachable(_) | MailboxError::RateLimited(_))) => {
            Err(problem(EXIT_OFFLINE, "relay_unreachable", e.to_string(), Some("check the network and the relay, then retry")))
        }
        Err(MailboxError::Malformed) => Err(problem(EXIT_ERROR, "bad_answer", "the relay's answer for that code didn't check out: nothing was sent", Some("get a new code on your other device and try again"))),
        Err(e) => Err(problem(EXIT_ERROR, "code", e.to_string(), None)),
    }
}

/// `ligus devices request --code K7Q-4MX`: resolve, then the `--to` flow.
pub async fn cmd_request_code(cfg: &Config, text: &str, relay: Option<String>, mut o: ToOptions, new_device: bool, ask: Option<Ask<'_>>) -> Result<()> {
    if ask.is_none() {
        // Before the code is used up: a computer is linked by a person at it.
        return Err(problem(
            EXIT_HUMAN,
            "terminal_needed",
            "a computer is linked by a person at it: run this in a terminal to confirm the code and who you link as",
            Some("run `ligus devices request --code …` in a terminal"),
        ));
    }
    let relay = code_relay(cfg, relay);
    let address = resolve_code(cfg, text, &relay).await?;
    ac::note(&format!("Found your other device's workspace on {}.", address.relay));
    o.to = address.encode();
    ac::cmd_devices_request_to(cfg, o, new_device, ask).await
}
