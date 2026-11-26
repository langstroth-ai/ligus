//! Every Ligus HTTP client still builds when the OS has no CA certificates
//! (a minimal container, an empty `SSL_CERT_FILE`). reqwest 0.13 takes roots
//! from the platform verifier, which refuses to build with an empty store on
//! Linux; before the upgrade the bundled Mozilla roots made this work, and
//! `Client::new()` now panics instead (taking loopback-only callers, like a
//! local model or relay over plain http, down with it). Its own test binary:
//! it sets process-wide environment before any client exists.

#[test]
fn clients_build_without_a_system_ca_store() {
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("empty.pem");
    std::fs::write(&empty, "").unwrap();
    std::env::set_var("SSL_CERT_FILE", &empty);
    std::env::set_var("SSL_CERT_DIR", dir.path());
    assert!(
        ligus_forge::http_client_builder().build().is_ok(),
        "the client builder builds"
    );
    assert!(
        std::panic::catch_unwind(ligus_forge::new_http_client).is_ok(),
        "new_http_client doesn't panic"
    );
}
