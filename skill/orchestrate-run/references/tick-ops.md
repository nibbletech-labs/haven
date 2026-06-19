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
- CLI: `haven artifact get <CONTAINER> --role spec --path context-pack.md -p <P>`
- MCP: `haven_get_artifact {"project":"<P>","ref":"<CONTAINER>","role":"spec"}` → `{path, role, content}`

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

## 7. Gate — compose the `verify` skill (unattended) or plan-mode approval (attended)

No Haven op — the unattended gate **is** the standalone `verify` skill (Mode 1): a fresh verifier
agent given only `done_looks_like` + pack shared-requirements + the diff, running
`build + lint + test` + an acceptance judgment, returning PASS / NEEDS-HUMAN / FAIL + evidence. The
attended gate is a human plan-mode "go". **See `skill/verify` (and `references/dispatch-policy.md`
§GATE) for the verifier contract** — the executor consumes the verdict, it does not re-implement it.

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

## Failure path

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

- **Report the remaining AI queue / human queue:** `haven next --owner ai` / `--owner human`.
- **Container progress:** `rollup_state` rides the step-0 graph read (Dormant|Queued|Active|Done).
- **Follow a stale ref** from a resume note: CLI `haven evolve resolve <ref> -p <P>` · MCP
  `haven_resolve_live {"project":"<P>","ref":"<ref>"}`.
