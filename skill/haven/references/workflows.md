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
- [5. Decompose vs group vs depend](#5-decompose-vs-group-vs-depend)
- [6. Evolve — split / merge / supersede](#6-evolve)
- [7. Handoff](#7-handoff)
- [8. Artifacts & content](#8-artifacts--content)
- [9. Gates & reviews](#9-gates--reviews)
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
| `in_progress` | Being worked | Track; verify output against `done_looks_like` on completion. |
| `blocked` | Parked on something | Check `wait_state` / the blocking dependency; clear when resolved. |
| `done` | Complete | Verify against `done_looks_like`; check whether any **gate** is now unblocked (workflow 9). |
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
  file and register it as an artifact (workflow 8) — don't cram it into `body`.

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
   `--status definition`, and `--icebox`.
2. For each, judge and apply:
   - Well-enough-defined to dispatch → set its acceptance and mark ready in one go:
     `haven item update HV-7 --status ready --done-looks-like "what success is"`.
     A `ready` item without `done_looks_like` can't be verified — always set it.
   - Needs a spec/decision first → `--status definition`, write/attach the artifact
     (workflow 8).
   - Too big → split (workflow 6). Duplicate → merge (workflow 6).
   - Stale / won't-do → `haven item archive HV-7 --rationale "…"` (never delete).
   - Floating but clearly in-play → commit (workflow 2).
3. Clear stale waits: if the external thing arrived,
   `haven item update HV-7 --wait none --status ready`.

**Heuristics:**
- **Ready means dispatchable** — someone could pick it up and start *without
  further definition*. If you'd have to ask "what does this even mean," it's not
  ready.
- **Groom toward fewer, clearer items.** Archive aggressively — it's reversible via
  `reopen`; the icebox is not sacred.
- **Don't bulk-commit during grooming.** Grooming is maturity (axis 1); commitment
  (axis 2) is the planning step's job. Mixing them dispatches half-baked work.

## 4. Dispatch

**Trigger:** the user (or an orchestrator) asks what to work on.

```bash
haven next --pretty                 # top of the dispatch queue
haven next --owner human --limit 3  # what the human could pick up
haven next --owner ai               # what an AI agent should take
```

**Heuristics:**
- **If `next` is empty, diagnose — don't shrug.** Common causes: nothing committed;
  committed items still `discovery`/`definition` (not `ready`); everything blocked
  by a dependency; or items in a `wait_state`. Reconstruct which from `item list`
  filters, report it, and offer the fix (commit it / mark it ready / resolve the
  blocker). Run this diagnosis automatically whenever `next` is empty.
- **Respect ownership.** Filter `--owner ai` when dispatching to an agent; never
  hand an agent a human-owned node waiting on a real-world action.
- **Advance maturity on pickup:** `--status in_progress` when work starts (and
  `assign` if needed), `--status done` on completion.

## 5. Decompose vs group vs depend

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

## 6. Evolve

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

## 7. Handoff

**Trigger:** an AI finishes its part and a human must act (review, decide, sign
something) — or vice versa.

**Steps:**
1. Write the handoff note as a file and register it (it carries `from`/`to`):
   ```bash
   haven artifact add HV-7 --role handoff --from ai --to human \
     --content "Implemented the API; needs your review of the rate-limit defaults." \
     --name 2026-06-09-to-tom.md
   ```
2. Flip ownership and set the wait-state so it's clearly the other party's move:
   ```bash
   haven item assign HV-7 --to human
   haven item update HV-7 --status blocked --wait on_human
   ```
3. On hand-back, reverse it: `assign --to ai`, `--wait none`, `--status ready` or
   `in_progress`.

**Heuristics:**
- A handoff is a *transition*, not just a note — set `from`/`to` and flip ownership
  in the same logical step.
- `wait_state` says *why* it's parked: `on_human`, `on_external` (a real-world
  event), `on_dependency` (prefer a real dependency edge for that).
- A handoff'd, waiting item correctly **drops out of `next`** — that's the system
  working.

## 8. Artifacts & content

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

**Register vs just write:**
- **Register** (`artifact add`) durable, referenceable work products someone will
  find via the graph: `spec`, `research`, `design`, `decision`, `handoff`,
  `vision`, `source`, `delivery`.
- **Don't register** throwaway scratch/working notes — write them into `notes/` or
  use `haven note`. `notes/` is a free filesystem; over-registering clutters the
  queryable layer.

## 9. Gates & reviews

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

**User:** "It's done, but I want to review the cache TTL before we ship."

```bash
haven item update HV-1 --status done
haven artifact add HV-1 --role handoff --from ai --to human \
  --content "Done. One open call: TTL defaulted to 10m — confirm before ship." --name to-tom.md
haven item assign HV-1 --to human
haven item update HV-1 --status blocked --wait on_human   # drops out of `next` until Tom acts
```

Net: every structure change went through a tool; spec and handoff live as files
(registered, queryable); nothing was deleted; `next` stayed honest; the two axes
were managed independently.

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
- **Don't commit speculative work** to make the backlog look active. Keep maybes
  floating.
- **Don't sync after every mutation.** It's manual and offline-first; one
  `haven sync` after a batch is enough.
- **Don't conflate the two axes,** and **don't treat empty `next` as "nothing to
  do"** — diagnose why and surface the fix.
