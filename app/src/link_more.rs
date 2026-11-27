//! A computer link that brings all the person's workspaces, on the
//! approving desktop (`ligus_runtime::approval_mailbox` "more workspaces").
//!
//! The request came to one workspace's approval address. Every other team
//! workspace this desktop holds where it can add the computer (the person is
//! a member, this desktop is confirmed there, there is a passkey, no
//! lockdown…) gets a grant too, offered with the first. Once the computer
//! acknowledges, each one is confirmed with the passkey in turn, with a count
//! on the sheet ("Add computer “mbp” to your devices, code K7Q4 M9X2, in
//! “Beta” (2 of 3)"); a cancelled or refused one is skipped and said so. One
//! passkey over all of them is a later change (one passkey over a batch).

use std::sync::Arc;

use ligus_core::presence::PresenceAssertion;
use ligus_runtime::approval_mailbox::MoreGrant;
use ligus_runtime::chat_service::{PresenceProvider, PresenceRequest};
use ligus_runtime::computer_link::LinkRequest;
use ligus_runtime::workspace_invite::WorkspaceConn;
use serde::Serialize;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

use crate::AppState;

/// One more workspace on the approver's sheet. `state` is one of:
///
/// * `offered`: offered with the link, waiting for the computer;
/// * `skipped`: this desktop can't add the computer there (`message` says why);
/// * `leftOut`: too many to send at once: link it there afterwards;
/// * `notTaken`: the computer keeps one workspace (an older one, or the CLI);
/// * `adding`: its passkey is up;
/// * `added`: the computer is one of your devices there too;
/// * `notAdded`: its passkey was cancelled or refused (`message`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MoreWsDto {
    pub workspace_id: String,
    pub name: String,
    pub state: String,
    pub message: Option<String>,
}

fn row(ws: Uuid, name: &str, state: &str, message: Option<String>) -> MoreWsDto {
    MoreWsDto { workspace_id: ws.to_string(), name: name.to_string(), state: state.into(), message }
}

/// The other team workspaces (not `first`, not this device's own Home, not
/// direct messages): their id, name and connection.
fn others(st: &AppState, first: Uuid) -> Vec<(Uuid, String, Result<String, String>)> {
    let conns: Vec<WorkspaceConn> = st.settings.lock().unwrap().workspaces.clone();
    conns
        .into_iter()
        .filter(|c| c.dm_account.is_none() && c.id() != first && c.id() != st.local_workspace_id)
        .map(|c| (c.id(), crate::shown(&c.display_name()), crate::computer_link_conn(st, c.id())))
        .collect()
}

/// For a request answered in `first`: the more grants this desktop can
/// offer, and the rows for the workspaces it can't add the computer to.
pub(crate) fn prepare(st: &AppState, first: Uuid, request: &LinkRequest, now_ms: i64) -> (Vec<MoreGrant>, Vec<MoreWsDto>) {
    let mut rows = Vec::new();
    let mut ready = Vec::new();
    for (ws, name, conn) in others(st, first) {
        match conn {
            Ok(c) => ready.push((ws, name, c)),
            Err(e) => rows.push(row(ws, &name, "skipped", Some(e))),
        }
    }
    let names: Vec<(Uuid, String)> = ready.iter().map(|(w, n, _)| (*w, n.clone())).collect();
    let prepared = st.service.lock().unwrap().prepare_more_computer_links(request, &ready, now_ms);
    let mut grants = Vec::new();
    for (ws, r) in prepared {
        let name = names.iter().find(|(w, _)| *w == ws).map(|(_, n)| n.clone()).unwrap_or_default();
        match r {
            Ok(g) => grants.push(g),
            Err(why) => rows.push(row(ws, &name, "skipped", Some(sentence(&why)))),
        }
    }
    (grants, rows)
}

/// The rows once the offer went out: offered, or left out.
pub(crate) fn offered_rows(grants: &[(Uuid, String)], left_out: &[Uuid]) -> Vec<MoreWsDto> {
    grants
        .iter()
        .map(|(ws, name)| {
            if left_out.contains(ws) {
                row(*ws, name, "leftOut", Some("Too many to add at once: link this computer there afterwards (＋ → Join → Link this computer).".into()))
            } else {
                row(*ws, name, "offered", None)
            }
        })
        .collect()
}

fn sentence(s: &str) -> String {
    let mut out = s.trim().to_string();
    if let Some(c) = out.get(..1) {
        out.replace_range(..1, &c.to_uppercase());
    }
    if !out.ends_with('.') {
        out.push('.');
    }
    out
}

/// The desktop's passkey, with "in “Beta” (2 of 3)" on its sheet.
pub(crate) struct StepPresence {
    pub app: AppHandle,
    pub workspace: String,
    pub index: usize,
    pub total: usize,
}

impl PresenceProvider for StepPresence {
    fn assert(&self, req: &PresenceRequest) -> Result<PresenceAssertion, String> {
        let mut r = req.clone();
        r.summary = step_summary(&req.summary, &self.workspace, self.index, self.total);
        crate::passkey_ui::DesktopPresence(self.app.clone()).assert(&r)
    }
}

/// `summary`, naming the workspace and the count.
pub(crate) fn step_summary(summary: &str, workspace: &str, index: usize, total: usize) -> String {
    format!("{summary}, in \u{201C}{}\u{201D} ({index} of {total})", ligus_runtime::approval_mailbox::hint(workspace))
}

/// Run `f` on the service with the counted passkey sheet (only when there
/// is more than one to confirm), then the desktop's own provider again.
pub(crate) fn with_step<R>(app: &AppHandle, workspace: &str, index: usize, total: usize, f: impl FnOnce(&mut ligus_runtime::ChatService) -> R) -> R {
    let st = app.state::<AppState>();
    let mut svc = st.service.lock().unwrap();
    if total > 1 {
        svc.set_presence_provider(Some(Arc::new(StepPresence { app: app.clone(), workspace: workspace.to_string(), index, total })));
    }
    let r = f(&mut *svc);
    if total > 1 {
        svc.set_presence_provider(Some(Arc::new(crate::passkey_ui::DesktopPresence(app.clone()))));
    }
    r
}

/// Publish the certificate for `request` in the more workspace `ws`, with
/// its passkey (`index` of `total`), and push it to the relay.
pub(crate) async fn publish_one(app: AppHandle, ws: Uuid, name: String, request: LinkRequest, ctx: ligus_runtime::chat_service::MatchContext, index: usize, total: usize) -> Result<(), String> {
    let app2 = app.clone();
    let conn = crate::off_main(app.clone(), move |app| {
        let st = app.state::<AppState>();
        let code = crate::computer_link_conn(&st, ws)?;
        with_step(app, &name, index, total, |svc| {
            svc.set_match_context(Some(ctx));
            let r = svc.publish_computer_link_in(ws, &request, &code, ligus_core::flow::now_ms()).map_err(|e| sentence(&e.to_string()));
            svc.set_match_context(None);
            r
        })?;
        let conn = st.settings.lock().unwrap().workspaces.iter().find(|w| w.id() == ws).cloned();
        Ok::<_, String>(conn)
    })
    .await?;
    // The computer looks for the certificate on that workspace's log.
    if let Some(conn) = conn {
        if let Err(e) = crate::link_request_ui::sync_conn(&app2.state::<AppState>(), &conn).await {
            tracing::warn!(target: "approvals", "pushing the certificate to the relay failed (the next sync sends it): {e}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sheet_names_the_workspace_and_the_count() {
        assert_eq!(
            step_summary("Add computer \u{201C}mbp\u{201D} to your devices, code K7Q4 M9X2", "Beta\u{202E}", 2, 3),
            "Add computer \u{201C}mbp\u{201D} to your devices, code K7Q4 M9X2, in \u{201C}Beta\u{201D} (2 of 3)"
        );
        let rows = offered_rows(&[(Uuid::from_u128(1), "Beta".into()), (Uuid::from_u128(2), "Gamma".into())], &[Uuid::from_u128(2)]);
        assert_eq!(rows.iter().map(|r| r.state.as_str()).collect::<Vec<_>>(), ["offered", "leftOut"]);
        assert_eq!(sentence("add a passkey first"), "Add a passkey first.");
    }
}
