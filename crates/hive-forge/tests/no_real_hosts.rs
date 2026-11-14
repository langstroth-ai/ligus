//! Test builds never reach a real code host and never read the owner's `gh`
//! login (an early run of the forge tests asked this Mac's `gh` for a token
//! and sent a fake one to codeberg.org). One test per binary: it changes
//! `PATH` for this process.

use std::sync::Arc;

use hive_forge::tokens::{AuthSource, GhCredentials, MemoryTokens, Purpose, Secret, StoreCredentials, TokenStore};
use hive_forge::*;

#[tokio::test]
async fn tests_never_reach_real_code_hosts_or_gh() {
    // A fake `gh` first on PATH that records being run.
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("gh-ran");
    let gh = dir.path().join("gh");
    std::fs::write(&gh, format!("#!/bin/sh\ntouch '{}'\necho gho_fake\n", marker.display())).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![dir.path().to_path_buf()];
    paths.extend(std::env::split_paths(&path));
    std::env::set_var("PATH", std::env::join_paths(paths).unwrap());

    // The host's gh login is never asked for.
    let probe = GhCredentials::new("github.com").probe();
    assert!(probe.is_err(), "a test build read the gh login");
    assert!(!marker.exists(), "a test build ran gh");
    // Nor through the implicit connection a data dir without forges.toml has.
    let empty = tempfile::tempdir().unwrap();
    let implicit = Connections::load(empty.path()).forges.remove(0);
    assert_eq!(implicit.auth, AuthSource::Gh);
    let creds = implicit.credentials(Arc::new(MemoryTokens::default()), None, None);
    assert!(creds.token(Purpose::Read).await.unwrap().is_none());
    assert!(!marker.exists(), "a test build ran gh through the implicit connection");

    // No request leaves for a real host, with or without a token.
    for (kind, base) in [(ForgeKind::Gitea, "https://codeberg.org"), (ForgeKind::GitHub, "https://github.com"), (ForgeKind::GitLab, "https://gitlab.com")] {
        let c = Connection::new(kind, base, AuthSource::Token).unwrap();
        let store = Arc::new(MemoryTokens::default());
        store.put(&c.id, Purpose::Read, &Secret::new("fake-token-never-sent")).unwrap();
        let creds = Arc::new(StoreCredentials::new(store, c.id.clone(), AuthSource::Token, None));
        let err = c.build(creds).whoami().await.unwrap_err().to_string();
        assert!(err.contains("test builds never reach a real code host"), "{base}: {err}");
    }
    let ep = device::DeviceEndpoints {
        device_url: "https://github.com/login/device/code".into(),
        token_url: "https://github.com/login/oauth/access_token".into(),
        client_id: "x".into(),
        scope: None,
        internal: false,
    };
    let err = device::start(&ep).await.unwrap_err().to_string();
    assert!(err.contains("test builds never reach a real code host"), "{err}");
}
