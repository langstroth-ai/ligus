//! Ligus's own secrets in the OS secret store (Phase 0, B2).
//!
//! Identity seeds, this device's key-agreement key, the workspace passphrase
//! and delivered epoch keys, relay tokens, the relay issuer key, provider API
//! keys, MCP tokens and the ligus-auth refresh token all live behind
//! [`SecretStore`]; `settings.json` and `config.toml` keep only `{"set": true}`
//! placeholders.
//!
//! | Backend | Where |
//! |---|---|
//! | [`MacKeychain`] | macOS: a **dedicated keychain file inside the Ligus data dir**, unlocked with a random password kept in the login keychain |
//! | [`SecretService`] | Linux desktop: the session's Secret Service (through `secret-tool`) |
//! | [`SystemdCreds`] | headless Linux (serve, worker): `systemd-creds encrypt` (TPM2 when present, else the host key) |
//! | [`K8sSecretFile`] | the chart: read-only mounted Secret files, writes fall back to [`File0600`] |
//! | [`File0600`] | fallback: owner-only files, with a warning |
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
//! login-keychain item, and that item's ACL trusts only the Ligus binary;
//! even an agent that talked a user into allowing it would get a password
//! for a file it can't open. On Linux the Secret Service is a D-Bus session
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
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::MacKeychain => "keychain",
            Backend::SecretService => "secret-service",
            Backend::SystemdCreds => "systemd-creds",
            Backend::K8sSecretFile => "k8s",
            Backend::File0600 => "file",
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

/// Write `bytes` to `path` owner-only and atomically, creating parent dirs
/// 0700 ([`crate::private_file::write_private_atomic`]).
pub fn write_private_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
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
        Ok(write_private_atomic(&self.path(id), value)?)
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

/// Read-only Secret files mounted by the operator (`<dir>/<scope>.<name>`);
/// anything not provisioned there (a fresh pod's identity seeds) is kept in
/// [`File0600`] under the data dir.
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
        if self.mounted(id).exists() {
            return Err(SecretError::ReadOnly("a mounted Kubernetes Secret"));
        }
        self.fallback.put(id, value)
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
        std::fs::create_dir_all(&self.dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700))?;
        }
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
    /// (`<service>.unlock`, its ACL trusting only this binary). See the
    /// module docs for why Ligus's items aren't in the login keychain.
    pub struct MacKeychain {
        path: PathBuf,
        service: String,
        lock: Mutex<()>,
    }

    impl MacKeychain {
        /// The keychain for `data_dir` (created on first use).
        pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, SecretError> {
            Self::open_with(data_dir.as_ref().join("secrets").join("ligus.keychain-db"), SERVICE)
        }

        /// A keychain at `path` with its own service name (tests use a
        /// throwaway one).
        pub fn open_with(path: PathBuf, service: &str) -> Result<Self, SecretError> {
            let me = Self { path, service: service.to_string(), lock: Mutex::new(()) };
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

        /// Open (or create) and unlock the keychain.
        fn keychain(&self) -> Result<SecKeychain, SecretError> {
            let err = |e: security_framework::base::Error| backend_err("keychain", e.to_string());
            let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
            let password = match find_generic_password(None, &self.unlock_service(), &self.unlock_account()) {
                Ok((pw, _)) => Some(Zeroizing::new(String::from_utf8_lossy(&pw).into_owned())),
                Err(_) => None,
            };
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
            getrandom::getrandom(&mut raw).map_err(|e| backend_err("keychain", e.to_string()))?;
            let pw = Zeroizing::new(hive_core::keys::hex(&raw));
            SecKeychain::default()
                .and_then(|login| login.set_generic_password(&self.unlock_service(), &self.unlock_account(), pw.as_bytes()))
                .map_err(err)?;
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
            if let Ok((_, item)) = find_generic_password(None, &self.unlock_service(), &self.unlock_account()) {
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
        pub(crate) fn unopened(path: PathBuf, service: &str) -> Self {
            Self { path, service: service.to_string(), lock: Mutex::new(()) }
        }

        /// Whether the unlock password item exists in the login keychain.
        pub fn unlock_item_exists(&self) -> bool {
            find_generic_password(None, &self.unlock_service(), &self.unlock_account()).is_ok()
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
/// that can't be used falls back to [`File0600`] with a warning.
pub fn open(data_dir: &Path, choice: Option<&str>) -> Arc<dyn SecretStore> {
    let choice = choice.map(|c| c.trim().to_ascii_lowercase()).filter(|c| !c.is_empty());
    let fallback = |why: String| -> Arc<dyn SecretStore> {
        tracing::warn!(target: "secrets", "{why}; keeping Ligus's secrets in owner-only files under the data dir (run `ligus doctor`)");
        Arc::new(File0600::new(data_dir))
    };
    match choice.as_deref() {
        Some("file") => return Arc::new(File0600::new(data_dir)),
        Some("k8s") => {
            let mount = hive_core::compat::var_os("LIGUS_SECRETS_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/var/run/secrets/ligus"));
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
        match MacKeychain::open(data_dir) {
            Ok(k) => Arc::new(k),
            Err(e) => fallback(format!("the Ligus keychain isn't available ({e})")),
        }
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
/// imported: 2.0 starts from scratch).
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
        roundtrip(&store);
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

    /// The real macOS keychain, only with `LIGUS_TEST_KEYCHAIN=1` (CI never
    /// writes to a keychain): a throwaway keychain file and service name,
    /// removed afterwards with its unlock password.
    #[cfg(target_os = "macos")]
    #[test]
    fn vault_roundtrip_mac_keychain_opt_in() {
        if std::env::var("LIGUS_TEST_KEYCHAIN").ok().as_deref() != Some("1") {
            eprintln!("skipped: set LIGUS_TEST_KEYCHAIN=1 to exercise the real keychain");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let service = format!("ai.ligus.secrets.test-{}", uuid::Uuid::new_v4());
        let path = dir.path().join("secrets/ligus-test.keychain-db");
        let store = MacKeychain::open_with(path.clone(), &service).unwrap();
        roundtrip(&store);
        store.put(&SecretId::new("settings", "probe"), b"keychain-secret").unwrap();
        // The secret isn't in the file in the clear, and the keychain file
        // lives under the data dir (which the agent sandbox denies).
        assert!(path.exists());
        let raw = std::fs::read(&path).unwrap();
        assert!(!raw.windows(15).any(|w| w == b"keychain-secret"));
        // Reopening (a restart) finds the unlock password and the item.
        drop(store);
        let again = MacKeychain::open_with(path.clone(), &service).unwrap();
        assert_eq!(again.get(&SecretId::new("settings", "probe")).unwrap().unwrap().as_slice(), b"keychain-secret");
        // The Ligus keychain is not in the user's search list, so a lookup
        // by service alone (what any process with keychain access does)
        // doesn't find Ligus's items.
        let out = std::process::Command::new("/usr/bin/security").args(["find-generic-password", "-s", &service]).output().unwrap();
        assert!(!out.status.success(), "found Ligus's item through the default search list");
        assert!(again.unlock_item_exists());
        let probe = MacKeychain::unopened(path.clone(), &service);
        again.destroy().unwrap();
        assert!(!path.exists());
        assert!(!probe.unlock_item_exists(), "the unlock password left the login keychain");
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
        // 2.0 starts from scratch: a settings.json holding plaintext secrets
        // (hand-edited, or from a pre-2.0 build) loses them, and the file is
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
