# Running Haven work — the executor, or yourself

There is **one real fork**: hand an already-planned graph to **`orchestrate-run`** (the
autonomous executor), or **build it yourself** directly. Everything else — plan first or not,
use a plan-mode gate on a single feature or not, verify or not — is free-form composition on the
*yourself* side, not a separate mode. The supporting skills (`plan-item`, `orchestrate-plan`,
`create-context-pack`, `verify-acceptance`) compose into **either** side.

## Planning grain — settle this before you run anything

Planning is a **sequence, not a menu**. **Every plan starts on one item, in `plan-item`** —
a one-line fix and a whole product launch alike. Only once there's a plan to look at do you
ask the scale question, and that question has one form: **could a single build pass deliver
this against one spec?**

- **Yes** → the plan gets its build checklist, the item goes `ready`, and it's built. This is
  the ordinary case. **Chunky is fine** — a multi-stage plan sits happily on one ticket and
  `done_looks_like` can carry several criteria. Size is not the test and neither is criteria
  count.
- **No** → hand that ref to **`orchestrate-plan`**, which roots there and decomposes the plan
  you just wrote. Only three things make it a "no": a **gate in the middle** (something must
  be decided or produced before the rest can even be shaped), **split ownership** (part is
  real-world human work), or it **won't survive one pass**. Decomposition is the second
  stage, never the entry point — fragmenting costs handoffs and lost context.
- Then **`plan-item` again per leaf**, and **`create-context-pack`** where several leaves are
  about to be built together and want one shared brief.

**`orchestrate-plan` requires a plan** — it decomposes a ref that already carries a `spec`
and never starts from a bare goal or a title. A greenfield "build the whole thing from
scratch" is not an exception: it gets a high-level plan on one item first, and *that* is what
gets broken down. The point of the ordering is that the decision to decompose is made by a
person looking at a plan, not guessed at from a sentence.

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
  `orchestrate-run`'s `references/dispatch-policy.md`; live runs proved the machine and opened
  the dial).
- **Workers default to session parity — with an opt-in tier.** By default a Build/Verify subagent
  inherits the **same model and effort** as the orchestrating session — no *silent* downgrade.
  A run may **opt into asymmetric tiering** at kickoff (a lighter build/plan agent, a
  heavier validator), under one guardrail: the **verifier tier is never below the builder tier**, so
  the judgment is never the thing downgraded (see `orchestrate-run`'s
  `references/dispatch-policy.md` § MODEL_TIERS).
- **Two different verifications — don't conflate them:**
  - **Code** — `build + lint + test` green, and "does the diff meet `done_looks_like`." That's the
    `verify-acceptance` skill (Mode 1), and it's the executor's per-leaf gate. The AI does this.
  - **Functionality** — does the built thing actually *work* in use (front-end / runtime). The AI
    version is `verify-acceptance` **Mode 2** (browser/runtime QA) — invoke it ad hoc
    on a UI leaf with a reachable `dev_url` and it drives the running app. It also takes the
    **unattended executor gate** on UI-acceptance leaves: only a clean PASS merges,
    and every verdict must ship an evidence bundle (screenshots, steps, per-clause results) filed
    on the leaf — trust is post-hoc audit of that evidence, not a prior proving run. The
    trust-ramp for auto-complete still lands separately.
- **Entry escape.** When `orchestrate-run` fires it first checks the executor is actually the right
  call for this work and checks with you before spawning, rather than running the full loop
  blindly. So an eager trigger is fine — a wrong fire gets caught at the door.

## Picking

Few leaves, or quality-critical → **direct**. Many leaves that would blow the main context →
**executor** (serial, or parallel when the frontier is disjoint — its call per run). When it's
ambiguous, the executor asks rather than assuming.
