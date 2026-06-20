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

## The spec-shippability linter — a HARD gate, not advice

The good/bad/check bar above is the *aim*; this is the **mechanical pass** that catches the spec
when the aim slips. Run it over a `spec` (or a `done_looks_like`) **before** you call the item
ready — every rule is a **yes/no check, not a judgment call**, so it either passes or it doesn't.
Treat a failing rule as blocking: fix it or the item is not shippable. Each rule is the
distillation of a class of bug that reaches the build agent and wrecks it (the build/verify
subagents don't inherit this skill — what isn't in the spec is gone).

1. **Weasel-word scan — quantify or remove.** Scan the spec for these words and kill each one:
   **fast, slow, quick, easy, seamless, intuitive, simple, obvious, efficient.** Each is a hole
   where a number, a threshold, or a concrete behaviour should be. "Fast" → "p95 < 200 ms";
   "intuitive" → the actual interaction; "simple" → cut it or say what's excluded. A weasel word
   is unverifiable by construction, so it cannot anchor acceptance.
2. **Schema AND example — both, for every contract.** Every API shape / payload / event you
   specify (an `API-[ID]` / `EVENT-[ID]`-class contract) carries **both a schema and a concrete
   example payload**. A schema *without* an example is **rejected** — the schema says what's
   legal, the example says what's *intended*, and the gap between them is where builders guess
   wrong. One filled-in example is worth more than a paragraph of prose about the field.
3. **Architecture notes cite real files.** Any technical grounding must name the **actual file
   with its path** — `lib/auth/session.ts`, not "we have auth"; "we use caching" must name the
   cache implementation, not wave at it. An unsourced claim ("the API supports filtering") is an
   assumption masquerading as a fact; force it to a path or drop it. In Haven terms: cite the real
   repo file, not a vibe.
4. **Quality gates are yes/no, not judgment calls.** Every gate / acceptance line must be
   **executable as a binary check** — "the export round-trips losslessly", not "code quality is
   acceptable"; "returns a 422 with `{error, field}`", not "returns an appropriate error". Sign-off
   is a **checklist, not a judgment**: if reading a gate makes you *decide* rather than *observe*,
   it isn't a gate yet.

Red flags this linter is built to catch (each is a real failure mode, not a style nit):
"returns appropriate error" with no error format; a contract with a schema but no example; an
architecture note that says "we use caching" without naming the implementation; a gate like
"code quality is acceptable" that no one can run; acceptance phrased as a milestone ("endpoint
deployed") rather than an observable outcome.

## Missing upstream input — surface the gap, never fabricate

A groom/spec step often expects an **upstream input** — a parent's `spec`, an anchor's `design`,
a research artifact, a prior decision. When that required input is **absent**, do **not** quietly
invent it. The discipline (lifted from the cross-skill handoff protocol) is:

1. **Surface the gap** with the **exact path/ref** you were looking for **and the producer you
   expected** — name the artifact role and the skill/step that produces it (e.g. "no `spec`
   artifact on HV-42's parent — `create-context-pack` is what writes that").
2. **Offer three explicit choices**, don't pick silently:
   (a) **invoke the producer** to generate it properly;
   (b) **accept a hand-rolled substitute** the human provides; or
   (c) **proceed with documented degraded input** — note in the spec that the upstream was
   missing and what you assumed in its place.
3. **Never silently fabricate** the missing input's content. A fabricated upstream is worse than a
   flagged gap: it looks authoritative and propagates downstream unchecked. This is the same rule
   as the autonomous `[VERIFY]` tagging above — nothing critical is assumed *invisibly*.

> **Candidate Haven primitive — the gate "Refines" enrichment loop.** Builder's backlog gates
> carried an optional **`Refines:`** edge: a review checkpoint, on firing, **lists the items to
> enrich based on the gate's findings** — the gate's outcome feeds *back* into sharpening the
> downstream items it gates, not just a pass/fail. Haven has no first-class equivalent yet (a
> completed gate-style item that re-grooms its dependents). Worth capturing as a floating item if
> the pattern recurs — findings-enrich-downstream is the durable idea, the `Refines:` syntax is
> just one encoding of it.

> **Type is descriptive, not a dispatch instruction.** An item's **type** (code / research /
> data / design / admin) is **descriptive metadata for filtering and reporting — not a dispatch
> instruction.** Don't branch behaviour on it ("type=research, so skip the spec"); groom every
> item to the same bar and let ownership/acceptance drive what happens next. Haven already encodes
> this — `kind`/type is a label on the node, never a router — so this is a confirmation, not a new
> rule: resist the temptation to make type do dispatch work it isn't for.
