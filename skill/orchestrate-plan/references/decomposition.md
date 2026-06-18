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
branches need** — a logo that blocks both the homepage and the about page, an API
contract two features build against? Promote it to its own node so every dependent
can unblock independently the moment it's done. Wire the dependents to it with
dependency edges. This is what turns a flat list into a *stacked* graph.

## Split when

- Mixed activity types (needs different tools / acceptance per part).
- A hidden dependency several branches share (make it explicit and shared).
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

The test that separates **seal**, **split**, and **defer** is one question:

> *Can I write this node's children — or its `done_looks_like` — correctly **right
> now**?*

- **Yes, and it's one unit** → **seal** (a knowable leaf, even if a dependency blocks
  its *execution*: "Produce the hero image" is one task no matter what the logo turns
  out to be).
- **Yes, and it's several units** → **split** now.
- **No — I can't even name the children until X is decided** → **defer** on X
  ("Build the storefront" is "configure a theme" *or* "build 8 components" depending
  on **Choose platform** — so split out *Choose platform*, make storefront depend on
  it, and decompose storefront on the pass after the platform's chosen).

Rule of thumb: **stop decomposing a branch at the point where the next level depends
on an output that doesn't exist yet.** Don't plan detail you can't know — that detail
is the *output* of execution, not of more planning. This is what makes the
plan→execute→replan rhythm real rather than a single doomed up-front guess.

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
- **Product copy** *depends-on* Product definition.

The stacking — logo feeds hero feeds homepage; brand feeds everything — is exactly
what the dependency edges capture, and it's why the leaves come out in a sensible
dispatch order rather than a flat to-do list.

## When you stop

A branch is fully decomposed when each of its frontier nodes has become a sealed,
skill-grain leaf. The whole plan converges when **no** node anywhere passes the
split test (or hits the depth-5 cap). At that point the leaves, read in dependency
order, are the plan.
