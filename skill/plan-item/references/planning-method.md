# Planning method — plan mode's process, without plan mode

Read this at **steps 3–5**, when you're working out the approach. It covers why running
this skill inside a native plan mode loses the artifact, and how to get plan mode's rigour
without it.

A native plan mode is worth learning from and **wrong to run this skill inside**. Take the
discipline; leave the mode.

### Why not the mode itself

- **Its plan lands in the harness's own plan file, not the graph.** Getting it onto the
  item is a separate step, afterwards.
- **Exiting is the moment control flips to building.** The approval options are framed as
  *execute the plan*, so code starts immediately and that separate step is the one that
  silently gets skipped. The artifact is this skill's only output, so losing it loses
  everything.
- **What's approved isn't the artifact.** The person approves the code-grain plan on
  screen, not the spec that would land on the item — two documents, one of them unagreed.
- **The endpoints are opposite.** This skill stops *at* a plan on an item; plan mode is
  built to *start* a build. Composed, one of them loses, and in practice it's this one.

Plan mode keeps its place **one step later**: once the spec exists and the item is
`ready`, it's the right human gate on the *code* plan, because the durable artifact is
already safe in the graph and what's being gated is the build.

### The process, run inline

Plan mode's value was never the mode — it's the discipline it imposes. That discipline is
written out once, for every consumer, in the `haven` skill's
**`references/code-planning-method.md`** — read it now; it is the method, and the list
below is only how this skill applies it.

1. **Read the code before you propose.** Follow the method: trace the path the behaviour
   being changed actually takes, find the similar feature already in the repo, hunt for
   the functions and utilities to reuse, look at how the area is tested. Reasoning from
   the request alone is the failure the method exists to prevent.
2. **Explore first, then hand the architect its brief.** Where the harness has a read-only
   architect agent (Claude Code's `Plan` type), it is a designer that expects requirements
   and a perspective — not an explorer to send off with a bare ref. Explore inline or
   with a read-only search agent per focus, then hand the architect the requirements,
   constraints, the traces and filenames you found, and one perspective; it returns a
   design and *you* write the spec, so the artifact still lands on the item. No such
   agent? Do the design inline; the method is the whole of it.
3. **Weigh more than one approach**, along the axes the method names for the task type,
   and record in the spec which you took and why the runner-up lost. One line saves the
   whole argument being had again in three weeks.
4. **Write at spec altitude.** The spec keeps the seams the trace found (entry point,
   landing layer, effect — by file), the similar feature and the reuse targets by path,
   and the chosen approach. **Name the real files** — the shippability linter demands it,
   and it is the fastest tell that the reading actually happened. Sequencing, anticipated
   challenges, and the test-pattern note go to the build checklist
   (`references/build-handoff.md`) on a one-pass call; the file-by-file edit list never
   goes in the spec — it's stale the moment code moves, and belongs to the build session.

Do not call `EnterPlanMode` to get the native version of this: entering the mode is the
thing this whole section exists to avoid, and its tool description is only the thin
summary — the discipline lives in the reminders the mode injects, which is what
`code-planning-method.md` transcribes.
