// Hide the extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Inside the Linux agent sandbox this process is only the pre-exec helper.
    #[cfg(target_os = "linux")]
    ligus_runtime::executor::linux::maybe_run_helper();
    // Inside an agent's sandbox this process may be the Ligus MCP bridge.
    ligus_runtime::ligus_mcp::bridge::maybe_run_bridge();
    // When git runs this binary as the credential helper for a code host.
    ligus_desktop_lib::maybe_run_git_credential();
    ligus_desktop_lib::run();
}
