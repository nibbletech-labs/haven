# Build handoff — the checklist and the review

Read this at **step 5–6**, on the **one-pass** branch only, when you're writing the spec
that's about to be built. A plan headed for `orchestrate-plan` needs none of it.

## The build checklist

One item can hold a lot of work, so a spec that's going to build ends with an ordered
**build checklist** — the route through the work, as tickable lines. (A plan headed for
decomposition gets none: its steps aren't known yet.)

```markdown
## Build checklist
- [ ] Add the `source_kind` column and its migration
- [ ] Wire the TVDB client behind the existing lookup interface
- [ ] **REVIEW** — fresh-eyes code review of the schema and the lookup contract,
      before anything is built on top of them
- [ ] Backfill artwork for titles already scanned
- [ ] Show per-title match status in the library view
- [ ] **REVIEW** — fresh-eyes code review of the full diff
- [ ] Tick each box here as it lands — this is how progress is reported
```

- **Ordered, and every line a visible unit of progress.** Aim for steps a person could
  watch tick past. Not "implement the feature" (one box is not a checklist), and not
  twenty micro-edits (that's a diff, not progress).
- **Review points are checklist lines**, so they're tracked and ticked like the rest of
  the route. How many, and where, is *Review* below.
- **It's the route, not the destination.** `done_looks_like` stays the outcome test and
  the thing verification judges. A ticked checklist is **not** evidence the item is
  done — never let the checklist stand in for acceptance.
- **Tell the builder to tick it in the file as they go**, in the spec itself. Where the
  harness has its own to-do list, mirror the checklist into it — but `spec.md` is the
  durable record and the one the person can read without asking anyone.
- **Chunky items are exactly why this exists.** A five-stage ticket with no checklist is
  opaque to the person and easy for the builder to lose its place in. This is the price
  of keeping items big, and it's a cheap one.

## Review — a one-pass build still gets fresh eyes

`orchestrate-run` gates every leaf with a **separate** verifier before it merges. A
single item built in one pass skips that machinery, and rightly so — standing up an
orchestrator for one ticket is pure overhead. It must not skip the **judgment**. So the
plan says so, in the spec, where whoever builds it will actually read it:

- **At least one code review, always.** Before the item completes, a review agent that
  **did not write the code** reviews the diff. Fresh eyes is the whole point: the agent
  that just built it re-reads its own intent instead of the code in front of it. In
  Claude Code that's `/code-review` or a review subagent; use whatever your harness
  offers, so long as it isn't the builder.
- **More than one when the work has real checkpoints.** Put extra `REVIEW` lines in the
  build checklist at the boundaries that matter — a migration landing, an API contract
  going in, a flow becoming usable end to end. Four reviews of 400 lines beat one review
  of 1,600, and a checkpoint review catches a wrong foundation *before* three more stages
  get built on it. A chunky item with no interior review point is exactly the case this
  rule exists for.
- **Code review and acceptance are different questions.** A review asks "is this code
  right?"; `verify-acceptance` asks "does this meet `done_looks_like`?" A substantial
  item wants both, and neither stands in for the other.
- **Nothing completes on an unreviewed diff.** The review runs, its findings are fixed or
  explicitly accepted, and `haven item complete <ref> --evidence "…"` names which review
  ran and what it found. An item completed with no review named is a gap, not a shortcut.
