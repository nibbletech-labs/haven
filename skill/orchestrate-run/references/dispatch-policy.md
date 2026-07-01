# Dispatch policy — the per-batch dials

`orchestrate-run` is a **policy/dial layer over native tooling**: it picks *how hard* and
*how gated* each batch runs, and how many run at once. It never reimplements the build or the
review — it dials plan mode, ultracode, and the verifier. The knobs:

## MAX_PARALLEL — how many independent batches run at once

**You choose this per run, from the build's coupling risk — it is not a fixed default.**
Parallelism is a *speed* dial, never a correctness one: the serialized merge + mandatory
post-rebase re-gate protects `main` at any value (`references/worktree-merge.md`), so the
worst case of choosing wrong is a slower run, never a broken one. That is what makes the
choice safe to make per run instead of pinning it. Even at 1, MERGE runs the full
lock→rebase→re-gate→ff path.

- **Stay serial (1) when the build is risky.** The ready items likely touch overlapping code,
  or involve schema/migrations, concurrency, security, or cross-cutting refactors — anywhere
  hidden coupling makes the re-gate the only thing between you and broken `main`. Don't stack
  that seam under concurrency. **When unsure, this is the default** — parallelism is pure
  speed, so the safe choice under doubt is the slow one.
- **Fan out when the frontier is clearly disjoint and low-blast.** Items in separate
  crates/modules, additive or mechanical work, no shared files — the case the re-gate almost
  never fires on.
- **Keep the ceiling conservative (3–4)** regardless. The merge queue serializes anyway, so
  wider buys little, and a low cap bounds the blast radius of the one silent failure mode (a
  missed re-gate landing broken code on `main`). The human can always override the posture in
  plain language when kicking off the run ("this one's touchy, keep it serial" / "these are
  independent, run them together").

> Serial-first was the *bring-up* posture (HV-84/85): prove the whole machine — happy path,
> loop, replan, failure, recovery, **and** the parallel merge/re-gate seam — before trusting
> fan-out. That proving run is done (HV-85), so the dial is now the coordinator's judgment,
> not a pinned `1`.

## EFFORT — set on the *spawned build agent*, never on yourself

You control what you spawn, not your own effort. Map complexity → effort (the *model* axis is
§ MODEL_TIERS below):

| signal | effort |
|---|---|
| mechanical / low-blast (rename, config, small CRUD) | low |
| ordinary feature work | medium (default) |
| complex / novel / high-blast (concurrency, schema, security, cross-cutting) | **ultracode** |

Complexity signal = a node hint if present, else inferred from `done_looks_like` + the pack's
shared requirements. **Retry escalation (policy knob):** on a strike, you *may* bump effort
(e.g. low→ultracode) for the retry — set this default per run; conservative default is to hold
effort on the first retry and bump on the second.

## MODEL_TIERS — which model runs the build vs the verifiers

Two tiers, set at kickoff:

- **BUILD_TIER** — the one plan+code agent (tick steps 6a/6c).
- **VERIFY_TIER** — the fresh validators: the **plan-gate** (6b) and the **code verifier** (7).

**Default: session parity.** Both tiers inherit the orchestrating session's model and effort — no
separate dial, no *silent* downgrade (**HV-167**; the `haven` skill's `references/running-work.md`).

**Opt-in asymmetric tiering** — a human sets it in plain language when kicking off the run ("build
light, verify heavy"): BUILD_TIER may run a **lighter** model than VERIFY_TIER, spending the heavier
model where the leverage is (the judgment), not on generation. Under **one guardrail, inviolable:**

> **VERIFY_TIER ≥ BUILD_TIER, always.** The verifier is never below the builder, so the *judgment*
> is never the thing downgraded — only the generation is. That is what keeps asymmetry compatible
> with HV-167's fear (a *silent weakening of the gate*) instead of a reversal of it.

This **amends HV-167** (which read "no separate dial"); the amendment + rationale is recorded on
**HV-242**. Effort still maps per § EFFORT — MODEL_TIERS is the orthogonal *model* axis.

## PLAN-GATE — validate the approach before any code (tick steps 6a/6b)

A build agent **plans first** and has that plan validated **before it writes code** — catching a
wrong approach before it costs a full build + a failed § GATE. Same on/off rule as TDD:

- **On for complex / ultracode batches** (novel, cross-cutting, schema / security / concurrency).
- **Optional for mechanical batches** — a rename / config edit has no approach worth gating; it
  skips 6a/6b and builds directly (the degenerate path).

The validator is **fresh eyes at VERIFY_TIER — never the build agent** (a same-context reviewer is
structurally blind, exactly as for the code gate). Verdicts **APPROVE / REVISE / REJECT**; the
plan-validation criteria (covers every acceptance clause, stays in envelope, sequences TDD, is
concrete) are in `references/executor-discipline.md` § The build plan. This is the **AI** gate that
replaces native plan mode's **human** gate on the autonomous path — the plan is read-only *by
instruction*, and the real backstop is still the post-build verifier (§ GATE).

## GATE — how a batch is judged before merge

- **Unattended (default for autonomous runs): compose the `verify-acceptance` skill.** The gate **is** the
  standalone `verify-acceptance` skill (Mode 1) — a fresh verifier agent given **only** the leaf's
  `done_looks_like` + the pack's shared-requirements + the diff (never the build agent's reasoning
  or worktree narrative), running `build + lint + test` (exit-0) + an independent acceptance
  judgment, and returning a **PASS / NEEDS-HUMAN / FAIL** verdict + evidence. **Read `skill/verify-acceptance`
  (`SKILL.md` + `references/verdict-contract.md` + `references/evaluation-lens.md`) for the contract
  and FORWARD it into the verifier's prompt** — the verifier is a spawned subagent that inherits no
  skill (§ Dispatch-prompt quality below), so naming the skill reaches nothing; *"do not restate it
  here"* means don't duplicate the contract in **this** doc, **not** withhold it from the verifier.
  The executor **consumes** the verdict — it forwards `verify-acceptance`'s contract verbatim and never
  re-implements the judgment. Only **PASS** merges; a
  **FAIL** keeps the batch in the worktree → failure path (STRIKES below); a **NEEDS-HUMAN**
  escalates straight to `handoff` (ambiguity won't clear on a blind retry). The verifier's
  independence by construction is the load-bearing quality guarantee — deterministic exit-0 alone
  is only partial cover (test adequacy + "does this meet `done_looks_like`" are judgment calls).
- **Attended: native plan-mode human approval** per complex batch. Use when a person is driving
  and the batch warrants a human "go".

> The verifier (or the human) runs **twice** for any merged batch: once in-worktree (step 7) and
> again post-rebase inside the merge lock (step 8). The post-rebase run is non-negotiable — *when*,
> *which worktree*, and *how many times* the gate runs is the **executor's**, not the skill's.

## The build agent's self-check (shifts acceptance left)

When you dispatch a leaf, derive **2–5 concrete, executable checks** from its `done_looks_like`
+ the pack's shared requirements, and put them in the build-agent prompt as a self-check it must
pass *before* signalling done. A repo-wide green build does not prove a specific leaf's
acceptance — many leaves have acceptance no global test covers. The verifier then re-runs the
deterministic subset independently. For a **behavioral** code leaf, default to **TDD** (write
the failing test from the acceptance first; include the red→green transition in the evidence) —
on for ultracode/complex, optional for mechanical. **TDD here is a gate, not a style** — the
RED-before-GREEN sequence and the binary sign-off item *"a failing test was written before the
implementation"* are spelled out in `references/executor-discipline.md` § TDD as a gate.

## The build agent's envelope (Change-Request rule)

The build agent is handed an **explicit member list** — its envelope. It may **not** write the
graph and may **not** expand scope. If it discovers in-worktree that its member list is wrong, a
dependency is missing, or scope must grow, it **surfaces the finding** (in its result / a scratch
note) and **returns** — it must never silently overreach (poisoning the merge) or silently stall.
You, the single orchestrator, decide on the next tick whether to re-pack, re-plan, or adjust the
member list. The full **Change-Request envelope** — the trigger conditions and the
Reason/Requested/Impact form the agent surfaces, then *waits, no assumed approval* — is in
`references/executor-discipline.md` § The change-request envelope.

## STRIKES — the fix-log circuit breaker

On a gate-fail, append a fix-log entry to the **batch container** (`role:scratch`,
`fix-log.md`); the strike count is **derived by counting entries** (no schema field). At **N
strikes (default 2–3)**, stop retrying and `handoff` the batch to a human (`wait:on_human`),
which self-evicts it from `next --owner ai`. The N-strike ceiling is the **liveness guarantee**:
the AI frontier strictly shrinks, so the loop provably converges — no batch retries forever. The
fix-log discipline this counts (append-only on the container, fresh fixer reads-prior-attempts +
tries-something-different, log BEFORE and AFTER, the entry format, and the 3-strike-**per-
acceptance-id** breaker that escalates *with the fix-log path*) is in
`references/executor-discipline.md` § The fix-log / § The 3-strike circuit breaker.

## Unattended ⇒ deterministic gate only

When there is no human to approve and no human to escalate to in real time, the gate **must** be
the deterministic verifier (never plan-mode approval); escalation still parks the batch on
`wait_state on_human` and the loop reports it and continues. Convergence is always reachable:
every batch terminates as merged-and-done **or** parked-on-human-after-N-strikes.

## Dispatch-prompt quality — the synthesis test (the most load-bearing dial)

The build agent you spawn **does not inherit any skill** and does not share your context — it
sees only the prompt you hand it. So the *quality of that forwarded prompt IS the product*; a
thin prompt is a thin build, and no gate downstream can recover understanding you failed to
synthesise upstream. Every dispatch prompt must **prove you understood the work before handing it
off**. Never delegate understanding.

- **Bad:** "Based on the research, implement the auth feature."
- **Bad:** "Look at the codebase and figure out how to add caching."
- **Good:** "Implement JWT auth middleware. The Express app at `src/app.ts` uses
  `express.Router()` (line 34). Auth routes stubbed at `src/routes/auth.ts`. User model at
  `src/models/user.ts` has `passwordHash` and `email`. Use `jsonwebtoken` (in package.json).
  Write middleware to `src/middleware/auth.ts`, integrate at `src/routes/index.ts:15`."

> **The test:** if you removed the agent's ability to explore the codebase, could it still make
> meaningful progress from your prompt alone? If not, you haven't done enough synthesis.

Apply it to *every* spawn — the build agent, the verifier, the fixer. The pack's `context-pack.md`
is most of this synthesis pre-done; your job is to forward it whole plus the leaf-specific edges,
not to gesture at it.

## Don't peek, don't race

Once a batch is dispatched, **do not read the agent's working files mid-flight**, and **do not
predict or fabricate its results**. Wait for completion — then **explicitly retrieve and confirm
the agent's report before processing it**: an idle/completion signal is **not** the report (agents
frequently go idle without delivering one), so `SendMessage` to pull the structured result and
**never advance on an absent or empty verdict** (a silent missing verdict must not read as a pass).
Peeking tempts you to act on a half-written state (which the stateless reorient does not model)
and racing tempts you to invent a verdict the verifier hasn't returned — both poison the one
truth the loop trusts. The graph's `in_progress` status, the worktree, and the done-marker are
the only mid-flight signals you read (`references/worktree-merge.md` § RECOVER); the agent's
scratch is *its* working memory, not yours.
