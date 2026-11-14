//! Passkey presence from the phone (Phase 0, B3): `POST v1/presence/prepare`
//! and `POST v1/presence/commit`.
//!
//! A box enrolled as its owner's device (`ligus serve`) signs as the owner
//! without the owner being there, so a vote, a lockdown clear or an agent
//! rebind from the phone carries the owner's passkey assertion over the
//! event's digest. `prepare` drafts the event exactly as the plain endpoint
//! would (same payload, frontier, lamport and event id; nothing is written)
//! and returns its challenge and a preview; the phone runs
//! `ASAuthorization` (RP `ligus.ai`) over the challenge; `commit` attaches
//! the assertion inside the stamp, signs with this box's key and appends.
//! The fold counts the vote as the account that owns the credential, which
//! must be the voter's; a passkey of any other account is refused.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use hive_core::presence::{b64url, PresenceAssertion, RP_ID};
use hive_runtime::chat_service::PresenceDraft;
use serde::Deserialize;
use serde_json::{json, Value};

use super::api::{find_proposal, load_chat, ApiError, ApiResult};
use super::Shared;

/// How long a prepared event waits for its passkey.
const TTL: Duration = Duration::from_secs(300);
/// Prepared events kept at once.
const MAX_PENDING: usize = 32;

/// A fresh prepared-event id: 16 random bytes, base64url.
fn pending_id(fill: fn(&mut [u8]) -> Result<(), getrandom::Error>) -> Result<String, getrandom::Error> {
    let mut raw = [0u8; 16];
    fill(&mut raw)?;
    Ok(b64url(&raw))
}

/// Prepared events by id (single use, in memory).
#[derive(Default)]
pub struct Pending(Mutex<HashMap<String, (PresenceDraft, Instant)>>);

impl Pending {
    fn put(&self, draft: PresenceDraft) -> Result<String, getrandom::Error> {
        let id = pending_id(getrandom::fill)?;
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|_, (_, at)| at.elapsed() < TTL);
        if m.len() >= MAX_PENDING {
            if let Some(oldest) = m.iter().min_by_key(|(_, (_, at))| *at).map(|(k, _)| k.clone()) {
                m.remove(&oldest);
            }
        }
        m.insert(id.clone(), (draft, Instant::now()));
        Ok(id)
    }
    fn take(&self, id: &str) -> Option<PresenceDraft> {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.remove(id).filter(|(_, at)| at.elapsed() < TTL).map(|(d, _)| d)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareReq {
    /// `approve`, `reject`, `lockdown_clear` or `agent_host`.
    action: String,
    /// The proposal, chat or agent id.
    target: String,
    /// `agent_host`: the host to bind to (empty: the owner's device).
    #[serde(default)]
    host_id: Option<String>,
}

/// `POST /v1/presence/prepare {action, target}` →
/// `{prepareId, challenge, preview}`.
pub async fn prepare(State(s): State<std::sync::Arc<Shared>>, Json(req): Json<PrepareReq>) -> ApiResult<Json<Value>> {
    let mut preview = json!({});
    let outcome = match req.action.as_str() {
        "approve" | "reject" => {
            let (chat, p) = find_proposal(&s, &req.target)?;
            preview = json!({
                "chatTitle": chat.title,
                "proposalTitle": p.title,
                "diffDigest": hive_core::authz::votes::diff_digest(&p),
            });
            let mut svc = s.lock();
            svc.capture_presence(true);
            let r = svc.vote_on_proposal(chat.id, chat.workspace_id, p.id, req.action == "approve").map(|_| ());
            (r, svc.take_presence_draft())
        }
        "lockdown_clear" => {
            let chat = load_chat(&s, &req.target)?;
            preview = json!({ "chatTitle": chat.title });
            let mut svc = s.lock();
            svc.capture_presence(true);
            let r = svc.clear_lockdown(s.ws, chat.id).map(|_| ());
            (r, svc.take_presence_draft())
        }
        "agent_host" => {
            let agent = uuid::Uuid::parse_str(req.target.trim()).map_err(|_| ApiError::bad_request("agent id"))?;
            let host = req.host_id.clone().unwrap_or_default();
            let mut svc = s.lock();
            svc.capture_presence(true);
            let r = svc.set_agent_host(s.ws, agent, host.trim()).map(|_| ());
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
    preview["rpId"] = json!(RP_ID);
    preview["allowCredentials"] = json!(r.credential_ids);
    let challenge = b64url(&r.challenge);
    let id = s.presence_drafts.put(draft).map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("no OS randomness: {e}")))?;
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
pub async fn commit(State(s): State<std::sync::Arc<Shared>>, Json(req): Json<CommitReq>) -> ApiResult<Json<Value>> {
    let draft = s
        .presence_drafts
        .take(&req.prepare_id)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown or expired prepareId"))?;
    let a = PresenceAssertion {
        credential_id: req.assertion.credential_id,
        authenticator_data: req.assertion.authenticator_data,
        client_data_json: req.assertion.client_data_json,
        signature: req.assertion.signature,
    };
    let kind = draft.request.kind.clone();
    let env = s.lock().commit_presence(draft, a)?;
    s.after_write();
    Ok(Json(json!({ "committed": true, "kind": kind, "eventId": env.event_id })))
}

#[cfg(test)]
mod tests {
    /// No OS randomness: no id (an all-zero id would be guessable, and the
    /// next one would collide with it).
    #[test]
    fn pending_ids_need_randomness() {
        assert!(super::pending_id(|_| Err(getrandom::Error::UNSUPPORTED)).is_err());
        let (a, b) = (super::pending_id(getrandom::fill).unwrap(), super::pending_id(getrandom::fill).unwrap());
        assert_ne!(a, b);
        assert_eq!(a.len(), 22);
    }
}
