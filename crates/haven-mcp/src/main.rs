//! `haven-mcp` — standalone entry point for the MCP server. The primary entry
//! is `haven mcp` (single binary); this exists for direct invocation/testing.
//! Resolves the store from `$HAVEN_HOME` (or `~/.haven`) and serves over stdio.

use std::path::PathBuf;

use haven_core::Store;

fn main() {
    if let Err(e) = run() {
        eprintln!("haven-mcp: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let root = match std::env::var_os("HAVEN_HOME") {
        Some(h) => PathBuf::from(h),
        None => {
            // BaseDirs, not $HOME: %HOME% isn't set on native Windows, and this
            // matches how haven-cli resolves the same directory (the known
            // folder there, untouched by env-var overrides).
            let base = directories::BaseDirs::new()
                .ok_or("cannot resolve the home directory for ~/.haven")?;
            base.home_dir().join(".haven")
        }
    };
    std::fs::create_dir_all(&root)?;
    let store = Store::open(root.join("haven.db"), &root)?;
    haven_mcp::serve(&store)?;
    Ok(())
}
