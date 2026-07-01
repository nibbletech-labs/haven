# Worktree + merge — the git runbook

Everything Haven deliberately does **not** model. Haven owns work-grain nodes and their
state; **git owns the code and its integration.** This file is pure runner-convention —
no Haven schema, no Haven op. `<C>` = a batch container ref (e.g. `HV-12`); `<base>` =
the integration branch (usually `main`).

## Layout

- One worktree per in-flight batch, under a run directory: `.haven-run/<C>/`.
- One branch per batch: `run/<C>`, always created **off current `<base>`**.
- One merge **lockfile**: `.haven-run/.merge-lock` — held for the whole
  rebase→re-gate→ff sequence, released after. This is the entire concurrency control for
  integration; there is no Haven primitive for it.

## Create a worktree (step 6)

```
git worktree add .haven-run/<C> -b run/<C> <base>
```

Spawn exactly one build agent with its cwd inside `.haven-run/<C>`. The agent commits its
work on `run/<C>` and **must write a done-marker** into its final commit so RECOVER can tell
"already built" from "crashed mid-build": a trailer line

```
Haven-Batch: <C> members=<ref,ref,...>
```

(or an equivalent marker file the agent always writes). The marker is the only signal that a
worktree's branch is a complete, gate-passing build versus a half-finished one.

## Gate in the worktree (step 7)

The verifier agent runs against `run/<C>` **before any merge**: `build + lint + test` must
exit 0, plus acceptance judged against each member's `done_looks_like`. A fail never leaves
the worktree.

## Merge — serialized, rebase, MANDATORY re-gate, ff (step 8)

Acquire the lock, then for the one batch holding it:

```
# 1. acquire (block/spin until free; one merge at a time, even at MAX_PARALLEL>1)
#    e.g. mkdir .haven-run/.merge-lock   (atomic create = the lock)
# 2. rebase onto whatever main is NOW (a sibling may have merged since this batch branched)
git -C .haven-run/<C> fetch <base> 2>/dev/null; git -C .haven-run/<C> rebase <base>
# 3. RE-GATE post-rebase — re-run the deterministic gate on the rebased tree.
#    This is INVIOLABLE: it catches semantic conflicts a clean textual merge hid.
#    Only a green re-gate proceeds.
# 4. fast-forward main to the rebased branch
git -C <base-worktree> merge --ff-only run/<C>
# 5. release the lock (rmdir .haven-run/.merge-lock), then COMPLETE the leaves (tick step 9)
```

**Rebase conflict** the build agent can't cleanly resolve, **or a red re-gate** → abort the
rebase (`git rebase --abort`), do **not** merge, **release the lock**, and send the batch to
the failure path (fix-log + retry/strike, `references/tick-ops.md`). `<base>` and every
sibling worktree are untouched. Completion is bound to a landed merge, so the graph's `done`
always reflects what is actually on `<base>`.

> **Why serialize + re-gate (the sharpest risk).** Two batches that share no dependency edge
> can still touch the same module/config. They each pass their own in-worktree gate in
> isolation; the collision only surfaces at integration. Rebase resolves the textual case;
> the **post-rebase re-gate** is the only defense against the semantic case (merges clean,
> breaks at runtime). Skip it "to save time" and you ship green-per-worktree code that is
> red-on-`<base>` — and because the loop is stateless, it has no memory anything is wrong.

## Cleanup

After a successful merge **and** its completes, remove the worktree:

```
git worktree remove .haven-run/<C>        # --force only if you've confirmed it's spent
git branch -d run/<C>                      # -D after a discarded/failed batch
```

## RECOVER — reconcile graph vs disk (tick step 0, before dispatch)

`git worktree list` + the graph's `in_progress` leaves are the two sources of truth:

| graph leaf | worktree on disk | live build agent | action |
|---|---|---|---|
| `in_progress` | yes | yes | healthy in-flight batch (from a prior tick) — leave it |
| `in_progress` | yes, branch has the done-marker, gate passes | no | crashed after build — resume at MERGE (step 8) |
| `in_progress` | yes, no/partial marker | no | crashed mid-build — `git worktree remove --force` + prune, send batch to failure path (strike count survives in the container fix-log) |
| `in_progress` | none | no | crashed after claim before spawn (or after merge before complete) — if the work is already on `<base>` (marker in `<base>` history) `complete` it; else reset `status: ready` and re-dispatch |
| (none) | yes | — | stale worktree — `git worktree remove --force` + `git worktree prune` |

Always `git worktree prune` first to clear dead administrative entries. Every crash window —
after claim, mid-build, after gate, after merge before complete — lands in one of these rows
and is fixed idempotently. The graph is truth; worktrees are reconcilable cache.

## Same path at any `MAX_PARALLEL`

At `MAX_PARALLEL=1` there is at most one worktree and the lock is never contended, but the
**exact same** create → gate → lock → rebase → re-gate → ff → complete path runs — and **even that
one batch still gets its own worktree; never build in `<base>`** (invariant 4: at serial the
isolation reads like overhead, which is exactly when it gets skipped and corrupts the primary
checkout). Fanning out (`MAX_PARALLEL>1`) changes only how many worktrees exist at once and whether
the lock is contended — not the merge discipline, which is proven from the first serial run.
