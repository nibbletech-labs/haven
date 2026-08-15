---
name: plan-item
description: >-
  The front door for ALL planning. Settle what's being built and how, and
  write it onto ONE Haven item — a `spec` artifact (scope boundary,
  constraints, approach) plus concrete `done_looks_like`. Fires on "make a
  plan", "plan out the next phase of X", "plan the approach for HV-42", "how
  should we do this ticket", and equally on a whole product, launch or
  greenfield build with nothing in place yet: everything gets a high-level
  plan on one item first. Captures the item if none exists, and folds in
  overlapping or duplicate items. Then it calls the scale — one build pass
  (add the build checklist and its fresh-eyes review points, hand it to
  build) or hand the ref to `orchestrate-plan` to break down. Planning, not
  doing: it writes no code.
---

# plan-item — every plan starts here, on one item

You settle **what is being built and how**, and write it onto **one Haven item**: a
firm `done_looks_like` plus a `spec` artifact carrying the scope boundary, the
constraints, and the approach. Then you **call the scale** — is this one build pass,
or does it need breaking down? You produce a plan; **you write no product code**.

**Every plan starts here**, whatever its size. A one-line bug fix and a whole product
launch both begin as one item with a plan on it. What differs is what happens next,
and that's a decision made *after* there's a plan to look at — never before.

## Where it sits (the planner family — they meet only at the graph)

It's a sequence, not a menu:

```
plan-item  →  one build pass?  ──yes──→  build it (spec + checklist, status=ready)
                     │
                     no
                     ↓
             orchestrate-plan (decompose, rooted at this ref)
                     ↓
             plan-item per leaf  →  build
                     ↓  (several leaves built together)
             create-context-pack (one shared brief over the group)
```

- **`orchestrate-plan` requires a plan.** It is the *second* stage and it decomposes
  a ref that already carries one — it never starts from a bare goal or a title. If it
  fires with no plan in place, the plan comes first, here.
- **`plan-item` runs again per leaf.** `orchestrate-plan` stops at work-grain leaves
  (what / why / done, above the code); a leaf whose approach still needs settling
  comes back here, one leaf at a time.
- **`create-context-pack`** is for several leaves about to be built *together*.

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
- **One item, one `spec`.** Role `spec`, filename `spec.md`, holding boundary +
  constraints + approach. Not `design` — that's an anchor-side role. The same holds
  for an item headed for decomposition: its high-level plan is still `spec` on that
  node, and it becomes the shared foundation the children read up to.

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

3. **CLARIFY.** Score the gap, then ask your targeted questions through the normal
   interactive channel. Start with **who** and **why** before **what**, and always
   ask for the **negative** constraints — "what should this *not* do?" is the
   question builders most often wish had been asked. Pitch the questions at the
   work's actual altitude: a bug fix gets mechanics, a product launch gets shape and
   sequence. If there is genuinely no person available, infer but tag every inference
   `[VERIFY] assumed X because Y`.

4. **CALL THE SCALE — the decision point.** Now, with the work understood, run the
   size test (below): **could a single build pass deliver this against one spec?**
   Run it on the *post-fold* shape, since folding overlap in can change it.

   **Say the call out loud with its reason** — this is the decision the person most
   wants a say in, and it's the whole reason the plan comes before the breakdown.
   An obvious one-pass item: state it in a clause and carry on. Anything borderline,
   or a "decompose" call: name which of the three reasons applies (a gate in the
   middle, split human/AI ownership, or it won't survive one pass) and let them
   redirect you before you write.

5. **WRITE THE PLAN ONTO THE ITEM.** Three places, no duplication between them:
   - `why` — the problem, one line, from the user's view;
   - `done_looks_like` — concrete, testable success criteria;
   - `spec.md` — **scope boundary** + **constraints** (both always present) + the
     approach, edge cases, and any file paths it rests on. Write the file directly,
     then register it once: `haven artifact add <ref> --role spec --file …`.

   **The scale call shapes what you write:**
   - **One pass** → a build spec: approach at code altitude, plus the **build
     checklist** (below).
   - **Needs decomposition** → a **high-level plan**: the goal, the parts the work
     divides into (as prose, not child items), the ordering and gates you can see,
     the constraints, the boundary, and the decisions still open. **No build
     checklist** — you don't know the steps yet, and inventing them is the guessing
     this whole sequence exists to avoid. This spec becomes the shared foundation
     that `orchestrate-plan`'s children inherit by reading up to this node, so the
     boundary and constraints are the load-bearing parts.

   Where the harness has a **native plan mode**, that's the natural place to have done
   steps 3–5's thinking — see *Borrow your harness's plan mode* below, including what
   of its output to keep and what to drop.

   Then run the **shippability linter** (`spec-quality.md`): kill weasel words,
   every contract carries a schema *and* an example, architecture claims name real
   file paths, every acceptance line is observable rather than a judgment call.
   A failing rule is blocking, not advice.

6. **HAND IT ON.** Two exits, per the step-4 call:

   **One pass** — set `status=ready` with an owner, then say what happens next in a
   line; this skill stops at the plan:
   - **build it here** — do the work in this session against the spec (in a harness
     with a plan-mode gate, that gate goes here; without one, just build to the spec
     and show the diff);
   - **hand it to the executor** — `orchestrate-run`, if it's one of several ready
     leaves and you want them driven end-to-end;
   - **it's someone else's** — `haven item handoff` to a person.

   Building it here is the **normal** choice for one item — `orchestrate-run` is a loop
   for many leaves, and it's overhead for one. But building it here means **you** carry
   the review the executor would have run: at least one fresh-eyes code review before it
   completes (*Review*, below), written into the spec so it isn't optional.

   Whoever builds it finishes with `haven item complete <ref> --evidence "…"`, naming
   the review that ran.

   **Needs decomposition** — hand the ref to **`orchestrate-plan`**, which roots
   there and reads the plan you just wrote. **Don't set it `ready`** and don't give
   it an owner: it isn't dispatchable work, it's about to become a parent. Say what
   it is and why it's being broken down, in a line.

## Borrow your harness's plan mode (where it has one)

Some harnesses ship a **native plan mode**: read-only exploration of the codebase, a
structured plan, and a human approve-or-redirect gate before anything gets written.
Where yours has one, use it to do the thinking in steps 3–5. It is real infrastructure,
and it enforces this skill's own rule for you — plan mode can't write product code.

Then **land the result on the item.** A plan-mode plan normally evaporates into the
transcript. The whole point here is that it becomes the item's `spec`, where the builder
reads it — often a subagent that never saw your session — and where the person can read
it again next week.

- **Distil, don't paste.** Keep the durable half: the approach, the scope boundary, the
  constraints, and the sequence (which becomes the build checklist). Drop the volatile
  half — exact line numbers, a file-by-file edit list — which is stale the moment code
  moves. A one-pass item about to be built can carry more detail, since it's consumed
  within the hour; a high-level plan headed for `orchestrate-plan` stays well above the
  code, because it has to survive until the tree is built.
- **The approval carries.** If the person approved the plan in plan mode, your step-4
  scale call is confirmed with it — don't re-ask what they just said yes to.
- **No plan mode? Nothing changes.** Do the same thinking inline and write the same spec.
  Plan mode is a convenience for the harnesses that have one, never a prerequisite — this
  skill exists precisely because not every harness does.

## The build checklist (every one-pass spec carries one)

One item can hold a lot of work, so a spec that's going to build ends with an ordered
**build checklist** — the route through the work, as tickable lines. (A plan headed for
decomposition gets none: its steps aren't known yet.)

```markdown
## Build checklist
- [ ] Add the `source_kind` column and its migration
- [ ] Wire the TVDB client behind the existing lookup interface
- [ ] **REVIEW** — fresh-eyes code review of the schema and the lookup contract,
      before anything is built on top of them
- [ ] Backfill artwork for titles already scanned
- [ ] Show per-title match status in the library view
- [ ] **REVIEW** — fresh-eyes code review of the full diff
- [ ] Tick each box here as it lands — this is how progress is reported
```

- **Ordered, and every line a visible unit of progress.** Aim for steps a person could
  watch tick past. Not "implement the feature" (one box is not a checklist), and not
  twenty micro-edits (that's a diff, not progress).
- **Review points are checklist lines**, so they're tracked and ticked like the rest of
  the route. How many, and where, is *Review* below.
- **It's the route, not the destination.** `done_looks_like` stays the outcome test and
  the thing verification judges. A ticked checklist is **not** evidence the item is
  done — never let the checklist stand in for acceptance.
- **Tell the builder to tick it in the file as they go**, in the spec itself. Where the
  harness has its own to-do list, mirror the checklist into it — but `spec.md` is the
  durable record and the one the person can read without asking anyone.
- **Chunky items are exactly why this exists.** A five-stage ticket with no checklist is
  opaque to the person and easy for the builder to lose its place in. This is the price
  of keeping items big, and it's a cheap one.

## Review — a one-pass build still gets fresh eyes

`orchestrate-run` gates every leaf with a **separate** verifier before it merges. A
single item built in one pass skips that machinery, and rightly so — standing up an
orchestrator for one ticket is pure overhead. It must not skip the **judgment**. So the
plan says so, in the spec, where whoever builds it will actually read it:

- **At least one code review, always.** Before the item completes, a review agent that
  **did not write the code** reviews the diff. Fresh eyes is the whole point: the agent
  that just built it re-reads its own intent instead of the code in front of it. In
  Claude Code that's `/code-review` or a review subagent; use whatever your harness
  offers, so long as it isn't the builder.
- **More than one when the work has real checkpoints.** Put extra `REVIEW` lines in the
  build checklist at the boundaries that matter — a migration landing, an API contract
  going in, a flow becoming usable end to end. Four reviews of 400 lines beat one review
  of 1,600, and a checkpoint review catches a wrong foundation *before* three more stages
  get built on it. A chunky item with no interior review point is exactly the case this
  rule exists for.
- **Code review and acceptance are different questions.** A review asks "is this code
  right?"; `verify-acceptance` asks "does this meet `done_looks_like`?" A substantial
  item wants both, and neither stands in for the other.
- **Nothing completes on an unreviewed diff.** The review runs, its findings are fixed or
  explicitly accepted, and `haven item complete <ref> --evidence "…"` names which review
  ran and what it found. An item completed with no review named is a gap, not a shortcut.

## The size test

Ask one question: **could a single build pass deliver all of this against one spec?**

- **Yes** → it's one item. Plan it here, **however chunky**. A multi-stage plan is
  perfectly happy on one ticket, and `done_looks_like` can carry several criteria — an
  AI build gets a lot done in one pass, so a plan with five stages in it is usually
  one item, not five. **Size is not the test, and neither is the number of success
  criteria.**
- **No** → it needs **decomposition**. Write the high-level plan anyway (step 5), then
  hand the ref to `orchestrate-plan`. Don't hand-create children here: shallow-splitting
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
  whoever step 6 named.
- **Spec a group.** Several leaves about to be built together share one brief:
  that's `create-context-pack`.
- **Complete the item.** Evidence is stamped by whoever builds it.
