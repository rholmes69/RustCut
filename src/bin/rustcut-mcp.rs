//! `rustcut-mcp`: the RustCut Model Context Protocol server (stdio transport).
//!
//! Register it with an MCP client, e.g. Claude Code:
//!   claude mcp add rustcut -- /path/to/rustcut-mcp

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    eprintln!("rustcut-mcp {} ready (stdio)", env!("CARGO_PKG_VERSION"));
    let mut server = rustcut::mcp::Server::new(stdout.lock());
    if let Err(e) = server.serve(stdin.lock()) {
        eprintln!("rustcut-mcp: {e}");
        std::process::exit(1);
    }
}
