# Verdict contract — three verdicts, deterministic-only counting

`verify` returns exactly **one of three** verdicts, each with **evidence**. The yardstick
is **always the target's LIVE `done_looks_like`** (read per invocation via the graph,
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

Re-read `done_looks_like` from the graph on **every** invocation, so re-grooming the node
can never make a stale verdict drift. **Brownfield:** reality-check each `[VERIFY]` claim
against the live code before judging. **Greenfield:** there is little code to check against
— treat `[VERIFY]` items as **human-locked design decisions**; an unlocked one is a
NEEDS-HUMAN, not a guess. The verify discipline is identical; only what you check *against*
differs (live code, or human sign-off).

## No fourth verdict

There is no "pass-with-issues". Cosmetic nits ride **PASS** (noted in evidence); anything
that genuinely needs a human is **NEEDS-HUMAN**. Two implementations of the gate would mean
two judgments and a lost independence guarantee — there is one verifier, one verdict.
