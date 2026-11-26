//! Secure file attachments on the desktop (issue #118).
//!
//! * Sharing a file in a synced workspace: checked against the workspace
//!   attachment policy and blocklist, sealed, uploaded to the relay's blob
//!   channel, and bound to a signed `AttachmentShared`; the message carries
//!   only an `[Attachment: <id>]` marker. When the policy is off (the
//!   default) or there is no relay, the file stays a device-local
//!   `[Attached: <path>]` marker that only this device can open, as before.
//! * Receiving: nothing is fetched, opened or previewed automatically.
//!   "Check" downloads into a private cache, verifies the bytes against the
//!   signed reference (failing closed), detects the type from the content,
//!   inspects archives without extracting them and runs the optional scanner.
//!   "Save" writes the verified file where the person chooses, with the OS
//!   quarantine marking; risky types need a second confirmation that names
//!   the sender. A preview exists only for inert formats (raster images and
//!   plain text), rendered by the UI in a sandboxed frame.
//! * An agent's approved file offer is uploaded and shared by the host that
//!   produced it ([`share_approved_offers`]).

use std::collections::HashMap;
use std::sync::Mutex;

use ligus_core::attachment::{self, AttachmentPolicy, AttachmentRef, SharedAttachment};
use ligus_runtime::attachment_blob as blob;
use ligus_runtime::attachment_safety as safety;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

use crate::{blob_transport, map_err, AppState};

/// Staged agent offers: `<data>/attachments/staged/<proposal id>` (0600 in
/// 0700). Only an offer this device staged, under the proposal it filed, is
/// ever uploaded.
fn staged_dir(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join("attachments").join("staged")
}

/// Keep an agent's offered file until a person approves the offer.
pub(crate) fn stage(data_dir: &std::path::Path, proposal_id: Uuid, sha256: &str, bytes: &[u8]) -> Result<(), String> {
    if !attachment::is_sha256_hex(sha256) || blob::sha256_hex(bytes) != sha256 {
        return Err("the staged file doesn't match its hash".into());
    }
    let dir = staged_dir(data_dir);
    std::fs::create_dir_all(&dir).map_err(map_err)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    ligus_runtime::private_file::write_private_atomic(&dir.join(proposal_id.to_string()), bytes).map_err(map_err)
}

/// The relay transport for `workspace_id`, when it is the active synced
/// workspace.
fn transport_for(state: &AppState, workspace_id: Uuid) -> Result<(ligus_runtime::RelayClient, String, ligus_runtime::keyring::KeyringEngine), String> {
    let (url, room, ring) = blob_transport(state).ok_or("no relay is configured for this workspace, so files can't be shared or fetched")?;
    if ligus_core::room_workspace_id(&room) != workspace_id {
        return Err("switch to this chat's workspace to use its files".into());
    }
    Ok((state.relay_client(&url), room, ring))
}

fn session_of(state: &AppState, session_id: &str) -> Result<ligus_core::ChatSession, String> {
    let id = Uuid::parse_str(session_id).map_err(map_err)?;
    state.service.lock().unwrap().load_resolving(id).map_err(map_err)?.ok_or_else(|| "unknown chat".to_string())
}

fn shared_in(session: &ligus_core::ChatSession, attachment_id: &str) -> Result<SharedAttachment, String> {
    let id = Uuid::parse_str(attachment_id).map_err(map_err)?;
    session.attachments.get(&id).cloned().ok_or_else(|| "no such attachment in this chat".to_string())
}

fn sender_of(s: &SharedAttachment) -> String {
    blob::sender_label(s)
}

/// Share a composer attachment (issue #118). A synced workspace whose policy
/// allows this person: upload + sign, and return `[Attachment: <id>]`.
/// Otherwise (the device-local workspace, policy off, no relay): the
/// device-local `[Attached: <path>]` marker only this device can open.
#[tauri::command]
pub(crate) async fn share_attachment(state: State<'_, AppState>, session_id: String, path: String) -> Result<String, String> {
    let p = std::path::PathBuf::from(path.trim());
    let local = format!("[Attached: {}]", p.display());
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let workspace_id = {
        let svc = state.service.lock().unwrap();
        svc.workspace_of(sid).map_err(map_err)?.ok_or("unknown chat")?
    };
    if state.is_local_only_ws(workspace_id) {
        return Ok(local);
    }
    let (rules, (kind, role)) = {
        let svc = state.service.lock().unwrap();
        (svc.attachment_rules(workspace_id).map_err(map_err)?, svc.attachment_standing(workspace_id).map_err(map_err)?)
    };
    if !rules.policy.enabled {
        // Sharing is off here (the default): the file stays on this device.
        return Ok(local);
    }
    let Ok((client, room, ring)) = transport_for(&state, workspace_id) else {
        return Ok(local);
    };
    let meta = std::fs::metadata(&p).map_err(|e| format!("read attachment: {e}"))?;
    if !meta.is_file() {
        return Err("only a regular file can be shared".into());
    }
    if meta.len() > rules.policy.max_bytes {
        return Err(format!("the file is larger than this workspace allows ({} MB)", rules.policy.max_bytes / (1024 * 1024)));
    }
    let bytes = std::fs::read(&p).map_err(|e| format!("read attachment: {e}"))?;
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();
    let prepared = tauri::async_runtime::spawn_blocking(move || blob::prepare(&name, bytes, &rules, kind, role))
        .await
        .map_err(map_err)??;
    let device = state.service.lock().unwrap().device_id();
    let att = blob::upload(&client, &room, &ring, &prepared, device).await.map_err(|e| format!("upload attachment: {e}"))?;
    let signed = state.service.lock().unwrap().share_attachment(sid, workspace_id, att.clone());
    if let Err(e) = signed {
        // Nothing references the blob: take it back off the relay.
        let _ = client.delete_blob(&room, &att.blob_id).await;
        return Err(map_err(e));
    }
    // The sharer keeps the verified plaintext (no round trip to see it).
    let _ = blob::cache_verified(&blob::verified_cache_dir(&state.data_dir), &att, &prepared.plaintext);
    Ok(att.marker())
}

/// One shared file as the chat shows it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttachmentView {
    id: String,
    name: String,
    size: u64,
    content_type: String,
    sha256: String,
    author: String,
    sender: String,
    author_kind: ligus_core::ActorKind,
    deleted: bool,
    blocked: bool,
    mine: bool,
    can_delete: bool,
    /// The class the name claims (the UI's first warning; the content is
    /// checked on "Check").
    name_class: attachment::TypeClass,
    risky_by_name: bool,
    /// Shared by a host for an agent's approved offer.
    from_agent_offer: bool,
    /// The relay no longer has it and this device holds no verified copy:
    /// shown as "This file expired on the relay", not a download.
    expired: bool,
    /// The messages (signed by the sharer) that place it: the only ones that
    /// show it.
    message_ids: Vec<String>,
}

/// A chat's files: the shared ones, and the device-local `[Attached: <path>]`
/// markers in this person's own messages (the only path markers the UI turns
/// into chips; a path in someone else's message is just text).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatAttachments {
    shared: Vec<AttachmentView>,
    own_local_paths: Vec<String>,
}

#[tauri::command]
pub(crate) async fn list_attachments(ligus_app: tauri::AppHandle, session_id: String) -> Result<ChatAttachments, String> {
    crate::off_main(ligus_app, move |ligus_app| list_attachments_sync(tauri::Manager::state::<AppState>(ligus_app), session_id)).await
}

pub(crate) fn list_attachments_sync(state: State<AppState>, session_id: String) -> Result<ChatAttachments, String> {
    let session = session_of(&state, &session_id)?;
    let svc = state.service.lock().unwrap();
    let me = svc.author().id.clone();
    let rules = svc.attachment_rules(session.workspace_id).unwrap_or_default();
    let (_, role) = svc.attachment_standing(session.workspace_id).unwrap_or((ligus_core::ActorKind::Human, ligus_core::WorkspaceRole::Viewer));
    let placed = blob::placed_attachments(&session);
    let gone = svc.gone_blobs(session.workspace_id).unwrap_or_default();
    drop(svc);
    let cache_dir = blob::verified_cache_dir(&state.data_dir);
    let signed_by = |mid: &Uuid| session.authz.message_authors.get(mid).cloned();
    let own_local_paths = {
        let mut out = Vec::new();
        // Only turns the person wrote themselves (never an agent's reply or
        // a runtime effect signed under their identity).
        for m in session.messages.iter().filter(|m| session.authz.person_messages.get(&m.id) == Some(&me)) {
            let mut rest = m.body.as_str();
            while let Some(i) = rest.find("[Attached: ") {
                let after = &rest[i + "[Attached: ".len()..];
                let Some(end) = after.find(']') else { break };
                out.push(after[..end].trim().to_string());
                rest = &after[end + 1..];
            }
        }
        out
    };
    let shared = placed
        .values()
        .map(|s| {
            let marker = s.attachment.marker();
            let message_ids = session
                .messages
                .iter()
                .filter(|m| m.body.contains(&marker) && signed_by(&m.id).as_deref() == Some(s.author.as_str()))
                .map(|m| m.id.to_string())
                .collect();
            let a = &s.attachment;
            let class = attachment::class_of_extension(&attachment::extension(&a.name));
            AttachmentView {
                id: a.id.to_string(),
                // Shown as is: `invoice\u{202E}fdp.exe` read as "invoiceexe.pdf".
                name: crate::shown(&a.name),
                size: a.size,
                content_type: a.content_type.clone(),
                sha256: a.sha256.clone(),
                author: crate::shown(&s.author),
                sender: sender_of(s),
                author_kind: s.author_kind,
                deleted: s.deleted,
                blocked: rules.blocked.contains(&a.sha256),
                mine: s.author == me,
                can_delete: !s.deleted && (s.author == me || role.rank() >= ligus_core::WorkspaceRole::Admin.rank()),
                name_class: class,
                risky_by_name: class.risky() || attachment::double_extension(&a.name),
                from_agent_offer: a.proposal_id.is_some(),
                expired: !s.deleted && gone.contains(&a.blob_id) && !blob::cached_path(&cache_dir, a).exists(),
                message_ids,
            }
        })
        .collect();
    Ok(ChatAttachments { shared, own_local_paths })
}

/// What "Check" found: shown before anything is saved.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttachmentCheck {
    id: String,
    sha256: String,
    name: String,
    size: u64,
    sender: String,
    assessment: attachment::Assessment,
    archive: Option<attachment::ArchiveReport>,
    scan: safety::ScanResult,
    /// The workspace's policy refuses the detected type (saving is refused).
    blocked_type: Option<String>,
    /// Why saving is refused outright, if it is.
    refused: Option<String>,
    /// A second confirmation that names the sender is needed.
    needs_confirmation: bool,
}

/// The last check of each attachment on this device (what "Save" relies
/// on), by workspace and attachment: an attachment id is its sharer's choice,
/// so the same id in another workspace is another file.
fn checks() -> &'static Mutex<HashMap<(Uuid, Uuid), AttachmentCheck>> {
    static C: std::sync::OnceLock<Mutex<HashMap<(Uuid, Uuid), AttachmentCheck>>> = std::sync::OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Remember the check of attachment `id` in workspace `ws`.
fn remember_check(ws: Uuid, id: Uuid, check: AttachmentCheck) {
    checks().lock().unwrap().insert((ws, id), check);
}

/// The check "Save" relies on: of this attachment, in this workspace, of
/// these bytes.
fn checked(ws: Uuid, id: Uuid, sha256: &str) -> Option<AttachmentCheck> {
    checks().lock().unwrap().get(&(ws, id)).cloned().filter(|c| c.sha256 == sha256)
}

/// Download, verify and inspect a shared file (an explicit action). Nothing
/// is opened, previewed or written outside the private cache.
#[tauri::command]
pub(crate) async fn check_attachment(state: State<'_, AppState>, session_id: String, attachment_id: String) -> Result<AttachmentCheck, String> {
    let session = session_of(&state, &session_id)?;
    let s = shared_in(&session, &attachment_id)?;
    if !blob::placed_attachments(&session).contains_key(&s.attachment.id) {
        return Err("this attachment isn't in its sharer's own message".into());
    }
    let rules = state.service.lock().unwrap().attachment_rules(session.workspace_id).map_err(map_err)?;
    blob::receivable(&s, &rules).map_err(|e| e.to_string())?;
    let a = s.attachment.clone();
    let cache_dir = blob::verified_cache_dir(&state.data_dir);
    let bytes = match blob::cached(&cache_dir, &a) {
        Some(b) => b,
        None => {
            let (client, room, ring) = transport_for(&state, session.workspace_id)?;
            match blob::fetch_verified(&client, &room, &ring, &cache_dir, &a).await {
                Ok(b) => b,
                Err(e) => {
                    // Gone from the relay: remembered, so the chat shows it
                    // as expired from now on.
                    if e == blob::BlobError::NotFound {
                        let _ = state.service.lock().unwrap().mark_blob_gone(session.workspace_id, &a.blob_id, unix_now());
                    }
                    return Err(e.to_string());
                }
            }
        }
    };
    let name = a.name.clone();
    let (assessment, archive) = tauri::async_runtime::spawn_blocking(move || {
        let assessment = attachment::assess(&name, &bytes);
        let archive = safety::archive_report(&bytes, &assessment);
        (assessment, archive)
    })
    .await
    .map_err(map_err)?;
    let mut tokens = attachment::type_tokens(&a.name, Some(&assessment.detected));
    // What an archive holds counts too: a zip of programs is refused where
    // programs are.
    if let Some(r) = &archive {
        tokens.extend(r.entry_types.iter().cloned());
    }
    let blocked_type = rules.policy.file_refusal(&tokens, a.size).map(|why| {
        if archive.is_some() { format!("{why} (inside this archive)") } else { why }
    });
    let path = blob::cached_path(&cache_dir, &a);
    let scan = match safety::Scanner::from_env() {
        Some(scanner) => tauri::async_runtime::spawn_blocking(move || scanner.scan(&path)).await.unwrap_or(safety::ScanResult::Failed {
            scanner: "scanner".into(),
            detail: "the scan task failed".into(),
        }),
        None => safety::ScanResult::NotConfigured,
    };
    let refused = if let Some(b) = &blocked_type {
        Some(b.clone())
    } else if let Some(bomb) = archive.as_ref().and_then(|r| r.bomb.clone()) {
        Some(format!("this archive looks like a decompression bomb ({bomb})"))
    } else if let safety::ScanResult::Infected { scanner, detail } = &scan {
        Some(format!("{scanner} found a threat: {detail}"))
    } else {
        None
    };
    let needs_confirmation =
        assessment.risky || archive.as_ref().is_some_and(|r| !r.unsafe_names.is_empty() || r.risky_entries > 0 || r.hidden_entries || r.opaque_entries > 0);
    let check = AttachmentCheck {
        id: a.id.to_string(),
        sha256: a.sha256.clone(),
        name: a.name.clone(),
        size: a.size,
        sender: sender_of(&s),
        assessment,
        archive,
        scan,
        blocked_type,
        refused,
        needs_confirmation,
    };
    remember_check(session.workspace_id, a.id, check.clone());
    Ok(check)
}

/// Save a checked file where the person chooses (a save dialog), never
/// executable, quarantine-marked. Risky files need `confirmed` (the UI's
/// second confirmation, naming the sender). `Ok(None)` = cancelled.
#[tauri::command]
pub(crate) async fn save_received_attachment(ligus_app: tauri::AppHandle, session_id: String, attachment_id: String, confirmed: bool) -> Result<Option<String>, String> {
    crate::off_main(ligus_app, move |ligus_app| save_received_attachment_sync(tauri::Manager::state::<AppState>(ligus_app), session_id, attachment_id, confirmed)).await
}

pub(crate) fn save_received_attachment_sync(state: State<AppState>, session_id: String, attachment_id: String, confirmed: bool) -> Result<Option<String>, String> {
    let session = session_of(&state, &session_id)?;
    let s = shared_in(&session, &attachment_id)?;
    let rules = state.service.lock().unwrap().attachment_rules(session.workspace_id).map_err(map_err)?;
    blob::receivable(&s, &rules).map_err(|e| e.to_string())?;
    let a = &s.attachment;
    let check = checked(session.workspace_id, a.id, &a.sha256).ok_or("check the file first")?;
    if let Some(why) = &check.refused {
        return Err(why.clone());
    }
    if check.needs_confirmation && !confirmed {
        return Err(format!("this file needs a second confirmation: it is from {} and can run code when opened", check.sender));
    }
    // Re-verified from the private cache: what was checked is what is saved.
    let bytes = blob::cached(&blob::verified_cache_dir(&state.data_dir), a).ok_or("check the file again")?;
    // The policy may have changed since the check: judge the file as the
    // check does, the types inside an archive included.
    if let Some(why) = safety::policy_refusal(&rules.policy, &a.name, a.size, &bytes) {
        return Err(why);
    }
    let Some(chosen) = rfd::FileDialog::new().set_title("Save attachment").set_file_name(&a.name).save_file() else {
        return Ok(None);
    };
    let dir = chosen.parent().ok_or("choose a folder")?;
    let name = chosen.file_name().and_then(|n| n.to_str()).map(attachment::sanitize_filename).unwrap_or_else(|| a.name.clone());
    let saved = safety::save_received(dir, &name, &bytes, &check.sender).map_err(map_err)?;
    Ok(Some(saved.to_string_lossy().into_owned()))
}

/// An inert preview's bytes (raster images and plain text only), after a
/// check. The UI renders them in a sandboxed frame with no scripts or loads.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttachmentPreview {
    kind: attachment::PreviewKind,
    mime: String,
    data_base64: String,
}

#[tauri::command]
pub(crate) async fn preview_attachment(ligus_app: tauri::AppHandle, session_id: String, attachment_id: String) -> Result<AttachmentPreview, String> {
    crate::off_main(ligus_app, move |ligus_app| preview_attachment_sync(tauri::Manager::state::<AppState>(ligus_app), session_id, attachment_id)).await
}

pub(crate) fn preview_attachment_sync(state: State<AppState>, session_id: String, attachment_id: String) -> Result<AttachmentPreview, String> {
    let session = session_of(&state, &session_id)?;
    let s = shared_in(&session, &attachment_id)?;
    let rules = state.service.lock().unwrap().attachment_rules(session.workspace_id).map_err(map_err)?;
    blob::receivable(&s, &rules).map_err(|e| e.to_string())?;
    let a = &s.attachment;
    let bytes = blob::cached(&blob::verified_cache_dir(&state.data_dir), a).ok_or("check the file first")?;
    let assessment = attachment::assess(&a.name, &bytes);
    let kind = assessment.preview.ok_or("this file type has no safe preview; save it to open it")?;
    use base64::Engine;
    Ok(AttachmentPreview {
        kind,
        mime: assessment.detected.mime.to_string(),
        data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
    })
}

/// Delete a shared file (its sharer or an Admin): a signed deletion, then
/// the blob off the relay.
#[tauri::command]
pub(crate) async fn delete_attachment(state: State<'_, AppState>, session_id: String, attachment_id: String) -> Result<(), String> {
    let session = session_of(&state, &session_id)?;
    let s = shared_in(&session, &attachment_id)?;
    state.service.lock().unwrap().delete_attachment(session.id, session.workspace_id, s.attachment.id).map_err(map_err)?;
    blob::forget_cached(&blob::verified_cache_dir(&state.data_dir), &s.attachment);
    if let Ok((client, room, _)) = transport_for(&state, session.workspace_id) {
        if let Err(e) = client.delete_blob(&room, &s.attachment.blob_id).await {
            tracing::warn!(target: "attachments", "relay delete failed (the signed deletion stands): {e}");
        }
    }
    Ok(())
}

/// Remove every file a member shared in the active workspace (an Admin, after
/// removing them; or a member cleaning up their own). Returns how many.
#[tauri::command]
pub(crate) async fn delete_member_attachments(state: State<'_, AppState>, member_id: String) -> Result<usize, String> {
    let ws = state.active_workspace_id();
    let gone = state.service.lock().unwrap().delete_member_attachments(ws, &member_id).map_err(map_err)?;
    let cache = blob::verified_cache_dir(&state.data_dir);
    let transport = transport_for(&state, ws).ok();
    for a in &gone {
        blob::forget_cached(&cache, a);
        if let Some((client, room, _)) = &transport {
            let _ = client.delete_blob(room, &a.blob_id).await;
        }
    }
    Ok(gone.len())
}

/// The active workspace's attachment rules, for Settings and the composer.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttachmentRulesView {
    policy: AttachmentPolicy,
    recommended: AttachmentPolicy,
    blocked: Vec<String>,
    /// This person may change the policy and the blocklist (Admin).
    can_edit: bool,
    /// This person may share under the policy in force.
    can_share: bool,
    /// The workspace syncs through a relay (sharing is possible at all).
    relay: bool,
    local_only: bool,
    hard_max_bytes: u64,
}

#[tauri::command]
pub(crate) async fn attachment_rules(ligus_app: tauri::AppHandle) -> Result<AttachmentRulesView, String> {
    crate::off_main(ligus_app, move |ligus_app| attachment_rules_sync(tauri::Manager::state::<AppState>(ligus_app))).await
}

pub(crate) fn attachment_rules_sync(state: State<AppState>) -> Result<AttachmentRulesView, String> {
    let ws = state.active_workspace_id();
    let local_only = state.is_local_only_ws(ws);
    let svc = state.service.lock().unwrap();
    let rules = svc.attachment_rules(ws).map_err(map_err)?;
    let (kind, role) = svc.attachment_standing(ws).map_err(map_err)?;
    drop(svc);
    Ok(AttachmentRulesView {
        can_share: !local_only && rules.policy.sharer_refusal(kind, role).is_none(),
        can_edit: !local_only && role.rank() >= ligus_core::WorkspaceRole::Admin.rank() && kind == ligus_core::ActorKind::Human,
        relay: blob_transport(&state).is_some(),
        local_only,
        blocked: rules.blocked.iter().cloned().collect(),
        policy: rules.policy,
        recommended: AttachmentPolicy::recommended(),
        hard_max_bytes: attachment::HARD_MAX_BYTES,
    })
}

/// Set the attachment policy (Admin; asks for a passkey confirmation).
#[tauri::command(async)]
pub(crate) async fn set_attachment_policy(ligus_app: tauri::AppHandle, workspace_id: String, policy: AttachmentPolicy) -> Result<(), String> {
    crate::off_main(ligus_app, move |ligus_app| set_attachment_policy_sync(tauri::Manager::state::<AppState>(ligus_app), workspace_id, policy)).await
}

pub(crate) fn set_attachment_policy_sync(state: State<AppState>, workspace_id: String, policy: AttachmentPolicy) -> Result<(), String> {
    let ws = crate::shown_workspace(&state, &workspace_id)?;
    if state.is_local_only_ws(ws) {
        return Err("files in your own workspace stay on this device".into());
    }
    state.service.lock().unwrap().set_attachment_policy(ws, policy).map_err(map_err)
}

/// Block a file by its SHA-256 (Admin).
#[tauri::command(async)]
pub(crate) async fn block_attachment_hash(ligus_app: tauri::AppHandle, workspace_id: String, sha256: String) -> Result<(), String> {
    crate::off_main(ligus_app, move |ligus_app| block_attachment_hash_sync(tauri::Manager::state::<AppState>(ligus_app), workspace_id, sha256)).await
}

pub(crate) fn block_attachment_hash_sync(state: State<AppState>, workspace_id: String, sha256: String) -> Result<(), String> {
    let ws = crate::shown_workspace(&state, &workspace_id)?;
    state.service.lock().unwrap().block_attachment_hash(ws, &sha256).map_err(map_err)
}

/// Lift a block (Admin; asks for a passkey confirmation).
#[tauri::command(async)]
pub(crate) async fn unblock_attachment_hash(ligus_app: tauri::AppHandle, workspace_id: String, sha256: String) -> Result<(), String> {
    crate::off_main(ligus_app, move |ligus_app| unblock_attachment_hash_sync(tauri::Manager::state::<AppState>(ligus_app), workspace_id, sha256)).await
}

pub(crate) fn unblock_attachment_hash_sync(state: State<AppState>, workspace_id: String, sha256: String) -> Result<(), String> {
    let ws = crate::shown_workspace(&state, &workspace_id)?;
    state.service.lock().unwrap().unblock_attachment_hash(ws, &sha256).map_err(map_err)
}

fn unix_now() -> i64 {
    ligus_core::Timestamp::now().inner().unix_timestamp()
}

/// Renew the relay blobs of the active workspace's live attachments
/// (identity model v2, 10.5): the files this person shared, and every live
/// one when they are an Admin (this is a desktop). A low-priority background
/// job after a sync, at most once an hour; each blob at most once a day,
/// past a third of its life. A relay without the retain route changes
/// nothing; a blob it no longer has is shown as expired.
pub(crate) fn renew_attachment_blobs(app: &AppHandle) {
    use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
    static RUNNING: AtomicBool = AtomicBool::new(false);
    static LAST: AtomicI64 = AtomicI64::new(0);
    const EVERY_SECS: i64 = 60 * 60;
    let now = unix_now();
    if now - LAST.load(Ordering::SeqCst) < EVERY_SECS || RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let ws = state.active_workspace_id();
        if !state.is_local_only_ws(ws) {
            let due = state.service.lock().unwrap().blob_renewals_due(ws, true, now);
            match due {
                Ok(due) if !due.is_empty() => {
                    if let Ok((client, room, _)) = transport_for(&state, ws) {
                        let outcomes = blob::renew_blobs(&client, &room, &due).await;
                        for (b, o) in &outcomes {
                            if let blob::RenewalOutcome::Failed(e) = o {
                                tracing::debug!(target: "attachments", blob = %b.chars().take(8).collect::<String>(), "renewal will be retried: {e}");
                            }
                        }
                        if let Err(e) = state.service.lock().unwrap().record_blob_renewals(ws, &outcomes, now) {
                            tracing::warn!(target: "attachments", "recording blob renewals: {e}");
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(target: "attachments", "planning blob renewals: {e}"),
            }
        }
        LAST.store(now, Ordering::SeqCst);
        RUNNING.store(false, Ordering::SeqCst);
    });
}

/// Upload and share every agent offer this device produced that a person
/// approved (issue #118). Run after a sync; each staged file is re-checked
/// against the rules at the head before it leaves.
pub(crate) fn share_approved_offers(app: &AppHandle) {
    static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let ws = state.active_workspace_id();
        let (offers, settled) = {
            let svc = state.service.lock().unwrap();
            (svc.approved_offers(ws).unwrap_or_default(), svc.settled_offers(ws).unwrap_or_default())
        };
        // A rejected, dismissed, superseded or shared offer's staged file goes.
        for pid in settled {
            let _ = std::fs::remove_file(staged_dir(&state.data_dir).join(pid.to_string()));
        }
        let mut shared = 0;
        if !offers.is_empty() {
            if let Ok((client, room, ring)) = transport_for(&state, ws) {
                for (chat, pid, offer) in offers {
                    match share_offer(&state, &client, &room, &ring, ws, chat, pid, &offer).await {
                        Ok(()) => shared += 1,
                        Err(e) => tracing::warn!(target: "attachments", proposal = %pid, "couldn't share an approved offer: {e}"),
                    }
                }
            }
        }
        if shared > 0 {
            let _ = app.emit("workspace://synced", 1);
        }
        RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
    });
}

#[allow(clippy::too_many_arguments)]
async fn share_offer(
    state: &AppState,
    client: &ligus_runtime::RelayClient,
    room: &str,
    ring: &ligus_runtime::keyring::KeyringEngine,
    ws: Uuid,
    chat: Uuid,
    pid: Uuid,
    offer: &AttachmentRef,
) -> Result<(), String> {
    let bytes = std::fs::read(staged_dir(&state.data_dir).join(pid.to_string())).map_err(|_| "the offered file isn't staged on this device".to_string())?;
    let rules = state.service.lock().unwrap().attachment_rules(ws).map_err(map_err)?;
    let prepared = blob::check_file(&offer.name, bytes, &rules)?;
    if prepared.sha256 != offer.sha256 || prepared.name != offer.name || prepared.content_type != offer.content_type {
        return Err("the staged file isn't the one that was approved".into());
    }
    let mut att = blob::upload_as(client, room, ring, &prepared, offer.id).await.map_err(|e| e.to_string())?;
    att.proposal_id = Some(pid);
    let r = {
        let mut svc = state.service.lock().unwrap();
        // The share, then the message that places it in the chat (signed by
        // the same author, so every device shows it there).
        svc.share_attachment(chat, ws, att.clone()).and_then(|_| {
            svc.post_system_note(chat, ws, format!("\u{1f4ce} An agent's file, approved for sharing: {}", att.marker()))
        })
    };
    if let Err(e) = r {
        let _ = client.delete_blob(room, &att.blob_id).await;
        return Err(map_err(e));
    }
    let _ = blob::cache_verified(&blob::verified_cache_dir(&state.data_dir), &att, &prepared.plaintext);
    let _ = std::fs::remove_file(staged_dir(&state.data_dir).join(pid.to_string()));
    Ok(())
}

#[cfg(test)]
mod check_gate_tests {
    use super::*;

    fn check(sha: &str, sender: &str) -> AttachmentCheck {
        AttachmentCheck {
            id: String::new(),
            sha256: sha.into(),
            name: "a.txt".into(),
            size: 1,
            sender: sender.into(),
            assessment: attachment::assess("a.txt", b"a"),
            archive: None,
            scan: safety::ScanResult::NotConfigured,
            blocked_type: None,
            refused: None,
            needs_confirmation: false,
        }
    }

    /// An attachment id is the sharer's choice: the same id and bytes in
    /// another workspace (another sharer, another policy) needs its own
    /// check, and one workspace's check never lets "Save" through in another.
    #[test]
    fn a_check_counts_only_in_its_own_workspace() {
        let (a, b, id) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        remember_check(a, id, check("s1", "Ada"));
        assert_eq!(checked(a, id, "s1").map(|c| c.sender), Some("Ada".into()));
        assert!(checked(b, id, "s1").is_none(), "workspace B used A's check");
        assert!(checked(a, id, "s2").is_none(), "other bytes used the check");
        remember_check(b, id, check("s1", "Mallory"));
        assert_eq!(checked(a, id, "s1").map(|c| c.sender), Some("Ada".into()), "B's check replaced A's");
        assert_eq!(checked(b, id, "s1").map(|c| c.sender), Some("Mallory".into()));
    }
}
