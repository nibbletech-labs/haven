## v0.1.7: Safe concurrent Codex sessions

Haven no longer lets one Codex session's sticky project selection silently retarget another session. This release also makes the local Haven store permission an explicit, narrowly scoped part of Codex setup, so agents can update canonical session metadata without requesting broad filesystem access.

**Concurrent project safety**

- **Repo binding wins over shared sticky state.** Project-scoped CLI commands now resolve in this order: an explicit `-p`, the nearest `.haven-project`, then the sticky selector used as a human-shell fallback outside linked repos. A selector change in another process can no longer move a command running inside a linked project.
- **Explicit overrides remain explicit.** `-p` and the project-bearing `status`/`prime` forms still win, including the existing warning when they intentionally cross a repo binding. Telemetry distinguishes a caller-supplied project from one injected by the repo link.
- **Agents carry the project per call.** The shipped Haven skill now requires the MCP `project` argument or CLI `-p <key>` on every project-scoped operation and tells agents never to run `haven project use`.

**Scoped Codex store access**

- **Opt in during setup.** `haven setup --agent codex --grant-store-access` adds write access only for the resolved Haven root. It preserves unrelated Codex configuration, is idempotent, respects `CODEX_HOME`, never introduces Full Access, and reports legacy read-only or malformed configurations without overwriting them.
- **Both Codex configuration generations are supported safely.** Modern permission profiles get a `haven-local` profile extending the current default; legacy `workspace-write` configurations get only the Haven root appended to `writable_roots`. Haven never mixes the two systems.
- **Installers can make the same explicit grant.** For the POSIX installer, pass `--grant-store-access` or set `HAVEN_GRANT_CODEX_STORE_ACCESS=1`. For PowerShell, set `HAVEN_GRANT_CODEX_STORE_ACCESS=1`. The default remains permission-neutral.
- **Doctor verifies the result.** `haven doctor` reports Codex MCP registration, skill freshness, and whether the active Codex configuration grants the resolved store root. Restart Codex after changing the profile so new sessions inherit it.

**Other hardening**

- The Windows install-check workflow now exercises install-then-self-update composition and proves the native installer refuses unsupported Windows ARM64 instead of fetching a nonexistent asset.
- `orchestrate-plan`'s seal gate now uses the same one-build-pass rule as its planning front door: size alone does not force a coherent item into artificial subtasks.

**Upgrade notes**

- No database migration is required.
- Existing Codex users who want Haven to write outside a repository sandbox should rerun `haven setup --agent codex --grant-store-access`, then start a new Codex session.
- Existing human CLI behavior outside linked repos is unchanged; the sticky selector remains available there.
