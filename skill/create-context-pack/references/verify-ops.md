# Verify-ops — the exact CLI and MCP call for each flow step

The canonical argument reference is the `haven` skill's `references/surface-map.md`
(CLI⇄MCP differences). This file is `create-context-pack`'s per-step cheat-sheet. `<P>` =
the project key.

> **Two surfaces, one contract.** Locally use the `haven` CLI; remotely use the
> `haven_*` MCP tools. **Over MCP there is no sticky session — pass `project` on every
> call.** **No batch over MCP:** `haven_add_edge` / `haven_update_item` /
> `haven_add_artifact` are one entity per call; loop. The CLI can pack (`haven group <g>
> --add a --add b`); the skill never depends on that.

## 0. Reorient — the whole graph in one read

- CLI: `haven graph -p <P>`
- MCP: `haven_graph {"project":"<P>"}`

Returns live nodes (compact: ref, title, type, status, committed, owner, priority, wait,
**and `done_looks_like`**) + a flat edge list `{kind, from, to}`. Resolve the project
first if unknown: CLI `haven project list`; MCP `haven_list_projects`.

## 1. Resolve the target — find or create the grouping container

The input is a **single item or an explicit set of refs**. Determine their common
container from step-0's grouping edges (a `release`/`phase` node they all belong to), then
pick the pack's home by comparing your target set to that container's **full** membership:

- **Target set == the container's full membership:** use the existing container.
- **Target set ⊊ the container's members** (a strict subset — building part of a broader
  phase), **or no common container exists:** create a dedicated **build-batch** container
  and group your members in. The pack lands here, **never on the broad phase** — a phase
  holds one `context-pack.md`, so a subset's pack on the broad phase is mis-scoped and a
  later batch clobbers it. Members keep any existing phase membership — grouping is
  **additive / many-to-many**, so you *add* the batch edge and **never remove** the
  member's original group (a leaf can sit in its theme phase *and* a build batch at once).
  - CLI: `haven item add "<batch title> — dev batch" --type phase -p <P>` → returns
    `<CONTAINER>`; then `haven group <CONTAINER> --add <ref> --add <ref> … -p <P>`.
  - MCP: `haven_add_item {"project":"<P>","title":"…","type":"phase"}` → `<CONTAINER>`;
    then **one call per member** `haven_add_edge {"project":"<P>","kind":"grouping",
    "from":"<CONTAINER>","to":"<ref>"}`.

> **Grouping direction:** `from` / the group arg is the **container**; `to` /
> `--add <ref>` is the **member** (`group_id`, `member_id`). Container type **must** be
> `release`/`phase`/`gate` or the store rejects it; this skill uses `phase` (or an
> existing `release`). Grouping is **additive and idempotent** — a member keeps its
> decomposition parent and any other groups; re-adding is a no-op.

## 2. Dependency-closure check (pure reasoning on step-0 edges, then context)

For each member, read its `depends_on` (from step 0 / step 4). For a dependency `d`
**outside** the target set:
- `d` is `done` → record its output as read-only context in pack section 3 (read it via
  step 4 if you need its acceptance/artifacts).
- `d` is not done → list it in pack section 3 as a boundary/blocker for the human. **Do
  not** add `d` to the group yourself.

No mutation here — it shapes pack sections 3 and the dependency edges you wire in step 7.

## 3. Precondition check

A member is prep-ready if it's a sealed leaf (has `done_looks_like`; `ready` or close)
and not a container. If any targeted member is coarse/un-planned (no acceptance, or still
needs decomposing): **STOP** and tell the user to run `orchestrate-plan` on it first.
Don't decompose here.

**Clash check — single active pack per leaf.** `haven_get_item` returns a derived
`context_pack` pointer (and `context_pack_clash`) on each leaf. Before claiming a member
into this build batch, inspect it: if it already carries a `context_pack` pointing at a
**different** container — or a `context_pack_clash` — it's already governed by another
pack. **STOP and surface it**; never auto-pick or merge. Resolve by pulling the member out
of the other batch, or re-prepping the existing container. (Re-prepping the **same**
container is fine — it overwrites its own pack, and dedup-by-container means that isn't a
clash.)
- CLI: `haven item get <ref> --include edges -p <P>` → inspect `context_pack`
- MCP: `haven_get_item {"project":"<P>","ref":"<ref>","include":["edges"]}` → inspect `context_pack`

## 4. Read each member's detail

- CLI: `haven item get <ref> --include edges,artifacts -p <P>`
- MCP: `haven_get_item {"project":"<P>","ref":"<ref>","include":["edges","artifacts"]}`

Read `body`/`why`/`done_looks_like` + edges. From a member, `edges.groups` shows the
container; `edges.depends_on` drives step 2.

## 5. Shared-context assessment

Apply the `haven` workflow-5 heuristic (shared architecture / contracts / data model /
sequencing / risky parallelism?). If **none** apply → simple batch, no pack:

- CLI: `haven artifact add <CONTAINER> --role decision --name batch-decision.md --content "Simple batch — no shared architecture; dispatch members individually." -p <P>`
- MCP: `haven_add_artifact {"project":"<P>","ref":"<CONTAINER>","role":"decision","name":"batch-decision.md","content":"…"}`

…then stop. Otherwise continue to 6.

## 6. Synthesise the pack

Build the `context-pack.md` body per `references/pack-template.md` — section 0 verbatim,
sections 1–4 synthesised, every code-level claim tagged `[VERIFY]`. This is reasoning,
not an op.

## 7. Write to the graph (all additive)

**a. Sharpen each member's acceptance** (one call per member):
- CLI: `haven item update <ref> --done-looks-like "<concrete, testable>" -p <P>`
- MCP: `haven_update_item {"project":"<P>","ref":"<ref>","done_looks_like":"<…>"}`

**b. Wire real ordering** found in step 2 (one edge per call; `from`=blocked/consumer,
`to`=blocker/producer; the store rejects cycles — don't pre-check):
- CLI: `haven depend <consumer> --on <producer> -p <P>`
- MCP: `haven_add_edge {"project":"<P>","kind":"dependency","from":"<consumer>","to":"<producer>"}`

**c. Write the pack onto the container** (the content channel writes the bytes):
- CLI: `haven artifact add <CONTAINER> --role spec --name context-pack.md --content "<pack>" -p <P>`
  (or `--file <path>` if you wrote it to disk under the item dir)
- MCP: `haven_add_artifact {"project":"<P>","ref":"<CONTAINER>","role":"spec","name":"context-pack.md","content":"<pack>"}`

**d. Point the container's `why` at the pack:**
- CLI: `haven item update <CONTAINER> --why "Context pack: see spec artifact context-pack.md" -p <P>`
- MCP: `haven_update_item {"project":"<P>","ref":"<CONTAINER>","why":"Context pack: see spec artifact context-pack.md"}`

> **No status flips.** Do **not** set any member `in_progress` and do **not** complete
> anything — execution is plan mode's, not this skill's.

## 8. Hand off + read-back

Report the container ref. The next session / plan mode takes the pack as input:
- CLI: `haven artifact get <CONTAINER> --role spec --path context-pack.md -p <P>`
- MCP: `haven_get_artifact {"project":"<P>","ref":"<CONTAINER>","role":"spec"}` → `{path, role, content}`

A leaf now **advertises** its pack: `haven_get_item {ref}` returns a derived `context_pack`
`{container, artifact}` (and `haven_graph` carries it per leaf), so a dispatcher reads one
pointer instead of walking `edges.groups` and guessing which container holds the pack.

> **Consumer rule — load the pack before building.** Any dev / plan-mode session that pulls
> a leaf MUST read its `context_pack` and load that container's `spec` `context-pack.md`
> (`haven_get_artifact {ref: container, role:"spec"}`) **before building** — never build a
> member naked. If the leaf carries `context_pack_clash` instead of `context_pack`, do
> **not** build: route back to create-context-pack to resolve the clash first.
