# Verify-ops — the exact CLI and MCP call for each flow step

The canonical argument reference is the `haven` skill's `references/surface-map.md`
(CLI⇄MCP differences). This file is `verify-acceptance`'s per-step cheat-sheet. `<P>` = the project key.

> **Two surfaces, one contract.** Locally use the `haven` CLI; remotely use the `haven_*`
> MCP tools. **Over MCP there is no sticky session — pass `project` on every call.**
> **No new verbs or roles:** every op below already exists. There is **no `verification`
> role** — verdict/PASS evidence rides **`delivery`**, escalation rides **`scratch`**.

## 0. Reorient — the whole graph in one read

- CLI: `haven graph -p <P>`
- MCP: `haven_graph {"project":"<P>"}`

Returns live nodes (compact: ref, title, type, status, a **`has_acceptance`** flag — add **`include_acceptance:true`** to ride each node's **`done_looks_like`** text) + edges +
per-leaf `context_pack {container, artifact}`. Resolve the project first if unknown: CLI
`haven project list`; MCP `haven_list_projects`.

## 1. Resolve the target — live acceptance + the pack (when present)

Read the target's **live `done_looks_like`** and its derived `context_pack` pointer:

- CLI: `haven item get <ref> --include edges,artifacts -p <P>`
- MCP: `haven_get_item {"project":"<P>","ref":"<ref>","include":["edges","artifacts"]}`

If it carries a `context_pack`, load the container's pack for the **shared-requirements**
section (an **input**, not a precondition — a leaf with no pack is judged on its own
`done_looks_like`):

- CLI: `haven artifact get <CONTAINER> --role context-pack --path context-pack.md -p <P>`
- MCP: `haven_get_artifact {"project":"<P>","ref":"<CONTAINER>","role":"context-pack"}` → `{path, role, content}`

> A **container** target is a verdict-only **rollup** — never auto-completed.

## 2–4. Evidence base, suite, judgment (reasoning — no Haven op)

Assemble the **diff** under test (the leaf's branch/worktree, or an explicit diff/branch
the caller names — **never** the build agent's narrative). Run `build + lint + test`
(exit-0). Judge acceptance independently against the live `done_looks_like` + shared
requirements. Verdict per `references/verdict-contract.md`.

## 5–6. Write the verdict — per the dial

### Verdict-only (Posture A, default) — write `verdict.md`, touch no status

- CLI: `haven artifact add <ref> --role delivery --name verdict.md --content "<verdict + evidence>" --replace -p <P>`
- MCP: `haven_add_artifact {"project":"<P>","ref":"<ref>","role":"delivery","name":"verdict.md","content":"<…>","replace":true}`

> `--replace` keeps re-verify **idempotent** — a second run overwrites its own `verdict.md`
> instead of colliding.

### Auto-complete (Posture B, opt-in, leaves only, unambiguous PASS only)

Run the existing completion path; the evidence artifact **defaults to `--role delivery`**.
Returns `unblocked[]` (the items this completion freed):

- CLI: `haven item complete <ref> --evidence "<what was built + verifier result>" -p <P>`
- MCP: `haven_complete_item {"project":"<P>","ref":"<ref>","evidence":"<…>"}`

### NEEDS-HUMAN / FAIL — escalate (any posture)

**a. Append a fix-log entry on the CONTAINER** (append-only; strikes = entry count):

- CLI: `haven artifact add <CONTAINER> --role scratch --name fix-log.md --content "<verdict: what failed + gate excerpt / acceptance gap>" -p <P>`
- MCP: `haven_add_artifact {"project":"<P>","ref":"<CONTAINER>","role":"scratch","name":"fix-log.md","content":"<…>"}`

**b. Hand off to a human** — self-evicts the item from `next --owner ai`:

- CLI: `haven item handoff <ref> --to human --wait on_human --note "<verdict + evidence excerpt>" -p <P>`
- MCP: `haven_handoff {"project":"<P>","ref":"<ref>","to":"human","wait":"on_human","note":"<…>"}`

> Inside `orchestrate-run`, the executor owns completion (step 9) and the N-strike ceiling;
> this skill returns the verdict and the executor consumes it. Ad hoc, the dial above
> decides whether `verify-acceptance` itself completes or simply records the verdict.

## Mode 2 (browser) write-back — same ops, same dial

A **browser (Mode 2)** verdict writes back through the **exact same ops and dial** as Mode 1
— a `delivery`-role `verdict.md` (Posture A, default), or `complete_item` on an unambiguous
clean PASS with the dial explicitly on, or the fix-log + `handoff` escalation on
NEEDS-HUMAN / FAIL. The verdict body carries the browser-mode rung (PASS / PASS-WITH-ISSUES /
NEEDS-HUMAN / FAIL) and the screenshot / console evidence per `references/browser-mode.md`.
**Until the HV-261 attended proving record exists, default browser verdicts to verdict-only
(Posture A)** — auto-complete on a browser PASS is not yet earned, so the human/dispatcher
completes.
