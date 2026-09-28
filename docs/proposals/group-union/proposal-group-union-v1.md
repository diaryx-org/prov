---
title: grouping by every field, not the first
created: 2026-09-28
updated: 2026-09-28
status: draft
part_of: '[Proposals](/docs/proposals/proposals.md)'
---
# Grouping by every field, not the first — a union form for `group:`

## Status

**Draft.** Argued 2026-09-28. Nothing here is built. The open questions at
the end are the ones a decision needs.

## Summary

`group:` takes a field path or a list of them, and the list is a **chain**:
the first field that carries a value supplies all of the document's group
keys, and the rest are never read.

```yaml
views:
  daily:
    group: [date_of_document, created]   # first non-empty wins
    by: month
```

That is the right reading for a view whose fields are one fact with
fallbacks — *the date this document is about, or failing that the date it
was made*. It is the wrong reading for a view whose fields are **several
facts that each place the document**. An activity calendar, at `by: day`,
wants a document under the day it was created *and* the day it was last
updated. Under the chain it lands on one of them only, whichever field
comes first.

This proposal adds a second spelling, a **union**, in which every listed
field contributes its keys:

```yaml
views:
  activity:
    group: { any-of: [created, updated] }
    by: day
```

A document created on `2026-09-01` and updated on `2026-09-20` is under both
days. One created and updated on the same day is under that day once.

## Why this is not a new behaviour

prov already places one document under several groups, and says so as the
point of a view: a multi-valued field yields one key per element, so a
letter listing two people is under both. The union is that behaviour,
sourced from more than one key rather than from more than one element of
one key. `group()` already deduplicates a document within a bucket
(`people: [Ada, Ada]` is one row under `Ada`), and a `RowSet` already keeps
the *document* count apart from the *row* count, so neither the output
shape nor the counts need to change. Only `Grouping::keys_of` does: rather
than returning at the first non-empty field, it concatenates what every
field yields.

## Spelling

The list must keep meaning a chain. Every declared view that writes
`group: [a, b]` today means *first non-empty*, and a reading that changed
under it would silently move documents between groups — exactly the kind
of change a view cannot report, since a view has no invariant to fail.

So the union is a mapping with one key. The proposal is `any-of`, because
the vocabulary already has it: in `where:`, `any-of` is *a document matches
if any of these holds*, and a union group is *a document is under a key if
any of these fields yields it*. The same word, the same disjunction, one
place earlier in the view. Alternatives considered:

- `{ union: [...] }` — says the set operation directly, but introduces a
  word the config does not otherwise use.
- `{ all: [...] }` / `{ each: [...] }` — reads as *every field*, which is
  what is read, but `all-of` already means conjunction in `where:`, and
  `all` beside it would read as the opposite of what it does.

A chain would get no mapping spelling of its own (`{ first-of: [...] }`);
the list is the chain and stays the only way to write one. Adding an
explicit form for it is possible later and not needed now.

## What the union means at the edges

- **The grain applies per field, then the keys are united.** `by: day` cuts
  `created` and `updated` separately; a value the grain rejects contributes
  nothing, as it does now. A document is ungrouped only when *no* field
  yields a key.
- **The chain's no-fall-through rule does not carry over, and does not need
  to.** A chain refuses to fall through past a present-but-unparseable
  value, because that would file the document under a date it does not
  claim. A union files under every date it does claim; a bad `created`
  beside a good `updated` leaves the document under the `updated` day, and
  the bad value is a `where:`/`fields` question, not a grouping one. The
  document is not hidden, and it is not placed anywhere it does not say.
- **Field paths work as everywhere else.** `{ any-of: [written.on,
  confirmed[].by] }` reads each path as a `fields` declaration writes it.
- **`nest:` over a union is refused.** Filing needs one home, and a union
  is by construction not single-valued — the same reason `nest:` over a
  field declared `type: seq` is `NestNotSingleValued`. Unlike that case,
  there is nothing to weigh: the seq diagnosis has two repairs that say
  different things about the workspace, while here the fact is in the view
  alone and the only repair is to drop the `nest:` or write a chain. So
  this can be refused when the view is parsed or linted, without needing
  the `fields` block to share a surface.
- **`prov views --json`** lists `group` as an array today. The listing
  needs to say which form a view uses — either `group` becomes
  `{ "chain": [...] }` / `{ "any-of": [...] }`, or a `group_mode` key sits
  beside the array. The second is additive and changes nothing for a
  consumer that reads the array now.

## The alternative: no change

A consumer can declare two views, `by-created` and `by-updated`, run both,
and unite the day keys itself. That works today and costs prov nothing. It
is clunkier in exactly the way views exist to remove: the union becomes
code each consumer carries, the two selections are run separately, and the
document count across both is the consumer's to get right — the split a
`RowSet` already keeps. If the answer here is no, this is the answer.

## Open questions

1. **Spelling** — `any-of`, or one of the alternatives above.
2. **Chains inside a union.** A workspace may want *the document's date or
   its creation date, plus its update date*:
   `{ any-of: [[date_of_document, created], updated] }`. That is a list
   element meaning a chain, which is what a list means at the top level. It
   is a natural reading, but the first version could be flat and leave this
   until someone asks.
3. **Refusal or diagnosis** for `nest:` over a union: a `ViewSpec::parse`
   error, or a lint issue alongside the other view issues.
4. **The JSON listing shape**, per above.
