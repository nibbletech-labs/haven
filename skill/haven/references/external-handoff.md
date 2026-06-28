# External-system handoff — Jira / Linear / GitHub (HV-221)

Hand a Haven item off to an external PM/dev system (Jira, Linear, GitHub, …) and
track it while it runs there — **without** Haven owning a native integration. The
model: an AI that already has the external system's MCP/API access is the
integration point; Haven just stores a durable **locator** on the item and reuses
its existing status primitives. No connector, no webhooks, no batch export.

This is **item-level** (`items.metadata.external_refs[]`), and **distinct from** the
artifact-level `xref` (content provenance — see `surface-map.md` / SPEC §4): an
external ref is the *execution* locator for the work, not a link between content
blobs. It is also **not** the ai↔human `handoff` (workflow 8) — different axis (see
§3 and *Not this* below).

## 1. The workflow (item-by-item)

The handoff is one item at a time — there is deliberately **no** batch container or
export object.

1. **Create/update the external issue.** The AI uses whatever tool access it has (a
   Jira MCP, the GitHub CLI, …) to open or update the ticket. Haven does not
   prescribe the ticket's shape or content — that's the destination system's and the
   user's call.
2. **Record the locator on the Haven item** and mark it active:
   ```bash
   haven item extref add HV-12 --store jira --target PROJ-9 \
     --url https://acme.atlassian.net/browse/PROJ-9 \
     --canonical --receipt "handed to the platform team for Q3"
   # → records the external ref AND flips HV-12 to in_progress, in one revision.
   ```
   Over MCP: `haven_set_extref {project, ref:"HV-12", store:"jira", target:"PROJ-9",
   url, execution_canonical:true, receipt}`.
3. **Leave it `in_progress`** while the work is live externally (see §3). Owner and
   `wait_state` are left untouched — the item is owned and *actively in flight*, just
   elsewhere.
4. **Reconcile later by policy** (see §3) — the AI inspects the external system and
   updates Haven.

`--no-in-progress` (MCP `in_progress:false`) records the locator *without* the status
flip — e.g. noting where something *will* go, before it's actually handed off.

## 2. Receipt convention

Each entry in `items.metadata.external_refs[]`:

| field | required | meaning |
|---|---|---|
| `store` | **yes** | the external system — `jira` / `linear` / `github` / … (free string) |
| `target` | **yes** | the locator within that store — `PROJ-9`, `owner/repo#42` (opaque; Haven never resolves it) |
| `url` | no | direct link to the external item |
| `status` | no | last-observed external status (free text; Haven doesn't interpret it) |
| `execution_canonical` | no | `true` = this system OWNS execution now; `false` (default) = a mirror/secondary |
| `receipt` | no | free-text human record of the handoff — named `receipt`, **not** `note`, to avoid colliding with `haven note` and the ai↔human handoff note |

- **Multiple refs per item** are preserved (an issue *and* a PR). `extref add`
  upserts by `(store, target)` — re-recording the same pair refreshes it in place.
- **`execution_canonical` vs mirror:** pass `--canonical` on the one system that is
  the source of truth for *doing* the work; a `false` entry (the default) is a
  convenience pointer Haven won't treat as authoritative.
- Read them back from the item's `metadata`: `haven item extref list HV-12` (or
  `haven item get HV-12` / `haven_get_item`).

## 3. Status & reconciliation

**While active externally → `in_progress`.** A handed-off item is genuinely being
worked, just in another system, so it reads as `in_progress` (not parked). `extref
add` sets this by default.

**`wait=on_external` is for PASSIVE blockers only** — when the item is *waiting on*
something outside Haven you don't control (a vendor, an external approval), **not**
when it's being actively executed elsewhere. Don't use `on_external` to mean "it's in
Jira now"; that's what `in_progress` + the external ref already say.

**Finding handed-off work** — by status plus the locator:
```bash
haven item list --status in_progress     # everything active (incl. external)
haven item extref find --target PROJ-9    # reverse lookup: which item carries PROJ-9?
```
Over MCP: `haven_find_extref {project, target:"PROJ-9"}` — reconcile an external id
back to its Haven item.

**Completing from external "Done" is policy, not automation.** Haven does NOT watch
the external system. Whether an external Done auto-completes the Haven item or
produces a review list for a human is the user's / system-prompt's call. When you do
complete it, attach evidence from the external system:
```bash
haven item complete HV-12 --evidence "PROJ-9 closed Done; PR acme/svc#88 merged"
```

## Not this

- **Not a native integration.** Haven holds no client to Jira/Linear/GitHub and
  verifies nothing about the external ticket — the AI is the integration point, under
  the user's setup and prompt.
- **Not a batch export.** Handoff is item-by-item; there is no "export these N items"
  container (that idea was tried and dropped — HV-223).
- **Not the ai↔human `handoff`.** That verb (workflow 8) is a different axis — *who
  owns* the item — and flips the owner and parks it `on_human`. External handoff keeps
  ownership and goes `in_progress`. Don't reach for `haven item handoff` here.
