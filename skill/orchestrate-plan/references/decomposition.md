# Decomposition — the split-or-seal test

At each frontier node ask the one question that drives every tick:

> **Would breaking this node down further materially improve the eventual output?**

If **no** → it's a leaf: seal it (acceptance + commit + owner). If **yes** and
depth < 5 → split it one level. Judge against the node's `title` / `body` /
`done_looks_like`, using three factors.

## The three factors

**1. Mixed activity types.** Does the node bundle fundamentally different *kinds* of
work that want different tools and different acceptance? "Build the homepage" mixes
copy (writing), imagery (visual creation), and layout (code). "Define the product"
mixes research, a pricing decision, and naming. Different activity types almost
always benefit from splitting — they're judged and done differently. The Haven
`type` enum is a useful tell: if a node would need *two* types at once (`design` +
`code`, `research` + `decision`), split it so each child has one clean type.

**2. Skill-grain boundary.** Would **one coherent unit of work** — one owner, one
activity, roughly a day or less, with a single checkable `done_looks_like` — finish
this as-is? If yes, **don't split**: sub-fragments add coordination overhead with no
quality gain. This is the floor. A leaf should be something you could hand to one
agent (or person) with a clear "done" and expect a clean result. (v1 has no live
skill-discovery manifest, so judge grain by this heuristic, not by matching a
catalogue of skills.)

**3. Hidden cross-branch dependency.** Is there a sub-part that **several siblings or
branches need** — whether a shared **deliverable** they each consume (a logo that blocks
both the homepage and the about page) *or* a shared **decision/contract they each assume**
(the data model they all read/write, an API two features build against, an auth/format
convention)? Promote it to its own node so every dependent can unblock independently the
moment it's done, and wire the dependents to it with dependency edges. The tell for the
*assumed* case — sharper than ordinary grain — is that **each child's `done_looks_like` is
individually writable and checkable, yet two children built apart against different guesses
about the shared shape would each pass its own gate and still fail to compose.** When you see
it, emit the **coupling** (the shared node + a dependency edge per dependent) — never group
the dependents or write them a shared brief; that grouping is `create-context-pack`'s axis,
not the planner's. This is what turns a flat list into a *stacked* graph.

## Split when

- Mixed activity types (needs different tools / acceptance per part).
- A hidden **deliverable** several branches share (make it an explicit shared node).
- A shared **decision/contract** several branches *assume* (a data model, API, or
  convention) — promote it to one node and depend the leaves on it *before* sealing them,
  so none is sealed over an undecided foundation.
- Too large for one agent/person to finish cleanly in one go.
- Real parallelism to unlock (independent children that can proceed at once).

## Don't split when

- One coherent activity with a clear, checkable "done".
- Splitting would only produce trivial fragments ("write the headline", "pick the
  font") that add tracking overhead without improving the result.
- A single skill or agent handles it well as a unit — don't decompose below that
  grain.

## Defer when — the knowability horizon

A node can *deserve* splitting yet not be splittable **yet**, because you don't know
what its children are until an upstream result lands. Splitting it now would be
guessing. **Defer** instead (tick step 7): leave it coarse, `blocked`, with a
dependency on the work that will answer it; the next pass decomposes it once that
work is `done`.

The test that separates **seal**, **split**, **defer**, and **discover** is one question:

> *Can I write this node's children — or its `done_looks_like` — correctly **right
> now**, without guessing?*

- **Yes, and it's one unit** → **seal** (a knowable leaf, even if a dependency blocks
  its *execution*: "Produce the hero image" is one task no matter what the logo turns
  out to be).
- **Yes, and it's several units** → **split** now.
- **No — I can't even name the children until X is decided** → **defer** on X
  ("Build the storefront" is "configure a theme" *or* "build 8 components" depending
  on **Choose platform** — so split out *Choose platform*, make storefront depend on
  it, and decompose storefront on the pass after the platform's chosen).
- **"Yes" — but only by *assuming* a load-bearing unknown** → **discover**: seal a
  discovery leaf that produces the evidence, and defer the dependent on it (see *Discover
  when* below). The tell: you *can* write a `done_looks_like`, but it rests on a guess
  about feasibility, fit, approach, mechanics, magnitude, an external answer, or whether
  it's worth doing at all.

Rule of thumb: **stop decomposing a branch at the point where the next level depends
on an output that doesn't exist yet — or on a fact you'd only be guessing at.** Don't
plan detail you can't know — that detail is the *output* of execution (or of discovery),
not of more planning. This is what makes the plan→execute→replan rhythm real rather than
a single doomed up-front guess.

## Discover when — the evidence check (route it AI-first)

Sealing asks *"can I write `done_looks_like`?"* — but a fluent planner can always write a
*plausible* one, even over a guess. So before you seal, check: **is this `done_looks_like`
something you know, or something you're assuming?** Name the evidence in a line, or mark it
`ASSUMED: <the guess>`. If a **load-bearing** assumption (the work is wasted or wrong if it's
false) is unknown, don't seal build work over it — seal a **discovery leaf** that produces the
missing knowledge and **defer** the dependent on it (the defer machinery, but the blocker is
something to *find out*, not to *build*).

**What warrants discovery — the unknowns** (full scope, not just dev features):

- **Feasibility** — *can this even be done?* (will the mortgage approve; does the API support
  batch writes; can we get the licence).
- **Desirability / fit** — *is this the right thing — does anyone actually want it?* (the user,
  the partner, the team) — settled by validation, not assertion.
- **Approach** — *which of several real options?* (buy vs build vs partner; which vendor / school
  / treatment) — **including a cross-cutting architecture/contract several leaves share but that
  isn't decided yet**: route it AI-first as **one shared design/decision node** and **defer the
  dependents on it**, rather than sealing each leaf over its own guess at the shared shape.
- **Mechanics** — *how does this system / process / person actually work?*
- **Magnitude** — *how big or costly is this really?* — the sizing spike you can't plan the rest
  without ("audit the finances", "scope the renovation").
- **External answer** — *gated on someone else's information or decision* ("ask the client what
  they need", "wait for sign-off").
- **Worth** — *should we do this at all?* — the cheap falsification / kill-criteria before
  committing real effort.

**Resolve it the cheapest way — AI first, human last.** AI research and AI build are now cheap;
**human time is the scarce resource**, so the value of human pre-work is far lower than it used to
be. Route each discovery leaf to the cheapest resolver that clears the uncertainty, and set its
`type` + owner to match:

1. **Investigate (AI)** — web research, docs, data-gathering. Most *feasibility, mechanics,
   approach, market/worth* unknowns answer here. `type: research`, owner `ai`. **The default
   first move** — reach for it before ever routing to a human.
2. **Probe (AI)** — a cheap, **reversible** build/spike; learn from the result. When trying is
   cheaper than studying *and* undoable, don't even gate — seal the probe as build work and let
   the outcome replan you. Cheap dev inverts the old calculus: prefer build-and-learn over upfront
   analysis.
3. **Ask / decide (human) — the reserved exception.** Escalate to a person *only* when the unknown
   is genuinely human-only — a personal preference/decision, external sign-off, tacit/relational
   knowledge, a real-world action — **and** still material after AI has done what it can (it
   couldn't get the uncertainty under a low bar). Owner `human`; it parks on `next --owner human`.
   Never spend scarce human time on what AI can research or cheaply probe.

**The boundary — don't gate what you can just try.** Discovery is for the **load-bearing +
expensive-or-irreversible-if-wrong** unknown only. Routine, low-stakes, or reversible-and-cheap
work just gets sealed — and with AI build cheap, "reversible-and-cheap" now covers a lot: a
reversible probe is its own cheapest discovery. Discovery before buying the house; an AI probe,
not a study, before trying the restaurant.

## Worked example — "Launch the e-commerce store for a perfume brand"

Tick by tick, this decomposes (abbreviated) to something like:

- **Brand identity** (mixed → split: positioning/decision, name/decision, voice).
- **Product definition** (research + a formulation decision — split; some parts are
  real-world human tasks, not ai work).
- **Visual design system** *depends-on* Brand identity.
- **Logo** *depends-on* Brand identity. It's a **shared** node: both the hero image
  and the favicon depend on it → promote it, don't duplicate it.
- **Hero image** *depends-on* Logo + Visual design system (a visual-creation leaf).
- **Storefront build** (mixed → split: catalog model / cart+checkout / CMS), each
  *depends-on* the Visual design system.
- **Storefront API / data contract** (a design/decision node): catalog, cart, and CMS each
  *depend-on* it — a shared **contract** they all *assume*, not a shared *artifact* they
  consume; built apart they'd each invent an incompatible schema, so promote it and depend
  them on it rather than letting each leaf guess.
- **Product copy** *depends-on* Product definition.

The stacking — logo feeds hero feeds homepage; brand feeds everything — is exactly
what the dependency edges capture, and it's why the leaves come out in a sensible
dispatch order rather than a flat to-do list.

## When you stop

A branch is fully decomposed when each of its frontier nodes has become a sealed,
skill-grain leaf. The whole plan converges when **no** node anywhere passes the
split test (or hits the depth-5 cap). At that point the leaves, read in dependency
order, are the plan.
