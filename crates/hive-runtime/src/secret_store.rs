//! Ligus's own secrets in the OS secret store (Phase 0, B2).
//!
//! Identity seeds, this device's key-agreement key, the workspace passphrase
//! and delivered epoch keys, relay tokens, the relay issuer key, provider API
//! keys, MCP tokens and the langstroth-auth refresh token all live behind
//! [`SecretStore`]; `settings.json` and `config.toml` keep only `{"set": true}`
//! placeholders.
//!
//! | Backend | Where |
//! |---|---|
//! | [`MacKeychain`] | macOS: a **dedicated keychain file inside the Ligus data dir**, unlocked with a random password kept in the login keychain |
//! | [`SecretService`] | Linux desktop: the session's Secret Service (through `secret-tool`) |
//! | [`SystemdCreds`] | headless Linux (serve, worker): `systemd-creds encrypt` (TPM2 when present, else the host key) |
//! | [`K8sSecretFile`] | the chart: read-only mounted Secret files (operator input), runtime secrets in [`File0600`] under the data dir |
//! | [`File0600`] | fallback: owner-only files, with a warning (on macOS only when chosen: `LIGUS_SECRET_BACKEND=file`) |
//!
//! ## Agents can't reach Ligus's items
//!
//! Track C keeps Keychain *service* access for subscription CLIs (Claude
//! Code's login lives in the login keychain), so an item in the login
//! keychain would be one system prompt away from an agent. Ligus's items are
//! therefore **not** in the login keychain: they live in their own keychain
//! file under the data dir, which every agent sandbox denies (`file-read*`
//! of the data dir, Seatbelt and bubblewrap alike), and that keychain stays
//! out of the user's keychain search list. Only its unlock password is a
//! login-keychain item, and that item's access list trusts only the Ligus
//! binary (by its designated requirement when signed: Apple anchor, bundle
//! id `ai.ligus.desktop`, team id; a release app refuses to create it unless
//! it satisfies that requirement). Even an agent that talked a user into
//! allowing it would get a password for a file it can't open. A development
//! build is pinned by its code hash, so every rebuild asks once for the login
//! keychain password. On Linux the Secret Service is a D-Bus session
//! service the bubblewrap profile never binds, and `DBUS_SESSION_BUS_ADDRESS`
//! is outside the child env allow-list.
//!
//! Backend selection is device-local (`LIGUS_SECRET_BACKEND`, or the OS
//! default); it never affects what a fold decides.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use zeroize::Zeroizing;

/// The keychain service (and Secret Service attribute) Ligus's items use.
pub const SERVICE: &str = "ai.ligus.secrets";

/// The code-signing identifier of a standalone Ligus binary (not the desktop
/// app bundle), set once at startup with [`set_binary_identifier`].
static BINARY_IDENTIFIER: OnceLock<String> = OnceLock::new();

/// Name this process's signing identifier (`ai.ligus.term` for the terminal
/// app), before the secret store is opened. On macOS a release build signed
/// with Ligus's team then holds the keychain's unlock password under the same
/// designated requirement check as the desktop app (Apple anchor, this
/// identifier, the team id) instead of only being pinned by code hash; a
/// development build (no team id) is unchanged. The first call wins; later
/// calls with another identifier return `false`. Other platforms keep it but
/// don't use it.
pub fn set_binary_identifier(identifier: &str) -> bool {
    let id = identifier.trim();
    if id.is_empty() {
        return false;
    }
    BINARY_IDENTIFIER.get_or_init(|| id.to_string()) == id
}

/// The identifier [`set_binary_identifier`] named, if any.
pub fn binary_identifier() -> Option<&'static str> {
    BINARY_IDENTIFIER.get().map(String::as_str)
}

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{backend}: {message}")]
    Backend { backend: &'static str, message: String },
    #[error("{0} is read-only here")]
    ReadOnly(&'static str),
}

fn backend_err(backend: &'static str, message: impl Into<String>) -> SecretError {
    SecretError::Backend { backend, message: message.into() }
}

/// One secret: a scope (`seed`, `settings`, `cli`) and a name within it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SecretId {
    pub scope: &'static str,
    pub name: String,
}

impl SecretId {
    pub fn new(scope: &'static str, name: impl Into<String>) -> Self {
        Self { scope, name: name.into() }
    }
    /// `<scope>:<name>` (the keychain account / Secret Service attribute).
    pub fn account(&self) -> String {
        format!("{}:{}", self.scope, self.name)
    }
    /// A file-name-safe form of the name.
    fn file_name(&self) -> String {
        self.name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' })
            .collect()
    }
}

/// Which backend a store is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    MacKeychain,
    SecretService,
    SystemdCreds,
    K8sSecretFile,
    File0600,
    /// No usable store, and plain files weren't chosen (macOS): every use
    /// fails with the reason ([`Unavailable`]).
    Unavailable,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::MacKeychain => "keychain",
            Backend::SecretService => "secret-service",
            Backend::SystemdCreds => "systemd-creds",
            Backend::K8sSecretFile => "k8s",
            Backend::File0600 => "file",
            Backend::Unavailable => "unavailable",
        }
    }
    /// Whether secrets sit in plain owner-only files (the UI and `ligus
    /// doctor` warn).
    pub fn is_plain_file(self) -> bool {
        matches!(self, Backend::File0600)
    }
}

pub trait SecretStore: Send + Sync {
    fn backend(&self) -> Backend;
    fn get(&self, id: &SecretId) -> Result<Option<Zeroizing<Vec<u8>>>, SecretError>;
    fn put(&self, id: &SecretId, value: &[u8]) -> Result<(), SecretError>;
    fn delete(&self, id: &SecretId) -> Result<(), SecretError>;

    /// A UTF-8 secret.
    fn get_string(&self, id: &SecretId) -> Result<Option<Zeroizing<String>>, SecretError> {
        Ok(self.get(id)?.and_then(|b| String::from_utf8(b.to_vec()).ok().map(Zeroizing::new)))
    }
}

// ---------------------------------------------------------------------------
// File0600
// ---------------------------------------------------------------------------

/// Owner-only files (0600 in 0700 dirs), written atomically. Seeds keep the
/// historical `keys/<id>.seed` layout; everything else is under
/// `secrets/<scope>/`.
pub struct File0600 {
    data_dir: PathBuf,
}

impl File0600 {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self { data_dir: data_dir.as_ref().to_path_buf() }
    }

    fn path(&self, id: &SecretId) -> PathBuf {
        if id.scope == "seed" {
            self.data_dir.join("keys").join(format!("{}.seed", id.file_name()))
        } else {
            self.data_dir.join("secrets").join(id.scope).join(id.file_name())
        }
    }
}

/// Write `bytes` to `path` owner-only and atomically, its parent dir made
/// private ([`crate::private_file::ensure_private_dir`]: 0700 when ours, never
/// a chmod of a mount root that isn't) ([`crate::private_file::write_private_atomic`]).
pub fn write_private_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        crate::private_file::ensure_private_dir(dir)?;
    }
    crate::private_file::write_private_atomic(path, bytes)
}

/// Register every string inside a secret value for redaction.
fn register_strings(v: &serde_json::Value) {
    match v {
        serde_json::Value::String(s) => crate::secret_redact::register(s),
        serde_json::Value::Array(xs) => xs.iter().for_each(register_strings),
        serde_json::Value::Object(m) => m.values().for_each(register_strings),
        _ => {}
    }
}

impl SecretStore for File0600 {
    fn backend(&self) -> Backend {
        Backend::File0600
    }
    fn get(&self, id: &SecretId) -> Result<Option<Zeroizing<Vec<u8>>>, SecretError> {
        match std::fs::read(self.path(id)) {
            Ok(b) => Ok(Some(Zeroizing::new(b))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    fn put(&self, id: &SecretId, value: &[u8]) -> Result<(), SecretError> {
        let path = self.path(id);
        // `secrets/` itself too, not only `secrets/<scope>/`: every dir
        // between the data dir and the file is owner-only.
        // The data dir itself is left alone here: it may be a volume's mount
        // root that isn't ours to chmod.
        if let Some(top) = path.strip_prefix(&self.data_dir).ok().and_then(|r| r.components().next()) {
            let top = self.data_dir.join(top);
            if top != path {
                crate::private_file::ensure_private_dir(&top)?;
            }
        }
        Ok(write_private_atomic(&path, value)?)
    }
    fn delete(&self, id: &SecretId) -> Result<(), SecretError> {
        match std::fs::remove_file(self.path(id)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

// ---------------------------------------------------------------------------
// K8sSecretFile
// ---------------------------------------------------------------------------

/// Where [`K8sSecretFile`] looks when `LIGUS_SECRETS_DIR` is unset (with
/// `LIGUS_SECRET_BACKEND=k8s`). Hosts deny it to agent processes.
pub const K8S_SECRETS_DIR_DEFAULT: &str = "/var/run/secrets/ligus";

/// Read-only Secret files mounted by the operator (`<dir>/<scope>.<name>`),
/// layered over [`File0600`] under the data dir.
///
/// * **Reads**: the mounted file wins (the operator's Secret is the source of
///   truth for what it provisions: the workspace key, the relay token), else
///   the data dir.
/// * **Writes**: everything a box derives at runtime (identity seeds,
///   key-agreement keys, delivered epoch keys, the redeemed admission's
///   secrets) goes to the data dir: 0600 files in 0700 dirs on the box's own
///   ReadWriteOncePod volume, which agent sandboxes deny like the mount.
/// * Writing a mounted item **its own value** is a no-op: a box re-saving
///   its connection after redeeming an admission (`config.toml` and its
///   `cli.*` secrets) must not fail on the key the Secret already holds. A
///   *different* value, or deleting a mounted item, is refused: it would
///   either be shadowed by the mount on the next read or silently override
///   the operator.
pub struct K8sSecretFile {
    mount: PathBuf,
    fallback: File0600,
}

impl K8sSecretFile {
    pub fn new(mount: impl AsRef<Path>, data_dir: impl AsRef<Path>) -> Self {
        Self { mount: mount.as_ref().to_path_buf(), fallback: File0600::new(data_dir) }
    }
    fn mounted(&self, id: &SecretId) -> PathBuf {
        self.mount.join(format!("{}.{}", id.scope, id.file_name()))
    }
}

impl SecretStore for K8sSecretFile {
    fn backend(&self) -> Backend {
        Backend::K8sSecretFile
    }
    fn get(&self, id: &SecretId) -> Result<Option<Zeroizing<Vec<u8>>>, SecretError> {
        match std::fs::read(self.mounted(id)) {
            Ok(b) => Ok(Some(Zeroizing::new(b))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => self.fallback.get(id),
            Err(e) => Err(e.into()),
        }
    }
    fn put(&self, id: &SecretId, value: &[u8]) -> Result<(), SecretError> {
        match std::fs::read(self.mounted(id)) {
            Ok(held) => {
                let held = Zeroizing::new(held);
                if held.as_slice() == value {
                    // Already what the Secret holds; nothing to write.
                    return Ok(());
                }
                Err(SecretError::ReadOnly("a mounted Kubernetes Secret"))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => self.fallback.put(id, value),
            Err(e) => Err(e.into()),
        }
    }
    fn delete(&self, id: &SecretId) -> Result<(), SecretError> {
        if self.mounted(id).exists() {
            return Err(SecretError::ReadOnly("a mounted Kubernetes Secret"));
        }
        self.fallback.delete(id)
    }
}

// ---------------------------------------------------------------------------
// Command-line backends (Secret Service via secret-tool, systemd-creds)
// ---------------------------------------------------------------------------

fn run(bin: &Path, args: &[&str], stdin: Option<&[u8]>) -> std::io::Result<std::process::Output> {
    use std::process::Stdio;
    let mut cmd = hive_core::process_util::command(bin);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    let mut child = cmd.spawn()?;
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(input)?;
    }
    child.wait_with_output()
}

fn b64(bytes: &[u8]) -> Zeroizing<String> {
    use base64::Engine;
    Zeroizing::new(base64::engine::general_purpose::STANDARD.encode(bytes))
}

fn unb64(text: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    use base64::Engine;
    let s = std::str::from_utf8(text).ok()?.trim();
    base64::engine::general_purpose::STANDARD.decode(s).ok().map(Zeroizing::new)
}

/// The session's Secret Service (GNOME Keyring, KWallet's bridge, KeePassXC)
/// through `secret-tool` (libsecret). Values are stored base64-encoded under
/// the attributes `service=ai.ligus.secrets account=<scope>:<name>`.
pub struct SecretService {
    bin: PathBuf,
    /// Distinguishes test stores; `ai.ligus.secrets` in production.
    service: String,
}

impl SecretService {
    pub fn new(bin: impl AsRef<Path>) -> Self {
        Self { bin: bin.as_ref().to_path_buf(), service: SERVICE.into() }
    }
    pub fn with_service(mut self, service: impl Into<String>) -> Self {
        self.service = service.into();
        self
    }
}

impl SecretStore for SecretService {
    fn backend(&self) -> Backend {
        Backend::SecretService
    }
    fn get(&self, id: &SecretId) -> Result<Option<Zeroizing<Vec<u8>>>, SecretError> {
        let out = run(&self.bin, &["lookup", "service", &self.service, "account", &id.account()], None)?;
        if !out.status.success() || out.stdout.is_empty() {
            // secret-tool exits 1 for "not found".
            return Ok(None);
        }
        unb64(&out.stdout).map(Some).ok_or_else(|| backend_err("secret-service", "stored value isn't base64"))
    }
    fn put(&self, id: &SecretId, value: &[u8]) -> Result<(), SecretError> {
        let label = format!("Ligus {}", id.account());
        let encoded = b64(value);
        let out = run(
            &self.bin,
            &["store", "--label", &label, "service", &self.service, "account", &id.account()],
            Some(encoded.as_bytes()),
        )?;
        if !out.status.success() {
            return Err(backend_err("secret-service", String::from_utf8_lossy(&out.stderr).trim().to_string()));
        }
        Ok(())
    }
    fn delete(&self, id: &SecretId) -> Result<(), SecretError> {
        let _ = run(&self.bin, &["clear", "service", &self.service, "account", &id.account()], None)?;
        Ok(())
    }
}

/// `systemd-creds` on a headless Linux host: each secret is a credential file
/// encrypted with `--with-key=auto` (TPM2 when present, else the host key) in
/// `<data dir>/credentials/` (0700). A credential the service unit loads with
/// `LoadCredentialEncrypted=` is read from `$CREDENTIALS_DIRECTORY` first.
/// Unprivileged hosts use `systemd-creds --user` (systemd 256+).
pub struct SystemdCreds {
    bin: PathBuf,
    dir: PathBuf,
    credentials_directory: Option<PathBuf>,
    user: bool,
}

impl SystemdCreds {
    pub fn new(bin: impl AsRef<Path>, data_dir: impl AsRef<Path>, user: bool) -> Self {
        Self {
            bin: bin.as_ref().to_path_buf(),
            dir: data_dir.as_ref().join("credentials"),
            credentials_directory: std::env::var_os("CREDENTIALS_DIRECTORY").map(PathBuf::from),
            user,
        }
    }
    fn name(id: &SecretId) -> String {
        format!("ligus.{}.{}", id.scope, id.file_name())
    }
    fn file(&self, id: &SecretId) -> PathBuf {
        self.dir.join(format!("{}.cred", Self::name(id)))
    }
    fn args<'a>(&self, rest: &[&'a str]) -> Vec<&'a str> {
        let mut v: Vec<&str> = if self.user { vec!["--user"] } else { Vec::new() };
        v.extend_from_slice(rest);
        v
    }
}

impl SecretStore for SystemdCreds {
    fn backend(&self) -> Backend {
        Backend::SystemdCreds
    }
    fn get(&self, id: &SecretId) -> Result<Option<Zeroizing<Vec<u8>>>, SecretError> {
        if let Some(dir) = &self.credentials_directory {
            if let Ok(b) = std::fs::read(dir.join(Self::name(id))) {
                return Ok(Some(Zeroizing::new(b)));
            }
        }
        let file = self.file(id);
        if !file.exists() {
            return Ok(None);
        }
        let name = format!("--name={}", Self::name(id));
        let file_s = file.to_string_lossy().into_owned();
        let out = run(&self.bin, &self.args(&["decrypt", &name, &file_s, "-"]), None)?;
        if !out.status.success() {
            return Err(backend_err("systemd-creds", String::from_utf8_lossy(&out.stderr).trim().to_string()));
        }
        Ok(Some(Zeroizing::new(out.stdout)))
    }
    fn put(&self, id: &SecretId, value: &[u8]) -> Result<(), SecretError> {
        crate::private_file::ensure_private_dir(&self.dir)?;
        let file = self.file(id);
        let tmp = file.with_extension("cred.tmp");
        let name = format!("--name={}", Self::name(id));
        let tmp_s = tmp.to_string_lossy().into_owned();
        let out = run(&self.bin, &self.args(&["encrypt", "--with-key=auto", &name, "-", &tmp_s]), Some(value))?;
        if !out.status.success() {
            let _ = std::fs::remove_file(&tmp);
            return Err(backend_err("systemd-creds", String::from_utf8_lossy(&out.stderr).trim().to_string()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, &file)?;
        Ok(())
    }
    fn delete(&self, id: &SecretId) -> Result<(), SecretError> {
        match std::fs::remove_file(self.file(id)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

// ---------------------------------------------------------------------------
// MacKeychain
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
pub use mac::MacKeychain;

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use security_framework::os::macos::keychain::{CreateOptions, KeychainSettings, SecKeychain};
    use security_framework::os::macos::passwords::find_generic_password;

    /// A dedicated keychain file (`<data dir>/secrets/ligus.keychain-db`),
    /// unlocked with a random password that is itself a login-keychain item
    /// (`<service>.unlock`). That item's access list trusts only the Ligus
    /// binary ([`acl`]), not whatever process happened to create it. See the
    /// module docs for why Ligus's items aren't in the login keychain.
    pub struct MacKeychain {
        path: PathBuf,
        service: String,
        /// The keychain holding the unlock password: the login keychain
        /// (`None`), or a throwaway keychain in tests.
        unlock_in: Option<PathBuf>,
        lock: Mutex<()>,
    }

    impl MacKeychain {
        /// The keychain for `data_dir` (created on first use).
        pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, SecretError> {
            Self::open_with(data_dir.as_ref().join("secrets").join("ligus.keychain-db"), SERVICE)
        }

        /// A keychain at `path` with its own service name.
        pub fn open_with(path: PathBuf, service: &str) -> Result<Self, SecretError> {
            let me = Self { path, service: service.to_string(), unlock_in: None, lock: Mutex::new(()) };
            me.keychain()?;
            Ok(me)
        }

        /// A keychain at `path` whose unlock password lives in the keychain
        /// file `unlock_in` instead of the login keychain (tests: a
        /// throwaway pair, so no test ever writes to the login keychain).
        #[cfg(test)]
        pub(crate) fn open_with_unlock_in(path: PathBuf, service: &str, unlock_in: PathBuf) -> Result<Self, SecretError> {
            let me = Self { path, service: service.to_string(), unlock_in: Some(unlock_in), lock: Mutex::new(()) };
            me.keychain()?;
            Ok(me)
        }

        fn unlock_account(&self) -> String {
            use sha2::{Digest, Sha256};
            let d = Sha256::digest(self.path.to_string_lossy().as_bytes());
            format!("keychain:{}", hive_core::keys::hex(&d[..8]))
        }

        fn unlock_service(&self) -> String {
            format!("{}.unlock", self.service)
        }

        /// The keychains to look the unlock password up in (`None`: the
        /// user's search list, where the login keychain is).
        fn unlock_search(&self) -> Result<Option<Vec<SecKeychain>>, SecretError> {
            match &self.unlock_in {
                None => Ok(None),
                Some(p) => Ok(Some(vec![SecKeychain::open(p).map_err(|e| backend_err("keychain", e.to_string()))?])),
            }
        }

        fn find_unlock(&self) -> Option<(Vec<u8>, security_framework::os::macos::keychain_item::SecKeychainItem)> {
            let search = self.unlock_search().ok()?;
            find_generic_password(search.as_deref(), &self.unlock_service(), &self.unlock_account())
                .ok()
                .map(|(pw, item)| (pw.to_vec(), item))
        }

        /// Open (or create) and unlock the keychain.
        fn keychain(&self) -> Result<SecKeychain, SecretError> {
            let err = |e: security_framework::base::Error| backend_err("keychain", e.to_string());
            let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
            let password = self.find_unlock().map(|(pw, _)| Zeroizing::new(String::from_utf8_lossy(&pw).into_owned()));
            if self.path.exists() {
                let Some(pw) = password else {
                    return Err(backend_err("keychain", "the Ligus keychain's unlock password is missing from the login keychain"));
                };
                let mut kc = SecKeychain::open(&self.path).map_err(err)?;
                kc.unlock(Some(&pw)).map_err(err)?;
                return Ok(kc);
            }
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
                }
            }
            let mut raw = [0u8; 32];
            getrandom::fill(&mut raw).map_err(|e| backend_err("keychain", e.to_string()))?;
            let pw = Zeroizing::new(hive_core::keys::hex(&raw));
            // The unlock password trusts only the Ligus binary: in a release
            // build, only after this app proves it is Ligus (its designated
            // requirement: Apple-anchored, Ligus's bundle id and team id).
            let binary = acl::ligus_binary()?;
            acl::check_self(&binary)?;
            if let Some((_, stale)) = self.find_unlock() {
                stale.delete(); // a password for a keychain file that is gone
            }
            acl::add_generic_password(
                self.unlock_in.as_deref(),
                &self.unlock_service(),
                &self.unlock_account(),
                "Ligus keychain unlock",
                pw.as_bytes(),
                &binary,
            )
            .map_err(|e| backend_err("keychain", e))?;
            let mut kc = CreateOptions::new().password(&pw).prompt_user(false).create(&self.path).map_err(err)?;
            // Stay unlocked while the user's session is (no idle or sleep lock):
            // the file itself is what agents can't reach.
            let mut settings = KeychainSettings::new();
            settings.set_lock_on_sleep(false);
            settings.set_lock_interval(None);
            let _ = kc.set_settings(&settings);
            Ok(kc)
        }

        /// Remove the keychain file and its unlock password (tests, and
        /// `ligus doctor` resets).
        pub fn destroy(self) -> Result<(), SecretError> {
            if let Some((_, item)) = self.find_unlock() {
                item.delete();
            }
            match std::fs::remove_file(&self.path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.into()),
            }
        }

        pub fn path(&self) -> &Path {
            &self.path
        }

        /// A handle that doesn't open anything (tests probing cleanup).
        #[cfg(test)]
        pub(crate) fn unopened(path: PathBuf, service: &str, unlock_in: Option<PathBuf>) -> Self {
            Self { path, service: service.to_string(), unlock_in, lock: Mutex::new(()) }
        }

        /// Whether the unlock password item exists in the login keychain.
        pub fn unlock_item_exists(&self) -> bool {
            self.find_unlock().is_some()
        }
    }

    impl SecretStore for MacKeychain {
        fn backend(&self) -> Backend {
            Backend::MacKeychain
        }
        fn get(&self, id: &SecretId) -> Result<Option<Zeroizing<Vec<u8>>>, SecretError> {
            let kc = self.keychain()?;
            match find_generic_password(Some(&[kc]), &self.service, &id.account()) {
                Ok((pw, _)) => Ok(Some(Zeroizing::new(pw.to_vec()))),
                Err(e) if e.code() == -25300 => Ok(None), // errSecItemNotFound
                Err(e) => Err(backend_err("keychain", e.to_string())),
            }
        }
        fn put(&self, id: &SecretId, value: &[u8]) -> Result<(), SecretError> {
            let kc = self.keychain()?;
            kc.set_generic_password(&self.service, &id.account(), value).map_err(|e| backend_err("keychain", e.to_string()))
        }
        fn delete(&self, id: &SecretId) -> Result<(), SecretError> {
            let kc = self.keychain()?;
            if let Ok((_, item)) = find_generic_password(Some(&[kc]), &self.service, &id.account()) {
                item.delete();
            }
            Ok(())
        }
    }

    /// The unlock item's access list (Security.framework's legacy keychain
    /// ACLs, which the security-framework crate doesn't wrap).
    ///
    /// The item is created with an explicit access whose only trusted
    /// application is the Ligus binary: the `.app` bundle the desktop runs
    /// from, else the executable. For signed code the keychain records the
    /// binary's designated requirement (Apple anchor, bundle id, team id), so
    /// an update signed the same way keeps access without a prompt, and any
    /// other program (an agent's CLI, `/usr/bin/security`) gets a system
    /// prompt. A development build is unsigned or ad-hoc signed: the entry
    /// then pins that build's code hash, and every rebuild asks once for the
    /// login keychain password (Always Allow) before it can read the item.
    pub(super) mod acl {
        use std::ffi::{c_char, c_void, CString};
        use std::os::unix::ffi::OsStrExt;
        use std::path::{Path, PathBuf};

        use security_framework::os::macos::code_signing::{Flags, SecCode, SecRequirement};

        use super::super::{backend_err, SecretError};

        /// The desktop app's bundle id (`app/tauri.conf.json`).
        pub const BUNDLE_ID: &str = "ai.ligus.desktop";

        type CFTypeRef = *const c_void;
        type OSStatus = i32;

        #[repr(C)]
        struct Attribute {
            tag: u32,
            length: u32,
            data: *mut c_void,
        }

        #[repr(C)]
        struct AttributeList {
            count: u32,
            attr: *mut Attribute,
        }

        const GENERIC_PASSWORD: u32 = u32::from_be_bytes(*b"genp");
        const SERVICE: u32 = u32::from_be_bytes(*b"svce");
        const ACCOUNT: u32 = u32::from_be_bytes(*b"acct");
        const LABEL: u32 = u32::from_be_bytes(*b"labl");
        const UTF8: u32 = 0x0800_0100;

        #[link(name = "CoreFoundation", kind = "framework")]
        extern "C" {
            static kCFTypeArrayCallBacks: [usize; 5];
            fn CFArrayCreate(alloc: CFTypeRef, values: *const CFTypeRef, count: isize, callbacks: *const c_void) -> CFTypeRef;
            fn CFStringCreateWithBytes(alloc: CFTypeRef, bytes: *const u8, len: isize, encoding: u32, external: u8) -> CFTypeRef;
            fn CFRelease(cf: CFTypeRef);
        }

        #[link(name = "Security", kind = "framework")]
        extern "C" {
            fn SecTrustedApplicationCreateFromPath(path: *const c_char, app: *mut CFTypeRef) -> OSStatus;
            fn SecAccessCreate(descriptor: CFTypeRef, trusted: CFTypeRef, access: *mut CFTypeRef) -> OSStatus;
            fn SecKeychainOpen(path: *const c_char, keychain: *mut CFTypeRef) -> OSStatus;
            fn SecKeychainCopyDefault(keychain: *mut CFTypeRef) -> OSStatus;
            fn SecKeychainItemCreateFromContent(
                class: u32,
                attrs: *mut AttributeList,
                length: u32,
                data: *const c_void,
                keychain: CFTypeRef,
                access: CFTypeRef,
                item: *mut CFTypeRef,
            ) -> OSStatus;
        }

        /// Releases a Core Foundation object when dropped.
        struct Owned(CFTypeRef);
        impl Drop for Owned {
            fn drop(&mut self) {
                if !self.0.is_null() {
                    unsafe { CFRelease(self.0) }
                }
            }
        }

        fn status(what: &str, st: OSStatus) -> Result<(), String> {
            if st == 0 {
                Ok(())
            } else {
                Err(format!("{what} failed (OSStatus {st})"))
            }
        }

        fn c_path(p: &Path) -> Result<CString, String> {
            CString::new(p.as_os_str().as_bytes()).map_err(|_| "a path with a NUL byte".to_string())
        }

        /// The Ligus binary to trust: the `.app` bundle the running
        /// executable is in, else the executable itself.
        pub fn ligus_binary() -> Result<PathBuf, SecretError> {
            let exe = std::env::current_exe()?.canonicalize()?;
            Ok(exe
                .ancestors()
                .find(|p| p.extension().is_some_and(|e| e == "app") && exe.starts_with(p.join("Contents").join("MacOS")))
                .map(Path::to_path_buf)
                .unwrap_or(exe))
        }

        /// The designated requirement a release build of the desktop app
        /// satisfies, when this build knows its team id
        /// (`LIGUS_APPLE_TEAM_ID` at build time) and runs from an app bundle.
        /// `None` for development builds and the CLI.
        pub fn requirement_for(binary: &Path) -> Option<String> {
            requirement_as(option_env!("LIGUS_APPLE_TEAM_ID"), binary, super::super::binary_identifier())
        }

        /// Ligus's designated requirement for team `team`, when `binary` is
        /// an app bundle.
        #[cfg_attr(not(test), allow(dead_code))]
        pub fn requirement(team: Option<&str>, binary: &Path) -> Option<String> {
            requirement_as(team, binary, None)
        }

        /// [`requirement`], or, for a standalone binary that names its
        /// signing identifier (`identifier`, see
        /// [`super::super::set_binary_identifier`]), the requirement for
        /// that identifier whether or not it runs from an app bundle.
        pub fn requirement_as(team: Option<&str>, binary: &Path, identifier: Option<&str>) -> Option<String> {
            let team = team.map(str::trim).filter(|t| !t.is_empty())?;
            let id = match identifier.map(str::trim).filter(|i| !i.is_empty()) {
                Some(id) => id,
                None if binary.extension().is_some_and(|e| e == "app") => BUNDLE_ID,
                None => return None,
            };
            Some(format!("anchor apple generic and identifier \"{id}\" and certificate leaf[subject.OU] = \"{team}\""))
        }

        /// A release app trusts itself with the unlock password only after
        /// proving it is Ligus: the running code must satisfy
        /// [`requirement_for`] (a copied or re-signed binary doesn't).
        pub fn check_self(binary: &Path) -> Result<(), SecretError> {
            match requirement_for(binary) {
                Some(req) => check_self_against(&req),
                None => Ok(()),
            }
        }

        /// Whether the running code satisfies `req`.
        pub fn check_self_against(req: &str) -> Result<(), SecretError> {
            let req: SecRequirement = req.parse().map_err(|e: security_framework::base::Error| backend_err("keychain", e.to_string()))?;
            SecCode::for_self(Flags::NONE).and_then(|c| c.check_validity(Flags::NONE, &req)).map_err(|e| {
                backend_err("keychain", format!("this app isn't signed as Ligus ({e}), so it won't hold the keychain's unlock password"))
            })
        }

        /// Add a generic password to `keychain` (the login keychain when
        /// `None`) whose access list trusts only `trusted`.
        pub fn add_generic_password(
            keychain: Option<&Path>,
            service: &str,
            account: &str,
            label: &str,
            secret: &[u8],
            trusted: &Path,
        ) -> Result<(), String> {
            unsafe {
                let mut kc: CFTypeRef = std::ptr::null();
                match keychain {
                    Some(p) => status("SecKeychainOpen", SecKeychainOpen(c_path(p)?.as_ptr(), &mut kc))?,
                    None => status("SecKeychainCopyDefault", SecKeychainCopyDefault(&mut kc))?,
                }
                let kc = Owned(kc);
                let mut app: CFTypeRef = std::ptr::null();
                status("SecTrustedApplicationCreateFromPath", SecTrustedApplicationCreateFromPath(c_path(trusted)?.as_ptr(), &mut app))?;
                let app = Owned(app);
                let apps = [app.0];
                let list = Owned(CFArrayCreate(std::ptr::null(), apps.as_ptr(), 1, kCFTypeArrayCallBacks.as_ptr() as *const c_void));
                let desc = Owned(CFStringCreateWithBytes(std::ptr::null(), label.as_ptr(), label.len() as isize, UTF8, 0));
                if list.0.is_null() || desc.0.is_null() {
                    return Err("out of memory".into());
                }
                let mut access: CFTypeRef = std::ptr::null();
                status("SecAccessCreate", SecAccessCreate(desc.0, list.0, &mut access))?;
                let access = Owned(access);
                let mut attrs = [
                    Attribute { tag: SERVICE, length: service.len() as u32, data: service.as_ptr() as *mut c_void },
                    Attribute { tag: ACCOUNT, length: account.len() as u32, data: account.as_ptr() as *mut c_void },
                    Attribute { tag: LABEL, length: label.len() as u32, data: label.as_ptr() as *mut c_void },
                ];
                let mut attr_list = AttributeList { count: attrs.len() as u32, attr: attrs.as_mut_ptr() };
                let mut item: CFTypeRef = std::ptr::null();
                let st = SecKeychainItemCreateFromContent(
                    GENERIC_PASSWORD,
                    &mut attr_list,
                    secret.len() as u32,
                    secret.as_ptr() as *const c_void,
                    kc.0,
                    access.0,
                    &mut item,
                );
                drop(Owned(item));
                status("SecKeychainItemCreateFromContent", st)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Unavailable
// ---------------------------------------------------------------------------

/// A store that refuses every use with its reason: on macOS a Ligus keychain
/// that can't be used is an error the person sees, not a silent move of
/// every secret into plain files (B2 residual). `LIGUS_SECRET_BACKEND=file`
/// is the explicit opt-in to owner-only files.
pub struct Unavailable {
    why: String,
}

impl SecretStore for Unavailable {
    fn backend(&self) -> Backend {
        Backend::Unavailable
    }
    fn get(&self, _: &SecretId) -> Result<Option<Zeroizing<Vec<u8>>>, SecretError> {
        Err(backend_err("keychain", self.why.clone()))
    }
    fn put(&self, _: &SecretId, _: &[u8]) -> Result<(), SecretError> {
        Err(backend_err("keychain", self.why.clone()))
    }
    fn delete(&self, _: &SecretId) -> Result<(), SecretError> {
        Err(backend_err("keychain", self.why.clone()))
    }
}

/// The keychain, or, when it can't be used, a store that refuses with a
/// clear error (macOS never degrades to plain files unless asked).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn keychain_or_refuse<S: SecretStore + 'static>(keychain: Result<S, SecretError>) -> Arc<dyn SecretStore> {
    match keychain {
        Ok(k) => Arc::new(k),
        Err(e) => {
            let why = format!(
                "the Ligus keychain isn't available ({e}). Ligus doesn't move its secrets into plain files on its own: \
                 fix the keychain, or set LIGUS_SECRET_BACKEND=file to keep them in owner-only files under the data dir"
            );
            tracing::error!(target: "secrets", "{why}");
            Arc::new(Unavailable { why })
        }
    }
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

fn on_path(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(bin)).find(|p| p.is_file())
}

/// Open the store for `data_dir`: `choice` (`keychain`, `secret-service`,
/// `systemd-creds`, `k8s`, `file`) or, when `None`, the OS default. A backend
/// that can't be used falls back to [`File0600`] with a warning, except the
/// macOS keychain: that one refuses ([`Unavailable`]) unless `file` was
/// chosen.
pub fn open(data_dir: &Path, choice: Option<&str>) -> Arc<dyn SecretStore> {
    let choice = choice.map(|c| c.trim().to_ascii_lowercase()).filter(|c| !c.is_empty());
    let fallback = |why: String| -> Arc<dyn SecretStore> {
        tracing::warn!(target: "secrets", "{why}; keeping Ligus's secrets in owner-only files under the data dir (run `ligus doctor`)");
        Arc::new(File0600::new(data_dir))
    };
    match choice.as_deref() {
        Some("file") => return Arc::new(File0600::new(data_dir)),
        Some("k8s") => {
            let mount = hive_core::compat::var_os("LIGUS_SECRETS_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(K8S_SECRETS_DIR_DEFAULT));
            return Arc::new(K8sSecretFile::new(mount, data_dir));
        }
        Some("secret-service") => {
            return match on_path("secret-tool") {
                Some(bin) => Arc::new(SecretService::new(bin)),
                None => fallback("secret-tool (libsecret) isn't installed".into()),
            };
        }
        Some("systemd-creds") => {
            return match systemd_creds(data_dir) {
                Some(s) => Arc::new(s),
                None => fallback("systemd-creds can't encrypt here".into()),
            };
        }
        _ => {}
    }
    #[cfg(target_os = "macos")]
    {
        keychain_or_refuse(MacKeychain::open(data_dir))
    }
    #[cfg(target_os = "linux")]
    {
        if hive_core::compat::var_os("LIGUS_SECRETS_DIR").is_some() {
            return open(data_dir, Some("k8s"));
        }
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some() {
            if let Some(bin) = on_path("secret-tool") {
                let s = SecretService::new(bin);
                // A probe lookup: exit 0 or 1 means the service answered.
                if run(&s.bin, &["lookup", "service", SERVICE, "account", "probe:probe"], None).is_ok_and(|o| o.status.code().is_some_and(|c| c <= 1)) {
                    return Arc::new(s);
                }
            }
        }
        match systemd_creds(data_dir) {
            Some(s) => Arc::new(s),
            None => fallback("no Secret Service session and no usable systemd-creds".into()),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        // Windows Credential Manager is [2.x].
        fallback("no OS secret store backend on this platform yet".into())
    }
}

/// A working `systemd-creds` (probed with a round trip), user mode first.
fn systemd_creds(data_dir: &Path) -> Option<SystemdCreds> {
    let bin = on_path("systemd-creds")?;
    let probe = SecretId::new("probe", "probe");
    for user in [!is_root(), false] {
        let s = SystemdCreds::new(&bin, data_dir, user);
        if s.put(&probe, b"ok").is_ok() && s.get(&probe).ok().flatten().is_some_and(|v| v.as_slice() == b"ok") {
            let _ = s.delete(&probe);
            return Some(s);
        }
    }
    None
}

fn is_root() -> bool {
    #[cfg(unix)]
    {
        std::env::var("USER").is_ok_and(|u| u == "root")
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn stores() -> &'static Mutex<HashMap<PathBuf, Arc<dyn SecretStore>>> {
    static STORES: OnceLock<Mutex<HashMap<PathBuf, Arc<dyn SecretStore>>>> = OnceLock::new();
    STORES.get_or_init(Default::default)
}

/// Open the OS store for `data_dir` (`LIGUS_SECRET_BACKEND`, else `choice`,
/// else the OS default) and use it for that data dir from now on. Called by
/// the desktop and the CLI at startup; library code and tests that never call
/// it get [`File0600`] from [`for_data_dir`], so no test touches a real
/// keychain.
pub fn init_for_data_dir(data_dir: &Path, choice: Option<&str>) -> Arc<dyn SecretStore> {
    let env = hive_core::compat::var("LIGUS_SECRET_BACKEND").ok();
    let store = open(data_dir, env.as_deref().or(choice));
    if store.backend().is_plain_file() {
        tracing::warn!(target: "secrets", "Ligus's secrets are in owner-only files under {}, not an OS secret store", data_dir.display());
    }
    set_for_data_dir(data_dir, store.clone());
    store
}

/// The store for a data dir: the one [`init_for_data_dir`] opened, else
/// [`File0600`].
pub fn for_data_dir(data_dir: &Path) -> Arc<dyn SecretStore> {
    let mut map = stores().lock().unwrap_or_else(|e| e.into_inner());
    map.entry(data_dir.to_path_buf()).or_insert_with(|| Arc::new(File0600::new(data_dir))).clone()
}

/// Use `store` for `data_dir` from now on in this process (tests, and hosts
/// that choose a backend from their own config).
pub fn set_for_data_dir(data_dir: &Path, store: Arc<dyn SecretStore>) {
    stores().lock().unwrap_or_else(|e| e.into_inner()).insert(data_dir.to_path_buf(), store);
}

// ---------------------------------------------------------------------------
// The key vault over the store
// ---------------------------------------------------------------------------

/// [`crate::identity_store::KeyVault`] over a [`SecretStore`] (scope `seed`).
pub struct StoreKeyVault {
    store: Arc<dyn SecretStore>,
}

impl StoreKeyVault {
    pub fn new(store: Arc<dyn SecretStore>) -> Self {
        Self { store }
    }
    /// The vault for `data_dir`'s store.
    pub fn for_data_dir(data_dir: &Path) -> Self {
        Self::new(for_data_dir(data_dir))
    }
}

impl crate::identity_store::KeyVault for StoreKeyVault {
    fn store_seed(&self, key_id: &str, seed: &[u8; 32]) -> Result<(), crate::identity_store::IdentityError> {
        crate::secret_redact::register_bytes(seed);
        self.store
            .put(&SecretId::new("seed", key_id), seed)
            .map_err(|e| crate::identity_store::IdentityError::Vault(e.to_string()))
    }
    fn load_seed(&self, key_id: &str) -> Result<Option<[u8; 32]>, crate::identity_store::IdentityError> {
        let v = self
            .store
            .get(&SecretId::new("seed", key_id))
            .map_err(|e| crate::identity_store::IdentityError::Vault(e.to_string()))?;
        match v {
            None => Ok(None),
            Some(b) => {
                let seed: [u8; 32] = b
                    .as_slice()
                    .try_into()
                    .map_err(|_| crate::identity_store::IdentityError::MissingSeed(key_id.to_string()))?;
                crate::secret_redact::register_bytes(&seed);
                Ok(Some(seed))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Settings secrets: {"set": true} placeholders
// ---------------------------------------------------------------------------

/// Where secrets sit in a settings JSON object. Each path is `/`-separated
/// keys; `*` matches every key of an object; `[field]` matches every element
/// of an array, naming it by that element's `field` (e.g. `workspaces[room]/key`).
pub type SecretPaths = &'static [&'static str];

/// The placeholder a secret leaves in the file.
pub fn placeholder() -> serde_json::Value {
    serde_json::json!({ "set": true })
}

fn is_placeholder(v: &serde_json::Value) -> bool {
    v.as_object().is_some_and(|o| o.len() == 1 && o.get("set") == Some(&serde_json::Value::Bool(true)))
}

/// A visitor of one secret slot: `(name, the object holding it, its key)`.
type SlotFn<'a> = dyn FnMut(String, &mut serde_json::Map<String, serde_json::Value>, &str) + 'a;
/// What `persist` last wrote, per (scope, name).
type Written = Mutex<HashMap<(String, String), [u8; 32]>>;

/// Visit every secret slot `paths` name in `root`: `f(name, slot_parent, key)`.
fn visit(root: &mut serde_json::Value, paths: SecretPaths, f: &mut SlotFn<'_>) {
    for path in paths {
        let parts: Vec<&str> = path.split('/').collect();
        walk(root, &parts, String::new(), f);
    }
}

fn walk(
    node: &mut serde_json::Value,
    parts: &[&str],
    prefix: String,
    f: &mut SlotFn<'_>,
) {
    let Some((head, rest)) = parts.split_first() else { return };
    let join = |p: &str, k: &str| if p.is_empty() { k.to_string() } else { format!("{p}/{k}") };
    if let Some((field, key)) = head.split_once('[').and_then(|(k, r)| r.strip_suffix(']').map(|n| (k, n))) {
        let Some(arr) = node.get_mut(field).and_then(|v| v.as_array_mut()) else { return };
        for el in arr.iter_mut() {
            let Some(id) = el.get(key).and_then(|v| v.as_str()).map(str::to_string) else { continue };
            walk(el, rest, join(&join(&prefix, field), &id), f);
        }
        return;
    }
    let Some(obj) = node.as_object_mut() else { return };
    if rest.is_empty() {
        if *head == "*" {
            let keys: Vec<String> = obj.keys().cloned().collect();
            for k in keys {
                f(join(&prefix, &k), obj, &k);
            }
        } else if obj.contains_key(*head) {
            f(join(&prefix, head), obj, head);
        }
        return;
    }
    if *head == "*" {
        let keys: Vec<String> = obj.keys().cloned().collect();
        for k in keys {
            if let Some(child) = obj.get_mut(&k) {
                walk(child, rest, join(&prefix, &k), f);
            }
        }
    } else if let Some(child) = obj.get_mut(*head) {
        walk(child, rest, join(&prefix, head), f);
    }
}

/// Take every secret out of `root`, leaving `{"set": true}` (null and empty
/// values are just dropped). Returns name → the secret's JSON.
pub fn split(root: &mut serde_json::Value, paths: SecretPaths) -> BTreeMap<String, serde_json::Value> {
    let mut out = BTreeMap::new();
    visit(root, paths, &mut |name, obj, key| {
        let Some(v) = obj.get(key) else { return };
        let empty = v.is_null() || v.as_str() == Some("") || v.as_array().is_some_and(Vec::is_empty);
        if is_placeholder(v) {
            return;
        }
        if empty {
            obj.remove(key);
            return;
        }
        out.insert(name, v.clone());
        obj.insert(key.to_string(), placeholder());
    });
    out
}

/// Put the secrets back where `{"set": true}` stands; a placeholder whose
/// secret is missing, and any plaintext value left in the file (a file from
/// before secrets moved to the store, or edited by hand), is dropped.
/// Returns whether the file held plaintext that must be rewritten.
pub fn merge(root: &mut serde_json::Value, paths: SecretPaths, get: &dyn Fn(&str) -> Option<serde_json::Value>) -> bool {
    let mut scrubbed = false;
    visit(root, paths, &mut |name, obj, key| {
        let Some(v) = obj.get(key) else { return };
        if is_placeholder(v) {
            match get(&name) {
                Some(secret) => {
                    obj.insert(key.to_string(), secret);
                }
                None => {
                    obj.remove(key);
                }
            }
        } else if !v.is_null() && v.as_str() != Some("") {
            obj.remove(key);
            scrubbed = true;
        }
    });
    scrubbed
}

/// Persist `secrets` (from [`split`]) under scope `scope`, deleting names a
/// previous save stored that are gone now. Writes only what changed since
/// this process last saved.
pub fn persist(store: &dyn SecretStore, scope: &'static str, secrets: &BTreeMap<String, serde_json::Value>) -> Result<(), SecretError> {
    static LAST: OnceLock<Written> = OnceLock::new();
    use sha2::{Digest, Sha256};
    let mut last = LAST.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    let index_id = SecretId::new(scope, "__index");
    let previous: Vec<String> = store
        .get(&index_id)?
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    for (name, v) in secrets {
        let bytes = Zeroizing::new(serde_json::to_vec(v).unwrap_or_default());
        register_strings(v);
        let digest: [u8; 32] = Sha256::digest(&*bytes).into();
        let key = (scope.to_string(), name.clone());
        if last.get(&key) == Some(&digest) && previous.contains(name) {
            continue;
        }
        store.put(&SecretId::new(scope, name.clone()), &bytes)?;
        last.insert(key, digest);
    }
    let names: Vec<String> = secrets.keys().cloned().collect();
    for gone in previous.iter().filter(|n| !names.contains(n)) {
        store.delete(&SecretId::new(scope, gone.clone()))?;
        last.remove(&(scope.to_string(), gone.clone()));
    }
    if previous != names {
        store.put(&index_id, &serde_json::to_vec(&names).unwrap_or_default())?;
    }
    Ok(())
}

/// Read a secret saved by [`persist`].
pub fn load_json(store: &dyn SecretStore, scope: &'static str, name: &str) -> Option<serde_json::Value> {
    let b = store.get(&SecretId::new(scope, name)).ok()??;
    let v: serde_json::Value = serde_json::from_slice(&b).ok()?;
    register_strings(&v);
    Some(v)
}

/// Save a settings object to `path`: secrets to `store` (scope `scope`),
/// `{"set": true}` in their place, the file written 0600 and atomically.
pub fn save_settings_json(
    path: &Path,
    store: &dyn SecretStore,
    scope: &'static str,
    mut value: serde_json::Value,
    paths: SecretPaths,
) -> Result<(), SecretError> {
    let secrets = split(&mut value, paths);
    persist(store, scope, &secrets)?;
    let text = serde_json::to_vec_pretty(&value).map_err(|e| backend_err("settings", e.to_string()))?;
    write_private_atomic(path, &text)?;
    Ok(())
}

/// Load a settings object from `path` with its secrets from `store`. A file
/// still holding plaintext secrets is rewritten without them (they are not
/// imported: 0.1 starts from scratch).
pub fn load_settings_json(
    path: &Path,
    store: &dyn SecretStore,
    scope: &'static str,
    paths: SecretPaths,
) -> Option<serde_json::Value> {
    let text = std::fs::read(path).ok()?;
    let mut value: serde_json::Value = serde_json::from_slice(&text).ok()?;
    let scrubbed = merge(&mut value, paths, &|name| load_json(store, scope, name));
    if scrubbed {
        tracing::warn!(target: "secrets", "{} held plaintext secrets; removed them (enter them again in Settings)", path.display());
        let mut clean = value.clone();
        let _ = split(&mut clean, paths);
        let _ = serde_json::to_vec_pretty(&clean).map(|t| write_private_atomic(path, &t));
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn roundtrip(store: &dyn SecretStore) {
        let id = SecretId::new("settings", "provider_keys/anthropic");
        assert!(store.get(&id).unwrap().is_none());
        store.put(&id, b"sk-test-1").unwrap();
        assert_eq!(store.get(&id).unwrap().unwrap().as_slice(), b"sk-test-1");
        store.put(&id, b"sk-test-2").unwrap();
        assert_eq!(store.get(&id).unwrap().unwrap().as_slice(), b"sk-test-2");
        let seed = SecretId::new("seed", "device:0b4c");
        store.put(&seed, &[0u8, 1, 2, 255, 10, 13]).unwrap();
        assert_eq!(store.get(&seed).unwrap().unwrap().as_slice(), &[0u8, 1, 2, 255, 10, 13]);
        store.delete(&id).unwrap();
        assert!(store.get(&id).unwrap().is_none());
        store.delete(&id).unwrap();
        store.delete(&seed).unwrap();
    }

    #[cfg(unix)]
    fn mode(p: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn vault_roundtrip_file0600() {
        let dir = tempfile::tempdir().unwrap();
        let store = File0600::new(dir.path());
        roundtrip(&store);
        store.put(&SecretId::new("settings", "x"), b"v").unwrap();
        #[cfg(unix)]
        {
            assert_eq!(mode(&dir.path().join("secrets/settings/x")), 0o600);
            assert_eq!(mode(&dir.path().join("secrets/settings")), 0o700);
            assert_eq!(mode(&dir.path().join("secrets")), 0o700);
        }
        // Seeds keep the historical keys/<id>.seed layout (FileKeyVault).
        store.put(&SecretId::new("seed", "account:abc"), &[7; 32]).unwrap();
        assert_eq!(std::fs::read(dir.path().join("keys/account_abc.seed")).unwrap(), vec![7; 32]);
    }

    #[test]
    fn vault_roundtrip_k8s_mounted_then_file() {
        let dir = tempfile::tempdir().unwrap();
        let mount = dir.path().join("mount");
        std::fs::create_dir_all(&mount).unwrap();
        std::fs::write(mount.join("cli.token"), "hrt1-from-operator").unwrap();
        let store = K8sSecretFile::new(&mount, dir.path().join("data"));
        let token = SecretId::new("cli", "token");
        assert_eq!(store.get(&token).unwrap().unwrap().as_slice(), b"hrt1-from-operator");
        assert!(matches!(store.put(&token, b"x"), Err(SecretError::ReadOnly(_))), "a mounted Secret is read-only");
        assert!(matches!(store.delete(&token), Err(SecretError::ReadOnly(_))), "and can't be deleted");
        // Re-saving the value the Secret holds is a no-op, not an error (a
        // box re-saving its connection after redeeming an admission).
        store.put(&token, b"hrt1-from-operator").unwrap();
        assert!(!dir.path().join("data/secrets/cli/token").exists(), "never copied into the data dir");
        assert_eq!(store.get(&token).unwrap().unwrap().as_slice(), b"hrt1-from-operator");
        roundtrip(&store);
    }

    /// A read-only mount (the kubelet's 0400 files in a dir the box can't
    /// write): runtime secrets land owner-only in the data dir.
    #[cfg(unix)]
    #[test]
    fn k8s_runtime_secrets_go_to_the_data_dir_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mount = dir.path().join("mount");
        std::fs::create_dir_all(&mount).unwrap();
        std::fs::write(mount.join("cli.key"), "ws-key").unwrap();
        std::fs::set_permissions(mount.join("cli.key"), std::fs::Permissions::from_mode(0o400)).unwrap();
        std::fs::set_permissions(&mount, std::fs::Permissions::from_mode(0o500)).unwrap();
        let data = dir.path().join("data");
        let store = K8sSecretFile::new(&mount, &data);
        store.put(&SecretId::new("cli", "key"), b"ws-key").unwrap();
        store.put(&SecretId::new("cli", "ek"), b"[]").unwrap();
        store.put(&SecretId::new("seed", "device:1"), &[9; 32]).unwrap();
        assert_eq!(mode(&data.join("secrets/cli/ek")), 0o600);
        assert_eq!(mode(&data.join("secrets/cli")), 0o700);
        assert_eq!(mode(&data.join("secrets")), 0o700);
        assert_eq!(mode(&data.join("keys/device_1.seed")), 0o600);
        assert_eq!(mode(&data.join("keys")), 0o700);
        assert!(!data.join("secrets/cli/key").exists());
        std::fs::set_permissions(&mount, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// The data dir is often a volume's mount root that the box can't chmod
    /// (root-owned under a pod's fsGroup): storing secrets never chmods it,
    /// only the private dirs created inside it (0700).
    #[cfg(unix)]
    #[test]
    fn storing_secrets_never_chmods_the_data_dir_itself() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (mount, data) = (dir.path().join("mount"), dir.path().join("data"));
        std::fs::create_dir_all(&mount).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o2770)).unwrap();
        let before = std::fs::metadata(&data).unwrap().permissions().mode() & 0o7777;
        let store = K8sSecretFile::new(&mount, &data);
        store.put(&SecretId::new("cli", "ek"), b"[]").unwrap();
        store.put(&SecretId::new("seed", "device:1"), &[9; 32]).unwrap();
        File0600::new(&data).put(&SecretId::new("cli", "token"), b"t").unwrap();
        assert_eq!(std::fs::metadata(&data).unwrap().permissions().mode() & 0o7777, before);
        assert_eq!(mode(&data.join("secrets")), 0o700);
        assert_eq!(mode(&data.join("secrets/cli")), 0o700);
        assert_eq!(mode(&data.join("keys")), 0o700);
    }

    /// A stand-in for a CLI backend: a shell script storing values under a
    /// directory (what the real tool keeps in the keyring or a credential).
    #[cfg(unix)]
    fn fake_tool(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\nDB='{}'\n{body}\n", dir.join("db").display())).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir_all(dir.join("db")).unwrap();
        p
    }

    #[cfg(unix)]
    #[test]
    fn vault_roundtrip_secret_service() {
        let dir = tempfile::tempdir().unwrap();
        // secret-tool store --label L service S account A  (secret on stdin)
        // secret-tool lookup service S account A   (exit 1 when missing)
        // secret-tool clear service S account A
        let bin = fake_tool(
            dir.path(),
            "secret-tool",
            r#"case "$1" in
  store) f="$DB/$(printf %s "$5.$7" | tr '/:' '__')"; cat > "$f" ;;
  lookup) f="$DB/$(printf %s "$3.$5" | tr '/:' '__')"; [ -f "$f" ] || exit 1; cat "$f" ;;
  clear) rm -f "$DB/$(printf %s "$3.$5" | tr '/:' '__')" ;;
esac"#,
        );
        let store = SecretService::new(&bin).with_service("ai.ligus.secrets.test");
        roundtrip(&store);
        // Values reach the service base64-encoded, never raw.
        store.put(&SecretId::new("settings", "t"), b"plain-secret").unwrap();
        let stored: Vec<String> = std::fs::read_dir(dir.path().join("db"))
            .unwrap()
            .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
            .collect();
        assert!(stored.iter().all(|s| !s.contains("plain-secret")));
    }

    #[cfg(unix)]
    #[test]
    fn vault_roundtrip_systemd_creds() {
        let dir = tempfile::tempdir().unwrap();
        // systemd-creds [--user] encrypt --with-key=auto --name=N - OUT
        // systemd-creds [--user] decrypt --name=N IN -
        let bin = fake_tool(
            dir.path(),
            "systemd-creds",
            r#"[ "$1" = "--user" ] && shift
case "$1" in
  encrypt) base64 > "$5" ;;
  decrypt) base64 -d < "$3" 2>/dev/null || base64 -D < "$3" ;;
esac"#,
        );
        let data = dir.path().join("data");
        let store = SystemdCreds { credentials_directory: None, ..SystemdCreds::new(&bin, &data, true) };
        roundtrip(&store);
        store.put(&SecretId::new("cli", "token"), b"hrt1-secret").unwrap();
        let cred = data.join("credentials/ligus.cli.token.cred");
        assert!(!std::fs::read_to_string(&cred).unwrap().contains("hrt1-secret"));
        #[cfg(unix)]
        {
            assert_eq!(mode(&cred), 0o600);
            assert_eq!(mode(&data.join("credentials")), 0o700);
        }
        // A credential the unit loaded (LoadCredentialEncrypted=) wins.
        let creds_dir = dir.path().join("run-creds");
        std::fs::create_dir_all(&creds_dir).unwrap();
        std::fs::write(creds_dir.join("ligus.cli.token"), "from-unit").unwrap();
        let store = SystemdCreds { credentials_directory: Some(creds_dir), ..SystemdCreds::new(&bin, &data, true) };
        assert_eq!(store.get(&SecretId::new("cli", "token")).unwrap().unwrap().as_slice(), b"from-unit");
    }

    /// A throwaway stand-in for the login keychain (tests never write to
    /// the real one): a keychain file in `dir` with a random password.
    #[cfg(target_os = "macos")]
    fn throwaway_unlock_keychain(dir: &Path) -> PathBuf {
        use security_framework::os::macos::keychain::CreateOptions;
        let path = dir.join("unlock-standin.keychain-db");
        let pw = uuid::Uuid::new_v4().simple().to_string();
        CreateOptions::new().password(&pw).prompt_user(false).create(&path).unwrap();
        path
    }

    #[cfg(target_os = "macos")]
    fn keychain_opt_in() -> bool {
        if std::env::var("LIGUS_TEST_KEYCHAIN").ok().as_deref() != Some("1") {
            eprintln!("skipped: set LIGUS_TEST_KEYCHAIN=1 to exercise the real keychain (throwaway keychains only)");
            return false;
        }
        true
    }

    /// The real macOS keychain, only with `LIGUS_TEST_KEYCHAIN=1` (CI never
    /// writes to a keychain): a throwaway keychain file and service name,
    /// with the unlock password in a throwaway stand-in for the login
    /// keychain, all removed afterwards.
    #[cfg(target_os = "macos")]
    #[test]
    fn vault_roundtrip_mac_keychain_opt_in() {
        if !keychain_opt_in() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let unlock = throwaway_unlock_keychain(dir.path());
        let service = format!("ai.ligus.secrets.test-{}", uuid::Uuid::new_v4());
        let path = dir.path().join("secrets/ligus-test.keychain-db");
        let store = MacKeychain::open_with_unlock_in(path.clone(), &service, unlock.clone()).unwrap();
        roundtrip(&store);
        store.put(&SecretId::new("settings", "probe"), b"keychain-secret").unwrap();
        // The secret isn't in the file in the clear, and the keychain file
        // lives under the data dir (which the agent sandbox denies).
        assert!(path.exists());
        let raw = std::fs::read(&path).unwrap();
        assert!(!raw.windows(15).any(|w| w == b"keychain-secret"));
        // Reopening (a restart) finds the unlock password and the item.
        drop(store);
        let again = MacKeychain::open_with_unlock_in(path.clone(), &service, unlock.clone()).unwrap();
        assert_eq!(again.get(&SecretId::new("settings", "probe")).unwrap().unwrap().as_slice(), b"keychain-secret");
        // The Ligus keychain is not in the user's search list, so a lookup
        // by service alone (what any process with keychain access does)
        // doesn't find Ligus's items.
        let out = std::process::Command::new("/usr/bin/security").args(["find-generic-password", "-s", &service]).output().unwrap();
        assert!(!out.status.success(), "found Ligus's item through the default search list");
        assert!(again.unlock_item_exists());
        let probe = MacKeychain::unopened(path.clone(), &service, Some(unlock.clone()));
        again.destroy().unwrap();
        assert!(!path.exists());
        assert!(!probe.unlock_item_exists(), "the unlock password stayed behind");
    }

    /// Track B residual 6: the unlock password's access list trusts only the
    /// Ligus binary (here, the test executable), named explicitly, rather
    /// than defaulting to whatever process created the item. Throwaway
    /// keychains only, with `LIGUS_TEST_KEYCHAIN=1`.
    #[cfg(target_os = "macos")]
    #[test]
    fn unlock_item_trusts_only_the_ligus_binary() {
        if !keychain_opt_in() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let unlock = throwaway_unlock_keychain(dir.path());
        let service = format!("ai.ligus.secrets.test-{}", uuid::Uuid::new_v4());
        let path = dir.path().join("secrets/ligus-test.keychain-db");
        let store = MacKeychain::open_with_unlock_in(path.clone(), &service, unlock.clone()).unwrap();
        let out = std::process::Command::new("/usr/bin/security").arg("dump-keychain").arg("-a").arg(&unlock).output().unwrap();
        let dump = String::from_utf8_lossy(&out.stdout);
        let me = mac::acl::ligus_binary().unwrap();
        // `0: /path/to/binary (OK)` under each entry's applications.
        let apps: Vec<&str> = dump
            .lines()
            .map(str::trim)
            .filter_map(|l| l.split_once(": /").filter(|(n, _)| n.chars().all(|c| c.is_ascii_digit())).map(|(_, rest)| rest))
            .collect();
        assert!(!apps.is_empty(), "no trusted applications listed:\n{dump}");
        let me = me.to_string_lossy();
        assert!(apps.iter().all(|a| format!("/{a}").starts_with(&*me)), "trusts something other than the Ligus binary {me}:\n{dump}");
        assert!(dump.contains("description: Ligus keychain unlock"), "not the explicit access list:\n{dump}");
        store.destroy().unwrap();
    }

    /// Track B residual 6: a release app trusts itself with the unlock
    /// password only if it satisfies Ligus's designated requirement (Apple
    /// anchor, bundle id, team id). This test binary isn't signed as Ligus,
    /// so it is refused. No keychain is touched.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_binary_not_signed_as_ligus_is_not_trusted_with_the_unlock_password() {
        let app = Path::new("/Applications/Ligus.app");
        let req = mac::acl::requirement(Some("ABCDE12345"), app).unwrap();
        assert_eq!(
            req,
            "anchor apple generic and identifier \"ai.ligus.desktop\" and certificate leaf[subject.OU] = \"ABCDE12345\""
        );
        let err = mac::acl::check_self_against(&req).unwrap_err().to_string();
        assert!(err.contains("isn't signed as Ligus"), "{err}");
        // No team id (a development build), or not an app bundle (the CLI):
        // no requirement, and the access list pins the binary's code hash.
        assert!(mac::acl::requirement(None, app).is_none());
        assert!(mac::acl::requirement(Some("ABCDE12345"), Path::new("/usr/local/bin/ligus")).is_none());
    }

    /// A standalone binary that names its identifier (the terminal app) gets
    /// the same requirement as the app, under its own identifier, bundle or
    /// not; a development build (no team) still gets none. No keychain is
    /// touched.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_named_standalone_binary_gets_its_own_requirement() {
        let bin = Path::new("/usr/local/bin/lt");
        let req = mac::acl::requirement_as(Some("ABCDE12345"), bin, Some("ai.ligus.term")).unwrap();
        assert_eq!(req, "anchor apple generic and identifier \"ai.ligus.term\" and certificate leaf[subject.OU] = \"ABCDE12345\"");
        assert!(mac::acl::check_self_against(&req).is_err());
        assert!(mac::acl::requirement_as(None, bin, Some("ai.ligus.term")).is_none());
        assert_eq!(mac::acl::requirement_as(Some("T"), bin, Some(" ")), None);
        assert_eq!(mac::acl::requirement_as(Some("T"), Path::new("/A/Ligus.app"), None), mac::acl::requirement(Some("T"), Path::new("/A/Ligus.app")));
    }

    #[test]
    fn the_binary_identifier_is_set_once() {
        assert!(!set_binary_identifier("  "));
        let first = binary_identifier().is_none();
        assert!(set_binary_identifier("ai.ligus.test-binary") || !first);
        assert!(!set_binary_identifier("ai.ligus.other") || binary_identifier() == Some("ai.ligus.other"));
        assert!(binary_identifier().is_some());
    }

    /// Track B residual 6: on macOS a Ligus keychain that can't be used
    /// doesn't silently degrade to plain files. Every use fails with an error
    /// that names the opt-in, nothing reaches the data dir, and only an
    /// explicit `file` choice keeps secrets in owner-only files.
    #[test]
    fn an_unavailable_keychain_refuses_instead_of_degrading_to_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = keychain_or_refuse(Err::<File0600, _>(backend_err("keychain", "no keychain here")));
        assert_eq!(store.backend(), Backend::Unavailable);
        assert!(!store.backend().is_plain_file());
        let id = SecretId::new("settings", "api_key");
        let err = store.put(&id, b"secret-value").unwrap_err().to_string();
        assert!(err.contains("LIGUS_SECRET_BACKEND=file") && err.contains("no keychain here"), "{err}");
        assert!(store.get(&id).is_err(), "a read doesn't pass for 'not set'");
        assert!(store.delete(&id).is_err());
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none(), "something was written under the data dir");
        assert_eq!(open(dir.path(), Some("file")).backend(), Backend::File0600, "the explicit opt-in");
    }

    /// An in-memory store (property tests; the file and CLI backends have
    /// their own round trips above).
    #[derive(Default)]
    struct Mem(Mutex<HashMap<SecretId, Vec<u8>>>);
    impl SecretStore for Mem {
        fn backend(&self) -> Backend {
            Backend::File0600
        }
        fn get(&self, id: &SecretId) -> Result<Option<Zeroizing<Vec<u8>>>, SecretError> {
            Ok(self.0.lock().unwrap().get(id).cloned().map(Zeroizing::new))
        }
        fn put(&self, id: &SecretId, value: &[u8]) -> Result<(), SecretError> {
            self.0.lock().unwrap().insert(id.clone(), value.to_vec());
            Ok(())
        }
        fn delete(&self, id: &SecretId) -> Result<(), SecretError> {
            self.0.lock().unwrap().remove(id);
            Ok(())
        }
    }

    const PATHS: SecretPaths = &[
        "relay_access_token",
        "api_key",
        "provider_keys/*",
        "mcp_oauth/*/accessToken",
        "mcp_oauth/*/refreshToken",
        "workspaces[room]/key",
        "workspaces[room]/ek",
    ];

    fn settings_with(secrets: &[String]) -> serde_json::Value {
        serde_json::json!({
            "relay_url": "https://relay.example",
            "relay_access_token": secrets[0],
            "api_key": secrets[1],
            "provider_keys": { "anthropic": secrets[2], "openAI": secrets[3] },
            "mcp_oauth": { "linear": { "accessToken": secrets[4], "refreshToken": secrets[5], "clientId": "public-client-id" } },
            "workspaces": [
                { "name": "Home", "room": "home", "key": secrets[6], "ek": [{ "v": 1, "k": secrets[7] }] },
                { "name": "Plain", "room": "plain" }
            ],
            "git_email": "a@example.com"
        })
    }

    proptest! {
        #[test]
        // Values under 16 characters (the redaction registry ignores them,
        // so 256 cases don't grow it); where they sit is what's tested.
        fn no_secret_values_in_settings_json(secrets in proptest::collection::vec("[A-Za-z0-9+/_-]{10,15}", 8)) {
            let dir = tempfile::tempdir().unwrap();
            let store = Mem::default();
            let path = dir.path().join("settings.json");
            let value = settings_with(&secrets);
            save_settings_json(&path, &store, "settings", value.clone(), PATHS).unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            for s in &secrets {
                prop_assert!(!text.contains(s.as_str()), "a secret reached settings.json");
            }
            prop_assert!(text.contains("public-client-id") && text.contains("a@example.com"), "non-secrets stay");
            let back = load_settings_json(&path, &store, "settings", PATHS).unwrap();
            prop_assert_eq!(back, value);
        }
    }

    #[cfg(unix)]
    #[test]
    fn settings_file_is_0600() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{}").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        let store = File0600::new(dir.path());
        save_settings_json(&path, &store, "settings", serde_json::json!({ "api_key": "k" }), PATHS).unwrap();
        assert_eq!(mode(&path), 0o600);
    }

    #[test]
    fn removed_secrets_are_deleted_from_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = File0600::new(dir.path().join("store"));
        let path = dir.path().join("settings.json");
        let mut v = settings_with(&(0..8).map(|i| format!("secret-number-{i}")).collect::<Vec<_>>());
        save_settings_json(&path, &store, "settings-del", v.clone(), PATHS).unwrap();
        v["provider_keys"].as_object_mut().unwrap().remove("openAI");
        v["workspaces"].as_array_mut().unwrap().remove(0);
        save_settings_json(&path, &store, "settings-del", v.clone(), PATHS).unwrap();
        assert!(store.get(&SecretId::new("settings-del", "provider_keys/openAI")).unwrap().is_none());
        assert!(store.get(&SecretId::new("settings-del", "workspaces/home/key")).unwrap().is_none());
        assert!(store.get(&SecretId::new("settings-del", "provider_keys/anthropic")).unwrap().is_some());
        assert_eq!(load_settings_json(&path, &store, "settings-del", PATHS).unwrap(), v);
    }

    #[test]
    fn plaintext_secrets_left_in_a_file_are_scrubbed_not_imported() {
        // 0.1 starts from scratch: a settings.json holding plaintext secrets
        // (hand-edited, or from a Hive 1.x build) loses them, and the file is
        // rewritten without them.
        let dir = tempfile::tempdir().unwrap();
        let store = File0600::new(dir.path().join("store"));
        let path = dir.path().join("settings.json");
        let old = settings_with(&(0..8).map(|i| format!("old-plaintext-{i}")).collect::<Vec<_>>());
        std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let loaded = load_settings_json(&path, &store, "settings-scrub", PATHS).unwrap();
        assert!(loaded.get("relay_access_token").is_none());
        assert!(loaded["provider_keys"].as_object().unwrap().is_empty());
        assert_eq!(loaded["relay_url"], "https://relay.example");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("old-plaintext"), "{text}");
        assert!(store.get(&SecretId::new("settings-scrub", "relay_access_token")).unwrap().is_none(), "nothing was imported");
    }

    #[test]
    fn key_vault_over_the_store() {
        use crate::identity_store::{IdentityStore, KeyVault};
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn SecretStore> = Arc::new(File0600::new(dir.path().join("store")));
        let vault = StoreKeyVault::new(store.clone());
        vault.store_seed("device:x", &[3; 32]).unwrap();
        assert_eq!(vault.load_seed("device:x").unwrap(), Some([3; 32]));
        assert_eq!(vault.load_seed("device:y").unwrap(), None);
        let ids = IdentityStore::new(dir.path(), StoreKeyVault::new(store));
        let stored = ids.bootstrap("Ana", "ana", "Mac").unwrap();
        assert!(ids.device_keypair(stored.device.id).is_ok());
        let ka = ids.key_agreement(stored.device.id).unwrap();
        assert_eq!(ids.key_agreement(stored.device.id).unwrap().public_key_bytes(), ka.public_key_bytes(), "kept in the vault");
    }
}
