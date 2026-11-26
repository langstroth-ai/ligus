//! Passkey presence from the phone (Phase 0, B3): `POST v1/presence/prepare`
//! and `POST v1/presence/commit`.
//!
//! A box enrolled as its owner's device (`ligus serve`) signs as the owner
//! without the owner being there, so a vote, a lockdown clear or an agent
//! rebind from the phone carries the owner's passkey assertion over the
//! event's digest. `prepare` drafts the event exactly as the plain endpoint
//! would (same payload, frontier, lamport and event id; nothing is written)
//! and returns its challenge and a preview; the phone runs
//! `ASAuthorization` (the workspace RP, `ligus.ai` by default) over the challenge; `commit` attaches
//! the assertion inside the stamp, signs with this box's key and appends.
//! The fold counts the vote as the account that owns the credential, which
//! must be the voter's; a passkey of any other account is refused.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::{Extension, Json};
use ligus_core::presence::{b64url, PresenceAssertion};
use ligus_runtime::chat_service::PresenceDraft;
use serde::Deserialize;
use serde_json::{json, Value};

use super::api::{ApiError, ApiResult};
use super::tunnel::TunnelPeer;
use super::Shared;

/// How long a prepared event waits for its passkey.
const TTL: Duration = Duration::from_secs(300);
/// Prepared events one credential keeps (its oldest goes past this).
const PER_CALLER: usize = 8;
/// Prepared events kept at once.
const MAX_PENDING: usize = 32;

/// Prepared events by id (single use, in memory), per credential.
pub struct Pending(super::drafts::OwnedDrafts<PresenceDraft>);

impl Default for Pending {
    fn default() -> Self {
        Self(super::drafts::OwnedDrafts::new(TTL, PER_CALLER, MAX_PENDING))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareReq {
    /// `agent_host` or `invite` (a single-use invite to the box's
    /// workspace; `target` is ignored). `approve`, `reject` and
    /// `lockdown_clear` are gone (410): the phone signs them itself
    /// (`/v1/phone-actions`).
    action: String,
    /// The proposal, chat or agent id.
    target: String,
    /// `agent_host`: the host to bind to (empty: the owner's device).
    #[serde(default)]
    host_id: Option<String>,
}

/// `POST /v1/presence/prepare {action, target}` →
/// `{prepareId, challenge, preview}`.
pub async fn prepare(
    State(s): State<std::sync::Arc<Shared>>,
    peer: Option<Extension<TunnelPeer>>,
    Json(req): Json<PrepareReq>,
) -> ApiResult<Json<Value>> {
    let mut preview = json!({});
    let outcome = match req.action.as_str() {
        // Phase B: the phone signs these itself (`/v1/phone-actions`).
        "approve" | "reject" => return Err(super::api::gone_to_phone_action("vote")),
        "lockdown_clear" => return Err(super::api::gone_to_phone_action("clearLockdown")),
        "agent_host" => {
            let agent = uuid::Uuid::parse_str(req.target.trim()).map_err(|_| ApiError::bad_request("agent id"))?;
            let host = req.host_id.clone().unwrap_or_default();
            let mut svc = s.lock();
            svc.capture_presence(true);
            let r = svc.set_agent_host(s.ws, agent, host.trim()).map(|_| ());
            (r, svc.take_presence_draft())
        }
        "invite" => {
            // The commit answers with the invite code: secure channel only.
            super::identity_api::require_secret_channel(&s, peer.as_deref())?;
            if let Some(reason) = s.lock().write_block() {
                return Err(ApiError::new(StatusCode::FORBIDDEN, reason.to_string()));
            }
            preview = json!({ "workspaceName": super::identity_api::invite_preview_name(&s) });
            let mut svc = s.lock();
            svc.capture_presence(true);
            let r = svc.issue_invite(s.ws).map(|_| ());
            (r, svc.take_presence_draft())
        }
        _ => return Err(ApiError::bad_request("unknown action")),
    };
    s.lock().capture_presence(false);
    let draft = match outcome {
        (_, Some(d)) => d,
        (Err(e), None) => return Err(e.into()),
        (Ok(()), None) => return Err(ApiError::new(StatusCode::CONFLICT, "nothing to confirm")),
    };
    let r = &draft.request;
    preview["kind"] = json!(r.kind);
    preview["summary"] = json!(r.summary);
    preview["digestShort"] = json!(r.digest_short);
    preview["rpId"] = json!(r.policy.rp_id);
    preview["origins"] = json!(r.policy.origins);
    preview["allowCredentials"] = json!(r.credential_ids);
    let challenge = b64url(&r.challenge);
    let id = s.presence_drafts.0.put(&super::api::caller().key(), draft).map_err(super::api::draft_refused)?;
    Ok(Json(json!({ "prepareId": id, "challenge": challenge, "preview": preview })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitReq {
    prepare_id: String,
    assertion: Assertion,
}

/// The phone's `ASAuthorizationPlatformPublicKeyCredentialAssertion`, base64url.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Assertion {
    credential_id: String,
    authenticator_data: String,
    #[serde(alias = "clientDataJSON")]
    client_data_json: String,
    signature: String,
}

/// `POST /v1/presence/commit {prepareId, assertion}` → the appended event.
pub async fn commit(
    State(s): State<std::sync::Arc<Shared>>,
    peer: Option<Extension<TunnelPeer>>,
    Json(req): Json<CommitReq>,
) -> ApiResult<Json<Value>> {
    let draft = s
        .presence_drafts
        .0
        .take(&super::api::caller().key(), &req.prepare_id)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown or expired prepareId"))?;
    // An invite prepared over the tunnel and committed over plain HTTP would
    // hand its code out there: refused before anything is appended.
    if matches!(&draft.env.payload, ligus_core::SessionEvent::JoinApproved { subject, .. } if subject.starts_with("invite:")) {
        super::identity_api::require_secret_channel(&s, peer.as_deref())?;
    }
    let a = PresenceAssertion {
        credential_id: req.assertion.credential_id,
        authenticator_data: req.assertion.authenticator_data,
        client_data_json: req.assertion.client_data_json,
        signature: req.assertion.signature,
    };
    let kind = draft.request.kind.clone();
    let env = s.lock().commit_presence(draft, a)?;
    s.after_write();
    let mut out = json!({ "committed": true, "kind": kind, "eventId": env.event_id });
    // A confirmed invite: the code to share (only for the box's workspace).
    if let Some(invite) = super::identity_api::invite_after_commit(&s, &env) {
        out["invite"] = invite;
    }
    Ok(Json(out))
}
