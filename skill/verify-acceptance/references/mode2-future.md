# Mode 2 (FUTURE) — the browser-only verdict tier & flake discipline

> **Fenced future reference — nothing consumes this file in v1.** Mode 2
> (runtime/browser QA) is not built (see SKILL.md § Modes); this file reserves its
> verdict vocabulary so Mode 2 lands with one contract instead of two. Do **not**
> forward this file to a code-leaf (Mode 1) verifier — it is browser-mode material
> only, and forwarding it wastes the verifier's context on rules it must not apply.

## PASS-WITH-ISSUES — the browser-only middle tier

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

This **extends** the three-verdict contract (`references/verdict-contract.md`), it does
not contradict it: PASS / NEEDS-HUMAN / FAIL keep their exact meanings; PASS-WITH-ISSUES
is a strictly browser-mode refinement of the PASS band, and a caller that demands a clean
gate treats it as not-a-clean-PASS. Mode 2 is out of v1 scope — this rung is documented so
the vocabulary is reconciled when Mode 2 lands, not invented twice.

## Browser flake discipline (for Mode 2 behavioural checks)

Browser tests are flaky; distinguish transient from deterministic **before** assigning a
verdict (this is the runtime analogue of `verdict-contract.md`'s *deterministic vs
transient* rule, kept here as reference for when Mode 2 lands):

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
