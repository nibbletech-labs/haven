# Haven

Haven is a local-first backlog for work that spans humans, AI agents, and real
life.

It keeps track of what needs doing, who owns it, what is blocked, what changed,
and what is ready to pick up next. You can use it from the `haven` CLI, through
an MCP server in tools like Codex and Claude, or later through synced apps.

Haven is useful when a normal TODO list is too flat:

- Work can depend on other work.
- A task can wait on a person, an agent, another task, or an outside event.
- Decisions, specs, research, and evidence can stay attached to the work they
  belong to.
- Agents can ask for the next ready item instead of guessing from stale notes.
- Finished work can include proof, so handoffs do not rely on memory.

Under the hood, Haven is one `haven` binary with a local SQLite store, a CLI, and
a stdio MCP server. Cloud Sync through Supabase/Auth0 is private preview and
disabled in public installs by default.

```sh
haven setup --project-key haven --project-title "Haven" --prefix HV
haven item add "Draft the spec" --status ready --commit --assign ai \
  --done-looks-like "approved by review"
haven next                          # show ready, unblocked work
haven item get HV-1 --include edges,artifacts,lineage
haven item assign HV-1 --to human
haven docs                          # project vision, architecture, and spec anchors
```

## How It Fits Together

Haven separates the shape of the work from the documents around it:

- The work graph lives in local SQLite and is exposed through the CLI and MCP.
- Specs, research, notes, and other artifacts live as files under `~/.haven/`.
- Project docs attach to `anchor` items and are discoverable with `haven docs`
  or `haven_docs`.
- Repo-local `Haven/` folders are just visible workspaces. The durable data
  stays under `~/.haven/`.

Your editor, shell, agents, and future apps are clients of Haven. Haven does not
depend on any one of them.

## Install

**Homebrew:**

```sh
brew install nibbletech-labs/tap/haven
haven setup --project-key haven --project-title "Haven"
haven doctor       # verify the install
```

**Install script** (downloads a prebuilt binary — no Rust toolchain; macOS
arm64/x64, Linux arm64/x64):

```sh
curl -fsSL https://raw.githubusercontent.com/nibbletech-labs/haven/main/packaging/install.sh | sh
```

It downloads the matching release tarball, verifies its sha256, and installs to
the first writable of `$HAVEN_BIN_DIR`, `/usr/local/bin`, `~/.local/bin`. Pin a
version with `HAVEN_VERSION=v0.1.1`. On a platform without a prebuilt binary it
falls back to building from source (needs cargo); force that with
`--from-source` or `HAVEN_BUILD_FROM_SOURCE=1`.

**From source:**

```sh
git clone https://github.com/nibbletech-labs/haven && cd haven
cargo build --release
./target/release/haven setup --project-key haven --project-title "Haven"
```

> **macOS Gatekeeper:** binaries from `brew` or `curl` run from a terminal are
> not quarantined. If you download a release tarball with a browser and macOS
> refuses to run it ("cannot verify the developer"), clear the quarantine flag
> once: `xattr -d com.apple.quarantine "$(which haven)"`.

`haven setup` is safe to run more than once. It creates `~/.haven`, runs
migrations, registers the `haven` MCP server for Claude and Codex, installs the
bundled skill into agent-readable skill paths, writes or refreshes the Haven
stanza in `AGENTS.md`, and can create or select your first project with
`--project-key`.

Use `haven doctor` to check whether the local pieces are wired correctly.

## Updating

```sh
haven self update --check     # report current vs latest, change nothing
haven self update             # apply the right update for how haven was installed
```

`haven self update` is install-method aware. For an `install.sh` install it
downloads the latest prebuilt binary, verifies its sha256, and atomically swaps
it in place (`--binary` forces this for an unrecognized install location). For a
Homebrew install, `haven self update --run` runs `brew upgrade` for you. A dev
symlink just needs a rebuild. Use `--tag v0.1.1-rc.1` to install a specific
release (e.g. to try a pre-release).

## Agent Setup

`haven setup` wires the default local integrations, but you can target one agent
when that is all you need:

```sh
haven setup --agent codex
haven setup --agent claude
haven skill install --agent codex
```

Codex reads MCP servers from `~/.codex/config.toml` or trusted project
`.codex/config.toml`. Haven writes this stanza for Codex:

```toml
[mcp_servers.haven]
command = "haven"
args = ["mcp"]
```

Codex/Open Agent Skills are installed to `~/.agents/skills/haven` by default.
Codex can read `.agents/skills`, `~/.agents/skills`, and `/etc/codex/skills`.
Claude keeps using `~/.claude/skills/haven`; Codex does not read that Claude
path.

## Repo Workspace

To add a human- and agent-readable project entry point inside a repo:

```sh
haven link
```

This creates a visible `Haven/` workspace containing a generated `backlog.md`
view and room for docs. The real graph and content remain under `~/.haven/`, so
the `Haven/` directory can be regenerated. When run inside a Git repo, Haven adds
`/Haven/` to `.git/info/exclude`.

Do not hand-edit `backlog.md`; it is generated from Haven's store.

## Develop

Haven is a Rust workspace with one shipped binary:

- `crates/haven-core` - the shared store, data model, and ordering logic
- `crates/haven-cli` - the `haven` command
- `crates/haven-mcp` - the stdio JSON-RPC MCP server
- `crates/haven-sync` and `crates/haven-auth` - optional sync and auth support
- `migrations/` - local SQLite schema
- `supabase/` - remote mirror schema and policies

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --check
```

For a development install that follows your local builds, symlink the binary
onto your PATH:

```sh
cargo build --release
./target/release/haven self install --link   # ~/.local/bin/haven -> target/release/haven
```

After that, rebuilds are picked up automatically. Without `--link`,
`haven self install` copies the binary instead. `haven doctor` verifies the
local wiring.

## Status

The local workflow runs end to end on one machine: items, dependency layers,
handoffs, lineage, `haven next`, full-text search, artifacts, the generated
`backlog.md` view, and the MCP server.

Cloud Sync is partly built but not part of the public local-first release yet.
The Supabase schema, RLS, and push flow are validated against a local Supabase
stack; the remaining hosted service/Auth0 wiring and product surface are still
in progress. The unfinished `auth`/`sync` commands are hidden and require
`HAVEN_CLOUD_SYNC_PREVIEW=1`.

## Running the Work — and How to Ask for It

Haven work runs one of two ways: you **build it directly**, or you hand a planned graph to the
**autonomous executor** (`orchestrate-run`). The planning / spec / verify skills compose into
either. The full picture — when to pick which, and the code-vs-functionality verification split —
is the bundled skill's `references/running-work.md`.

- **Direct** — the agent builds it in one thread (optionally decomposing and speccing first).
  Highest quality and your direct oversight; best for a task or a handful.

  > "just fix X" · "add Y" · "plan this change, then build it"
  > "break the whole `<product>` into a Haven work-graph" — decompose first (`orchestrate-plan`)
  > "create a context pack for HV-3 and HV-4" — spec a batch (`create-context-pack`)

- **Executor** — `orchestrate-run`: the session becomes a conductor that, per leaf, builds in a
  git worktree, gates it with a separate fresh verifier, merges to `main`, and loops the ready
  frontier. Best for many leaves where an inline build would blow the context.

  > "run the build" · "execute the plan" · "work the ready frontier autonomously"

  **Serial today** — it builds **one leaf at a time**; parallel fan-out is built but gated off
  (HV-85). For a small job, direct is usually the better-quality choice.

## License

MIT - see [`LICENSE`](LICENSE).
