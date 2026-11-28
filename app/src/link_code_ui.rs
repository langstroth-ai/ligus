//! My devices > Add a computer, the device the person already uses: "Link a
//! device: code K7Q-4MX". A 6-character code for this workspace's approval
//! address (`ligus_runtime::approval_mailbox::LinkCodeOffer`), good for 10
//! minutes and one use. The new device types it (first run "I already use
//! Ligus", lt, `ligus devices request --code`); its request then comes to
//! the Inbox like any other, and the code match goes on there.
//!
//! One code at a time. While it is on screen it is checked every few
//! seconds: when the relay restarted it is posted again (the same code, and
//! the screen says so), and when it was used the screen says the request is
//! on its way. A relay without the code routes shows the approval address
//! instead (`noRoutes`).

use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use ligus_runtime::approval_mailbox::{self as am, CodeState, EpochKey, LinkCodeOffer, MailboxClient, MailboxError};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

/// The code moved on. Payload: [`LinkCodeDto`].
pub(crate) const CHANGED_EVENT: &str = "link-code://changed";
const CHECK_EVERY: Duration = Duration::from_secs(3);

/// What Add a computer shows. `state` is one of `live` (show `code`),
/// `used` (the new device typed it: its request comes to the Inbox),
/// `expired`, `noRoutes` (the relay has no short codes: `address` is the
/// approval address to paste instead) or `failed` (`error`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LinkCodeDto {
    pub state: String,
    pub workspace_id: String,
    /// `K7Q-4MX`.
    pub code: Option<String>,
    /// For a screen reader: "K 7 Q, 4 M X".
    pub code_spelled: Option<String>,
    /// The relay the code is on, when it isn't the community relay (the new
    /// device then names it too).
    pub relay: Option<String>,
    /// Unix ms.
    pub expires_at: Option<i64>,
    /// A passing note ("The relay restarted; the code works again.").
    pub notice: Option<String>,
    pub error: Option<String>,
    /// With `noRoutes`: the approval address (not a secret).
    pub address: Option<String>,
}

struct Shown {
    gen: u64,
    view: LinkCodeDto,
    offer: Option<Arc<tokio::sync::Mutex<LinkCodeOffer>>>,
    client: Option<MailboxClient>,
}

static SHOWN: LazyLock<Mutex<Shown>> = LazyLock::new(|| Mutex::new(Shown { gen: 0, view: LinkCodeDto::default(), offer: None, client: None }));

fn idle() -> LinkCodeDto {
    LinkCodeDto { state: "idle".into(), ..LinkCodeDto::default() }
}

fn emit(app: &AppHandle, v: &LinkCodeDto) {
    let _ = app.emit(CHANGED_EVENT, v.clone());
}

/// Change the view if `gen` is still the one on screen.
fn set(app: &AppHandle, gen: u64, f: impl FnOnce(&mut LinkCodeDto)) -> bool {
    let v = {
        let mut s = SHOWN.lock().unwrap();
        if s.gen != gen {
            return false;
        }
        f(&mut s.view);
        s.view.clone()
    };
    emit(app, &v);
    true
}

fn failed(ws: Uuid, e: &MailboxError) -> LinkCodeDto {
    let text = match e {
        MailboxError::Unreachable(_) | MailboxError::RateLimited(_) => format!("Couldn't reach the relay ({e}). Check the network, then try again."),
        other => format!("The relay refused the code: {other}."),
    };
    LinkCodeDto { state: "failed".into(), workspace_id: ws.to_string(), error: Some(text), ..LinkCodeDto::default() }
}

fn live_view(ws: Uuid, offer: &LinkCodeOffer) -> LinkCodeDto {
    let community = ligus_core::link_code::relay_salt(offer.relay()) == ligus_core::link_code::relay_salt(ligus_core::COMMUNITY_RELAY_URL);
    LinkCodeDto {
        state: "live".into(),
        workspace_id: ws.to_string(),
        code: Some(offer.code().shown()),
        code_spelled: Some(offer.code().spelled()),
        relay: (!community).then(|| offer.relay().to_string()),
        expires_at: Some(offer.expires_at()),
        ..LinkCodeDto::default()
    }
}

/// Show a code for `workspace_id` (a fresh one, or the one already live
/// for it).
#[tauri::command]
pub(crate) async fn link_code_show(app: AppHandle, workspace_id: String) -> Result<LinkCodeDto, String> {
    let ws = Uuid::parse_str(&workspace_id).map_err(|e| e.to_string())?;
    {
        let s = SHOWN.lock().unwrap();
        if s.view.workspace_id == workspace_id && s.view.state == "live" {
            return Ok(s.view.clone());
        }
    }
    let (client, pin, epochs) = crate::off_main(app.clone(), move |app| crate::approvals_ui::link_code_target(app, ws)).await?;
    let gen = {
        let mut s = SHOWN.lock().unwrap();
        s.gen += 1;
        s.offer = None;
        s.client = None;
        s.view = LinkCodeDto { state: "posting".into(), workspace_id: workspace_id.clone(), ..LinkCodeDto::default() };
        s.gen
    };
    let view = match LinkCodeOffer::post(&client, ws, &pin, &epochs).await {
        Ok(offer) => {
            let v = live_view(ws, &offer);
            let offer = Arc::new(tokio::sync::Mutex::new(offer));
            {
                let mut s = SHOWN.lock().unwrap();
                if s.gen != gen {
                    // Closed meanwhile: take it back.
                    let c = client.clone();
                    tokio::spawn(async move {
                        let _ = offer.lock().await.withdraw(&c).await;
                    });
                    return Ok(s.view.clone());
                }
                s.view = v.clone();
                s.offer = Some(offer.clone());
                s.client = Some(client.clone());
            }
            tokio::spawn(keep(app.clone(), gen, ws, offer, client, pin, epochs));
            v
        }
        Err(MailboxError::NoRoutes) => {
            // No code routes: the address works where the relay has a mailbox.
            let address = am::current_address(&client, ws, &pin, &epochs).await.ok().map(|a| a.encode());
            LinkCodeDto { state: "noRoutes".into(), workspace_id: workspace_id.clone(), address, ..LinkCodeDto::default() }
        }
        Err(e) => failed(ws, &e),
    };
    let mut s = SHOWN.lock().unwrap();
    if s.gen == gen {
        s.view = view.clone();
    }
    tracing::info!(target: "approvals", workspace = %ws, state = %view.state, "showing a link code");
    Ok(view)
}

/// Check the code until it is used, expires or the screen closes; post it
/// again after a relay restart.
async fn keep(app: AppHandle, gen: u64, ws: Uuid, offer: Arc<tokio::sync::Mutex<LinkCodeOffer>>, client: MailboxClient, pin: String, epochs: Vec<EpochKey>) {
    loop {
        tokio::time::sleep(CHECK_EVERY).await;
        let state = offer.lock().await.check(&client, &pin, &epochs).await;
        let go_on = match state {
            Ok(CodeState::Live) => set(&app, gen, |v| v.notice = None),
            Ok(CodeState::Reposted) => {
                let until = offer.lock().await.expires_at();
                set(&app, gen, |v| {
                    v.notice = Some("The relay restarted; the code works again.".into());
                    v.expires_at = Some(until);
                })
            }
            Ok(CodeState::Used) => {
                set(&app, gen, |v| {
                    v.state = "used".into();
                    v.notice = None;
                });
                return;
            }
            Ok(CodeState::Expired) => {
                set(&app, gen, |v| {
                    v.state = "expired".into();
                    v.notice = None;
                });
                return;
            }
            Err(e) if e.retryable() => set(&app, gen, |v| v.notice = Some(format!("Can't reach the relay right now ({e}). Still trying…"))),
            Err(e) => {
                let f = failed(ws, &e);
                set(&app, gen, |v| *v = f);
                return;
            }
        };
        if !go_on {
            return;
        }
    }
}

/// Where the code stands.
#[tauri::command]
pub(crate) fn link_code_status() -> LinkCodeDto {
    let s = SHOWN.lock().unwrap();
    if s.view.state.is_empty() {
        return idle();
    }
    s.view.clone()
}

/// The screen closed: take a live code back.
#[tauri::command]
pub(crate) async fn link_code_close(app: AppHandle) -> LinkCodeDto {
    let (offer, client, was_live) = {
        let mut s = SHOWN.lock().unwrap();
        s.gen += 1;
        let was_live = s.view.state == "live";
        s.view = idle();
        (s.offer.take(), s.client.take(), was_live)
    };
    if let (Some(offer), Some(client), true) = (offer, client, was_live) {
        if let Err(e) = offer.lock().await.withdraw(&client).await {
            tracing::debug!(target: "approvals", error = %e, "couldn't withdraw the link code (it expires anyway)");
        }
    }
    let v = idle();
    emit(&app, &v);
    v
}

/// The approval address behind a short code a person typed on this new
/// desktop, on `relay` (the community relay when none is named).
pub(crate) async fn resolve_typed(code: &ligus_core::link_code::ShortCode, relay: &str, token: Option<String>) -> Result<am::ApprovalAddress, MailboxError> {
    am::resolve_link_code(&MailboxClient::new(relay).with_auth(token), code, true).await
}
