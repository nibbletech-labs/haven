# Tick ops — the exact CLI and MCP call for each step

The canonical argument reference is the `haven` skill's `references/surface-map.md`
(CLI⇄MCP differences). This file is the planner's per-step cheat-sheet, and is meant
to be reused verbatim by the v2 executor. `<P>` = the project key.

> **Two surfaces, one contract.** Locally use the `haven` CLI; remotely use the
> `haven_*` MCP tools. **Over MCP there is no sticky session — pass `project` on
> every call.** **No batch over MCP**: `haven_add_item` / `haven_add_edge` /
> `haven_update_item` are one entity per call; loop. The CLI can pack (`depend …
> --on a --on b`), but the planner never depends on that.

## 0. Reorient — the whole graph in one read

- CLI: `haven graph -p <P>`
- MCP: `haven_graph {"project":"<P>"}`

Returns live nodes (compact: ref, title, type, status, committed, owner, priority,
wait — **and `done_looks_like`**, which the graph view includes when you pass `include_acceptance:true`, so you can run the
sealed-leaf test from this one read) plus a flat edge list `{kind, from, to}`. Pass
`all:true` only if you need superseded/archived nodes.

Resolve the project first if unknown: CLI `haven project list`; MCP
`haven_list_projects`.

## 1. Ensure the decomposition root (anchor), idempotently

- CLI: `haven item add "<goal>" --type anchor --why "<provenance>" -p <P>`
- MCP: `haven_add_item {"project":"<P>","title":"<goal>","type":"anchor","if_absent":true}`

`if_absent` matches a normalized title **project-wide** and returns `existing:true`
on a hit — safe here because the goal title is unique. Skip this step entirely if a
root already exists.

## 4. Read one node's detail (only if the compact node is insufficient)

- CLI: `haven item get <ref> --include edges,artifacts -p <P>`
- MCP: `haven_get_item {"project":"<P>","ref":"<ref>","include":["edges","artifacts"]}`

## 6a. Create each child (one decomposition edge per child)

- CLI: `haven item add "<specific child title>" --parent <ref> --type <task|code|research|design|admin> -p <P>`
- MCP: `haven_add_item {"project":"<P>","title":"<specific child title>","parent":"<ref>","type":"<…>"}`

One call per child. **Do NOT use `if_absent` on children** — its title match is
project-wide, so a generic child title ("Design", "Checkout") would wrongly dedupe
against an unrelated same-named node elsewhere. Give children **specific** titles
("Storefront — checkout flow"). To reuse an existing node that several branches
need, don't recreate it — wire a dependency edge to it (6b).

## 6b. Wire dependency edges (where one child's output feeds another)

- CLI: `haven depend <consumer> --on <producer> -p <P>`  (repeat `--on` for several producers in one call)
- MCP: `haven_add_edge {"project":"<P>","kind":"dependency","from":"<consumer>","to":"<producer>"}`  (one edge per call)

Direction: `from` / the positional `<consumer>` is the **blocked** node; `to` /
`--on <producer>` is the **blocker**. The store **rejects a cycle with an error** —
don't pre-check; if an edge errors, you have the direction backwards or the edge is
redundant — fix or drop it and move on.

## 6c. Record a split rationale when non-obvious

- CLI: `haven item update <ref> --why "<why this split>" -p <P>`  (or an artifact:)
- CLI: `haven artifact add <ref> --role decision --content "<rationale>" --name decision.md -p <P>`
- MCP: `haven_update_item {"project":"<P>","ref":"<ref>","why":"<…>"}`  or
  `haven_add_artifact {"project":"<P>","ref":"<ref>","role":"decision","content":"<…>","name":"decision.md"}`

## 7. Defer a branch (knowability horizon)

The node should split but its children aren't knowable until an upstream output
lands. Ensure the producing node exists, wire the dependency, and park this node
`blocked` — don't decompose it. It re-enters the frontier when the dependency is
`done`.

- CLI: `haven depend <node> --on <producer> -p <P>` then `haven item update <node> --status blocked --why "decompose after <producer> lands" -p <P>`
- MCP: `haven_add_edge {"project":"<P>","kind":"dependency","from":"<node>","to":"<producer>"}` then `haven_update_item {"project":"<P>","ref":"<node>","status":"blocked","why":"decompose after <producer> lands"}`

Do **not** seal it (no `done_looks_like`, no commit, no owner) — it isn't a unit of
work, it's a placeholder for planning that happens later.

## 8. Seal a leaf — acceptance + maturity + commitment + owner

- **MCP — one call:**
  `haven_update_item {"project":"<P>","ref":"<ref>","done_looks_like":"<concrete, testable>","status":"ready","commit":true,"priority":<0-4>,"assign":"ai"}`
- **CLI — three calls** (`item update` has no `--commit`/`--assign`):
  1. `haven item update <ref> --done-looks-like "<concrete, testable>" --status ready -p <P>`
  2. `haven item commit <ref> --priority <0-4> -p <P>`
  3. `haven item assign <ref> --to ai -p <P>`  (use `--to human` for a real-world task)

> **Not atomic.** The single MCP `haven_update_item` runs three sequential store
> writes (update → commit → assign), each bumping the revision — it is a best-effort
> composition, **not** one transaction. A crash between them half-seals a leaf (e.g.
> `ready` but uncommitted). That's fine: the stateless reorient sees a half-sealed
> node as still on the frontier and re-seals it next tick. A leaf **must** carry
> `done_looks_like`, or `ready` is meaningless.

## 9. Gate (deferred unless the user asks)

- CLI: `haven item add "<batch> review" --type gate --done-looks-like "<pass criteria>" -p <P>` then `haven depend <gate> --on <leaf> -p <P>` per reviewed leaf
- MCP: `haven_add_item {"project":"<P>","title":"<batch> review","type":"gate","done_looks_like":"<…>"}` then `haven_add_edge {"project":"<P>","kind":"dependency","from":"<gate>","to":"<leaf>"}` per leaf

A gate surfaces in `next` only once **every** reviewee is `done`; it can be created
before they complete.

## Convergence-time ops

- **Report the dispatch queue:** CLI `haven next --owner ai -p <P>` · MCP `haven_next {"project":"<P>","owner":"ai"}`
- **Capture a coverage gap** (floating, uncommitted — don't fabricate work): CLI `haven item add "<gap>" -p <P>` · MCP `haven_add_item {"project":"<P>","title":"<gap>"}`
- **Follow a stale ref** found in a resume note: CLI `haven evolve resolve <ref> -p <P>` · MCP `haven_resolve_live {"project":"<P>","ref":"<ref>"}`

## Content over MCP (no filesystem)

When a leaf needs a spec/notes and you're remote, write via the artifact **content**
channel (`haven_add_artifact {…,"content":"…","name":"…"}`) — the server writes the
bytes. Locally you *may* edit files under `~/.haven/<P>/items/<ref>/` directly and
register them with `haven artifact add … --file`, but never assume a filesystem.
