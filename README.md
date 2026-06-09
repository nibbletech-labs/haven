# Haven

A **local-first, cloud-synced store for a long-lived work-graph** — the backlog
substrate behind an AI-assisted development pipeline, and a future phone/web app.

Nodes of dependent work, each owned by a **human** or an **AI**, long-lived (a
node can stay open for days, blocked on a real-world event), with lineage
tracking and sync that lets you ask "what's next?" or report "done" from
anywhere. A single `haven` binary is the local SQLite + files store, a CLI, and a
stdio MCP server — with an opt-in remote half (Supabase + Auth0).

```
haven setup --project-key haven --project-title "Haven" --prefix HV
haven item add "Draft the spec" --status ready --commit --assign ai \
  --done-looks-like "approved by review"
haven next                          # what should I do next?
haven item get HV-1 --include edges,artifacts,lineage
haven item assign HV-1 --to human
```

- **Structure** (the work-graph) lives in local SQLite, exposed over CLI + MCP,
  optionally synced to the cloud.
- **Content** (specs, research, notes) lives as files under `~/.haven/`, edited
  directly by local agents.
- Your tools and apps are *clients* of Haven; Haven doesn't depend on them.

## Install

**Homebrew** (once the tap is published):

```sh
brew install nibbletech-labs/tap/haven
haven setup --project-key haven --project-title "Haven"
haven doctor       # verify the install
```

**Install script** (builds from source; needs a [Rust toolchain](https://rustup.rs)):

```sh
curl -fsSL https://raw.githubusercontent.com/nibbletech-labs/haven/main/packaging/install.sh | sh
```

**From source:**

```sh
git clone https://github.com/nibbletech-labs/haven && cd haven
cargo build --release
./target/release/haven setup --project-key haven --project-title "Haven"
```

`haven setup` is idempotent: it creates `~/.haven`, runs migrations, registers the
`haven` MCP server in your Claude config, installs the bundled Claude skill, and
can create/select your first project with `--project-key`. `haven doctor` reports
whether each local install piece is wired.

## Develop

Rust workspace, single binary:

- `crates/haven-core` — the `Store` service (db / model / store / sortkey)
- `crates/haven-cli` — the `haven` binary
- `crates/haven-mcp` — the stdio JSON-RPC MCP server
- `crates/haven-sync`, `crates/haven-auth` — the opt-in remote half
- `migrations/` — local SQLite DDL · `supabase/` — the remote mirror

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --check
```

## Status

The local slice runs end-to-end on one machine: items, four edge layers,
evolve/lineage, `next`, full-text search, artifacts, `backlog.md` projection, and
the MCP server. The remote half (Supabase schema + RLS, sync push) is validated
against a local Supabase stack; two-way pull and live Auth0 wiring are in
progress.

## License

MIT — see [`LICENSE`](LICENSE).
