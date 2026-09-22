---
title: No relation lets a document say it replaces, or was derived from, another
description: The default vocabulary has containment and cross-reference and nothing for succession or origin, so a rewritten proposal says "supersedes v1" in a sentence prov cannot read — a claim the record cannot make on the author's behalf, and one Dublin Core and PROV-O both have a word for
author: adammharris
created: 2026-09-16
updated: 2026-09-16
status: done
part_of: '[Closed tasks](/docs/tasks/closed/closed.md)'
---

# No relation lets a document say it replaces, or was derived from, another

**Status: done (2026-09-16).** Resolved by `feat(relation): the base vocabulary
says replaces and derived_from`. Both pairs joined the base vocabulary rather
than a preset — `replaces`/`replaced_by` and `derived_from`/`derivations`,
glossed in `RelationSet::diaryx_means` and mapped to Dublin Core and PROV-O in
[Spec](/docs/spec.md) §2 — and the snapshots and provenance proposal chains
carry them beside the sentence. Two corrections to what is argued below. There
is no `MissingBacklink`: `check` verifies the inverse of the spanning pair
only, so a `replaced_by` that does not answer its `replaces` is as silent as a
`link_of` that does not answer its `links`, which is what "like any overlay
relation" turns out to mean. And the unit `edit` does not maintain an overlay
inverse either; `mv` retargets both halves and `rm` reports the half it
leaves dangling, and that is the whole of what an overlay pair gets today.

**Where this starts.** The default vocabulary is two pairs — `contents` /
`part_of` for containment and `links` / `link_of` for cross-reference
(`prov-graph/src/relation.rs`, `RelationSet::diaryx_means`). Neither says
*succession* or *origin*. This repository's own proposals show the cost:
[Snapshots v2](/docs/proposals/history/proposal-snapshots-v2.md) is
`rejected`, and its body says "Superseded by v3" in a paragraph and
"Supersedes `proposal-snapshots.md`" in a blockquote. Both are true, both are
prose, and a tool listing what replaced what — `dx tasks`, a site build, an
RO-Crate export — has nothing to read.

**Why this is prov's to say, and not the record's.** A history tool knows the
bytes of one file became the bytes of another; only the author knows the second
document is the *successor* of the first — a rewrite from scratch that
supersedes has no byte lineage, and an edit that keeps most of the text may be
a different document. Succession and derivation are statements a keeper makes,
on the same footing as `author` and `generated`: a claim in the file, as
trustworthy as the file, never filled in from a version-control record and
checkable against one. That is why they belong in the vocabulary and not in
whatever tool the workspace is kept under.

Dublin Core and PROV-O have both words already — `dcterms:replaces` /
`isReplacedBy` and `dcterms:source`; `prov:wasRevisionOf` and
`prov:wasDerivedFrom` — and `means:` is where a declared relation glosses them
so an exporter maps mechanically.

**What is settled.** Two relation pairs, both non-spanning, both `many`
(a document may replace several and be derived from several), each an ordinary
overlay edge with the inverse prov maintains for every relation. No new
mechanism: the spec's `relations:` block declares them today, and this is a
question of what the *default* vocabulary says, not of what prov can do.

**What is open.**

- *The names.* `replaces` / `replaced_by` reads as the workspace already
  reads; `supersedes` is the word the proposals use. Derivation is harder:
  `derived_from` wants an inverse (`source_of`? `derivations`?) that is not
  a word anyone writes. Whatever is chosen, `diaryx_means` glosses each.
- *Default or declared.* Shipping them in the base vocabulary means every
  workspace can say them without configuration and a stranger reading
  `about.md` learns them; it also means four more field names prov claims.
  Declaring them per workspace keeps the base at two pairs and puts the
  burden on the one who wants them. The activity task leaves the same
  question open about built-in terms, and the two should be decided together.
- *What `check` says.* An inverse that does not match is the ordinary
  `MissingBacklink` finding, for free. Beyond that — a `replaces` naming a
  document that is not `rejected`, `dropped`, or `deferred`; a document
  replaced twice — is a lint over the vocabulary's *meaning*, which prov has
  so far refused to have. Diagnosis-only if at all, and probably not.
- *The record.* A `replaces` a version-control tool's history contradicts is
  a finding a future integration could report and never repair, by the same
  rule that keeps `check` from writing a confirmation. Out of scope here;
  noted so the relation is shaped for it.

**Done when** a document can declare that it replaces another and that it
was derived from another, in the base vocabulary or a shipped preset, with
the inverse maintained by `edit`, `mv`, and `rm` like any overlay relation;
`about.md` tables the two pairs with their glosses; and the two proposals
above carry the relation instead of, or beside, the sentence — pinned by a
test in which `rm` of the replaced document leaves a broken `replaces` that
`check` reports as it would a broken `links`.
