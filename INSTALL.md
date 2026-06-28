# Installing & configuring Haven

The [README](README.md) has the quick start. This is the full reference: install
options, what `haven setup` wires up, agent configuration, updating, the repo
workspace, and developing Haven itself.

## Supported platforms

| Platform | Prebuilt binary | Homebrew tap | Install script | From source |
| --- | --- | --- | --- | --- |
| macOS Apple Silicon | Yes | Yes | Yes | Yes |
| macOS Intel | Yes | Yes | Yes | Yes |
| Linux ARM64 | Yes | Yes | Yes | Yes |
| Linux x64 | Yes | Yes | Yes | Yes |
| Windows | No | No | No | Not supported yet |

Prebuilt Linux binaries are static musl builds. Platforms without a prebuilt
binary fall back to source builds when the install script can find a Rust
toolchain.

## Install

**Homebrew:**

```sh
brew install nibbletech-labs/tap/haven
haven setup --project-key my-work --project-title "My Work" --prefix MW
haven item add "First item"
haven doctor       # verify the install
```

**Install script** (downloads a prebuilt binary — no Rust toolchain; macOS
arm64/x64, Linux arm64/x64):

```sh
curl -fsSL https://raw.githubusercontent.com/nibbletech-labs/haven/main/packaging/install.sh | sh
```

It downloads the matching release tarball, verifies its sha256, and installs to
the first writable of `$HAVEN_BIN_DIR`, `/usr/local/bin`, `~/.local/bin`. Pin a
version with `HAVEN_VERSION=v0.1.4`. On a platform without a prebuilt binary it
falls back to building from source (needs cargo); force that with
`--from-source` or `HAVEN_BUILD_FROM_SOURCE=1`.

**From source:**

```sh
git clone https://github.com/nibbletech-labs/haven && cd haven
cargo build --release
./target/release/haven setup --project-key my-work --project-title "My Work" --prefix MW
```

> **macOS Gatekeeper:** binaries from `brew` or `curl` run from a terminal are
> not quarantined. If you download a release tarball with a browser and macOS
> refuses to run it ("cannot verify the developer"), clear the quarantine flag
> once: `xattr -d com.apple.quarantine "$(which haven)"`.

## What `haven setup` does

`haven setup` is safe to run more than once. Beyond creating `~/.haven` and
running migrations, it makes your AI agents Haven-aware: it installs a suite of
skills (`haven`, plus the planning and execution skills) into your **user-scope**
skills folders — `~/.claude/skills` for Claude, `~/.agents/skills` for Codex — and
registers the `haven` MCP server for both. So every new Claude or Codex session,
in any repo, already knows how to drive Haven from plain language and can act on it
through the MCP tools — no per-project wiring, and a binary upgrade refreshes the
skills automatically. A plain `setup` creates **no project** — a fresh install
starts with none; pass `--project-key` (with `--project-title` and `--prefix`) to
create one up front, or just let your AI create one the first time you ask it to
track something. It writes nothing into the current directory unless you opt in
with `--agents-md`, which refreshes the Haven stanza in the repo's `AGENTS.md`.

## Projects

Every item belongs to a Haven project. A project gives the backlog a namespace and
an item ref prefix, so a project with `--prefix MW` creates refs such as `MW-1`,
`MW-2`, and so on.

A fresh install starts with **no project** — there's no default. You get one of
three ways: pass `--project-key` to `haven setup` to create and select it up front,
run `haven project add` later, or simply ask your AI to track something and let it
create the project for you. To name one at setup:

```sh
haven setup --project-key my-work --project-title "My Work" --prefix MW
```

Later commands use the current project by default. You can switch projects or
target one command explicitly:

```sh
haven project add --key website --title "Website" --prefix WEB
haven project use website
haven item add "Draft homepage copy"     # creates WEB-1
haven --project my-work item list        # read another project without switching
```

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

## Agent setup

`haven setup` wires the default local integrations, but you can target one agent
when that is all you need:

```sh
haven setup --agent codex
haven setup --agent claude
haven skill install --agent codex
haven setup --agents-md   # opt in to repo-local AGENTS.md discovery
```

Codex reads MCP servers from `~/.codex/config.toml` or trusted project
`.codex/config.toml`. Haven writes this stanza for Codex:

```toml
[mcp_servers.haven]
command = "haven"
args = ["mcp"]
```

Codex/Open Agent Skills are installed to `~/.agents/skills` by default. Codex can
read `.agents/skills`, `~/.agents/skills`, and `/etc/codex/skills`. Claude keeps
using `~/.claude/skills`; Codex does not read that Claude path.

## Repo workspace

To add a human- and agent-readable project entry point inside a repo:

```sh
haven link
```

This creates a visible `_haven/` workspace containing a generated open `backlog.md`
view, an `items/` alias to the canonical content tree, and `docs/` aliases for
living-doc anchor item folders. The real graph and content remain under `~/.haven/`,
so the `_haven/` directory can be regenerated. When run inside a Git repo, Haven
adds `/_haven/` to `.git/info/exclude`.

Re-running `haven link` refreshes the projection in place, including upgrading older
empty projection docs folders. `haven unlink` removes only the repo-local
projection, `.haven-project`, and local git-exclude entries; it does not delete
canonical graph/content under `~/.haven/`.

Do not hand-edit `backlog.md`; it is generated from Haven's store.

## Cloud Sync (private preview)

Cloud Sync is not part of the public local-first release yet. It remains an
unfinished private preview; the `auth`/`sync` commands are hidden and require
`HAVEN_CLOUD_SYNC_PREVIEW=1`.

## Develop

Haven is a Rust workspace with one shipped binary:

- `crates/haven-core` — the shared store, data model, and ordering logic
- `crates/haven-cli` — the `haven` command
- `crates/haven-mcp` — the stdio JSON-RPC MCP server
- `crates/haven-sync` and `crates/haven-auth` — preview-gated sync and auth internals
- `migrations/` — local SQLite schema
- `supabase/` — preview-gated remote mirror schema and policies

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --check
```

For a development install that follows your local builds, symlink the binary onto
your PATH:

```sh
cargo build --release
./target/release/haven self install --link   # ~/.local/bin/haven -> target/release/haven
```

After that, rebuilds are picked up automatically. Without `--link`,
`haven self install` copies the binary instead. `haven doctor` verifies the local
wiring.
