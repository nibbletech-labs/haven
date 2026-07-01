# Using Haven through your AI

Haven is built to be driven by an AI agent, not typed at by hand. Once it's installed (see the [README](README.md)), your Claude and Codex sessions already know how to use it: the install gives them the **skills** that teach them *how*, and wires Haven in as an **MCP server** so they can act on it.

From then on you work with Haven entirely by talking to your AI in plain language: "add this to the backlog", "what's next?", "break this down", "run the build". You don't run anything yourself; the agent does the work for you. (Under the hood it mostly uses Haven's command-line interface; the MCP surface is mainly groundwork for future **remote** clients (web, desktop, mobile) that drive Haven without a local install.) This doc shows both halves: the asks that drive each skill, and the kinds of actions they perform. For what those actions actually store (items, edges, acceptance, and attached docs), see [`DATA-MODEL.md`](DATA-MODEL.md).

## Your first project

A **project** is a named backlog. You usually don't set one up as a separate step: the first time you ask your AI to track something, it checks whether a suitable project already exists and creates one if not, naming it and giving it a short prefix like `MW`, so items get refs like `MW-1`, `MW-2`. You can keep several (one per repo or product) and switch between them. It all lives in one place, a `~/.haven` folder in your home directory, kept separate from your code. Haven doesn't put anything inside your project repos unless you ask it to (like the in-repo view below).

One thing worth knowing up front: you can ask Haven to **show the backlog inside the repo**: it drops a visible, git-ignored folder next to your code holding the backlog and its docs, so you can read them without leaving the project. It's just a view; the real data stays under `~/.haven` and the folder refreshes itself. (The setup steps are in [`INSTALL.md`](INSTALL.md).)

## What you say → what your AI does (the skills)

Installing Haven gives your agents a family of skills they pick up automatically. You never invoke them by name; you describe what you want and the agent chooses the right one. (Where they install and how they stay updated is in the [README](README.md).)

**Everyday work, what you reach for constantly:**

| Skill | You say something like… | What it does |
|---|---|---|
| **haven** | "add this to the backlog" · "what should I work on?" · "break this down" · "park this" · "who's next?" · "firm this up" | the core backlog loop: capture, prioritise, groom, decompose, evolve, and hand work between people and agents |

**Larger efforts, the orchestrate pipeline, for a job too big for one pass:**

| Skill | You say something like… | What it does |
|---|---|---|
| **orchestrate-plan** | "break the whole `<product>` into a work-graph" · "decompose this into a backlog" | turns a big, unplanned goal into a tree of ownable tasks with dependencies and acceptance |
| **orchestrate-run** | "run the build" · "execute the plan" · "work the ready frontier" | autonomously builds the ready items: a worktree per batch, verify-gated, merged, looped (one at a time by default; it can also build several independent items at once when they're clearly disjoint, and you can just ask for that when you start the run) |

**Two more skills round out the set, `create-context-pack` and `verify-acceptance`, and you are not expected to trigger either.** `orchestrate-run` composes them as it builds: before building a *coupled* cluster of tickets it runs **create-context-pack** to write one shared spec (shared foundation + cross-cutting requirements, so the parts fit together instead of being built in isolation), and it gates every batch with **verify-acceptance** (an independent build + lint + test plus an acceptance judgment, returning PASS / NEEDS-HUMAN / FAIL). You *can* still invoke them directly (to shape a shared spec before a build, or to get a standalone pass/fail verdict on a leaf an agent says it finished), but in normal use they fire on their own.

The everyday **haven** skill covers almost everything; the rest are for planning and autonomously executing larger efforts.

## What the AI actually does (the kinds of actions)

Behind the plain-language asks, the AI works the graph for you. These are the kinds of things it does, roughly the life of a piece of work. You don't issue them as commands; you ask, and it does them.

**Capture**: turn a thought into a tracked item, often just a one-liner thrown into an inbox to triage later, so nothing's lost while you're mid-flow.

**Give it structure**. Connect the work: mark that one item is blocked by another, break a big item into smaller pieces, or group a set under a release or phase. This is what turns a flat list into a graph the AI can reason over.

**Groom and prioritise**: sharpen what "done" means, commit the work that's real (versus parked ideas), and rank what matters, the line between "someday" and "doing this now".

**Find the next thing**: ask "what should I work on?" and get back the items that are ready and not blocked, narrowed to your work or the AI's, or to a single release or phase.

**Own and hand off**. Every item is owned by a person or the AI, and the baton-pass is one move: "built, needs your review" flips it to you and parks it as waiting on you, so it surfaces rather than stalls.

**Finish with proof**: complete an item with evidence, which automatically unblocks whatever was waiting on it, so handoffs don't lean on memory.

**Attach knowledge**: keep specs, research, decisions, and notes as documents on the items they belong to; project-level docs (vision, architecture, spec) live on anchor items and stay discoverable.

**Trace how it evolved**. Split, merge, or replace items as your understanding changes. The history is kept, and an old reference always points you forward to whatever replaced it, so links never go dead.

The literal CLI commands and their `haven_*` MCP equivalents, if you ever want them, are in the bundled skill's [`surface-map.md`](skill/haven/references/surface-map.md).

## Hand work off to your team's tools

Haven is local and single-user, but your work often has to live where your team can see it: a Jira board, a Linear cycle, a GitHub issue. You don't have to set up an integration for that. If your AI already has access to those tools (most do, through their own connectors), it can be the bridge.

Ask it to *"hand HV-12 off to Jira"* and it will create or update the ticket over there, then record a structured **external reference** on the Haven item (which system, which ticket, the link) and mark the item **in progress**, because it's now actively being worked, just somewhere else. The item stays yours; Haven simply knows where it went and keeps a short receipt of the handoff.

Later, *"is PROJ-9 done?"* lets it walk back the other way and find the Haven item from the external ticket id, then reconcile: update the status, or complete the item with the external result as evidence. Whether an external "Done" closes the Haven item automatically or just lands on a review list for you is your call, not a rule Haven enforces.

What this is **not** is a native integration. Haven holds no connection to Jira or GitHub and checks nothing on their side. Your AI is the integration point, working under your setup. So there's nothing to configure specifically if you are already using your AI to interact with these other tools.
