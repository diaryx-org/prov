---
title: A finding carries no severity, and a relation site no item index
description: An editor drawing prov's findings beside the rows they concern has to keep its own list of which kinds are warnings, and cannot tell which item of a `contents` list a broken-link finding means when the list repeats a target
author: adammharris
created: 2026-09-16
updated: 2026-09-16
status: open
part_of: '[Tasks](/docs/tasks/tasks.md)'
---

# A finding carries no severity, and a relation site no item index

**Where this starts.** provui draws `check`'s findings in the editor — a body
site as a highlight in the prose, a metadata site as a marker on the row
(landed there in `9b95ed7`). Two things it needed were not on the finding.

**Severity.** `Finding` has a `kind()` and a `subject()` and no
`severity()`. `TermNearMiss` is a nudge and `BrokenLink` is an error, and
every consumer that draws them differently has to keep the list of which is
which — provui's is a match over `kind` strings with a comment per arm, and a
kind added here lands there as an error by default. The distinction is
already made once, in the CLI's exit code and in the remedy tiers; it should
be a method on the finding, so that a consumer inherits the judgement rather
than restating it.

**Item index.** `LinkSite::Relation(name)` names the field and not the item.
A finding about the third link of a `contents` list arrives as "contents",
and an editor recovers the index by matching the finding's `target` against
the links it can read off the document itself — which works until the list
repeats a target, and then it cannot. The census reads each item in turn and
knows the index at the moment it makes the site; `Relation { field, index:
Option<usize> }` would carry it, with `None` for a scalar field.

**Done when** `Finding::severity()` returns one of a small closed set
(`Error`, `Warning` at least) for every variant, the CLI's exit code and
`--fix mechanical` are expressed in terms of it, and a broken link in a list
relation reports the index it sits at — pinned by a test in which the same
target appears twice in `contents` and only one is broken.
