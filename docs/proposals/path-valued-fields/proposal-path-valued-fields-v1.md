---
title: path-valued fields
author: adammharris
created: 2026-09-17
updated: 2026-09-17
status: implemented
part_of: '[Proposals](/docs/proposals/proposals.md)'
derived_from:
- '[provenance and attestation](/docs/proposals/provenance/proposal-provenance-v1.md)'
- '[provenance v2 — who confirmed this, and against what](/docs/proposals/provenance/proposal-provenance-v2.md)'
---
# Path-valued fields — a link that sits beside other facts about itself

## Status

**Implemented** (2026-09-17), on `main` and not yet released, in the commit
that closes this document. Both phases of §7 landed together — the `[]` step
and `type: ref` share the one address grammar, and the second was the reason
for the first. The body is left as argued. One thing settled in the build
that the draft left open: a removal repair on a `ref` value takes the one
key and leaves the facts beside it, the way unlinking a body link leaves
its text.

Accepted the same day. The argument was made twice before it was written
down here: [provenance v1](/docs/proposals/provenance/proposal-provenance-v1.md)
§6 proposed it and said it deserved a proposal of its own; [provenance
v2](/docs/proposals/provenance/proposal-provenance-v2.md) §7 declined to settle
it and named what the provenance family needs from it. This document is that
proposal, with one change to the spelling (§3) that came from reading the
code rather than the sketch.

## Summary

A `fields` declaration can already turn a carried string into a *term* — a
value looked up by key in a vocabulary. This proposal lets it turn a carried
string into a *link*: a value resolved against the workspace the way a
`links:` entry is, reported when it dangles, rewritten when its target moves.

```yaml
prov:
  fields:
    sources[].resource:
      type: ref
```

Two things, and they are separable:

1. **A `[]` step in a field path.** `sources[].resource` names the `resource`
   key of every entry in the `sources` list; `confirmed[].by` names the actor
   of every confirmation. Today a declaration names a top-level key or a
   dotted path into a mapping, and cannot reach into a list. This is needed
   whether or not the field is a link — v2's actors vocabulary is
   term-valued and is blocked on it alone.
2. **`type: ref` is read, not only carried.** The config vocabulary already
   has it — "a link to another document" — and prov has carried it without
   interpreting it. From now on a field declared `ref` is a link site: its
   values are censused, resolved, checked and rewritten like a relation's.

What it is not: a relation. A path-valued field is one-way — no inverse, no
backlink field, never spanning — because a relation is what a link that
stands alone should be, and the base vocabulary has just gained
`derived_from` for exactly the flat case. The rule this proposal ends with is
in §2.

## 1. The bug it fixes

Every workspace has a field somewhere holding a path prov does not know is a
path. A card in a genealogy vault:

```yaml
sources:
  - resource: cards/1910-census-sheet-4.md#line-12
    title: 1910 US Census, Ward 3
    consulted: 2026-03-02
  - resource: https://familysearch.org/ark:/61903/…
    title: FamilySearch image
```

`sources` is a carried field (DESIGN §2, tier 3). `check` does not read it,
so a `resource` that names a document that was never there is silent. `mv
cards/1910-census-sheet-4.md …` does not read it either, so the first move
turns a working reference into a broken one, and prov — which promised that
a move rewrites every reference to where a document was — is the author of
the rot. The URL in the second entry is fine, and would stay fine: an
external target is recognized by syntax and never resolved (spec §4).

The fix is not a new kind of link target. The machinery — resolution,
locators, dangling reports, rewrite-on-move — is keyed on what a target *is*,
not on which field it was found in. What is missing is the declaration that
tells prov the field holds one.

## 2. Why not a relation

The obvious answer to "a field holding links" is `relations:`. Declare
`sources` with a cardinality and an inverse and it gets everything a `links:`
entry gets, plus a `source_of` on the other side. For a list of bare links
that is the right answer, and this proposal does not change it.

It fails the moment the entry is a mapping. A relation's value is a list of
links and nothing else; an entry cannot carry `title`, `consulted`, `page`,
`role` beside the target. A citation wants those. A person record wants
`people: [{who: people/ada.md, role: witness}]`. The moment there is a fact
*about the reference* — not about the target, about this particular act of
referring — the link has to sit inside a structure, and the only way to tell
prov that one string inside that structure is a path is a field declaration
with a path into the structure.

So the rule, stated once:

> **A link that stands alone is a relation. A link that sits beside other
> facts about itself is a path-valued field.**

The two are not in tension. A workspace that wants both a backlink *and*
per-reference facts declares the relation for the backlink and the field for
the facts, and the same target appears in each.

## 3. The spelling: `type: ref`, not `values: path`

v1 sketched `values: path`, a third value beside `open` and `closed`. The
code says otherwise. `FieldSpec` declares two independent things: a **type**
— what the value *is*, "pure data shape, decidable from the value alone",
spelled in `fig-schema`'s vocabulary — and a **vocabulary** — which values
are *legal*. `values:` is the posture of the vocabulary and is "meaningful
only alongside" one. A link is not a posture; it is a shape. And the shape
already has a name: `ref`, "a link to another document", in the type table
since types were declared, carried by prov and never read.

So this proposal adds no enum. It makes prov read a declaration it already
accepts. `values:` keeps meaning what it means, and a `ref` field can still
carry a vocabulary if a workspace finds a use for one — the two axes stay
orthogonal, as `FieldSpec`'s own doc says they are.

## 4. What a `ref` field gets, and what it does not

A field declared `ref` is a **link site**, and each of its values is treated
exactly as a relation entry is:

- **Any spelling `Link::parse` accepts** — a bare path, `[label](path)`,
  `[[Title]]`, `id:…`, `id:<ws>/…`, a URL — and the same resolution: a path
  against the tree, an id through the registry, a title through the index,
  an external target by syntax and never resolved, a `#locator` carried and
  never checked.
- **Censused.** The target is *reached*: it is in the reachable set, so it is
  not an orphan, it is fixity-checked, and it is in a history capture — the
  same standing an overlay `links:` target has.
- **Checked.** A path with nothing on disk is `BrokenLink`; a dangling id,
  a case mismatch, an ambiguous title are the findings they already are. The
  site is named as `sources[2].resource` — the declared path with the list
  position filled in, which is the address a repair edits.
- **Rewritten.** `mv` retargets every path-form value that resolved to the
  moved document, keeping its label and wrapper; a document whose directory
  changes re-relativizes its own; `retitle` refreshes a stale label; `rm`
  reports the value it leaves dangling; `convert --links` restyles it.
- **Repairable.** A broken value offers the retarget/remove pair a relation
  entry offers, addressed by the same concrete path.

What it does not get, on purpose:

- **No inverse.** Nothing is written on the target. A reader who wants "what
  cites this" asks the backlink map, which the census already answers, or
  declares a relation.
- **Never spanning.** A `ref` field cannot be the containment spine; the spine
  is a relation with a single-valued inverse, and stays one.
- **No scope.** `under:` scopes what a field's values must *be* — one status
  vocabulary under `Tasks`, another under `Proposals`. Whether a key is a
  link is a fact about the vocabulary, like a relation's name, and holds
  everywhere. A `ref` declaration that also says `under:` is reported by
  `check` as a config issue and read as unscoped.
- **No type check on the target.** `ref` says the value is a link, not what
  the link must land on. A `resource` that resolves to an image sidecar is a
  resolved link.

## 5. The `[]` step

A field path is a sequence of steps: a key (`sources`), a key inside a
mapping (`generated.how`), and now **every item of a list** (`sources[]`).
Steps compose: `sources[].resource` is the `resource` key of each item;
`confirmed[].by` the `by` of each confirmation. A path that reaches a scalar
governs that value; a path that reaches a list governs each string in it,
which is how `tags` has always been read and stays so.

Every reader of a declaration follows the path the same way:

- `check` judges every value the path reaches — for a vocabulary, each is a
  term; for a `ref`, each is a link.
- A repair edits the one it was raised on. The finding names the concrete
  address (`sources[2].resource`), and the concrete address is what the
  editor addresses: a numeric step is a list index, so the path grammar is
  its own address grammar.
- `default:` is written at the path for a key path. A path with a `[]` step
  has nowhere to write a starting value — a new document has no list to fill
  — so a default on one is carried and never written.

Where the path lands on nothing, the document is held to nothing, as today.

## 6. Consumers

- **provenance v2 §7, Phase 2** — an actors vocabulary over `confirmed[].by`
  and `generated.by`, term-valued, needs the `[]` step and nothing else here.
- **provenance v2 §7, `sources`** — `sources[].resource` as a `ref`, with
  `title` and the rest carried beside it.
- **diaryx** — a card's citation (`sources`, with a locator) and a person
  record's `people[].who`; both tasks link this proposal by description and
  follow it under the name it lands with.

## 7. Staging

**Phase 1 — the `[]` step.** Field paths parse a `[]` step; `check` judges
every value reached; repairs address the concrete path. No new config.

**Phase 2 — `type: ref`.** The census reads `ref` declarations from the
read settings and censuses each value as a link site; every rewriter that
walks relation fields walks `ref` fields too; `about.md` says which fields
are links; `check` reports a scoped `ref`.

**Not here.** A `ref` that constrains what its target may be (a document
under `People`) — a real want, and the reified-vocabulary machinery is where
it would come from, but no consumer needs it yet.
