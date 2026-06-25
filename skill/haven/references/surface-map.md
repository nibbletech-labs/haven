# Haven surface map — CLI, MCP, and how they differ

The two front-ends drive the **same** store but are **not 1:1**. The CLI has many
friendly verbs (for a human typing); the MCP is a deliberately smaller, more
general set of 30 tools (for an agent). When a workflow runs over MCP, translate
using the mapping below — and see the **[verb-divergence map](#verb-divergence-top-level-vs-item-nested-vs-mcp-only)**
for the cases where the same verb lives at a different level on each surface.

## Contents
- [Enums (valid values)](#enums)
- [CLI command surface](#cli-command-surface)
- [MCP tool catalogue (30 tools)](#mcp-tool-catalogue)
- [CLI → MCP mapping](#cli--mcp-mapping)
- [Verb-divergence map (top-level vs item-nested vs MCP-only)](#verb-divergence-top-level-vs-item-nested-vs-mcp-only)
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
haven setup [--agent all|claude|codex] [--no-skill] | init | status [<key>] | doctor
                                                    # `status <key>` resolves like `-p <key>`
haven config get <key> | set <key> <value>
haven link [--name Haven]  # visible repo-local workspace/projection; canonical state stays in ~/.haven

# Projects
haven project add --key <k> --title <t> [--prefix HV] [--description …]
haven project list [--include-archived] | get <key> | use <key>
haven project archive <key> [--rationale "…" (alias --reason)] [--by <name>]  # reversible retire; namespace stays reserved
haven project reopen  <key> [--by <name>]                                     # total restore (refs continue from preserved counter)

# Items (nodes)
haven item add "<title>" [--type] [--body] [--done-looks-like "…"] [--why "…"]
                         [--status] [--priority N] [--commit] [--assign human|ai] [--due-at YYYY-MM-DD]
                         [--parent <ref>] [--depends-on <ref>] [--group <ref>]
                         [--if-absent]   # normalized-title dedupe: return the existing item
haven import <file.json> [--if-absent]  # bulk add: one validated, all-or-nothing transaction;
                                        # items take the add fields + temp `id` and ref-or-temp-id
                                        # parent / depends_on (array) / group edge fields
haven item list [--status] [--type] [--owner] [--committed] [--icebox] [--group <ref>]
                [--wait on_human|on_dependency|on_external] [--stale <days>] [--all]
                # live-only by default (hides archived/superseded); --all includes them,
                # and an explicit --status archived|superseded still reaches them
haven item get <ref> [--include edges,artifacts,lineage]
haven item update <ref>… [--title] [--body] [--done-looks-like "…"] [--why "…"]
                        [--status] [--priority N] [--type] [--wait]
                        [--due-at YYYY-MM-DD|none]   # 1+ refs, same update each; `none` clears due-at
haven item commit <ref>… [--priority N]      # one or more refs (grooming)
haven item uncommit <ref>…
haven item claim <ref> [--as ai|human] [--actor <name>]   # atomic: owner + in_progress in one op
haven item assign <ref> --to human|ai [--actor <name>]
haven item handoff <ref> --to human|ai [--from] [--note "…"] [--status] [--wait] [--actor]
haven item complete <ref> [--evidence "…"] [--role delivery] [--by]
haven item rank <ref> [--before <ref>] [--after <ref>]
haven item archive <ref>… [--rationale "…"]  # one or more refs (grooming)
haven item reopen  <ref> [--rationale "…"]

# Dispatch
haven next [--owner human|ai] [--limit N]   # --owner = ASSIGNMENT filter (owner_kind = owner); unassigned (NULL) excluded
haven graph [--lineage] [--all]  # whole project: all nodes + edges in one read.
                                 # live-only by default (drops archived/superseded
                                 # + dangling edges), matching haven_graph; --all includes them
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

# Server / preview cloud
haven mcp
# Cloud Sync is hidden/preview-gated in public installs:
# HAVEN_CLOUD_SYNC_PREVIEW=1 haven auth login [--token <jwt>] | logout | status
# HAVEN_CLOUD_SYNC_PREVIEW=1 haven sync [status] [--watch]
```

## MCP tool catalogue

30 tools, each taking an optional `project` and naming items by `ref` or
`public_id`. Required args in **bold**.

| Tool | Args |
|---|---|
| `haven_list_items` | `status?, type?, owner?, committed?, icebox?, group?, wait?, stale?, limit?, offset?` — returns a compact, paginated envelope `{total, count, offset, items[]}` (default `limit` 100) |
| `haven_inbox` | `owner?, limit?, offset?` — untriaged floaters (uncommitted, live, no `done_looks_like` yet); same compact paginated envelope as `haven_list_items` |
| `haven_xref` | **`ref`** — cross-store links on the node's artifacts: a sorted `{node, outbound[], inbound[]}` report (outbound xrefs + inbound backlinks); read-only |
| `haven_get_item` | **`ref`**, `include?: ["edges","artifacts","lineage"]` — the full item (prose + includes); the detail door. A superseded/archived ref still returns the item but rides a `stale_ref` `{ref, resolved_to:[…]}` hint (the work moved — follow `resolved_to`) |
| `haven_next` | `owner?, limit?` — compact items; `owner` filters ASSIGNMENT (`owner_kind = owner`), unassigned (NULL) excluded |
| `haven_next_explain` | `owner?` — diagnose an empty queue (counts by reason + hint) |
| `haven_rank` | **`ref`**, `before?` \| `after?` (exactly one) — reorder within a priority band (fine ordering) |
| `haven_add_item` | **`title`**, `type?, body?, done_looks_like?, why?, status?, priority?, commit?, assign?, due_at?, parent?, depends_on?, group?, if_absent?` — with `if_absent` a normalized-title match returns the existing item (`existing: true`); responses may carry advisory `similar` |
| `haven_import` | **`items`** (array of `{title*, id?, type?, body?, done_looks_like?, why?, status?, priority?, commit?, assign?, parent?, depends_on?, group?}`), `if_absent?` — the `haven import` envelope inline: bulk-add an N-node sub-graph in ONE atomic call (temp-id / forward-ref resolution, all-or-nothing rollback, `if_absent` dedupe). Inherits the born-state guard (no engaged-born / committed item; `ready` needs `done_looks_like`). Returns one outcome per item (`id` echoed, the item, `existing`) |
| `haven_update_item` | **`ref`**, `title?, body?, done_looks_like?, why?, status?, priority?, type?, wait?, due_at?, commit?, assign?, group?, actor?` (`due_at` accepts `"none"` to clear; `group` adds the item to a release/phase/gate container, mirroring `haven_add_item`). A dead (superseded/archived) `ref` still updates but rides a `stale_ref` hint |
| `haven_add_edge` | **`kind`** (`decomposition`\|`dependency`\|`grouping`), **`from`**, **`to`**, `remove?` — direction: `from→to` is parent→child / blocked→blocker / container→member (the container `from` must be release/phase/gate). A dead endpoint still forms the edge but rides a `stale_ref` hint (re-point it) |
| `haven_evolve` | **`op`** (`split`\|`merge`\|`supersede`), **`refs`**, `into?, with?, title?, rationale?, by?` |
| `haven_lineage` | **`ref`**, `direction?, depth?` |
| `haven_resolve_live` | **`ref`** — _deprecated (kept one release):_ follow a stale (superseded/archived) ref to its live descendant(s); compact items. The read path now runs this automatically — `haven_get_item`/`haven_update_item`/`haven_add_edge` ride a `stale_ref` hint — so you rarely call this directly |
| `haven_search` | **`query`**, `limit?` |
| `haven_graph` | `lineage?, all?` — the whole project graph (compact nodes + `{kind,from,to}` edges) in one read; live nodes only unless `all` |
| `haven_docs` | `project?` — live project-doc anchors and their artifacts |
| `haven_get_artifact` | **`ref`**, `role?, path?` |
| `haven_add_artifact` | **`ref`**, **`role`**, `kind?, content?, name?, replace?, path?, uri?, title?, from?, to?, by?` — `name` sets the destination filename (also for `path`); `replace?` overwrites a same-path artifact in place (default: collision is rejected) |
| `haven_rm_artifact` | **`ref`**, one of `role?` \| `name?` \| `id?` — remove an artifact (row + backing file); an ambiguous `role` is refused |
| `haven_mv_artifact` | **`ref`**, **`new_name`**, one of `role?` \| `name?` \| `id?` — rename the backing file (role/history preserved) |
| `haven_status` | `project?` |
| `haven_prime` | `project?` — one-shot session-context block (project state, committed queue with next-eligible flagged, in-progress/waiting incl. owner, core conventions, untriaged inbox) as a `prime` text block; read at session start instead of separate `status`/`next`/`list`/`inbox` calls |
| `haven_list_projects` | `include_archived?` — discover backlogs (hides archived unless `include_archived:true`; a deleted project is never listed) |
| `haven_add_project` | **`key`**, **`title`**, `prefix?, description?` |
| `haven_archive_project` | **`key`**, `rationale?, by?` — soft-archive a project: retire it, namespace stays reserved (key/prefix/counter untouched, refs never reused). Reversible. The project-level analogue of `haven_archive`; there is no hard-delete tool |
| `haven_reopen_project` | **`key`**, `by?` — reopen an archived project (total restore; refs continue from the preserved counter) |
| `haven_archive` | **`ref`**, `rationale?, by?` |
| `haven_reopen` | **`ref`**, `rationale?, by?` |
| `haven_claim` | **`ref`**, `owner?` (`human`\|`ai`, default `ai`), `actor?` — atomically set owner + `in_progress` (compare-and-set); errors with a conflict if already claimed/in_progress. Frames `in_progress` as a soft claim |
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
| `import <file.json>` | `haven_import {items: [...]}` — the file's JSON array passed inline; same atomic batch (temp-id/forward-ref resolution, `if_absent` dedupe, born-state guard) |
| `item update` **+** `commit` / `uncommit` / `assign` | **all one tool:** `haven_update_item` (fields `commit: true/false`, `assign`, plus the update fields) |
| `decompose` / `depend` / `group` | **one tool:** `haven_add_edge {kind: "decomposition"\|"dependency"\|"grouping", from, to}` |
| `evolve split`/`merge`/`supersede` | `haven_evolve {op, refs, …}` |
| `evolve graph` / `evolve resolve` | `haven_lineage` / `haven_resolve_live` |
| `item archive` / `reopen` | `haven_archive` / `haven_reopen` |
| `item claim` | `haven_claim` |
| `item handoff` | `haven_handoff` |
| `item complete` | `haven_complete_item` |
| `next` / `next --explain` | `haven_next` / `haven_next_explain` |
| `prime` | `haven_prime` |
| `item rank` | `haven_rank` |
| `search`, `status`, `artifact get`/`add` | `haven_search`, `haven_status`, `haven_get_artifact`/`haven_add_artifact` |
| `artifact rm` / `mv` | `haven_rm_artifact` / `haven_mv_artifact` |
| `graph` | `haven_graph` |
| `xref` | `haven_xref` |
| `docs` | `haven_docs` |
| `project list` / `add` | `haven_list_projects` / `haven_add_project` |
| `project list --include-archived` | `haven_list_projects {include_archived: true}` |
| `project archive` / `reopen` | `haven_archive_project` / `haven_reopen_project` (required `key`) |

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
gateway). `haven_add_project` starts a new backlog remotely, and
`haven_archive_project` / `haven_reopen_project` retire and restore one (a phone /
web client needs to retire a finished backlog — there is no hard-delete tool,
archive is the reversible, namespace-reserving drop).

## Verb-divergence (top-level vs item-nested vs MCP-only)

The same *word* can live at a different level on each surface, so a verb guessed
from one surface fails on the other. The CLI nests item lifecycle under `item …`;
the MCP keeps every tool **flat** (`haven_get_item`, `haven_archive`, …). The CLI
intercepts the common wrong guesses and answers with an error naming the exact
corrective command (HV-158) — you don't have to memorise the table, but here it is.

| You might type | What's correct | Note |
|---|---|---|
| `haven get <ref>` | `haven item get <ref>` | flat MCP name (`haven_get_item`) typed at the CLI top level → tip to the nested verb |
| `haven add "<title>"` | `haven item add "<title>"` | same: `haven_add_item` is flat; the CLI nests it |
| `haven archive <ref>` | `haven item archive <ref>` | `haven_archive` is flat; CLI nests under `item` |
| `haven handoff <ref> --to …` | `haven item handoff <ref> --to …` | `haven_handoff` is flat; CLI nests under `item`. Use handoff (not assign+update) for an **ai↔human baton-pass** — it flips owner, records a note, and sets wait/status atomically |
| `haven list-items` | `haven item list` | the MCP tool is `haven_list_items`; the CLI verb is `item list` |
| `haven item show <ref>` | `haven item get <ref>` | `show` is a built-in **alias** of `get` — it just works |
| `haven item update --commit <ref>` | `haven item commit <ref>` | commitment is its **own verb**, not an update flag; `--uncommit` → `haven item uncommit <ref>` |
| `haven status <key>` | `haven status -p <key>` | a bare positional key on `status` resolves like `-p <key>` — both forms work |

**Three buckets:**

- **Top-level CLI verbs** (no `item` prefix): `next`, `inbox`, `graph`, `docs`,
  `search`, `xref`, `import`, `decompose`/`depend`/`group`, `evolve`, `note`,
  `render`, plus lifecycle/admin (`setup`, `status`, …). These are *not* under
  `item`.
- **Item-nested CLI verbs** (`item <verb>`): `add`, `list`, `get` (alias `show`),
  `update`, `commit`/`uncommit`, `assign`, `handoff`, `complete`, `rank`,
  `archive`, `reopen`. The MCP flattens these (`haven_add_item`,
  `haven_get_item`, `haven_archive`, `haven_handoff`, …).
- **MCP-only / CLI-only**: see [CLI → MCP mapping](#cli--mcp-mapping) (collapses
  like `item update`+`commit`+`assign` → one `haven_update_item`) and
  [CLI-only operations](#cli-only-operations) below.

## CLI-only operations

These have **no MCP tool** in v1 — a remote/headless client can't do them and
must rely on a local CLI or a pre-arranged state:

- **`project use` / `get`** — local conveniences. (`project list` / `add` /
  `archive` / `reopen` *are* available over MCP via `haven_list_projects` /
  `haven_add_project` / `haven_archive_project` / `haven_reopen_project` — see
  "Selecting a project over MCP" above; a remote client discovers backlogs and
  selects per-call, so it never needs `use`.)
- **`note`**, **`render`** — scratch lines and forced re-render (render happens
  automatically anyway).
- **Lifecycle/admin** — `setup`, `init`, `doctor`, `config`, `link`, `skill`;
  `auth` and `sync` are preview-gated behind `HAVEN_CLOUD_SYNC_PREVIEW=1`.

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
  synced cloud copy (`remote_path`), cloud hydration is available only with
  `HAVEN_CLOUD_SYNC_PREVIEW=1`; otherwise it errors `content_not_local` with the
  remote location.
- **Write:** `haven_add_artifact {ref, role, content, name}` — the **server**
  writes the bytes into `items/<ref>/<file>`, hashes them, and records the pointer.
  The content never lands in the DB. Filenames must be a single plain component
  (no `/`, `\`, or `..`).

A **local** agent should skip this channel and edit files directly — it's for
filesystem-less clients (phone, remote sandbox).
