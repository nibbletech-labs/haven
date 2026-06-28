<!-- HAVEN:BEGIN -->
## Haven

Haven is the canonical project work graph. Use `haven` CLI commands locally, or the `haven_*` MCP tools when available. Keep structure in Haven: do not hand-edit `backlog.md`; it is a generated projection.

Discovery:
- Canonical graph/content lives under `~/.haven`.
- Repo-local `_haven/` is a disposable visible workspace/projection when present.
- Codex MCP config is `~/.codex/config.toml` or trusted `.codex/config.toml`: `[mcp_servers.haven]` with `command = "haven"` and `args = ["mcp"]`.
- Codex/Open Agent Skills are read from `.agents/skills`, `~/.agents/skills`, or `/etc/codex/skills`; Claude skills live under `~/.claude/skills`.

Core local verbs:
- `haven project list` / `haven project use <key>` to select a backlog.
- `haven item get <ref> --include edges,artifacts,lineage` to inspect work.
- `haven item add "<title>" --if-absent` to capture without duplicating.
- `haven next --explain` to diagnose an empty dispatch queue.
- `haven item complete <ref> --evidence "<proof>"` to finish with evidence.
<!-- HAVEN:END -->
