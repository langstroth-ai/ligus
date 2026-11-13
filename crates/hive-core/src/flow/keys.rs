//! Opaque keys for files and repositories in flow records. A touch record
//! must never carry a path or a repo name in clear, so it carries an
//! HMAC-SHA256 of the workspace-relative path under a workspace secret (the
//! workspace key when the workspace is encrypted). Members of the workspace
//! derive identical keys; nobody without the secret can reverse them.

use hkdf::Hkdf;
use sha2::Sha256;
use uuid::Uuid;

/// Domain-separation salt for flow keys. New in this build; distinct from
/// every signing / sealing context.
const FLOW_KEY_CONTEXT: &[u8] = b"ligus-flow-key-v1";

fn hmac_hex(secret: &[u8], label: &str, value: &str) -> String {
    // HKDF-Extract(salt, ikm) is HMAC-SHA256(salt, ikm).
    let mut ikm = Vec::with_capacity(label.len() + value.len() + 1);
    ikm.extend_from_slice(label.as_bytes());
    ikm.push(0);
    ikm.extend_from_slice(value.as_bytes());
    let mut salt = Vec::with_capacity(FLOW_KEY_CONTEXT.len() + secret.len());
    salt.extend_from_slice(FLOW_KEY_CONTEXT);
    salt.extend_from_slice(secret);
    let (prk, _) = Hkdf::<Sha256>::extract(Some(&salt), &ikm);
    prk.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// The secret flow keys are made under: the workspace key when there is one,
/// else the workspace id (a relay-less workspace never shares its log).
pub fn workspace_secret(workspace_key: Option<[u8; 32]>, workspace_id: Uuid) -> Vec<u8> {
    match workspace_key {
        Some(k) => k.to_vec(),
        None => workspace_id.as_bytes().to_vec(),
    }
}

/// Normalise a workspace-relative path: forward slashes, no leading `./` or `/`.
pub fn normalize_path(path: &str) -> String {
    let p = path.replace('\\', "/");
    let p = p.trim_start_matches("./").trim_start_matches('/');
    p.to_string()
}

/// Opaque key of a workspace-relative file path.
pub fn file_key(secret: &[u8], path: &str) -> String {
    format!("f_{}", hmac_hex(secret, "file", &normalize_path(path)))
}

/// Opaque key of a repository (by its root folder name or remote).
pub fn repo_key(secret: &[u8], repo: &str) -> String {
    format!("r_{}", hmac_hex(secret, "repo", repo.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_stable_opaque_and_secret_bound() {
        let s1 = workspace_secret(Some([7u8; 32]), Uuid::nil());
        let s2 = workspace_secret(Some([8u8; 32]), Uuid::nil());
        let k = file_key(&s1, "src/lib.rs");
        assert_eq!(k, file_key(&s1, "./src/lib.rs"));
        assert_eq!(k, file_key(&s1, "src\\lib.rs"));
        assert_ne!(k, file_key(&s2, "src/lib.rs"));
        assert_ne!(k, file_key(&s1, "src/main.rs"));
        assert!(!k.contains("lib"));
        assert_eq!(k.len(), 2 + 32);
        assert_ne!(repo_key(&s1, "src/lib.rs"), k.replacen("f_", "r_", 1));
    }
}
