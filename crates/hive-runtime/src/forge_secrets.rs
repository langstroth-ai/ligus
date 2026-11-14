//! Forge tokens in the OS secret store: [`hive_forge::TokenStore`] over
//! [`crate::secret_store::SecretStore`], scope `forge`, name
//! `<forge id>:<purpose>` (spec IDENTITY-AND-FORGES 5.4).
//!
//! These are the only forge-token slots in Ligus. They live on the device
//! or box that does the git work, are never synced, never sent to the
//! relay, never written to `settings.json` or `hive.db`. Every token read
//! is registered with [`crate::secret_redact`], so a token an agent somehow
//! echoed is masked before it reaches the log.

use std::sync::Arc;

use hive_forge::tokens::{secret_name, Purpose, Secret, TokenStore};
use hive_forge::{ForgeError, ForgeId};

use crate::secret_store::{SecretId, SecretStore};

/// The secret-store scope of forge tokens.
pub const SCOPE: &str = "forge";

pub struct ForgeSecrets {
    store: Arc<dyn SecretStore>,
}

impl ForgeSecrets {
    pub fn new(store: Arc<dyn SecretStore>) -> Self {
        Self { store }
    }

    /// The tokens of `data_dir`'s store.
    pub fn for_data_dir(data_dir: &std::path::Path) -> Arc<dyn TokenStore> {
        Arc::new(Self::new(crate::secret_store::for_data_dir(data_dir)))
    }

    fn id(forge: &ForgeId, purpose: Purpose) -> SecretId {
        SecretId::new(SCOPE, secret_name(forge, purpose))
    }
}

fn err(e: crate::secret_store::SecretError) -> ForgeError {
    ForgeError::Store(e.to_string())
}

impl TokenStore for ForgeSecrets {
    fn get(&self, forge: &ForgeId, purpose: Purpose) -> Result<Option<Secret>, ForgeError> {
        let v = self.store.get_string(&Self::id(forge, purpose)).map_err(err)?;
        Ok(v.filter(|t| !t.trim().is_empty()).map(|t| {
            crate::secret_redact::register(&t);
            Secret::new(t.as_str())
        }))
    }
    fn put(&self, forge: &ForgeId, purpose: Purpose, token: &Secret) -> Result<(), ForgeError> {
        crate::secret_redact::register(token.expose());
        self.store.put(&Self::id(forge, purpose), token.expose().as_bytes()).map_err(err)
    }
    fn delete(&self, forge: &ForgeId, purpose: Purpose) -> Result<(), ForgeError> {
        self.store.delete(&Self::id(forge, purpose)).map_err(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `no_forge_token_leaves_device` (store side): a token put through
    /// the forge slots lands only in the store's own `forge` scope, never in
    /// the connection file or anything else under the data dir in clear.
    #[test]
    fn forge_tokens_live_only_in_the_forge_scope() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn SecretStore> = Arc::new(crate::secret_store::File0600::new(dir.path()));
        let t = ForgeSecrets::new(store.clone());
        let id = ForgeId::parse("gitea:codeberg.org").unwrap();
        t.put(&id, Purpose::Write, &Secret::new("cb-token-XYZ-0123456789")).unwrap();
        assert_eq!(t.get(&id, Purpose::Write).unwrap().unwrap().expose(), "cb-token-XYZ-0123456789");
        assert!(t.get(&id, Purpose::Read).unwrap().is_none());
        assert_eq!(store.get_string(&SecretId::new("forge", "gitea:codeberg.org:write")).unwrap().unwrap().as_str(), "cb-token-XYZ-0123456789");
        let mut conns = hive_forge::Connections::default();
        conns.upsert(hive_forge::Connection::new(hive_forge::ForgeKind::Gitea, "https://codeberg.org", hive_forge::AuthSource::Token).unwrap());
        conns.save(dir.path()).unwrap();
        let text = std::fs::read_to_string(hive_forge::Connections::path(dir.path())).unwrap();
        assert!(!text.contains("cb-token-XYZ-0123456789"));
        assert!(!crate::secret_redact::redacted("leak cb-token-XYZ-0123456789 here").contains("cb-token-XYZ-0123456789"), "a token read is masked in logs");
        t.delete_all(&id).unwrap();
        assert!(t.present(&id).is_empty());
    }
}
