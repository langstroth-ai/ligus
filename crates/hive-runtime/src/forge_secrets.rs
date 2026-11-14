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
}

impl TokenPurpose {
    pub fn as_str(self) -> &'static str {
        match self {
            TokenPurpose::Read => "read",
            TokenPurpose::Write => "write",
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

/// Remove both tokens of a connection.
pub fn delete_connection(store: &dyn SecretStore, forge_id: &str) -> Result<(), SecretError> {
    store.delete(&secret_id(forge_id, TokenPurpose::Read)?)?;
    store.delete(&secret_id(forge_id, TokenPurpose::Write)?)
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
}
