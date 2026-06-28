# The evaluation lens — how the acceptance judgment is actually made

The deterministic suite (`build + lint + test`, exit-0) is necessary but **not
sufficient** — exit-0 only covers what the tests assert. Whether the diff actually
satisfies the live `done_looks_like` is a **judgment call**, and this file is the lens
for making it well: a structured review checklist, a confidence filter that keeps false
positives out of the gate, an accessibility lens for UI leaves, and a severity model for
ranking what you find. It is forwarded IP — a build/verify agent does **not** inherit
the skills this came from, so the technique has to live here as readable text, not as a
reference to a skill it can't load.

This lens **feeds** the verdict; it does not replace it. Findings here resolve up into
the three-verdict contract (`references/verdict-contract.md`): a must-fix that breaks
acceptance is a **FAIL**, an ambiguity you can't resolve deterministically is a
**NEEDS-HUMAN**, cosmetic nits ride **PASS** noted in evidence. (For a browser leaf, the
fuller verdict ladder including the **PASS-WITH-ISSUES** middle tier is in
`verdict-contract.md` — that tier is browser-only.)

---

## Independence by construction — the load-bearing precondition

The whole reason a separate verify step exists is that a same-context reviewer is
structurally blind to its own blind spots. **A verifier carrying the build agent's priors
will miss exactly what the build agent missed.** So independence is not an instruction to
"try to be objective" — it is *structural*: judge acceptance seeing **only** the target's
live `done_looks_like`, any container shared-requirements, and **the diff**. Never the
build agent's spawn prompt, reasoning, narrative, or self-check. When invoked ad hoc, if
a caller pastes the builder's narrative, **ignore it** — taking it on board re-couples the
judgment and forfeits the one guarantee the gate is here to provide.

## Sign-off is an exhaustive checklist, not a "probably fine"

Acceptance is judged by walking **every** acceptance id, one at a time — each is a yes/no
item, run it or verify the test exists and passes. You **cannot** sign off with:

- unchecked items ("probably fine");
- partial coverage of the acceptance clauses;
- a failure you noticed but didn't surface in the verdict evidence.

Acceptance clauses are **binary** — met or not, never "mostly passes". An exhaustive walk
is what makes a PASS trustworthy enough to (dialed on) auto-complete a leaf. This is the
discipline whether the ids are Haven `done_looks_like` clauses or, in a forwarded build
spec, scenario / behaviour / design ids (SC / BV / DR).

---

## The 5-category review checklist (for a code diff)

Five categories, checked in order. Each finding must cite specific evidence — file, line,
and the rule it violates.

### 1. Correctness
Logic errors, off-by-ones, unhandled edge cases, null/undefined handling, race
conditions, resource leaks. Only flag what you're confident will cause incorrect
behaviour **in practice** — not theoretical possibilities.

### 2. Security
Injection (SQL, XSS, command), auth gaps, secret exposure, unsafe input handling.
**OWASP-relevant issues only** — not speculative attack surfaces.

### 3. Conventions
Project patterns from CLAUDE.md, existing code, and framework conventions: import
patterns, naming, file structure, error-handling style, logging, platform compatibility.
**Cite the specific rule or pattern.** "Violates project convention" is not enough —
"CLAUDE.md requires snake_case for utility functions, this uses camelCase" is. If you
can't point to a specific rule or established pattern, it's not a convention finding.

### 4. Complexity
Unnecessary abstractions, dead code, duplication that warrants extraction, premature
generalization. Only flag when there's a **clearly simpler alternative** — not style
preferences.

### 5. Interface contracts
Implementation matches defined contracts **exactly** — API signatures, event payloads,
type definitions, response schemas. In a build-spec context, verify against the pack's
API ids and EVENT ids (the contracts the leaves were told to conform to).

### Severity for code findings
- **must-fix** — blocks the PASS. Correctness bugs, security issues, interface-contract
  violations.
- **note** — recorded in evidence but does not block. Convention deviations, minor
  complexity.

### Finding format
Each finding includes: **file:line** (exact location) · **category** (which of the five)
· **finding** (what's wrong, with specific evidence) · **rule / reference** (which project
guideline, OWASP category, interface contract, or established pattern is violated) ·
**severity** (must-fix or note) · **suggested fix** (concrete and actionable — not "fix
this", show what the fix looks like). If there are no findings, say so in one line — don't
manufacture issues.

---

## The confidence filter — keep false positives out of the gate

Only report findings you're confident about. Before recording one, ask:

- Is this a **real issue that will cause problems in practice**, or a theoretical concern?
- Am I sure this violates an **actual rule/pattern**, or is it just a style preference?
- Would I **mass-flag this across a codebase**, or is it specific to this context?

If uncertain, don't report it. **Quality over quantity — false positives waste fix
cycles.** A false finding that blocks a good diff is as bad for the gate as a missed bug.

### The calibration rule
**A finding must point at a rule, a contract, a CLAUDE.md line, or an OWASP/APG entry.
Hand-waved "this seems too complex" is not a finding.**

### Worked examples — the must-fix / note threshold

These calibrate where the line sits, and what counts as cited evidence.

**Example 1 — correctness, must-fix.** A `verifyToken` calls `jwt.verify(token, secret)`
without checking that `token` is non-empty; an empty token throws and the catch swallows
it, and the same path is reused on `/refresh` where a missing token must be handled
differently and instead silently fails with no logging. Rule: OWASP A07:2021 —
Identification and Authentication Failures; the catch must distinguish "no token" from
"token invalid". *Why must-fix:* real bug path, identified rule, named lines, concrete
fix.

**Example 2 — conventions, note.** `formatDate` is camelCase, but CLAUDE.md (§ Conventions,
line 34) requires snake_case for utility functions in `src/utils/`. Rule: project
CLAUDE.md § Conventions, line 34. *Why note:* real rule, real evidence, but it doesn't
affect correctness or security — convention drift only.

**Example 3 — interface contracts, must-fix.** An endpoint returns `{ id, email, name }`
but the spec's `API-USR-LIST` defines the response as `{ id, email, displayName,
role }` — the handler omits `displayName` and `role`. Rule: the spec's interface-contract
clause for `API-USR-LIST.response`. *Why must-fix:* contract violation, specific
citation, concrete fix.

**Example 4 — complexity, deliberately WITHHELD.** A function repeats a ~4-line block
**three** times. The reviewer considers flagging it as "duplication that warrants
extraction." **Decision: do not report.** Three repetitions of a small, locally-coupled
block are **not yet** a duplication finding — the confidence filter applies; this is
judgement, not an established rule. It crosses into a **note** only if a **fourth call
site** appears, **or the block grows beyond ~6 lines.** This is the calibration: when in
doubt below the threshold, withhold.

---

## The accessibility (a11y) acceptance lens — for UI leaves

When a leaf's `done_looks_like` is a user-facing UI behaviour, accessibility is part of
acceptance, not a nicety. Two complementary checks:

### a11y as deterministic verify-suite steps
- **`eslint-plugin-jsx-a11y`** in the lint step — static ARIA issues, hook violations,
  code-quality. Run with `--max-warnings 0` on the changed component files.
- **`axe-core`** against the **rendered** DOM — via the test runner (`jest-axe` /
  `vitest-axe` calling `axe(container)` on each render, or the Storybook test runner with
  the a11y addon). This catches ~57% of WCAG mechanically.

> **Coverage caveat — load-bearing:** **axe-core and the tests only see what the
> stories / tests actually render.** If there's no story/test for the error state, axe
> can't check the error state's a11y. A green a11y suite over thin coverage is a **false
> green** — judge whether each acceptance-relevant state is *rendered* before trusting the
> a11y pass. Coverage gaps hide a11y bugs.

### Key component a11y contracts — as acceptance checks
The recurring failure modes that AI builds reliably produce. Treat each as a yes/no
acceptance item for the relevant component:

- **Radio groups and tabs use arrow keys, not Tab, to move between items** — only the
  active item is in the tab order; Tab enters/leaves the group. (Tab-between-items is the
  single most common implementation mistake here.)
- **`aria-expanded` is present from initial render** on toggles/disclosures — not added on
  first click.
- **A live region must exist in the DOM *before* the update** that announces into it
  (`aria-live` / `role="status"`) — a region injected at update time is not announced.
- **A disabled control must block its handlers, not just style itself** — `disabled`/
  `aria-disabled` plus click handlers that genuinely don't fire. "Visually disabled but
  functionally active" is a real defect.
- Semantic HTML over ARIA equivalents (`<button>` not `<div role="button">`), required
  ARIA attributes present and referencing valid ids, no redundant ARIA on native elements.

---

## The design-evaluation lens (UX acceptance) — four checklists, summarised

For a leaf whose acceptance is design/UX quality (not just "code compiles"), evaluate
through four lenses. These are the **eval lenses** distilled to an actionable acceptance
checklist — the full corpus lives in the design skill; what's here is enough to judge
acceptance against, not the 340KB original. Apply **per screen or per feature**, not a
whole product at once. Select the lenses and items that apply; skipping inapplicable ones
is correct, not lazy.

### 1. Nielsen's 10 heuristics + Krug's trunk / 3-second test
Walk the ten usability heuristics (visibility of system status; match to the real world;
user control & freedom; consistency & standards; error prevention; recognition over
recall; flexibility & efficiency; aesthetic & minimalist design; help users recover from
errors; help & documentation). Then run **Krug's trunk test** on any screen a user can
land on directly: at a glance, blurry-eyed, they must answer — *What site/product is
this? What page am I on? What are the major sections? What can I do here (the primary
action, within 3 seconds)? Where am I in the scheme of things? How do I search?* Pass =
all six answered immediately without reading; **two or more unanswerable = fail.**

### 2. Laws of UX
Apply the **5–8 most applicable** laws (don't run all 30) — e.g. Miller's Law / chunking
(no ungrouped set >7; chunk into 3–5), cognitive load (every element earns its place;
consistent interaction patterns; don't make the user carry state the system could),
Hick's, Fitts's, Jakob's. Skip a law whose applicability criteria don't match the feature.

### 3. WCAG 2.2 AA
Organise by component type ("I'm shipping a modal — what applies?"), not by criterion
number. Text contrast (≥4.5:1 normal, ≥3:1 large; test every state and background), text
resize, focus visible & order, keyboard operability, names/roles/values, target sizes,
the **[2.2 NEW]** items teams carrying over 2.1 habits miss. Baseline AA; AAA where noted.

### 4. Per-component a11y contract
For each component, apply its full contract: semantic role, accessible name, required
`aria-*` states, the keyboard contract (every key and what it does), focus management (on
open/close/delete/create/route-change), screen-reader announcements + live regions, touch
target sizes. (This is the same family as the frontend a11y checks above — here it's the
exhaustive per-component contract; above it's the high-frequency defect shortlist.)

---

## The severity model — severity = frequency × impact × persistence

One model ranks everything the lens finds, so a verdict can say *how bad*, not just
*what*. Severity is the product of three factors:

- **frequency** — how often the problem occurs;
- **impact** — how hard it is for the user to overcome;
- **persistence** — a one-time nuisance vs. a repeated obstacle.

| Level | Label | Meaning |
|---|---|---|
| 0 | Non-issue | Does not affect usability / acceptance; no action |
| 1 | Minor | Cosmetic or low-frequency; fix if time permits |
| 2 | Moderate | Causes occasional difficulty; should be fixed |
| 3 | Major | Causes significant difficulty; high priority |
| 4 | Catastrophic | Prevents task completion or causes serious harm; must fix before release |

A rare-but-catastrophic problem (data loss) is still a 4; a frequent-but-trivial one is
still a 1 — the factors interact, they don't simply add.

### Per-check verdicts and quick-mode tags
Each individual check returns **PASS / CONCERN / FAIL** (CONCERN = partially met or
"flag for discussion"; mark **N/A** where it doesn't apply and note why). In a fast pass,
run only the items tagged **[CRITICAL]**; in a full pass, run them all.

### Rolling check-verdicts up into the gate's verdict
- Any **FAIL** at severity 3–4, or an acceptance clause demonstrably unmet → **FAIL**.
- A **CONCERN** you can't resolve deterministically (ambiguous/under-specified
  acceptance) → **NEEDS-HUMAN**.
- Severity-1 cosmetics with everything else green → **PASS**, noted in evidence (or, for a
  browser leaf, **PASS-WITH-ISSUES** per `verdict-contract.md`).
