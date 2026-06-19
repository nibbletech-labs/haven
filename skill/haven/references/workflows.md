# Haven workflows — the playbook

Read this when you're about to run one of the workflows. Each gives a trigger,
steps, judgment heuristics, and real commands. Commands are the local CLI; for a
remote/headless client, the MCP equivalent and the CLI↔MCP differences are in
`surface-map.md`. JSON is the default output — read it to confirm refs and state.

**Before any project-scoped op:** make sure you know which project you're in
(every item lives in one). **CLI:** `haven project list` → `haven project use <key>`
(sticky) or `haven project add …`. **MCP/remote:** `haven_list_projects` →
pass `project: "<key>"` per call (no `use`); `haven_add_project` to create. Settle
this once per session, not per command (see `surface-map.md` for the selection model).

## Contents
- [Status routing (per-status action)](#status-routing)
- [1. Capture](#1-capture)
- [2. Plan / prioritise](#2-plan--prioritise)
- [3. Groom](#3-groom)
- [4. Dispatch — "what's next"](#4-dispatch)
- [5. Multi-item delivery](#5-multi-item-delivery)
- [6. Decompose vs group vs depend](#6-decompose-vs-group-vs-depend)
- [7. Evolve — split / merge / supersede](#7-evolve)
- [8. Handoff](#8-handoff)
- [9. Complete](#9-complete)
- [10. Artifacts & content](#10-artifacts--content)
- [11. Gates & reviews](#11-gates--reviews)
- [12. Project docs & anchors](#12-project-docs--anchors)
- [Conventions: acceptance, open questions, test-and-learn](#conventions)
- [Worked end-to-end example](#worked-example)
- [Anti-patterns](#anti-patterns)

---

## Status routing

The maturity lifecycle isn't just labels — each status implies an action (this is
how the consumer, e.g. `orchestrate`, routes work). Use it when grooming and
dispatching:

| Status | What it means | Action |
|---|---|---|
| `discovery` | Unknowns remain before it can be defined | Resolve its **open questions** (see [Conventions](#conventions)); when none remain → `definition`. |
| `definition` | Understood, needs a spec/decision | Write the spec (artifact), set **`done_looks_like`**; when defined → `ready`. |
| `ready` | Fully specified, dispatchable | It must carry `done_looks_like`. Dispatch (workflow 4); set `in_progress`. |
| `in_progress` | Being worked | Track; finish with **`item complete`** (workflow 9), which verifies against `done_looks_like`. |
| `blocked` | Parked on something | Check `wait_state` / the blocking dependency; clear when resolved. |
| `done` | Complete | Reached via `item complete` (workflow 9); it reports the items/gates the completion **unblocked** — your next dispatch set. |
| `superseded` / `archived` | Evolved away / dropped | Terminal; reachable through lineage. |

---

## 1. Capture

**Trigger:** the user dumps an idea, or a discovery/brainstorm surfaces work.

**Steps:**
1. One idea → one **bare** node. No edges, no priority, no commit unless the user
   signals engagement. `haven item add "Cache the auth JWKS lookup"` — that's it:
   floating, uncommitted, `discovery`.
2. A messy multi-idea conversation → extract each *distinct* unit of work as its
   own bare node. Title = a crisp imperative. One line of context in `--body` only
   if it would otherwise be lost (body is a summary, not the content).
3. Only if the user is clearly *planning* (not just capturing) do you start wiring
   — go to workflow 2.

**Heuristics:**
- **One idea, one node.** When unsure, capture more granularly — splitting and
  merging later are both cheap (lineage). But don't pre-decompose a vague idea into
  imagined subtasks; that's premature structure.
- **Capture verbatim intent.** Don't re-scope or "improve" the user's idea at
  capture time.
- **Don't raise status above `discovery`** unless the user says it's already
  well-defined. **Don't commit on capture** — "add to backlog" ≠ "I'm doing this."
- If the conversation produced a *document* (research, a rough spec), write it as a
  file and register it as an artifact (workflow 10) — don't cram it into `body`.

```bash
haven item add "Rate-limit the public search endpoint" --body "Abuse vector flagged in perf review"
haven item add "Add JWKS caching to auth verify path"
haven item add "Decide: presigned vs inline content for large artifacts" --type research
haven item list --icebox --pretty   # show the user what landed
```

## 2. Plan / prioritise

**Trigger:** the user wants to turn a pile of floating items into a committed,
ordered plan.

**Steps:**
1. Survey: `haven item list --icebox` (floating pool) and `haven item list --committed`.
2. For each item the user decides to do, **commit** it, optionally with a band:
   `haven item commit HV-12 --priority 1`.
3. Order *within* a band only where order genuinely matters:
   `haven item rank HV-12 --before HV-8` (relative, LexoRank-style — not absolute).
4. Wire **dependencies** for real ordering constraints: `haven depend HV-12 --on HV-9`.
5. For a release/phase, create the container node and group members:
   `haven item add "v1 launch" --type release` then `haven group HV-30 --add HV-12 --add HV-8`.
6. If the user is asking to deliver several items as one effort, continue through
   [Multi-item delivery](#5-multi-item-delivery) before dispatching the work.

**Heuristics:**
- **Commit only what you'll pull soon.** Commitment means "in play." Keep
  speculative work floating.
- **Priority bands, not a ranked list of 50:** 0 now · 1 next · 2 soon · 3 later ·
  4 someday. Reserve 0 for genuine urgency. Use `rank` only for fine order within a
  band.
- **A committed item still won't dispatch unless it's `ready`.** If you commit a
  `discovery` item, tell the user it needs definition first.

## 3. Groom

**Trigger:** "groom the backlog", "tidy this up", or a pass before planning.

**Steps:**
1. Pull the under-defined work: `haven item list --status discovery --pretty`,
   `--status definition`, and `--icebox`. Surface rot with `--stale <days>` (items
   untouched for N+ days), and answer "what's waiting on me?" with
   `--wait on_human` (or `--wait on_external` for real-world blockers).
2. For each, judge and apply:
   - Well-enough-defined to dispatch → set its acceptance and mark ready in one go:
     `haven item update HV-7 --status ready --done-looks-like "what success is"`.
     A `ready` item without `done_looks_like` can't be verified — always set it, and
     hold it to the bar in `spec-quality.md` (concrete + testable, not "works well").
   - Needs a spec/decision first → `--status definition`, then write the artifact
     (workflow 10) **to the bar in `spec-quality.md`** — score the gap, **clarify with
     the human before writing** where it's genuinely under-defined (don't assume), and
     give the spec its backbone (scope boundary + constraints).
   - Too big → split (workflow 7). Duplicate → merge (workflow 7).
   - Stale / won't-do → `haven item archive HV-7 --rationale "…"` (never delete).
   - Floating but clearly in-play → commit (workflow 2).
3. Clear stale waits: if the external thing arrived,
   `haven item update HV-7 --wait none --status ready`.

**Heuristics:**
- **Ready means dispatchable** — someone could pick it up and start *without
  further definition*. If you'd have to ask "what does this even mean," it's not
  ready.
- **Clarify, don't assume.** Scale the work to the gap (`spec-quality.md`: rich →
  fast-validate, thin → ask 2–4 targeted questions *then* write). With a human in the
  loop, asking beats inferring a pile of assumptions into a big unvalidated spec.
- **Groom toward fewer, clearer items.** Archive aggressively — it's reversible via
  `reopen`; the icebox is not sacred.
- **Don't bulk-commit during grooming.** Grooming is maturity (axis 1); commitment
  (axis 2) is the planning step's job. Mixing them dispatches half-baked work.

## 4. Dispatch

**Trigger:** the user (or an orchestrator) asks what to work on.

**Preflight — groom before you build (never build an ungroomed item).** Before
handing any item to a builder — a named ref *or* one pulled from `haven next` —
confirm it is **`ready` with a non-empty `done_looks_like`**. If it's still
`discovery`/`definition`, or has no acceptance, **groom it to ready first**
(workflow 3) or bounce it to planning — never build against a target you can't
verify. The store enforces this (`ready` requires `done_looks_like`), but treat
it as deliberate intent, not something to discover by tripping the guard.

```bash
haven next --pretty                 # top of the dispatch queue
haven next --owner human --limit 3  # work a human is ELIGIBLE to pull
haven next --owner ai               # work an AI is ELIGIBLE to pull (owner_eligible ai|any)
haven next --explain --owner ai     # WHY the queue is empty (when it is)
```

**Heuristics:**
- **If `next` is empty, diagnose — don't shrug.** Run `haven next --explain`
  (MCP `haven_next_explain`): it returns a per-reason breakdown — `owner_mismatch`,
  `blocked_by_dependency`, `waiting`, `committed_not_ready`, `ready_but_uncommitted`
  — and a `hint`. Report the reason and offer the fix (commit it / mark it ready /
  resolve the blocker), rather than reconstructing it by hand or inventing work.
- **Eligibility — not assignment — gates `--owner`.** `next --owner ai` returns items
  whose **`owner_eligible`** is `ai` or `any` (*who MAY pull it*) and **excludes
  untriaged (`owner_eligible` NULL) work, so it is never auto-pulled**. This is distinct
  from *assignment* (`owner_kind`, set by `assign` — *who IS doing it*): assigning a node
  to `ai` does **not** put it on the `--owner ai` frontier. So when you ready AI work, set
  its eligibility — `haven item update <ref> --owner-eligible ai` (or `any`) — or it stays
  invisible to the agent queue (`--owner-eligible none` re-untriages it). Never hand an
  agent a human-eligible node waiting on a real-world action.
- **Advance maturity on pickup:** `--status in_progress` when work starts (and `assign`
  to record *who's doing it*, separate from eligibility); finish with **`item complete`**
  (workflow 9), not a bare `--status done` — it records evidence and tells you what unblocked.

## 5. Multi-item delivery

**Trigger:** the user asks to deliver several items together: "ship these",
"do the first three", "for launch", "this release", "the MVP slice", "bundle X and
Y", or similar.

**Rule:** multi-item delivery must have a durable container before dispatch. Use:
- `release` when the group is an externally meaningful shipping scope, launch, or
  milestone.
- `phase` when the group is an internal slice, preparation batch, or sequencing
  step that is not itself a release.

```bash
haven item add "v1 auth hardening" --type release
haven group HV-30 --add HV-12 --add HV-13 --add HV-14
```

**Shared-context check:** before telling an AI to work the group, inspect whether
the members share any of these:
- architecture or subsystem boundary
- API, event, schema, or data model contract
- user journey, screen, design pattern, or copy surface
- test strategy, fixture setup, migration, or rollout concern
- overlapping key files, risky parallelism, or integration checkpoint

If none apply and each member is individually `ready`, the group may be a simple
batch: dispatch members normally, keeping the release/phase as the durable
membership record.

If any apply, **do not silently dispatch the items in isolation.** The group needs
an integrated execution view first:
1. If the `create-context-pack` skill is available, use it: it writes the integrated pack
   (with a verify-first preamble) as a `spec` artifact on the release/phase node and
   sharpens each member's acceptance.
2. If that workflow is unavailable or unknown, pause and clarify the integrated
   architecture with the user. Capture the result as a short `spec` or `decision`
   artifact on the release/phase node before dispatch.
3. If the check surfaces missing work, capture it as new items and group or depend
   them correctly.

**Heuristics:**
- The release/phase node is the delivery container; member items remain the value
  or scope contributors. Don't lose the user's original items just because the
  execution slice needs a different shape.
- A Context Pack is **conditional, not blanket**: skipped for a simple batch, but
  established **pack-first** (before building any member) once members share an
  architecture / contract / data model.
- If the pack introduces execution-local items, map them back to Haven refs or
  create child/member items before work starts. Don't let the pack become a second
  hidden backlog.
- Record the decision either way: "simple batch, no shared architecture found" or
  "shared API and migration; pack/spec required". Put durable reasoning in a
  `decision` artifact if it matters later.

## 6. Decompose vs group vs depend

Pick the edge layer by *why* the nodes relate — they can co-exist; don't force one
to do another's job:
- **Decomposition** when sub-parts are *intrinsic* ("auth = API + UI + tests"). The
  parent becomes a coordinating concept; children are the real work.
  `haven decompose HV-20 --into HV-21 --into HV-22`.
- **Grouping** when membership is a *delivery choice* ("these five ship in v1").
  Re-batch freely. `haven group HV-30 --add HV-21 --add HV-22`.
- **Dependency** when there's a hard ordering ("UI can't start until the API
  exists"). `haven depend HV-22 --on HV-21`.

Don't fake a release with decomposition, and don't model blocking as decomposition.

## 7. Evolve

**Trigger:** an item is too big (split), two items are the same problem (merge), or
a redesign replaces an item (supersede). All emit append-only lineage; sources
become `superseded`.

```bash
haven evolve split HV-10 \
  --into "Backend API for auth" --into "Frontend login UI" \
  --rationale "Spans two owners and >1 day; splitting for independent dispatch"
haven evolve merge HV-11 HV-12 --title "Unified auth flow" --rationale "Same problem, two angles"
haven evolve supersede HV-13 --with HV-20 --rationale "Replaced by the redesign in HV-20"
haven evolve graph HV-10 --direction descendants   # see what HV-10 became
```

**When to split:** spans more than one owner; or more than ~a day of work; or has
internal ordering you want as dependencies; or mixes maturity (one part ready, one
needs discovery). *Don't* split just to look organised — a coherent half-day task
stays one node.

**When to merge:** two nodes are genuinely the same unit of work, or two captures
of one idea. Don't merge merely-related items — use a dependency or shared group.

**When to supersede:** the new node *replaces* the old one's intent (a redesign, a
pivot). If you're only refining the same item, just `update` it — don't churn
lineage.

## 8. Handoff

**Trigger:** an AI finishes its part and a human must act (review, decide, sign
something) — or vice versa.

**Use the atomic tool** — `haven item handoff` (MCP `haven_handoff`). It does the
three steps in one call: records a `handoff` artifact (stamped `from`/`to`), flips
the owner, and sets the wait-state/status. Don't hand-assemble `assign` + `update`
+ `artifact add` — you'll do it inconsistently.

```bash
haven item handoff HV-7 --to human \
  --note "Implemented the API; needs your review of the rate-limit defaults."
# → owner=human, status=blocked, wait=on_human; a handoff artifact under notes/.
haven item handoff HV-7 --to ai        # hand back: clears the wait, unblocks it
```

**Defaults (direction-aware; override with `--status` / `--wait`):**
- **to a human:** `status=blocked`, `wait=on_human` (now waiting on them);
- **to ai:** the wait clears, and a `blocked` item becomes `ready` (actionable).

**Heuristics:**
- A handoff is a *transition*, not just a note — the atomic tool guarantees the
  owner flip and wait-state happen together with the note.
- A handed-off, waiting item correctly **drops out of `next`** — that's the system
  working. (Prefer a real dependency edge over `on_dependency` for blockers.)

## 9. Complete

**Trigger:** an item's work is finished and verified.

**Use the atomic tool** — `haven item complete` (MCP `haven_complete_item`). It
records the evidence as an artifact (default role `delivery`), sets `status=done`,
and **returns the items/gates the completion unblocked** — your next dispatch set.

```bash
haven item complete HV-1 --evidence "cargo test --workspace: 72 passed"
# → {item: …done…, artifact: delivery.md, unblocked: [HV-2], warnings: []}
```

**Heuristics:**
- **Verify against `done_looks_like` before completing.** If the item has no
  acceptance, `complete` still runs but **warns** — treat that warning as a signal
  the item was under-defined, not noise.
- **Attach evidence** (test output, a summary, a link) — completion should be
  auditable, not a bare status flip.
- **Act on `unblocked`.** Those items just became dispatchable — feed them into the
  next `dispatch` pass (or surface a now-unblocked gate for review, workflow 11).
- Completing a `superseded`/`archived` item is refused — `reopen` it first.

## 10. Artifacts & content

**Trigger:** real work product exists (spec, research, design, decision), or a
filesystem-less client must read/write content.

**Local agent (has the filesystem) — the common case:**
- Write/edit the file **directly** (Read/Edit/Write) under
  `~/.haven/<project>/items/<ref>/` (e.g. `spec.md`). No round-trips.
- Register it once so it's queryable:
  `haven artifact add HV-7 --role spec --file ~/.haven/<project>/items/HV-7/spec.md`
  (`--file` copies the file into the item tree and records a typed pointer).
- Read it back: `haven artifact get HV-7 --role spec`; list: `haven artifact list HV-7`.
- Quick scratch, no DB row: `haven note HV-7 "remembered: prod key rotates monthly"`.

**Filesystem-less client (phone, remote sandbox) — the content channel:**
- **Write:** pass `content` and the server writes the file:
  `haven artifact add HV-7 --role spec --content "# Spec\n…" --name spec.md`
  (MCP `haven_add_artifact {ref, role, content, name}`). The bytes never land in
  the DB — only the pointer.
- **Read:** `haven_get_artifact {ref, role}` returns `{path, role, content}`,
  lazy-pulling from Storage if the file is remote-only.

**Writing a good `spec` (not just a file):** registering the artifact is the
mechanical half — the content has a bar. See `spec-quality.md` for it: the field map
(problem → `why`, success → `done_looks_like`, boundary/constraints/detail → the spec —
never duplicated), the always-present backbone (**scope boundary + constraints**),
adaptive ceremony (score rich/moderate/thin, don't pay uniform overhead), and the
depth mode — **clarify-first with a human present** (ask targeted questions before
writing), infer-and-`[VERIFY]` only when headless. Don't dead-end at "wrote a file."

**Register vs just write:**
- **Register** (`artifact add`) durable, referenceable work products someone will
  find via the graph: `spec`, `research`, `design`, `decision`, `handoff`,
  `vision`, `source`, `delivery`.
- **Role for a leaf you're building = `spec`, full stop** (one `spec.md`, holding
  boundary + constraints + design detail). `design`/`research`/`source`/`vision` are
  anchor-side living-doc roles — *not* a leaf's contract. See `spec-quality.md`
  ("One leaf, one `spec`"); picking `design` for a leaf is the common mistake.
- **Don't register** throwaway scratch/working notes — write them into `notes/` or
  use `haven note`. `notes/` is a free filesystem; over-registering clutters the
  queryable layer.

## 11. Gates & reviews

**Trigger:** a batch of work needs a review checkpoint before the next phase — "once
the API, UI, and tests are done, review the auth feature before we ship."

A gate is **not** special machinery — it's a `gate`-type node wired with the edges
you already know:

**Steps:**
1. Create the gate node and give it **dependency** edges on the items it reviews
   (its "triggered after" set), with the pass-criteria as its acceptance:
   ```bash
   haven item add "Auth feature review" --type gate \
     --done-looks-like "rate-limit defaults signed off; no P1s open; security checklist passed"
   haven depend HV-40 --on HV-21 --on HV-22 --on HV-23   # HV-40 = the gate
   ```
2. The gate is blocked until all its triggers are `done`, so it **surfaces for
   review on its own** — it shows up in `haven next` (or `haven item list --type
   gate`) the moment the last trigger completes. No polling.
3. Run the review against the gate's `done_looks_like`. If it passes → `--status
   done`. If it surfaces follow-up work, capture items (optionally group them under
   the gate or the release).

**Heuristics:**
- A gate's triggers are a **dependency** relationship ("can't review until these are
  done"), not grouping — don't model it with `group`.
- Keep the gate's `done_looks_like` concrete (what must be true to pass), so the
  review is a check, not a vibe.

## 12. Project docs & anchors

**Trigger:** durable project knowledge needs a home — a vision doc, style guide,
prompt library, research note, decision record — or the user asks "should this
live in the repo or in Haven?" / "tidy up the project docs".

**The placement test:** *does a script, build step, or pipeline read this file by
path?*
- **Yes → repo.** Build inputs, prompts a generator consumes, config docs that
  tooling validates against. Moving these breaks the pipeline; they ship with the
  code by necessity.
- **No → Haven.** Docs read only by humans and agents for context: vision and
  direction, style guides and taste docs, prompt experiments and clippings,
  research, decision records. Keeping them in Haven also keeps them out of the
  repo if it is ever public.

**Steps:**
1. Check `haven docs` for an existing anchor before creating one.
2. Create **a few thematic anchors** — not one catch-all, not one per doc:
   ```bash
   haven item add "Acme — vision & direction" --type anchor
   haven item add "Acme — style guides" --type anchor
   haven item add "Acme — research" --type anchor
   ```
3. Attach each doc with the role matching its nature:
   ```bash
   haven artifact add HV-50 --role design --file docs/brand-style.md
   ```
   `--file` copies it into `~/.haven/<project>/items/HV-50/`; remove the loose
   repo copy once migrated so there is one source of truth.
4. Thereafter edit the files under the anchor's item directory **directly** —
   living docs are updated in place, no re-registration.
5. Discover, never hard-code: `haven docs` / `haven_docs` lists every anchor with
   its artifacts.

**Heuristics:**
- **Anchors are shelving, not work.** Leave them uncommitted and out of the
  status lifecycle — they never dispatch, never complete.
- **Grey zone defaults to Haven.** A doc an agent uses mid-task but no script
  reads (a house-style guide, a prompt scrapbook) goes in Haven; repo docs are
  for what a stranger cloning the repo needs to build and understand the code.
- **Need it visible in-tree?** `haven link` projects a git-excluded `Haven/`
  folder into the repo working tree — docs sit beside the code without being
  committed.
- **Doc backs one item?** Attach it to that item, not an anchor. Anchors hold
  knowledge that outlives any single item.

## Conventions

Three orchestrate-grade patterns that need no special fields — judgment + the
primitives you have:

**Acceptance (`done_looks_like`).** The acceptance statement an item's output is
checked against. Set it the moment an item becomes `ready` (workflow 3) — a `ready`
item without it can't be verified or dispatched cleanly. Keep it concrete and
testable ("p95 < 200ms", "all rows have a non-null `email`"), not "works well". A
dispatcher builds its verification from this field.

**Open questions (discovery gating).** A `discovery` item is "not yet definable
because X, Y, Z are unknown." Track those unknowns as a short list in the item's
notes or a `research`/`scratch` artifact (`haven note HV-7 "Q: which font APIs
allow commercial redistribution?"`). The rule: **don't advance `discovery →
definition` until the open questions are resolved.** Dispatch research to answer
them; record findings as a `research` artifact.

**Test-and-learn (competing approaches).** When you're unsure which approach wins,
model each as its own item, **grouped** together, each with a `done_looks_like`
that doubles as its *test* (the measurable bar that decides success). Run the
highest-priority one first; if its test passes, `evolve supersede` the alternatives
with a rationale ("approach B met the latency bar; A/C superseded"). If it fails,
move to the next. Lineage records why the losers were dropped.

---

## Worked example

A discovery chat about hardening auth, then plan → dispatch → handoff. The
comments explain the *judgment*.

**User:** "Brain-dump on auth: the JWKS lookup is slow, the public search endpoint
has no rate limit, and I'm unsure whether to use presigned uploads or inline
content for big artifacts. Park all that."

```bash
# Capture: one node each, all floating/discovery. No edges, no commit.
haven item add "Cache the auth JWKS lookup" --type code
haven item add "Rate-limit the public search endpoint" --type code --body "Abuse vector flagged in perf review"
haven item add "Decide: presigned vs inline content for large artifacts" --type research
haven item list --icebox --pretty     # HV-1, HV-2, HV-3 — all discovery, uncommitted
```

**User:** "Do the first two for the v1 hardening release. They're independent. The
research one stays parked."

```bash
haven item add "v1 auth hardening" --type release   # HV-4
haven group HV-4 --add HV-1 --add HV-2
haven item commit HV-1 --priority 1
haven item commit HV-2 --priority 2
# HV-3 stays floating — correct, not in play.
```

**User:** "The JWKS cache is well-defined; I wrote up the approach. Make it
dispatchable to the AI."

```bash
haven artifact add HV-1 --role spec --content "# JWKS cache\nCache for 10m, refresh on kid-miss…" --name spec.md
haven item update HV-1 --status ready
haven item assign HV-1 --to ai
# HV-2 stays discovery — committed but not yet ready, so it won't dispatch.
```

**User:** "What should the AI pick up?"

```bash
haven next --owner ai --pretty
# → HV-1 only. HV-2 is committed but still discovery, so absent — correct.
```

**User:** "The AI built it, but I want to review the cache TTL before we call it
done."

```bash
# Not done yet — a human review is the remaining work. Hand it off atomically:
# one call records the note, flips the owner to human, and parks it on_human.
haven item handoff HV-1 --to human \
  --note "Cache implemented; p95 verify 3ms. One open call: TTL defaulted to 10m — confirm before ship."
# → owner=human, status=blocked, wait=on_human, handoff artifact under notes/.
# HV-1 drops out of `next` until Tom acts — correct.
```

**User (later):** "TTL's fine. Done."

```bash
# Tom confirms → complete it with evidence; this also reports what it unblocked.
haven item complete HV-1 --evidence "TTL reviewed and accepted at 10m. Shipping."
# → status=done; unblocked: [HV-4 ship gate, …] if anything depended on HV-1.
```

Net: every structure change went through a tool (the atomic `handoff`/`complete`
where they fit); the spec and handoff note live as files (registered, queryable);
nothing was deleted; `next` stayed honest; the two axes were managed independently.

---

## Anti-patterns

- **Don't hand-edit `backlog.md`** (or any projection) — it's regenerated and your
  edits get clobbered; the DB is canonical.
- **Don't put content in the DB / `body`.** Specs, notes, code are *files*; `body`
  is a one-line summary.
- **Don't over-structure on capture** — no edges, priority, commit, or invented
  subtasks for a fresh idea. Floating + `discovery` is correct.
- **Don't hard-delete — there is none.** `archive --rationale`; revive with
  `reopen`. Lineage must stay intact.
- **Don't create dependency cycles** (the core guards against it). If A waits on B
  and B on A, you've mis-modelled — likely decomposition or a merge.
- **Don't overload edge layers.** Release = grouping. Blocking = dependency.
  "Part of" = decomposition.
- **Don't register every scratch file.** `notes/` is free; register only durable,
  queryable products.
- **Don't migrate pipeline inputs into Haven.** If a script or build reads a file
  by path it stays in the repo; Haven holds the docs only humans and agents read
  (workflow 12).
- **Don't commit speculative work** to make the backlog look active. Keep maybes
  floating.
- **Don't sync after every mutation.** It's manual and offline-first; one
  `haven sync` after a batch is enough.
- **Don't conflate the two axes,** and **don't treat empty `next` as "nothing to
  do"** — diagnose why and surface the fix.
