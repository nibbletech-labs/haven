---
name: orchestrate-plan
description: >-
  The starting point for any large, multi-part effort that has no plan yet —
  building or launching a product, app, site, platform, brand, or business
  end-to-end, especially greenfield ("from scratch", "nothing exists yet", "the
  lot"). Break it into a structured Haven work-graph: a multi-level tree of
  ownable tasks with dependencies (one output feeds another) and acceptance
  criteria. Planning, not doing — fire it even when phrased as doing the work
  ("build it", "create the site", "get the whole thing moving", "have a go at
  X"); you map the whole effort before any is built. Also fires on explicit
  "decompose this into a dependency / work graph / backlog". But a bare "make a
  plan" / "plan this" / "plan mode" for one feature or change is native plan
  mode, not this — every leaf here is plan-mode-grain. If a plan already exists
  and they just want it built, that's `orchestrate-run`; for one task, status, or
  grooming one item, use `haven`. Not for a one-off or single-component task, or
  already-decomposed work.
---

# orchestrate-plan — the planner half of orchestrate

You take a goal and **decompose it into a Haven work-graph**, one node per tick,
until every node is either broken down further or is a skill-grain **leaf** a
dispatcher can pick up. You build structure; **you execute nothing** — no agents,
no skills dispatched, no files written as work product. Execution is a separate
sibling skill (`orchestrate-run`, future) that consumes the same graph.

This is the recursive-decomposition reasoning from builder's `orchestrate`,
re-expressed natively against Haven: the graph **is** the plan (no `state.json`,
no backlog files), and the four edge layers carry the structure.

## Why a graph, and why ticks

- **Work that feeds other work.** A goal decomposes recursively; some pieces'
  outputs feed others (brand → logo → hero image). Decomposition edges carry
  "part of"; dependency edges carry "blocked by / feeds".
- **One node per tick, stateless.** Each tick reads the whole graph fresh,
  advances it by exactly one decompose-or-seal, and ends. There is **no session
  memory** — the graph is the only state. So a fresh session (after a context
  reset, a `/loop` wake, or on another machine over MCP) reorients perfectly by
  re-reading the graph. Run it inline to convergence in one sitting, or wrap it
  in `/loop` to advance across wall-clock time and gates — same skill.

## Operating rules (inherit from the `haven` skill)

Read the `haven` skill's `references/surface-map.md` (CLI⇄MCP differences) and
`references/workflows.md` for op detail — do not restate arguments from memory.
The gotchas that bite the planner:

- **Structure only through ops; content as files/artifacts.** Mutate nodes/edges
  only via `haven …` / `haven_*`. `body` is a one-line summary, never content.
- **`ready` requires `done_looks_like`.** A leaf with no acceptance can't be
  verified or dispatched — never seal without it.
- **Capture ≠ commit.** A bare node is floating/uncommitted/`discovery`. The
  planner *commits* leaves it seals (they're real, ordered work); it leaves
  genuine unknowns floating and surfaces them, rather than fabricating work.
- **Over MCP there is no sticky session** — pass `project` on every call.
- **Creative & strategic decisions are research-and-propose nodes, never quick
  questions.** A brand name, a visual aesthetic, a positioning, a platform choice —
  when the answer should be *earned* by studying comparables and trade-offs, mint it
  as a `research`/`design` leaf that yields options + a recommendation, with the
  dependent work blocked on it; the human decides *after* seeing the proposal. Never
  use AskUserQuestion to shortcut a decision the graph should derive — that's the
  piecemeal trap applied to judgment, not just architecture.

## The tick

The full per-step ops (CLI **and** MCP) are in `references/tick-ops.md`. The
loop:

0. **REORIENT.** Read the whole graph in one call (`haven graph` / `haven_graph`).
   This is the only tick state. Resolve the project first if unknown. If the read
   carries a **`grooming_nudge`** (untriaged/stale work has piled up), surface it
   and offer the groom workflow (`haven` skill, workflow 3) before planning —
   don't plan on top of a backlog that needs triage.
1. **ENSURE ROOT.** A fresh goal with no root → create the decomposition root as
   an `anchor`, idempotently (`if_absent` is safe for the unique goal title). An
   existing root (e.g. you were handed a project mid-plan) → skip.
2. **COMPUTE FRONTIER** (pure arithmetic on step-0's nodes + edges — no op). A node
   is **on the frontier** iff: it is **live** (status ∉ done/superseded/archived);
   **and** it has **no outgoing decomposition edge** (not yet split); **and** it is
   **not a sealed leaf** (not `ready` + committed + has `done_looks_like` + an
   assigned owner); **and** it is **not deferred** (not `blocked` awaiting an
   unfinished dependency — step 7). A deferred node **re-enters** the frontier the
   moment all its dependencies are `done` (i.e. its shape is now knowable) — that
   re-entry, on a later pass, *is* the replan. Nodes of type
   `anchor`/`release`/`phase`/`gate` are **never** frontier (containers/reviews).
   **Never touch a node owned by the executor and `in_progress`** — the planner
   mutates structure, not live work.
3. **PICK ONE** node deterministically: shallowest decomposition-depth first, then
   by `ref`. (Depth is single-valued — see "Keep it a tree".)
4. **READ ITS DETAIL** if step-0's compact node lacks the prose you need to judge
   grain (`haven item get` / `haven_get_item`). Read `body`/`why`/`done_looks_like`.
5. **DECIDE — seal, split, defer, or discover?** Apply the stop test
   (`references/decomposition.md`). It's skill-grain → **seal** (step 8); it should split
   **and** depth < 5 **and** its children are knowable now → **split** (step 6); it should
   split **but** its shape depends on an output that doesn't exist yet → **defer** (step 7);
   or you *could* seal it but only by **assuming** a load-bearing unknown (feasibility, fit,
   approach, mechanics, magnitude, an external answer, or whether it's worth doing at all) →
   **discover** — mint an AI-first discovery leaf and defer the dependent on it (step 7).
   **Never seal architecturally-coupled siblings as independent leaves over an unmade shared
   decision/contract** (the data model / API / convention they all assume): promote that
   foundation to one node — *decided* now if you can, else a discovery leaf — and **defer the
   dependents on it**, so the frontier predicate holds them until it resolves. Sealing fragments
   over an undecided foundation is the piecemeal trap.
6. **SPLIT** (one level): create each child wired to the parent via a decomposition
   edge; wire dependency edges where one child's output feeds another. Record a
   one-line rationale on the parent's `why` (or a `decision` artifact) when the
   split is non-obvious. **End the tick** — children existing *is* the parent's
   decomposed state; there is no `decomposed` status to set.
7. **DEFER / DISCOVER** (the knowability & evidence horizons): the node should break down,
   but you can't yet — either because its children depend on an **unproduced output** (you
   can't know the storefront's sub-tasks until the platform is chosen) *or* because sealing it
   would rest on a **load-bearing unknown** you'd only be guessing at (feasibility, fit,
   approach, mechanics, magnitude, an external answer, or whether it's worth doing at all).
   Either way, don't guess. Make the **producer** exist as its own node — for a missing output,
   the build work that yields it; for a missing fact, a **discovery leaf** (`done_looks_like` =
   the evidence/decision it produces) — wire this node's **dependency** edge to it, set this
   node **`status=blocked`**, leave it coarse, record *why*. **End the tick.** It re-enters the
   frontier (step 2) when the producer completes — the next pass decomposes it *then*, with the
   answer in hand. **Route a discovery producer AI-first:** default a `research`/probe leaf owned
   `ai` (web research, data, or a cheap *reversible* try); reserve `human` for genuinely
   human-only unknowns AI couldn't settle under a low uncertainty bar (full routing + the seven
   unknowns: `references/decomposition.md`, "the evidence check"). Also drop a floating
   `discovery` node for any genuine gap/unknown you notice but can't place yet (capture is cheap).
8. **SEAL AS LEAF** (skill-grain reached, or depth cap hit): set a concrete,
   testable `done_looks_like` + `status=ready` + commit (priority) + an **owner** —
   `ai` for work the executor can do, `human` for real-world tasks (formulation,
   payments, legal sign-off) only a person can. A node can be sealed even while a
   dependency blocks its *execution* — that just orders the queue; sealing needs only
   that the work itself is a knowable unit. **Before you seal, run the seal gate
   (`references/value-density.md`):** the **first-cut tests** (value-density / effort /
   dependency) + the **Grounding Rule** decide whether this work even belongs in the
   first pass or is **Future** ("Why deferred" + "Why it matters later"); the
   **decomposition-quality battery** (single-buildable-unit, complexity realism,
   dependency + external-dependency honesty, oversized-item detection, the
   bidirectional false-ready/false-discovery check) decides whether it's sound enough to
   seal or must go back to split/defer; and the **Gherkin-readiness bar** decides whether
   `done_looks_like` is specific enough to verify ("works correctly" fails; "user can
   create an account, log in, see their dashboard" passes). **End the tick.**
9. **(Deferred unless the user asks) GATE.** A reviewable batch of leaves → a `gate`
   node depended on each reviewed leaf, with pass-criteria in its `done_looks_like`.
   Gates are not required for convergence.

Loop to step 0 until the frontier is empty.

### Keep it a tree

Create **single-parent** decomposition (each child has exactly one parent). Model
cross-branch sharing — a logo two pages both need — as a **dependency** edge to the
one shared node, never as a second decomposition parent. This keeps "depth" a single
well-defined number and the graph easy to reason about. (Haven's store *allows* a
decomposition DAG; v1 deliberately doesn't use it.) The same single-node-plus-dependency
pattern models a shared **decision or architecture contract**, not just a shared artifact:
emit the coupling as one dependency edge to the shared node, never as a duplicated assumption
threaded across the leaves.

### Cycles are enforced by the store

A dependency edge that would create a cycle is **rejected with an error** — don't
pre-walk to check. Attempt the edge; if it errors, you almost certainly have the
direction backwards (`from` is the blocked/consumer, `to` is the blocker/producer)
— fix it or drop it, and log why.

## Convergence

Converged ⇔ a fresh reorient yields an **empty frontier**: every live, non-container
node is decomposed, a sealed leaf, or **deferred** (`blocked`, awaiting an unproduced
input). That's "planned as far as current knowledge allows" — deferred branches are
expected, not a failure; they get fleshed out on the pass that follows their
dependency completing. Then:

- **Report the leaf set** — that's the dispatch queue (`haven next --owner ai`) — **and
  the deferred branches**, each with what it awaits ("Build storefront — deferred until
  Choose platform is done"). That report is the plan→execute→replan handshake.
- **Goal-coverage check.** If the leaves don't plausibly cover the root anchor's
  goal/`done_looks_like`, capture the gap as a **floating `discovery`** node and
  surface it — never invent committed leaves to paper over a gap.
- **Depth cap 5** (planner-enforced; Haven imposes none). Warn at depth 4; at depth
  5 seal-and-warn rather than splitting deeper ("PF-n is still coarse at depth 5 —
  widen its siblings or revisit the parent"), and surface to the human.

## Fresh-session handoff

Because every tick commits before it ends and the graph holds all state, a cold
session just runs step 0 and recomputes the frontier — zero bespoke handoff. If a
session dies mid-tick the worst case is a parent with some-but-not-all children,
which the next reorient sees as still-on-frontier; the title-dedupe in step 6 keeps
re-runs from duplicating.

There is **no callable token gauge**, so checkpoint by proxy: after roughly **15–20
ticks**, or when a single `haven graph` read is itself large, finish the current
tick (never mid-tick) and hand off. v1 ships a **manual resume** one-liner —
`/orchestrate-plan continue <project>` (the project key is the only argument; the
graph carries the rest). The harness auto-compact is the backstop, so the threshold
is an optimisation, not a correctness requirement. (A self-firing scheduler is a
future addition, not v1.)

## What the planner guarantees (the contract with the executor)

The two skills **never talk directly — they meet only at the graph.** At convergence:

1. every live, non-container node is decomposed or a sealed leaf;
2. a **sealed leaf** = `ready` + committed (priority 0–4) + concrete `done_looks_like`
   + an assigned owner (`ai` for the executor's queue, `human` for real-world tasks);
3. dependency edges encode real ordering (`from` blocked-by `to`) and are acyclic;
4. shared cross-cutting architecture — a decision/contract several leaves *assume* — is
   reified as its own node with a dependency edge from every dependent, so **no leaf
   presupposing an undecided foundation is dispatchable in isolation** (these are the edges
   the executor's pack-first grouping folds on);
5. no orphaned half-decompositions at rest; depth ≤ 5; lineage intact (restructuring
   went through evolve/archive with a rationale, never deletion).

The future executor relies on `haven next --owner ai` as the AI dispatch queue and on
`complete` reporting what each completion unblocks — which it re-feeds into this same
tick loop. Human-owned leaves surface to the person (`next --owner human` /
`wait_state`), not the AI queue. (`next` does not *itself* require `done_looks_like`;
that guarantee is the planner's discipline. A leaf missing acceptance is a planner
defect, not something to dispatch.)

## Deferred to v2 / not in this skill

Execution (agent/skill dispatch, verification, completion, evidence). Also: a live
skill-discovery manifest (grain is judged by the stop test's heuristics, not a
manifest), explicit scope/constraint fields, autonomy modes, and conflict detection
across concurrent planners (v1 assumes one planner per project per session). (The
**evidence/discovery gate** — minting AI-first research/probe leaves over load-bearing
unknowns — is **in** now, step 7; heavier test-and-learn/alternatives machinery beyond
that is not.) These orchestrate concepts have no Haven primitive yet — reason about them
in prose if useful, but don't pretend the graph encodes them.
