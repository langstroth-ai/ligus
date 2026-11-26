//! `scripts/release-matrix.sh` keeps API keys off command lines: a key in
//! curl's argv is readable by every local process (`ps`) for the request's
//! lifetime. The test runs the API rows against a stub `curl` that records
//! its argv and the headers it was handed, with fake keys; no network.

#[cfg(unix)]
#[test]
fn release_matrix_keeps_api_keys_off_the_command_line() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let stub = tempfile::tempdir().unwrap();
    let rec = stub.path().join("rec");
    std::fs::create_dir_all(&rec).unwrap();
    // The stub records argv, copies any `-H @file` header file it was given,
    // and answers like a model that said ok.
    let curl = stub.path().join("curl");
    std::fs::write(
        &curl,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{r}/argv'\nprev=\nfor a in \"$@\"; do\n  if [ \"$prev\" = -H ] || [ \"$prev\" = --header ]; then case \"$a\" in @*) cat \"${{a#@}}\" >> '{r}/headers';; esac; fi\n  case \"$prev\" in -K|--config) [ \"$a\" = - ] && cat >> '{r}/headers' || cat \"$a\" >> '{r}/headers';; esac\n  prev=\"$a\"\ndone\necho '{{\"content\":\"ok\"}}'\n",
            r = rec.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        stub.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = std::process::Command::new("bash")
        .arg(root.join("scripts/release-matrix.sh"))
        .env("PATH", path)
        .env("ROWS", "anthropic-api openai-api gemini-api")
        // Logs inside the test's own temp dir, removed by the script on exit
        // (and by the TempDir guard if it isn't).
        .env("MATRIX_OUT_DIR", stub.path().join("out"))
        .env("MATRIX_KEEP_LOGS", "0")
        .env("ANTHROPIC_API_KEY", "fake-anthropic-key-1")
        .env("OPENAI_API_KEY", "fake-openai-key-2")
        .env("GEMINI_API_KEY", "fake-gemini-key-3")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let argv = std::fs::read_to_string(rec.join("argv")).unwrap_or_default();
    let headers = std::fs::read_to_string(rec.join("headers")).unwrap_or_default();
    for key in [
        "fake-anthropic-key-1",
        "fake-openai-key-2",
        "fake-gemini-key-3",
    ] {
        assert!(!argv.contains(key), "{key} was on curl's command line");
        assert!(
            headers.contains(key),
            "{key} never reached curl (headers: {headers:?}); output: {stdout}"
        );
        assert!(!stdout.contains(key), "{key} was printed");
    }
    assert_eq!(stdout.matches("| pass |").count(), 3, "{stdout}");
    assert!(
        !stub.path().join("out").exists(),
        "the script left its logs behind"
    );
}
