# Value-density — the first-cut discipline and the quality bar

Decomposition (`references/decomposition.md`) answers *"how do I split this node?"*
This file answers two adjacent questions the planner needs every time it seals:
*"does this work belong in the first pass at all?"* and *"is this leaf high enough
quality to dispatch?"* It is the scoping-debate IP folded in as planner discipline —
the PM's value-density case and the Critic's decomposition-quality battery, now run
inside one head rather than across an agent team.

A leaf you *can* write is not always a leaf you *should* seal **now**. Before you
commit a node as first-pass build work, run the three first-cut tests; before you set
its `done_looks_like`, run the quality battery and the Gherkin bar.

## The first-cut tests — does this belong in the first pass?

For each candidate first-pass node, test against:

1. **Value-density test**: Does this item deliver real user value relative to the
   effort required? If not, it's Future.
2. **Effort test**: Could a simpler version deliver most of the value at a fraction
   of the effort? If yes, scope down.
3. **Dependency test**: Is this item required for another first-pass item to work? If
   not, it's probably Future.

In Haven terms, a node that fails these isn't a `discovery` floater and isn't a
sealed leaf — it's a **Future** node: capture it, but don't commit it into the
first-pass dispatch queue. Future items aren't a dumping ground; each one keeps a
**"Why deferred"** and a **"Why it matters later"** so the deferral is a decision, not
an omission.

### The Grounding Rule

Every first-pass item should be traceable to at least one of:

- A success metric in the Vision Doc
- The core value proposition in the Vision Doc
- A hard dependency of another first-pass item

If an item can't be traced, it goes to Future. (When there is no formal vision doc,
read the root anchor's goal / `done_looks_like` as the vision: the success metrics
and value prop are whatever that goal implies.)

### Feels-like-first-pass-but-isn't

These almost always read as essential and almost always aren't — defer them unless
the effort is trivial and the value is real, or unless that thing *is* the value:

- **Onboarding flows** — can be done manually at first.
- **Settings and preferences** — use sensible defaults.
- **Admin dashboards** — use direct database access initially.
- **Social features, sharing, notifications** — unless that IS the core value.
- **"Polish" items** — animations, empty states, error messages.

## The decomposition-quality battery — is this leaf sound?

The Critic's battery, run against every node before you seal it. Each check that
fires sends the node back to split, defer, or re-scope rather than seal:

- **Single buildable unit.** Each item must be *one* unit of work, not a compound
  feature. "Implement user management" is **three items**, not one. If a node is
  really two features compressed into one, split it.
- **Complexity realism.** Don't seal an undersized leaf. An item that looks small but
  secretly requires building auth, a data pipeline, or a new service is **not** small
  — its hidden weight means it should be split or deferred, not sealed as a quick win.
- **Dependency honesty.** Items that look independent sometimes aren't. Surface the
  hidden ordering — a node blocked on another node's output, or on infrastructure not
  yet planned — and wire the dependency edge before sealing.
- **External-dependency honesty.** Flag external dependencies — third-party APIs,
  payment processors, developer accounts, API keys, hosted services — that need human
  action outside the build. For each, state what it is **and** the mitigation if it's
  unavailable: stub behind an interface, use test/sandbox mode, or mock responses. A
  leaf that silently assumes an external service is set up is a false-ready leaf.
- **Oversized-item detection.** If a node is XL, or its description implies multiple
  distinct user flows (e.g. "manage users" covers CRUD, roles, and permissions) or
  spans multiple technical domains (e.g. "real-time sync" requires WebSocket, state
  management, and conflict resolution), flag it for decomposition — it is not a leaf.
- **Bidirectional lifecycle check.** Catch the node falsely marked ready that has
  real unknowns (it should be a discovery leaf with its question named) **and** the
  node falsely marked as needing discovery when the "uncertainty" is just an
  engineering decision the executor can make (it should be sealed `ready`). Challenge
  both directions, not just under-specification.

## The Gherkin-readiness bar — is `done_looks_like` specific enough?

A leaf's `done_looks_like` must be specific enough that a QA agent (or the `verify`
skill) could write Given/When/Then scenarios from it **alone**, without asking what
"done" means.

- **"works correctly" fails** — expected by whom? what would a tester actually check?
- **"user can create an account, log in, see their dashboard" passes** — it describes
  the user flow step by step.

If the acceptance can't be turned into Gherkin without guessing the flow, it isn't
specific enough — sharpen it before you seal, or the leaf is a planner defect the
executor can't verify. (For non-code leaves the equivalent bar is: an agent reading
it knows *exactly* what to produce.)
