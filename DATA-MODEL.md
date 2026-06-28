# What Haven stores — items, edges, and attached docs

Haven keeps a **work-graph**: *items* (the nodes) connected by typed *edges*, with
*documents* attached to them as files. This page describes what is actually in the
store — so you know what your agent is reading and writing when it drives Haven.

For *how* you operate on all this (by talking to your AI), see
[`USING-HAVEN.md`](USING-HAVEN.md). For the exhaustive field/flag list, see the
bundled skill's
[`references/surface-map.md`](skill/haven/references/surface-map.md).

## Items (the nodes)

An **item** is one unit of work. It has a stable ref (`MW-1`, `MW-2`, …) and a
small set of *axes* — independent dimensions you set as the work firms up:

| Axis | What it is |
|---|---|
| **title** | the one-line name (**the only required field**) |
| **type** | what kind of node this is: leaves `task` (default) · `code` · `research` · `data` · `design` · `admin`; containers `phase` · `release` · `gate` · `anchor` — see [Types](#types) |
| **status** | how mature / finished it is: `discovery` → `definition` → `ready` → `in_progress` → `done`, plus `blocked` · `superseded` · `archived` — see [Status](#status-the-maturity-axis) |
| **owner** | who is doing it: `human`, `ai`, or unassigned |
| **committed** | whether it's real planned work or just a floating idea — see [Backlog vs inbox](#commitment-backlog-vs-inbox) |
| **priority** + rank | where it sits in the queue |
| **done_looks_like** | the acceptance criteria — see [Acceptance](#acceptance-criteria-done_looks_like) |
| **why** | a one-line rationale: why this item exists |
| **due_at** | an optional `YYYY-MM-DD` deadline |
| **wait_state** | what it's parked on: `on_human`, `on_dependency`, `on_external` |
| **body** | a **one-line summary** — *not* the content (content lives as [attached files](#documents-attached-to-items-artifacts)) |

Plus bookkeeping Haven manages for you: timestamps, a revision counter, and sync
state.

> **`body` is a summary, never the content.** A one-liner lives on the item; the
> actual prose — specs, research, notes — is stored as files attached to the item.
> See [attached documents](#documents-attached-to-items-artifacts).

## Acceptance criteria (`done_looks_like`)

The most important field. `done_looks_like` states **what "done" means** — the
testable yardstick the work is checked against on the `ready → done` transition.

It is **free text — there is no required format.** The store enforces exactly one
rule: an item can't become **`ready`** without a non-empty `done_looks_like` (you
can't commit to building something with no definition of done).

There *is*, though, real guidance for writing a good one. The Haven skill carries it
(its `spec-quality` reference) and your AI applies it when grooming an item — so in
practice you rarely write acceptance from scratch. Its essence:

- **Every line a yes/no you can *observe*, not *decide*.** "Returns a 422 with
  `{error, field}`", "the export round-trips losslessly" — not "returns an
  appropriate error" or "code quality is acceptable". If reading a line makes you
  *judge* rather than *check*, it isn't acceptance yet.
- **Product-level outcomes, not milestones or tests.** State the result a user would
  recognise ("p95 latency < 200ms") — not "endpoint deployed" or "unit tests pass".
- **No weasel words.** *Fast, simple, intuitive, seamless, efficient* are holes where
  a number or a concrete behaviour belongs — "fast" → "p95 < 200ms".
- **The check:** if every line is met, would a user agree the problem is solved?

Keep it short — it's the yardstick, not the design; detail goes in an attached
`spec`. It's also the exact statement the `verify-acceptance` skill judges against,
and what a human signs off on.

## Types

The `type` axis says what kind of node an item is — it signals what the work
produces and how "done" gets judged. It's a classification you can filter on; it
does **not** gate dispatch. Three families:

**Leaves** — units of work someone picks up and finishes. `task` is the *default*
(what you get if you don't set a type), but it's really the catch-all: on a
software project most leaves are **`code`**, so reach for the specific type instead
of leaving everything as `task`.

- **`code`** — software implementation that changes the codebase: a feature or bug
  fix that lands as a diff, checked by build + lint + test. The workhorse type, and
  what the `verify-acceptance` skill is built to gate.
- **`task`** — the generic default/fallback, for work that isn't one of the more
  specific types below (an idea, a story, a chore).
- **`research`** — investigation whose output is findings or a decision, not a diff.
- **`data`** — data work: datasets, migrations, pipelines.
- **`design`** — design work: UX, visual, interface.
- **`admin`** — process and real-world actions: reviews, approvals, decisions,
  errands. Often human-owned (see [Ownership](#ownership-and-handoffs--human-vs-ai)).

**Containers** — nodes that own a subtree and **roll up** from their children
rather than being worked directly; their status is *derived* (see
[rollups](#containers-and-rollups)), never set by hand:

- **`phase`** — a stage of a larger plan (phase 1, phase 2, …).
- **`release`** — a shippable milestone: the work that ships together.
- **`gate`** — a review checkpoint over a batch of leaves. It depends on the leaves
  it reviews and only becomes actionable once they're *all* done, so downstream work
  waits on the review passing.

**Anchors** — `anchor` is a special container that holds **living project docs**
(vision, architecture, spec) that outlive any single item. `haven docs` lists them.

## Status (the maturity axis)

How well-defined and far-along a leaf is:

```
discovery → definition → ready → in_progress → done
```

- **discovery / definition** — being shaped; not yet ready to pick up.
- **ready** — dispatchable (requires `done_looks_like`).
- **in_progress** — being worked (a soft claim).
- **done** — finished, ideally with evidence.

Plus **blocked** (waiting on something), and two *dead* states — **superseded**
(replaced by a successor) and **archived** (retired). Dead items drop out of the
live views but stay reachable through [lineage](#edges-how-items-connect).
Containers don't use this axis directly — they roll up from their children.

## Ownership and handoffs — human vs AI

Every item is **owned** by a `human` or an `ai` (or left unassigned), and this is
one of the most useful things Haven tracks: it's how a mixed human/AI workflow
stays coordinated without a standup. Because an agent reads the same graph you do,
it can tell **what's its to do from what's waiting on you**:

- It works only its own ready frontier (`next --owner ai`) and **never** auto-picks
  up an unassigned or human-owned item — so you assign AI work to `ai` deliberately.
- When work can't proceed without you — a review, an approval, a decision, a
  real-world action — it gets **handed off**: ownership flips to `human` and the item
  is parked with `wait_state: on_human`. The agent then *surfaces that item as
  waiting on you* instead of silently stalling on it.
- `wait_state` records what an item is parked on — `on_human` (you), `on_dependency`
  (other work), or `on_external` (something outside the project) — so "what's
  waiting on whom" is always answerable.

The baton-pass is a first-class, atomic operation (a **handoff**): it flips the
owner and sets the wait/status in one move — e.g. "API's done, needs your review" →
`owner=human`, `wait=on_human`. Nothing is left `in_progress` with no live owner,
and the next "what's next?" already knows who's on the hook.

**Autonomous mode.** By default the AI keeps you in the loop — it hands work back
for your review and sign-off. When you want, you can also let it run *autonomously*
(the `orchestrate-run` skill): it works its own ready frontier end to end —
building each item in an isolated worktree, checking it with an independent
verifier, merging, and completing what passes — and only stops for the things that
genuinely need you (a human-owned item, or a check it flags as needs-human). The
ownership and `wait_state` signals above are exactly what make this safe: it steps
around anything that's yours and surfaces it, rather than guessing.

## Commitment: backlog vs inbox

The `committed` flag separates what you mean to do from raw capture:

- **Committed** items are the real **backlog** — planned, prioritised work.
- **Uncommitted floaters** are the **icebox**. An uncommitted floater that has no
  acceptance yet is the **inbox** — untriaged capture. Fire ideas in fast as
  one-liners, then triage (commit, refine, or drop) later.

## Edges (how items connect)

Items are wired together by four kinds of relationship:

1. **decomposition** — parent → children ("this is made up of these").
2. **dependency** — blocked → blocker ("this can't start until that is done").
3. **grouping** — a container (`release` / `phase` / `gate`) → its members.
4. **lineage** — provenance recorded when items **split**, **merge**, or
   **supersede** each other. A stale ref always resolves forward to its live
   successor, so history is never a dead end.

The first three are the structure you build; lineage is written automatically as
items evolve.

## Containers and rollups

A container doesn't have its own status — Haven **derives** one from its committed
descendants:

- **dormant** — no committed work beneath it yet
- **queued** — committed work, none started
- **active** — something underneath is in progress
- **done** — every committed child is done

A container can read `done` while uncommitted work still sits beneath it; Haven
flags that separately so a rollup never quietly hides remaining work.

## Documents attached to items (artifacts)

Real content is **files**, stored under `~/.haven/<project>/items/<ref>/` and
recorded against the item as an **artifact** with a **role** that says what kind of
document it is:

| Role | What it holds |
|---|---|
| **spec** | the contract — requirements and acceptance detail |
| **research** | investigation, findings, references |
| **design** | how something will be built |
| **decision** | a decision record (what was chosen and why) |
| **vision** | the north star, usually on an `anchor` |
| **handoff** | context passed when work changes hands |
| **delivery** | evidence of completion (what `complete --evidence` writes) |
| **scratch** | working notes, fix-logs |
| **source** | source material / provenance |
| **context-pack** | the build-ready brief on a grouping container |

Separately, `haven note <ref> "…"` appends a free line to the item's dated notes
file — quick scratch with no role or row.

Project-level **living docs** (vision, architecture, spec) hang off `anchor` items
and are discoverable with `haven docs` — they're the knowledge that outlives any
one task.

## What's required vs optional

- **To capture an item:** just a **title**. Everything else has a sensible default
  (`type` → `task`, `status` → `discovery`, uncommitted, unassigned).
- **To make it dispatchable (`ready`):** add **`done_looks_like`**.
- **To have an agent auto-pick it up:** assign it to **`ai`** — an unassigned item
  is never auto-dispatched.
- **To make it count as planned work:** **commit** it (otherwise it's an inbox
  floater).

## In practice, you don't have to hold any of this

In reality you won't worry about most of what's on this page. When you work through
your AI, it takes care of the mechanics for you — picking the right type, moving
status along as work progresses, wiring up dependencies, writing acceptance, and
handing items back to you when they need a person. That's the whole point of it
knowing Haven.

This page is here for the moments you *want* to go a level deeper — steering how a
build is shaped, directing a piece of research, iterating on a UI, reviewing a
document. Reach for it then. The rest of the time, just talk to your agent in plain
language (see [`USING-HAVEN.md`](USING-HAVEN.md)); you don't need to understand the
ins and outs of the system for it to work for you.
