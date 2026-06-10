---
name: haven
description: >-
  Track long-lived project work as a graph of items in Haven — both the building
  and the human tasks around it (reviews, approvals, decisions, real-world
  actions). Use to capture work to revisit, decide what's next, plan and
  prioritise, groom rough items until ready, break work into parts, evolve items
  as understanding shifts, and hand work between people and AI (tracking who owns
  each item and what's waiting on whom). Fire on capture, planning, or handoff
  intent even when the user never says "Haven" or "backlog" — e.g. "add this to
  the backlog", "look at X later", "what should I work on next", "what's left for
  the launch", "break this down", "I've finished my part — who's next?", "what's
  waiting on me?", "track that legal needs to sign off", "park this". Not for
  ephemeral one-off reminders outside a tracked project.
---

# Haven — the work-graph

Haven is a durable store for a long-lived **work-graph**: every backlog item,
half-formed idea, task, research question, release — **and human task** (a review,
approval, decision, or real-world action) — is a **node**. AI-owned and
human-owned work share one graph, and passing items between them is first-class.

You drive it through the **`haven` CLI** (local agent with a terminal) or the
**`haven_*` MCP tools** (remote/headless client) — both over the identical store.
Your job is **judgment**: knowing *when* to split / commit / leave floating, and
what *not* to do. The tools are complete.

## Operating loop

Run every Haven interaction through these five steps:

1. **Establish the project.** Every item lives in one (each mints its own `HV-1`,
   `HV-2`… refs). Settle it once per session — see *Selecting a project* below.
2. **Classify the intent:** capture · groom · plan · dispatch · execute · handoff ·
   complete · evolve · archive. (One reply can span a few; do the smallest set.)
3. **Use the smallest safe operation** — and prefer the *atomic* tool when one
   exists (`handoff`, `complete`) over hand-assembling the steps.
4. **Confirm the result** from the returned JSON: the `ref`, `status`, `committed`,
   `owner`, `wait_state`. Don't assume — read it back.
5. **Never touch structure or `backlog.md` by hand.** Mutate the graph *only*
   through tools; edit *content* as files. (The one rule, below.)

## Critical gotchas

The mistakes that actually bite — internalise these:

- **`next` is exact:** it returns only items that are **committed AND `ready` AND
  not waiting AND have no open dependency.** Creating an item — or just marking it
  `ready` — will *not* put it in `next`. Set both axes.
- **Capture ≠ commit.** "Add to the backlog" creates a floating, uncommitted
  `discovery` node. Don't commit, prioritise, or wire it unless the user engages.
- **`ready` requires `done_looks_like`.** An item with no acceptance can't be
  verified or cleanly dispatched. Set it when you mark something `ready`.
- **Empty `next` → diagnose, don't invent.** Call `next --explain` /
  `haven_next_explain`; it tells you *why* (uncommitted / not-ready / blocked /
  waiting / owner-mismatch). Never fabricate work.
- **Multi-item delivery needs a container.** If the user asks to deliver several
  items together ("this release", "ship these", "do the first three", "for
  launch"), create or reuse a `release`/`phase` node and group the members before
  dispatch. Then check for shared architecture/UX/API/data/test strategy. If
  shared context exists, recommend a Context Pack when that workflow is available;
  otherwise pause to clarify the integrated architecture and attach the result as
  a `spec`/`decision` artifact on the container.
- **Handoff and complete are atomic tools, not recipes.** Use `item handoff` /
  `haven_handoff` and `item complete` / `haven_complete_item` — don't hand-assemble
  assign + update + add_artifact (you'll do it inconsistently).
- **Archive, never delete.** There is no hard delete. "Drop it" = `archive
  --rationale`; reversible via `reopen`.
- **The two axes are orthogonal.** Maturity (`status`) ≠ commitment (`committed` +
  `priority`). "Make this ready" and "do this next" are different operations.
- **On the CLI, commit/uncommit are their own verbs.** `haven item commit <ref>
  [--priority N]` — `item update` does *not* take `--commit` (`--commit` exists
  only on `item add`). Over MCP it's the opposite: one tool, `haven_update_item
  {commit: true, …}`. Don't carry one surface's shape to the other.
- **Over MCP, pass `project` on every call** — there is no sticky session.

## The one rule that matters most: structure vs content

**Structure** — nodes, edges, status, priority, lineage — is mutated **only**
through `haven …` / `haven_*` ops, never by editing files or the DB by hand
(the projection, lineage, and sync metadata must stay coherent).

**Content** — the work product: specs, research, notes, code — lives as **files**
under `~/.haven/<project>/items/<ref>/`. A local agent reads/edits those files
**directly** (Read/Edit/Grep) — no round-trips through the graph. The `body` field
is a one-line summary, **never** the content. (A filesystem-less client uses the
artifact `content` channel instead — see `references/surface-map.md`.)

**Project-level documents belong in the store too.** Vision, architecture, and
decision docs are artifacts (`--role vision|design|decision`) on a long-lived
**anchor node** (`--type phase`, e.g. "Project X — vision & architecture") — not
loose repo markdown. They then sync, lazy-download, and back every item's `why`
trace like all other content. When asked to tidy or restructure a project's
docs, **migrating the durable ones into the graph is usually the move** — don't
just reshuffle files.

## Lazy structure: capture is cheap, structure is earned

Capturing an idea is a single bare node — floating, uncommitted, `discovery`.
**That floating state is the correct default, not a defect.** Wire edges, set
priority, and commit *only when the user actually engages*. When in doubt, capture
the node and stop — premature structure just churns.

## Mental model (the concepts, and no more)

- **Node = item.** One unified object for everything; a node with no edges, no
  priority, uncommitted is valid and normal.
- **Two independent axes:** **maturity** (`status`: `discovery → definition →
  ready → in_progress → done`, + `blocked`/`superseded`/`archived`) and
  **commitment** (`committed` + `priority` 0–4 + `sort_key`). A spec'd-but-parked
  item is `ready` + uncommitted; a committed-but-fuzzy one is `discovery` +
  committed and needs definition before it can dispatch.
- **Four edge layers** (don't overload one for another): **decomposition** ("part
  of"), **dependency** ("blocked by"), **grouping** ("ships in this release"),
  **lineage** ("what this became" — append-only).
- **Acceptance & gates.** A `ready` item carries **`done_looks_like`** (the
  verify anchor). A **gate** is a review node: give it *dependency* edges to the
  items it reviews and it surfaces in `next` once they're all `done`, with the
  pass-criteria in its own `done_looks_like`. No special machinery.
- **Completion reports what it unblocks.** `complete` returns the items/gates whose
  last dependency just closed — that's your next dispatch set.
- **Never delete.** Splits/merges/supersessions/archives are all recorded; an old
  ref resolves forward to the live one (`evolve resolve` / `haven_resolve_live`).

## Selecting a project

- **Local (CLI):** `haven project list`, then `haven project use <key>` sets a
  sticky current project (or `haven project add` to create one).
- **Remote (MCP):** `haven_list_projects` to discover, then **pass `project:
  "<key>"` on every call** — selection is per-call, carried through the
  conversation; there is no `haven_use_project`. `haven_add_project` creates one.
  If a call errors with `project_required`, the message lists the available keys.

Settle this once per session; don't nag. One project per product/repo.

## How to act: workflows

Read **`references/workflows.md`** when you're about to run one — don't work from
memory. Each has a trigger, steps, judgment heuristics, and real commands:

1. **Capture** — "add this to the backlog"
2. **Plan / prioritise** — floating items → a committed, ordered plan
3. **Groom** — move items `discovery → ready` (set acceptance)
4. **Dispatch** — "what's next" (and `next --explain` when it's empty)
5. **Multi-item delivery** — create/reuse a release/phase container and assess
   shared execution context before dispatch
6. **Decompose vs group vs depend** — choosing the edge layer
7. **Evolve** — split / merge / supersede, with real rationale
8. **Handoff** — `item handoff`: the atomic ai↔human baton-pass
9. **Complete** — `item complete`: evidence + done + what it unblocked
10. **Artifacts & content** — registering work product; the no-filesystem channel
11. **Gates & reviews** — review checkpoints as dependency + acceptance

Precise arguments and the **CLI-vs-MCP differences** (they are *not* 1:1) are in
**`references/surface-map.md`**.

## MCP quick reference (concrete payloads)

Over MCP, pass `project` on every call. The common ops:

```jsonc
// Capture — floating, uncommitted, discovery (the default).
haven_add_item   {"project":"haven","title":"Cache the JWKS lookup"}
// Make it dispatchable: commit + ready + acceptance, in one update.
haven_update_item{"project":"haven","ref":"HV-1","status":"ready","commit":true,
                  "priority":1,"done_looks_like":"p95 verify < 5ms"}
// Dispatch, and diagnose if empty.
haven_next         {"project":"haven","owner":"ai"}
haven_next_explain {"project":"haven","owner":"ai"}   // when next is empty
// Atomic baton-pass and completion.
haven_handoff      {"project":"haven","ref":"HV-1","to":"human",
                    "note":"Implemented; review the rate-limit defaults."}
haven_complete_item{"project":"haven","ref":"HV-1","evidence":"cargo test: ok"}
// Edges, evolve, stale refs.
haven_add_edge     {"project":"haven","kind":"dependency","from":"HV-2","to":"HV-1"}
haven_resolve_live {"project":"haven","ref":"HV-9"}   // old ref → live descendant
// Whole graph in one read — to render it, or reason over all dependencies at once.
haven_graph        {"project":"haven"}                // {nodes[], edges[{kind,from,to}]}
```

**Reasoning over the whole backlog** (reorganising, fixing dependencies, rendering
a graph view) — pull it all in one call with `haven graph` / `haven_graph` rather
than N per-node fetches. It returns every node plus a flat `{kind, from, to}` edge
list (the same shape `add_edge` takes, so your fixes round-trip).

## Standing cautions

- **Reference items by `ref`** (`HV-42`) in human-facing flows; `public_id` only
  across machines.
- **Let `backlog.md` regenerate** — it re-renders after every mutation; never
  hand-edit it.
- **Always give a real `--rationale`** on evolve/archive/reopen — lineage exists to
  reconstruct intent ("spans two owners, splitting for independent dispatch", not
  "too big").
- **Sync is manual in v1.** Run `haven sync` once after a batch, not per mutation;
  `haven sync status` shows the queue.
