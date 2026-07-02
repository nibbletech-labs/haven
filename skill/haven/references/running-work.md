# Running Haven work — the executor, or yourself

There is **one real fork**: hand an already-planned graph to **`orchestrate-run`** (the
autonomous executor), or **build it yourself** directly. Everything else — plan first or not,
use native plan mode on a single feature or not, verify or not — is free-form composition on the
*yourself* side, not a separate mode. The supporting skills (`orchestrate-plan`,
`create-context-pack`, `verify-acceptance`) compose into **either** side.

## The fork

- **Direct** — you (the main agent) build it, in this thread. Optionally decompose first
  (`orchestrate-plan`), spec a batch (`create-context-pack`), and check the result (`verify-acceptance`).
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
a long multi-leaf run scales (plus speed when it fans out disjoint work in parallel — below). For a handful of tasks,
direct is usually the better call.

## What the executor is — and isn't — today

- **Serial or parallel — the coordinator's per-run call** (`MAX_PARALLEL`). It runs one leaf at a
  time (Build → Verify → merge → complete → next) by default, and can fan out several independent
  builds at once when the ready frontier is clearly disjoint and low-blast. It's a *speed* choice,
  not a correctness one — the serialized merge + post-rebase re-gate protects `main` either way, so
  serial is just the safe fallback when the build is risky or unclear (the full risk rule lives in
  `orchestrate-run`'s `references/dispatch-policy.md`; HV-84/85 proved the machine, HV-241 opened
  the dial).
- **Workers default to session parity — with an opt-in tier.** By default a Build/Verify subagent
  inherits the **same model and effort** as the orchestrating session — no *silent* downgrade
  (**HV-167**). A run may **opt into asymmetric tiering** at kickoff (a lighter build/plan agent, a
  heavier validator), under one guardrail: the **verifier tier is never below the builder tier**, so
  the judgment is never the thing downgraded (**HV-242** amends HV-167 — see `orchestrate-run`'s
  `references/dispatch-policy.md` § MODEL_TIERS).
- **Two different verifications — don't conflate them:**
  - **Code** — `build + lint + test` green, and "does the diff meet `done_looks_like`." That's the
    `verify-acceptance` skill (Mode 1), and it's the executor's per-leaf gate. The AI does this.
  - **Functionality** — does the built thing actually *work* in use (front-end / runtime). The AI
    version is `verify-acceptance` **Mode 2** (browser/runtime QA, **HV-139**) — invoke it ad hoc
    on a UI leaf with a reachable `dev_url` and it drives the running app. It also takes the
    **unattended executor gate** on UI-acceptance leaves (**HV-262**): only a clean PASS merges,
    and every verdict must ship an evidence bundle (screenshots, steps, per-clause results) filed
    on the leaf — trust is post-hoc audit of that evidence, not a prior proving run. The
    trust-ramp for auto-complete (**HV-100**) still lands separately.
- **Entry escape.** When `orchestrate-run` fires it first checks the executor is actually the right
  call for this work and checks with you before spawning, rather than running the full loop blindly
  (**HV-168**). So an eager trigger is fine — a wrong fire gets caught at the door.

## Picking

Few leaves, or quality-critical → **direct**. Many leaves that would blow the main context →
**executor** (serial, or parallel when the frontier is disjoint — its call per run). When it's
ambiguous, the executor asks rather than assuming.
