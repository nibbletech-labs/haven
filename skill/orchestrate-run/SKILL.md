---
name: orchestrate-run
description: >-
  Autonomously execute an already-planned Haven graph (packing any still-packless
  coupled cluster first, as it goes): loop the
  AI-owned ready frontier, build each batch in its own git worktree via native
  plan mode, gate it with a fresh verifier, merge to main, complete the leaves
  (which unblocks downstream), and converge — stepping around anything blocked on
  a human. The executor half of orchestrate. Use when the plan exists and you want
  it built — e.g. "run the build", "execute the plan", "dispatch the ready work",
  "build phase 1", "autonomously implement the graph", "work the backlog". DEFERS:
  decomposing a goal → `orchestrate-plan`; writing the build spec → `create-context-pack`;
  the actual code + the human approval gate → native plan mode. It owns the
  loop / worktree / gate / merge — it does NOT write code itself. Not for planning,
  spec-writing, or a one-off single-item edit (use the `haven` skill).
---

# orchestrate-run — the executor half of orchestrate

You take a graph that's already **planned** (`orchestrate-plan`) — **packed**
(`create-context-pack`), or with any still-packless coupled cluster packed first as you
go — and **build it** — autonomously, batch by batch, until the AI-owned frontier is empty. You own the **loop / worktree / gate / merge**; you do
**not** write code. The code work is **native plan mode + ultracode**, handed the
pack; you dial its effort and gate and integrate its output. The two halves meet
only at the graph.

This is builder's `devteam`/`orchestrate` executor re-expressed natively: the graph
**is** the coordination medium (no `state.json`, no message bus, no batch-plan file),
and every tick reorients from it.

## Where it sits (the family — meet only at the graph)

`orchestrate-plan` (decompose a goal → graph) → `create-context-pack` (batch ready
leaves under a container + a verify-first `spec` pack) → native **plan mode / ultracode**
(the code) → **`orchestrate-run`** (this: loop, gate, worktree, merge, complete, converge).

This executor is **one of four ways** Haven work gets driven — the most orchestrated end of the
spectrum, and **serial today** (`MAX_PARALLEL=1`, HV-85 for parallel). For when to reach for it
vs the inline / solo-plan-mode paths — and the build-subagent parity caveat (HV-167) that makes
inline often the better choice for *small* runs — see the `haven` skill's
`references/running-work.md`.

## Three load-bearing invariants (do not weaken these)

1. **SINGLE ORCHESTRATOR PER PROJECT.** You are the *sole* reader of `haven next
   --owner ai` and the *sole* writer of graph state. The per-batch build agents you
   spawn are **pure executors**: handed an explicit member list + the pack, and
   **forbidden to touch the graph** (no status flips, no edges, no completes). This is
   what makes the soft claim race-free — setting a leaf `in_progress` drops it from the
   frontier, and nothing else reads the frontier in the read→write gap. Haven has **no
   atomic claim / lease** (a plain UPDATE, monotonic-LWW `revision`, WAL serializes
   writers without surfacing a conflict), so safety is **topology**, not a lock. (The
   atomic-claim verb is HV-24; it earns its place only if you ever drop this invariant.)

2. **SERIALIZED MERGE QUEUE with a mandatory post-rebase re-gate.** Builds may fan out
   into N worktrees, but merges to `main` fan **in** through one lockfile:
   `lock → rebase onto current main → RE-GATE → fast-forward → complete`. The re-gate is
   **inviolable, not a tunable**: two batches that are *graph-independent* but share an
   *implicit code surface* can merge textually clean yet break semantically, and the
   re-gate is the only thing that catches it before it lands on `main` as "done". Merge
   **before** complete, always — that keeps the one crash window recoverable.

3. **STATELESS REORIENT + graph↔worktree reconcile every tick.** The graph
   (`in_progress` status), `git worktree list`, and a commit done-marker are the **only**
   truth; nothing is held in session memory. Recovery is not a special mode — every tick
   begins by reconciling, so a crash anywhere lands in a state the next tick fixes
   idempotently. A cold session (context reset, `/loop` wake, another machine over MCP)
   reorients perfectly by re-reading the graph.

## Operating rules (inherit from the `haven` skill)

Read the `haven` skill's `references/surface-map.md` (CLI⇄MCP) for op detail — don't
restate arguments from memory. The exact op per tick step is in `references/tick-ops.md`;
the git runbook is in `references/worktree-merge.md`; the effort/gate/strike knobs are in
`references/dispatch-policy.md`; the build-agent discipline the gate enforces — fix-log +
3-strike circuit breaker, TDD-as-a-gate, the change-request envelope, batching heuristics, and
human-gated knowledge promotion — is in `references/executor-discipline.md`.

- **Structure only through ops.** Mutate nodes/edges only via `haven …` / `haven_*`.
  The fix-log and any run notes are artifact **content**, never `body`.
- **Over MCP there is no sticky session** — pass `project` on every call; **no batch** —
  one entity per `haven_update_item` / `haven_complete_item` call, loop.
- **You never write code.** Every code edit happens inside a spawned build agent's
  worktree. If you find yourself editing a source file directly, stop — that's the
  build agent's job.

## The tick (one stateless pass; loop to convergence)

0. **REORIENT + RECOVER.** Read the whole graph in one call (`haven graph` /
   `haven_graph`); resolve the project first if unknown. Then **reconcile** the graph's
   `in_progress` leaves against `git worktree list` (`references/worktree-merge.md`): an
   `in_progress` leaf with a live build agent → leave it; with no live agent → **orphaned**
   (crashed mid-build) → if its worktree holds a clean, gate-passing build resume at MERGE,
   else prune the worktree and send the batch down the failure path (strike count survives
   in the container's fix-log); a worktree with no `in_progress` leaf → **stale** → prune.
   Do this **before** dispatching anything.
   - *Large-graph fallback.* On a mature project the whole-graph read can exceed the MCP
     response limit (`haven_graph` is all-or-nothing — no scoped read yet; HV-25/HV-195). When
     it does, **reorient from the frontier, not the whole dump**: `haven list_items --status
     in_progress --owner ai` for the RECOVER reconcile set, `haven next --owner ai` (step 1)
     for the dispatch queue, then read only each **active container's** `context-pack` (steps
     2/4). Same tick, a bounded slice instead of the whole graph — don't grep the spilled dump.
1. **FRONTIER.** The AI dispatch queue is exactly `haven next --owner ai`
   (DISPATCHABLE_PREDICATE: committed + `ready` + ≠anchor + `wait_state` NULL + no open
   dependency). This **inherently steps around** human-owned work and AI work blocked by
   an unfinished dependency. Trust the predicate; never re-derive it.
2. **GROUP.** Fold the frontier two ways. **Packed leaves** fold by their derived
   `context_pack.container` pointer (the fold key for already-packed work); a leaf with a
   `context_pack_clash` (>1 packed container) is **skipped and surfaced**, never auto-picked.
   **Packless leaves** have a NULL `context_pack.container`, so they carry no fold key —
   tentatively cluster them by a **shared `depends_on` producer** (the build-time mirror of the
   planner's foundation node): a packless multi-leaf cluster sharing one → **step 4 (packed
   first)**. **Never fold by decomposition parent** (that auto-bundles independent siblings). A
   packless leaf sharing no producer with others, or a packless **singleton**, → a degenerate
   batch, built directly under the deterministic gate.
3. **SELECT.** A batch is dispatchable **now** iff every member is in the ready frontier
   (no member has an open cross-batch dependency — dependent batches simply aren't ready
   yet, so they don't appear). Take up to **MAX_PARALLEL** independent batches
   (`references/dispatch-policy.md`; **default 1** — see *Serial-first* below).
4. **ENSURE-PACKED — pack-first is a precondition of CLAIM, never a fallback after it.**
   For an **already-packed** batch this is the cheap assertion: the container carries a `spec`
   `context-pack.md` (the pointer guarantees it). For a **ready packless cluster whose members
   share an architecture** (signalled by a shared `depends_on` producer — the build-time mirror
   of the planner's foundation node — and confirmed by `create-context-pack`'s shared-context
   assessment) → **pause
   before claiming any member** and **compose `create-context-pack`** on the member-ref set
   (it owns the grouping axis — resolving/creating the container, grooming, clash-checking, and
   writing the pack), then **re-tick** so the members fold by their new `context_pack.container`
   into one batch — and only **then** reach CLAIM. You hand over the member set as a dispatch
   **hint**; you **never** pre-create the container, add grouping edges, or write a pack
   yourself. `create-context-pack` may return **"simple batch — no pack"** (no shared
   architecture) → those members proceed to CLAIM as ordinary singletons. (A packed batch only
   ever holds **mutually-ready** members — step 3 excludes any leaf with an open dependency — so
   it co-builds independent members that share a *brief*, never a dependent with its unmerged
   foundation; that ordering is the **dependency edge's** job, not the pack's. The pack groups
   the batch's shared **context** the verifier reads; it does not change dispatch granularity.)
5. **CLAIM.** Soft-claim every member of the batch: `haven_update_item {ref,
   status:in_progress}`, one call per leaf. This removes them from `next`, so a re-read
   this same tick won't re-pick them. **Claim before you spawn — never spawn before claim.**
6. **DISPATCH.** Per batch, create an isolated worktree off `main`
   (`references/worktree-merge.md`) and spawn **one** native build agent into it, handed:
   the container's `context-pack.md` (`haven_get_artifact {ref:container, role:context-pack}`), the
   members' `done_looks_like`, the **effort/model** and **gate mode** per
   `references/dispatch-policy.md`, and — for each leaf — a **2–5 step self-check derived
   from its `done_looks_like`** (a green global build is not proof a specific leaf's
   acceptance is met). The agent is a pure executor: it builds, runs its self-check, and
   reports pass/fail + evidence + any **scope finding** — it does **not** touch the graph.
   If it discovers its member list is wrong / a dependency is missing, it **surfaces and
   returns** — it must never silently overreach or stall (the Change-Request rule); you
   decide next tick whether to re-pack, re-plan, or adjust the batch.
7. **GATE — a fresh verifier, not the builder.** Run the gate **inside** the worktree.
   *Unattended:* spawn a **separate verifier agent** given only the leaf's `done_looks_like`
   + the pack's shared requirements + the diff — **not** the build agent's reasoning — which
   runs `build + lint + test` (exit-0) and judges acceptance, returning pass/fail + evidence.
   A same-context reviewer is structurally blind; the verifier's independence is the point.
   **The verifier is a spawned subagent and does NOT inherit the `verify` skill** — so
   "compose `verify`" means the executor **reads `skill/verify` and FORWARDS its contract into
   the verifier's prompt**: the PASS / NEEDS-HUMAN / FAIL definitions
   (`verify/references/verdict-contract.md`), the independence rule, and the **exhaustive
   every-acceptance-clause walk** + lens (`verify/references/evaluation-lens.md`). Naming the
   skill reaches nothing — the spawned agent only knows what its prompt carries; you forward the
   contract, you don't re-implement the judgment. *Attended:* native plan-mode human approval. A
   fail stays in the worktree — nothing merged, siblings untouched → failure path (§ below).
8. **MERGE (serialized).** Acquire the single merge lock; `rebase` the batch branch onto
   current `main`; **re-run the deterministic gate post-rebase** (catches semantic conflicts
   a clean textual merge hid); only a green re-gate **fast-forwards** to `main`. Rebase
   conflict or red re-gate → do **not** merge, release the lock, send the batch to the
   failure path; `main` and siblings stay clean. (`references/worktree-merge.md`.)
9. **COMPLETE + REPLAN.** Only after the work is on `main`: `haven_complete_item {ref,
   evidence}` per leaf — which returns `unblocked[]`. When a leaf made a non-obvious
   integration/contract decision, also append a short `delivery`/`decision` artifact on it
   so a downstream batch's build agent reads it. **Replan check:** if a completion's
   evidence **contradicts** a downstream leaf's `done_looks_like` or makes it moot, do
   **not** silently build the stale leaf — bounce that branch back to `orchestrate-plan`
   (the pack's "structurally-wrong → re-plan" escape, applied in the *run* loop). Remove
   the worktree; release the lock.

**Collecting a spawned agent's result (build § 6, gate § 7).** A spawned agent often signals
**idle/complete WITHOUT delivering its final report** — treat the idle signal as *"go fetch the
report,"* never as the report itself. After any build or verifier agent goes idle, **explicitly
retrieve and confirm its structured result** (the build self-check outcome / the
PASS·NEEDS-HUMAN·FAIL verdict + evidence) — `SendMessage` to pull it when it didn't arrive — and
**never advance the tick on a missing or empty report**: a silent absent verdict must not be read
as a pass. The loop waits for the *report*, not merely the completion notification.

Loop to step 0. **Converge** when `haven next --owner ai` is empty **and** nothing is in
flight → report blocked-on-human items (`next --owner human` / `wait_state on_human`) and
any strike-escalated items, then stop (inline) or sleep (`/loop`, v4).

When you surface progress to a person — mid-run status, a merge-gate pause, the convergence
report — **report it as plain capabilities, not node refs** (the `haven` skill's *Standing
caution*: lead with what a user can now do, before → now; a list of `HV-…` refs is not a
progress report). Inherited rule — see the haven skill, not restated here.

## Failure, retry, escalation

A gate-fail is isolated in the worktree (and again post-rebase in the merge lock). On fail:
**append a fix-log entry** as an append-only artifact on the **batch container** (graph-
durable; `metadata` is write-once, so run-state cannot live on the node — and that's a
feature). **Strikes are derived by counting** fix-log entries — no schema field, same
derived-on-read lens as `context_pack`/`rollup_state`. Retry by putting the leaf back on the
frontier — for an `in_progress` failed leaf the cheap path is `haven_update_item {status:
ready}` (note: `haven_reopen` resets to **discovery**, not ready, so a reopened leaf must be
re-groomed before re-dispatch — reserve it for leaves the run archived). At an **N-strike
ceiling** (default 2–3, `references/dispatch-policy.md`) **stop retrying and escalate**:
`haven_handoff {ref, to:human, wait:on_human}` with the fix-log as the diagnosis — which
self-evicts the batch from `next --owner ai` while the rest of the graph keeps converging.
The strike ceiling is the **liveness guarantee**: the AI frontier strictly shrinks, so the
loop provably converges.

## Serial-first (this version) — parallelism is a gated dial

The whole skill is specified, but **`MAX_PARALLEL` defaults to 1**: this version dispatches
**one batch at a time** and proves the full machine — happy path, loop, replan, failure,
and crash-recovery — with **zero concurrency**. Even at 1 the MERGE step runs the full
`lock → rebase → re-gate → ff` path (a degenerate one-entry queue), so the merge discipline
is exercised from the start. **Turning on parallel fan-out (`MAX_PARALLEL > 1`) is the one
gated step** (`references/dispatch-policy.md`): do it only once the serial path holds on a
real run, because the parallel-merge + re-gate seam is where a missed re-gate can silently
land broken code on `main` as "done" — the failure mode the whole design is organized to
prevent, and the one you cannot observe in a parallel soup.

**Pack-first stays serial too.** A packless coupled cluster takes **two ticks** — tick N
composes `create-context-pack` to establish the pack, tick N+1 dispatches the now-packed batch
— with **never more than one batch in flight** and no added concurrency. And because coupled leaves
are ordered by their shared foundation's dependency edge, they never build as separate
worktrees over an undecided architecture, so the cross-worktree semantic-conflict seam
(invariant 2) cannot arise for them.

## Convergence / fresh-session handoff

All state is in the graph + the worktrees on disk, so a cold session re-runs step 0,
reconciles, and continues — re-running is idempotent. v1 ships a manual resume:
`/orchestrate-run <project>`. There is no callable token gauge; finish the current
tick (never mid-tick) before handing off, and the harness auto-compact is the backstop.

## Deferred to v4 / not in this skill

The `/loop` wrapper for fully-unattended autonomy; an atomic CAS-claim / lease (HV-24) —
needed only for true multi-orchestrator, not for single-orchestrator + N worktrees; a
gate-before-complete store contract — redundant while one trusted orchestrator binds
complete to merge-after-green by convention. A mutable run-state field on the node is
**rejected**: derived-from-artifact strikes are strictly better. These have no Haven
primitive yet — reason about them in prose, but don't pretend the graph encodes them.
