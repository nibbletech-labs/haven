## v0.1.4 — Priority Provenance

Haven now keeps a clearer audit trail for the decisions that move work up, down,
in, or out of the active backlog. Priority changes, fine ordering, and
commitment changes can carry a rationale, and agents get better guidance when
they need to fetch several item details at once.

**New Features**

- **Priority-change rationale** — `haven item update HV-1 --priority 1 --rationale "Needed for launch"` and `haven_update_item` now record the reason in lineage, including the old and new priority bands.
- **Rank rationale** — `haven item rank HV-2 --before HV-1 --rationale "Unblocks the release"` and `haven_rank` now leave an auditable lineage event with the placement target and sort context.
- **Commitment rationale** — `haven item commit` and `haven item uncommit` can now record why work is being pulled into or parked outside the committed backlog.

**Agent Improvements**

- **Better batch-read hints** — MCP tool descriptions now point agents from inbox, dispatch, and session-prime summaries toward `haven_get_items` when they need full details for several refs. That keeps agents from making a string of separate item reads.

**Distribution**

- **Supported platforms are explicit** — the README now names the current public install targets: macOS Apple Silicon, macOS Intel, Linux ARM64, and Linux x64. Windows is not supported yet.
- **Homebrew tap sync** — stable release builds now push the generated `haven.rb` formula to `nibbletech-labs/homebrew-tap` so the tap does not drift behind GitHub Releases.

**Upgrade Notes**

- No migration is required. The new provenance events use Haven's existing
  lineage model.
