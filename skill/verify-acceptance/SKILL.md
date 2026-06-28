---
name: verify-acceptance
description: >-
  Verify that a Haven item actually meets its acceptance — give it a target
  ref (a leaf, or a container as a rollup) and it returns a PASS / NEEDS-HUMAN
  / FAIL verdict with evidence, judged against the item's LIVE
  `done_looks_like`. It runs the deterministic suite (build + lint + test) and
  makes an independent acceptance judgment from a fresh perspective — never
  the build agent's. Use when you want to confirm work is done without doing
  it yourself — e.g. "verify HV-41", "is this leaf actually done?", "check
  done_looks_like is met", "give me a pass/fail verdict on this work". It is
  the executor's step-7 gate lifted out to stand alone: orchestrate-run
  COMPOSES this same skill as its verifier, so there is one judgment, not two.
  It does NOT decompose a goal (use `orchestrate-plan`), does NOT write a
  build spec (use `create-context-pack`), does NOT build the code or own the
  human go (native plan mode), and does NOT dispatch/merge/loop the whole
  graph (`orchestrate-run`).
---

# verify-acceptance — the standalone acceptance-verifier (the executor's step-7 gate, lifted out)

Give it a **target ref**; it returns **PASS / NEEDS-HUMAN / FAIL + evidence**, judged
against the item's **live `done_looks_like`**. It clears the **human-as-verifier
bottleneck**: today items pile up on `wait_state on_human` because nothing but a person
can confirm acceptance is met. `verify-acceptance` is that confirmation, made invokable — and
(dialed on, for a leaf) it can complete the leaf itself so verification stops gating on
your attention.

## Compose, don't duplicate — one verifier, two callers

`verify-acceptance` **is** the fresh-agent verifier `orchestrate-run` spawns at step 7 — *not a
second judge*. The contract used to live inline in
`orchestrate-run/references/dispatch-policy.md` + `tick-ops.md`; it now lives **here**,
with exactly two callers:

1. **Ad hoc** — a human/agent says *"verify HV-41"*.
2. **The executor** — `orchestrate-run` composes this skill as its gate.

One verifier, one judgment, one independence guarantee. The skill is **the single
judgment only** — it does **not** own re-invocation, the merge, or strike-counting.
Inside the executor those stay where they belong: `orchestrate-run` owns *when*, *which
worktree*, and *how many times* (the mandatory twice-run post-rebase re-gate, the
serialized merge lock, the N-strike circuit breaker). `verify-acceptance` just returns a verdict.

## Where it sits (the executor family — meet only at the graph)

`orchestrate-plan` (decompose a goal) → `create-context-pack` (spec a group) → native
**plan mode** (build + the human go) → `orchestrate-run` (dispatch/gate/merge/loop). The
**gate** in that pipeline is this skill; `verify-acceptance` lifts it out so the same judgment is
callable against any item, any time, by anyone.

## Operating rules (inherit from the `haven` skill)

Read the `haven` skill's `references/surface-map.md` (CLI⇄MCP) for op detail — don't
restate arguments from memory. The exact call per step is in `references/verify-ops.md`;
the verdict definitions are in `references/verdict-contract.md`; **how the acceptance
judgment is actually made** — the 5-category review checklist, the confidence filter, the
a11y + design-eval lenses, and the `frequency × impact × persistence` severity model — is
in `references/evaluation-lens.md`. The rules that bite here:

- **Structure only through ops; content as files.** A verdict is artifact **content**
  (a `delivery`-role `verdict.md`); status changes go only through `haven …` / `haven_*`.
- **No new verbs or roles.** Everything reuses ops that already exist — `complete_item`,
  `add_artifact`, `handoff`, `haven_graph`. There is **no `verification` role**: PASS /
  verdict evidence rides **`delivery`**, escalation rides **`scratch`** (`fix-log.md`).
- **The yardstick is always the node's LIVE `done_looks_like`**, read per invocation via
  the graph — **never** a frozen copy. Re-grooming can't make a verdict drift.
- **Over MCP there is no sticky session** — pass `project` on every call.

## The independence contract (the load-bearing rule)

Judge acceptance seeing **only**: the target's live `done_looks_like`, the container
context-pack's **shared-requirements** section *when present* (an input, **not** a
precondition), and **the diff**. **Never** the build agent's reasoning, narrative, or
self-check. A same-context reviewer is structurally blind to its own blind spots — **a
verifier carrying the build agent's priors misses exactly what the build agent missed.**
So independence is **by construction**, not by good intentions: it's the whole point, and
deterministic exit-0 alone is only partial cover (test adequacy and "does this actually
meet `done_looks_like`" are judgment calls). When invoked ad hoc, take **target + diff
only** — if a caller pastes the builder's narrative, ignore it.

The independent judgment is **exhaustive, not "probably fine"**: walk **every** acceptance
clause as a yes/no item — no unchecked items, no partial coverage, no failure you noticed
but didn't surface. That exhaustive walk over the live `done_looks_like`, and the lens for
making each call well, are in `references/evaluation-lens.md`.

## The flow

0. **REORIENT.** Read the graph in one call (`haven graph` / `haven_graph`); resolve the
   project first if unknown. This is the only tick state.
1. **RESOLVE THE TARGET.** One ref — a leaf, or a container (a container verdict is a
   **rollup**, always verdict-only). Read its **live `done_looks_like`** and its derived
   `context_pack` pointer (`haven item get <ref> --include edges,artifacts`); if it
   carries a pack, load that container's `context-pack.md` for the **shared-requirements**
   (input, not precondition — a leaf with no pack is verified against its own
   `done_looks_like` alone).
2. **ASSEMBLE THE EVIDENCE BASE.** The **diff** under test — the leaf's branch/worktree,
   or an explicit diff/branch the caller names. Not the builder's reasoning.
3. **RUN THE DETERMINISTIC SUITE.** `build + lint + test`, **exit-0 mandatory**.
   Deterministic-only counting: transient noise is **logged, never counted** toward the
   verdict (`references/verdict-contract.md`).
4. **JUDGE ACCEPTANCE.** Independently decide whether the diff actually satisfies the live
   `done_looks_like` (+ any shared requirements), walking **every** clause exhaustively
   through `references/evaluation-lens.md` (5-category code review + confidence filter;
   the a11y lens for UI leaves; the design-eval checklists + severity model where UX is
   the acceptance). Brownfield: reality-check each `[VERIFY]` claim against the live code.
   Greenfield: treat `[VERIFY]` items as human-locked design.
5. **VERDICT.** **PASS / NEEDS-HUMAN / FAIL + evidence** (`references/verdict-contract.md`).
6. **WRITE — per the dial** (`references/verify-ops.md`):
   - **Verdict-only (Posture A, default):** write a `delivery`-role `verdict.md` on the
     target; touch **no** status. The human/dispatcher completes.
   - **Auto-complete (Posture B, opt-in, leaves only):** on an **unambiguous PASS only**,
     run the existing completion path — `complete_item` (evidence rides `--role delivery`,
     the default) → marks the leaf done, writes immutable evidence, returns `unblocked[]`.
   - **NEEDS-HUMAN / FAIL never auto-complete** (any posture): append a fix-log entry on
     the **container** (`role:scratch`, `fix-log.md`) + `handoff {to:human, wait:on_human}`
     with the verdict + an evidence excerpt — which self-evicts the item from
     `next --owner ai`.

## The auto-complete dial (default OFF) — earn the lever

Verdict-only leaves *you* the blocker — the exact thing this skill exists to clear — so
auto-complete-on-PASS is the value lever (it matters most for the **ad-hoc** caller;
inside the executor, `orchestrate-run` already owns completion at step 9 and just consumes
the verdict). But it trades the human gate, so it is **earned, not assumed**:

- **In v1 the dial is a skill INPUT, default OFF.** There is **no persisted per-project
  trust store yet** (that store is a separate follow-on, HV-100) — so Posture B is a
  deliberate per-call/per-run choice, never a silent default.
- Flip it on per project only after the verifier's PASS verdicts have **demonstrably
  matched human sign-off on a real sample** (prove-before-trust — the discovery gate's
  "default the cheap check, reserve the human" principle, pointed at verification).
- Even then, only an **unambiguous** PASS auto-completes (suite green AND acceptance judged
  met, zero NEEDS-HUMAN flags); anything softer escalates. The one silent failure mode — a
  false PASS landing "done" on an unmet leaf — is bounded behind the explicit opt-in and
  the auditable, reopenable `delivery` evidence.
- **Leaves only.** Auto-complete applies to a leaf; a **container** target is a verdict-
  only rollup.

## Modes — keyed to leaf acceptance

- **Mode 1 — code-level (v1, this skill):** `build + lint + test` (exit-0) + an independent
  acceptance judgment. This is exactly `orchestrate-run`'s current gate — extraction, not
  new capability.
- **Mode 2 — runtime/browser QA (later, NOT in v1):** for a UI leaf, **verify is expected
  to drive the running app** — open it in a real browser (natively, via Claude-in-Chrome),
  exercise the behaviour, and judge acceptance *behaviourally* against the live
  `done_looks_like`, not from the diff alone. This is an **expectation**, not a separate
  tool: a leaf whose `done_looks_like` is a user-facing browser behaviour routes here; a
  code/contract leaf stays in Mode 1. Browser checks use the flake discipline and the
  **PASS-WITH-ISSUES** middle tier in `references/verdict-contract.md`, judged with the
  a11y + design-eval lenses and the severity model in `references/evaluation-lens.md`. The
  retry *engine* is out of v1 scope; the expectation and its contract are stated now.

## Convergence / fresh-session handoff

`verify-acceptance` is **stateless** — its inputs are the live graph and the diff, so a cold session
just re-runs the flow; re-running is idempotent (Posture A overwrites its own `verdict.md`
with `--replace`). v1 ships a manual resume: `/verify-acceptance <ref>`.

## Deferred / not in this skill

Mode 2 (runtime/browser QA); the flake-retry engine (code tests are already deterministic
— transient handling here is log-don't-count, not a retry harness); co-located session /
evidence dirs; dev-server auto-start on an unreachable URL; exploratory checklists (focused
acceptance is the gate); and the **persisted per-project trust-ramp store** for the
auto-complete dial (the dial is a plain input in v1 — the store is HV-100). The executor-
specific machinery the gate sits inside — the twice-run post-rebase re-gate, the serialized
merge lock, strike-counting, MAX_PARALLEL, crash recovery — **stays in `orchestrate-run`**;
this skill is the single judgment, never the loop.
