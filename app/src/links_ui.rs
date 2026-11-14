//! Linked accounts (IDENTITY-AND-FORGES §2, §6.2): Settings → Profile →
//! Linked accounts, and the link chips on People.
//!
//! Linking: pick a forge and type your handle; Ligus asks the forge for the
//! stable id, drafts one statement per workspace you pick (each names your
//! account, its key, that workspace, the forge and the id), and you sign them
//! with an SSH key the forge already lists for you, with your own
//! `ssh-keygen -Y sign -n ligus-identity-link-v1@ligus.ai` (paste the result)
//! or through your ssh-agent. GitHub and GitLab also take the fallback of
//! adding your Ligus key to the forge's signing keys (D15), which links the
//! two accounts publicly; the UI says so.
//!
//! Checking: every device asks the forge itself (never the relay), with no
//! token, at most once a day per link (the cache is device-local), and only
//! the forges built in or added on this device. "Contact forges to check
//! linked identities" is on by default (D13).

use std::collections::BTreeMap;
use std::sync::Mutex;

use hive_core::identity_link::{IdentityLink, LinkMethod};
use hive_core::Timestamp;
use hive_runtime::chat_service::LinkDraft;
use hive_runtime::identity_links::{
    self, account_key_openssh, CachedCheck, ForgeConfig, LinkCheckCache, LinkContext, LinkFetcher, LinkStatus,
    SelfHostedForge, LINK_NAMESPACE,
};
use tauri::State;
use uuid::Uuid;

use crate::{map_err, save_settings, AppState};

/// Device-local link settings (`settings.json`; never synced).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LinkSettings {
    /// "Contact forges to check linked identities". `None` = on (D13).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) check_forges: Option<bool>,
    /// Self-managed forges this device may ask.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) forges: Vec<SelfHostedForge>,
    /// Links this device also carries on its contact record ("discoverable
    /// by this identity"), each signed for the contact's workspace.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) contact_links: Vec<IdentityLink>,
}

impl LinkSettings {
    pub(crate) fn is_default(&self) -> bool {
        *self == Self::default()
    }
    fn checks_on(&self) -> bool {
        self.check_forges.unwrap_or(true)
    }
}

/// `explicit`: the person asked for this check (linking their own account,
/// "Check now"), so hosts the link's owner chose may be asked too.
fn fetcher(state: &AppState, explicit: bool) -> LinkFetcher {
    let s = state.settings.lock().unwrap();
    LinkFetcher::new(ForgeConfig {
        check_forges: s.identity_links.checks_on(),
        self_hosted: s.identity_links.forges.clone(),
        ask_owner_chosen_hosts: explicit,
        ..Default::default()
    })
}

/// Links drafted and waiting for their signatures, by link id.
static PENDING: Mutex<BTreeMap<Uuid, Vec<(Uuid, IdentityLink)>>> = Mutex::new(BTreeMap::new());

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DraftStatementDto {
    workspace_id: String,
    workspace_name: String,
    /// The exact bytes to sign (ends with one newline).
    statement: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LinkDraftDto {
    link_id: String,
    provider: String,
    host: String,
    subject_id: String,
    handle: String,
    method: LinkMethod,
    statements: Vec<DraftStatementDto>,
    /// `ssh-keygen -Y sign -n ligus-identity-link-v1@ligus.ai -f <your key>`
    /// (reads the statement on stdin, prints the signature).
    sign_command: String,
    namespace: String,
    /// The keys the forge lists for this account that this machine's
    /// ssh-agent holds (sign with one of these, or with any listed key).
    agent_keys: Vec<String>,
    /// KeyPublished: the `ssh-ed25519 ...` line to add to the forge's
    /// signing keys.
    account_key_openssh: Option<String>,
}

fn workspaces_named(state: &AppState, ids: &[String]) -> Result<Vec<(Uuid, String)>, String> {
    let s = state.settings.lock().unwrap();
    let mut out = Vec::new();
    for id in ids {
        let id = Uuid::parse_str(id.trim()).map_err(|_| "not a workspace id".to_string())?;
        let w = s.workspaces.iter().find(|w| w.id() == id).ok_or("not one of your workspaces")?;
        out.push((id, w.display_name()));
    }
    Ok(out)
}

/// The keys in this machine's ssh-agent (`ssh-add -L`), best effort.
fn agent_keys() -> Vec<identity_links::SshPublicKey> {
    let Ok(out) = std::process::Command::new("ssh-add").arg("-L").output() else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout).lines().filter_map(identity_links::parse_openssh).collect()
}

/// Step 1: resolve the handle to the forge's stable id and draft one
/// statement per chosen workspace (plus, if `discoverable`, one for this
/// device's contact record).
#[tauri::command]
pub(crate) async fn identity_link_prepare(
    state: State<'_, AppState>,
    provider: String,
    host: String,
    handle: String,
    workspace_ids: Vec<String>,
    method: LinkMethod,
    discoverable: bool,
) -> Result<LinkDraftDto, String> {
    let provider = provider.trim().to_ascii_lowercase();
    let host = host.trim().to_ascii_lowercase();
    if method == LinkMethod::KeyPublished && !hive_core::identity_link::KEY_PUBLISHED_PROVIDERS.contains(&provider.as_str()) {
        return Err("Adding your Ligus key as a signing key is only offered on GitHub and GitLab: elsewhere it would grant push access.".into());
    }
    if !matches!(method, LinkMethod::SshSig | LinkMethod::KeyPublished) {
        return Err("That link method isn't available yet.".into());
    }
    let mut targets = workspaces_named(&state, &workspace_ids)?;
    if discoverable {
        let cid = state.settings.lock().unwrap().contact.as_ref().map(|c| c.cid.clone());
        let cid = cid.ok_or("This device has no contact yet; publish to workspaces first.")?;
        targets.push((hive_runtime::contacts::contact_workspace_id(&cid), "Your contact card".into()));
    }
    if targets.is_empty() {
        return Err("Pick at least one workspace to show this identity in.".into());
    }
    let f = fetcher(&state, true);
    let (subject_id, canonical) = f.resolve_handle(&provider, &host, &handle).await.map_err(|e| match e {
        identity_links::FetchError::Gone => format!("{host} has no account {handle:?}"),
        identity_links::FetchError::NotConfigured(why) => why,
        identity_links::FetchError::Inconsistent(why) | identity_links::FetchError::Transport(why) => {
            format!("Couldn't ask {host}: {why}")
        }
    })?;
    let draft = LinkDraft::new(&provider, &host, &subject_id, &canonical, method);
    let mut drafted = Vec::new();
    {
        let svc = state.service.lock().unwrap();
        for (ws, _) in &targets {
            drafted.push((*ws, svc.draft_identity_link(*ws, &draft).map_err(map_err)?));
        }
    }
    // Which agent keys the forge lists for this account (online, no token).
    let listed = match f.key_list(&provider, &host, &subject_id).await {
        Ok(l) => l.keys,
        Err(_) => Vec::new(),
    };
    let agent: Vec<String> = agent_keys()
        .into_iter()
        .filter(|k| listed.iter().any(|l| l.key.key_data() == k.key_data()))
        .filter_map(|k| k.to_openssh().ok())
        .collect();
    let account_key = state.service.lock().unwrap().account_public_key();
    let dto = LinkDraftDto {
        link_id: draft.link_id.to_string(),
        provider,
        host,
        subject_id,
        handle: canonical,
        method,
        statements: drafted
            .iter()
            .zip(&targets)
            .map(|((ws, l), (_, name))| DraftStatementDto { workspace_id: ws.to_string(), workspace_name: name.clone(), statement: l.statement.clone() })
            .collect(),
        sign_command: format!("ssh-keygen -Y sign -n {LINK_NAMESPACE} -f ~/.ssh/id_ed25519"),
        namespace: LINK_NAMESPACE.to_string(),
        agent_keys: agent,
        account_key_openssh: (method == LinkMethod::KeyPublished).then(|| account_key_openssh(&account_key)).flatten(),
    };
    PENDING.lock().unwrap().insert(draft.link_id, drafted);
    Ok(dto)
}

/// Sign every pending statement of `link_id` with `public_key` through this
/// machine's ssh-agent (`ssh-keygen -Y sign -f <public key>`; a hardware key
/// asks for its touch). Returns the armored signatures by workspace.
#[tauri::command]
pub(crate) async fn identity_link_sign_with_agent(link_id: String, public_key: String) -> Result<BTreeMap<String, String>, String> {
    let id = Uuid::parse_str(&link_id).map_err(|_| "unknown link".to_string())?;
    let drafted = PENDING.lock().unwrap().get(&id).cloned().ok_or("That link draft expired; start again.")?;
    let key = identity_links::parse_openssh(&public_key).ok_or("not an SSH public key")?;
    tokio::task::spawn_blocking(move || {
        let dir = std::env::temp_dir().join(format!("ligus-link-{}", Uuid::new_v4().simple()));
        std::fs::create_dir(&dir).map_err(|e| e.to_string())?;
        let pubfile = dir.join("key.pub");
        let res = (|| {
            std::fs::write(&pubfile, key.to_openssh().map_err(|e| e.to_string())? + "\n").map_err(|e| e.to_string())?;
            let mut out = BTreeMap::new();
            for (ws, l) in &drafted {
                use std::io::Write;
                let mut child = std::process::Command::new("ssh-keygen")
                    .args(["-Y", "sign", "-n", LINK_NAMESPACE, "-f"])
                    .arg(&pubfile)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .map_err(|e| format!("couldn't run ssh-keygen: {e}"))?;
                child.stdin.take().ok_or("ssh-keygen stdin")?.write_all(l.statement.as_bytes()).map_err(|e| e.to_string())?;
                let o = child.wait_with_output().map_err(|e| e.to_string())?;
                if !o.status.success() {
                    return Err(format!("ssh-keygen: {}", String::from_utf8_lossy(&o.stderr).trim()));
                }
                out.insert(ws.to_string(), String::from_utf8_lossy(&o.stdout).to_string());
            }
            Ok(out)
        })();
        let _ = std::fs::remove_dir_all(&dir);
        res
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PublishedDto {
    /// Workspaces the link is now shown in.
    published: Vec<String>,
    /// What the forge says right now (it may not list the key yet).
    status: LinkStatus,
}

/// Step 2: check the signatures (pasted or from the agent), then publish
/// the link to each workspace. `signatures` maps workspace id → armored
/// SSHSIG; empty for a published key.
#[tauri::command]
pub(crate) async fn identity_link_publish(
    state: State<'_, AppState>,
    link_id: String,
    signatures: BTreeMap<String, String>,
) -> Result<PublishedDto, String> {
    let id = Uuid::parse_str(&link_id).map_err(|_| "unknown link".to_string())?;
    let drafted = PENDING.lock().unwrap().get(&id).cloned().ok_or("That link draft expired; start again.")?;
    let account_key = state.service.lock().unwrap().account_public_key();
    let mut ready = Vec::new();
    for (ws, mut l) in drafted {
        if l.method == LinkMethod::SshSig {
            let sig = signatures.get(&ws.to_string()).map(|s| s.trim()).filter(|s| !s.is_empty()).ok_or("Sign every statement (one per workspace).")?;
            let ok = identity_links::verify_sshsig(sig, LINK_NAMESPACE, l.statement.as_bytes())
                .map_err(|e| format!("That signature doesn't check out: {e}"))?;
            l.proof = sig.to_string();
            l.key_fingerprint = ok.fingerprint;
        }
        ready.push((ws, l));
    }
    // Ask the forge once before publishing, so a key it doesn't list is
    // caught now (an unreachable forge doesn't block: others will check).
    let f = fetcher(&state, true);
    let (ws0, l0) = ready.first().cloned().ok_or("nothing to publish")?;
    let status = identity_links::check_link(&l0, LinkContext { workspace_id: ws0, account_key: Some(&account_key) }, &f, Timestamp::now(), None).await;
    if let LinkStatus::Stale { reason, .. } | LinkStatus::Invalid { reason } = &status {
        return Err(format!("{}: {reason}", l0.host));
    }
    let contact_ws = state.settings.lock().unwrap().contact.as_ref().map(|c| hive_runtime::contacts::contact_workspace_id(&c.cid));
    let mut published = Vec::new();
    for (ws, l) in ready {
        if Some(ws) == contact_ws {
            let mut s = state.settings.lock().unwrap();
            s.identity_links.contact_links.retain(|x| x.subject_key() != l.subject_key());
            s.identity_links.contact_links.push(l);
            if let Some(c) = s.contact.as_mut() {
                c.published.values_mut().for_each(|t| *t = 0);
            }
            save_settings(&state.data_dir, &s);
            continue;
        }
        state.service.lock().unwrap().publish_identity_link(ws, l).map_err(map_err)?;
        published.push(ws.to_string());
    }
    PENDING.lock().unwrap().remove(&id);
    if contact_ws.is_some() {
        crate::people::ensure_contact(&state).await;
    }
    Ok(PublishedDto { published, status })
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LinkDto {
    link_id: String,
    provider: String,
    host: String,
    subject_id: String,
    handle: String,
    method: LinkMethod,
    issued_at: Timestamp,
    expires_at: Timestamp,
    workspaces: Vec<crate::people::SharedWorkspaceDto>,
    /// The cached status (no request), or a fresh one when checked.
    status: Option<LinkStatus>,
    discoverable: bool,
}

/// The links of `account` across the workspaces this device holds, merged
/// by the identity they name, each with its cached status for the first
/// workspace. Only workspaces whose log registers the account's agreed key
/// count (`chat_service::links_for_account`).
/// A link as shown, the workspace its status is checked in, the link, and
/// the account key there.
type ShownLink = (LinkDto, Uuid, IdentityLink, Option<Vec<u8>>);

fn links_of(state: &AppState, account: Uuid) -> Vec<ShownLink> {
    let conns: Vec<(Uuid, String)> = {
        let s = state.settings.lock().unwrap();
        s.workspaces.iter().filter(|w| w.dm_account.is_none()).map(|w| (w.id(), w.display_name())).collect()
    };
    let (contact_links, checks_on) = {
        let s = state.settings.lock().unwrap();
        (s.identity_links.contact_links.clone(), s.identity_links.checks_on())
    };
    let svc = state.service.lock().unwrap();
    let per_ws: Vec<(Uuid, hive_runtime::chat_service::WorkspaceLinks)> =
        conns.iter().filter_map(|(ws, _)| svc.identity_links(*ws).ok().map(|wl| (*ws, wl))).collect();
    let me = svc.author().account_id;
    // The key this account's links must be checked under: this device's own
    // for its account; the one the co-member directory agrees on (no
    // conflict) for anyone else; on a delegated device, the one every
    // workspace agrees on.
    let agreed: Option<Vec<u8>> = if me == Some(account) && !svc.is_delegated() {
        Some(svc.account_public_key().to_vec())
    } else {
        let keys: std::collections::BTreeSet<Vec<u8>> =
            per_ws.iter().filter_map(|(_, wl)| wl.account_keys.get(&account).cloned()).collect();
        let fp = (me != Some(account))
            .then(|| {
                hive_runtime::directory_trust::co_members(svc.store(), me.unwrap_or_default())
                    .into_iter()
                    .find(|c| c.account_id == account)
                    .filter(|c| !c.key_conflict)
                    .and_then(|c| c.fingerprint)
            })
            .flatten();
        match fp {
            Some(fp) => keys.into_iter().find(|k| hive_runtime::envelope_verifier::FounderPin::fingerprint_hex(k).eq_ignore_ascii_case(&fp)),
            None if me == Some(account) && keys.len() == 1 => keys.into_iter().next(),
            None => None,
        }
    };
    let Some(agreed) = agreed else { return Vec::new() };
    let now = Timestamp::now();
    let names: BTreeMap<Uuid, String> = conns.into_iter().collect();
    hive_runtime::chat_service::links_for_account(&per_ws, account, |k| k == agreed.as_slice())
        .into_iter()
        .map(|shown| {
            let l = shown.link;
            let ws = shown.workspaces[0];
            let cached = svc.store().get_check(&identity_links::cache_key(&l, ws, &shown.account_key));
            let ctx = LinkContext { workspace_id: ws, account_key: Some(&shown.account_key) };
            let status = identity_links::cached_status(cached.as_ref(), &l, ctx, now, checks_on);
            let dto = LinkDto {
                link_id: l.link_id.to_string(),
                provider: l.provider.clone(),
                host: l.host.clone(),
                subject_id: l.subject_id.clone(),
                handle: l.handle_at_link.clone(),
                method: l.method,
                issued_at: l.issued_at,
                expires_at: l.expires_at,
                workspaces: shown
                    .workspaces
                    .iter()
                    .map(|w| crate::people::SharedWorkspaceDto { id: w.to_string(), name: names.get(w).cloned().unwrap_or_default() })
                    .collect(),
                status,
                discoverable: contact_links.iter().any(|c| c.subject_key() == l.subject_key()),
            };
            (dto, ws, l, Some(shown.account_key))
        })
        .collect()
}

/// Your linked accounts, with the cached status of each.
#[tauri::command]
pub(crate) fn my_identity_links(state: State<AppState>) -> Vec<LinkDto> {
    let Some(me) = state.service.lock().unwrap().author().account_id else { return Vec::new() };
    links_of(&state, me).into_iter().map(|(d, ..)| d).collect()
}

/// Check `account`'s links against their forges (the cache answers within
/// 24 h unless `force`). Opening a profile calls this.
#[tauri::command]
pub(crate) async fn identity_link_statuses(state: State<'_, AppState>, account_id: String, force: bool) -> Result<Vec<LinkDto>, String> {
    let account = Uuid::parse_str(account_id.trim()).map_err(|_| "not an account id".to_string())?;
    let f = fetcher(&state, force);
    let now = Timestamp::now();
    let mut out = Vec::new();
    for (mut dto, ws, link, key) in links_of(&state, account) {
        let ctx = LinkContext { workspace_id: ws, account_key: key.as_deref() };
        let ck = identity_links::cache_key(&link, ws, key.as_deref().unwrap_or_default());
        let cached = state.service.lock().unwrap().store().get_check(&ck);
        let offline = identity_links::offline_check(&link, ctx, now);
        let status = match offline {
            Err(s) => s,
            Ok(_) => match cached.as_ref().filter(|c| !force && f.config().check_forges && identity_links::fresh(c, now)) {
                Some(c) => c.status.clone(),
                None => {
                    let last = cached.as_ref().and_then(|c| match &c.status {
                        LinkStatus::Verified { checked_at, .. } => Some(*checked_at),
                        _ => c.last_verified_at,
                    });
                    let s = identity_links::check_link(&link, ctx, &f, now, last).await;
                    if matches!(s, LinkStatus::Verified { .. } | LinkStatus::Stale { .. } | LinkStatus::Unreachable { .. }) {
                        let lv = if s.is_verified() { Some(now) } else { last };
                        state.service.lock().unwrap().store().put_check(&ck, &CachedCheck { status: s.clone(), checked_at: now, last_verified_at: lv });
                    }
                    s
                }
            },
        };
        dto.status = Some(status);
        out.push(dto);
    }
    Ok(out)
}

/// Unlink: remove the link from every workspace it is shown in (and from
/// this device's contact card).
#[tauri::command]
pub(crate) async fn identity_link_remove(state: State<'_, AppState>, link_id: String) -> Result<u32, String> {
    let id = Uuid::parse_str(&link_id).map_err(|_| "unknown link".to_string())?;
    let me = state.service.lock().unwrap().author().account_id.ok_or("no account yet")?;
    let mut removed = 0u32;
    let mut subject = None;
    for (dto, _, l, _) in links_of(&state, me) {
        if dto.link_id != link_id {
            continue;
        }
        subject = Some((l.provider.clone(), l.host.clone(), l.subject_id.clone()));
        for w in dto.workspaces {
            let ws = Uuid::parse_str(&w.id).map_err(|e| e.to_string())?;
            state.service.lock().unwrap().remove_identity_link(ws, me, id).map_err(map_err)?;
            removed += 1;
        }
    }
    let had_contact = {
        let mut s = state.settings.lock().unwrap();
        let before = s.identity_links.contact_links.len();
        s.identity_links
            .contact_links
            .retain(|c| c.link_id != id && subject.as_ref().is_none_or(|(p, h, sub)| (p.as_str(), h.as_str(), sub.as_str()) != c.subject_key()));
        let changed = s.identity_links.contact_links.len() != before;
        if changed {
            if let Some(c) = s.contact.as_mut() {
                c.published.values_mut().for_each(|t| *t = 0);
            }
            save_settings(&state.data_dir, &s);
        }
        changed
    };
    if had_contact {
        crate::people::ensure_contact(&state).await;
    }
    Ok(removed)
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LinkSettingsDto {
    check_forges: bool,
    forges: Vec<SelfHostedForge>,
}

#[tauri::command]
pub(crate) fn identity_link_settings(state: State<AppState>) -> LinkSettingsDto {
    let s = state.settings.lock().unwrap();
    LinkSettingsDto { check_forges: s.identity_links.checks_on(), forges: s.identity_links.forges.clone() }
}

/// Turn forge checks on or off, and set the self-managed forges this device
/// may ask (HTTPS base URLs only).
#[tauri::command]
pub(crate) fn set_identity_link_settings(state: State<AppState>, settings: LinkSettingsDto) -> Result<(), String> {
    let mut forges = Vec::new();
    for f in settings.forges {
        let provider = f.provider.trim().to_ascii_lowercase();
        if !matches!(provider.as_str(), "github" | "gitlab" | "forgejo" | "gitea") {
            return Err(format!("{provider}: self-managed GitHub, GitLab, Forgejo or Gitea only"));
        }
        let url = reqwest::Url::parse(f.base_url.trim()).map_err(|_| format!("{}: not a URL", f.base_url))?;
        if url.scheme() != "https" || url.host_str().is_none() || !url.username().is_empty() {
            return Err(format!("{}: an https:// address without credentials", f.base_url));
        }
        let host = match url.port() {
            Some(p) => format!("{}:{p}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_string(),
        }
        .to_ascii_lowercase();
        forges.push(SelfHostedForge { provider, host, base_url: url.as_str().trim_end_matches('/').to_string() });
    }
    let mut s = state.settings.lock().unwrap();
    s.identity_links.check_forges = (!settings.check_forges).then_some(false);
    s.identity_links.forges = forges;
    save_settings(&state.data_dir, &s);
    Ok(())
}

/// The links a co-member list shows as chips (from the config logs; the
/// status is whatever this device cached, no request).
pub(crate) fn chips_for(state: &AppState, account: Uuid) -> Vec<LinkChipDto> {
    links_of(state, account)
        .into_iter()
        .filter(|(d, ..)| !matches!(d.status, Some(LinkStatus::Invalid { .. })))
        .map(|(d, ..)| LinkChipDto { provider: d.provider, host: d.host, handle: d.handle, subject_id: d.subject_id, status: d.status })
        .collect()
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LinkChipDto {
    provider: String,
    host: String,
    handle: String,
    subject_id: String,
    status: Option<LinkStatus>,
}

/// This device's contact-record link publications (opt-in), signed for the
/// contact's workspace.
pub(crate) fn contact_link_envelopes(state: &AppState, cid: &str) -> Vec<hive_core::SessionEventEnvelope> {
    let links = state.settings.lock().unwrap().identity_links.contact_links.clone();
    let ws = hive_runtime::contacts::contact_workspace_id(cid);
    let svc = state.service.lock().unwrap();
    let key = svc.account_public_key();
    links
        .into_iter()
        .filter(|l| l.expires_at > Timestamp::now())
        .filter(|l| hive_core::identity_link::check_statement(l, ws, Some(&key)).is_ok())
        .map(|l| svc.signed_link_publication(ws, l))
        .collect()
}
