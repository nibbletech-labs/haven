---
name: plan-item
description: >-
  Plan one piece of work on a single Haven item: settle the approach with the
  person, write it onto the item as its `spec` (scope boundary, constraints,
  design detail), sharpen `done_looks_like`, then hand it to build. The
  default for "make a plan", "plan out the next phase of X", "plan the
  approach for HV-42", "how should we do this ticket", "work out how to do
  this before you build it" — one feature, one change, one bug, one phase
  that fits a single buildable item — chunky and multi-stage is fine, one
  ticket holds a lot. Captures the item first if none exists yet. Escalates
  to `orchestrate-plan` only when one build pass genuinely can't deliver it.
  Not decomposition, not a work tree, and it writes no code.
---

# plan-item — the plan for one item

You take **one piece of work**, settle **how it will be done**, and write that onto
**one Haven item**: a firm `done_looks_like` plus a `spec` artifact carrying the
scope boundary, the constraints, and the approach. Then it goes to build. You
produce a plan; **you write no product code**.

This is the ordinary case. Most planning is one item — a feature, a change, a bug,
the next phase of something already running. It does **not** need a work-graph.

## Where it sits (the planner family — they meet only at the graph)

| The work is… | Skill | What it produces |
|---|---|---|
| One item | **`plan-item`** | acceptance + a `spec` on that item |
| Too big for one item | `orchestrate-plan` | a decomposition tree of leaves |
| A chosen group about to be built together | `create-context-pack` | one shared brief over the group |

The boundary between the first two is a **test, not a vibe** — see *The size test*
below. Run it early; it costs one judgment and saves a tree nobody wanted.

`plan-item` is also the **second stage** after a decomposition: `orchestrate-plan`
stops at work-grain leaves (what / why / done), and any leaf that still needs its
approach settled gets it here, one leaf at a time.

## Operating rules (inherit from the `haven` skill)

Read the `haven` skill's **`references/spec-quality.md`** — it is the bar this whole
skill writes to (the field map, adaptive ceremony, clarify-first, the shippability
linter). Read `references/surface-map.md` for CLI⇄MCP op detail rather than
restating arguments from memory. The gotchas that bite here:

- **Structure only through ops; content as files.** Node fields move via
  `haven …` / `haven_*`; the spec is a **file** under `~/.haven/<project>/items/<ref>/`.
  `body` is a one-line summary, never the plan.
- **Clarify, don't assume.** With a person in the loop, ask targeted questions
  *before* writing. Scale the questions to the gap (rich → 0, moderate → 1–2,
  thin → 2–4). Writing a big unvalidated spec is the failure this skill exists
  to prevent.
- **Never let a ref travel alone.** Say what the item *is* in the same sentence as
  its ref (`haven` skill, § *Talking about the backlog to a person*).
- **One leaf, one `spec`.** Role `spec`, filename `spec.md`, holding boundary +
  constraints + approach. Not `design` — that's an anchor-side role.

## The steps

1. **RESOLVE THE ITEM.** One of three:
   - the person named a ref → use it;
   - the work is obviously the item already in flight → confirm it in a line;
   - nothing tracks it yet → **capture one** (`haven item add`), then plan it.
     Capture is one command; a plan with no home is the thing to avoid.

   Read what's already there (`haven item get` / `haven_get_item`) — `why`,
   `done_looks_like`, existing artifacts. Don't re-derive what the item already says.

2. **SCAN FOR OVERLAP** (don't skip — this is where duplicate backlog comes from).
   Before you plan, find what the graph already holds on this subject:
   `haven search "<the real terms>"`, then read the neighbours of anything it
   surfaces. A long-lived backlog collects half-captured versions of the same idea,
   and planning one of them in isolation quietly ships the other three as debt.

   Sort each hit into one of four, and **name the action rather than absorbing it
   silently**:
   - **The same work** → `haven evolve merge` into one item. Lineage is preserved;
     nothing is deleted.
   - **Replaced by what you're about to plan** → `haven evolve supersede <old>
     --with <this>` with a real rationale.
   - **Stale or no longer wanted** → `haven item archive --rationale "…"`
     (reversible via `reopen` — archive beats leaving it to rot).
   - **Genuinely related but separate** → leave it standing. Wire a dependency edge
     if there's real ordering, and name it in the spec's **scope boundary**
     ("HV-x covers the export side; not in scope here").

   Apply the clear-cut merges and archives yourself. **Ask before superseding**, or
   before folding in anything the person may still want tracked separately. Report
   what you folded and what you left, in plain English with the refs in parentheses —
   this is the moment a person finds out their backlog just got smaller.

3. **RUN THE SIZE TEST.** Below — and run it on the *post-fold* shape, since folding
   overlap in can change it. One item → carry on. Too big → hand to
   `orchestrate-plan`, scoped to *this ref*, and stop. Do this **before** the
   questions, so you don't clarify a shape that's about to be split.

4. **CLARIFY.** Score the gap, then ask your targeted questions through the normal
   interactive channel. Start with **who** and **why** before **what**, and always
   ask for the **negative** constraints — "what should this *not* do?" is the
   question builders most often wish had been asked. If there is genuinely no
   person available, infer but tag every inference `[VERIFY] assumed X because Y`.

5. **WRITE THE PLAN ONTO THE ITEM.** Three places, no duplication between them:
   - `why` — the problem, one line, from the user's view;
   - `done_looks_like` — concrete, testable success criteria;
   - `spec.md` — **scope boundary** + **constraints** (both always present) + the
     approach, edge cases, any file paths it rests on, and the **build checklist**
     (below). Write the file directly, then register it once:
     `haven artifact add <ref> --role spec --file …`.

   Then run the **shippability linter** (`spec-quality.md`): kill weasel words,
   every contract carries a schema *and* an example, architecture claims name real
   file paths, every acceptance line is observable rather than a judgment call.
   A failing rule is blocking, not advice.

6. **READY IT, THEN HAND TO BUILD.** Set `status=ready` with an owner. Then say what
   happens next in one line — this skill stops at the plan:
   - **build it here** — do the work in this session against the spec (in a harness
     with a plan-mode gate, that gate goes here; without one, just build to the spec
     and show the diff);
   - **hand it to the executor** — `orchestrate-run`, if it's one of several ready
     leaves and you want them driven end-to-end;
   - **it's someone else's** — `haven item handoff` to a person.

   Whoever builds it finishes with `haven item complete <ref> --evidence "…"`.

## The build checklist (every spec carries one)

One item can hold a lot of work, so the spec ends with an ordered **build checklist** —
the route through the work, as tickable lines:

```markdown
## Build checklist
- [ ] Add the `source_kind` column and its migration
- [ ] Wire the TVDB client behind the existing lookup interface
- [ ] Backfill artwork for titles already scanned
- [ ] Show per-title match status in the library view
- [ ] Tick each box here as it lands — this is how progress is reported
```

- **Ordered, and every line a visible unit of progress.** Aim for steps a person could
  watch tick past. Not "implement the feature" (one box is not a checklist), and not
  twenty micro-edits (that's a diff, not progress).
- **It's the route, not the destination.** `done_looks_like` stays the outcome test and
  the thing verification judges. A ticked checklist is **not** evidence the item is
  done — never let the checklist stand in for acceptance.
- **Tell the builder to tick it in the file as they go**, in the spec itself. Where the
  harness has its own to-do list, mirror the checklist into it — but `spec.md` is the
  durable record and the one the person can read without asking anyone.
- **Chunky items are exactly why this exists.** A five-stage ticket with no checklist is
  opaque to the person and easy for the builder to lose its place in. This is the price
  of keeping items big, and it's a cheap one.

## The size test

Ask one question: **could a single build pass deliver all of this against one spec?**

- **Yes** → it's one item. Plan it here, **however chunky**. A multi-stage plan is
  perfectly happy on one ticket, and `done_looks_like` can carry several criteria — an
  AI build gets a lot done in one pass, so a plan with five stages in it is usually
  one item, not five. **Size is not the test, and neither is the number of success
  criteria.**
- **No** → it needs **decomposition**. Hand it to `orchestrate-plan` **scoped to this
  ref**, say so in a line, and stop. Don't hand-create children here: shallow-splitting
  an item to dodge the escalation is the failure mode this boundary exists to catch.

There are only three honest reasons for "no":

1. **A gate in the middle** — something must be decided, produced or approved before
   the rest can even be *shaped*. You'd be guessing at the later stages, not just
   writing them out.
2. **Split ownership** — part is real-world human work (a payment, a sign-off, a
   physical action) and part is the AI's, so it can't be one dispatchable unit.
3. **It genuinely won't survive one pass** — too much to hold at once, or it has to
   land incrementally with something verified in between.

Absent one of those, keep it as one item. **Fragmenting is not free**: every extra
node is another handoff, another spec, and context lost between them.

The boundary reads **both ways**. `orchestrate-plan` runs the same test at its own
front door and hands work back here when one pass could do it.

## What this skill does not do

- **Decompose.** No child items, no dependency tree, no anchors. That's
  `orchestrate-plan` — escalate, don't improvise.
- **Write product code.** The plan is the deliverable. Building is the next step, by
  whoever step 5 named.
- **Spec a group.** Several leaves about to be built together share one brief:
  that's `create-context-pack`.
- **Complete the item.** Evidence is stamped by whoever builds it.
