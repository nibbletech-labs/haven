# Code-planning method — understand the code before you propose a change to it

Read this when an agent is about to work out **how a piece of code should change**: the
approach step of `plan-item`, and the build-plan step (6a) and its validator (6b) in
`orchestrate-run`. It is the discipline a native plan mode imposes on a planning agent,
kept here so it survives on any harness and reaches spawned agents, who inherit no skill
and see only what their prompt carries.

The cardinal rule: **a plan for changing code is only as good as the reading that preceded
it.** Grepping for a filename is not reading. Tracing the path the behaviour actually takes
is.

Two parts: § The method is written to be **forwarded verbatim** into a spawn prompt. § Who
uses it, at what depth says what each consumer keeps and drops.

## The method (forwardable — paste this block into the planning agent's brief)

> You are planning a change to existing code. Before you propose anything, understand what is
> there. Work through these in order and let the plan show the evidence of each.
>
> 1. **Read what you were given.** The files, line ranges, and docs named in your brief are
>    the starting set, not the whole set. Read them fully, not the first screen.
> 2. **Trace the code path.** For the behaviour being changed or extended, follow it from
>    its entry point (the command, the endpoint, the event, the call site) through to its
>    effect (the write, the response, the render). Name each hop by file and function. Where
>    the path forks, follow the fork that matters for this change and say why the others
>    don't. A change to a behaviour you have not traced is a guess.
> 3. **Find the similar feature.** The repo almost certainly already does something shaped
>    like this. Find it and read how it does it — it is the reference implementation the
>    plan should rhyme with, and the fastest way to learn the conventions that apply here.
> 4. **Hunt for reuse before you propose new code.** Actively search for existing functions,
>    utilities, types, and patterns that already do part of the job. Prefer calling them
>    over rewriting them. Name every reuse target with its file path. New code that
>    duplicates something already in the repo is a defect in the plan, however tidy it is.
> 5. **Look at how this area is tested.** Find the existing tests around the path you
>    traced: their location, harness, fixtures, and style. The plan's tests follow that
>    pattern, and the failing-test-first order is written per acceptance clause.
> 6. **Weigh more than one approach, along the axes that fit the task.** New feature:
>    simplicity vs performance vs maintainability. Bug fix: root cause vs workaround vs
>    prevention. Refactor: minimal change vs clean architecture. Pick one and say in a
>    line why the runner-up lost, so the argument is not re-had later.
> 7. **Sequence the work and name its dependencies.** Which change has to land before
>    which, and what each step leaves buildable and testable. Ordering is part of the plan,
>    not something the builder discovers.
> 8. **Anticipate what will go wrong.** The edge case the trace exposed, the caller you
>    would break, the migration that has to run first, the test that will be hard to write.
>    A plan with no risks listed has not looked hard enough.
> 9. **End with the critical files.** The three to five files most central to the change,
>    with one line each on what changes there. For a pattern repeated across many files,
>    describe the pattern once and give representative paths — don't enumerate.
>
> Write for someone who will act on the plan without being able to ask you a question:
> which files, what changes, in what order, how to verify. Where the change has real
> structure — edits that depend on each other, data moving between components, a
> meaningful before/after — a small `mermaid` or ascii diagram of that shape lets a reviewer
> check the plan hangs together at a glance. When the change is linear, skip it.

## Who uses it, at what depth

The same reading, two altitudes of output. Both consumers read the code the same way; they
differ in what they write down.

### `plan-item` — the spec (step 5, "write the plan onto the item")

The spec stays at **spec altitude** (its `references/planning-method.md`): the file-by-file
edit list is stale the moment code moves and belongs to the build session. So from the
method, the spec keeps:

- the **seams** the change touches, from the trace — the entry point, the layer the change
  lands in, the effect — named by file, so the shippability linter's "architecture notes
  cite real files" rule is met by understanding rather than by grep;
- the **similar feature** it should rhyme with, by path;
- the **reuse targets**, by path — these are constraints on the build, not suggestions;
- the **approach chosen and the runner-up**, one line.

Sequencing, anticipated challenges, and the test-pattern note go into the **build
checklist** (`references/build-handoff.md`) when the scale call is one pass; a decompose
call carries them as prose in the high-level plan for `orchestrate-plan`'s children to
inherit.

**Delegation, done the right way round.** Where the harness has a read-only architect
agent (Claude Code's `Plan` type), it is a *designer*, not an explorer: its prompt expects
requirements and a perspective, and it returns a design. So explore first — inline, or
with a read-only search agent (`Explore`) per focus (existing implementations, related
components, testing patterns) — then hand the architect the requirements, the constraints,
the traces and filenames you found, and one perspective from step 6, and take its design
back into the spec you write. Handing it a bare ref to go and find out is using it
against its grain.

### `orchestrate-run` — the build plan (tick steps 6a / 6b)

The build plan is the **file-level** plan, so the whole method applies at full depth, and
the plan shows its working:

- **6a dispatch.** The plan agent is a **plain, unnamed subagent** (dispatch-policy
  § Transport) of the harness's general-purpose type — the read-only architect type has
  its write tools removed and cannot write the plan file the coordinator registers. It
  inherits nothing, so the coordinator **forwards § The method verbatim** into the brief,
  after the leaf, the acceptance, the envelope, and the paths to read.
- **6b validation.** Beyond coverage, envelope, TDD order, and concreteness
  (executor-discipline § The build plan), the validator checks the plan **shows a trace**
  of the path for the behaviour it changes and **names reuse targets** where the repo
  already has the code. A plan missing either is a REVISE with those as the named gaps —
  it is the class of plan that passes the gate and then rewrites something that existed.

### Not consumers

`orchestrate-plan` decomposes from the spec and `create-context-pack` grooms and
synthesises; neither opens the code. A spec thin on code grounding is a `plan-item`
defect the pack's verify-first preamble surfaces — not something the pack repairs by
exploring on its own.

## Provenance

Transcribed from the prompts a native plan mode injects while active (the Explore and Plan
subagent prompts, the phase-1/2/4 reminders, and the remote "ultraplan" reminder), as
extracted per version by the community; the harness's tool description of its plan mode
is the thin summary, not the discipline. This file is a snapshot — when the native guidance
moves, move this with it.
