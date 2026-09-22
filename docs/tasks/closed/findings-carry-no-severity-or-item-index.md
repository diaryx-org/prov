---
title: A finding carries no severity, and a relation site no item index
description: An editor drawing prov's findings beside the rows they concern has to keep its own list of which kinds are warnings, and cannot tell which item of a `contents` list a broken-link finding means when the list repeats a target
author: adammharris
created: 2026-09-16
updated: 2026-09-16
status: done
part_of: '[Closed tasks](/docs/tasks/closed/closed.md)'
---

# A finding carries no severity, and a relation site no item index

**Status.** Done, 2026-09-16, in `feat(validate): a finding carries its
severity, and a relation site its item index`. `Finding::severity()` is total
over the variants and returns `Error` or `Warning`; the line is the one written
on `Severity` — an error is a claim the workspace makes that is not so, a
warning is drift from a claim it still keeps — and the seven warnings are the
list provui had been keeping (`case_mismatch`, `stale_label`,
`term_near_miss`, `confirmation_stale`, `config_spec_ahead`,
`legacy_body_hash`, `legacy_deletions_pointer`). `LinkSite::Relation` is now
`{ field, index: Option<usize> }`, the index counted over the list as written
(a non-string item holds its place), `None` for a scalar; the human line reads
`contents[3]` and `--json` carries `site` and `index` as two keys.

Two things the done-when below asked for were not done as written. The CLI's
exit code did *not* already make the distinction — `check` failed on any
finding, and `getting-started.md` promised it would — so rather than change
the gate it has a flag: `check --ignore-warnings` narrows the verdict to
errors and prints the warnings regardless, and the count line says how many
of each. And `--fix mechanical` stays expressed in terms of the repair's
`Warrant`, not the finding's severity: a warning may offer only a judgment
(`TermNearMiss`) and an error a derived repair (`MissingInverse`), so the two
are different axes and `Severity`'s docs say so. provui drops its
`WARNING_KINDS` and its target-matching once it moves to the release that
carries this.

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
