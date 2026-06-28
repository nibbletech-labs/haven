# Haven

Haven is a local-first backlog management tool that spans humans, AI agents, and real life.

It's the tool I run my own work through, built over months and many
iterations, and used every day.

**Today it's local-first and single-user**, driven from the `haven` CLI and a
stdio MCP server your AI agents talk to. **Not here yet:** multi-user support, any
cloud or remote sync, and a UI — all on the priority list, just not in this release.

Single-user doesn't mean siloed. When work belongs in your team's tools, your AI can
**hand an item off to Jira, Linear, or GitHub** — creating the ticket there, recording
where it's running, and reconciling status back into Haven — with no integration to
wire up. More in [Using Haven](USING-HAVEN.md#hand-work-off-to-your-teams-tools).

It keeps track of what needs doing, who owns it, what's blocked, what changed, and
what's ready to pick up next — useful when a flat TODO list isn't enough:

- Work can depend on other work.
- A task can wait on a person, an agent, another task, or an outside event.
- Decisions, specs, research, and evidence stay attached to the work they belong to.
- Agents can ask for the next ready item instead of guessing from stale notes.
- Finished work can carry proof, so handoffs don't rely on memory.

Under the hood, Haven models your work as a **graph**: nodes are individual work
items or the containers that group them, connected by dependency, decomposition, and
grouping edges. [`DATA-MODEL.md`](DATA-MODEL.md) covers exactly what's stored.

## What you get

One `haven` binary that is both a **CLI** and a **stdio MCP server**, plus a suite
of **skills** that teach your AI agents how to use it. Running `haven setup`:

- **installs the skills into your user skills folder** (`~/.claude/skills`,
  `~/.agents/skills`), so every new Claude or Codex session — in any repo — already
  knows Haven, and a binary upgrade refreshes them automatically;
- **registers the `haven` MCP server** for Claude and Codex, so agents can act on
  the backlog; and
- keeps all your data **local** under `~/.haven`.

So in practice you don't type commands — you drive Haven by talking to your agent:
"add this to the backlog", "what's next?", "break this down", "run the build". See
**[Using Haven through your AI](USING-HAVEN.md)** for the plain-language asks and the
kinds of work the agent does on your behalf.

## Quick start

Install the binary — on **macOS** (Homebrew):

```sh
brew install nibbletech-labs/tap/haven
```

On **Linux** (prebuilt binary, no toolchain needed):

```sh
curl -fsSL https://raw.githubusercontent.com/nibbletech-labs/haven/main/packaging/install.sh | sh
```

Then wire up your agents and check the install:

```sh
haven setup
haven doctor
```

`haven setup` installs the skills and registers the MCP server — it doesn't create
a project, and there's no default. From here you don't run anything else: just talk
to your AI. Ask it to "add this to the backlog" and it creates your first project
and starts tracking — see **[Using Haven through your AI](USING-HAVEN.md)**. (You
can also name a project up front yourself; that and other setup details are in
**[INSTALL.md](INSTALL.md)**, along with other platforms, building from source,
agent configuration, and updating.)

## How it fits together

Haven separates the shape of the work from the documents around it:

- The work graph lives in local SQLite, exposed through the CLI and MCP.
- Specs, research, notes, and other artifacts live as files under `~/.haven/`.
- Project docs attach to `anchor` items and surface with `haven docs`.
- Repo-local `_haven/` folders are just visible workspaces; the durable data stays
  under `~/.haven/`.

Your editor, shell, agents, and future apps are clients of Haven — it doesn't depend
on any one of them.

## Learn more

- **[USING-HAVEN.md](USING-HAVEN.md)** — how you drive it all by talking to your AI:
  the skills, and the kinds of actions they perform.
- **[DATA-MODEL.md](DATA-MODEL.md)** — what's actually stored: items, edges,
  acceptance criteria, and the documents you can attach.
- **[INSTALL.md](INSTALL.md)** — install options, agent setup, updating, the repo
  workspace, and developing Haven.

## License

MIT — see [`LICENSE`](LICENSE).
