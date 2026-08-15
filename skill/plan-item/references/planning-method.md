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

Plan mode's value was never the mode — it's the discipline it imposes. Run that here:

1. **Explore before you propose.** Glob / grep / read the areas the work touches, rather
   than reasoning from the request alone. **Name the real files in the spec** — the
   shippability linter demands it, and it's the fastest tell that the exploration
   actually happened.
2. **Follow the patterns already in the codebase.** A plan that invents a second way to do
   something the repo already does is a worse plan, however tidy it reads.
3. **Weigh more than one approach** where the task admits several, and record in the spec
   which you took and why. One line about the rejected alternative saves the whole
   argument being had again in three weeks.
4. **Delegate the exploring to a read-only planning subagent** where the harness has one.
   In Claude Code that's the `Plan` agent type: architect-grade, every write tool removed,
   and it **returns** the plan to you instead of taking over the session. That is how you
   get plan mode's rigour without plan mode's exit — the subagent thinks, *you* write the
   spec, and the artifact still lands on the item. No such agent? Do it inline; the points
   above are the whole method.
5. **Stay at spec altitude.** The output is the item's `spec`, not a file-by-file edit
   list. That last layer is stale the moment code moves, and belongs in the build session
   rather than the graph.

**Prefer your harness's own version of this where you can read it without entering the
mode.** The list above is transcribed from what a native plan mode asks for, and it's
written out here so the method survives on a harness that has no plan mode at all — but
it's a snapshot, and first-party guidance moves. In Claude Code the `EnterPlanMode` tool
description is readable directly (via `ToolSearch`) and carries the current version; read
it and follow it where it's richer than the list. **Do not call `EnterPlanMode` to find
out** — reading the guidance is free, entering the mode is the thing this whole section
exists to avoid.
