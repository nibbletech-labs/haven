# Spec quality — how to firm acceptance and write a good spec

Read this when grooming (workflow 3) or writing an artifact (workflow 10) — i.e. whenever
you're firming an item's `done_looks_like` or writing its `spec`. The store now **enforces**
that a `ready` item has `done_looks_like`; this file is the guidance that makes that
acceptance — and the spec behind it — actually good, instead of a box ticked with vague prose.

The cardinal rule: **clarify, don't assume.** With a human present, your default is to *ask
targeted questions* where the item is genuinely under-defined — not to infer a pile of
assumptions and emit a big spec nobody validated. Scale the effort to the gap.

## Where each thing lives — the field map

A Haven item is not one document. The product context spreads across **node fields** and the
**`spec` artifact** — don't smush it into one, and **never duplicate node fields into the spec**:

| Captures | Lives in | Notes |
|---|---|---|
| **Problem** — current state + why it's insufficient, from the user's view | node `why` | One line of provenance. The *why we're building*, not the *what*. |
| **Success criteria** — product-level, testable outcomes | node `done_looks_like` | The verify anchor. Store-enforced for `ready`. |
| **Scope boundary** — explicit exclusions | `spec` artifact (**backbone**) | Always present. The single highest-value, most-skipped field. |
| **Constraints** — non-obvious NFRs, esp. negative ("must NOT") | `spec` artifact (**backbone**) | Always present. |
| **User context** — who, entry points, related flows | `spec` artifact | Where warranted. |
| **Detail / design** — approach, edge cases, references | `spec` artifact (free-form) | Whatever the item needs. |

So "writing the spec" never means restating `why` and `done_looks_like` — it means the
*boundary, constraints, and detail* the node fields can't carry.

## One leaf, one `spec` — which *role* to use (read this; it's the common mistake)

A buildable item gets **exactly one authored artifact, role `spec`** (filename `spec.md`). That
single file carries the scope boundary, constraints, and all design/approach detail — there is
**no separate `design` artifact on a leaf**. If an item arrived from capture with a
`design`/sketch artifact, **grooming re-roles it to `spec`** (or writes `spec.md` and removes the
old one) — never leave a leaf's contract under the `design` role.

The other roles are **not** leaf-grooming choices — don't reach for them when readying a leaf:
- **`design`** — architecture / the *why it's built this way*. Belongs on an **anchor** node (a
  project's living-docs hub: `ARCHITECTURE.md` beside `SPEC.md`), *not* on a thing you're about
  to build. This is the role most often picked by mistake for a leaf — use `spec` instead.
- **`research` / `source` / `vision`** — reference and living-doc roles, also anchor-side.
- **`decision`** — a recorded call (e.g. a "simple batch — no pack" note); **`handoff`** — an
  ai↔human handover note; **`delivery`** — the evidence stamped at `complete`.

Rule of thumb: **grooming/prep authors a leaf's `spec` and nothing else.** Design depth lives
*inside* that spec (the free-form detail), not in a second artifact.

## The quality bar — good / bad / check

Apply per field while grooming. The **check** is the one question that tells you if it's done.

### Problem (node `why`)
- **Good:** the gap from the user's perspective — current state + why it's insufficient.
- **Bad:** the solution restated as the problem ("we need a search bar"); implementation framing
  ("the API doesn't support filtering").
- **Check:** could someone with no context understand *why this matters*?

### Success criteria (node `done_looks_like`)
- **Good:** product-level outcomes, **concrete and testable** — "p95 latency < 200ms", "the
  export round-trips losslessly", not "fast" or "works well".
- **Bad:** test cases disguised as criteria ("unit tests pass"); implementation milestones
  ("endpoint deployed"); vague aspiration ("users are happy").
- **Check:** if every criterion is met, would a user agree the problem is solved?

### Scope boundary (spec backbone)
- **Good:** explicit exclusions with a one-line reason — what is **NOT** being built, and why.
- **Bad:** restating what *is* in scope (that's description, not a boundary); missing entirely
  (the most common failure, and the one that lets an eager builder drift).
- **Check:** would an eager agent know what **not** to build?

### Constraints (spec backbone)
- **Good:** non-obvious NFRs, especially **negative** ones — "must NOT break the existing API
  contract", "must NOT require a migration". Things that make the result unacceptable even if it
  otherwise works.
- **Bad:** universal obviousness ("must be secure", "must be fast"); implementation choices in
  disguise ("must use React" — unless that genuinely is a project constraint).
- **Check:** if violated, would the result be unacceptable even though functional?

### User context (spec, where warranted)
- **Good:** primary/secondary users identified by *situation* (not persona name), entry points,
  related flows named.
- **Bad:** generic personas ("power user"); no entry point; isolated from adjacent features.
- **Check:** could a builder make an edge-case call from this without asking?

## Adaptive ceremony — scale work to the gap

Score the item first; don't pay uniform overhead. Count how many of the fields above are already
present **and non-trivial**:

- **Rich (most fields present):** *fast-validate only.* Confirm they meet the bar, note any minor
  gap, move on. Don't re-derive what's already there. **0 questions.**
- **Moderate (some present):** targeted gap-filling. **1–2 questions.**
- **Thin (little or nothing):** full shaping. **2–4 questions.**

The goal is the right amount of work per item — a one-line bugfix does not get a five-section
spec, and a cross-cutting feature does not get a single vague line.

## Depth mode — clarify-first (default) vs autonomous

**Default — clarify-first (a human is in the loop):**
1. Score the item (rich / moderate / thin) → set your question budget.
2. Ask **targeted** questions via the normal interactive channel. Start with **who** and **why**
   before **what**. **Actively prompt for negative constraints** — "what should this *not* do?"
   is the question builders most often wish had been asked.
3. Build on the answers, then write. Don't write the substantial spec *before* asking — that's
   the assumption-sprawl this whole file exists to prevent.

**Autonomous — infer + tag (only when explicitly headless / batch):** when no human is available
(an unattended `create-context-pack` run, a batch groom), you may infer from context — but tag
**every** inference as an assumption: `[VERIFY] assumed X because Y — override if wrong`. This is
the same discipline as the context-pack's greenfield/brownfield `[VERIFY]` items
(`create-context-pack/references/pack-template.md` §0): nothing is assumed *silently*; a human or
the live code reconciles it later. Clarify-first is the rule; assume-and-tag is the fallback,
not the reverse.

## The spec's shape — backbone + free-form

Every `spec` artifact carries, at minimum, a **Scope boundary** and a **Constraints** section
(the backbone — they're the two highest-value, most-skipped fields). Beyond that the shape is
free: add user context, design detail, edge cases, references — whatever the item needs, in
whatever structure fits. Thin items lean on the backbone to fight the blank page; rich items stay
lean. Don't impose a rigid five-section template, and don't dead-end at "just write a file" with
no backbone at all.

> Worked example of this shape: HV-86's own `spec` artifact (`items/HV-86/spec.md`) — problem in
> the node `why`, success in `done_looks_like`, then **Scope boundary** + **Constraints** +
> free-form design.

## Oversized → flag → bounce (don't split here)

If an item is structurally too big — multiple user flows, multiple technical domains, 5+
independent success criteria — grooming does **not** split it. **Flag it and bounce to
`orchestrate-plan`** (workflow 6 / the decomposition skill). Spec-authoring firms a single
coherent item in place; decomposition is a different operation. A merely *under-specified* item is
groomed in place — only a *mis-scoped* one bounces.

## Anti-patterns

- **Assume-and-dump.** Inferring a dozen unknowns and emitting a big spec a human never saw.
  Clarify-first exists precisely to stop this.
- **Uniform ceremony.** A five-section spec on a one-line change; a single vague line on a
  cross-cutting feature. Score first.
- **Duplicating node fields into the spec.** `why` and `done_looks_like` are canonical on the
  node — restating them in the spec creates drift. The spec carries what they can't.
- **Boundary-less spec.** No scope boundary = guaranteed builder drift. It's the backbone for a
  reason.
- **Vague acceptance.** "Works well" / "is fast" is not testable and won't survive the
  `complete` verify step. Make it concrete.
