## v0.1.5: External Handoff

Haven stays local and single-user, but your work no longer has to stay on one machine. When a task needs to live where your team can see it, your AI can hand the item off to an external tracker (Jira, Linear, or GitHub), record where it went, and reconcile the status back later. Haven holds no connection to those systems and configures nothing on their side: the AI agent you already use is the bridge. This release also tightens first-run setup, the in-repo workspace, and the front-door docs.

**External-system handoff**

- **Item external references.** A structured locator for work that is being executed somewhere else. `haven item extref add HV-12 --store jira --target PROJ-123 --url <link>` records which system, which ticket, and the link, then flips the item to `in_progress`. Pass `--no-in-progress` to record the locator without changing status, `--canonical` to mark the external system as where execution really happens, and `--receipt "..."` to keep a short note of the handoff.
- **Reconcile back the other way.** `haven item extref find --target PROJ-123` finds the Haven item from an external id, so when something is marked done over there you can complete the Haven item with that result as evidence. `haven item extref list HV-12` and `haven item extref rm HV-12 --target PROJ-123` round out the surface.
- **The same surface for agents over MCP.** `haven_set_extref`, `haven_find_extref`, and `haven_rm_extref` mirror the CLI, so an agent can do the handoff and the later reconciliation conversationally. A separate `haven_xref` reports artifact cross-store links (content provenance), kept distinct from where an item is being executed.
- **Ownership is left untouched.** An external handoff records where the work is running. It is not the AI-to-human ownership handoff, so it leaves owner and wait-state alone.

**Setup and workspace**

- **Fresh installs start with no project.** `haven setup` now wires the MCP server and skills without creating or selecting a default project, so there is no placeholder backlog to clean up. You name the first project when you capture the first item, or pass `--project-key`/`--prefix` to name one up front.
- **Richer in-repo workspace.** The optional in-repo `_haven/` view now surfaces items and their attached docs, the generated backlog hides completed items, and a new `haven unlink` detaches the workspace again.
- **Skill rename.** The standalone acceptance check is now the `verify-acceptance` skill (collision-safe), matching how `orchestrate-run` composes it.

**Docs**

- A reworked front door: a leaner `README` plus `USING-HAVEN`, `DATA-MODEL`, and `INSTALL` guides, and a dedicated walk-through of the external-handoff flow.

**Upgrade Notes**

- No migration is required. External references live in existing item metadata.
- `haven setup` no longer creates a default project. Create your first project by capturing an item, or name one up front with `--project-key`/`--prefix`.
- If you referred to the acceptance-check skill as `verify`, it is now `verify-acceptance`.
