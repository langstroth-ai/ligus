//! Where forge (GitHub, GitLab, Forgejo, ...) tokens live: only in the OS
//! secret store of the device or box that does the git work, under scope
//! `forge`, name `<forge id>:<purpose>` (spec: IDENTITY-AND-FORGES §5.4).
//!
//! - Read and write tokens are separate secrets. The read token serves
//!   listings and the read-only agent tools; the write token serves pushes
//!   and opening change requests, on the host only. Asking for one never
//!   returns the other.
//! - Never in `settings.json`, `hive.db`, synced events, invites, contact
//!   records, logs, or any relay request (the relay client has no way to
//!   carry one). They are never synced: another device connects its own.
//! - A token is registered with the log redactor when it is read or
//!   written, so an accidental log line shows a placeholder instead.

use zeroize::Zeroizing;

use crate::secret_store::{SecretError, SecretId, SecretStore};

/// The secret-store scope of forge tokens.
pub const FORGE_SCOPE: &str = "forge";

/// What a forge token may be used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenPurpose {
    Read,
    Write,
    /// An OAuth refresh token from a device sign-in (never handed out; it
    /// only buys a new write token).
    Refresh,
}

impl TokenPurpose {
    pub fn as_str(self) -> &'static str {
        match self {
            TokenPurpose::Read => "read",
            TokenPurpose::Write => "write",
            TokenPurpose::Refresh => "refresh",
        }
    }
}

/// The secret id of one forge connection's token for `purpose`.
pub fn secret_id(forge_id: &str, purpose: TokenPurpose) -> Result<SecretId, SecretError> {
    let f = forge_id.trim();
    if f.is_empty() || f.chars().any(|c| c.is_control() || c.is_whitespace() || c == '/') {
        return Err(SecretError::Backend { backend: "forge", message: "invalid forge connection id".into() });
    }
    Ok(SecretId::new(FORGE_SCOPE, format!("{f}:{}", purpose.as_str())))
}

/// Store a forge token. An empty token deletes it.
pub fn put_token(store: &dyn SecretStore, forge_id: &str, purpose: TokenPurpose, token: &str) -> Result<(), SecretError> {
    let id = secret_id(forge_id, purpose)?;
    let t = token.trim();
    if t.is_empty() {
        return store.delete(&id);
    }
    crate::secret_redact::register(t);
    store.put(&id, t.as_bytes())
}

/// Read a forge token for exactly `purpose`.
pub fn get_token(store: &dyn SecretStore, forge_id: &str, purpose: TokenPurpose) -> Result<Option<Zeroizing<String>>, SecretError> {
    let t = store.get_string(&secret_id(forge_id, purpose)?)?;
    if let Some(t) = &t {
        crate::secret_redact::register(t);
    }
    Ok(t)
}

/// Remove every token of a connection.
pub fn delete_connection(store: &dyn SecretStore, forge_id: &str) -> Result<(), SecretError> {
    store.delete(&secret_id(forge_id, TokenPurpose::Read)?)?;
    store.delete(&secret_id(forge_id, TokenPurpose::Write)?)?;
    store.delete(&secret_id(forge_id, TokenPurpose::Refresh)?)
}

// ── hive-forge's TokenStore over these slots ───────────────────────────────

/// [`hive_forge::TokenStore`] over the functions above, so the forge
/// crate, `ligus serve` and the desktop share one set of slots.
pub struct ForgeSecrets {
    store: std::sync::Arc<dyn SecretStore>,
}

impl ForgeSecrets {
    pub fn new(store: std::sync::Arc<dyn SecretStore>) -> Self {
        Self { store }
    }

    /// The tokens of `data_dir`'s store.
    pub fn for_data_dir(data_dir: &std::path::Path) -> std::sync::Arc<dyn hive_forge::TokenStore> {
        std::sync::Arc::new(Self::new(crate::secret_store::for_data_dir(data_dir)))
    }
}

fn purpose_of(p: hive_forge::Purpose) -> TokenPurpose {
    match p {
        hive_forge::Purpose::Read => TokenPurpose::Read,
        hive_forge::Purpose::Write => TokenPurpose::Write,
        hive_forge::Purpose::Refresh => TokenPurpose::Refresh,
    }
}

fn forge_err(e: SecretError) -> hive_forge::ForgeError {
    hive_forge::ForgeError::Store(e.to_string())
}

impl hive_forge::TokenStore for ForgeSecrets {
    fn get(&self, forge: &hive_forge::ForgeId, purpose: hive_forge::Purpose) -> Result<Option<hive_forge::Secret>, hive_forge::ForgeError> {
        Ok(get_token(&*self.store, forge.as_str(), purpose_of(purpose))
            .map_err(forge_err)?
            .filter(|t| !t.trim().is_empty())
            .map(|t| hive_forge::Secret::new(t.as_str())))
    }
    fn put(&self, forge: &hive_forge::ForgeId, purpose: hive_forge::Purpose, token: &hive_forge::Secret) -> Result<(), hive_forge::ForgeError> {
        put_token(&*self.store, forge.as_str(), purpose_of(purpose), token.expose()).map_err(forge_err)
    }
    fn delete(&self, forge: &hive_forge::ForgeId, purpose: hive_forge::Purpose) -> Result<(), hive_forge::ForgeError> {
        self.store.delete(&secret_id(forge.as_str(), purpose_of(purpose)).map_err(forge_err)?).map_err(forge_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret_store::File0600;

    #[test]
    fn read_and_write_tokens_are_separate() {
        let dir = std::env::temp_dir().join(format!("ligus-forge-secrets-{}", uuid::Uuid::new_v4()));
        let store = File0600::new(&dir);
        put_token(&store, "github.com", TokenPurpose::Read, "read-token-1").unwrap();
        assert_eq!(get_token(&store, "github.com", TokenPurpose::Read).unwrap().as_deref().map(String::as_str), Some("read-token-1"));
        assert!(get_token(&store, "github.com", TokenPurpose::Write).unwrap().is_none(), "a read token never answers a write");
        put_token(&store, "gitlab:gitlab.example.com", TokenPurpose::Write, "write-token-2").unwrap();
        assert!(get_token(&store, "gitlab:gitlab.example.com", TokenPurpose::Read).unwrap().is_none());
        assert_eq!(secret_id("github.com", TokenPurpose::Write).unwrap().account(), "forge:github.com:write");
        assert!(secret_id("", TokenPurpose::Read).is_err());
        assert!(secret_id("../x", TokenPurpose::Read).is_err());
        delete_connection(&store, "github.com").unwrap();
        assert!(get_token(&store, "github.com", TokenPurpose::Read).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The forge crate's view of the same slots: a token put through
    /// [`ForgeSecrets`] is the one [`get_token`] reads, and it lands only in
    /// the `forge` scope, never in the connection file.
    #[test]
    fn forge_crate_uses_the_same_slots() {
        use hive_forge::TokenStore as _;
        let dir = std::env::temp_dir().join(format!("ligus-forge-secrets-{}", uuid::Uuid::new_v4()));
        let store: std::sync::Arc<dyn SecretStore> = std::sync::Arc::new(File0600::new(&dir));
        let t = ForgeSecrets::new(store.clone());
        let id = hive_forge::ForgeId::parse("gitea:codeberg.org").unwrap();
        t.put(&id, hive_forge::Purpose::Write, &hive_forge::Secret::new("cb-token-XYZ-0123456789")).unwrap();
        assert_eq!(get_token(&*store, "gitea:codeberg.org", TokenPurpose::Write).unwrap().as_deref().map(String::as_str), Some("cb-token-XYZ-0123456789"));
        assert!(t.get(&id, hive_forge::Purpose::Read).unwrap().is_none());
        t.put(&id, hive_forge::Purpose::Refresh, &hive_forge::Secret::new("refresh-0123456789abcdef")).unwrap();
        assert_eq!(t.present(&id), vec![hive_forge::Purpose::Write, hive_forge::Purpose::Refresh]);
        let mut conns = hive_forge::Connections::default();
        conns.upsert(hive_forge::Connection::new(hive_forge::ForgeKind::Gitea, "https://codeberg.org", hive_forge::AuthSource::Token).unwrap());
        conns.save(&dir).unwrap();
        assert!(!std::fs::read_to_string(hive_forge::Connections::path(&dir)).unwrap().contains("cb-token-XYZ"));
        assert!(!crate::secret_redact::redacted("leak cb-token-XYZ-0123456789 here").contains("cb-token-XYZ-0123456789"), "a token read is masked in logs");
        delete_connection(&*store, "gitea:codeberg.org").unwrap();
        assert!(t.present(&id).is_empty(), "removing a connection removes its refresh token too");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
