//! Box admission on the CLI (Phase B): a `ligus serve` or `ligus worker` box
//! joins a workspace as its **own service member**, never as a person's
//! device.
//!
//! * `ligus admit request` prints the box's join request (`ligusjoin1:…`)
//!   and its fingerprint. An Admin approves it with a passkey on the desktop
//!   (or the phone) and hands back a code sealed to this box.
//! * `ligus admit redeem <code>` (or `LIGUS_ADMISSION_FILE` at start-up, the
//!   Kubernetes path) keeps the admission this box redeems when it adds
//!   itself. A bearer `hivews1:` admission is single use and expiring; the
//!   box's own device signature binds it to this box on redemption.
//! * `ligus admit issue` is the headless Admin path (CI, scripts): it issues
//!   an admission from this CLI identity, confirming with the Admin's
//!   recovery key (`--recovery-words-file`). Prefer the desktop or phone.
//!
//! [`prepare_box`] is the start-up both boxes share: sync first, refuse to go
//! on without a copy of the workspace, never found one, and join only
//! through an admission.

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use ligus_core::workspace_config_session_id;
use ligus_runtime::box_admission::{self, JoinRequest};
use ligus_runtime::device_enroll::Standing;
use ligus_runtime::secret_store::StoreKeyVault;
use ligus_runtime::{ChatService, EventStore, IdentityStore};
use uuid::Uuid;

use crate::{bootstrap_identity, load_file_config, uuid_of_room, Config};

/// The variable naming a file that holds an admission code (a mounted
/// Kubernetes Secret, or any file only this box's user can read).
pub const ADMISSION_FILE_ENV: &str = "LIGUS_ADMISSION_FILE";

/// Why `ligus serve enroll` and `LIGUS_SERVE_OWNER` stopped working.
pub const OWNER_MODE_REMOVED: &str = "a box no longer acts as a person's device: since 0.1 `ligus serve` and `ligus worker` join \
     the workspace as their own service member. Unset LIGUS_SERVE_OWNER (chart: serve.owner), then admit the box: run \
     `ligus admit request` on it and approve the request on your desktop (workspace menu > Admit a box) or phone, \
     or give it an admission code in LIGUS_ADMISSION_FILE. See the operator guide, Admitting a box";

fn identity(cfg: &Config) -> IdentityStore<StoreKeyVault> {
    IdentityStore::new(&cfg.data_dir, StoreKeyVault::for_data_dir(&cfg.data_dir))
}

/// Make this data dir a box: create its identity if needed and mark it a
/// service member (one way). Returns the identity.
pub fn mark_box(cfg: &Config) -> Result<ligus_runtime::StoredIdentity> {
    std::fs::create_dir_all(&cfg.data_dir)?;
    let ids = identity(cfg);
    bootstrap_identity(&ids)?;
    Ok(ids.mark_service()?)
}

/// This box's join request.
pub fn join_request(cfg: &Config, label: &str) -> Result<JoinRequest> {
    let stored = mark_box(cfg)?;
    let ka = identity(cfg).key_agreement(stored.device.id)?;
    Ok(JoinRequest::new(stored.account.id, stored.device.id, &stored.device.signing_public_key, &ka.public_key_bytes(), label))
}

/// Print the join request an Admin approves.
pub fn print_request(cfg: &Config, label: &str) -> Result<()> {
    let r = join_request(cfg, label)?;
    println!(
        "\nThis box isn't a member of {} yet. Ask an Admin to admit it: on the desktop, workspace menu > \
         Admit a box (or on the phone), paste this join request, check the fingerprint and approve with a passkey:\n\n  {}\n\n\
         Fingerprint: {}  (label \"{}\")\n\nThen run `ligus admit redeem <code>` here with the sealed code they send back, or put \
         the code in the file {ADMISSION_FILE_ENV} names, and start the box again.\n",
        // A fresh box has no workspace yet: the admission brings it.
        if cfg.relay_url.is_none() && cfg.room == "default" { "a workspace".to_string() } else { format!("workspace '{}'", cfg.room) },
        r.encode(),
        r.fingerprint(),
        r.label
    );
    Ok(())
}

/// What a redeemed admission set up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redeemed {
    pub room: String,
    pub invite: Option<Uuid>,
}

/// Keep the admission in `code` (sealed or bearer) for this box: its relay,
/// room, workspace key and epoch keys (unless this box already has them),
/// the invite it redeems when it adds itself, and the founder pin. Updates
/// `cfg` in place. Never writes an event.
pub fn redeem(cfg: &mut Config, code: &str) -> Result<Redeemed> {
    let stored = mark_box(cfg)?;
    let ka = identity(cfg).key_agreement(stored.device.id)?;
    let invite = box_admission::open_admission(code, &ka).map_err(|e| anyhow!(e))?;
    let (relay_url, room, key, inv, ek, pin) = crate::decode_ligusws_invite_full(&invite)?;
    if cfg.room != "default" && cfg.room != room {
        bail!("that admission is for workspace '{room}', but this box serves '{}' (LIGUS_WORKSPACE)", cfg.room);
    }
    let (Some(inv), Some(redeem_key)) = (inv, box_admission::redeem_key_of(&invite)) else {
        bail!("that code isn't a box admission: ask an Admin for one (workspace menu > Admit a box)");
    };
    let mut f = load_file_config(&cfg.data_dir);
    if cfg.relay_url.is_none() {
        if !crate::usable_relay_url(&relay_url) {
            bail!("that admission's workspace has no relay this box can use; set LIGUS_RELAY_URL");
        }
        cfg.relay_url = Some(relay_url.trim().to_string());
        f.relay_url = cfg.relay_url.clone();
    }
    cfg.room = room.clone();
    f.room = Some(room.clone());
    if cfg.key.is_none() {
        cfg.key = key.clone();
        f.key = key;
    }
    for k in ek {
        if !cfg.epoch_keys.iter().any(|h| h.v == k.v) {
            cfg.epoch_keys.push(k.clone());
        }
        if !f.ek.iter().any(|h| h.v == k.v) {
            f.ek.push(k);
        }
    }
    f.save(&cfg.data_dir)?;
    let store = EventStore::open(cfg.data_dir.join("ligus.db"))?;
    let ws = uuid_of_room(&room);
    store.set_join_invite(ws, inv)?;
    store.set_join_redeem_key(ws, &redeem_key)?;
    if let Some((founder, fk)) = pin {
        store.set_workspace_pin(ws, &founder, &fk)?;
    }
    Ok(Redeemed { room, invite: Some(inv) })
}

/// The admission code in `path` (a mounted Secret: trailing whitespace is
/// fine, nothing else is read).
pub fn read_code_file(path: &Path) -> Result<String> {
    let meta = std::fs::metadata(path).with_context(|| format!("reading the admission file {}", path.display()))?;
    if meta.len() as usize > box_admission::MAX_CODE_LEN {
        bail!("{} is too large to be an admission code", path.display());
    }
    Ok(std::fs::read_to_string(path)?.trim().to_string())
}

/// The admission code in the file `LIGUS_ADMISSION_FILE` names, if any. A
/// missing or empty file is no admission: the chart mounts the Secret as
/// optional so it can be deleted once the box has joined, and a restart
/// after that must not fail.
pub fn admission_from_file(path: Option<String>) -> Result<Option<String>> {
    let Some(path) = path else { return Ok(None) };
    let path = Path::new(path.trim());
    match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("ligus: {ADMISSION_FILE_ENV} names {} but it is gone (deleted once the box joined): no admission", path.display());
            Ok(None)
        }
        _ => Ok(Some(read_code_file(path)?).filter(|c| !c.is_empty())),
    }
}

/// Whether this box is already a member of `ws` in its local copy.
fn is_member(svc: &ChatService, ws: Uuid) -> Result<bool> {
    let me = svc.author().id.clone();
    Ok(svc.load_in(ws, workspace_config_session_id(ws))?.is_some_and(|c| c.members.iter().any(|m| m.id == me)))
}

/// The start-up shared by `ligus serve` and `ligus worker` (Phase B):
///
/// 1. this data dir is a box (a service member);
/// 2. an admission code from `admission` (the file `LIGUS_ADMISSION_FILE`
///    names) is kept, unless the box is already a member;
/// 3. **sync first**: the workspace must come from the relay. A box with no
///    copy of the workspace refuses to start, so it never seeds a competing
///    config log naming itself founder or Owner;
/// 4. a box enrolled as a person's device (the removed owner mode) refuses;
/// 5. the box adds itself through its admission. Not admitted: it prints its
///    join request and refuses.
///
/// `name` is the display name the box joins under (else its identity's).
/// `sync` runs one sync round. Returns the opened service, ready to register
/// its host, and whether the first sync succeeded.
pub async fn prepare_box<F, Fut>(
    cfg: &mut Config,
    label: &str,
    name: Option<&str>,
    admission: Option<String>,
    mut sync: F,
) -> Result<(ChatService, bool)>
where
    F: FnMut(Config) -> Fut,
    Fut: std::future::Future<Output = Result<(usize, usize)>>,
{
    mark_box(cfg)?;
    let ws = uuid_of_room(&cfg.room);
    if let Some(code) = admission.filter(|c| !c.trim().is_empty()) {
        let member = is_member(&crate::open_service(cfg)?, ws)?;
        if !member {
            let r = redeem(cfg, &code)?;
            println!("ligus: kept the box admission for workspace '{}'; joining it as this box's own member.", r.room);
        }
    }
    let ws = uuid_of_room(&cfg.room);
    if let Some(problem) = crate::relay_url_problem(cfg.relay_url.as_deref()) {
        bail!("{problem}");
    }
    let synced = sync(cfg.clone()).await;
    let svc = crate::open_service(cfg)?;
    if svc.load_in(ws, workspace_config_session_id(ws))?.is_none() {
        let why = match &synced {
            Err(e) => format!("the first sync failed ({e})"),
            Ok(_) => "the relay has no copy of it".to_string(),
        };
        bail!(
            "this box has no copy of the workspace '{}' yet: {why}. A box never founds a workspace; it joins one an Admin \
             admitted it to (check LIGUS_WORKSPACE, the relay URL and the admission)",
            cfg.room
        );
    }
    if let Err(e) = &synced {
        eprintln!("ligus: the first sync failed (continuing on the local copy, will retry): {e}");
    }
    drop(svc);
    let mut svc = crate::open_service(cfg)?;
    if let Some(n) = name.map(str::trim).filter(|n| !n.is_empty()) {
        svc.set_author_display_name(n.to_string());
    }
    match svc.device_standing(ws, None)? {
        Standing::OwnerDevice(owner) => bail!(
            "this box's device is certified as {}'s device; {OWNER_MODE_REMOVED}. Revoke it first (Settings > Account > My \
             devices > Revoke) and start the box from a fresh data dir",
            owner.display_name
        ),
        Standing::Revoked => return Ok((svc, synced.is_ok())),
        Standing::Own => {}
    }
    if !is_member(&svc, ws)? {
        let joined = svc.ensure_self_member(workspace_config_session_id(ws), ws);
        if !is_member(&svc, ws)? {
            let why = match joined {
                Err(e) => e.to_string(),
                Ok(_) => "it holds no admission".into(),
            };
            print_request(cfg, label)?;
            bail!("this box isn't a member of workspace '{}' and couldn't join: {why}", cfg.room);
        }
        println!("ligus: joined workspace '{}' as its own service member (Contributor).", cfg.room);
    }
    Ok((svc, synced.is_ok()))
}

/// `ligus admit <request|redeem|issue>`.
pub fn cmd_admit(cfg: &Config, positional: &[&String], flag: impl Fn(&str) -> Option<String>) -> Result<()> {
    let label = flag("--label").or_else(|| crate::env_opt("LIGUS_SERVE_LABEL")).or_else(ligus_runtime::machine_hostname).unwrap_or_else(|| "box".into());
    match positional.first().map(|s| s.as_str()) {
        Some("request") => print_request(cfg, &label),
        Some("redeem") => {
            let code = redeem_code(positional.get(1).map(|s| s.as_str()), flag("--file"), || {
                let mut s = String::new();
                std::io::Read::read_to_string(&mut std::io::Read::take(std::io::stdin(), box_admission::MAX_CODE_LEN as u64 + 1), &mut s)?;
                Ok(s)
            })?;
            let mut cfg = cfg.clone();
            let r = redeem(&mut cfg, &code)?;
            println!("Kept the box admission for workspace '{}'. Start `ligus serve` or `ligus worker`: it joins as its own member.", r.room);
            Ok(())
        }
        Some("issue") => cmd_issue(cfg, flag("--ttl"), flag("--request"), flag("--recovery-words-file"), flag("--out")),
        Some("close") => {
            let mut svc = crate::open_service(cfg)?;
            let n = svc.close_settled_admissions(uuid_of_room(&cfg.room))?;
            println!("closed {n} settled box admission(s). Sync to publish.");
            Ok(())
        }
        _ => bail!(
            "usage: ligus admit request [--label NAME] | redeem <ligusadmit1:code> | redeem - | redeem --file PATH | \
             issue [--ttl 1h] [--request ligusjoin1:...] --recovery-words-file PATH [--out PATH] | close"
        ),
    }
}

/// The code `ligus admit redeem` keeps: a sealed `ligusadmit1:` code on the
/// command line (only this box opens it), or any code from `--file PATH` or
/// standard input (`-`). A bearer `hivews1:` code carries the workspace key,
/// so it is refused on the command line, where `ps` and shell history keep it.
pub fn redeem_code(arg: Option<&str>, file: Option<String>, stdin: impl FnOnce() -> std::io::Result<String>) -> Result<String> {
    let code = match (arg, file) {
        (Some("-"), None) => stdin()?.trim().to_string(),
        (Some(c), None) => {
            if !c.trim().starts_with(box_admission::SEALED_PREFIX) {
                bail!(
                    "a bearer admission code carries the workspace key and the command line leaks it (ps, shell history): \
                     use `ligus admit redeem --file PATH` or pipe it to `ligus admit redeem -`"
                );
            }
            c.trim().to_string()
        }
        (None, Some(path)) => read_code_file(Path::new(&path))?,
        _ => bail!("usage: ligus admit redeem <ligusadmit1:code> | - | --file <path>"),
    };
    if code.len() > box_admission::MAX_CODE_LEN {
        bail!("that admission code is too long");
    }
    Ok(code)
}

/// `1h`, `90m`, `2d`, or plain seconds.
pub fn parse_ttl(s: &str) -> Result<i64> {
    let s = s.trim();
    let (n, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: i64 = n.parse().map_err(|_| anyhow!("--ttl takes a duration like 1h, 90m or 2d"))?;
    let per = match unit {
        "" | "s" => 1000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => bail!("--ttl takes a duration like 1h, 90m or 2d"),
    };
    let ms = n.checked_mul(per).unwrap_or(i64::MAX);
    if !(60_000..=ligus_core::admission::MAX_ADMISSION_TTL_MS - 60_000).contains(&ms) {
        bail!("--ttl must be between a minute and just under 7 days");
    }
    Ok(ms)
}

/// A presence provider from an Admin's recovery key (the headless path).
struct RecoveryPresence(ligus_core::presence::RecoveryKey);

impl ligus_runtime::chat_service::PresenceProvider for RecoveryPresence {
    fn assert(&self, req: &ligus_runtime::chat_service::PresenceRequest) -> std::result::Result<ligus_core::presence::PresenceAssertion, String> {
        Ok(self.0.assert(&req.challenge))
    }
}

/// The recovery key in a recovery-words file: a regular file only its owner
/// can read (the words are the person's presence), wiped once parsed.
fn recovery_from_file(path: &str) -> Result<ligus_core::presence::RecoveryKey> {
    crate::secret_arg::recovery_key_from_file(Path::new(path))
}

/// `ligus admit issue`: the headless Admin path.
fn cmd_issue(cfg: &Config, ttl: Option<String>, request: Option<String>, words: Option<String>, out: Option<String>) -> Result<()> {
    let request = request.map(|r| JoinRequest::decode(&r).map_err(|e| anyhow!(e))).transpose()?;
    let ttl = match ttl {
        Some(t) => parse_ttl(&t)?,
        None if request.is_some() => box_admission::DEFAULT_TTL_MS,
        None => box_admission::DEFAULT_BEARER_TTL_MS,
    };
    let Some(words) = words else {
        bail!("issuing an admission needs an Admin's presence: use the desktop or phone, or pass --recovery-words-file");
    };
    let ids = identity(cfg);
    if ids.load()?.is_some_and(|s| s.service) {
        bail!("this data dir is a box (a service member); an Admin issues admissions from their own identity");
    }
    let mut svc = crate::open_service(cfg)?;
    svc.set_presence_provider(Some(std::sync::Arc::new(RecoveryPresence(recovery_from_file(&words)?))));
    let ws = uuid_of_room(&cfg.room);
    let (inv, mut seed) = svc.issue_box_admission(ws, ttl, request.as_ref())?;
    let code = crate::serve::admission_code(cfg, &svc, inv, &seed);
    // The redeem seed is in the code now; no other copy is kept.
    zeroize::Zeroize::zeroize(&mut seed);
    let code = code?;
    let code = match &request {
        Some(r) => r.seal(&code).map_err(|e| anyhow!(e))?,
        None => code,
    };
    match out {
        Some(path) => {
            write_out(&path, code.as_bytes())?;
            println!("wrote the admission code to {path} (single use, expires in {} min). Sync to publish it.", ttl / 60_000);
        }
        None => println!("{code}"),
    }
    Ok(())
}

/// Asks the person at a terminal whether the phone shows this code's
/// fingerprint ([`confirm_phone_fingerprint_on_tty`]); `Ok(true)` is yes.
pub type ConfirmFingerprint<'a> = &'a mut dyn FnMut(&ligus_runtime::phone_enroll::PhoneCode) -> Result<bool>;

/// `ligus phones certify`: "Add this phone" for a headless identity. Certify
/// the phone whose code (`ligusphone1:…`, from the app's Settings > This
/// phone) is given, as a phone of this CLI identity's person, confirming with
/// their recovery key (`--recovery-words-file`). The check the desktop's
/// sheet asks a person to do is required: `--fingerprint` (the code is
/// refused unless its key has that fingerprint), or, without it, `confirm`
/// (asking the person at a terminal to compare it with the phone's screen).
/// With neither (not a terminal) it refuses. Never from a box. Sync to
/// publish.
pub fn cmd_phones_certify(
    cfg: &Config,
    code: Option<&str>,
    code_file: Option<String>,
    words: Option<String>,
    fingerprint: Option<String>,
    confirm: Option<ConfirmFingerprint<'_>>,
) -> Result<ligus_runtime::phone_enroll::PhoneCode> {
    use ligus_runtime::phone_enroll::{PhoneCode, PhoneStanding};
    let usage = "usage: ligus phones certify <ligusphone1:code> | --code-file PATH  --recovery-words-file PATH --fingerprint XXXX-XXXX-XXXX-XXXX-XXXX";
    let text = match (code, code_file) {
        (Some(c), None) => c.to_string(),
        (None, Some(path)) => read_code_file(Path::new(&path))?,
        _ => bail!(usage),
    };
    let phone = PhoneCode::decode(&text).map_err(|e| anyhow!(e))?;
    match (fingerprint, confirm) {
        (Some(f), _) => {
            if !f.trim().eq_ignore_ascii_case(&phone.fingerprint()) {
                bail!("the phone code's fingerprint is {}, not {}: don't certify it", phone.fingerprint(), f.trim());
            }
        }
        (None, Some(ask)) => {
            if !ask(&phone)? {
                bail!("not certified: the fingerprint wasn't confirmed against the phone's screen");
            }
        }
        (None, None) => bail!(
            "compare the fingerprint with the phone's screen (Settings > This phone) and pass it: --fingerprint XXXX-XXXX-XXXX-XXXX-XXXX (or run this at a terminal to confirm it there)"
        ),
    }
    let Some(words) = words else {
        bail!("certifying a phone needs its person's presence: use the desktop (My devices > Add this phone), or pass --recovery-words-file");
    };
    if identity(cfg).load()?.is_some_and(|s| s.service) {
        bail!("this data dir is a box (a service member); a phone is certified by its own person's identity");
    }
    let mut svc = crate::open_service(cfg)?;
    svc.set_presence_provider(Some(std::sync::Arc::new(RecoveryPresence(recovery_from_file(&words)?))));
    let ws = uuid_of_room(&cfg.room);
    if svc.phone_standing(ws, &phone)? == PhoneStanding::AlreadyMine {
        println!("phone {} ({}) is already one of your devices", phone.device_id, phone.fingerprint());
        return Ok(phone);
    }
    let id = svc.certify_phone_code(ws, &phone)?;
    println!(
        "certified phone '{}' ({id}, fingerprint {}) as one of your devices (the phone tier). Sync to publish it (`ligus sync`), give the box a few seconds to pull it, then tap Retry on the phone (or scan a new `ligus pair` link).",
        phone.label,
        phone.fingerprint()
    );
    Ok(phone)
}

/// `ligus phones certify` at a terminal without `--fingerprint`: show the
/// phone's fingerprint and ask the person to confirm the phone's screen shows
/// the same one. Anything but yes is no.
pub fn confirm_phone_fingerprint_on_tty(phone: &ligus_runtime::phone_enroll::PhoneCode) -> Result<bool> {
    use std::io::Write;
    let mut err = std::io::stderr();
    write!(
        err,
        "Phone '{}' ({})\nFingerprint: {}\nDoes the phone's screen (Settings > This phone) show this same fingerprint? [y/N] ",
        phone.label,
        phone.device_id,
        phone.fingerprint()
    )?;
    err.flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

/// Write a secret the user asked for (`--out PATH`) owner-only (0600) and
/// atomically. Unlike the data-dir writer it never creates or chmods the
/// parent directory: that directory is the user's (a home dir, `/tmp`, a
/// bind-mounted volume owned by someone else), so tightening it to 0700 is
/// not ours to do and fails with EPERM when we don't own it.
fn write_out(path: &str, bytes: &[u8]) -> Result<()> {
    let p = Path::new(path);
    let parent = p.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        bail!("{} is not a directory: create it first", parent.display());
    }
    ligus_runtime::private_file::write_private_atomic(p, bytes).with_context(|| format!("writing {path}"))
}

/// `ligus recovery-key create --out PATH`: register a recovery key for this
/// CLI identity (a founder's first credential, or with presence it already
/// holds) and write its 24 words to `PATH` (owner-only). Never for a box.
pub fn cmd_recovery_key(cfg: &Config, positional: &[&String], out: Option<String>) -> Result<()> {
    if positional.first().map(|s| s.as_str()) != Some("create") {
        bail!("usage: ligus recovery-key create --out PATH");
    }
    let out = out.ok_or_else(|| anyhow!("usage: ligus recovery-key create --out PATH"))?;
    // Never replace a file: it may hold the words of a key already
    // registered, and a failed registration below removes `out`.
    if std::fs::symlink_metadata(&out).is_ok() {
        bail!("{out} already exists: choose a new path (it may hold the words of a recovery key you already registered)");
    }
    if identity(cfg).load()?.is_some_and(|s| s.service) {
        bail!("this data dir is a box (a service member): it holds no passkey or recovery key");
    }
    let mut svc = crate::open_service(cfg)?;
    let account = svc.author().account_id.ok_or_else(|| anyhow!("this identity has no account"))?;
    let key = ligus_core::presence::RecoveryKey::generate().map_err(|e| anyhow!(e.0))?;
    let ws = uuid_of_room(&cfg.room);
    let mut words = key.words();
    let k2 = ligus_core::presence::RecoveryKey::from_words(&words).map_err(|e| anyhow!(e.0));
    // Words first: a key registered whose words could not be written is a
    // credential nobody holds.
    let written = k2.is_ok().then(|| write_out(&out, words.as_bytes()));
    zeroize::Zeroize::zeroize(&mut words);
    let k2 = k2?;
    written.unwrap_or(Ok(()))?;
    if let Err(e) = svc.register_passkey(ws, key.credential(account, "Recovery key"), &move |req| Ok(k2.assert(&req.challenge))) {
        let _ = std::fs::remove_file(&out);
        return Err(e.into());
    }
    println!("registered a recovery key; its 24 words are in {out} (keep them offline). Sync to publish it.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A recovery-words file others can read is refused, not used: the
    /// words are the person's presence (as the lt app's rule).
    #[cfg(unix)]
    #[test]
    fn a_recovery_words_file_others_can_read_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let key = ligus_core::presence::RecoveryKey::generate().unwrap();
        let f = dir.path().join("words");
        std::fs::write(&f, key.words()).unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(recovery_from_file(f.to_str().unwrap()).is_err(), "a 0644 recovery-words file was used");
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(recovery_from_file(f.to_str().unwrap()).is_ok());
        assert!(recovery_from_file(dir.path().to_str().unwrap()).is_err());
    }

    #[test]
    fn a_bearer_code_is_never_taken_from_the_command_line() {
        // Review R3: `ligus admit redeem hivews1:…` put the workspace key in
        // argv (ps, /proc, shell history).
        let no_stdin = || -> std::io::Result<String> { panic!("stdin not read") };
        assert!(redeem_code(Some("hivews1:secret"), None, no_stdin).is_err());
        assert!(redeem_code(Some(" hivews1:secret"), None, no_stdin).is_err());
        assert_eq!(redeem_code(Some("ligusadmit1:abc"), None, no_stdin).unwrap(), "ligusadmit1:abc");
        assert_eq!(redeem_code(Some("-"), None, || Ok("hivews1:secret\n".into())).unwrap(), "hivews1:secret");
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("code");
        std::fs::write(&f, "hivews1:secret\n").unwrap();
        assert_eq!(redeem_code(None, Some(f.to_string_lossy().into_owned()), no_stdin).unwrap(), "hivews1:secret");
        assert!(redeem_code(None, None, no_stdin).is_err());
    }

    #[test]
    fn a_deleted_admission_secret_is_no_admission() {
        // Review R4: the chart mounts the admission Secret as optional and
        // says to delete it once the box joined, but a missing file made
        // serve and the worker refuse to start, so the Secret (which holds
        // the workspace key) had to stay.
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("admission");
        assert_eq!(admission_from_file(Some(gone.to_string_lossy().into_owned())).unwrap(), None);
        std::fs::write(&gone, "\n").unwrap();
        assert_eq!(admission_from_file(Some(gone.to_string_lossy().into_owned())).unwrap(), None);
        std::fs::write(&gone, "ligusadmit1:x\n").unwrap();
        assert_eq!(admission_from_file(Some(gone.to_string_lossy().into_owned())).unwrap().as_deref(), Some("ligusadmit1:x"));
        assert_eq!(admission_from_file(None).unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn out_files_are_owner_only_and_leave_the_parent_alone() {
        // kind-e2e: `recovery-key create --out /work/recovery-words` in a
        // container (uid 65532) failed with a bare EPERM because the writer
        // chmodded the bind-mounted /work, owned by the runner, to 0700.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let out = dir.path().join("words");
        write_out(out.to_str().unwrap(), b"one two").unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"one two");
        assert_eq!(std::fs::metadata(&out).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777, 0o777);
        // Overwrite replaces; no temp files left.
        write_out(out.to_str().unwrap(), b"three").unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"three");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        // A missing parent is an error, never created.
        let missing = dir.path().join("nope").join("words");
        assert!(write_out(missing.to_str().unwrap(), b"x").unwrap_err().to_string().contains("not a directory"));
    }

    #[test]
    fn ttl_parses_and_stays_in_bounds() {
        assert_eq!(parse_ttl("1h").unwrap(), 3_600_000);
        assert_eq!(parse_ttl("90m").unwrap(), 5_400_000);
        assert_eq!(parse_ttl("2d").unwrap(), 172_800_000);
        assert!(parse_ttl("30s").is_err());
        assert!(parse_ttl("7d").is_err());
        assert!(parse_ttl("soon").is_err());
        // Overflow never wraps back into range (nor panics).
        assert!(parse_ttl("106751991167d").is_err());
        assert!(parse_ttl("9223372036854775807h").is_err());
    }
}
