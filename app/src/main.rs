// Hide the extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Inside the Linux agent sandbox this process is only the pre-exec helper.
    #[cfg(target_os = "linux")]
    hive_runtime::executor::linux::maybe_run_helper();
    hive_app_lib::run();
}
