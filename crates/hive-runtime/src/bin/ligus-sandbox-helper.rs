//! The Linux sandbox's pre-exec helper as a standalone binary (tests, and
//! hosts whose own executable doesn't carry it). Ligus's own binaries run it
//! from `main` through `executor::linux::maybe_run_helper`.

fn main() {
    #[cfg(target_os = "linux")]
    hive_runtime::executor::linux::maybe_run_helper();
    eprintln!("ligus-sandbox-helper only runs as Ligus's Linux sandbox helper");
    std::process::exit(2);
}
