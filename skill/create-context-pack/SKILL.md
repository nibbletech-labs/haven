---
name: create-context-pack
description: >-
  Create a build-ready spec (a "context pack") for an already-planned group of
  Haven items: enrich them into one integrated brief — shared foundation,
  cross-cutting requirements, sharpened acceptance, a verify-first preamble —
  written as a `context-pack` artifact onto the group's top node, ready for
  plan mode. Headline case: a greenfield build — one large spec for a phase-1
  build, setting the contracts the leaves conform to. Use when you have
  planned leaves you're about to build together and want one brief — e.g.
  "spec out phase 1 of the build", "create a context pack for HV-3 and HV-4",
  "ready these items for dev". Run on one item it degenerates to grooming that
  leaf; run on several it grooms each, then synthesises the cross-cutting
  brief. Sits on the GROUPING axis, above `haven` and after
  `orchestrate-plan`: it does NOT decompose a goal (use `orchestrate-plan`)
  and does NOT execute or write code (that's plan mode). Not for coarse items
  that still need decomposition.
---

# create-context-pack — the build-prep (spec) half of orchestrate

You take a **chosen set of already-planned Haven leaves you're about to build
together** and turn them into a single, verify-first **spec (context pack)** on the
group's top node — shared foundation, cross-cutting requirements, sharpened
per-leaf acceptance, and an explicit boundary of what the group depends on.
You build the brief; **you execute nothing** — no code, no agents dispatched,
no status flipped to `in_progress`. The code work and the human approval gate
belong to native **plan mode**, which reads this pack as its input.

## Where it sits (the planner family — meet only at the graph)

`orchestrate-plan` (decompose a goal → graph, **decomposition** axis) →
**`create-context-pack`** (enrich + verify-prep a chosen group, **grouping** axis) →
native **plan mode** (the code-level plan + the human "go").

- `orchestrate-plan` stops at **work-grain** leaves (what / why / done, deliberately
  *above* the code). It reasons over decomposition and never touches grouping.
- `create-context-pack` runs **after** planning, over the **grouping** axis the planner
  ignores, and writes a cross-cutting synthesis for one group you're about to build.
- Plan mode does the **code-grain** layer (which files, what approach) that goes
  stale the instant code moves — so it's never frozen into the graph.

The pack lives **in the graph** (a `context-pack` artifact on the group's top node + the
members' own fields), never a separate frozen file. Leaves inherit the shared
material by reading **up** their one grouping edge to the top node.

## Operating rules (inherit from the `haven` skill)

Read the `haven` skill's `references/surface-map.md` (CLI⇄MCP) and
`references/workflows.md` (esp. the release/phase "Context Pack" assessment) for op
detail — don't restate arguments from memory. The gotchas that bite here:

- **Structure only through ops; content as files/artifacts.** Mutate nodes/edges
  only via `haven …` / `haven_*`. The pack is artifact **content**; `body` is a
  one-line summary, never the pack.
- **Per member, groom — don't invent a parallel check.** Readying a member *is*
  the `haven` skill's **groom** workflow (wf 3): firm its `done_looks_like` to a
  concrete, testable bar and write/firm its own `spec` artifact (wf 10) when it
  warrants one. Never leave a prepped member without concrete acceptance. Only a
  member that needs **decomposition** (structurally too big) bounces to
  `orchestrate-plan`; mere under-specification you groom in place.
- **Defer-all corrections.** This skill *does not apply fixes*. When the brief is
  wrong, the correction is plan mode's first **human-approved** step (see the
  verify-first pack). You only *enrich and prepare* — the human gate stays
  authoritative.
- **Over MCP there is no sticky session** — pass `project` on every call, and
  there's **no batch**: one entity per `haven_add_edge` / `haven_update_item` call.

## The flow

The exact CLI **and** MCP op for each step is in `references/verify-ops.md`; the
pack's section layout + the verbatim preamble are in `references/pack-template.md`.

0. **REORIENT.** Read the whole graph in one call (`haven graph` / `haven_graph`).
   Resolve the project first if unknown. This is the only tick state.
1. **RESOLVE THE TARGET.** The input is an explicit **single item or set of items**
   — never the whole project. Read the members' grouping edges, find their common
   `release`/`phase` container, then pick the pack's home by comparing your target set
   to that container's **full** membership:
   - **Target set == the container's full membership** → use that container directly.
   - **Target set is a strict *subset* of its members** (you're building only part of a
     broader phase), **or there is no common container** → **create a dedicated
     build-batch container** (`--type phase`) and group **only the targeted members**
     into it. Members keep their existing phase membership — grouping is **many-to-many**,
     so you *add* the batch edge and *never remove* the member's original group. The pack
     must land on this batch container: a phase holds **one** `context-pack.md`, so writing
     a subset's pack onto the broad phase mis-scopes it (a pack covering 2 of 7 members)
     **and** a later batch from the same phase would **clobber** it.
     - **The broad phase must NOT keep a live `context-pack` artifact.** If it already
       holds one (e.g. from an earlier whole-phase prep), **remove it** —
       `haven artifact rm <broad-phase> --role context-pack --name context-pack.md` (or
       `--id` if duplicated). A stale pack left on the broad phase misroutes the pointer
       (HV-75): every still-grouped member resolves its `context_pack` to that pack
       (now a dead redirect). If you want a "moved to `<BATCH>`" breadcrumb, record it
       as a **non-`context-pack`** artifact (a `scratch` note) or in the container body —
       **never** as a `context-pack` artifact (a prose "MOVED" note left as the pack is
       exactly the tombstone `haven doctor`'s `context_pack_integrity` check flags).
   A single item is a **degenerate group**: there is nothing cross-cutting to
   synthesise, so just **groom that one leaf** (wf 3 — firm acceptance + write/firm
   the leaf's **own** `spec` artifact `spec.md` via wf 10 where warranted) and stop.
   **No container, and no separate `context-pack` artifact** — a pack only exists
   to govern a *group* from its container, so on a lone leaf it would just co-reside
   with the leaf's own `spec.md` as a pointless `context-pack` artifact with no group
   to govern. Firm `spec.md` in place; that is the leaf's contract.
   What groups a batch is simply that **you intend to
   build the members together** — neither a dependency between them nor shared architecture
   is required (**the GROUP axis**). Shared architecture is the **PACK** trigger *within* that
   group: when members touch the same code, contracts, or data model, the pack captures that
   write-once context — and a dispatcher (`orchestrate-run`) establishes it **pack-first**,
   before building any member; when they don't, step 5's shared-context assessment records a
   one-line "simple batch — no pack" and you still keep the grouping. So "simple batch → no
   pack" and "packless-shared-architecture → pack first" are the two arms of one rule, not a
   contradiction: **build-together makes the group; shared architecture makes the pack within
   it.** Dependency is never the trigger.
2. **DEPENDENCY-CLOSURE CHECK.** Walk each member's dependency edges. For an
   external dependency `d` (not in the set): if `d` is `done`, pull its
   output/acceptance into the pack's foundation as **read-only context**; if `d` is
   unbuilt, **surface it as a scope boundary** ("`b` depends on `d` [status] — pull
   `d` in, or it blocks `b`") and let the human decide. **Never auto-expand scope.**
3. **GROOM EACH MEMBER (precondition).** Bring every member to a sealed leaf via
   the `haven` **groom** workflow (wf 3): firm its `done_looks_like` to a concrete,
   testable bar and write/firm its `spec` (wf 10) where it warrants one — both held to
   the `haven` skill's `references/spec-quality.md` bar (field map, backbone, adaptive
   depth). With a human present this is **clarify-first**: ask before assuming. Under-
   specified-but-coherent members you groom **in place**; only a member that needs
   **decomposition** (structurally too big) → **STOP and route to
   `orchestrate-plan`** (this skill fleshes out a planned group; it does not
   decompose). **Single active pack per leaf:** a member that already carries a
   `context_pack` pointer for a *different* container (or a `context_pack_clash`) is
   governed by another pack — **STOP and surface the clash**, never auto-pick or merge
   (`references/verify-ops.md` step 3).
4. **READ MEMBERS.** `haven item get <ref> --include edges,artifacts` /
   `haven_get_item` per member — read `body`/`why`/`done_looks_like` + edges.
5. **SHARED-CONTEXT ASSESSMENT** (the `haven` workflow-5 heuristic). If members share
   no architecture, contracts, data model, or sequencing, it's a **simple batch**:
   record a one-line "no pack needed" `decision` artifact on the container and exit. If they
   **do** share architecture/contracts/data model, **do not exit — continue to step 6** and
   synthesise the pack (a dispatcher composing this skill is doing so **pack-first**, before any
   member builds). A foundation that is its own **done** node is **read-only `section-3`
   context** in the pack (an external dependency the members consume) — **never** re-asserted as
   a `section-2` write-once requirement.
6. **SYNTHESISE THE PACK** following `references/pack-template.md`: the verify-first
   preamble, foundation/why, cross-cutting requirements & shared behaviour (the
   write-once material), the external-dependency boundary, and a per-leaf
   acceptance **reference** (pointers to each member's `spec` + its live
   `done_looks_like` — never a frozen copy). **Tag every code-level claim `[VERIFY]`** with an explicit
   assumption tied to a code location — you are asserting, not confirming.
7. **WRITE TO THE GRAPH** (additive):
   - groom each member — firm `done_looks_like` and write/firm its `spec` where
     warranted (`haven item update` / `haven_update_item`; wf 3 + wf 10);
   - add **dependency edges** for any real ordering you found (`haven depend` /
     `haven_add_edge {kind:"grouping"|"dependency"}`);
   - write the pack as a `context-pack` artifact `context-pack.md` on the **container**
     (`haven artifact add … --role context-pack --name context-pack.md --replace` /
     `haven_add_artifact {… role:"context-pack", name:"context-pack.md", replace:true}`) —
     **`--replace` is required for idempotent re-prep**: a re-run overwrites the
     container's existing `context-pack.md` in place instead of erroring on the
     `(container, context-pack.md)` collision;
   - set the container's `why` to a one-line pointer at the pack.
8. **HAND OFF.** Report the container ref and tell the next session / plan mode to
   take its `context-pack` artifact `context-pack.md` as input. The two skills meet only at the graph.
   Each prepped leaf now **advertises** its pack: `haven_get_item` / `haven_graph` return a
   derived `context_pack {container, artifact}` pointer, so a dispatcher loads the pack
   *before* building rather than building the member naked. A leaf surfacing a
   `context_pack_clash` must not be built until the clash is resolved.

## Safety — every write is additive

Grouping is a **separate edge layer**: adding a member only inserts a grouping edge
(idempotently) and touches nothing else. So readying a set for dev **adds** grouping
edges, dependency edges, the pack artifact, and filled `done_looks_like`/`why` — and
**removes or rewires nothing**. A node keeps its one decomposition parent (epic) and
any existing release/phase memberships. The *only* operation that ever restructures is
the escape hatch (step 3 / a structurally-wrong brief), which bounces to
`orchestrate-plan` — and that restructures via archive + lineage, **never deletion**.

## The verify-first pack (what plan mode picks up)

Section 0 of every pack is an imperative preamble (verbatim in
`references/pack-template.md`) telling plan mode: treat the pack as **assumptions**;
reality-check each `[VERIFY]` item against the live code **before building**; if an
assumption is wrong but the brief is sound, make correcting Haven the **first
human-approved step** of the plan (write it back to the node, not just the plan doc);
if the brief is **structurally** wrong, **stop and bounce to `orchestrate-plan`**.
Because corrections are Haven ops on the canonical fields, the brief and the graph
never diverge. The same pack is the **doneness yardstick** on the way out (a leaf is
done when its `done_looks_like` + any inherited shared requirement is met).

**Greenfield vs brownfield — what `[VERIFY]` checks against.** In a **brownfield**
batch the members touch existing code, so `[VERIFY]` means *reality-check this
assumption against the live code* before building. In a **greenfield** phase-1 build
there's little or no code yet — the spec is the **primary design artifact**,
establishing the contracts the leaves will conform to — so its `[VERIFY]` items are
**design decisions to lock down with the human**, not facts to confirm against code.
The verify-first discipline is identical (nothing is assumed silently); only what you
check *against* differs — live code, or human sign-off.

## Convergence / fresh-session handoff

Done when the targeted group has a `context-pack.md` on its container (or a "no pack
needed" decision), every member carries concrete `done_looks_like`, and real ordering
is wired. Because all state is in the graph, a cold session re-runs step 0, re-reads
the container + members, and continues — re-running is idempotent (it overwrites its
own `context-pack.md` and grouping/edge inserts are no-ops). v1 ships a manual resume:
`/create-context-pack <container-ref-or-item-refs>`.

## Deferred to v2 / not in this skill

Execution (the code-level plan, dispatch, verification, completion) — that's plan mode
now, and a future `orchestrate-run` later. Also: auto-applying corrections (we
defer-all to the human gate), a `haven`-side pack **projection** command (the skill
assembles the pack; a query verb is future), `gate` containers as pack targets (a gate
is a review, not a build batch), and decomposition-root epics as the primary target
(supported as a documented secondary path via the `parents` walk, but grouping
containers are the v1 primary). These have no extra Haven primitive yet — reason about
them in prose if useful, but don't pretend the graph encodes them.
