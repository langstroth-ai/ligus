//! Identity for the phone (IDENTITY-AND-FORGES §2.8, §3, §4, §6.3): the
//! box's safety code, people's safety codes and linked-identity chips,
//! signed invites, contacts and direct messages. Contract: `docs/ios-app.md`.
//!
//! - `GET  /v1/profile` → this box's account, safety code and what it can do
//! - `GET  /v1/people/{account}/links` → that person's linked-identity chips
//!   (cached statuses only: this route never asks a forge)
//! - `POST /v1/invites` → a single-use, founder-signed invite link to the
//!   box's workspace (Admin; presence where the host needs it)
//! - `POST /v1/invites/inspect {code}` → who an invite is from (no state)
//! - `POST /v1/invites/join {code, confirmSafetyCode}` → join a DM invite
//! - `GET  /v1/invites/pending` → invites in the box's contact drop box
//! - `POST /v1/invites/pending/accept {relay, seq, confirmSafetyCode}`
//! - `POST /v1/invites/pending/dismiss {relay, seq}`
//! - `GET  /v1/dms` → the box's direct messages
//! - `POST /v1/dms {accountId}` → open (founding on first use) a DM
//! - `GET  /v1/chats/{id}/founder` → whether sending waits for the founder
//!
//! An invite code carries the workspace passphrase and its epoch keys (a
//! DM's room key too), so codes travel only over a secure channel: the
//! end-to-end encrypted tunnel or a loopback bind ([`require_secret_channel`]).
//! Over plain HTTP on a network bind, `POST /v1/invites`, presence for an
//! invite, `inspect`, `join` and founding a DM are refused with
//! [`SECRET_PLAINTEXT_REFUSED`]. Accepting a pending invite moves no code
//! (it goes by reference) and stays open there.
//!
//! Every name is cleaned (`ligus_proto::names`). A co-member's name and
//! safety code in an invite count as theirs only with their device's
//! signature (R1); an unsigned DM invite is refused.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use ligus_core::workspace_config_session_id;
use ligus_runtime::directory_trust::{self, CoMember};
use ligus_runtime::dm::{self, FounderTrust};
use ligus_runtime::dm_delivery::Delivery;
use ligus_runtime::envelope_verifier::FounderPin;
use ligus_runtime::identity_links::LinkCheckCache;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::api::{iso, ApiError, ApiResult};
use super::contacts_api;
use super::dms::{self, DmConn};
use super::invite_code::{random_hex, trust_str, InviteConn};
use super::tunnel::TunnelPeer;
use super::Shared;

type S = State<Arc<Shared>>;

/// Codes read from the drop box at the last listing, by (relay, seq): the
/// phone accepts by reference, so another workspace's secrets never travel
/// to it.
#[derive(Default)]
pub struct PendingCodes(Mutex<HashMap<(String, u64), String>>);

impl PendingCodes {
    fn replace(&self, list: &[contacts_api::PendingInvite]) {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.clear();
        for p in list {
            m.insert((p.relay.clone(), p.seq), p.code.clone());
        }
    }
    fn get(&self, relay: &str, seq: u64) -> Option<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).get(&(relay.to_string(), seq)).cloned()
    }
    fn remove(&self, relay: &str, seq: u64) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(&(relay.to_string(), seq));
    }
}

// ── Who this box is, and who it shares workspaces with ─────────────────────

impl Shared {
    /// The account this box writes as (its own, or its owner's).
    pub fn my_account(&self) -> Option<Uuid> {
        self.lock().author().account_id
    }

    /// Everyone this box shares a pinned workspace with (its workspace and
    /// its DMs), keyed by account id, with keys verified on those logs.
    pub fn co_members(&self) -> Vec<CoMember> {
        let svc = self.lock();
        let me = svc.author().account_id.unwrap_or_default();
        directory_trust::co_members(svc.store(), me)
    }

    /// This box's account fingerprint: its own key's when it holds it;
    /// on a box acting for its owner, the key the shared logs trust for the
    /// owner.
    pub fn my_fingerprint(&self) -> Option<String> {
        let svc = self.lock();
        if !svc.is_delegated() {
            return Some(FounderPin::fingerprint_hex(&svc.account_public_key()));
        }
        let me = svc.author().account_id?;
        let logs = directory_trust::pinned_logs(svc.store());
        match directory_trust::account_keys(&logs).get(&me) {
            Some(directory_trust::AccountKey::Trusted(fk)) => Some(fk.clone()),
            _ => None,
        }
    }

    /// Whether this box already holds `room` (its workspace or a DM).
    pub fn holds_room(&self, room: &str) -> bool {
        room == self.cfg.room || self.dms.holds_room(room)
    }

    /// The DM rule for `conn` (`dm::accept_dm_invite`): the other person's
    /// account, or why not. `confirmed`: the person compared a stranger's
    /// safety code.
    pub fn dm_refusal(&self, conn: &InviteConn, _trust: FounderTrust, confirmed: bool) -> Result<Uuid, String> {
        let me = self.my_account().ok_or("this box has no account yet")?;
        let people = self.co_members();
        let known = dm::known_fingerprints(&people);
        let signer = conn.signed_by(&|d| people.iter().any(|p| p.revoked_devices.contains(&d)));
        let peer = dm::accept_dm_invite(
            conn.dm_account.as_deref(),
            conn.founder_actor_id.as_deref(),
            conn.fk.as_deref(),
            me,
            &known,
            signer,
            confirmed,
            self.holds_room(&conn.room),
        )?;
        if !dms::dm_relay_allowed(self.cfg.relay_url.as_deref(), &conn.relay_url) {
            return Err("the invite names a relay this box won't use (only its own relay, or a public https one)".into());
        }
        if conn.key.as_deref().is_none_or(|k| k.trim().is_empty()) {
            return Err("the invite carries no key for the conversation".into());
        }
        Ok(peer)
    }
}

fn safety(fk: Option<&str>) -> Option<String> {
    fk.and_then(directory_trust::safety_code)
}

// ── GET /v1/profile ────────────────────────────────────────────────────────

pub async fn profile(State(s): S) -> ApiResult<Json<Value>> {
    let (account, name, avatar, delegated) = {
        let svc = s.lock();
        let a = svc.author();
        (a.account_id, ligus_proto::names::clean_display_name(&a.display_name), a.avatar_url.clone(), svc.is_delegated())
    };
    let fk = s.my_fingerprint();
    let mut v = json!({
        "accountId": account.map(|a| a.to_string()).unwrap_or_default(),
        "displayName": name,
        "holdsAccountKey": !delegated,
        // A DM is founded by a device that holds its account's key.
        "canFoundDm": !delegated && account.is_some(),
        "contacts": s.contact_status(),
    });
    if let Some(code) = safety(fk.as_deref()) {
        v["safetyCode"] = json!(code);
    }
    if let Some(u) = avatar.filter(|u| u.trim_start().get(..11).is_some_and(|p| p.eq_ignore_ascii_case("data:image/"))) {
        v["avatarUrl"] = json!(u);
    }
    Ok(Json(v))
}

impl Shared {
    /// The last contacts status this box saw (`on`, `unsupported`,
    /// `noRelay`, `delegated`; `unknown` before the first check).
    pub fn contact_status(&self) -> String {
        self.contact_state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_contact_status(&self, status: &str) {
        *self.contact_state.lock().unwrap_or_else(|e| e.into_inner()) = status.to_string();
    }
}

// ── People: safety codes, look-alikes, links ───────────────────────────────

/// What `GET /v1/people` adds to each person, by account id: the safety
/// code from the shared logs, whether the logs disagree about their key,
/// whether their name looks like another person's, how many devices are
/// verified, their link chips, and the DM with them.
pub fn person_extras(s: &Shared) -> HashMap<Uuid, Value> {
    let people = s.co_members();
    let me_name = {
        let svc = s.lock();
        ligus_proto::names::clean_display_name(&svc.author().display_name)
    };
    let my_skeleton = ligus_proto::names::name_skeleton(&me_name);
    let dms = s.dm_rows(&people);
    let mut out = HashMap::new();
    for p in &people {
        // Desktop's flag (two co-members alike), and one more: a co-member
        // whose name looks like this box's own (someone posing as you).
        let looks_like_me = !my_skeleton.is_empty() && ligus_proto::names::name_skeleton(&p.display_name) == my_skeleton;
        let mut v = json!({
            "accountId": p.account_id.to_string(),
            "keyConflict": p.key_conflict,
            "lookAlike": p.look_alike || looks_like_me,
            "devices": p.devices.len(),
        });
        if let Some(code) = p.safety_code().filter(|_| !p.key_conflict) {
            v["safetyCode"] = json!(code);
        }
        let chips = link_chips(s, p.account_id);
        if !chips.is_empty() {
            v["links"] = json!(chips);
        }
        if let Some(dm) = dms.iter().find(|d| d["account"] == json!(p.account_id.to_string()) && d["archived"] == json!(false)) {
            v["dmChatId"] = dm["chatId"].clone();
            v["dmWorkspaceId"] = dm["workspaceId"].clone();
        }
        out.insert(p.account_id, v);
    }
    out
}

/// A person's linked identities in the box's workspace, each with the
/// status this box cached (never a request to a forge): from the box's
/// workspace only, and only under the account key the box agrees on for
/// them (its own for its account; the co-member directory's for anyone
/// else).
pub fn link_chips(s: &Shared, account: Uuid) -> Vec<Value> {
    let svc = s.lock();
    let Ok(wl) = svc.identity_links(s.ws) else { return Vec::new() };
    let me = svc.author().account_id;
    let agreed: Option<Vec<u8>> = if me == Some(account) && !svc.is_delegated() {
        Some(svc.account_public_key().to_vec())
    } else {
        let fp = directory_trust::co_members(svc.store(), me.unwrap_or_default())
            .into_iter()
            .find(|c| c.account_id == account)
            .filter(|c| !c.key_conflict)
            .and_then(|c| c.fingerprint);
        let key = wl.account_keys.get(&account).cloned();
        match (fp, key) {
            (Some(fp), Some(k)) if FounderPin::fingerprint_hex(&k).eq_ignore_ascii_case(&fp) => Some(k),
            (None, Some(k)) if me == Some(account) => Some(k),
            _ => None,
        }
    };
    let Some(agreed) = agreed else { return Vec::new() };
    let now = ligus_core::Timestamp::now();
    let per_ws = vec![(s.ws, wl)];
    ligus_runtime::chat_service::links_for_account(&per_ws, account, |k| k == agreed.as_slice())
        .into_iter()
        .filter_map(|shown| {
            let l = shown.link;
            let ws = shown.workspaces[0];
            let ctx = ligus_runtime::identity_links::LinkContext { workspace_id: ws, account_key: Some(&shown.account_key) };
            let cached = svc.store().get_check(&ligus_runtime::identity_links::cache_key(&l, ws, &shown.account_key));
            let status = ligus_runtime::identity_links::cached_status(cached.as_ref(), &l, ctx, now, true);
            if matches!(status, Some(ligus_runtime::identity_links::LinkStatus::Invalid { .. })) {
                return None;
            }
            let mut status = serde_json::to_value(&status).unwrap_or(Value::Null);
            // A forge-confirmed handle is the forge's text: shown cleaned.
            if let Some(h) = status.get("handle").and_then(Value::as_str).map(clean_handle) {
                status["handle"] = json!(h);
            }
            if let Some(r) = status.get("reason").and_then(Value::as_str).map(|r| ligus_proto::names::clean_display_name(r)) {
                status["reason"] = json!(r);
            }
            Some(json!({
                "provider": l.provider,
                "host": l.host,
                "handle": clean_handle(&l.handle_at_link),
                "subjectId": clean_handle(&l.subject_id),
                "status": status,
            }))
        })
        .collect()
}

/// A handle or id as shown: hidden and control characters removed (the
/// fold refuses them in links; a forge's answer is cleaned here).
fn clean_handle(h: &str) -> String {
    h.chars().filter(|c| !ligus_proto::names::is_hidden_name_char(*c) && !c.is_whitespace()).collect()
}

/// `GET /v1/people/{account}/links` → `{links: [...]}`.
pub async fn person_links(State(s): S, Path(account): Path<String>) -> ApiResult<Json<Value>> {
    let account = Uuid::parse_str(account.trim()).map_err(|_| ApiError::not_found("person"))?;
    let known = s.my_account() == Some(account) || s.co_members().iter().any(|c| c.account_id == account);
    if !known {
        return Err(ApiError::not_found("person"));
    }
    Ok(Json(json!({ "links": link_chips(&s, account) })))
}

// ── Invites ────────────────────────────────────────────────────────────────

/// Why an invite can't travel over this connection (shown on the phone).
pub const SECRET_PLAINTEXT_REFUSED: &str = "Invite links carry the workspace's keys, so the host sends and takes them \
     only over the secure connection: open this over remote access (the end-to-end encrypted tunnel) or on the host \
     itself. This connection is plain HTTP on the network.";

/// Invite codes go only through the tunnel or on a loopback bind (the same
/// rule as code-host tokens: [`super::forges_api::secure_channel`]). The API
/// speaks plain HTTP; https in front of it terminates on this machine only
/// when the box listens on loopback.
pub fn require_secret_channel(s: &Shared, peer: Option<&TunnelPeer>) -> ApiResult<()> {
    if super::forges_api::secure_channel(s, peer) {
        Ok(())
    } else {
        Err(ApiError::new(StatusCode::FORBIDDEN, SECRET_PLAINTEXT_REFUSED))
    }
}

fn ensure_can_write(s: &Shared) -> ApiResult<()> {
    if let Some(reason) = s.lock().write_block() {
        return Err(ApiError::new(StatusCode::FORBIDDEN, reason.to_string()));
    }
    Ok(())
}

/// The workspace's name in an invite: the config log's title, else the room.
pub(super) fn workspace_name(s: &Shared) -> String {
    let svc = s.lock();
    svc.load_in(s.ws, workspace_config_session_id(s.ws))
        .ok()
        .flatten()
        .map(|c| ligus_runtime::display::workspace_name(&c.title))
        .filter(|t| !t.is_empty())
        .unwrap_or_default()
}

/// Build a signed invite for `conn` with `inv` and the epoch keys this box
/// holds for it. Signed only when this box is the founder's device (R1).
pub fn finish_invite(s: &Shared, mut conn: InviteConn, inv: Uuid, cfg: &crate::Config) -> String {
    conn.inv = Some(inv.to_string());
    conn.sig = None;
    if let Ok(src) = crate::key_source(cfg) {
        if let Ok(keys) = s.lock().delivered_epoch_keys(conn.workspace_id(), &src) {
            conn.ek = keys;
        }
    }
    let identity = ligus_runtime::IdentityStore::new(&s.cfg.data_dir, ligus_runtime::secret_store::StoreKeyVault::for_data_dir(&s.cfg.data_dir));
    if let Some(stored) = identity.load().ok().flatten() {
        let sig = {
            let svc = s.lock();
            conn.with_claims(|c| svc.sign_invite(c, &stored.device.certificate))
        };
        conn.sig = sig;
    }
    let code = conn.encode();
    // The invite's redeem key seed, kept when it was issued: only the code's
    // holder can redeem it (`ligus_core::admission`).
    match s.lock().take_invite_redeem_seed(conn.workspace_id(), inv).ok().flatten() {
        Some(seed) => ligus_runtime::box_admission::with_redeem_key(&code, &seed).unwrap_or(code),
        None => code,
    }
}

/// The box's workspace as an invite names it (no `inv` yet).
fn workspace_conn(s: &Shared) -> ApiResult<InviteConn> {
    let relay = s.cfg.relay_url.clone().ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "this box has no relay, so nobody could join"))?;
    let pin = s.lock().store().workspace_pin(s.ws).map_err(|e| ApiError::from(anyhow::Error::from(e)))?;
    Ok(InviteConn {
        name: workspace_name(s),
        relay_url: relay,
        room: s.cfg.room.clone(),
        key: s.cfg.key.clone(),
        icon: None,
        dm_account: None,
        founder_actor_id: pin.as_ref().map(|p| p.founder_account.to_string()),
        fk: pin.as_ref().map(|p| p.fk.iter().map(|b| format!("{b:02x}")).collect()),
        inv: None,
        ek: Vec::new(),
        sig: None,
    })
}

/// The workspace's name for the passkey sheet's preview.
pub fn invite_preview_name(s: &Shared) -> String {
    let n = workspace_name(s);
    if n.is_empty() { s.cfg.room.clone() } else { n }
}

/// The JSON for a fresh workspace invite.
pub fn invite_json(s: &Shared, code: &str) -> Value {
    let conn = InviteConn::decode(code).ok();
    json!({
        "code": code,
        "signed": conn.as_ref().is_some_and(|c| c.sig.is_some()),
        "safetyCode": conn.as_ref().and_then(|c| safety(c.fk.as_deref())),
        "workspaceName": conn.as_ref().map(|c| ligus_proto::names::clean_display_name(&c.display_name())).unwrap_or_default(),
        "relay": s.cfg.relay_url.clone().unwrap_or_default(),
    })
}

/// `POST /v1/invites` → `{code, signed, safetyCode, workspaceName}`: a
/// single-use invite to the box's workspace (an Admin's `JoinApproved` of
/// `invite:<id>`). On a box acting for its owner the phone's request would
/// pass for the owner's own: it needs presence (`presence_required`; the
/// passkey path is `POST /v1/presence/prepare {action: "invite"}`), as it
/// does on any workspace running rules v2.
pub async fn create_invite(State(s): S, peer: Option<Extension<TunnelPeer>>) -> ApiResult<Json<Value>> {
    require_secret_channel(&s, peer.as_deref())?;
    ensure_can_write(&s)?;
    super::api::presence_gate(&s, peer.as_deref(), "invite")?;
    let conn = workspace_conn(&s)?;
    let inv = { s.lock().issue_invite(s.ws) }?;
    s.after_write();
    let code = finish_invite(&s, conn, inv, &s.cfg);
    Ok(Json(invite_json(&s, &code)))
}

/// After a passkey-confirmed `JoinApproved` (presence commit): the invite
/// code for it, when it is an invite to the box's workspace.
pub fn invite_after_commit(s: &Shared, env: &ligus_core::SessionEventEnvelope) -> Option<Value> {
    let ligus_core::SessionEvent::JoinApproved { subject, .. } = &env.payload else { return None };
    if env.workspace_id != s.ws {
        return None;
    }
    let inv = subject.strip_prefix("invite:").and_then(|i| Uuid::parse_str(i).ok())?;
    let conn = workspace_conn(s).ok()?;
    let code = finish_invite(s, conn, inv, &s.cfg);
    Some(invite_json(s, &code))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeReq {
    code: String,
    #[serde(default)]
    confirm_safety_code: bool,
}

fn inspect_json(s: &Shared, conn: &InviteConn) -> Value {
    let people = s.co_members();
    let trust = conn.trust(&people);
    let me = s.my_account();
    let founder = conn.founder_actor_id.as_deref().and_then(|f| Uuid::parse_str(f.trim()).ok());
    let addressed = me.is_some_and(|m| conn.dm_account.as_deref().map(str::trim) == Some(m.to_string().as_str()));
    let (acceptable, refusal) = if conn.is_dm() {
        match s.dm_refusal(conn, trust, true) {
            Ok(_) => (true, None),
            Err(e) => (false, Some(e)),
        }
    } else if conn.room == s.cfg.room {
        (false, Some("This box is already in that workspace.".to_string()))
    } else {
        (false, Some("This is an invite to another workspace. A box serves one workspace: open it on your desktop.".to_string()))
    };
    json!({
        "workspaceName": if conn.is_dm() { String::new() } else { ligus_proto::names::clean_display_name(&conn.display_name()) },
        "isDm": conn.is_dm(),
        "addressedToMe": addressed,
        "founderAccount": founder.map(|f| f.to_string()),
        "founderName": founder.and_then(|f| people.iter().find(|p| p.account_id == f)).map(|p| p.display_name.clone()),
        "safetyCode": safety(conn.fk.as_deref()),
        "trust": trust_str(trust),
        "alreadyJoined": s.holds_room(&conn.room),
        "acceptable": acceptable,
        "refusal": refusal,
    })
}

/// `POST /v1/invites/inspect {code}` → who the invite says it's from, and
/// whether this box can take it. Nothing is stored.
pub async fn inspect(State(s): S, peer: Option<Extension<TunnelPeer>>, Json(req): Json<CodeReq>) -> ApiResult<Json<Value>> {
    require_secret_channel(&s, peer.as_deref())?;
    let conn = InviteConn::decode(&req.code).map_err(ApiError::bad_request)?;
    Ok(Json(inspect_json(&s, &conn)))
}

/// Join a DM from its (checked) invite: pin its founder, keep the invite to
/// redeem, save it, and start syncing. Returns the DM's JSON.
fn join_dm(s: &Shared, conn: InviteConn, redeem: Option<[u8; 32]>, confirmed: bool) -> ApiResult<Value> {
    ensure_can_write(s)?;
    let trust = conn.trust(&s.co_members());
    let peer = s.dm_refusal(&conn, trust, confirmed).map_err(|e| ApiError::new(StatusCode::FORBIDDEN, e))?;
    let ws = conn.workspace_id();
    let (founder, fk) = (conn.founder_actor_id.clone().unwrap_or_default(), conn.fk.clone().unwrap_or_default().to_ascii_lowercase());
    {
        let svc = s.lock();
        let store = svc.store();
        if let Some(inv) = conn.inv.as_deref().and_then(|i| Uuid::parse_str(i.trim()).ok()) {
            store.set_join_invite(ws, inv).map_err(|e| ApiError::from(anyhow::Error::from(e)))?;
            store.set_join_redeem(ws, redeem.as_ref()).map_err(|e| ApiError::from(anyhow::Error::from(e)))?;
        }
        store.set_workspace_pin(ws, &founder, &fk).map_err(|e| ApiError::from(anyhow::Error::from(e)))?;
    }
    let name = s
        .co_members()
        .into_iter()
        .find(|p| p.account_id == peer)
        .map(|p| p.display_name)
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| "Direct message".into());
    let dm = DmConn { name, relay_url: conn.relay_url.trim().to_string(), room: conn.room.clone(), peer: peer.to_string(), founder_actor_id: founder, fk };
    s.dms
        .add(dm, conn.key.as_deref().unwrap_or_default(), &conn.ek)
        .map_err(|e| ApiError::new(StatusCode::CONFLICT, e.to_string()))?;
    s.dm_kick.notify_one();
    s.hub.emit("dms.changed", json!({}));
    Ok(json!({ "workspaceId": ws, "account": peer.to_string() }))
}

/// `POST /v1/invites/join {code, confirmSafetyCode}`: a DM invite only
/// (a box serves one workspace).
pub async fn join(State(s): S, peer: Option<Extension<TunnelPeer>>, Json(req): Json<CodeReq>) -> ApiResult<Json<Value>> {
    require_secret_channel(&s, peer.as_deref())?;
    let conn = InviteConn::decode(&req.code).map_err(ApiError::bad_request)?;
    if !conn.is_dm() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "This is an invite to another workspace. A box serves one workspace: open it on your desktop.",
        ));
    }
    dms::foreign_relay_check(&s.cfg, &conn.relay_url).await.map_err(|e| ApiError::new(StatusCode::FORBIDDEN, e))?;
    Ok(Json(join_dm(&s, conn, ligus_runtime::box_admission::redeem_key_of(&req.code), req.confirm_safety_code)?))
}

/// `GET /v1/invites/pending` → `{contacts, invites}` (codes stay on the box).
pub async fn pending(State(s): S) -> ApiResult<Json<Value>> {
    let (status, list) = contacts_api::pending(&s).await;
    s.set_contact_status(&status);
    s.pending_codes.replace(&list);
    let shown: Vec<Value> = list
        .iter()
        .map(|p| {
            let mut v = serde_json::to_value(p).unwrap_or(Value::Null);
            if let Some(o) = v.as_object_mut() {
                o.remove("code");
            }
            v
        })
        .collect();
    Ok(Json(json!({ "contacts": status, "invites": shown })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DropRef {
    relay: String,
    seq: u64,
    #[serde(default)]
    confirm_safety_code: bool,
}

/// `POST /v1/invites/pending/accept {relay, seq, confirmSafetyCode}`.
pub async fn accept_pending(State(s): S, Json(req): Json<DropRef>) -> ApiResult<Json<Value>> {
    let code = s.pending_codes.get(&req.relay, req.seq).ok_or_else(|| ApiError::not_found("pending invite (list them again)"))?;
    let conn = InviteConn::decode(&code).map_err(ApiError::bad_request)?;
    if !conn.is_dm() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "This is an invite to another workspace. A box serves one workspace: open it on your desktop.",
        ));
    }
    dms::foreign_relay_check(&s.cfg, &conn.relay_url).await.map_err(|e| ApiError::new(StatusCode::FORBIDDEN, e))?;
    let out = join_dm(&s, conn, ligus_runtime::box_admission::redeem_key_of(&code), req.confirm_safety_code)?;
    contacts_api::accepted(&s, &req.relay, req.seq);
    s.pending_codes.remove(&req.relay, req.seq);
    Ok(Json(out))
}

/// `POST /v1/invites/pending/dismiss {relay, seq}`.
pub async fn dismiss_pending(State(s): S, Json(req): Json<DropRef>) -> ApiResult<Json<Value>> {
    if s.pending_codes.get(&req.relay, req.seq).is_none() {
        return Err(ApiError::not_found("pending invite (list them again)"));
    }
    contacts_api::dismiss(&s, &req.relay, req.seq);
    s.pending_codes.remove(&req.relay, req.seq);
    Ok(Json(json!({ "dismissed": true })))
}

// ── Direct messages ────────────────────────────────────────────────────────

impl Shared {
    /// One row per DM: who it is with (from the shared logs), its chat, and
    /// whether it waits for its founder.
    pub fn dm_rows(&self, people: &[CoMember]) -> Vec<Value> {
        let archived = self.dms.archived();
        let mut rows = Vec::new();
        for d in self.dms.list() {
            let ws = d.workspace_id();
            let (chat, waiting, founded) = {
                let svc = self.lock();
                let chat = super::chats_of(&svc, ws).ok().and_then(|mut l| {
                    l.sort_by_key(|c| c.created_at);
                    l.into_iter().next()
                });
                (chat, svc.waiting_for_founder(ws).ok().flatten(), svc.store().config_founded_at(ws).ok().flatten())
            };
            let peer = Uuid::parse_str(&d.peer).ok();
            let person = peer.and_then(|p| people.iter().find(|c| c.account_id == p));
            let name = person
                .map(|p| p.display_name.clone())
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| ligus_proto::names::clean_display_name(&d.name));
            let mut v = json!({
                "workspaceId": ws,
                "account": d.peer,
                "name": if name.is_empty() { "Direct message".to_string() } else { name },
                "archived": archived.contains(&d.room),
                "waitingForFounder": waiting.is_some(),
                "foundedHere": peer.is_some() && Uuid::parse_str(&d.founder_actor_id).ok() != peer,
            });
            if let Some(c) = chat {
                v["chatId"] = json!(c.id);
                v["updatedAt"] = json!(iso(c.messages.iter().rev().find(|m| !m.superseded).map(|m| m.created_at).unwrap_or(c.updated_at)));
            }
            if let Some(t) = founded {
                v["foundedAt"] = json!(iso(t));
            }
            if let Some(h) = self.held.get(ws) {
                v["inviteDelivery"] = h.status_json();
            }
            if let Some(p) = person {
                if let Some(code) = p.safety_code().filter(|_| !p.key_conflict) {
                    v["safetyCode"] = json!(code);
                }
                if let Some(u) = p.avatar_url.as_ref().filter(|u| u.trim_start().get(..11).is_some_and(|x| x.eq_ignore_ascii_case("data:image/"))) {
                    v["avatarUrl"] = json!(u);
                }
            }
            rows.push(v);
        }
        rows
    }
}

/// `GET /v1/dms` → `{dms: [...], canFound}`.
pub async fn list_dms(State(s): S) -> ApiResult<Json<Value>> {
    s.resolve_concurrent_dms();
    let people = s.co_members();
    let rows = s.dm_rows(&people);
    let can_found = !s.lock().is_delegated();
    Ok(Json(json!({ "dms": rows, "canFound": can_found })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenDm {
    account_id: String,
}

/// `POST /v1/dms {accountId}` → `{workspaceId, chatId?, invite?, delivered,
/// pending, inviteDelivery?, safetyCode}`. `pending`: not delivered yet
/// (their contact id isn't known here), held sealed on the box and
/// delivered once it is. Opens the kept DM with a co-member, or founds one: a
/// random room and key, this box's account as founder (pinned), a signed
/// invite addressed to them, sealed to their verified devices and dropped
/// into their contact box when they have one.
pub async fn open_dm(State(s): S, peer_ext: Option<Extension<TunnelPeer>>, Json(req): Json<OpenDm>) -> ApiResult<Json<Value>> {
    ensure_can_write(&s)?;
    let peer = Uuid::parse_str(req.account_id.trim()).map_err(|_| ApiError::bad_request("not an account id"))?;
    let me = s.my_account().ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "this box has no account yet"))?;
    if peer == me {
        return Err(ApiError::bad_request("That's you."));
    }
    let person = s.co_members().into_iter().find(|c| c.account_id == peer).ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "You can open a DM with someone you share a workspace with. Otherwise send them an invite link.",
        )
    })?;
    person.seal_plan().map_err(|e| ApiError::new(StatusCode::CONFLICT, e))?;
    // "Message <peer>" lands only in a DM whose founding verifies (theirs
    // or this box's), never in one that merely names them.
    s.resolve_concurrent_dms();
    let existing = {
        let svc = s.lock();
        s.dms
            .list()
            .into_iter()
            .filter(|d| d.peer == peer.to_string() && !s.dms.archived().contains(&d.room))
            .find(|d| svc.founder_verified(d.workspace_id()).unwrap_or(false) && svc.store().workspace_pin(d.workspace_id()).ok().flatten().is_some())
    };
    if let Some(d) = existing {
        let rows = s.dm_rows(&[person.clone()]);
        let row = rows.into_iter().find(|r| r["workspaceId"] == json!(d.workspace_id())).unwrap_or(Value::Null);
        let held = s.held.get(d.workspace_id());
        return Ok(Json(json!({
            "workspaceId": d.workspace_id(),
            "chatId": row["chatId"].clone(),
            "delivered": false,
            "pending": held.as_ref().is_some_and(|h| h.status_json()["pending"] == json!(true)),
            "inviteDelivery": held.map(|h| h.status_json()),
            "safetyCode": person.safety_code(),
        })));
    }
    // Founding returns the DM's invite (its room key): secure channel only.
    require_secret_channel(&s, peer_ext.as_deref())?;
    if s.lock().is_delegated() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "This box acts for its owner without the owner's account key, so it can't start a DM. Start it on your desktop.",
        ));
    }
    let relay = s.cfg.relay_url.clone().ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "this box has no relay"))?;
    let my_fk = s.my_fingerprint().ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "this box has no account key"))?;
    let key = random_hex(24).map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("no OS randomness: {e}")))?;
    let conn = InviteConn {
        name: person.display_name.clone(),
        relay_url: relay.clone(),
        room: dm::new_room(),
        key: Some(key.clone()),
        icon: None,
        dm_account: Some(peer.to_string()),
        founder_actor_id: Some(me.to_string()),
        fk: Some(my_fk.clone()),
        inv: None,
        ek: Vec::new(),
        sig: None,
    };
    let ws = conn.workspace_id();
    let dm_conn = DmConn {
        name: person.display_name.clone(),
        relay_url: relay,
        room: conn.room.clone(),
        peer: peer.to_string(),
        founder_actor_id: me.to_string(),
        fk: my_fk.clone(),
    };
    s.dms.add(dm_conn.clone(), &key, &[]).map_err(|e| ApiError::new(StatusCode::CONFLICT, e.to_string()))?;
    // Found it: pin this account, seed the roster as Owner, a #general with
    // its chat, this device's key-agreement key, and the single-use invite.
    let inv = {
        let mut svc = s.lock();
        svc.store().set_workspace_pin(ws, &me.to_string(), &my_fk).map_err(|e| ApiError::from(anyhow::Error::from(e)))?;
        svc.ensure_self_member(workspace_config_session_id(ws), ws)?;
        svc.ensure_default_channel(ws, "")?;
        drop(svc);
        s.settle_dm(&dm_conn);
        s.lock().issue_invite(ws)?
    };
    let dm_cfg = s.dms.config_for(&s.cfg, &dm_conn);
    let invite = finish_invite(&s, conn, inv, &dm_cfg);
    // Delivered now, or held (sealed on this box) and delivered once their
    // contact id arrives.
    let plan = person.seal_plan().map_err(|e| ApiError::new(StatusCode::CONFLICT, e))?;
    let delivered = contacts_api::deliver_invite(&s, &person, &plan, &invite).await == Delivery::Delivered;
    let held = if delivered { None } else { contacts_api::hold_invite(&s, ws, &person, &plan, &invite) };
    s.dm_kick.notify_one();
    s.refresh();
    s.hub.emit("dms.changed", json!({}));
    let chat = {
        let svc = s.lock();
        super::chats_of(&svc, ws).ok().and_then(|l| l.into_iter().min_by_key(|c| c.created_at)).map(|c| c.id)
    };
    Ok(Json(json!({
        "workspaceId": ws,
        "chatId": chat,
        "invite": invite,
        "delivered": delivered,
        "pending": held.is_some(),
        "inviteDelivery": held.map(|h| h.status_json()),
        "safetyCode": person.safety_code(),
    })))
}

fn held_ws(s: &Shared, id: &str) -> ApiResult<Uuid> {
    let ws = Uuid::parse_str(id.trim()).map_err(|_| ApiError::bad_request("not a workspace id"))?;
    if s.held.get(ws).is_none() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "no invite waits for delivery in this DM"));
    }
    Ok(ws)
}

/// `GET /v1/dms/{workspaceId}/invite` → `{invite, inviteDelivery}`: the held
/// invite's link, to share by hand while it waits (secure channel only).
pub async fn held_invite(State(s): S, peer_ext: Option<Extension<TunnelPeer>>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    require_secret_channel(&s, peer_ext.as_deref())?;
    let ws = held_ws(&s, &id)?;
    let ka = contacts_api::own_ka(&s).ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "this box has no key-agreement key"))?;
    let invite = s
        .held
        .open_invite(ws, &ka)
        .ok_or_else(|| ApiError::new(StatusCode::GONE, "the invite no longer waits here (delivered, joined or expired)"))?;
    Ok(Json(json!({ "invite": invite.as_str(), "inviteDelivery": s.held.get(ws).map(|h| h.status_json()) })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmHeld {
    /// The safety code the opener compared with them.
    safety_code: String,
}

/// `POST /v1/dms/{workspaceId}/invite/confirm {safetyCode}` →
/// `{inviteDelivery}`: after the unverified-recipient warning (their keys
/// changed since the DM was opened), deliver to their current verified
/// devices. 409 unless `safetyCode` is theirs now (it changed again since
/// it was shown).
pub async fn confirm_held_invite(State(s): S, Path(id): Path<String>, Json(req): Json<ConfirmHeld>) -> ApiResult<Json<Value>> {
    ensure_can_write(&s)?;
    let ws = held_ws(&s, &id)?;
    let h = s.held.get(ws).expect("checked");
    let person = s
        .co_members()
        .into_iter()
        .find(|c| c.account_id == h.account_id)
        .ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "you no longer share a workspace with them"))?;
    let h = s.held.confirm(ws, &person, &req.safety_code, contacts_api::unix_now()).map_err(|e| ApiError::new(StatusCode::CONFLICT, e))?;
    s.held_kick.notify_one();
    s.hub.emit("dms.changed", json!({}));
    Ok(Json(json!({ "inviteDelivery": h.status_json() })))
}

/// `DELETE /v1/dms/{workspaceId}/invite` → `{inviteDelivery}`: stop
/// waiting (share the link instead). The sealed invite is forgotten.
pub async fn dismiss_held_invite(State(s): S, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    ensure_can_write(&s)?;
    let ws = held_ws(&s, &id)?;
    s.held.dismiss(ws);
    s.hub.emit("dms.changed", json!({}));
    Ok(Json(json!({ "inviteDelivery": s.held.get(ws).map(|h| h.status_json()) })))
}

// ── Waiting for the founder ────────────────────────────────────────────────

/// Whether writes into `ws` wait for its pinned founder, and their name
/// when they are a co-member.
pub fn founder_wait(s: &Shared, ws: Uuid) -> (bool, Option<String>) {
    let waiting = s.lock().waiting_for_founder(ws).ok().flatten();
    let name = waiting.and_then(|f| s.co_members().into_iter().find(|c| c.account_id == f)).map(|c| c.display_name);
    (waiting.is_some(), name)
}

/// `GET /v1/chats/{id}/founder` → `{waiting, founderName}`.
pub async fn chat_founder(State(s): S, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let chat = super::api::load_phone_chat(&s, &id)?;
    let (waiting, name) = founder_wait(&s, chat.workspace_id);
    Ok(Json(json!({ "waiting": waiting, "founderName": name })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_lose_hidden_characters() {
        assert_eq!(clean_handle("al\u{202E}ice"), "alice");
        assert_eq!(clean_handle("bob\u{200B} "), "bob");
        assert_eq!(clean_handle("did:plc:abc"), "did:plc:abc");
    }
}
