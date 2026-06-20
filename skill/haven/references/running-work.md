# Running Haven work — the executor, or yourself

There is **one real fork**: hand an already-planned graph to **`orchestrate-run`** (the
autonomous executor), or **build it yourself** directly. Everything else — plan first or not,
use native plan mode on a single feature or not, verify or not — is free-form composition on the
*yourself* side, not a separate mode. The supporting skills (`orchestrate-plan`,
`create-context-pack`, `verify`) compose into **either** side.

## The fork

- **Direct** — you (the main agent) build it, in this thread. Optionally decompose first
  (`orchestrate-plan`), spec a batch (`create-context-pack`), and check the result (`verify`).
  **Best for:** one task or a handful; you want the highest quality and your own eyes on it; the
  work fits the main context. **Enter:** just do the work; pull in the planning/spec/verify skills
  as needed.

- **Executor — `orchestrate-run`** — the main session becomes a **conductor**. Per leaf it makes a
  git worktree, spawns a **Build** subagent, gates it with a **separate Verify** subagent (fresh
  eyes — never the builder), merges to `main`, completes the leaf (unblocking downstream), and
  loops the Haven ready-frontier to convergence. **Best for:** many leaves, where running the
  build inline would blow the main context, and you want it driven end-to-end with a gate on each.
  **Needs:** an already-planned graph — so plan first (`orchestrate-plan` + `create-context-pack`)
  if there isn't one. **Enter:** invoke/compose the `orchestrate-run` skill.

The only reason to reach for the executor is **context isolation** — the conductor stays clean, so
a long multi-leaf run scales (plus speed, once parallel is on — below). For a handful of tasks,
direct is usually the better call.

## What the executor is — and isn't — today

- **Serial.** `MAX_PARALLEL=1` by design: one leaf at a time (Build → Verify → merge → complete →
  next). It does **not** fan out multiple builds at once yet — parallel fan-out is the gated next
  step (**HV-85**, under epic HV-83, behind the proven serial core HV-84).
- **Workers run at session parity.** A Build/Verify subagent inherits the **same model and the same
  effort** as the orchestrating session — no separate dial, no silent downgrade (**HV-167**).
- **Two different verifications — don't conflate them:**
  - **Code** — `build + lint + test` green, and "does the diff meet `done_looks_like`." That's the
    `verify` skill (Mode 1), and it's the executor's per-leaf gate. The AI does this.
  - **Functionality** — does the built thing actually *work* in use (front-end / runtime). That's a
    **human** check today (you verify before it's trusted to land). The AI version is `verify`
    **Mode 2** (browser/runtime QA) — not built yet (**HV-139**); **HV-100** is the trust-ramp for
    when it can stand in. The executor gates **code**, not functionality — functionality is still
    yours.
- **Entry escape.** When `orchestrate-run` fires it first checks the executor is actually the right
  call for this work and checks with you before spawning, rather than running the full loop blindly
  (**HV-168**). So an eager trigger is fine — a wrong fire gets caught at the door.

## Picking

Few leaves, or quality-critical → **direct**. Many leaves that would blow the main context →
**executor** (serial today). When it's ambiguous, the executor asks rather than assuming.
