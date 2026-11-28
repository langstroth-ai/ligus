//! Clean up's "Remove from view": the leftover keys this device hid, in
//! `<data dir>/setup-hidden.json`. Device-local, never synced, and nothing in
//! the workspace changes: the member is still listed, the device still
//! revoked. A missing or unreadable file hides nothing (the rows show again,
//! which is safe), and a write never drops a key it didn't read.

use std::path::{Path, PathBuf};

/// The file, under the data dir.
pub const FILE: &str = "setup-hidden.json";

/// At most this many keys are kept (the oldest go first).
const MAX_KEYS: usize = 512;

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE)
}

/// The keys hidden on this device.
pub fn load(data_dir: &Path) -> Vec<String> {
    std::fs::read(path(data_dir))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<String>>(&b).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|k| valid(k))
        .collect()
}

/// A key is `kind:id` with no control characters.
fn valid(key: &str) -> bool {
    let k = key.trim();
    !k.is_empty() && k.len() <= 200 && k.contains(':') && !k.chars().any(char::is_control)
}

/// Hide `key` (or with `show`, show it again). Refuses a malformed key.
pub fn set(data_dir: &Path, key: &str, show: bool) -> std::io::Result<Vec<String>> {
    if !valid(key) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a Clean up key (kind:id)"));
    }
    let file = path(data_dir);
    // Read what is there; a file that exists and can't be read is never
    // written over (it may be a newer format).
    let mut keys: Vec<String> = match std::fs::read(&file) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    let key = key.trim().to_string();
    keys.retain(|k| k != &key);
    if !show {
        keys.push(key);
    }
    if keys.len() > MAX_KEYS {
        let drop = keys.len() - MAX_KEYS;
        keys.drain(..drop);
    }
    let json = serde_json::to_vec(&keys).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::create_dir_all(data_dir)?;
    crate::private_file::write_private_atomic(&file, &json)?;
    Ok(keys)
}
