//! People: your profile and safety code, co-members, direct messages, and
//! invites to people you already share a workspace with (spec:
//! IDENTITY-AND-FORGES §2.1, §3.1, §4, §6.3).
//!
//! Nothing here involves a provider or a directory. Your account id is the
//! random id made on first launch; the people you can reach are the members
//! of workspaces you share, with keys verified on those config logs
//! (`hive_runtime::directory_trust`); a DM's room and invite id are random
//! (`hive_runtime::dm`). An invite to a co-member is sealed to their verified
//! key-agreement keys and dropped into their contact drop box (relay v0.3,
//! `hive_runtime::contacts`); on a relay without contacts, or for someone
//! who hasn't published one, it is a link or QR code you hand over. A DM's
//! invite for someone whose contact id isn't known yet is held, sealed to
//! this device's key, and delivered once when it arrives
//! (`hive_runtime::dm_delivery`, the same code `ligus serve` runs).
//!
//! Contacts are per device: each device keeps its own contact id and write
//! key, publishes its record to the relays of its workspaces, announces the
//! id on what it stamps, and reads its own drop box. A drop is acknowledged
//! (deleted on the relay) once this device handled it; no other device reads
//! that box, so there is nothing to wait for.

use std::collections::BTreeMap;

use hive_core::Timestamp;
use hive_proto::WorkspaceInfoDto;
use hive_runtime::directory_trust::{self, CoMember, SealPlan};
use hive_runtime::dm;
use hive_runtime::dm_delivery::{self, Delivery, HeldInvites};
use tauri::State;
use uuid::Uuid;

use hive_runtime::contacts::{self, ContactsError};

use crate::{
    decode_workspace_invite, encode_workspace_invite, join_workspace_conn, map_err, parse_hex32, random_token,
    save_settings, set_active_workspace, sign_invite, with_epoch_keys, AppState, LiveSettings, WorkspaceConn,
};

/// This device's account: id, key fingerprint (when it holds the key).
fn me(state: &AppState) -> (Option<Uuid>, Option<String>) {
    let svc = state.service.lock().unwrap();
    let account = svc.author().account_id.or_else(|| Uuid::parse_str(&svc.author().id).ok());
    let fk = (!svc.is_delegated())
        .then(|| hive_runtime::envelope_verifier::FounderPin::fingerprint_hex(&svc.account_public_key()));
    (account, fk)
}

/// The founder account an invite's signature proves (R1), checked against
/// the devices the shared logs revoked.
fn invite_signer(conn: &WorkspaceConn, people: &[CoMember]) -> Option<Uuid> {
    conn.signed_by(&|d| people.iter().any(|p| p.revoked_devices.contains(&d)))
}

/// An invite's trust label: a co-member's name and fingerprint count as
/// theirs only with their device's valid signature.
fn invite_trust(conn: &WorkspaceConn, people: &[CoMember]) -> dm::FounderTrust {
    let known = dm::known_fingerprints(people);
    dm::check_founder(conn.founder_actor_id.as_deref(), conn.fk.as_deref(), &known, invite_signer(conn, people))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FounderStatusDto {
    /// Writes into this chat's workspace wait for its founder's signed
    /// founding to sync (a state, not a failure).
    waiting: bool,
    /// The founder's name, when they are a co-member.
    founder_name: Option<String>,
}

/// Whether sending in a chat waits for its workspace's founder: an invite's
/// founder is a claim until the founder's signed founding is on this device,
/// and nothing is written there before.
#[tauri::command]
pub(crate) fn founder_status(state: State<AppState>, session_id: String) -> Result<FounderStatusDto, String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let waiting = {
        let svc = state.service.lock().unwrap();
        match svc.store().workspace_of(sid).map_err(map_err)? {
            Some(ws) => svc.waiting_for_founder(ws).map_err(map_err)?,
            None => None,
        }
    };
    let founder_name = waiting.and_then(|f| co_members_of(&state).into_iter().find(|c| c.account_id == f)).map(|c| c.display_name);
    Ok(FounderStatusDto { waiting: waiting.is_some(), founder_name })
}

fn co_members_of(state: &AppState) -> Vec<CoMember> {
    let (account, _) = me(state);
    let svc = state.service.lock().unwrap();
    directory_trust::co_members(svc.store(), account.unwrap_or_default())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProfileDto {
    /// The random account id (never derived from anything public).
    account_id: String,
    display_name: String,
    avatar_url: Option<String>,
    /// Eight groups of four hex characters people compare out of band.
    /// `None` on a device that acts for an account without holding its key.
    safety_code: Option<String>,
    git_email: String,
}

/// Your profile: name, picture, safety code and commit email.
#[tauri::command]
pub(crate) fn my_profile(state: State<AppState>) -> Result<ProfileDto, String> {
    let (account, fk) = me(&state);
    let loaded = state.identity.load().map_err(map_err)?;
    let (display_name, avatar_url) = {
        let svc = state.service.lock().unwrap();
        let a = svc.author();
        let name = loaded.as_ref().map(|s| s.account.display_name.clone()).unwrap_or_else(|| a.display_name.clone());
        (name, a.avatar_url.clone().or_else(|| loaded.as_ref().and_then(|s| s.account.avatar_url.clone())))
    };
    Ok(ProfileDto {
        account_id: account.map(|a| a.to_string()).unwrap_or_default(),
        display_name,
        avatar_url,
        safety_code: fk.as_deref().and_then(directory_trust::safety_code),
        git_email: state.settings.lock().unwrap().git_email.clone().unwrap_or_default(),
    })
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SharedWorkspaceDto {
    pub(crate) id: String,
    pub(crate) name: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CoMemberDto {
    account_id: String,
    display_name: String,
    avatar_url: Option<String>,
    safety_code: Option<String>,
    /// The shared logs disagree about this person's account key: nothing is
    /// sealed to them until that is resolved.
    key_conflict: bool,
    /// Another co-member's name looks like this one: tell them apart by
    /// safety code.
    look_alike: bool,
    /// Devices whose keys are verified on a shared config log.
    devices: u32,
    workspaces: Vec<SharedWorkspaceDto>,
    /// The open (kept) DM with them, if any.
    dm_workspace_id: Option<String>,
    /// Their linked accounts in the workspaces you share (opt-in claims;
    /// the status is this device's cached forge check, never synced).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    links: Vec<crate::links_ui::LinkChipDto>,
}

/// Everyone you share a workspace with, keyed by account id. Two people may
/// share a name; their safety codes tell them apart.
#[tauri::command]
pub(crate) fn list_co_members(state: State<AppState>) -> Vec<CoMemberDto> {
    let list = co_members_of(&state);
    let chips: BTreeMap<Uuid, Vec<crate::links_ui::LinkChipDto>> =
        list.iter().map(|c| (c.account_id, crate::links_ui::chips_for(&state, c.account_id))).collect();
    let mut chips = chips;
    let s = state.settings.lock().unwrap();
    let name_of = |id: Uuid| s.workspaces.iter().find(|w| w.id() == id).map(|w| w.display_name()).unwrap_or_default();
    list.into_iter()
        .filter(|c| !c.workspaces.iter().all(|w| s.workspaces.iter().any(|x| x.id() == *w && x.dm_account.is_some())))
        .map(|c| CoMemberDto {
            account_id: c.account_id.to_string(),
            display_name: c.display_name.clone(),
            avatar_url: c.avatar_url.clone(),
            safety_code: c.safety_code(),
            key_conflict: c.key_conflict,
            look_alike: c.look_alike,
            devices: c.devices.len() as u32,
            workspaces: c
                .workspaces
                .iter()
                .filter(|w| s.workspaces.iter().any(|x| x.id() == **w && x.dm_account.is_none()))
                .map(|w| SharedWorkspaceDto { id: w.to_string(), name: name_of(*w) })
                .collect(),
            dm_workspace_id: s
                .workspaces
                .iter()
                .find(|w| w.dm_account.as_deref() == Some(c.account_id.to_string().as_str()) && !s.archived_dms.contains(&w.room))
                .map(|w| w.id().to_string()),
            links: chips.remove(&c.account_id).unwrap_or_default(),
        })
        .collect()
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DmDto {
    workspace_id: String,
    /// The other person's account id.
    account: String,
    name: String,
    avatar_url: Option<String>,
    safety_code: Option<String>,
    /// Earlier DMs with the same person that the simultaneous-open rule
    /// archived (read-only), oldest first.
    archived: Vec<String>,
    /// The DM's invite while this device holds it for delivery (and a day
    /// after it finished): `{state, pending, delivered, deliveredAt,
    /// expiresAt, warning}` (`hive_runtime::dm_delivery`).
    invite_delivery: Option<serde_json::Value>,
}

/// Owner decision D12 over this device's DMs: for every peer with more than
/// one DM, keep the one with the smaller `(created_at, room)` and archive the
/// rest. Runs whenever the DM list is read; a pair still syncing is decided
/// later.
fn resolve_concurrent_dms(state: &AppState) {
    let by_peer: BTreeMap<String, Vec<WorkspaceConn>> = {
        let s = state.settings.lock().unwrap();
        let mut m: BTreeMap<String, Vec<WorkspaceConn>> = BTreeMap::new();
        for w in &s.workspaces {
            if let Some(peer) = &w.dm_account {
                m.entry(peer.clone()).or_default().push(w.clone());
            }
        }
        m
    };
    let mut resolutions: Vec<dm::DmResolution> = Vec::new();
    for dms in by_peer.values().filter(|v| v.len() > 1) {
        // Only DMs whose founder's signed seed is on their log take part: a
        // DM still syncing joins the decision once founded, and a forged one
        // (an invite "from" this peer that they never founded) never does.
        let candidates: Vec<dm::DmCandidate> = {
            let svc = state.service.lock().unwrap();
            dms.iter()
                .map(|w| dm::DmCandidate { room: w.room.clone(), created_at: svc.store().config_founded_at(w.id()).ok().flatten() })
                .filter(|c| c.created_at.is_some())
                .collect()
        };
        if let Some(r) = dm::resolve_concurrent(&candidates) {
            resolutions.push(r);
        }
    }
    if resolutions.is_empty() {
        return;
    }
    let mut s = state.settings.lock().unwrap();
    let before = s.archived_dms.clone();
    for r in &resolutions {
        dm::apply_resolution(&mut s.archived_dms, r);
    }
    if s.archived_dms != before {
        save_settings(&state.data_dir, &s);
    }
}

/// Whether a workspace is a DM the simultaneous-open rule archived
/// (read-only: new messages go to the kept one).
pub(crate) fn is_archived_dm(state: &AppState, workspace_id: Uuid) -> bool {
    let s = state.settings.lock().unwrap();
    s.workspaces.iter().any(|w| w.id() == workspace_id && w.dm_account.is_some() && s.archived_dms.contains(&w.room))
}

/// Your direct messages: one per person (the kept one), each linking the
/// archived ones.
#[tauri::command]
pub(crate) fn list_dms(state: State<AppState>) -> Vec<DmDto> {
    resolve_concurrent_dms(&state);
    let people: BTreeMap<String, CoMember> = co_members_of(&state).into_iter().map(|c| (c.account_id.to_string(), c)).collect();
    let conns: Vec<WorkspaceConn> = state.settings.lock().unwrap().workspaces.iter().filter(|w| w.dm_account.is_some()).cloned().collect();
    let created_at: BTreeMap<Uuid, Option<Timestamp>> = {
        let svc = state.service.lock().unwrap();
        conns.iter().map(|w| (w.id(), svc.store().config_founded_at(w.id()).ok().flatten())).collect()
    };
    let created = |w: &WorkspaceConn| created_at.get(&w.id()).copied().flatten();
    let s = state.settings.lock().unwrap();
    let mut out = Vec::new();
    for w in s.workspaces.iter().filter(|w| w.dm_account.is_some() && !s.archived_dms.contains(&w.room)) {
        let peer = w.dm_account.clone().unwrap_or_default();
        let mut archived: Vec<(Option<Timestamp>, String)> = s
            .workspaces
            .iter()
            .filter(|x| x.dm_account.as_deref() == Some(peer.as_str()) && s.archived_dms.contains(&x.room))
            .map(|x| (created(x), x.id().to_string()))
            .collect();
        archived.sort();
        let person = people.get(&peer);
        out.push(DmDto {
            workspace_id: w.id().to_string(),
            name: person.map(|p| p.display_name.clone()).filter(|n| !n.trim().is_empty()).unwrap_or_else(|| w.display_name()),
            avatar_url: person.and_then(|p| p.avatar_url.clone()),
            safety_code: person.and_then(|p| p.safety_code()),
            account: peer,
            archived: archived.into_iter().map(|(_, id)| id).collect(),
            invite_delivery: held(&state).get(w.id()).map(|h| h.status_json()),
        });
    }
    out
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OpenDmDto {
    workspace_id: String,
    /// The DM's invite link for the other person (`hivews1:`), on creation.
    /// Single-use and addressed to them. Hand it over yourself unless
    /// `delivered`.
    invite: Option<String>,
    /// The invite reached their drop box, sealed to their verified devices.
    delivered: bool,
    /// Not delivered yet (their contact id isn't known here): held sealed on
    /// this device and delivered once it is. The link still works by hand.
    pending: bool,
    /// The held invite's state (as [`DmDto::invite_delivery`]).
    invite_delivery: Option<serde_json::Value>,
    /// Their safety code, so they can check yours and you theirs.
    safety_code: Option<String>,
}

/// Open (creating on first use) the DM with a co-member and switch to it.
///
/// The room and the invite id are random; you found the DM, the invite names
/// their account and carries your key fingerprint. The DM is created only
/// when their keys are verified on a workspace you share.
#[tauri::command]
pub(crate) async fn open_dm(state: State<'_, AppState>, account_id: String) -> Result<OpenDmDto, String> {
    let peer = Uuid::parse_str(account_id.trim()).map_err(|_| "not an account id".to_string())?;
    let (me_account, my_fk) = me(&state);
    let me_account = me_account.ok_or("this device has no account yet")?;
    if peer == me_account {
        return Err("That's you.".into());
    }
    let person = co_members_of(&state)
        .into_iter()
        .find(|c| c.account_id == peer)
        .ok_or("You can open a DM with someone you share a workspace with. Otherwise send them an invite link.")?;
    let plan = person.seal_plan()?;
    let peer_s = peer.to_string();
    let existing = {
        let s = state.settings.lock().unwrap();
        s.workspaces
            .iter()
            .filter(|w| w.dm_account.as_deref() == Some(peer_s.as_str()) && !s.archived_dms.contains(&w.room))
            .map(|w| w.id())
            .collect::<Vec<_>>()
    };
    // "Message <peer>" lands only in a DM their signed founding (or this
    // device's own) verifies: never in a forged one that merely names them.
    let existing = {
        let svc = state.service.lock().unwrap();
        existing.into_iter().find(|id| svc.founder_verified(*id).unwrap_or(false))
    };
    if let Some(id) = existing {
        set_active_workspace(state.clone(), id.to_string())?;
        let held = held(&state).get(id);
        return Ok(OpenDmDto {
            workspace_id: id.to_string(),
            invite: None,
            delivered: false,
            pending: held.as_ref().is_some_and(|h| h.status_json()["pending"] == serde_json::json!(true)),
            invite_delivery: held.map(|h| h.status_json()),
            safety_code: person.safety_code(),
        });
    }
    let my_fk = my_fk.ok_or("this device acts for an account without its key and can't found a DM")?;
    let conn = {
        let mut s = state.settings.lock().unwrap();
        let conn = WorkspaceConn {
            name: person.display_name.clone(),
            relay_url: s.relay_url.clone().unwrap_or_default(),
            room: dm::new_room(),
            key: Some(random_token(24)),
            icon: None,
            dm_account: Some(peer_s.clone()),
            founder_actor_id: Some(me_account.to_string()),
            fk: Some(my_fk),
            inv: None,
            ek: Vec::new(),
            sig: None,
        };
        s.workspaces.push(conn.clone());
        save_settings(&state.data_dir, &s);
        conn
    };
    let ws_id = conn.id();
    set_active_workspace(state.clone(), ws_id.to_string())?;
    let inv = state.service.lock().unwrap().issue_invite(ws_id).map_err(map_err)?;
    let invite = encode_workspace_invite(&sign_invite(&state, with_epoch_keys(&state, WorkspaceConn { inv: Some(inv.to_string()), ..conn })));
    let invite = crate::with_invite_redeem_key(&state, ws_id, inv, invite)?;
    // Delivered now, or held (sealed on this device) and delivered once their
    // contact id arrives.
    let delivered = deliver_invite(&state, &person, &plan, &invite).await == Delivery::Delivered;
    let held = if delivered { None } else { hold_invite(&state, ws_id, &person, &plan, &invite) };
    Ok(OpenDmDto {
        workspace_id: ws_id.to_string(),
        invite: Some(invite),
        delivered,
        pending: held.is_some(),
        invite_delivery: held.map(|h| h.status_json()),
        safety_code: person.safety_code(),
    })
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InviteInfoDto {
    workspace_name: String,
    is_dm: bool,
    /// For a DM: whether it is addressed to this account.
    addressed_to_me: bool,
    founder_account: Option<String>,
    /// The founder's name, when they are a co-member.
    founder_name: Option<String>,
    /// The founder's safety code as the invite carries it.
    safety_code: Option<String>,
    /// `known` (a co-member whose device signed it, and the key matches),
    /// `claimed` (names a co-member and their key, but isn't signed by their
    /// device: it only says it's from them), `mismatch` (a co-member with a
    /// different key: not from them), `unknown` (compare the code out of
    /// band) or `none` (the invite carries no code).
    trust: String,
    already_joined: bool,
}

/// What an invite says about who sent it, before joining.
#[tauri::command]
pub(crate) fn inspect_invite(state: State<AppState>, code: String) -> Result<InviteInfoDto, String> {
    let conn = decode_workspace_invite(&code)?;
    let (me_account, _) = me(&state);
    let people = co_members_of(&state);
    let trust = invite_trust(&conn, &people);
    let founder = conn.founder_actor_id.as_deref().and_then(|f| Uuid::parse_str(f).ok());
    let already_joined = state.settings.lock().unwrap().workspaces.iter().any(|w| w.room == conn.room);
    Ok(InviteInfoDto {
        workspace_name: if conn.dm_account.is_some() { String::new() } else { hive_proto::names::clean_display_name(&conn.display_name()) },
        is_dm: conn.dm_account.is_some(),
        addressed_to_me: me_account.is_some_and(|m| conn.dm_account.as_deref() == Some(m.to_string().as_str())),
        founder_account: founder.map(|f| f.to_string()),
        founder_name: founder.and_then(|f| people.iter().find(|p| p.account_id == f)).map(|p| p.display_name.clone()),
        safety_code: conn.fk.as_deref().and_then(directory_trust::safety_code),
        trust: trust_str(trust).into(),
        already_joined,
    })
}

/// The accept rule for a DM invite: addressed to this account, from a
/// founder whose key matches what the shared logs register (or, for someone
/// this device shares nothing with, after the person compared the safety
/// code). Rewrites the connection to this side's view: `dm_account` becomes
/// the founder (the other person).
pub(crate) fn admit_dm_invite(state: &AppState, conn: &mut WorkspaceConn, safety_code_confirmed: bool) -> Result<(), String> {
    let (me_account, _) = me(state);
    let me_account = me_account.ok_or("this device has no account yet")?;
    let people = co_members_of(state);
    let known = dm::known_fingerprints(&people);
    let joined = state.settings.lock().unwrap().workspaces.iter().any(|w| w.room == conn.room);
    let peer = dm::accept_dm_invite(
        conn.dm_account.as_deref(),
        conn.founder_actor_id.as_deref(),
        conn.fk.as_deref(),
        me_account,
        &known,
        invite_signer(conn, &people),
        safety_code_confirmed,
        joined,
    )?;
    conn.dm_account = Some(peer.to_string());
    conn.name = people
        .iter()
        .find(|p| p.account_id == peer)
        .map(|p| p.display_name.clone())
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| "Direct message".into());
    Ok(())
}

/// Join a DM from its invite. `confirm_safety_code` is for someone you share
/// no workspace with, after you compared their safety code out of band.
#[tauri::command]
pub(crate) fn join_dm(state: State<AppState>, code: String, confirm_safety_code: bool) -> Result<WorkspaceInfoDto, String> {
    let mut conn = decode_workspace_invite(&code)?;
    if conn.dm_account.is_none() {
        return Err("not a direct message invite".into());
    }
    admit_dm_invite(&state, &mut conn, confirm_safety_code)?;
    join_workspace_conn(state, conn, hive_runtime::box_admission::redeem_key_of(&code))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CoMemberInviteDto {
    /// The single-use invite link (`hivews1:`).
    invite: String,
    name: String,
    /// Their safety code, to compare with theirs.
    safety_code: Option<String>,
    /// Devices of theirs whose keys are verified (what a drop-box delivery
    /// seals to).
    devices: u32,
    /// The invite reached their drop box, sealed to their verified devices.
    /// Otherwise (no contact, or a relay without contact routes) share the
    /// link yourself.
    delivered: bool,
}

/// Invite someone you share a workspace with to another workspace. Requires
/// their keys verified on a shared config log (so a later drop-box delivery
/// is sealed to them only). Returns the invite link to hand over.
#[tauri::command]
pub(crate) async fn co_member_invite(
    state: State<'_, AppState>,
    workspace_id: String,
    account_id: String,
) -> Result<CoMemberInviteDto, String> {
    let ws = Uuid::parse_str(&workspace_id).map_err(map_err)?;
    let peer = Uuid::parse_str(account_id.trim()).map_err(|_| "not an account id".to_string())?;
    let person = co_members_of(&state)
        .into_iter()
        .find(|c| c.account_id == peer)
        .ok_or("only people you share a workspace with can be invited this way; use an invite link")?;
    if person.workspaces.contains(&ws) {
        return Err(format!("{} is already a member", person.display_name));
    }
    let plan = person.seal_plan()?;
    let conn = {
        let s = state.settings.lock().unwrap();
        s.workspaces
            .iter()
            .find(|w| w.id() == ws && w.dm_account.is_none())
            .cloned()
            .ok_or_else(|| "unknown workspace".to_string())?
    };
    let inv = state
        .service
        .lock()
        .unwrap()
        .issue_invite(ws)
        .map_err(|e| format!("Only a workspace Admin can invite people ({e})."))?;
    let invite = encode_workspace_invite(&sign_invite(&state, with_epoch_keys(&state, WorkspaceConn { inv: Some(inv.to_string()), ..conn })));
    let invite = crate::with_invite_redeem_key(&state, ws, inv, invite)?;
    let delivered = deliver_invite(&state, &person, &plan, &invite).await == Delivery::Delivered;
    Ok(CoMemberInviteDto {
        invite,
        name: person.display_name.clone(),
        safety_code: person.safety_code(),
        devices: plan.recipients.len() as u32,
        delivered,
    })
}

// ── Contacts: this device's record and drop box (relay v0.3) ──────────────

/// Republish a record this long after the last write (the relay keeps a
/// record 30 days from its last write).
const REPUBLISH_SECS: i64 = 24 * 3600;
/// Ask a relay that had no contact routes again after this long.
const RECHECK_UNSUPPORTED_SECS: i64 = 24 * 3600;

/// This device's contact: a random id and write key, and where it is
/// published.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct OwnContact {
    pub(crate) cid: String,
    /// The capability to change the record and read the box (secret store).
    #[serde(default)]
    pub(crate) write_key: String,
    /// relay URL → unix seconds of the last successful write.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) published: BTreeMap<String, i64>,
    /// relay URL → unix seconds it last answered "no contact routes".
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) unsupported: BTreeMap<String, i64>,
    /// relay URL → drop seqs this device handled and hasn't acknowledged yet.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) handled: BTreeMap<String, Vec<u64>>,
}

impl OwnContact {
    fn fresh() -> Self {
        Self { cid: contacts::new_cid(), write_key: contacts::new_write_key(), ..Default::default() }
    }
}

/// The contact id this device announces: only once some relay holds its
/// record (else a sender would only find a 404).
pub(crate) fn announced_cid(s: &LiveSettings) -> Option<String> {
    s.contact.as_ref().filter(|c| !c.published.is_empty() && contacts::valid_cid(&c.cid)).map(|c| c.cid.clone())
}

fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// The relays this device syncs with (every workspace's, deduplicated).
fn my_relays(s: &LiveSettings) -> Vec<String> {
    let mut out: Vec<String> = s.workspaces.iter().map(|w| w.relay_url.trim().to_string()).collect();
    out.extend(s.relay_url.iter().map(|r| r.trim().to_string()));
    out.retain(|r| !r.is_empty());
    out.sort();
    out.dedup();
    out
}

fn ka_keypair(state: &AppState) -> Option<hive_core::e2ee::KeyAgreementKeypair> {
    let s = state.settings.lock().unwrap();
    s.ka_secret.as_deref().and_then(parse_hex32).and_then(|seed| hive_core::e2ee::KeyAgreementKeypair::from_seed(&seed).ok())
}

/// This device's record for `cid`, or `None` on a device that can't speak
/// for its account (delegated, or no certificate yet).
fn own_record(state: &AppState, cid: &str) -> Option<serde_json::Value> {
    let ka = ka_keypair(state)?;
    let stored = state.identity.load().ok().flatten()?;
    let svc = state.service.lock().unwrap();
    if svc.is_delegated() {
        return None;
    }
    let account_id = svc.author().account_id?;
    if stored.device.certificate.account_id != account_id || stored.device.id != svc.device_id() {
        return None;
    }
    let env = svc.signed_ka_publication(contacts::contact_workspace_id(cid), ka.public_key_bytes().to_vec());
    let device = contacts::RecordDevice { certificate: stored.device.certificate.clone(), ka_publication: env };
    let mut rec = contacts::build_record(cid, account_id, &svc.account_public_key(), vec![device], Timestamp::now());
    drop(svc);
    rec.links = crate::links_ui::contact_link_envelopes(state, cid);
    serde_json::to_value(rec).ok()
}

/// Publish (or refresh) this device's record on every relay it uses. A relay
/// without contact routes is remembered and asked again a day later; a cid
/// another key holds (it lapsed and was reclaimed) is replaced with a new
/// one. Best-effort; returns whether any relay holds the record.
pub(crate) async fn ensure_contact(state: &AppState) -> bool {
    let (contact, relays) = {
        let mut s = state.settings.lock().unwrap();
        if s.contact.as_ref().is_none_or(|c| !contacts::valid_cid(&c.cid) || c.write_key.is_empty()) {
            s.contact = Some(OwnContact::fresh());
            save_settings(&state.data_dir, &s);
        }
        (s.contact.clone().unwrap_or_default(), my_relays(&s))
    };
    let now = unix_now();
    let mut c = contact.clone();
    let mut refused = 0usize;
    c.published.retain(|r, _| relays.contains(r));
    for relay in &relays {
        if c.published.get(relay).is_some_and(|t| now - t < REPUBLISH_SECS) {
            continue;
        }
        if c.unsupported.get(relay).is_some_and(|t| now - t < RECHECK_UNSUPPORTED_SECS) {
            continue;
        }
        let Some(record) = own_record(state, &c.cid) else { break };
        let client = state.relay_client(relay);
        let first = !c.published.contains_key(relay);
        match client.contact_put(&c.cid, &c.write_key, &record, first).await {
            Ok(_) => {
                c.published.insert(relay.clone(), now);
                c.unsupported.remove(relay);
            }
            Err(ContactsError::Unsupported) => {
                c.unsupported.insert(relay.clone(), now);
            }
            Err(ContactsError::Unauthorized) => {
                // Someone else holds this cid on that relay now: stop using
                // it there (asked again a day later); start over with a new
                // id only when no relay holds the record any more.
                tracing::warn!(target: "contacts", "contact id refused on a relay");
                c.published.remove(relay);
                c.unsupported.insert(relay.clone(), now);
                refused += 1;
            }
            Err(e) => tracing::debug!(target: "contacts", error = %e, "contact publish failed"),
        }
    }
    if contacts::rotate_cid_after_refusal(refused, c.published.len()) {
        tracing::warn!(target: "contacts", "contact id refused everywhere; making a new one");
        c = OwnContact::fresh();
    }
    let changed = c != contact;
    let announced = (!c.published.is_empty()).then(|| c.cid.clone());
    if changed {
        let mut s = state.settings.lock().unwrap();
        s.contact = Some(c);
        save_settings(&state.data_dir, &s);
    }
    state.service.lock().unwrap().set_author_contact(announced.clone());
    announced.is_some()
}

/// Drop an invite into a co-member's box, sealed to `plan` (their verified
/// devices), through the relay of the workspace their contact id was seen
/// in ([`dm_delivery::drop_to_box`], shared with `ligus serve`). `NotYet`
/// when they have no contact, their relay has no contact routes, their
/// record isn't theirs, or the drop failed: the caller shows the link (and
/// holds a DM's invite for a retry).
async fn deliver_invite(state: &AppState, person: &CoMember, plan: &SealPlan, invite: &str) -> Delivery {
    let Some((cid, ws)) = person.contact.clone() else { return Delivery::NotYet("no contact id for them yet".into()) };
    let relay = {
        let s = state.settings.lock().unwrap();
        s.workspaces.iter().find(|w| w.id() == ws).map(|w| w.relay_url.trim().to_string()).filter(|r| !r.is_empty())
    };
    let Some(relay) = relay else { return Delivery::NotYet("no relay for their contact".into()) };
    // The drop box must be theirs: their record at that cid verifies under
    // the account key the shared logs register for them, is no older than
    // the newest one seen for it, and lists a device not revoked (S4). A cid
    // someone else stamped, or a relay serving a stale record, gets nothing.
    let client = state.relay_client(&relay);
    let out = dm_delivery::drop_to_box(&client, &cid, person, plan, invite, |record| {
        let svc = state.service.lock().unwrap();
        contacts::accept_contact_record(svc.store(), &cid, record, person.fingerprint.as_deref(), &|d| {
            person.revoked_devices.contains(&d)
        })
    })
    .await;
    if let Delivery::NotYet(why) = &out {
        tracing::info!(target: "contacts", reason = %why, "invite not delivered to the drop box yet");
    }
    out
}

// ── Held DM invites (hive_runtime::dm_delivery) ───────────────────────────

/// This device's held DM invites, one store per data dir.
fn held(state: &AppState) -> std::sync::Arc<HeldInvites> {
    static STORES: std::sync::OnceLock<std::sync::Mutex<BTreeMap<std::path::PathBuf, std::sync::Arc<HeldInvites>>>> =
        std::sync::OnceLock::new();
    let mut m = STORES.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    m.entry(state.data_dir.clone()).or_insert_with(|| std::sync::Arc::new(HeldInvites::for_data_dir(&state.data_dir))).clone()
}

/// Wakes the held-invite loop.
static HELD_KICK: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Hold a DM invite that couldn't be delivered yet, sealed to this device's
/// key-agreement key. `None` when it can't be held (no key, too many
/// waiting): the link is the way.
fn hold_invite(state: &AppState, ws: Uuid, person: &CoMember, plan: &SealPlan, invite: &str) -> Option<dm_delivery::HeldInvite> {
    let ka = ka_keypair(state)?;
    match held(state).hold(ws, person, plan, invite, &ka.public_key_bytes(), unix_now()) {
        Ok(h) => {
            HELD_KICK.notify_one();
            Some(h)
        }
        Err(e) => {
            tracing::warn!(target: "contacts", error = %e, "a DM invite can't wait for delivery; share the link");
            None
        }
    }
}

/// [`dm_delivery::Host`] for the desktop.
struct DesktopHost<'a>(&'a AppState);

impl dm_delivery::Host for DesktopHost<'_> {
    fn co_members(&self) -> Vec<CoMember> {
        co_members_of(self.0)
    }
    fn dm_exists(&self, ws: Uuid) -> bool {
        let s = self.0.settings.lock().unwrap();
        s.workspaces.iter().any(|w| w.id() == ws && w.dm_account.is_some() && !s.archived_dms.contains(&w.room))
    }
    fn own_ka(&self) -> Option<hive_core::e2ee::KeyAgreementKeypair> {
        ka_keypair(self.0)
    }
    fn deliver(&self, person: &CoMember, plan: &SealPlan, invite: &str) -> impl std::future::Future<Output = Delivery> + Send {
        deliver_invite(self.0, person, plan, invite)
    }
}

/// While one waits, look every few seconds (co-members come from the
/// synced logs, so a contact id or a join that arrived is seen at the next
/// pass); otherwise only check now and then for a new one.
const HELD_EVERY: std::time::Duration = std::time::Duration::from_secs(3);
const HELD_IDLE: std::time::Duration = std::time::Duration::from_secs(30);

/// Deliver held DM invites once their peer's contact id is known; emits
/// `dm-invite-changed` (`{workspaceId, inviteDelivery}`) for each change.
pub(crate) async fn run_held_dm_loop(app: tauri::AppHandle) {
    use tauri::{Emitter, Manager};
    loop {
        let state = app.state::<AppState>();
        let store = held(&state);
        let changed = dm_delivery::run_pass(&DesktopHost(&state), &store, unix_now()).await;
        for h in &changed {
            let _ = app.emit("dm-invite-changed", serde_json::json!({ "workspaceId": h.workspace_id, "inviteDelivery": h.status_json() }));
        }
        let wait = if store.any_open() { HELD_EVERY } else { HELD_IDLE };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = HELD_KICK.notified() => {}
        }
    }
}

fn held_ws(state: &AppState, workspace_id: &str) -> Result<Uuid, String> {
    let ws = Uuid::parse_str(workspace_id.trim()).map_err(map_err)?;
    held(state).get(ws).map(|_| ws).ok_or_else(|| "no invite waits for delivery in this DM".to_string())
}

/// The held invite's link, to share by hand while it waits.
#[tauri::command]
pub(crate) fn held_dm_invite(state: State<AppState>, workspace_id: String) -> Result<String, String> {
    let ws = held_ws(&state, &workspace_id)?;
    let ka = ka_keypair(&state).ok_or("this device has no key-agreement key")?;
    held(&state)
        .open_invite(ws, &ka)
        .map(|z| z.to_string())
        .ok_or_else(|| "The invite no longer waits here (delivered, joined or expired).".to_string())
}

/// After the unverified-recipient warning (their keys changed since the DM
/// was opened): deliver to their current verified devices. `safety_code` is
/// the code the opener compared; refused unless it is theirs now.
#[tauri::command]
pub(crate) fn confirm_dm_invite(state: State<AppState>, workspace_id: String, safety_code: String) -> Result<serde_json::Value, String> {
    let ws = held_ws(&state, &workspace_id)?;
    let store = held(&state);
    let account = store.get(ws).map(|h| h.account_id).ok_or("no invite waits for delivery in this DM")?;
    let person = co_members_of(&state)
        .into_iter()
        .find(|c| c.account_id == account)
        .ok_or("You no longer share a workspace with them.")?;
    let h = store.confirm(ws, &person, &safety_code, unix_now())?;
    HELD_KICK.notify_one();
    Ok(h.status_json())
}

/// Stop waiting (share the link instead); the sealed invite is forgotten.
#[tauri::command]
pub(crate) fn dismiss_dm_invite(state: State<AppState>, workspace_id: String) -> Result<Option<serde_json::Value>, String> {
    let ws = held_ws(&state, &workspace_id)?;
    let store = held(&state);
    store.dismiss(ws);
    Ok(store.get(ws).map(|h| h.status_json()))
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PendingInviteDto {
    relay: String,
    seq: u64,
    is_dm: bool,
    workspace_name: String,
    founder_account: Option<String>,
    founder_name: Option<String>,
    safety_code: Option<String>,
    /// As [`InviteInfoDto::trust`].
    trust: String,
    /// The decoded `hivews1:` code, passed back to accept.
    code: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PendingInvitesDto {
    /// `on` (a relay holds this device's contact), `unsupported` (no relay
    /// has contact routes: invites come as links and QR codes only) or
    /// `noRelay`.
    contacts: String,
    invites: Vec<PendingInviteDto>,
}

/// Refresh this device's contact and read its drop boxes: invites sealed to
/// this device that wait for a decision. Garbage, invites for workspaces
/// already joined and anything not addressed to this account are handled
/// (and acknowledged) silently.
#[tauri::command]
pub(crate) async fn list_pending_invites(state: State<'_, AppState>) -> Result<PendingInvitesDto, String> {
    let on = ensure_contact(&state).await;
    let (contact, relays) = {
        let s = state.settings.lock().unwrap();
        (s.contact.clone(), my_relays(&s))
    };
    let status = if relays.is_empty() {
        "noRelay"
    } else if on {
        "on"
    } else {
        "unsupported"
    };
    let (Some(contact), Some(ka)) = (contact, ka_keypair(&state)) else {
        return Ok(PendingInvitesDto { contacts: status.into(), invites: Vec::new() });
    };
    let (me_account, _) = me(&state);
    let people = co_members_of(&state);
    let joined: Vec<String> = state.settings.lock().unwrap().workspaces.iter().map(|w| w.room.clone()).collect();
    let mut out = Vec::new();
    let mut handled_now: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for relay in contact.published.keys() {
        let client = state.relay_client(relay);
        let Ok(items) = client.contact_drops_all(&contact.cid, &contact.write_key).await else { continue };
        let already = contact.handled.get(relay).cloned().unwrap_or_default();
        let mut pending = Vec::new();
        let mut handled = already.clone();
        for item in items {
            if already.contains(&item.seq) {
                continue;
            }
            let conn = contacts::open_drop(&item.blob, &ka)
                .and_then(|b| String::from_utf8(b).ok())
                .and_then(|code| decode_workspace_invite(&code).ok().map(|c| (code, c)));
            let Some((code, conn)) = conn else {
                handled.push(item.seq);
                continue;
            };
            let is_dm = conn.dm_account.is_some();
            let for_me = !is_dm || me_account.is_some_and(|m| conn.dm_account.as_deref() == Some(m.to_string().as_str()));
            if joined.contains(&conn.room) || !for_me {
                handled.push(item.seq);
                continue;
            }
            let trust = invite_trust(&conn, &people);
            let founder = conn.founder_actor_id.as_deref().and_then(|f| Uuid::parse_str(f).ok());
            pending.push(item.seq);
            out.push(PendingInviteDto {
                relay: relay.clone(),
                seq: item.seq,
                is_dm,
                workspace_name: if is_dm { String::new() } else { hive_proto::names::clean_display_name(&conn.display_name()) },
                founder_account: founder.map(|f| f.to_string()),
                founder_name: founder.and_then(|f| people.iter().find(|p| p.account_id == f)).map(|p| p.display_name.clone()),
                safety_code: conn.fk.as_deref().and_then(directory_trust::safety_code),
                trust: trust_str(trust).into(),
                code,
            });
        }
        if let Some(through) = contacts::ack_through(&handled, &pending) {
            if client.contact_drops_ack(&contact.cid, &contact.write_key, through).await.is_ok() {
                handled.retain(|h| *h > through);
            }
        }
        handled_now.insert(relay.clone(), handled);
    }
    {
        let mut s = state.settings.lock().unwrap();
        if let Some(c) = s.contact.as_mut() {
            if c.cid == contact.cid {
                for (relay, h) in handled_now {
                    if h.is_empty() {
                        c.handled.remove(&relay);
                    } else {
                        c.handled.insert(relay, h);
                    }
                }
                save_settings(&state.data_dir, &s);
            }
        }
    }
    Ok(PendingInvitesDto { contacts: status.into(), invites: out })
}

fn trust_str(t: dm::FounderTrust) -> &'static str {
    match t {
        dm::FounderTrust::KnownMatch => "known",
        dm::FounderTrust::Claimed => "claimed",
        dm::FounderTrust::Mismatch => "mismatch",
        dm::FounderTrust::Unknown => "unknown",
        dm::FounderTrust::NoFingerprint => "none",
    }
}

fn mark_handled(state: &AppState, relay: &str, seq: u64) {
    let mut s = state.settings.lock().unwrap();
    if let Some(c) = s.contact.as_mut() {
        let h = c.handled.entry(relay.to_string()).or_default();
        if !h.contains(&seq) {
            h.push(seq);
        }
        save_settings(&state.data_dir, &s);
    }
}

/// Accept an invite from the drop box. A DM invite must pass the DM rule
/// (`confirm_safety_code` for someone you share no workspace with, after
/// comparing codes); a workspace invite whose founder's key contradicts what
/// shared workspaces register is refused.
#[tauri::command]
pub(crate) fn accept_pending_invite(
    state: State<AppState>,
    relay: String,
    seq: u64,
    code: String,
    confirm_safety_code: bool,
) -> Result<WorkspaceInfoDto, String> {
    let mut conn = decode_workspace_invite(&code)?;
    if conn.dm_account.is_some() {
        admit_dm_invite(&state, &mut conn, confirm_safety_code)?;
    } else if invite_trust(&conn, &co_members_of(&state)) == dm::FounderTrust::Mismatch {
        return Err("the invite's founder key doesn't match the one your shared workspaces have for that person".into());
    }
    let joined = join_workspace_conn(state.clone(), conn, hive_runtime::box_admission::redeem_key_of(&code))?;
    mark_handled(&state, &relay, seq);
    Ok(joined)
}

/// Dismiss an invite from the drop box (it is deleted on the relay at the
/// next read).
#[tauri::command]
pub(crate) fn dismiss_pending_invite(state: State<AppState>, relay: String, seq: u64) -> Result<(), String> {
    mark_handled(&state, &relay, seq);
    Ok(())
}
