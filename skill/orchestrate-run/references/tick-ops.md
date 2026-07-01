# Tick ops — the exact CLI and MCP call for each step

The canonical argument reference is the `haven` skill's `references/surface-map.md`
(CLI⇄MCP differences). This file is `orchestrate-run`'s per-step cheat-sheet. `<P>` =
the project key. The git side (worktree create / merge / prune) is in
`references/worktree-merge.md`; the effort/gate/strike knobs are in
`references/dispatch-policy.md`.

> **Two surfaces, one contract.** Locally use the `haven` CLI; remotely use the
> `haven_*` MCP tools. **Over MCP there is no sticky session — pass `project` on every
> call.** **No batch over MCP:** `haven_update_item` / `haven_complete_item` /
> `haven_add_artifact` are one entity per call; loop.

## 0. Reorient — the whole graph in one read

- CLI: `haven graph -p <P>`
- MCP: `haven_graph {"project":"<P>"}`

Returns live nodes (compact: ref, title, type, status, committed, owner, priority, wait,
`done_looks_like`) + a flat edge list `{kind, from, to}` + per-container `rollup_state` +
**per-leaf `context_pack {container, artifact}`** (and `context_pack_clash`). This single
read drives the whole tick. Resolve the project first if unknown: CLI `haven project list`;
MCP `haven_list_projects`. (RECOVER reconciles this against `git worktree list` — see
`references/worktree-merge.md`.)

**Large-graph fallback.** Over MCP, `haven_graph` is bounded by default and reports
`totals`, `omitted`, `limits`, and `truncated` for nodes, edges, and lineage. If
`truncated:true` means the graph slice is not enough for this tick, reorient from
the **frontier** instead:

- RECOVER set (the `in_progress` leaves to reconcile): `haven list_items --status in_progress --owner ai -p <P>` / `haven_list_items {"project":"<P>","status":"in_progress","owner":"ai"}`
- Dispatch queue: step 1 (`haven next --owner ai`, bounded by default — HV-194).
- Per-batch context: read only each **active container's** context-pack (steps 2/4), not every node.

This is the same tick over a smaller bounded slice. It's what you fall back to until
a scoped graph read (HV-195) lands.

## 1. Frontier — the AI dispatch queue

- CLI: `haven next --owner ai -p <P>`
- MCP: `haven_next {"project":"<P>","owner":"ai"}`

Returns exactly the committed + `ready` + ≠anchor + `wait_state` NULL + no-open-dependency
leaves owned by `ai`. Human-owned and dependency-blocked work is already excluded — do not
filter further.

## 2–3. Group + select (pure reasoning on step-0 — no op)

Fold the frontier two ways: **packed** leaves by `context_pack.container` →
`{container → [ready leaves]}` (skip any `context_pack_clash`, surface it); **packless** leaves
(NULL container, no fold key) tentatively by a **shared `depends_on` producer** → a packless
cluster sharing one routes to step 4 (pack-first). **Never** fold by decomposition parent. A
batch is dispatchable iff every member is in the step-1 frontier. Take up to `MAX_PARALLEL`
independent batches.

## 4. Ensure-packed — compose create-context-pack BEFORE claiming (pack-first)

A selected multi-leaf cluster whose members share an architecture but carry **no**
`context_pack` pointer: invoke `create-context-pack` on the **member-ref set — before claiming
any member** — then re-tick (the leaves will then carry a pointer and fold into one batch).
Pass **only the member refs**; `create-context-pack` creates/reuses the container and wires the
grouping. Read a container's pack back with:
- CLI: `haven artifact get <CONTAINER> --role context-pack --path context-pack.md -p <P>`
- MCP: `haven_get_artifact {"project":"<P>","ref":"<CONTAINER>","role":"context-pack"}` → `{path, role, content}`

## 5. Claim — soft-claim every member (claim before spawn)

One call per leaf; this drops it from `next`:
- CLI: `haven item update <ref> --status in_progress -p <P>`
- MCP: `haven_update_item {"project":"<P>","ref":"<ref>","status":"in_progress"}`

## 6. Dispatch — hand the build agent its inputs (it never writes Haven)

Read the pack + members once, then spawn the agent into its worktree
(`references/worktree-merge.md`) with: the `context-pack.md` content (step 4), each member's
`done_looks_like`, the effort/gate per `references/dispatch-policy.md`, and a per-leaf
self-check derived from `done_looks_like`. Read member detail if the compact node is thin:
- CLI: `haven item get <ref> --include edges,artifacts -p <P>`
- MCP: `haven_get_item {"project":"<P>","ref":"<ref>","include":["edges","artifacts"]}`

**Verification recipe — derive it BEFORE dispatch, type it by the leaf's activity.** The per-leaf
self-check is **2–5 executable steps** derived from the leaf's `done_looks_like` + the codebase
context (test commands, dev server, linting, CI scripts) — concrete steps the agent runs to
confirm its output works *before* signalling done. Type them by what the leaf actually does:

- **Code:** run the tests, typecheck, start the dev server + curl the new path.
- **Research:** check the source count, contradiction coverage.
- **Writing:** review the structure against the format spec.
- **Visual:** screenshot, check dimensions/format.

If no concrete verification is possible, specify *what to review against the quality bar*. The same
recipe is what the step-7 verifier re-runs independently (`references/dispatch-policy.md` § GATE /
§ self-check); a green global build is not proof a specific leaf's acceptance is met.

**Plan-gate ops (plan-gate on — `references/dispatch-policy.md` § PLAN-GATE).** For a complex /
ultracode batch a **read-only plan agent** writes its plan to the **container** (6a); you read it
back for a **fresh** validator that judges the tick's plans **as a whole** (6b). Plan agent writes /
you read for the gate:
- CLI: `haven artifact add <CONTAINER> --role scratch --name build-plan.md --content "…" -p <P>` ·
  read: `haven artifact get <CONTAINER> --role scratch --path build-plan.md -p <P>`
- MCP: `haven_add_artifact {"project":"<P>","ref":"<CONTAINER>","role":"scratch","name":"build-plan.md","content":"…"}` ·
  read: `haven_get_artifact {"project":"<P>","ref":"<CONTAINER>","role":"scratch"}`

On **APPROVE** spawn a **fresh** build agent (6c) handed the approved `build-plan.md` as its brief +
the pack + `done_looks_like` — the coordinator carries the context; the loop keeps no agent alive
across the gate. **REVISE** re-spawns a plan agent with the gaps + the prior plan to rewrite, then
re-gate; **REJECT** routes to the failure / replan path. A **mechanical** batch skips this and builds
directly.

## 7. Gate — compose the `verify-acceptance` skill (unattended) or plan-mode approval (attended)

No Haven op — the unattended gate **is** the standalone `verify-acceptance` skill (Mode 1): a fresh verifier
agent given only `done_looks_like` + pack shared-requirements + the diff, running
`build + lint + test` + an acceptance judgment, returning PASS / NEEDS-HUMAN / FAIL + evidence. The
attended gate is a human plan-mode "go".

**Forward `verify-acceptance`'s contract into the verifier's prompt** (it inherits no skill — why:
`references/dispatch-policy.md` § GATE). Inline, from `skill/verify-acceptance`: the
PASS / NEEDS-HUMAN / FAIL definitions (`references/verdict-contract.md`), the independence rule
(judge from `done_looks_like` + shared-requirements + diff only), and the exhaustive
acceptance-clause walk + lens (`references/evaluation-lens.md`). **Collect the verdict explicitly** —
an idle signal means *fetch the verdict*, never proceed on an absent one.

## 8. Merge — serialized lock → rebase → re-gate → ff

Entirely git, no Haven op — `references/worktree-merge.md`. Completion (step 9) fires only
after the work is on `main`.

## 9. Complete + replan

**a. Complete each merged leaf** (returns `unblocked[]` — the items this completion freed):
- CLI: `haven item complete <ref> --evidence "<what was built + verifier result>" -p <P>`
- MCP: `haven_complete_item {"project":"<P>","ref":"<ref>","evidence":"<…>"}`

**b. Record a non-obvious integration decision** (so a downstream batch reads it):
- CLI: `haven artifact add <ref> --role delivery --name delivery.md --content "<decision>" -p <P>`
- MCP: `haven_add_artifact {"project":"<P>","ref":"<ref>","role":"delivery","name":"delivery.md","content":"<…>"}`

**c. Replan a contradicted downstream leaf** — do not build it stale; bounce to
`orchestrate-plan` (re-invoke it on that branch). If it's clearly moot, archive with a
rationale via lineage (never delete):
- CLI: `haven item archive <ref> --rationale "<superseded by <producer>'s outcome>" -p <P>`
- MCP: `haven_archive {"project":"<P>","ref":"<ref>","rationale":"<…>"}`

**d. Replan damping — WHEN NOT to replan.** Not every completion is a replan event; a naive loop
re-plans constantly and never converges. Calibrate the response to the *size* of what the
completion changed:

- A **minor subtask** completing as expected → **record the evidence and continue.** No
  reassessment. The completion's `unblocked[]` is the only thing you act on.
- A completion whose evidence **contradicts** a downstream leaf's `done_looks_like` or makes it
  moot → reassess **only that branch** (§ 9c above) — bounce the contradicted leaf to
  `orchestrate-plan` or archive it; leave the rest of the graph alone.
- A **whole-track-completing** event (a foundation merged, an architecture decided, a research
  leaf that changed the landscape) → **full reassessment** of what remains against the goal: are
  planned leaves now unnecessary, are there gaps, did the approach shift? It is also a **meaty
  checkpoint** → run the code-review + drain cycle (`references/dispatch-policy.md` § CHECKPOINTS;
  SKILL § tick 9). A **minor subtask never triggers a checkpoint** — meaty only.

The default is the cheap path (record + continue); escalate to reassessment only on the contradict
/ whole-track triggers. The frontier predicate already steps around blocked work, so most ticks
need no replanning at all.

**e. Checkpoint ops (meaty only — `references/dispatch-policy.md` § CHECKPOINTS).** Non-blocking nits
are appended to the container **punch-list** (by the verifier at step 7) and read back at the
checkpoint — same ops as the fix-log, just `--name/--path punch-list.md`. Drain via one batched fix
pass (steps 6–8). Promote an **undrained survivor** at convergence to a floating item:
- CLI: `haven item add "<nit>" --type task --assign ai --priority 4 --why "<HV-nn punch-list>" -p <P>`
- MCP: `haven_add_item {"project":"<P>","title":"<nit>","owner":"ai","priority":4}`

## Failure path

**Minor vs major first.** A *mechanical / deterministic* issue the verifier fixed inline (§ 7;
`references/dispatch-policy.md` § GATE) is **not a fail** — no fix-log, no strike, the batch
proceeds. The steps below are for a **major** fail: the verifier's **fix plan** (a `scratch` note)
feeds a **fresh fix agent** dispatched through the plan-first pipeline (§ 6), and it takes a strike.

**a. Append a fix-log entry on the batch CONTAINER** (append-only; strikes = entry count):
- CLI: `haven artifact add <CONTAINER> --role scratch --name fix-log.md --content "<strike N: what failed + gate excerpt>" -p <P>`
- MCP: `haven_add_artifact {"project":"<P>","ref":"<CONTAINER>","role":"scratch","name":"fix-log.md","content":"<…>"}`

**b. Retry** — put the `in_progress` failed leaf back on the frontier (cheap path):
- CLI: `haven item update <ref> --status ready -p <P>`
- MCP: `haven_update_item {"project":"<P>","ref":"<ref>","status":"ready"}`

  > `haven item reopen` / `haven_reopen` resets to **discovery**, not ready — a reopened leaf must
  > be re-groomed (and re-committed) before re-dispatch. Reserve it for leaves the run
  > archived/superseded; for a simple retry use `status: ready`.

**c. Escalate at the N-strike ceiling** — hand the batch to a human; it self-evicts from
`next --owner ai` (DISPATCHABLE gates on `wait_state` NULL + owner):
- CLI: `haven item handoff <ref> --to human --wait on_human --note "<fix-log summary + last gate output>" -p <P>`
- MCP: `haven_handoff {"project":"<P>","ref":"<ref>","to":"human","wait":"on_human","note":"<…>"}`

## Convergence-time ops

- **Promote undrained punch-list survivors** to floating items (§ 9e) so no non-blocking finding is lost.
- **Report the remaining AI queue / human queue:** `haven next --owner ai` / `--owner human`.
- **Container progress:** `rollup_state` rides the step-0 graph read (Dormant|Queued|Active|Done).
- **Follow a stale ref** from a resume note: CLI `haven evolve resolve <ref> -p <P>` · MCP
  `haven_resolve_live {"project":"<P>","ref":"<ref>"}`.
