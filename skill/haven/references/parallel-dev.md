# Parallel dev — worktree, diff-base & verify-gated merge

Read this when you're about to start a substantive build, review a diff in a worktree, or
merge one back — the **manual, human-driven** counterpart of the autonomous git runbook. The
autonomous executor's run-directory, lock, and RECOVER mechanics live in
`skill/orchestrate-run/references/worktree-merge.md`; this file is the **manual practice**, and
the two share one verify gate (`skill/verify`) — neither restates the other. It ships with the
`haven` skill because it's the by-hand discipline for building a batch the `orchestrate-run`
executor would otherwise run on its own.

The cardinal rule: **isolate the work, review it against a base that doesn't move, and let the
verifier — never the builder — decide the merge.**

## One feature = one branch = one dedicated worktree

A shared primary checkout (`<primary>`) is a single directory that **multiple threads and agents
use at once**. Keep it on the integration branch `<base>` and undisturbed:

- **Never `git checkout -b <branch>` or otherwise switch branches in `<primary>`.** A checkout
  swaps the files in that one shared directory — yanking them out from under every other thread
  using it and exposing your uncommitted edits to them. A clean tree on `<base>` does **not**
  make in-place branching safe; isolate regardless.
- **Always spin up a fresh worktree off a clean `<base>`** for each piece of work. Before you
  start, check `git worktree list` + `git status` to see what's already in flight.
- *Creating* a worktree is not the same as *working in one that's already checked out* — that one
  may hold another thread's uncommitted WIP, and building on top intertwines your diff with theirs
  in shared files so it can't be committed apart.

One feature = one branch = one dedicated worktree. For the exact create/cleanup command shapes
(`git worktree add … -b <branch> <base>`, `git worktree remove`, `git branch -d/-D`), see
`skill/orchestrate-run/references/worktree-merge.md` — the principle is here; the runbook owns the
commands.

## Review a diff against the merge-base, never live `<base>`

`<base>` **shifts as parallel threads merge.** So `git diff <base>` — or handing a reviewer or
subagent "the diff vs `<base>`" — silently pollutes your change with **their** merged work, shown
as phantom deletions. Any review built on that diff is untrustworthy.

Compare against a base that doesn't move under you:

- **Uncommitted** → `git diff` (vs your branch HEAD, the fork point), or
  `git diff $(git merge-base <base> HEAD)`.
- **Committed** → `git diff HEAD^ HEAD`, or three-dot `git diff <base>...HEAD` (the merge-base,
  not the tip).

Apply the same rule to any subagent or workflow you hand a diff to — pass it a merge-base diff,
not `git diff <base>`.

## Merge = a soft verification gate (who can reasonably verify this?)

The default is **AI-verify-then-merge, proactively.** If the check is reasonable for the AI to do
— build / lint / test / typecheck, a merge-base diff-review against the contract (above), a
dogfood the AI can actually run — do it and merge; don't leave finished work stranded on a branch.

The gate **is** the `verify` skill's **PASS / NEEDS-HUMAN / FAIL** verdict. See `skill/verify`
(`SKILL.md` + `references/verdict-contract.md`) for the contract — **do not restate it here.** Map
the verdict to a manual action:

- **PASS** → merge. Finished, verified work belongs on `<base>`: branch off `<base>`, commit as you
  go, and on PASS fast-forward/merge it back **from `<primary>` while that stays on `<base>`**
  (`git -C <primary> merge --ff-only <branch>`), then remove the worktree and delete the branch.
- **FAIL** → do **not** merge; route to the **fix path** — the gate *routes*, it does not repair.
  Leave the work in the worktree and send it back to be fixed.
- **The verifier returns NEEDS-HUMAN** (genuinely undecidable — a flaky or unresolvable-environment
  result, or a judgment gap) → **escalate / hand off.** It won't clear on a blind retry; don't
  treat it like a FAIL loop.
- **PASS, but you judge a load-bearing check still needs a person** (behavioural, visual,
  subjective-quality — a call only a human can sign off) → **pause *before* the merge.** Leave the
  work on its worktree (do **not** remove it), say exactly what to check and how to run it *there*,
  and ff-merge only once they've verified. This is the **reserved human gate** — the exception, not
  the default.

The last two both pause, but for different reasons by different routes — verifier-undecidable vs
your-own-judgment — so keep them distinct; don't collapse them onto one verdict.

**The gate is independent of the builder.** The verifier is never the build agent reviewing its
own work — that holds even when a human is the verifier (see `skill/verify` `SKILL.md`). The
**autonomous** version of this same gate is `skill/orchestrate-run/references/dispatch-policy.md`
(§GATE) — it reaches for the deterministic verifier precisely *because no human is in the loop*;
this file is the human-in-the-loop counterpart of the same gate, not a subordinate of it.

## Anti-patterns

- **Branching in-place in a shared checkout.** `git checkout -b` in `<primary>` swaps files under
  every other thread using it. Spin up a worktree.
- **Building on whichever worktree is already checked out.** It may hold another thread's WIP; your
  diff entangles with theirs.
- **Reviewing — or handing a subagent — `git diff <base>`.** `<base>` moved; you're reading phantom
  deletions. Use the merge-base.
- **The build agent self-reviewing as the gate.** The verifier is never the builder.
- **Merging past a genuine NEEDS-HUMAN, or treating it as a FAIL-style blind retry.** Escalate; it
  won't clear on its own.
- **Removing the worktree before a paused human check has happened.** The work has to stay put for
  them to verify it.
- **Restating the `verify` verdict definitions, or the autonomous lock→rebase→re-gate→ff sequence,
  here.** Point at the contract and the runbook; don't copy them.
