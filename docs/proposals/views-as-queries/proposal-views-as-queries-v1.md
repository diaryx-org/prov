---
title: views as queries
created: 2026-09-28
updated: 2026-09-28
status: implemented
part_of: '[Proposals](/docs/proposals/proposals.md)'
---
# Views as queries — CEL for reading, the spine left to filing

## Status

**Implemented** (2026-09-28), on `main` and not yet released, in the commit
that closes this document. Accepted the same day; it began as a narrower
draft, *grouping by every field, not the first*, which asked for a union form
beside `group:`'s first-non-empty chain, and arguing where that form should
stop turned into the question this document answers. The union is now one
expression, `[day(created), day(updated)]`. The body is left as argued; what
the implementation settled beyond it:

- **A fifth function, `field('a.b')`**, for a nested field path
  (`written.on`, `confirmed[].by`). CEL's own `written.on` fails on a document
  without `written`, and a retired `group: written.on` needed a translation
  that means what it meant.
- **`null` is an empty list to `in` and to a comprehension.** Without it,
  `'Ada' in people` failed on every document that lists nobody, the commonest
  question about a list field. This is the one place prov departs from plain
  CEL. `size(null)` still fails, and is reported.
- **`prov query [WHERE] [--key EXPR]`** runs an expression without declaring a
  view, over the same census. It is where a view is tried before it is
  written down, and what the walkthrough, [Querying your
  files](/docs/querying.md), is built on.
- **A filing entry may omit `nest:`**, to say only which index new records go
  under, and carries a `label`.
- **`prov config views.<name>.<key>` judges the entry whole**, so a view can be
  built one setting at a time once it has a key.

## Summary

A view stops being a set of verbs and becomes two expressions in an
embedded query language, [CEL](https://github.com/google/cel-spec):

```yaml
views:
  open-tasks:
    label: Open tasks
    where: >-
      doc.ancestors.exists(a, a.title == 'Tasks')
      && present(status) && !(status in ['done', 'dropped'])
    key: status
```

Three changes, each of which stands without the others:

1. **`where:` and `key:` are CEL.** They replace `where:`'s predicate
   mapping, `group:`, and `by:`. prov supplies a small, closed set of
   functions that carry its own decisions — `year`, `month`, `day` and
   `initial` cut values as the grains did, `first` is the fallback chain,
   `present` is `has`.
2. **Views do not know the spine.** `under:` is gone. prov walks the spine
   once, when it builds the census, and hands each document's place in it
   to the query as data: `doc.ancestors`.
3. **Filing leaves views.** `nest:` moves to a top-level `filing:` axis,
   which keeps the anchor, the field and the grain, and every check that
   filing needs.

## How this got here

The draft asked for `group: { any-of: [created, updated] }`, so an activity
calendar could put a document under every day it has a date for. The chain
could not be changed to mean that, so the union needed a spelling of its own,
and its second open question was whether a chain could sit inside a union.

Answering that was answering a bigger question. `where:` was already a small
language: two predicates and three combinators that nest. A union inside
`group:` made a second one. Nesting chains inside unions is composition, and
that is the point where a set of verbs has become a language whether or not
anyone calls it one. At that point, choosing a language on purpose is better
than growing one by accident.

The reasons the old format gave for staying a closed set of verbs turned out
to come from one place: `nest:`. Filing writes into the single-parent spine,
so it needs to know before anything runs that a grouping gives one value per
document and that its grain chains. Every constraint that made a language look
dangerous came from filing. Reading has none of them. DESIGN already says a
view has no invariant: a wrong view shows the wrong rows and you edit the file.

So the abstraction was wrong. A view was a way of reading and a rule for
filing at once, and the filing half's constraints were being placed on the
reading half. Exports met the same problem earlier and solved it the same way:
the part with an invariant, the `gate:`, is its own closed declaration, and
the part without one, the view it narrows by, is not.

## Why an embedded language, and why CEL

prov could write its own query language. The only things that would be
prov's own in it are the handful of functions below. A language needs a
grammar, a parser, error messages, documentation and a specification, and none
of that is prov's to be expert in. Embedding one moves the point of no return
DESIGN worried about: CEL's grammar and standard functions are fixed by CEL's
specification and maintained by its authors, and what prov commits to is its
own functions and the shape of the environment.

CEL fits better than the alternatives:

- **It always finishes and cannot write.** CEL is deliberately not Turing
  complete, and an expression has no side effects. `prov-views` keeps its
  structural guarantee: its dependencies are the read core, the EDTF parser,
  Unicode normalization, and now an expression evaluator. None of them can
  write a byte.
- **It was designed for this.** CEL is what Kubernetes, Envoy and Firebase
  embed for expressions inside configuration. It reads like the C-family
  syntax most people have seen.
- **jq** was the closest rival. Its expressions naturally yield zero, one or
  many results, which is exactly what a grain does. But `def f: f;` loops
  forever, and its syntax is dense for anyone who is not a programmer.
- **SQL** assumes tables and joins the workspace does not have, and brings
  aggregates, arithmetic and ordering along whether or not they are wanted.

The implementation is the `cel` crate, pure Rust, which builds for
`wasm32-unknown-unknown`. It is an interpreter without a static type checker,
so prov checks function names itself (below). Two differences from the
specification are worth knowing, and are CEL's to settle, not prov's: `size()`
counts a string's bytes rather than its code points (`size('Å')` is 2), and
the optional string extensions (`lowerAscii`, `split`) are not included.

## The environment an expression sees

- **Each metadata field is a variable of its own name.** `status`, `created`,
  `people`. A field the document does not declare is `null`, so `status ==
  'done'` is simply false on a document without one, rather than an error.
- **`doc` is the document itself**, and wins over a field called `doc`:
  - `doc.path`: workspace-relative
  - `doc.title`: or `null`
  - `doc.id`: the document's own `id` field, else the registry's, else `null`,
    as `prov docs --json` already reports it
  - `doc.meta`: the whole block, for a key that is not a valid identifier
    (`doc.meta['date-of-document']`) or for a field named `doc`
  - `doc.ancestors`: every document above this one in the spine, from the root
    down to its parent, each `{path, title, id}`

prov's functions, and no others:

| function | means |
|---|---|
| `present(x)` | `x` is not `null` and has a non-empty value. What `has:` meant: a field written blank has nothing to show |
| `first(a, b, …)` | the first argument that is `present`. The old chain, including its rule that a present-but-unparseable value does *not* fall through |
| `year(x)`, `month(x)`, `day(x)` | the calendar grains, reading EDTF: `1913~` → `['1913']`, `1918/1922` → five years, `XXXX` and `banana` → `[]` |
| `initial(x)`, `initial(x, n)` | the A–Z grain: the first `n` characters, upper-cased, counted as characters |

Each grain takes a value or a list and returns a list of keys, because a grain
can give none, one or several. The rule for adding a function is unchanged: a
concrete way of reading the workspace that nothing else can express.

The term-states draft plans an `in-state:` condition and a `state` grain.
Under this proposal both become one function, `state(x)`, that returns a
term's declared state. That document should be restated when it is taken up.

## Keys

`key:` is required: a view is a way of grouping. It yields a value or a list;
lists are flattened, each scalar becomes a key as its text, and `null` or an
empty string gives none. A document is under each distinct key once. One that
gets no key is in the ungrouped bucket, as before, reported and not dropped.

```yaml
key: status                                        # one field's values
key: month(first(date_of_document, created))       # the old chain at month grain
key: "[day(created), day(updated)]"                # the union this began as
key: initial(people)                               # an A–Z index of people
```

## Failures are reported, never guessed

An expression can fail on one document and not another: `size(rating) > 3`
on a document whose rating is a list, or arithmetic on text. A failing `where:`
does not include the document and does not silently drop it. Either would be
the guessing the old format refused. The document is listed as a failure with
the message, beside the result, in the text output and in `--json`. A failing
`key:` is reported the same way.

An expression that does not parse, or calls a function neither CEL nor prov
defines, is a config finding (`views.<name>.where`), so `dya(created)` is
caught when the config is read, not when the view runs.

## Views and the spine

`under:` resolved a link, walked the spine below it, and failed if the link
named nothing. It was the one part of a view that knew the workspace has a
shape. Now prov walks the spine once, for the census, and records each
document's ancestors on its row, the way it already records the registry id.
A view filters on that data like any other:

```yaml
where: doc.ancestors.exists(a, a.title == 'Tasks')
```

That still survives a move and a rename, because the ancestry is recomputed
from the spine on every run. It even survives a retitle when written against
the id: `doc.ancestors.exists(a, a.id == '1ch2991')`.

**What is lost** is DESIGN's broken-versus-empty distinction. An `under:`
that named nothing was an error, and `a.title == 'Taks'` matches nothing
without comment. By DESIGN's own reasoning for why views are not a validator,
that is an acceptable loss for a lens, and it is the only guarantee this
proposal gives up.

## Filing

```yaml
filing:
  daily:
    under: '[Daily](id:abc1234)'
    field: [date_of_document, created]
    nest: year
  journal:
    under: '[Calendar](/Calendar/index.md)'
    field: written.on
    nest: ref
```

A filing entry says where a frontend files a new record: below `under:`, by
the first present value of `field:`, at `nest:`, which is a grain or `ref`.
prov still only *describes* the route (`FilingSpec::route`, which was
`ViewSpec::nest_route`), and a frontend still hands it to `plan_route`. The
checks move with it: `nest:` over a field declared `type: seq` is
`NestNotSingleValued`, and `nest: ref` over a field not declared `type: ref`
is `NestRefNotDeclared`, both reported at `filing.<name>.nest`.

## Exports

Unchanged. An export's `gate:` stays a closed declaration made by the
document, because the gate carries the invariant that an export's documents
are a subset of what it admits, and a query mistake must not be able to leak a
file. An export's `view:` narrows by a view's `where:` as it did before.

## Migration

This is a breaking change to the configuration format. A view that still
carries `group:`, `by:`, `under:` or `nest:`, or whose `where:` is a mapping,
is a config finding that names the retired keys and prints its replacement,
with `under:` rewritten against the anchor's title or id and `nest:` as a
`filing:` entry. The tasks preset and prov's own configuration are rewritten
in the same change. A workspace running an older prov cannot read the new
format, so other workspaces move when they take the release that carries it.

## Costs

- **An expression is a string.** An editor that located `views.*.where.has` by
  its position in the YAML sees one opaque value. It needs an expression field
  or a builder that writes CEL.
- **The implementation's differences from the specification** (above) are
  the embedded language's, and prov inherits them until CEL's maintainers
  resolve them.
- **The loss of broken-versus-empty** for scope, argued above.
