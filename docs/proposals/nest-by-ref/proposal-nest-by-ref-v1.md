---
title: nest by reference
author: adammharris
created: 2026-09-17
updated: 2026-09-17
status: implemented
part_of: '[Proposals](/docs/proposals/proposals.md)'
derived_from:
- '[path-valued fields](/docs/proposals/path-valued-fields/proposal-path-valued-fields-v1.md)'
---
# Nest by reference — the record names its shelf

## Status

**Implemented** (2026-09-17), on `main` and not yet released, in the commit
that closes this document. Accepted the same day, from a conversation rather
than a draft: the question was whether `nest:` could be generalized past the
calendar, and the answer was already written in the path-valued-fields
proposal's one sentence — *a link that sits beside other facts about itself
is a path-valued field*. The body is left as argued.

## Summary

`nest:` files a new record into the spanning relation. Today it takes a
**grain** — `year`, `month`, `day`, `initial` — and computes the shelf from
the record's value: `2026-07-24` becomes the index titled `2026` and the one
titled `2026-07` inside it. To do that, prov has to know what a year is.

This proposal adds one spelling, `nest: ref`, that files the record under
**the document its grouping value links to**, and nothing else:

```yaml
fields:
  written.on:
    type: ref
views:
  journal:
    group: written.on
    under: '[Calendar](/Calendar/index.md)'
    nest: ref
```

A record carrying `written: { on: /Calendar/2026/09/17.md, at: "09:12" }`
files under that day node. The day already sits under its month, which sits
under its year, because that is the calendar index's own `contents` chain.

With it, a grouping key becomes a **field path** — `written.on`,
`confirmed[].by` — as a `fields` declaration already writes one, so a view
can group by a key inside a mapping or inside every item of a list.

## The worry this answers

prov is meant to be as unopinionated as it can be while keeping its core.
The core knows nothing about dates: not the spine, not `check`'s containment,
not `mv`, not the census. What does know is behind two opt-ins — a field the
workspace declares `type: date`, and a view that asks for a calendar grain —
and a declared type is a grammar, so that opinion is bounded and fine.

But filing was the one place the opinion leaked into a *write*. A workspace
that wanted a note under a person, a place or a project had no `nest:` for
it, because every nest was a coarsening prov computed, and the only
coarsenings prov knew were the calendar and the alphabet. Each new kind of
shelf would have been a new grain, and each grain another thing prov knows.

## What the reference shape removes, and what it does not

**Removed: every calendar assumption from filing.** `nest: ref` files under
the target. The same declaration files under a day, a person or a project,
and prov cannot tell which. The grain's two conditions are met without prov
checking them:

- *chain* — each coarser shelf determined by the finer — holds because a
  node has one parent, so the target's spine is the chain;
- *single-valued* — one home per record — is the same check as before, on
  the field: a record linking to two shelves has two homes, as one naming
  two people does.

**Removed: the deferred `sort:` axis, for this case.** Ordering by date
needs to know a value is a date. A calendar index in spine order is already
chronological, so a view nested by reference inherits its order from the
index's `contents` and no date is read anywhere.

**Removed: the join that filing through a ref seemed to need.** Filing by
reference never reads a date *through* the ref; it files under the target
and stops. Only something wanting the number out of the day node would need
to read through, and with the spine providing order, nothing does.

**Not removed: who makes the shelf.** Somebody turns today into a document
under the calendar index. That is the frontend's — diaryx creates the day
node the way it creates a year index now — and that is where the opinion
belongs. prov creates nothing: a link to a document that is not there is the
ordinary broken-link finding, not a shelf prov makes. The value grains stay
for a workspace that would rather not keep a node per day, and for the
archive whose dates are `1913~` and `1918/1922` and have no node to point at.

## Why the field must be `type: ref`

Filing by reference is only sound if the value is a *link* — resolved,
checked, and rewritten when the target moves. That is exactly what
`type: ref` declares and what a bare string is not. Without it the filing
works on the day it is written and breaks, silently, the day the shelf is
moved, which is the failure class prov exists to report. So `nest: ref` over
a field the same config surface does not declare a `ref` is a `check`
finding, `NestRefNotDeclared`, diagnosis-only for the reason its sibling
`NestNotSingleValued` is: the two repairs — declare the type, drop the nest —
say different things about the workspace, and the first changes how every
other consumer reads the field.

## Spelling

`nest: ref`, beside the grains, because `nest` already reads the grouping
chain and the record's link *is* the value there. `ref` is a way to file and
not a way to read: `by:` takes grains only, and `by: ref` is a bad grain.
Grouping by the field without a grain groups by the link as written, which
is honest and is what `group:` without `by:` has always meant; grouping by
target *identity*, so two spellings of one link fall together, is a
normalization nobody has asked for and would be the first place a view
resolved anything.

`nest_route` answers with either the index titles to file under (the grain
case, as before) or the link the record carries (the reference case). The
frontend resolves the link from where the record will live, the way a
view's anchor resolves — by path, by `id:`, or by title — and files there.

## What this costs consumers

`ViewSpec::nest` is `Option<Nest>` rather than `Option<Grain>`, and
`nest_route` returns a `NestRoute` rather than a `Vec<String>`. A consumer
that filed by grain matches `Nest::Grain` and `NestRoute::Titles` and is
otherwise unchanged. The config format grows a spelling and loses none.
