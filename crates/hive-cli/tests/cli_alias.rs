//! The binary is `ligus`; run under its former name `hive` (release alias or
//! symlink) it still works and prints a one-line reminder on stderr only.

use std::path::Path;
use std::process::Command;

fn run(bin: &Path) -> (String, String) {
    let out = Command::new(bin).arg("--version").output().expect("run cli");
    assert!(out.status.success(), "{out:?}");
    (String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn hive_alias_runs_and_warns_on_stderr_only() {
    let ligus = Path::new(env!("CARGO_BIN_EXE_ligus"));
    let (stdout, stderr) = run(ligus);
    assert!(stdout.starts_with("ligus "), "{stdout}");
    assert!(stderr.is_empty(), "no note under the new name: {stderr}");

    let dir = tempfile::tempdir().unwrap();
    let alias = dir.path().join(if cfg!(windows) { "hive.exe" } else { "hive" });
    std::fs::copy(ligus, &alias).unwrap();
    let (stdout, stderr) = run(&alias);
    assert!(stdout.starts_with("ligus "), "stdout is unchanged for scripts: {stdout}");
    assert_eq!(stderr.trim(), "hive: this command is now 'ligus'; 'hive' will stop working in v2.2");
}
