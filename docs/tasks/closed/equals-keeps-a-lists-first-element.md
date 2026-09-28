---
title: A list under `equals` is read as its first element
part_of: '[Closed tasks](/docs/tasks/closed/closed.md)'
created: 2026-09-22T19:15:52.148645Z
status: done
updated: 2026-09-28
description: '`where: { equals: { status: [done, dropped] } }` parses as `equals: { status: done }` — the rest of the list is dropped without a finding, so a view that meant any of the terms silently matches one'
---

# A list under `equals` is read as its first element

**Done** (2026-09-28), by [views as queries](/docs/proposals/views-as-queries/proposal-views-as-queries-v1.md):
`where:` is a CEL expression now, and `status in ['done', 'dropped']` reads a
list on the value side as *any of these* by CEL's own definition of `in`. A
`where:` still written as a mapping of predicates is a config finding that
prints its replacement, so nothing is dropped in silence. The body is left as
it was filed.

**Repro.** A workspace carrying the `tasks` preset as it stood before
2026-09-22, whose `open-tasks` view said:

```yaml
where:
  not: { equals: { status: [done, dropped] } }
```

A task with `status: done` is left out of `prov views open-tasks`; one with
`status: dropped` is listed, as if it were open. `prov check` says nothing
about the view.

**Why.** `equalities_of` in `prov-views/src/filter.rs` takes
`scalar_texts(v).into_iter().next()` — the first text of whatever the value
is — so a sequence becomes its first element and the rest is discarded. The
documented reading of a list (`config-vocab.md`, the `where:` table) is about a
list-valued *field* — `equals: { people: Ada }` matching a document that lists
several people — and says nothing about a list on the condition's side, which
is exactly the gap a person writing "any of these terms" falls into.

The preset now spells the condition as one `equals` per term under `any-of`,
which is correct under today's semantics; this task is the engine half.

## Done when

A list on the value side of `equals` is either read as *any of* these values
or refused as a config finding naming the view and suggesting the `any-of`
spelling — which of the two is the decision this task makes, and the
predicate table in `config-vocab.md` says it. Nothing is dropped in silence
either way.
