# Dispatch policy — the per-batch dials

`orchestrate-run` is a **policy/dial layer over native tooling**: it picks *how hard* and
*how gated* each batch runs, and how many run at once. It never reimplements the build or the
review — it dials plan mode, ultracode, and the verifier. The knobs:

## MAX_PARALLEL — how many independent batches run at once

- **Default: 1 (serial).** This version proves the whole machine — happy path, loop, replan,
  failure, recovery — with zero concurrency. Even at 1, MERGE runs the full
  lock→rebase→re-gate→ff path (`references/worktree-merge.md`).
- **> 1 is the one gated step (HV-85).** Raise it only once the serial path holds on a real
  run. Keep it **conservative (3–4)**: the parallel-merge + re-gate seam degrades toward
  serial under heavy hidden code-coupling anyway, and a conservative cap bounds the blast
  radius of the one silent failure mode (a missed re-gate landing broken code on `main`).

## EFFORT — set on the *spawned build agent*, never on yourself

You control what you spawn, not your own effort. Map complexity → effort/model:

| signal | effort |
|---|---|
| mechanical / low-blast (rename, config, small CRUD) | low |
| ordinary feature work | medium (default) |
| complex / novel / high-blast (concurrency, schema, security, cross-cutting) | **ultracode** |

Complexity signal = a node hint if present, else inferred from `done_looks_like` + the pack's
shared requirements. **Retry escalation (policy knob):** on a strike, you *may* bump effort
(e.g. low→ultracode) for the retry — set this default per run; conservative default is to hold
effort on the first retry and bump on the second.

## GATE — how a batch is judged before merge

- **Unattended (default for autonomous runs): a fresh VERIFIER AGENT.** Spawn a *separate*
  agent — **never the build agent** — given **only**: the leaf's `done_looks_like`, the pack's
  shared-requirements section, and the diff. It must **not** see the build agent's reasoning or
  worktree narrative. It runs `build + lint + test` (exit 0) **and** judges whether the diff
  actually satisfies the acceptance, returning `pass/fail + evidence`. A same-context reviewer
  is structurally blind to its own blind spots; the verifier's independence by construction is
  the load-bearing quality guarantee, and deterministic exit-0 alone is only partial cover
  (test adequacy + "does this meet `done_looks_like`" are judgment calls).
- **Attended: native plan-mode human approval** per complex batch. Use when a person is driving
  and the batch warrants a human "go".

> The verifier (or the human) runs **twice** for any merged batch: once in-worktree (step 7)
> and again post-rebase inside the merge lock (step 8). The post-rebase run is non-negotiable.

## The build agent's self-check (shifts acceptance left)

When you dispatch a leaf, derive **2–5 concrete, executable checks** from its `done_looks_like`
+ the pack's shared requirements, and put them in the build-agent prompt as a self-check it must
pass *before* signalling done. A repo-wide green build does not prove a specific leaf's
acceptance — many leaves have acceptance no global test covers. The verifier then re-runs the
deterministic subset independently. For a **behavioral** code leaf, default to **TDD** (write
the failing test from the acceptance first; include the red→green transition in the evidence) —
on for ultracode/complex, optional for mechanical.

## The build agent's envelope (Change-Request rule)

The build agent is handed an **explicit member list** — its envelope. It may **not** write the
graph and may **not** expand scope. If it discovers in-worktree that its member list is wrong, a
dependency is missing, or scope must grow, it **surfaces the finding** (in its result / a scratch
note) and **returns** — it must never silently overreach (poisoning the merge) or silently stall.
You, the single orchestrator, decide on the next tick whether to re-pack, re-plan, or adjust the
member list.

## STRIKES — the fix-log circuit breaker

On a gate-fail, append a fix-log entry to the **batch container** (`role:scratch`,
`fix-log.md`); the strike count is **derived by counting entries** (no schema field). At **N
strikes (default 2–3)**, stop retrying and `handoff` the batch to a human (`wait:on_human`),
which self-evicts it from `next --owner ai`. The N-strike ceiling is the **liveness guarantee**:
the AI frontier strictly shrinks, so the loop provably converges — no batch retries forever.

## Unattended ⇒ deterministic gate only

When there is no human to approve and no human to escalate to in real time, the gate **must** be
the deterministic verifier (never plan-mode approval); escalation still parks the batch on
`wait_state on_human` and the loop reports it and continues. Convergence is always reachable:
every batch terminates as merged-and-done **or** parked-on-human-after-N-strikes.
