# Verdict contract — three verdicts, deterministic-only counting

`verify` returns exactly **one of three** verdicts, each with **evidence**. The yardstick
is **always the target's LIVE `done_looks_like`** (read per invocation via `haven_graph {include_acceptance:true}` or `haven_get_item`,
never a frozen pack copy) plus any inherited **shared-requirements** from the container
context-pack. Three verdicts only — keep the gate crisp.

## PASS

Deterministic suite **green** (`build + lint + test`, exit-0) **AND** acceptance
demonstrably **met** against the live `done_looks_like` + shared requirements, with **no**
blocking NEEDS-HUMAN flags.

- **Evidence** = what was built + the suite result (exit-0; the red→green transition where
  the leaf was built TDD) + the **explicit acceptance judgment** (which `done_looks_like`
  clauses are satisfied, and how the diff satisfies them).
- On PASS + the auto-complete dial **on** (leaves only), this evidence **rides
  `complete_item`** and becomes immutable on the leaf. Verdict-only writes the same
  evidence as a `delivery` `verdict.md`.

## NEEDS-HUMAN

The verifier **cannot decide deterministically**. It never blocks via a false FAIL and
never lands a false PASS — it escalates. Triggers:

- flaky / transient noise above threshold after retries (see *deterministic vs transient*);
- an **unresolvable environment** (a dev server or auth the verifier can't stand up);
- a **greenfield `[VERIFY]` design decision** a human must lock (no live code to check
  against);
- a genuine **judgment gap** — acceptance is ambiguous or under-specified for this diff.

- **Evidence** = the ambiguity + what it tried + the **residual question** for the human.

## FAIL

Deterministic suite **red** OR acceptance **demonstrably unmet**.

- **Evidence** = the failing gate excerpt (the exact build/lint/test failure) **or** the
  specific acceptance gap (which `done_looks_like` clause is not satisfied, and why).

## Deterministic vs transient — keep false positives out of the gate

Only **deterministic** signal counts toward FAIL. A check that fails, then passes (or
fails differently) on a clean re-run is **transient** — **log it, do not count it**. A
check that fails the **same way** on re-run is **deterministic** and counts. When transient
noise is above threshold and can't be resolved, the verdict is **NEEDS-HUMAN**, not FAIL —
the verifier reports the noise rather than blocking on it. (v1 leans on the fact that code
build/lint/test are already deterministic; this is the rule, not a retry engine — that's a
Mode-2 concern, deferred.)

## The yardstick — live, not frozen

Re-read `done_looks_like` on **every** invocation (via `haven_graph {include_acceptance:true}` or `haven_get_item`), so re-grooming the node
can never make a stale verdict drift. **Brownfield:** reality-check each `[VERIFY]` claim
against the live code before judging. **Greenfield:** there is little code to check against
— treat `[VERIFY]` items as **human-locked design decisions**; an unlocked one is a
NEEDS-HUMAN, not a guess. The verify discipline is identical; only what you check *against*
differs (live code, or human sign-off).

## No fourth verdict for the code gate

For the code-level gate (Mode 1) there is no "pass-with-issues". Cosmetic nits ride
**PASS** (noted in evidence); anything that genuinely needs a human is **NEEDS-HUMAN**.
Two implementations of the gate would mean two judgments and a lost independence
guarantee — there is one verifier, one verdict.

## PASS-WITH-ISSUES — the browser-only middle tier (Mode 2)

Browser/runtime QA is the **one** place a middle tier earns its keep — driving a running
app surfaces a spread of severities that a code diff doesn't, and collapsing a couple of
non-blocking major issues into a bare PASS would lose signal a human wants. So when (and
only when) the judgment is **behavioural, against a running app** (Mode 2 — see
`references/evaluation-lens.md` for the severity model), the ladder gains one rung:

- **PASS** — no critical or major deterministic issues. (NEEDS-HUMAN flags do **not** block
  PASS — they're surfaced inline but don't change the verdict.)
- **PASS-WITH-ISSUES** — **1–2 major** deterministic issues, **or** only minor/cosmetic
  issues. Browser-only — it never appears on a code-level (Mode 1) verdict.
- **NEEDS-HUMAN** — verification couldn't complete deterministically (browser flake, dev
  server unstable, unresolvable auth, transient noise above threshold). Informational; a
  human reruns or fixes the environment. **NEEDS-HUMAN never blocks a PASS.**
- **FAIL** — **any** critical deterministic issue, **OR 3+** major deterministic issues.

This **extends** the three-verdict contract, it does not contradict it: PASS / NEEDS-HUMAN
/ FAIL keep their exact meanings; PASS-WITH-ISSUES is a strictly browser-mode refinement of
the PASS band, and a caller that demands a clean gate treats it as not-a-clean-PASS. Mode 2
is out of v1 scope (see SKILL.md) — this rung is documented so the vocabulary is reconciled
when Mode 2 lands, not invented twice.

## Browser flake discipline (reference — for Mode 2 behavioural checks)

Browser tests are flaky; distinguish transient from deterministic **before** assigning a
verdict (this is the runtime analogue of *deterministic vs transient* above, kept here as
reference for when Mode 2 lands):

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
