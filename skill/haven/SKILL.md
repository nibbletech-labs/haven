---
name: haven
description: >-
  Manage long-lived project work as a graph of items (the Haven work-graph) — both
  the building and the human tasks around it: reviews, approvals, decisions,
  real-world actions. Use it to capture ideas and tasks you'll revisit, decide
  what to work on next, plan and prioritise, groom rough items until they're ready
  to start, break big work into parts, evolve items as understanding changes, and
  hand work back and forth between people and AI agents — tracking who owns each
  item and what's waiting on whom. Reach for this whenever someone wants to track,
  plan, or order work they'll come back to, or pass a task to a person or agent —
  e.g. "add this to the backlog", "make a note to look at X later", "I keep
  meaning to…", "what should I work on next", "what's left for the launch", "tidy
  up the backlog", "break this down", "I've finished my part — who picks this up
  next?", "what's waiting on me?", "track that legal still needs to sign off", or
  "park this for now". Fire readily on capture, planning, or handoff intent even
  when they never say "backlog" or "Haven" by name. Not for genuinely ephemeral,
  one-off reminders that aren't part of a project being tracked.
---

# Haven — the work-graph

Haven is a durable store for a long-lived **work-graph**: every backlog item,
half-formed idea, task, research question, release — **and human task** (a review,
approval, decision, or real-world action someone has to take) — is a **node**.
It's for whole projects, not just the code: AI-owned and human-owned work live in
the same graph, and passing items between them is a first-class flow (see
*Ownership + handoffs* below). You manage it through the `haven` CLI (local agent,
with a terminal) or the `haven_*` MCP tools (remote/headless client). Both drive
the identical store.

Your job with this skill is **judgment**, not capability — the tools are
complete; what you add is knowing *when* to split / commit / leave floating, *how*
to run a grooming or planning pass, and what *not* to do. Two ideas carry most of
the weight; internalise these and read a reference file when you act.

## The one rule that matters most: structure vs content

**Structure** — the graph itself: nodes, edges, status, priority, lineage — is
mutated **only** through `haven …` / `haven_*` ops. Never by editing files or the
DB by hand, because the projection, lineage, and sync metadata must stay coherent.

**Content** — the actual work product: specs, research, notes, code — lives as
**files** under `~/.haven/<project>/items/<ref>/`. If you're a local agent, read
and edit those files **directly** (Read/Edit/Grep) — no round-trips through the
graph. The DB indexes the skeleton; it never carries the body. A node's `body`
field is a one-line summary, never the content itself.

(A filesystem-less client reads/writes content through the artifact `content`
channel instead — see `references/surface-map.md`.)

## Lazy structure: capture is cheap, structure is earned

Capturing an idea is a single bare node — floating, uncommitted, `discovery`.
**That floating state is the correct default, not a defect.** You wire edges, set
priority, and commit *only when the user actually engages* with the work.
Premature structure (inventing subtasks, committing speculative work, over-wiring
on capture) just churns. When in doubt, capture the node and stop.

## Mental model (the concepts, and no more)

- **Node = item.** One unified object for everything. The CLI/MCP call it `item`;
  the model is a "node". A node with no edges, no priority, uncommitted is valid
  and normal.
- **Two independent axes — don't conflate them:**
  - **Maturity** (`status`): `discovery → definition → ready → in_progress → done`
    (+ `blocked`, `superseded`, `archived`). *How well-defined* the work is.
  - **Commitment** (`committed` + `priority` 0–4 + `sort_key`): *whether and when*
    you'll do it.
  - They're orthogonal: a fully-spec'd-but-parked item is `ready` + uncommitted; a
    committed-but-fuzzy item is `discovery` + committed (and needs definition
    before it can dispatch).
- **`next` is the dispatch query**, and its contract is exact: it returns items
  that are **committed AND `ready` AND not waiting AND have no open dependency**,
  ordered by priority then `sort_key`. **Consequence:** an item the user "wants
  done next" will *not* appear in `next` unless it's committed *and* ready *and*
  unblocked. Creating the item is not enough — set those axes too.
- **Four edge layers** (all optional, all over the same nodes; never overload one
  for another's job): **decomposition** ("part of"), **dependency** ("blocked
  by"), **grouping** ("ships in this release/phase"), **lineage** ("what this
  became" — append-only history).
- **Acceptance & gates.** When an item is `ready`, set **`done_looks_like`** — the
  acceptance statement its output is verified against (the anchor for `ready→done`,
  and what a dispatcher like `orchestrate` checks). A **gate** is a review node:
  give it *dependency* edges to the items it reviews and it surfaces in `next` once
  they're all `done`, with the review's pass-criteria in its own `done_looks_like`.
  No special machinery — gates are just dependency + acceptance.
- **Never delete.** Splits, merges, supersessions, and "drop it" are all recorded;
  the old node persists (`superseded`/`archived`) with lineage to its descendants,
  and a reference to an old id resolves forward to the live one. "Drop it" =
  `archive --rationale`, which is reversible via `reopen`. There is no hard delete.
- **Ownership + handoffs.** Every node can be `human`- or `ai`-owned. A **handoff**
  is the baton-pass when ownership flips, recorded as a `handoff` artifact carrying
  `from`/`to`. This is the "I've done my part, over to you" flow made concrete.

## When to use this skill — and when not

Fire for anything the user will **revisit**: capturing ideas/tasks, planning,
prioritising, grooming, asking "what's next", breaking work down, parking or
reviving an idea, handing work between human and AI. Reach for it even when they
don't name "backlog" or "Haven".

Don't fire for a genuinely throwaway reminder the user won't return to, or for
editing the *content* of a file (that's plain Read/Edit — though if that file is a
Haven artifact, this skill still informs *how* you locate it).

## How to act: workflows

Each workflow has a trigger, concrete steps, judgment heuristics, and real
commands. Read **`references/workflows.md`** when you're about to do one — don't
work from memory:

1. **Capture** — "add this to the backlog" / "back-pocket this"
2. **Plan / prioritise** — turn floating items into a committed, ordered plan
3. **Groom** — move items `discovery → ready`
4. **Dispatch** — "what's next" (incl. diagnosing an empty `next`)
5. **Decompose vs group vs depend** — choosing the right edge layer
6. **Evolve** — split / merge / supersede, with good rationale
7. **Handoff** — ai ↔ human baton-pass
8. **Artifacts & content** — registering work product; the no-filesystem channel

The exact command/flag surface and the **CLI-vs-MCP differences** (they are *not*
1:1) live in **`references/surface-map.md`** — consult it for precise arguments
and for the MCP tool a remote client must use.

## Standing conventions and cautions

- **Pick the project once per session.** Every item lives in a project (a backlog
  / namespace; each mints its own `HV-1`, `HV-2`… refs). How you select depends on
  the surface:
  - **Local (CLI):** `haven project list`, then `haven project use <key>` sets a
    sticky current project for the session (or `haven project add` to create one).
  - **Remote (MCP):** `haven_list_projects` to discover, then **pass `project:
    "<key>"` on every call** — selection is per-call, carried through the
    conversation; there's no `use`. `haven_add_project` creates one. (See
    `references/surface-map.md` → "Selecting a project over MCP".)
  Check once; don't nag. One project per product/repo — don't scatter unrelated
  work into one.
- **Reference items by `ref`** (`HV-42`) in human-facing flows — stable and
  readable. `public_id` (UUID) only when crossing machines.
- **Let `backlog.md` regenerate itself.** It re-renders after every mutating op.
  Read it for an overview; **never hand-edit it** — edits get clobbered.
- **Always give a real `--rationale`** on evolve/archive/reopen. Lineage exists to
  reconstruct intent later; "too big" is weak, "spans two owners, splitting for
  independent dispatch" is useful.
- **Sync is manual in v1.** `haven sync` runs one push pass; there's no background
  loop yet. Don't sync after every mutation — run it (or tell the user to) when a
  batch of work should reach the cloud. `haven sync status` shows the queue.
- **Don't conflate the two axes.** "Make this ready" (maturity) ≠ "do this next"
  (commitment); `next` needs both.
- **Don't over-structure on capture, don't commit speculative work, don't overload
  edge layers, don't put content in `body`, don't hard-delete.** The fuller
  anti-pattern list is in `references/workflows.md`.
