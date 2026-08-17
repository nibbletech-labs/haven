## v0.1.6: Native Windows

Haven now runs natively on Windows. A PowerShell one-liner installs a prebuilt, checksum-verified `haven.exe` (no WSL, no admin rights, no Rust toolchain), wires the MCP server and skills into the same `%USERPROFILE%` locations Claude Code and Codex read on Windows, and `haven self update` can swap the binary even while it is running. macOS and Linux installs are untouched.

**Install on Windows**

- **One-line install.** `irm https://raw.githubusercontent.com/nibbletech-labs/haven/main/packaging/install.ps1 | iex` downloads the x64 release, verifies its sha256 sidecar, installs to `%LOCALAPPDATA%\Programs\haven\bin`, puts that directory on your user PATH, and runs `haven setup`. No elevation, no Unix shell. Windows on ARM is detected and refused with a clear message rather than a broken download (only x64 is published).
- **Windows release assets.** Every release now ships `haven-<version>-x86_64-pc-windows-msvc.tar.gz` with the same sha256 sidecar format as the other platforms, and CI runs the full workspace suite on Windows alongside Linux.

**Windows-correct behaviour**

- **Self-update can replace a running `haven.exe`.** Windows locks a running binary against overwrite but allows renaming it, so the updater moves the live image aside, moves the verified new one into place, and sweeps the leftovers on later runs.
- **Agent wiring uses the real binary name.** Setup writes `command = "haven.exe"` into the Claude and Codex MCP configs on Windows (MCP clients spawn the server without a shell, which resolves no extensionless names), and `haven doctor` looks for `haven.exe` on PATH.
- **Windows-saved Codex configs parse correctly.** A `config.toml` with Windows line endings no longer mis-reads the `[mcp_servers.haven]` section, so doctor stops false-warning and setup stops rewriting a stanza that was already correct.
- **Symlinks degrade loudly, never silently.** Without Developer Mode, `haven link` falls back to a copy and says so plainly (a snapshot, not a live view), and restoring a backup that contains symlinks reports each entry it could not recreate instead of dropping it without a word.

**Hardening on every platform**

- **Backup restore refuses to leave a stale WAL.** Cleaning up the SQLite `-wal`/`-shm` sidecars after a restore is now a hard error if it fails for any reason other than the file being absent, because a stale WAL beside a restored database replays as silent corruption.
- **Sync hydration cannot escape the project tree.** Remote-supplied artifact paths are re-checked after joining, closing an absolute-path edge case (Windows drive and UNC forms included).

**Upgrade Notes**

- No migration is required.
- macOS and Linux are unaffected: no path resolution, install location, or config format changed there.
- On Windows, restore a backup with no other haven process running (Windows locks the open database file; quit a live `haven mcp` first).
