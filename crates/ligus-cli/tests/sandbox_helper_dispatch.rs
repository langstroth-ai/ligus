//! The shipped `ligus` binary (also `ligus serve`, `ligus worker` and the
//! container image's `/usr/local/bin/ligus`) carries the Linux sandbox's
//! pre-exec helper: re-executed with the helper argument, its own `main`
//! hands over to the helper before parsing anything as a CLI command. The
//! sandbox relies on that whenever no `ligus-sandbox-helper` is installed
//! next to it.
#![cfg(target_os = "linux")]

use std::process::Command;

#[test]
fn the_ligus_binary_dispatches_the_sandbox_helper() {
    let data = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_ligus"))
        .arg(ligus_runtime::executor::linux::HELPER_ARG)
        .arg("--not-a-helper-option")
        .env("LIGUS_DATA_DIR", data.path())
        .env("LIGUS_SECRET_BACKEND", "file")
        .env("HOME", data.path())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    // The helper's own refusal (exit 126), not the CLI's argument parser.
    assert_eq!(out.status.code(), Some(126), "{err}");
    assert!(
        err.contains("ligus sandbox: unknown helper option --not-a-helper-option"),
        "{err}"
    );
}
