# Mode 2 (browser) — drive the running app, judge acceptance behaviourally

This is the **live Mode-2 contract**. When a leaf's acceptance is user-facing runtime
behaviour, `verify-acceptance` doesn't judge from the diff alone — it **drives the running
app** and judges what the app actually does against the live `done_looks_like`. Mode 2 is
**live for ad-hoc / attended use** (routed to automatically by acceptance type — see
SKILL.md § Modes); the `orchestrate-run` gate-wiring lands separately (HV-262), so a code
leaf is still never routed here.

The contract is written **platform-neutral** — "drive the running app" — because the same
judgment will drive an iOS simulator later (HV-263). **The browser is v1's only driver.**

## Routing — which clauses come here

In the skill's flow (step 1), split the target's live `done_looks_like` clause-by-clause:

- A clause describing **user-facing runtime behaviour** — see / click / navigate / render /
  focus / type / a visible state change — routes to **Mode 2** and is judged against the
  running app.
- A **pure code / contract** clause (a function signature, a migration, an exit code, a file
  that must exist) stays in **Mode 1** and is judged from the diff + deterministic suite.
- **Mixed leaves run both suites.** Each clause is judged in its own mode; the results roll
  up into one verdict. **Any Mode-1 FAIL dominates** — a broken build or an unmet code clause
  is a FAIL regardless of how the browser checks land (the deterministic suite is a hard
  gate, browser behaviour can't paper over it).

## Ingestion — what the browser verifier reads

Assemble the evidence base per invocation (never a frozen copy):

- **The live `done_looks_like`** of the target ref (read via `haven_graph {include_acceptance:true}`
  or `haven_get_item`) — the clauses routed to Mode 2 are the yardstick.
- **The container context-pack's shared-requirements** when the leaf carries a pack pointer
  (an input, not a precondition — a leaf with no pack is judged on its own `done_looks_like`).
- **Pack scenarios filtered to `verification-approach` `e2e` | `visual`** — the runtime
  scenarios the pack authored are the behavioural checklist; approaches that aren't runtime
  (unit, contract) are not Mode-2 material.
- **`design-spec.json` when present.** When a leaf/pack carries the machine-readable design
  contract, it is the **per-component checklist** the browser verifier walks (states,
  interactions, accessibility_contract per component). **Halt-on-missing-required-field
  applies**: if a component is missing a required field (`name`, `states`, `interactions`,
  `accessibility_contract`), halt with `ERROR: design-spec.json missing required field <path>`
  — an incomplete contract stops verification rather than being silently filled in. **Absence
  is NOT an error**: when no `design-spec.json` is present, the prose `done_looks_like` is the
  whole yardstick.
- **`dev_url` — resolution order: caller > leaf spec > pack section 3.** The caller's explicit
  arg wins; else read it from the leaf's spec; else from the pack's section 3. Mode 2 does
  **not** auto-start a dev server (that's a later leaf) — it expects a reachable URL. **An
  unreachable `dev_url` → NEEDS-HUMAN**, naming the URL and the failure (e.g. "dev_url
  http://localhost:3000 unreachable: connection refused"), never a FAIL — an environment the
  verifier can't stand up is an escalation, not an acceptance gap.

## Driver — how the app is driven

- **`agent-browser` CLI, headless by default** — the driver for unattended and default runs.
- **Claude-in-Chrome when attended** — when a human is driving and a real Chrome session is
  available, drive there.
- The **judgment is identical** across drivers; only the transport differs. Write checks
  against observable app behaviour, not against a specific driver's API.
- **Never trigger a blocking browser dialog during a check.** `alert` / `confirm` / `prompt`
  hang the driver. When acceptance involves a dialog, **verify it via console capture** (assert
  the app logged / would-have-called it) rather than letting the dialog block the run.

## Judge + evidence

Walk **every** routed clause exhaustively (no unchecked items) through the evaluation-lens
(`references/evaluation-lens.md`): per-check **PASS / CONCERN / FAIL** + **severity**
(`frequency × impact × persistence`). Apply the **a11y lens** and the **design-eval lenses**
where the acceptance is UX. Then roll up per the four-rung ladder below.

Capture evidence as you go:

- **Screenshot every checked state** — the state each clause asserts, captured at the moment
  it's judged, is the primary evidence.
- **On failure, capture console + network** — attach the console messages and network
  requests for any check that fails, so the failure is diagnosable without a re-run.
- Present the result as a **per-check PASS / CONCERN / FAIL table**, rolled up **per
  evaluation-lens** (a11y, design-eval, behavioural), then to the single verdict.

## The four-rung ladder (browser verdicts only)

Browser / runtime QA is the **one** place a middle tier earns its keep — driving a running
app surfaces a spread of severities that a code diff doesn't, and collapsing a couple of
non-blocking major issues into a bare PASS would lose signal a human wants. So when (and
only when) the judgment is **behavioural, against a running app**, the ladder gains one rung:

- **PASS** — no critical or major deterministic issues. (NEEDS-HUMAN flags do **not** block
  PASS — they're surfaced inline but don't change the verdict.)
- **PASS-WITH-ISSUES** — **1–2 major** deterministic issues, **or** only minor/cosmetic
  issues. Browser-only — it never appears on a code-level (Mode 1) verdict.
- **NEEDS-HUMAN** — verification couldn't complete deterministically (browser flake, dev
  server unstable, unreachable `dev_url`, unresolvable auth, transient noise above threshold).
  Informational; a human reruns or fixes the environment. **NEEDS-HUMAN never blocks a PASS.**
- **FAIL** — **any** critical deterministic issue, **OR 3+** major deterministic issues.

This **extends** the three-verdict contract (`references/verdict-contract.md`), it does
not contradict it: PASS / NEEDS-HUMAN / FAIL keep their exact meanings; PASS-WITH-ISSUES
is a strictly browser-mode refinement of the PASS band, and a caller that demands a clean
gate treats it as not-a-clean-PASS. The **no-fourth-verdict rule for Mode 1 stands verbatim**
— PASS-WITH-ISSUES must **never** appear on a code-level (Mode 1) verdict.

## Browser flake discipline (for Mode 2 behavioural checks)

Browser tests are flaky; distinguish transient from deterministic **before** assigning a
verdict (this is the runtime analogue of `verdict-contract.md`'s *deterministic vs
transient* rule):

- **Per check — 3-attempt retry.** Run the check; if it fails, retry up to **2 more times**
  (3 total). Then judge by the *root cause*, not the count: fails the **same root cause**
  on every attempt → **deterministic**, report it by category + severity. Fails with
  **different root causes** (timeouts, races) → **transient** → mark that check
  **NEEDS-HUMAN** and move on. One fail + two passes = log as transient, don't count it.
- **Session governor.** If **≥30%** of checks hit NEEDS-HUMAN, **halt the session early** —
  the environment isn't stable enough for a trustworthy verdict, and flagging more results
  is misleading. If the dev server crashes mid-run, halt immediately; report what
  completed, mark the rest NEEDS-HUMAN. Retries are a tool, not a fix — a project that
  needs them for 30% of checks has a quality finding worth surfacing.
