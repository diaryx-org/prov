---
title: A date is exact or unknown, and nothing in between
description: "The date grains cut an ISO date and refuse anything else, and `check` does not validate a `type: date` value at all — so `c. 1913`, `1943-05`, `before 1920` and `between 1918 and 1922` all file as undated, silently, and a typo files the same way"
author: adammharris
created: 2026-09-17
updated: 2026-09-17
status: done
part_of: '[Tasks](/docs/tasks/tasks.md)'
---

# A date is exact or unknown, and nothing in between

**Status.** Done, 2026-09-17, in `feat(views): a date is EDTF, and a date
field holding prose is a finding`. The parse is the
`edtf-core` crate's, levels 0–2, wrapped by `prov-views/src/date.rs`, which
decides what a grain makes of it; the repair is `edtf-normalize`'s reading of
the prose. Three of the questions below settled differently from how they
were asked:

- **`unknown` is not a value the type knows.** EDTF has its own spelling of
  a date that is not known — `XXXX`, a year with every digit unspecified —
  so prov has no keyword: `XXXX` is what the type accepts and what a view
  files as undated, and `unknown` is a `MalformedDate` whose repair is
  `XXXX`. Diaryx's document record spec and its importers move from
  `unknown` to `XXXX`; that is diaryx's task, downstream of this one.
- **An interval is under every group both ends reach**, not its start —
  `1918/1922` at year grain is five groups, the way a letter about two
  people is under both. An open or unknown end contributes nothing. The
  per-view choice of which end (`by: { year: start }`) waits for the view
  language that would spell it.
- **Level 1 is not a line.** The crate parses all three levels, and the
  grains cut what they understand: a season to its year, a set to nothing.
- **Sorting within a group** is left as it was — rows in path order — and
  is the deferred `sort:` axis's to take up, which can now read a date.

The date grains (`year`, `month`, `day`) validate rather than slice, which is
right: `banana` at year grain is no group, not the group `bana`. But the only
value they accept is a calendar date, or an RFC 3339 instant read as the date
it starts with. Every other statement of when something happened is a value
no grain can cut, and a document carrying one lands in `(ungrouped)`.

Diaryx's document record spec turns that into a convention — `date_of_document:
unknown` is the deliberate way to say a scan is undated — and then names the
two gaps this leaves in one paragraph: nothing privileges the literal `unknown`,
so `May 1943` files as undated identically, and `check` does not validate a
`type: date` value, so nothing ever says so.

An archive is mostly approximate dates. A photograph is "about 1913"; a letter
is "May 1943" because the day is torn off; a deed is "before the sale in
1921"; a birth is "between 1918 and 1922" from two censuses that disagree. A
workspace that can only say a day or nothing cannot order a shoebox, and
ordering the shoebox is the first thing its keeper wants.

There is a standard for exactly this. The Library of Congress **Extended
Date/Time Format** (EDTF, ISO 8601-2) writes a reduced precision date as
`1943-05`, an approximate one as `1913~`, an uncertain one as `1913?`, an
interval as `1918/1922`, an open-ended one as `../1920`, and `unknown` as
itself. It sorts, it grains, and a reader with `cat` can guess most of it.

## Done when

- A `type: date` field accepts **EDTF level 1** — reduced precision,
  `~`/`?`/`%` qualification, intervals with open and unknown ends, and the
  `unknown` marker — alongside the calendar dates it accepts today. Level 2
  (sets, seasons, exponential years) waits for a use.
- The date grains cut what they can and refuse what they cannot, on purpose:
  `1913~` cuts to `1913` at year grain and to nothing at month; an interval
  groups by its start, or by nothing, as the view says — `by: { year: start
  }` is one shape, and which is the default is settled here.
- `check` reports a `type: date` value that is neither a calendar date nor
  EDTF as a finding on the document, naming the field, so `May 1943` is a
  typo and not a silent undated record. The repair offered is the EDTF
  spelling when one is unambiguous.
- Sorting within a group orders qualified and partial dates sensibly: `1943`
  before `1943-05` before `1943-05-12`, an interval by its start.

## What to settle

- **Whether `unknown` stays a convention.** It becomes a value the type
  knows, which is the smaller change: the document record spec already
  writes it, and a workspace that never declared `type: date` keeps treating
  it as a string.
- **Instants.** An RFC 3339 instant is not EDTF, and the grains read it as
  the date it starts with today. Keep that; `updated:` is an instant and
  should not become a finding.
- **Which crate.** The grains live with the views; the finding lives with
  `check`. Both read the same parse, which wants to be one small module
  rather than two spellings of EDTF.

Diaryx's people-records task depends on this for a lifespan, and its
transcription and import tasks write `date_of_document` in exactly these
forms.
