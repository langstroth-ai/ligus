// Hide the extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Inside the Linux agent sandbox this process is only the pre-exec helper.
    #[cfg(target_os = "linux")]
    hive_runtime::executor::linux::maybe_run_helper();
    // Inside an agent's sandbox this process may be the Ligus MCP bridge.
    hive_runtime::ligus_mcp::bridge::maybe_run_bridge();
    hive_app_lib::run();
}
