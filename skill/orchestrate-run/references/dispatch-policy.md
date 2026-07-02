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
  speed, so the safe choice under doubt is the slow one. **Shared mutable infrastructure**
  (a local DB/stack, emulator, or dev server every stream resets or migrates) serializes those
  spans regardless of this dial — lock it per resource, see `references/worktree-merge.md`
  § Shared mutable infrastructure.
- **Fan out when the frontier is clearly disjoint and low-blast.** Items in separate
  crates/modules, additive or mechanical work, no shared files — the case the re-gate almost
  never fires on.
- **Keep the ceiling conservative (3–4)** regardless. The merge queue serializes anyway, so
  wider buys little, and a low cap bounds the blast radius of the one silent failure mode (a
  missed re-gate landing broken code on `main`). The human can always override the posture in
  plain language when kicking off the run ("this one's touchy, keep it serial" / "these are
  independent, run them together"). (Serial-first was the bring-up posture; HV-84/85 proved the
  full machine incl. the parallel seam, so the dial is now open.)

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

- **BUILD_TIER** — the **plan agent** (6a) and the **build agent** (6c), each spawned fresh.
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

A **read-only plan agent plans first** (6a) and a fresh validator approves that plan **before any
code is written** (6b) — catching a wrong approach before it costs a full build + a failed § GATE.
Same on/off rule as TDD:

- **On for complex / ultracode batches** (novel, cross-cutting, schema / security / concurrency).
- **Optional for mechanical batches** — a rename / config edit has no approach worth gating; it
  skips 6a/6b and builds directly (the degenerate path).

The validator is **fresh eyes at VERIFY_TIER — never a plan/build agent** (a same-context reviewer
is structurally blind, exactly as for the code gate), and it judges the tick's **plans as a whole**
so cross-batch conflicts surface before any building. Verdicts **APPROVE / REVISE / REJECT**; the
plan-validation criteria (covers every acceptance clause, stays in envelope, sequences TDD, is
concrete) are in `references/executor-discipline.md` § The build plan. Plan and build are **separate
fresh spawns** — the approved `build-plan.md` is the builder's brief, and the coordinator carries
full context into the build spawn (§ Dispatch-prompt quality); the loop does not keep one agent
alive across the gate. This is the **AI** gate that replaces native plan mode's **human** gate on the
autonomous path — the real backstop is still the post-build verifier (§ GATE).

## GATE — how a batch is judged before merge

- **Unattended (default for autonomous runs): compose the `verify-acceptance` skill.** The gate **is**
  `verify-acceptance` (Mode 1) — a fresh verifier given **only** the leaf's `done_looks_like` + the
  pack's shared-requirements + the diff (never the build agent's reasoning or worktree narrative),
  running `build + lint + test` (exit-0) + an independent acceptance judgment, returning a
  **PASS / NEEDS-HUMAN / FAIL** verdict + evidence. **The verifier inherits no skill, so FORWARD the
  contract into its prompt** — read `skill/verify-acceptance` (`SKILL.md` + `references/verdict-contract.md`
  + `references/evaluation-lens.md`) and inline it; naming the skill reaches nothing. **Trim what you
  forward to the leaf:** for a code leaf, inline the Mode-1 material only — the independence contract,
  the verdict definitions, and the lens's code sections (exhaustive walk, 5-category checklist,
  confidence filter, severity) — and skip the a11y + design-eval lenses unless the leaf's acceptance
  is user-facing UI; **never** forward `browser-mode.md` for a code leaf (UI routing lands with
  HV-262). Only **PASS**
  merges; a **FAIL** keeps the batch in the worktree → failure path (STRIKES below); a **NEEDS-HUMAN**
  escalates straight to `handoff` (ambiguity won't clear on a blind retry). The verifier's
  independence by construction is the load-bearing quality guarantee — deterministic exit-0 alone is
  only partial cover ("does this meet `done_looks_like`" is a judgment call).
- **Attended: native plan-mode human approval** per complex batch. Use when a person is driving
  and the batch warrants a human "go".

> The verifier (or the human) runs **twice** for any merged batch: once in-worktree (step 7) and
> again post-rebase inside the merge lock (step 8). The post-rebase run is non-negotiable — *when*,
> *which worktree*, and *how many times* the gate runs is the **executor's**, not the skill's.

**The verifier fixes minors, not majors.** A *mechanical / deterministic* problem (fmt, lint, a
missing import) the verifier fixes inline and re-runs the suite — the suite, not its opinion,
confirms the fix, so independence holds and it is **not a strike**. A *behavioral / structural /
acceptance-level* problem it does **not** self-fix (fixing then re-judging its own change is the
blindness the gate prevents): it writes a fix plan and **FAILs**, and the failure path dispatches a
fresh fix agent through the plan-first pipeline. The exact boundary is in
`references/executor-discipline.md` § Verifier fixes. When unsure, treat it as major.

## CHECKPOINTS — code review + drain, at meaty boundaries (not every ticket)

**Verification ≠ review.** The per-leaf gate (§ GATE) asks *"does it meet acceptance?"* and blocks
each merge. **Code review** asks *"is it good code?"* — design, maintainability, and the latent bugs
the suite misses. Review is a **finding source, not a second gate**: its findings route into the
same severity tiers, and its fixes go through the one merge gate.

**Cadence — meaty, the coarse default.** Run a checkpoint only at a *cohesive completed chunk*: a
container `rollup_state:Done`, an integration boundary (`references/executor-discipline.md`
§ Batching), a foundation merged, or a size threshold (≥N merged leaves / ≥N accumulated punch-list
items). **Never after every ticket** — thin slices waste agents and hide design-level findings.
Placement is your per-run judgment, like MAX_PARALLEL; bias coarse.

**At a checkpoint** spawn a **fresh review agent** (never a builder/verifier of this work; VERIFY_TIER)
that runs the repo's **`/code-review`** over the checkpoint's **merged diff** (post-merge — correctness
is already gated per-leaf; review lifts quality on the integrated result), forwarded since a subagent
inherits no skill. Aggregate its findings with the scope's `punch-list.md`(s) and drain them
(`references/executor-discipline.md` § The punch-list & checkpoint drain).

**Severity triage** (review findings + punch-list, one table):

| finding | route |
|---|---|
| real bug / acceptance-violating | **major** fix path (fresh fix agent, plan-first) + flag the suite was inadequate |
| mechanical (style, naming) | inline, or folded into the batched fix |
| non-blocking nit | the batched drain |

Capture, drain mechanics, survivor-promotion, and the recursion bound are in
`references/executor-discipline.md` § The punch-list & checkpoint drain.

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
