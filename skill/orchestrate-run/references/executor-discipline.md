# Executor discipline — how a build agent is gated, bounded, and remembered

`references/dispatch-policy.md` sets the per-batch *dials* (parallelism, effort, gate
mode). This file is the **discipline** the dials sit on: the anti-repeat memory that
stops a retry loop spinning, the binary acceptance gates a build cannot bypass, the
scope envelope a build agent may not silently exceed, the rules for *how* a batch is
composed, and the human-gated promotion of anything durable a build learns. These are
the techniques carried over from builder's `devteam` executor — re-expressed natively,
because **the build/verify subagents you spawn do not inherit any skill**, so this
discipline only exists if you put it in their prompt.

The `<C>` notation, the worktree paths, and the `haven_*` ops for each step live in
`references/worktree-merge.md` and `references/tick-ops.md`; this file is the *why* and
the *exact text* (formats, thresholds, lists) the executor enforces.

## The fix-log — file-based anti-repeat memory

Every gate-fail appends an entry to a single **append-only** fix-log artifact on the
**batch container** (`role:scratch`, `fix-log.md` — `references/tick-ops.md` § Failure
path). It is *graph-durable* and survives agent discontinuity: a build agent is
ephemeral (gone after its batch), but the fix-log is on disk and re-readable by any
fresh agent, the verifier, or a human.

A retry is a **fresh fixer agent**, not the original builder. Two non-negotiable rules
make the log do its job:

1. **A fresh fixer reads the fix-log first.** Before touching code, it reads every
   prior attempt on this acceptance id and **tries something DIFFERENT** — the log
   exists precisely so the loop does not re-walk a dead end with a clean memory.
2. **Log BEFORE and AFTER.** The fixer logs *what it is about to try and why* **before**
   starting, then logs *the result* **after** — so even a crash mid-fix leaves the
   attempt recorded and the next fixer is not blind.

**Fix-log entry format** (one block appended per attempt — keep it verbatim so any
fresh agent can parse the history):

```
### <ref> / <acceptance-id> — Attempt N
**What was tried:** [specific change made]
**Why:** [reasoning for this approach]
**Result:** [pass/fail — what happened]
```

The fix-log is file-based — it survives agent discontinuity, can be read by any fresh
agent or the user, and forces each Fix Agent to see all previous attempts.

## The 3-strike circuit breaker (per acceptance id)

Strikes are **derived by counting fix-log entries** — there is no schema field on the
node (same derived-on-read lens as `context_pack` / `rollup_state`; `metadata` is
write-once and *must not* hold run-state). The breaker is counted **per acceptance id**
(`done_looks_like` / the leaf's SC-equivalent), not per batch:

> **Circuit breaker.** After 3 consecutive failed fix attempts on the same acceptance
> id, the fixer stops retrying and escalates to a human — with the **fix-log path** as
> the diagnosis.

In native terms the escalation is `haven_handoff {ref, to:human, wait:on_human, note}`
carrying the fix-log summary + last gate excerpt — which self-evicts the batch from
`next --owner ai` while the rest of the graph keeps converging
(`references/tick-ops.md` § Failure path c). The ceiling is the **liveness guarantee**:
the AI frontier strictly shrinks, so the loop provably converges — no acceptance id
retries forever. (Default ceiling 2–3, a per-run dial in `references/dispatch-policy.md`
§ STRIKES; the *escalates-to-a-human-with-the-log* shape is fixed.)

## TDD as a gate — RED before GREEN, binary and non-bypassable

For a **behavioral** code leaf, translate the acceptance (the leaf's `done_looks_like`,
or the pack's Gherkin SC-ids where it has them) into a **FAILING test FIRST**, then
make it pass. **RED before GREEN. Always.** This is not a style suggestion — it is a
gate item:

1. Write a failing test that captures the acceptance criterion
2. Run it — confirm it fails (**RED**)
3. Write the minimum implementation to make it pass (**GREEN**)
4. Refactor if needed
5. Move to the next acceptance id

> Do not write implementation before tests. The acceptance scenarios are the
> specification — translate them to test code first.

The binary sign-off item the verifier checks is: **"a failing test was written before
the implementation"** — captured by including the **red→green transition in the
evidence**. It is in the **cannot-be-bypassed** set: *no implementation without a prior
failing test*. Default it **on** for ultracode / complex leaves; optional for purely
mechanical work (a rename / config edit has no behavior to drive out).

## The change-request envelope — a build agent implements ONLY its listed items

The build agent is handed an **explicit member list — its envelope** — and the pack.
It may **not** write the graph and may **not** self-grant scope. Adding a dependency,
making a schema change, standing up a new service, or finding the spec
ambiguous/incorrect does **not** authorise the agent to act — it must raise a
**structured Change Request and wait.**

**Change-Request trigger conditions** (any one forces a CR, not a freelance fix):

- Adding a dependency not already in the project
- Implementing functionality beyond the listed member items
- Making architectural changes (schema changes, new services, new tables)
- Discovering the spec / pack is ambiguous or incorrect in a way that affects
  implementation

**Change-Request format** (the agent surfaces this in its result / a scratch note and
**returns** — it does not proceed):

```
# Change Request: <ref>
Raised by: build agent
Reason: [specific gap in the spec / pack]
Requested: [exactly what is needed]
Impact: [what it affects]
```

Then **wait — do not assume approval.** A build agent that hits a trigger must
**surface and return**, never silently overreach (which would poison the merge) and
never silently stall. You — the single orchestrator — decide on the next tick whether
to re-pack, re-plan, or adjust the member list. No self-granted scope expansion, ever.

## Batching heuristics — how a batch is composed

When you compose a packless cluster into one batch (or hand a member set to
`create-context-pack`), the *grain* of the batch follows these rules. The graph's
**dependency edges are the skeleton** — the native analogue of builder's execution-graph
streams:

- **Batch WITHIN a stream, never across.** Co-batch only mutually-ready leaves under one
  shared producer (one foundation / one architecture); never bundle leaves that sit on
  opposite sides of a dependency edge into the same worktree.
- **Integration checkpoints are hard boundaries.** A point where independent work must
  re-converge **always ends the current batch** — downstream work starts a fresh batch
  after it lands.
- **Shared `key_files` within a stream → ONE agent owns the evolving file state.** If
  two members would edit the same file, give them to a **single** build agent — one
  agent understanding the file as it evolves beats two agents conflicting on it. (This
  is also why the cross-worktree semantic-conflict seam, invariant 2, is something to
  *avoid creating*, not just to re-gate against.)
- **Complexity caps the batch:** **~5 members at Standard, ~3 at Deep** (complex /
  cross-cutting work). Context quality degrades with volume — a fat batch is a thin
  build.
- **No ceremony for tiny packs.** A **1–2-item** batch is one build agent + one gate,
  no pack, no orchestration overhead — the degenerate path
  (`references/tick-ops.md` § 2–3, the packless singleton).

## Human-gated knowledge promotion

A build that makes a durable, project-level discovery — an architecture decision
(ADR-shaped), or a convention / key-file mapping / constraint that **outlives this
feature** — does **not** get to edit project docs. The build agent **drafts** it (as a
`delivery` / `decision` artifact on the leaf, or an `ARCHITECTURE-UPDATE`-style note),
and the executor **promotes it to the project's living docs ONLY on explicit user
approval** — never silently.

In native terms: a non-obvious integration/contract decision is recorded as a
`delivery` / `decision` artifact on the completed leaf (`references/tick-ops.md` § 9b)
so a downstream batch's build agent reads it — that is *in-graph* and automatic. But
folding it up into the project's **durable design docs** (e.g. Haven's own `HV-20`
living-docs anchor) is a **human gate**: surface the draft, name what it would change,
and wait for the go. Drafts are cheap and automatic; promotion is deliberate and
approved.
