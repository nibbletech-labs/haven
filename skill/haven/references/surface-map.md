# Haven surface map — CLI, MCP, and how they differ

The two front-ends drive the **same** store but are **not 1:1**. The CLI has many
friendly verbs (for a human typing); the MCP is a deliberately smaller, more
general set of 22 tools (for an agent). When a workflow runs over MCP, translate
using the mapping below.

## Contents
- [Enums (valid values)](#enums)
- [CLI command surface](#cli-command-surface)
- [MCP tool catalogue (22 tools)](#mcp-tool-catalogue)
- [CLI → MCP mapping](#cli--mcp-mapping)
- [CLI-only operations](#cli-only-operations)
- [The content channel](#the-content-channel)

---

## Enums

Used everywhere a `--type` / `--status` / `--role` etc. is accepted. Invalid
values error.

- **Node type** (`--type`): `task` (default), `code`, `research`, `data`,
  `design`, `admin`, `release`, `phase`, `gate`. The last three are *container*
  nodes (group targets).
- **Status** (`--status`): `discovery` (default) → `definition` → `ready` →
  `in_progress` → `done`, plus `blocked`, `superseded`, `archived`.
- **Owner** (`--to` / `--owner` / `--assign`): `human`, `ai`.
- **Wait state** (`--wait`): `on_human`, `on_dependency`, `on_external`, and
  `none` to clear.
- **Artifact role** (`--role`): `spec`, `research`, `design`, `decision`,
  `handoff`, `vision`, `source`, `delivery`, `scratch`.
- **Artifact kind** (`--kind`, usually inferred): `file`, `external`, `delivery`.

**Global CLI flags:** `--project/-p <key>` (defaults to the current project),
`--pretty` (tables instead of JSON).

## CLI command surface

```
# Setup & introspection
haven setup | init | status | doctor
haven config get <key> | set <key> <value>

# Projects
haven project add --key <k> --title <t> [--prefix HV] [--description …]
haven project list | get <key> | use <key>

# Items (nodes)
haven item add "<title>" [--type] [--body] [--done-looks-like "…"] [--why "…"]
                         [--status] [--priority N] [--commit] [--assign human|ai]
                         [--parent <ref>] [--depends-on <ref>] [--group <ref>]
haven item list [--status] [--type] [--owner] [--committed] [--icebox] [--group <ref>]
                [--wait on_human|on_dependency|on_external] [--stale <days>]
haven item get <ref> [--include edges,artifacts,lineage]
haven item update <ref>… [--title] [--body] [--done-looks-like "…"] [--why "…"]
                        [--status] [--priority N] [--type] [--wait]  # 1+ refs, same update each
haven item commit <ref>… [--priority N]      # one or more refs (grooming)
haven item uncommit <ref>…
haven item assign <ref> --to human|ai [--actor <name>]
haven item handoff <ref> --to human|ai [--from] [--note "…"] [--status] [--wait] [--actor]
haven item complete <ref> [--evidence "…"] [--role delivery] [--by]
haven item rank <ref> [--before <ref>] [--after <ref>]
haven item archive <ref>… [--rationale "…"]  # one or more refs (grooming)
haven item reopen  <ref> [--rationale "…"]

# Dispatch
haven next [--owner human|ai] [--limit N]
haven graph [--lineage]        # whole project: all nodes + edges in one read

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
haven artifact add <ref> --role <role> [--file <path> | --content "…" --name <f>]
                         [--kind] [--uri] [--title] [--excerpt] [--from] [--to] [--by]
haven artifact list <ref> [--role <role>]
haven artifact get  <ref> [--role <role>] [--path <relpath>]
haven note <ref> "<text>"
haven render

# Server / cloud
haven mcp
haven auth login [--token <jwt>] | logout | status
haven sync [status] [--watch]
```

## MCP tool catalogue

22 tools, each taking an optional `project` and naming items by `ref` or
`public_id`. Required args in **bold**.

| Tool | Args |
|---|---|
| `haven_list_items` | `status?, type?, owner?, committed?, icebox?, group?, wait?, stale?` |
| `haven_get_item` | **`ref`**, `include?: ["edges","artifacts","lineage"]` |
| `haven_next` | `owner?, limit?` |
| `haven_next_explain` | `owner?` — diagnose an empty queue (counts by reason + hint) |
| `haven_rank` | **`ref`**, `before?` \| `after?` (exactly one) — reorder within a priority band (fine ordering) |
| `haven_add_item` | **`title`**, `type?, body?, done_looks_like?, why?, status?, priority?, commit?, assign?, parent?, depends_on?, group?` |
| `haven_update_item` | **`ref`**, `title?, body?, done_looks_like?, why?, status?, priority?, type?, wait?, commit?, assign?, actor?` |
| `haven_add_edge` | **`kind`** (`decomposition`\|`dependency`\|`grouping`), **`from`**, **`to`**, `remove?` |
| `haven_evolve` | **`op`** (`split`\|`merge`\|`supersede`), **`refs`**, `into?, with?, title?, rationale?, by?` |
| `haven_lineage` | **`ref`**, `direction?, depth?` |
| `haven_resolve_live` | **`ref`** — follow a stale (superseded/archived) ref to its live descendant(s) |
| `haven_search` | **`query`**, `limit?` |
| `haven_graph` | `lineage?` — the whole project graph (all nodes + `{kind,from,to}` edges) in one read |
| `haven_get_artifact` | **`ref`**, `role?, path?` |
| `haven_add_artifact` | **`ref`**, **`role`**, `kind?, content?, name?, path?, uri?, title?, from?, to?, by?` |
| `haven_status` | `project?` |
| `haven_list_projects` | _(none)_ — discover backlogs |
| `haven_add_project` | **`key`**, **`title`**, `prefix?, description?` |
| `haven_archive` | **`ref`**, `rationale?, by?` |
| `haven_reopen` | **`ref`**, `rationale?, by?` |
| `haven_handoff` | **`ref`**, **`to`** (`human`\|`ai`), `from?, note?, status?, wait?, actor?` — atomic baton-pass |
| `haven_complete_item` | **`ref`**, `evidence?, artifact_role?, by?` — mark done, record evidence, report what it unblocked |

## CLI → MCP mapping

The collapses that catch people out:

| CLI | MCP |
|---|---|
| `item list` / `get` / `add` | `haven_list_items` / `haven_get_item` / `haven_add_item` |
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
| `graph` | `haven_graph` |
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
- **Lifecycle/admin** — `setup`, `init`, `doctor`, `config`, `auth`, `sync`.

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
