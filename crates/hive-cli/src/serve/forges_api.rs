//! Code-host connections from the phone (IDENTITY-AND-FORGES 5.7). The
//! phone drives the box; the box holds the tokens. The phone never stores
//! one.
//!
//! - `GET    /v1/forges` → `{forges:[{id, kind, displayName, baseUrl,
//!   authSource, user, available, reason, hasRead, hasWrite, deviceFlow, …}],
//!   tokenEntry}`. Never a token.
//! - `POST   /v1/forges {kind, baseUrl, auth?, clientId?}` → add
//! - `DELETE /v1/forges/:id` → remove it and its tokens
//! - `POST   /v1/forges/:id/test` → sign-in check
//! - `PUT    /v1/forges/:id/token {purpose: read|write, token}`
//! - `DELETE /v1/forges/:id/token?purpose=read|write`
//! - `POST   /v1/forges/:id/auth/device` → `{userCode, verificationUri, expiresIn}`
//! - `GET    /v1/forges/:id/auth/device` → `{state, …}`
//!
//! Adding, removing and anything that hands the box a credential need the
//! box's write standing and presence (they give it write access to code).
//! A token, and a device sign-in's user code, travel **only** through the
//! end-to-end encrypted tunnel (`hive-tunnel/1`: Noise between the phone
//! and this box, the relay forwards ciphertext) or on a loopback bind (this
//! machine). A direct LAN bind is plain HTTP, so it is refused there.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use hive_forge::tokens::{AuthSource, Purpose};
use hive_forge::{Connection, Connections, ForgeId};
use serde::Deserialize;
use serde_json::{json, Value};

use super::api::{presence_gate, ApiError, ApiResult};
use super::repos_api::{ensure_can_write, forge_dto, DeviceSession, ForgeHandle};
use super::tunnel::TunnelPeer;
use super::Shared;
use crate::forge_cmd;

type S = State<Arc<Shared>>;

/// Why a credential can't be set over this connection.
pub const PLAINTEXT_REFUSED: &str = "code-host tokens and sign-ins go only through the end-to-end encrypted tunnel \
     (pair the phone with remote access) or on the box itself (ligus forge token); this API is plain HTTP on the network";

/// Whether this request reached the box over a channel a token may travel
/// on: the tunnel (end-to-end encrypted to this box), or a loopback bind.
pub fn secure_channel(s: &Shared, peer: Option<&TunnelPeer>) -> bool {
    token_channel_ok(s.opts.listen, peer.is_some())
}

/// The rule behind [`secure_channel`]: through the tunnel, or a listener
/// bound to loopback only. A LAN, tailnet or wildcard bind is plain HTTP on
/// a network.
pub fn token_channel_ok(listen: std::net::SocketAddr, via_tunnel: bool) -> bool {
    via_tunnel || listen.ip().is_loopback()
}


fn require_secure(s: &Shared, peer: Option<&TunnelPeer>) -> ApiResult<()> {
    if secure_channel(s, peer) {
        Ok(())
    } else {
        Err(ApiError::new(StatusCode::FORBIDDEN, PLAINTEXT_REFUSED))
    }
}

fn parse_id(raw: &str) -> ApiResult<ForgeId> {
    ForgeId::parse(raw).map_err(|e| ApiError::bad_request(e.to_string()))
}

fn handle(s: &Shared, id: &ForgeId) -> ApiResult<ForgeHandle> {
    s.repos.forge(&s.cfg.data_dir, id).ok_or_else(|| ApiError::not_found("code host connection"))
}

fn forge_err(e: hive_forge::ForgeError) -> ApiError {
    use hive_forge::ForgeError as E;
    let code = match &e {
        E::Invalid(_) => StatusCode::BAD_REQUEST,
        E::Unsupported(_) => StatusCode::UNPROCESSABLE_ENTITY,
        E::NotFound(_) => StatusCode::NOT_FOUND,
        E::Store(_) => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::BAD_GATEWAY,
    };
    ApiError::new(code, e.to_string())
}

/// Edit `forges.toml` under the data dir's forges lock (a token refresh in
/// another process saves its expiry under it too) and the repos lock, then
/// reload.
async fn edit_connections<T>(s: &Shared, f: impl FnOnce(&mut Connections) -> ApiResult<T>) -> ApiResult<T> {
    let _forges = hive_forge::conn::ForgesLock::acquire_async(&s.cfg.data_dir).await.map_err(forge_err)?;
    let out = {
        let _g = s.repos.bindings.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = Connections::load(&s.cfg.data_dir);
        let out = f(&mut all)?;
        all.save(&s.cfg.data_dir).map_err(forge_err)?;
        out
    };
    s.repos.invalidate();
    Ok(out)
}

async fn dto(s: &Shared, h: &ForgeHandle) -> Value {
    let st = super::repos_api::forge_status_pub(&s.repos, h).await;
    let mut v = forge_dto(h, Some(&st));
    let present = forge_cmd::token_store(&s.cfg.data_dir).present(h.id());
    v["hasRead"] = json!(h.conn.auth == AuthSource::Gh || present.contains(&Purpose::Read) || present.contains(&Purpose::Write));
    v
}

/// `GET /v1/forges`
pub async fn list(State(s): S, peer: Option<Extension<TunnelPeer>>) -> ApiResult<Json<Value>> {
    let mut out = Vec::new();
    for h in s.repos.forges(&s.cfg.data_dir) {
        out.push(dto(&s, &h).await);
    }
    Ok(Json(json!({ "forges": out, "tokenEntry": secure_channel(&s, peer.as_deref()) })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddReq {
    kind: String,
    base_url: String,
    #[serde(default)]
    auth: Option<String>,
    #[serde(default)]
    client_id: Option<String>,
}

/// `POST /v1/forges`
pub async fn add(State(s): S, peer: Option<Extension<TunnelPeer>>, Json(req): Json<AddReq>) -> ApiResult<Json<Value>> {
    ensure_can_write(&s, None)?;
    presence_gate(&s, peer.as_deref(), "forge_add")?;
    let (kind, flavor) = forge_cmd::parse_kind(&req.kind).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let auth = match req.auth.as_deref().map(str::trim) {
        None | Some("") => {
            if kind == hive_forge::ForgeKind::GitHub && hive_forge::conn::validate_base_url(&req.base_url).ok().and_then(|b| hive_forge::spec::host_of(&b)).as_deref() == Some("github.com") {
                AuthSource::Gh
            } else {
                AuthSource::None
            }
        }
        Some("gh") => AuthSource::Gh,
        Some("token") => AuthSource::Token,
        Some("none") => AuthSource::None,
        Some(o) => return Err(ApiError::bad_request(format!("auth is gh, token or none (got {o}); device sign-in sets itself"))),
    };
    let mut c = Connection::new(kind, &req.base_url, auth).map_err(forge_err)?;
    if flavor.is_some() && c.flavor.is_none() {
        c.flavor = flavor;
    }
    c.client_id = req.client_id.map(|x| x.trim().to_string()).filter(|x| !x.is_empty() && x.len() <= 200 && x.chars().all(|ch| ch.is_ascii_graphic()));
    let id = c.id.clone();
    edit_connections(&s, |all| {
        if all.get(&id).is_some() {
            return Err(ApiError::new(StatusCode::CONFLICT, format!("{id} is already connected")));
        }
        all.upsert(c);
        Ok(())
    })
    .await?;
    let h = handle(&s, &id)?;
    Ok(Json(json!({ "forge": dto(&s, &h).await })))
}

/// `DELETE /v1/forges/:id`
pub async fn remove(State(s): S, peer: Option<Extension<TunnelPeer>>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    ensure_can_write(&s, None)?;
    presence_gate(&s, peer.as_deref(), "forge_remove")?;
    let id = parse_id(&id)?;
    let removed = edit_connections(&s, |all| Ok(all.remove(&id))).await?;
    forge_cmd::token_store(&s.cfg.data_dir).delete_all(&id).map_err(forge_err)?;
    s.repos.device.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    if !removed {
        return Err(ApiError::not_found("code host connection"));
    }
    Ok(Json(json!({ "removed": id.as_str() })))
}

/// `POST /v1/forges/:id/test`: sign in and remember who as.
pub async fn test(State(s): S, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let id = parse_id(&id)?;
    let h = handle(&s, &id)?;
    if !s.repos.online() {
        return Ok(Json(json!({ "ok": false, "error": super::repos_api::OFFLINE })));
    }
    let res = h.forge.whoami().await;
    let v = match res {
        Ok(Some(u)) => {
            let (login, uid) = (u.login.clone(), u.id.clone());
            edit_connections(&s, |all| {
                if let Some(c) = all.get_mut(&id) {
                    c.login = Some(login);
                    c.user_id = Some(uid);
                }
                Ok(())
            })
            .await?;
            json!({ "ok": true, "user": u.login, "userId": u.id })
        }
        Ok(None) => json!({ "ok": false, "error": h.forge.signed_out_reason().unwrap_or_else(|| "no credentials".into()) }),
        Err(e) => json!({ "ok": false, "error": e.to_string() }),
    };
    Ok(Json(v))
}

#[derive(Deserialize)]
pub struct TokenReq {
    purpose: String,
    token: String,
}

/// `PUT /v1/forges/:id/token`: tunnel or loopback only.
pub async fn put_token(State(s): S, peer: Option<Extension<TunnelPeer>>, Path(id): Path<String>, Json(req): Json<TokenReq>) -> ApiResult<Json<Value>> {
    require_secure(&s, peer.as_deref())?;
    ensure_can_write(&s, None)?;
    presence_gate(&s, peer.as_deref(), "forge_token")?;
    let id = parse_id(&id)?;
    let h = handle(&s, &id)?;
    if h.conn.auth == AuthSource::Gh {
        return Err(ApiError::new(StatusCode::CONFLICT, "this connection uses the box's gh login; add a token connection instead"));
    }
    let purpose = Purpose::parse(&req.purpose).ok_or_else(|| ApiError::bad_request("purpose is read or write"))?;
    let token = forge_cmd::clean_token(&req.token).map_err(|e| ApiError::bad_request(e.to_string()))?;
    forge_cmd::token_store(&s.cfg.data_dir).put(&id, purpose, &token).map_err(forge_err)?;
    drop(token);
    edit_connections(&s, |all| {
        if let Some(c) = all.get_mut(&id) {
            if c.auth == AuthSource::None {
                c.auth = AuthSource::Token;
            }
        }
        Ok(())
    })
    .await?;
    let h = handle(&s, &id)?;
    Ok(Json(json!({ "forge": dto(&s, &h).await })))
}

#[derive(Deserialize)]
pub struct PurposeQuery {
    purpose: String,
}

/// `DELETE /v1/forges/:id/token?purpose=`
pub async fn delete_token(State(s): S, peer: Option<Extension<TunnelPeer>>, Path(id): Path<String>, Query(q): Query<PurposeQuery>) -> ApiResult<Json<Value>> {
    ensure_can_write(&s, None)?;
    presence_gate(&s, peer.as_deref(), "forge_token")?;
    let id = parse_id(&id)?;
    let purpose = Purpose::parse(&q.purpose).ok_or_else(|| ApiError::bad_request("purpose is read or write"))?;
    forge_cmd::token_store(&s.cfg.data_dir).delete(&id, purpose).map_err(forge_err)?;
    s.repos.invalidate();
    Ok(Json(json!({ "deleted": purpose.as_str() })))
}

/// `POST /v1/forges/:id/auth/device`: start a device sign-in (only ever on
/// this request: S12). The box polls; the phone opens the URI.
pub async fn device_start(State(s): S, peer: Option<Extension<TunnelPeer>>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    require_secure(&s, peer.as_deref())?;
    ensure_can_write(&s, None)?;
    presence_gate(&s, peer.as_deref(), "forge_token")?;
    let id = parse_id(&id)?;
    let h = handle(&s, &id)?;
    if h.conn.auth == AuthSource::Gh {
        return Err(ApiError::new(StatusCode::CONFLICT, "this connection uses the box's gh login"));
    }
    let ep = hive_forge::device::endpoints(&h.conn).map_err(forge_err)?;
    if !s.repos.online() {
        return Err(ApiError::new(StatusCode::SERVICE_UNAVAILABLE, super::repos_api::OFFLINE));
    }
    let start = hive_forge::device::start(&ep).await.map_err(forge_err)?;
    let session = DeviceSession {
        user_code: start.user_code.clone(),
        verification_uri: start.verification_uri.clone(),
        expires_at: Instant::now() + Duration::from_secs(start.expires_in),
        state: "pending",
        error: None,
    };
    s.repos.device.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), session);
    let shared = s.clone();
    let (poll_id, expires_in) = (id.clone(), start.expires_in);
    tokio::spawn(async move {
        let mut interval = start.interval;
        let deadline = Instant::now() + Duration::from_secs(expires_in);
        let set = |state: &'static str, error: Option<String>| {
            if let Some(d) = shared.repos.device.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&poll_id) {
                d.state = state;
                d.error = error;
            }
        };
        loop {
            tokio::time::sleep(Duration::from_secs(interval)).await;
            if Instant::now() > deadline {
                set("expired", None);
                return;
            }
            match hive_forge::device::poll(&ep, &start.device_code).await {
                Ok(hive_forge::device::Poll::Pending) => {}
                Ok(hive_forge::device::Poll::SlowDown(i)) => interval = i,
                Ok(hive_forge::device::Poll::Denied) => return set("denied", None),
                Ok(hive_forge::device::Poll::Expired) => return set("expired", None),
                Ok(hive_forge::device::Poll::Done(t)) => {
                    let lock = hive_forge::conn::ForgesLock::acquire_async(&shared.cfg.data_dir).await;
                    let r = {
                        let _forges = lock;
                        let _g = shared.repos.bindings.lock().unwrap_or_else(|e| e.into_inner());
                        let mut all = Connections::load(&shared.cfg.data_dir);
                        forge_cmd::save_token_set(&shared.cfg.data_dir, &mut all, &poll_id, &t)
                    };
                    shared.repos.invalidate();
                    return match r {
                        Ok(()) => set("done", None),
                        Err(e) => set("error", Some(e.to_string())),
                    };
                }
                Err(e) => return set("error", Some(e.to_string())),
            }
        }
    });
    Ok(Json(json!({ "userCode": start.user_code, "verificationUri": start.verification_uri, "expiresIn": start.expires_in, "state": "pending" })))
}

/// `GET /v1/forges/:id/auth/device`. The user code redeems the sign-in for
/// whoever approves it first, so it is only read back over a channel a token
/// may travel on (as starting one is).
pub async fn device_status(State(s): S, peer: Option<Extension<TunnelPeer>>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    require_secure(&s, peer.as_deref())?;
    let id = parse_id(&id)?;
    let d = s.repos.device.lock().unwrap_or_else(|e| e.into_inner()).get(&id).cloned().ok_or_else(|| ApiError::not_found("device sign-in"))?;
    let mut v = json!({ "state": d.state, "userCode": d.user_code, "verificationUri": d.verification_uri, "expiresIn": d.expires_at.saturating_duration_since(Instant::now()).as_secs() });
    if let Some(e) = d.error {
        v["error"] = json!(e);
    }
    Ok(Json(v))
}

#[cfg(test)]
mod tests {
    #[test]
    fn tokens_travel_only_through_the_tunnel_or_loopback() {
        let lan: std::net::SocketAddr = "192.168.1.20:8787".parse().unwrap();
        let tailnet: std::net::SocketAddr = "100.101.102.103:8787".parse().unwrap();
        let wildcard: std::net::SocketAddr = "0.0.0.0:8787".parse().unwrap();
        for a in [lan, tailnet, wildcard] {
            assert!(!super::token_channel_ok(a, false), "{a}: plain HTTP on a network");
            assert!(super::token_channel_ok(a, true), "{a}: through the tunnel");
        }
        assert!(super::token_channel_ok("127.0.0.1:8787".parse().unwrap(), false));
        assert!(super::token_channel_ok("[::1]:8787".parse().unwrap(), false));
    }
}
