//! `ligus-mcp-bridge`: the Ligus MCP stdio bridge as its own binary (the
//! `ligus` CLI and the desktop app also run it as `<binary> mcp-bridge`).
//! See `ligus_runtime::ligus_mcp::bridge`.

fn main() {
    std::process::exit(ligus_runtime::ligus_mcp::bridge::run_stdio());
}
