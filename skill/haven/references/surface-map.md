# Haven surface map — CLI, MCP, and how they differ

The two front-ends drive the **same** store but are **not 1:1**. The CLI has many
friendly verbs (for a human typing); the MCP is a deliberately smaller, more
general set of 24 tools (for an agent). When a workflow runs over MCP, translate
using the mapping below.

## Contents
- [Enums (valid values)](#enums)
- [CLI command surface](#cli-command-surface)
- [MCP tool catalogue (24 tools)](#mcp-tool-catalogue)
- [CLI → MCP mapping](#cli--mcp-mapping)
- [CLI-only operations](#cli-only-operations)
- [The content channel](#the-content-channel)

---

## Enums

Used everywhere a `--type` / `--status` / `--role` etc. is accepted. Invalid
values error.

- **Node type** (`--type`): `task` (default), `code`, `research`, `data`,
  `design`, `admin`, `release`, `phase`, `gate`, `anchor`. `release`/`phase`/
  `gate` are container nodes (group targets); `anchor` is for living project docs.
- **Status** (`--status`): `discovery` (default) → `definition` → `ready` →
  `in_progress` → `done`, plus `blocked`, `superseded`, `archived`.
- **Owner — assignment** (`--to` / `--assign`, stored `owner_kind`): `human`, `ai`
  — NULL = unassigned (who IS doing it). **`next --owner human|ai` filters on
  `owner_kind`** (`owner_kind = <owner>`); an unassigned (NULL) leaf is **never**
  auto-pulled — when readying AI work, `assign` it to `ai`.
- **Wait state** (`--wait`): `on_human`, `on_dependency`, `on_external`, and
  `none` to clear.
- **Artifact role** (`--role`): `spec`, `research`, `design`, `decision`,
  `handoff`, `vision`, `source`, `delivery`, `scratch`, `context-pack` (the
  build-ready brief on a grouping container — HV-124; resolution keys on the role).
- **Artifact kind** (`--kind`, usually inferred): `file`, `external`, `delivery`.

**Global CLI flags:** `--project/-p <key>` (defaults to the current project),
`--pretty` (tables instead of JSON).

## CLI command surface

```
# Setup & introspection
haven setup [--agent all|claude|codex] [--no-skill] | init | status | doctor
haven config get <key> | set <key> <value>
haven link [--name Haven]  # visible repo-local workspace/projection; canonical state stays in ~/.haven

# Projects
haven project add --key <k> --title <t> [--prefix HV] [--description …]
haven project list | get <key> | use <key>

# Items (nodes)
haven item add "<title>" [--type] [--body] [--done-looks-like "…"] [--why "…"]
                         [--status] [--priority N] [--commit] [--assign human|ai] [--due-at YYYY-MM-DD]
                         [--parent <ref>] [--depends-on <ref>] [--group <ref>]
                         [--if-absent]   # normalized-title dedupe: return the existing item
haven import <file.json> [--if-absent]  # bulk add: one validated, all-or-nothing transaction;
                                        # items take the add fields + temp `id` and ref-or-temp-id
                                        # parent / depends_on (array) / group edge fields
haven item list [--status] [--type] [--owner] [--committed] [--icebox] [--group <ref>]
                [--wait on_human|on_dependency|on_external] [--stale <days>]
haven item get <ref> [--include edges,artifacts,lineage]
haven item update <ref>… [--title] [--body] [--done-looks-like "…"] [--why "…"]
                        [--status] [--priority N] [--type] [--wait]
                        [--due-at YYYY-MM-DD|none]   # 1+ refs, same update each; `none` clears due-at
haven item commit <ref>… [--priority N]      # one or more refs (grooming)
haven item uncommit <ref>…
haven item assign <ref> --to human|ai [--actor <name>]
haven item handoff <ref> --to human|ai [--from] [--note "…"] [--status] [--wait] [--actor]
haven item complete <ref> [--evidence "…"] [--role delivery] [--by]
haven item rank <ref> [--before <ref>] [--after <ref>]
haven item archive <ref>… [--rationale "…"]  # one or more refs (grooming)
haven item reopen  <ref> [--rationale "…"]

# Dispatch
haven next [--owner human|ai] [--limit N]   # --owner = ASSIGNMENT filter (owner_kind = owner); unassigned (NULL) excluded
haven graph [--lineage]        # whole project: all nodes + edges in one read
haven docs                     # live project-doc anchors + their artifacts

# Edges
haven decompose <parent> [--into <ref> …] [--remove <ref> …]
haven depend    <node>   [--on <ref> …]   [--remove <ref> …]
haven group     <group>  [--add <ref> …]  [--remove <ref> …]

# Evolve (lineage)
haven evolve split <ref> --into "<title>" [--into …] [--rationale] [--by]
haven evolve merge <ref> <ref> … --title "<t>" [--rationale] [--by]
haven evolve supersede <ref> --with <ref> [--rationale] [--by]
haven evolve graph <ref> [--direction ancestors|descendants|both] [--depth N]
haven evolve resolve <ref>     # stale ref → its live descendant(s)

# Search & content
haven search "<query>" [--limit N]
haven xref <ref>                # cross-store links: outbound xrefs + inbound backlinks (read-only)
haven artifact add <ref> --role <role> [--file <path> | --content "…"] [--name <f>] [--replace]
                         [--kind] [--uri] [--title] [--excerpt] [--from] [--to] [--by]
                         # --name sets the destination filename (also for --file);
                         # --replace overwrites an existing same-path artifact in place
haven artifact list <ref> [--role <role>]
haven artifact get  <ref> [--role <role>] [--path <relpath>]
haven artifact rm   <ref> (--role <r> | --name <f> | --id <pid>)   # remove row + file
haven artifact mv   <ref> <new-name> (--role <r> | --name <f> | --id <pid>)  # rename file
haven note <ref> "<text>"
haven render
haven skill install [--agent all|claude|codex]

# Server / cloud
haven mcp
haven auth login [--token <jwt>] | logout | status
haven sync [status] [--watch]
```

## MCP tool catalogue

26 tools, each taking an optional `project` and naming items by `ref` or
`public_id`. Required args in **bold**.

| Tool | Args |
|---|---|
| `haven_list_items` | `status?, type?, owner?, committed?, icebox?, group?, wait?, stale?, limit?, offset?` — returns a compact, paginated envelope `{total, count, offset, items[]}` (default `limit` 100) |
| `haven_inbox` | `owner?, limit?, offset?` — untriaged floaters (uncommitted, live, no `done_looks_like` yet); same compact paginated envelope as `haven_list_items` |
| `haven_xref` | **`ref`** — cross-store links on the node's artifacts: a sorted `{node, outbound[], inbound[]}` report (outbound xrefs + inbound backlinks); read-only |
| `haven_get_item` | **`ref`**, `include?: ["edges","artifacts","lineage"]` — the full item (prose + includes); the detail door |
| `haven_next` | `owner?, limit?` — compact items; `owner` filters ASSIGNMENT (`owner_kind = owner`), unassigned (NULL) excluded |
| `haven_next_explain` | `owner?` — diagnose an empty queue (counts by reason + hint) |
| `haven_rank` | **`ref`**, `before?` \| `after?` (exactly one) — reorder within a priority band (fine ordering) |
| `haven_add_item` | **`title`**, `type?, body?, done_looks_like?, why?, status?, priority?, commit?, assign?, due_at?, parent?, depends_on?, group?, if_absent?` — with `if_absent` a normalized-title match returns the existing item (`existing: true`); responses may carry advisory `similar` |
| `haven_update_item` | **`ref`**, `title?, body?, done_looks_like?, why?, status?, priority?, type?, wait?, due_at?, commit?, assign?, group?, actor?` (`due_at` accepts `"none"` to clear; `group` adds the item to a release/phase/gate container, mirroring `haven_add_item`) |
| `haven_add_edge` | **`kind`** (`decomposition`\|`dependency`\|`grouping`), **`from`**, **`to`**, `remove?` — direction: `from→to` is parent→child / blocked→blocker / container→member (the container `from` must be release/phase/gate) |
| `haven_evolve` | **`op`** (`split`\|`merge`\|`supersede`), **`refs`**, `into?, with?, title?, rationale?, by?` |
| `haven_lineage` | **`ref`**, `direction?, depth?` |
| `haven_resolve_live` | **`ref`** — follow a stale (superseded/archived) ref to its live descendant(s); compact items |
| `haven_search` | **`query`**, `limit?` |
| `haven_graph` | `lineage?, all?` — the whole project graph (compact nodes + `{kind,from,to}` edges) in one read; live nodes only unless `all` |
| `haven_docs` | `project?` — live project-doc anchors and their artifacts |
| `haven_get_artifact` | **`ref`**, `role?, path?` |
| `haven_add_artifact` | **`ref`**, **`role`**, `kind?, content?, name?, replace?, path?, uri?, title?, from?, to?, by?` — `name` sets the destination filename (also for `path`); `replace?` overwrites a same-path artifact in place (default: collision is rejected) |
| `haven_rm_artifact` | **`ref`**, one of `role?` \| `name?` \| `id?` — remove an artifact (row + backing file); an ambiguous `role` is refused |
| `haven_mv_artifact` | **`ref`**, **`new_name`**, one of `role?` \| `name?` \| `id?` — rename the backing file (role/history preserved) |
| `haven_status` | `project?` |
| `haven_list_projects` | _(none)_ — discover backlogs |
| `haven_add_project` | **`key`**, **`title`**, `prefix?, description?` |
| `haven_archive` | **`ref`**, `rationale?, by?` |
| `haven_reopen` | **`ref`**, `rationale?, by?` |
| `haven_handoff` | **`ref`**, **`to`** (`human`\|`ai`), `from?, note?, status?, wait?, actor?` — atomic baton-pass |
| `haven_complete_item` | **`ref`**, `evidence?, artifact_role?, by?` — mark done, record evidence, report what it unblocked (as compact items) |

### Item response shapes (compact vs full)

To keep context lean, item reads come in two shapes, and internal sync fields
(`public_id`, `sync_state`, `revision`, `sort_key`) are **never** emitted over MCP:

- **Compact** — navigation only: `ref, title, type, status, committed, owner_kind?,
  priority?, wait_state?`. Used by `haven_list_items` (inside the `{total, count,
  offset, items[]}` envelope), `haven_next`, `haven_resolve_live`, and the
  `unblocked[]` list of `haven_complete_item`.
- **Full** — compact **+** prose (`body, done_looks_like, why`), `assignee?`,
  timestamps, non-empty `metadata`, and any requested `edges`/`artifacts`/`lineage`.
  Returned by `haven_get_item` and `haven_update_item`.

So a list/next tells you *what* exists; reach for `haven_get_item` when you need the
prose or relationships of a specific item. `haven_graph` nodes are compact too
(live-only unless `all`); only `haven_docs` returns full anchor nodes (with artifacts).

## CLI → MCP mapping

The collapses that catch people out:

| CLI | MCP |
|---|---|
| `item list` / `get` / `add` | `haven_list_items` / `haven_get_item` / `haven_add_item` |
| `item add --if-absent` | `haven_add_item {if_absent: true}` — both surfaces return `existing`/`similar` on the add response |
| `item update` **+** `commit` / `uncommit` / `assign` | **all one tool:** `haven_update_item` (fields `commit: true/false`, `assign`, plus the update fields) |
| `decompose` / `depend` / `group` | **one tool:** `haven_add_edge {kind: "decomposition"\|"dependency"\|"grouping", from, to}` |
| `evolve split`/`merge`/`supersede` | `haven_evolve {op, refs, …}` |
| `evolve graph` / `evolve resolve` | `haven_lineage` / `haven_resolve_live` |
| `item archive` / `reopen` | `haven_archive` / `haven_reopen` |
| `item handoff` | `haven_handoff` |
| `item complete` | `haven_complete_item` |
| `next` / `next --explain` | `haven_next` / `haven_next_explain` |
| `item rank` | `haven_rank` |
| `search`, `status`, `artifact get`/`add` | `haven_search`, `haven_status`, `haven_get_artifact`/`haven_add_artifact` |
| `artifact rm` / `mv` | `haven_rm_artifact` / `haven_mv_artifact` |
| `graph` | `haven_graph` |
| `xref` | `haven_xref` |
| `docs` | `haven_docs` |
| `project list` / `add` | `haven_list_projects` / `haven_add_project` |

So, over MCP: to commit, call `haven_update_item {ref, commit: true, priority}`;
to add a decomposition edge, `haven_add_edge {kind:"decomposition", from, to}`.

**Batch grooming.** The CLI `item update` / `commit` / `uncommit` / `archive`
verbs accept **multiple refs** in one call (`haven item archive HV-3 HV-7 HV-9`,
`haven item update --status ready HV-1 HV-2`), validate them all up front, and
return an array — so "mark these ready, archive those, commit these two" is one op
each. `update` applies the *same* change to every ref. Over MCP, apply one ref per
call (loop); there's no batch tool.

**Selecting a project over MCP.** A remote/headless client has no local
`current_project`. It calls `haven_list_projects` to see what's available, then
**passes `project: "<key>"` on every subsequent call** — selection is per-call
(carry the chosen key through the conversation), not a stored default. There's no
`haven_use_project` by design (it would clobber other sessions on a shared
gateway). `haven_add_project` starts a new backlog remotely.

## CLI-only operations

These have **no MCP tool** in v1 — a remote/headless client can't do them and
must rely on a local CLI or a pre-arranged state:

- **`project use` / `get`** — local conveniences. (`project list` / `add` *are*
  available over MCP via `haven_list_projects` / `haven_add_project` — see
  "Selecting a project over MCP" above; a remote client discovers backlogs and
  selects per-call, so it never needs `use`.)
- **`note`**, **`render`** — scratch lines and forced re-render (render happens
  automatically anyway).
- **`import`** — bulk capture from a JSON file (plan-loading is an
  at-the-terminal act). Over MCP, loop `haven_add_item` — with
  `if_absent: true` for dedupe — one call per item.
- **Lifecycle/admin** — `setup`, `init`, `doctor`, `config`, `link`, `skill`,
  `auth`, `sync`.

## Agent discovery and setup

`haven setup` writes an agent-agnostic `AGENTS.md` Haven stanza in the current
repo, then wires selected agent integrations. `--agent all` is the default.

Claude MCP lives in the Claude user config and its skill snapshot lives under
`~/.claude/skills/haven`. Codex MCP lives in `~/.codex/config.toml` or a trusted
project `.codex/config.toml` as:

```toml
[mcp_servers.haven]
command = "haven"
args = ["mcp"]
```

Codex/Open Agent Skills are discovered from `.agents/skills`,
`~/.agents/skills`, and `/etc/codex/skills`; Haven installs the user-scope
snapshot to `~/.agents/skills/haven` by default. Codex does not read
`~/.claude/skills`.

`haven link` creates a visible repo-local `Haven/` workspace with `backlog.md`
linked/copied from the canonical generated projection under `~/.haven`. Treat
`Haven/` as disposable; structure still changes only through Haven tools.

If a remote client genuinely needs to create projects or re-rank, that's a gap to
raise against the binary — don't fake it through other tools.

## The content channel

Content is files (under `~/.haven/<project>/items/<ref>/`), but the artifact
`content` field is a **virtual** read/write channel for clients without a
filesystem. It is never a DB column — it's computed on read and consumed on write,
with the file (or cloud Storage blob) staying canonical and the row holding only
the pointer (`path`, `content_hash`, `remote_path`).

- **Read:** `haven_get_artifact {ref, role}` → `{path, role, content}`; `content`
  is the file's bytes. If the file isn't on this machine but the row carries a
  synced cloud copy (`remote_path`), the read **lazy-downloads it from Storage**
  (hash-verified, then cached locally) — transparent when sync is configured;
  otherwise it errors `content_not_local` with the remote location.
- **Write:** `haven_add_artifact {ref, role, content, name}` — the **server**
  writes the bytes into `items/<ref>/<file>`, hashes them, and records the pointer.
  The content never lands in the DB. Filenames must be a single plain component
  (no `/`, `\`, or `..`).

A **local** agent should skip this channel and edit files directly — it's for
filesystem-less clients (phone, remote sandbox).
