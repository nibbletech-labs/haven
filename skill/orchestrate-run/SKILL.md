---
name: orchestrate-run
description: >-
  Autonomously execute an already-planned Haven graph: loop the AI-owned ready
  frontier, build each batch in its own git worktree, gate it with a fresh
  verifier, merge to main, complete the leaves (which unblocks downstream),
  and converge — stepping around anything blocked on a human. Use when the
  plan exists and you want it built — "run the build", "execute the plan",
  "dispatch the ready work", "build phase 1", "work the backlog". It owns the
  loop / worktree / gate / merge and does NOT write code itself. Defers
  decomposing a goal to `orchestrate-plan`, the build spec to
  `create-context-pack`, the code + human approval gate to native plan mode.
  Not for a one-off single-item edit (use the `haven` skill).
---

# orchestrate-run — the executor half of orchestrate

You take a graph that's already **planned** (`orchestrate-plan`) — **packed**
(`create-context-pack`), or with any still-packless coupled cluster packed first as you
go — and **build it** — autonomously, batch by batch, until the AI-owned frontier is empty. You own the **loop / worktree / gate / merge**; you do
**not** write code. The code work is a **plan-first build agent** (ultracode-grade): handed
the pack, it plans, has that plan validated, then builds (§ tick step 6); you dial its
effort/model and gate and integrate its output. The two halves meet
only at the graph.

This is builder's `devteam`/`orchestrate` executor re-expressed natively: the graph
**is** the coordination medium (no `state.json`, no message bus, no batch-plan file),
and every tick reorients from it.

## Where it sits (the family — meet only at the graph)

`orchestrate-plan` (decompose a goal → graph) → `create-context-pack` (batch ready
leaves under a container + a verify-first `spec` pack) → a **plan-first build agent**
(ultracode-grade; the code) → **`orchestrate-run`** (this: loop, gate, worktree, merge, complete, converge).

This executor is **one of four ways** Haven work gets driven — the most orchestrated end of the
spectrum. It runs serial or parallel by the coordinator's per-run call (`MAX_PARALLEL`;
`references/dispatch-policy.md`). For when to reach for it
vs the inline / solo-plan-mode paths — and the build-subagent parity caveat (HV-167) that makes
inline often the better choice for *small* runs — see the `haven` skill's
`references/running-work.md`.

## Load-bearing invariants (do not weaken these)

1. **SINGLE ORCHESTRATOR PER PROJECT.** You are the *sole* reader of `haven next
   --owner ai` and the *sole* writer of graph state. The per-batch build agents you
   spawn are **pure executors**: they get an explicit member list plus the pack, and
   they are **forbidden to touch the graph** — no status flips, no edges, no
   completes. This is what makes the soft claim race-free: setting a leaf
   `in_progress` drops it from the frontier, and nothing else reads the frontier in
   the read→write gap. Haven has **no atomic claim / lease** — a claim is a plain
   UPDATE with a monotonic last-writer-wins `revision`, and the write-ahead log
   serializes writers without surfacing a conflict. So safety is **topology**, not
   a lock. (The atomic-claim verb is HV-24; it earns its place only if you ever
   drop this invariant.)

2. **SERIALIZED MERGE QUEUE with a mandatory post-rebase re-gate.** Builds may fan
   out into N worktrees, but merges to `main` fan **in** through one lockfile:
   `lock → rebase onto current main → RE-GATE → fast-forward → complete`. The
   re-gate is **inviolable**. It is the only thing that catches a semantic conflict
   a clean textual merge hid (why: `references/worktree-merge.md`). Merge **before**
   complete, always — that keeps the one crash window recoverable.

3. **STATELESS REORIENT + graph↔worktree reconcile every tick.** The **only** truth
   is the graph (`in_progress` status), `git worktree list`, and a commit
   done-marker. Nothing is held in session memory. Recovery is not a special mode:
   every tick begins by reconciling, so a crash anywhere lands in a state the next
   tick fixes idempotently. A cold session (context reset, `/loop` wake, another
   machine over MCP) reorients perfectly by re-reading the graph.

4. **WORKTREE ISOLATION — every build runs *off* `main`, never *in* it.** Each
   batch's plan / build / fix agent works in its **own git worktree with its cwd
   inside it** (`references/worktree-merge.md`). The shared **primary/session
   checkout is off-limits for builds**. This holds **even at `MAX_PARALLEL=1`** —
   serial makes skipping it look harmless, but a build in the primary checkout
   corrupts `main`, forfeits the disposable-failed-build property, and breaks
   invariant 3's "`git worktree list` is truth". Isolate first, always.

## Deviation protocol — declared, never silent

The tick below is a **reference procedure**: one known-good way to satisfy the invariants,
not the only way. When reality demands it, you may deviate — but a deviation is **declared,
never silent**: say what you're doing differently, **name the invariant that covers the
gap**, and record it (a line in the kickoff manifest when known up front; in the run's
status notes when it arises mid-run). The plan is authoritative, but reality wins — record
the deviation. A deviation you *can't* cover with an invariant isn't a deviation — it's a
stop-and-escalate.

## Operating rules (inherit from the `haven` skill)

Read the `haven` skill's `references/surface-map.md` (CLI⇄MCP) for op detail — don't
restate arguments from memory. The exact op per tick step is in `references/tick-ops.md`;
the git runbook is in `references/worktree-merge.md`; the effort/gate/strike knobs are in
`references/dispatch-policy.md`; the build-agent discipline the gate enforces — fix-log +
3-strike circuit breaker, TDD-as-a-gate, the change-request envelope, batching heuristics, and
human-gated knowledge promotion — is in `references/executor-discipline.md`.

- **Structure only through ops.** Mutate nodes/edges only via `haven …` / `haven_*`.
  The fix-log and any run notes are artifact **content**, never `body`.
- **No batch over MCP** — one entity per `haven_update_item` /
  `haven_complete_item` call; loop. The per-call MCP shape is in
  `references/tick-ops.md`.
- **You never write code.** Every code edit happens inside a spawned build agent's
  worktree. If you find yourself editing a source file directly, stop — that's the
  build agent's job.

## Kickoff — set the run config once, then route your reading

A run starts with a **declared configuration**, never an implicit one. Four dials, set
once per run: **attendance** (is a human watching?), **parallelism posture**
(`MAX_PARALLEL`), **model posture** (session parity vs build-light/verify-heavy), and
**UI-verification ownership** (any UI-acceptance leaves this run, and who gates them).
Dial options, recommended defaults, and the reading router live in
`references/dispatch-policy.md` § KICKOFF.

- **Attended or ambiguous invocation → ask, once.** One `AskUserQuestion` carrying all
  four dials, recommended defaults listed first. Never a second round of questions.
- **Autonomous invocation → declare, don't ask.** Post a short **kickoff manifest** (the
  chosen value per dial + the resulting reading plan) as your opening status message,
  then start.

**Each answer routes your reading.** The config decides which reference sections this
run actually needs — load those, skip the rest (the router table is § KICKOFF). Every
skipped section leaves a **one-line tripwire**: if the run's shape changes mid-flight
(a second worktree goes in flight, a UI-acceptance leaf enters the frontier), stop and
read the skipped section *before* acting on the change. **The four invariants above are
always-read and never routed** — the router trims mechanics, never safety.

## The tick (one stateless pass; loop to convergence)

0. **REORIENT + RECOVER.** Read the whole graph in one call (`haven graph` /
   `haven_graph`); resolve the project first if unknown. Then **reconcile** the
   graph's `in_progress` leaves against `git worktree list`
   (`references/worktree-merge.md`). Do this **before** dispatching anything.
   - An `in_progress` leaf with a live build agent → leave it.
   - An `in_progress` leaf with no live agent is **orphaned**. Three cases:
     - Its worktree holds a clean, gate-passing build → resume at MERGE (step 8).
     - The container has an **approved `build-plan.md`** but the worktree has no
       build commits (it crashed after plan-approval) → resume at **build**
       (step 6c) from that plan. A fresh agent reads it (invariant 3) — don't
       re-plan blind.
     - Otherwise → prune the worktree and send the batch down the failure path.
       The strike count survives in the container's fix-log.
   - A worktree with no `in_progress` leaf is **stale** → prune it.
   - *Large-graph fallback.* Over MCP, `haven_graph` is bounded and reports
     `totals`, `omitted`, `limits`, and `truncated` for nodes, edges, and lineage.
     If `truncated:true` means the graph slice is not enough for this tick,
     **reorient from the frontier** instead: `haven list_items --status
     in_progress --owner ai` for the RECOVER reconcile set, `haven next --owner ai`
     (step 1) for the dispatch queue, then read only each **active container's**
     `context-pack` (steps 2/4). Same tick, a smaller bounded slice — until scoped
     graph reads land (HV-25/HV-195).
1. **FRONTIER.** The AI dispatch queue is exactly `haven next --owner ai`
   (DISPATCHABLE_PREDICATE: committed + `ready` + ≠anchor + `wait_state` NULL + no open
   dependency). This **inherently steps around** human-owned work and AI work blocked by
   an unfinished dependency. Trust the predicate; never re-derive it.
2. **GROUP.** Fold the frontier two ways.
   - **Packed leaves** fold by their derived `context_pack.container` pointer —
     the fold key for already-packed work. A leaf with a `context_pack_clash`
     (more than one packed container) is **skipped and surfaced**, never
     auto-picked.
   - **Packless leaves** have a NULL `context_pack.container`, so they carry no
     fold key. Tentatively cluster them by a **shared `depends_on` producer**
     (the build-time mirror of the planner's foundation node). A packless
     multi-leaf cluster sharing one → **step 4 (packed first)**. A packless leaf
     sharing no producer with others, or a packless **singleton**, → a degenerate
     batch, built directly under the deterministic gate.
   - **Never fold by decomposition parent** — that auto-bundles independent
     siblings.
3. **SELECT.** A batch is dispatchable **now** iff every member is in the ready frontier
   (no member has an open cross-batch dependency — dependent batches simply aren't ready
   yet, so they don't appear). Take up to **MAX_PARALLEL** independent batches — the count
   **you choose for this run** from its coupling risk (`references/dispatch-policy.md`; serial
   when risky or unsure, fan out when disjoint — see *Choosing parallelism* below).
4. **ENSURE-PACKED.** Pack-first is a precondition of CLAIM, never a fallback
   after it.
   - For an **already-packed** batch this is the cheap assertion: the container
     carries a `spec` `context-pack.md` (the pointer guarantees it).
   - For a **ready packless cluster whose members share an architecture** —
     signalled by a shared `depends_on` producer (the build-time mirror of the
     planner's foundation node) and confirmed by `create-context-pack`'s
     shared-context assessment — **pause before claiming any member** and
     **compose `create-context-pack`** on the member-ref set. That skill owns the
     grouping axis: resolving or creating the container, grooming, clash-checking,
     and writing the pack. Then **re-tick** so the members fold by their new
     `context_pack.container` into one batch — and only **then** reach CLAIM.
   - You hand over the member set as a dispatch **hint**. You **never** pre-create
     the container, add grouping edges, or write a pack yourself.
   - `create-context-pack` may return **"simple batch — no pack"** (no shared
     architecture) → those members proceed to CLAIM as ordinary singletons.
   - A packed batch only ever holds **mutually-ready** members — step 3 excludes
     any leaf with an open dependency. So it co-builds independent members that
     share a *brief*, never a dependent with its unmerged foundation; that
     ordering is the **dependency edge's** job, not the pack's. The pack groups
     the batch's shared **context** the verifier reads; it does not change
     dispatch granularity.
5. **CLAIM.** Soft-claim every member of the batch: `haven_update_item {ref,
   status:in_progress}`, one call per leaf. This removes them from `next`, so a re-read
   this same tick won't re-pick them. **Claim before you spawn — never spawn before claim.**
6. **DISPATCH — plan → validate-as-a-batch → build, on fresh agents you load with
   context.** Per batch, create an isolated worktree off `main`. **Spawn each
   agent with its cwd inside it — it edits *there*, never the session checkout**
   (invariant 4; `references/worktree-merge.md`). **You — the coordinator — own
   the full-context handoff.** A spawned agent knows only what its prompt carries
   (`references/dispatch-policy.md` § Dispatch-prompt quality), so **synthesise
   full context into every spawn**. Do not rely on any agent staying alive across
   the gate — in practice the loop spawns fresh at each phase. The plan artifact
   is the **hand-off medium** between phases, not a recovery shadow.
   - **6a Plan.** For a complex/ultracode batch — the plan-gate dial,
     `references/dispatch-policy.md` § PLAN-GATE; a **mechanical** batch skips
     6a/6b and builds directly. Spawn a **read-only plan agent** (BUILD_TIER —
     § MODEL_TIERS) handed: the container's `context-pack.md`
     (`haven_get_artifact {ref:container, role:context-pack}`), the members'
     `done_looks_like`, and the envelope. It **produces a build plan and writes
     it as `build-plan.md` on the container** (`role:scratch` —
     `references/tick-ops.md` § 6). It does **not** modify code. The plan must
     pass the synthesis test *on its own* — rich enough that a fresh builder
     could execute from it plus the pack alone.
   - **6b Plan-gate — validate the tick's plan(s) as a whole, fresh eyes at
     VERIFY_TIER.** Once the plans are in, spawn a **separate** validator over
     the **plans together** — never a plan/build agent; fresh eyes. The whole-set
     view catches cross-batch conflicts, shared-surface collisions, and
     duplication *before* any code is written. Per plan it returns
     **APPROVE / REVISE / REJECT** (criteria: `references/executor-discipline.md`
     § The build plan).
     - **REVISE** → re-spawn a plan agent with the specific gaps plus the prior
       `build-plan.md` to rewrite, then re-gate.
     - **REJECT** (structurally wrong, or needs scope it can't self-grant) →
       Change Request, not a build → failure/replan path, no code written.
     - This AI gate replaces native plan mode's **human** gate on the autonomous
       path.
   - **6c Build.** For each APPROVEd plan, spawn a **fresh** build agent
     (BUILD_TIER) handed the **full context you synthesised**: the
     `context-pack.md`, the members' `done_looks_like`, the **approved
     `build-plan.md` as its primary brief**, and — per leaf — a **2–5 step
     self-check derived from `done_looks_like`** (a green global build is not
     proof a specific leaf's acceptance is met). It builds, runs its self-check,
     reports pass/fail + evidence + any **scope finding**, and never touches the
     graph. If it finds its member list wrong or a dependency missing, it
     **surfaces and returns** (the Change-Request rule); you decide next tick
     whether to re-pack, re-plan, or adjust the batch. A fresh builder isn't
     context-*starved* — it can re-read the code. The approved plan plus your
     synthesis is what keeps re-exploration cheap.
7. **GATE — a fresh verifier, not the builder.** Independence is the point (why:
   `references/dispatch-policy.md` § GATE). Run it **inside** the worktree.
   - *Unattended:* spawn a **separate verifier** given only the leaf's
     `done_looks_like`, the pack's shared requirements, and the diff — never the
     builder's reasoning. **Forward `verify-acceptance`'s contract into its
     prompt, routed by acceptance type** — the verifier inherits no skill
     (`references/dispatch-policy.md` § GATE covers what to forward and why).
     A **code leaf** runs Mode 1: `build + lint + test` (exit-0) + an acceptance
     judgment → **PASS / NEEDS-HUMAN / FAIL** plus evidence. A **UI-acceptance
     leaf** runs Mode 2: the verifier drives the running app and returns a
     four-rung verdict — **only a clean PASS merges** (PASS-WITH-ISSUES does
     not), and the verdict is **invalid without its evidence bundle**
     (per-clause results, screenshots, step transcript), which you file on the
     leaf at COMPLETE — never into the target repo's tree.
   - *Attended:* native plan-mode human approval.
   - The verifier **fixes MINOR (mechanical / deterministic) issues inline** and
     re-runs the suite — not a strike. A **MAJOR** issue it must **not** self-fix:
     it writes a fix plan, and the failure path dispatches a fresh fix agent
     (boundary: `references/executor-discipline.md` § Verifier fixes; when unsure,
     treat as major).
   - It also **captures non-blocking nits** (acceptance met, nothing broken) to
     the container's `punch-list.md` — never held to "eyeball later" — for the
     next checkpoint (§ tick 9; `references/dispatch-policy.md` § CHECKPOINTS).
   - A fail stays in the worktree — nothing merged, siblings untouched → failure
     path (§ below).
8. **MERGE (serialized).** Acquire the single merge lock. `rebase` the batch
   branch onto current `main`. **Re-run the deterministic gate post-rebase**
   (invariant 2); only a green re-gate **fast-forwards** to `main`. On a rebase
   conflict or a red re-gate, do **not** merge — release the lock and send the
   batch to the failure path; `main` and siblings stay clean.
   (`references/worktree-merge.md`.)
9. **COMPLETE + REPLAN.** Only after the work is on `main`:
   `haven_complete_item {ref, evidence}` per leaf — each returns `unblocked[]`.
   When a leaf made a non-obvious integration/contract decision, also append a
   short `delivery`/`decision` artifact on it, so a downstream batch's build
   agent reads it.
   - **Replan check.** If a completion's evidence **contradicts** a downstream
     leaf's `done_looks_like`, or makes it moot, do **not** silently build the
     stale leaf — bounce that branch back to `orchestrate-plan` (the pack's
     "structurally-wrong → re-plan" escape, applied in the *run* loop).
   - Remove the worktree; release the lock.
   - **Checkpoint check — meaty, not every ticket.** If this completion closes a
     **meaty checkpoint** (a whole-track event — `references/tick-ops.md` § 9d —
     never a minor subtask), run the checkpoint cycle: **code review** (a fresh
     lens on the merged diff) **plus the scope's `punch-list.md`(s)** → triage →
     **one batched fix pass** through the normal build → gate → merge
     (re-verified). Cadence, review composition, the severity table, and why
     review ≠ the per-leaf gate: `references/dispatch-policy.md` § CHECKPOINTS.

**Collecting a spawned agent's result (plan § 6a, plan-gate § 6b, build § 6c,
gate § 7).** A spawned agent often signals **idle/complete WITHOUT delivering its
final report**. Treat the idle signal as *"go fetch the report,"* never as the
report itself. After any build, validator, or verifier agent goes idle,
**explicitly retrieve and confirm its structured result**: the build self-check
outcome, the plan-gate APPROVE·REVISE·REJECT, or the PASS·NEEDS-HUMAN·FAIL verdict
plus evidence. Use `SendMessage` to pull it when it didn't arrive. **Never advance
the tick on a missing or empty report** — a silent absent verdict must not be read
as a pass. The loop waits for the *report*, not merely the completion
notification.

Loop to step 0. **Converge** when `haven next --owner ai` is empty **and** nothing is in
flight → **promote any undrained `punch-list.md` items to floating Haven items** (`owner:ai`, low
priority, xref the source leaf) so nothing is lost (`references/tick-ops.md` § Convergence-time),
then **run the post-run audit — the ratchet**: diff what this run actually did against what
this skill prescribes (deviations declared, tripwires fired or missed, failures nothing
covers, places the model outperformed the procedure) and **file the deltas as one floating
research item** on the project (capture, don't structure — `references/tick-ops.md`
§ Convergence-time; nothing-to-report skips the item, never the diff). The ratchet is how
this skill ages: it changes on run evidence, never on speculation. Then report
blocked-on-human items (`next --owner human` / `wait_state on_human`) and any
strike-escalated items, then stop (inline) or sleep (`/loop`, v4).

When you surface progress to a person — mid-run status, a merge-gate pause, the convergence
report — **report it as plain capabilities, not node refs** (the `haven` skill's *Standing
caution*: lead with what a user can now do, before → now; a list of `HV-…` refs is not a
progress report). Inherited rule — see the haven skill, not restated here.

## Failure, retry, escalation

A gate-fail is isolated in the worktree (and again post-rebase in the merge lock). **A minor,
deterministic issue the verifier fixed inline (§ tick 7) is not a fail and not a strike** — the
suite re-confirmed it and the batch proceeds. A **major** fail (behavioral / structural /
acceptance-level) is a *planned* fix, not a patch: the verifier's **fix plan** feeds a **fresh fix
agent** dispatched through the same plan-first pipeline (plan → validate → build → gate, § tick 6),
which reuses the fix-log + strike machinery below. On a major fail:
**append a fix-log entry** (append-only, on the **batch container**); **strikes = fix-log entry
count** (no schema field — `references/executor-discipline.md`). Retry by putting the leaf back on
the frontier (`haven_update_item {status: ready}`; not `reopen`, which resets to discovery). At an
**N-strike ceiling** (default 2–3, `references/dispatch-policy.md`) **stop and escalate**:
`haven_handoff {ref, to:human, wait:on_human}` with the fix-log as the diagnosis — which self-evicts
the batch from `next --owner ai` while the rest of the graph converges. The ceiling is the
**liveness guarantee**: the AI frontier strictly shrinks, so the loop provably converges.

## Choosing parallelism — the coordinator's per-run call

**`MAX_PARALLEL` is your judgment for the run, not a fixed default** — a *speed* dial, never a
correctness one (the serialized merge + re-gate protect `main` at every value, so a wrong choice
costs only wall-clock). Serial (1) when the build is risky or you're unsure; fan out (a conservative
3–4) when the frontier is clearly disjoint and low-blast. Full rule + rationale:
`references/dispatch-policy.md` § MAX_PARALLEL.

**Pack-first is always serial**, whatever `MAX_PARALLEL` you chose: a packless coupled cluster takes
two ticks (compose `create-context-pack`, then dispatch the now-packed batch), never >1 batch in
flight — and because coupled leaves are ordered by their shared foundation's dependency edge, they
never build as separate worktrees over an undecided architecture (invariant 2's seam can't arise).

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
