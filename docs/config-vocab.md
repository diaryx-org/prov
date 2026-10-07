---
part_of: '[prov](/README.md)'
---
# Config vocabulary — the reshaped spec

> Locked design for the workspace-config vocabulary and where it lives. Supersedes
> the flat, top-level `link_format`/`reference_*`/`embed_*` keys. Complements
> DESIGN §2 (opinionated mechanism), §5 (identity), §6 (reachability), §7
> (serialization).

## The two homes, one vocabulary

Workspace policy is a single namespace of keys that can live in either of two
places — the same keys, the same values:

- **Root document frontmatter**, nested under a `prov:` key. The root mixes
  structural links, identity, and user-owned fields; nesting policy under one key
  keeps it apart, so it is unambiguous to read *and* to lint. This is the
  **description** home — how the workspace is written.
- **The dedicated config document** (`prov.<ext>`), where keys sit at **top
  level** (the whole document is policy, so no wrapper is needed). This is the
  **policy** home — how prov behaves. It is reached two ways: the root names it
  through the `config` relation, *or* prov finds it by convention as the
  **workspace node** — stem `prov`, at the top level, then `config/`, then
  `.config/`. In the ordinary case those are the same file.

This mirrors the `.prettierrc` / `package.json` `"prettier"` duality: a tool's
config sits bare in its own file and namespaced in a shared one. Precedence, both
applied over the defaults:

```
default  <  root `prov:` block  <  workspace node  <  named config document
```

The last two rungs are the same document in every workspace that names its
config, which is why the order is rarely observable. Where they differ, the
explicit pointer wins — a workspace that went to the trouble of naming one meant
that one — and `check` reports the disagreement rather than letting the
precedence quietly decide.

Finding the node by convention is what makes [`root`](#the-vocabulary) possible:
it is the only policy home readable *before* the root is known, so it is the
only one that can say which document the root is. That is also why `root` is
read from the node alone — written in a root's own `prov:` block it names what
has already been found.

The split of *which* axes live *where* is a **convention** `init` authors, not a
mechanism — both homes accept the whole vocabulary, and the config document wins
on any overlap. A minimal hand-authored vault can therefore put a policy key in
the root `prov:` block and never create a config document.

### Converting between the homes

Because the two homes read identically, where policy lives is an ergonomic choice
you can change at any time — `prov config --home <root|sidecar>` relocates the
whole policy:

- **`--home root`** inlines the policy into the root's `prov:` block and removes
  the sidecar (one less file).
- **`--home sidecar`** moves it into `prov.yaml` and clears the root's `prov:`
  block (an uncluttered root).

It is a *move*, not a materialization: only the recognized policy keys travel — no
defaults are baked in, so the effective config is unchanged — and user fields stay
put. A `--home root` that would strand a hand-added field in the sidecar keeps the
file rather than deleting it. (This is distinct from `--setup`, which writes the
*full* effective config — defaults included — into the sidecar for those who want
nothing implicit.)

### Pointers stay top-level

The `config`, `registry`, `deletions`, `history`, and `about` **pointer relations** are
*not* policy — they are structural links the root declares so the workspace
unfolds from its own root (DESIGN §6). They remain at the root's top level
alongside `part_of`/`contents`, resolved by the same link machinery. The
top-level key is a *pointer* (a path to the log); the policy that governs
whether a delete writes to it is `record_deletions`, a bool in the `prov:`
block. (`history` points at a retired event store and survives only so an
unmigrated one stays out of every walk — nothing writes one. `recycle_bin` is
the spelling `deletions` replaced: it is still read, so a root written before
the rename resolves, and `check` reports it as a rename to make.)

```yaml
title: My Vault
author: adammharris
config: prov.yaml             # pointer (structure) — top level
registry: registry.yaml           # pointer — top level
deletions: deletions/index.yaml   # pointer (a path) — top level
history: history/index.md         # pointer (a path) — top level
about: about.md                   # pointer (a path) — top level
tags: [personal]                  # user field — prov never reads it
prov:                         # policy namespace (description home)
  spec: 1
  content_format: djot
  references:
    notation: markdown
    path_style: root
```

## The vocabulary

```yaml
prov:
  spec: 1                     # vocabulary version marker (integer) — held at 1 until after prov 1.0.0

  # ── description: how the workspace is written ──
  content_format: djot        # markdown | djot | html   (body grammar)
  metadata:
    format: yaml              # yaml | json | toml | fig  (frontmatter language)
    embed: delimited          # delimited | code_block | html_script | html_code | separate
  references:
    notation: markdown        # markdown | wikilink | bare
    path_style: root          # root | relative   (path targets only)
    target: path              # path | id | alias
    label: false              # bool — id/alias references carry a |Title label
  spanning: contents          # the single-parent discovery spine (DESIGN §3)
  relations:                  # per-relation *definitions* and reference-axis overrides
    contents:                 # …overlaying the built-in vocabulary — see below
      means: "documents contained by this one"   # human gloss — carried, never read
      cardinality: many       # one | many
      inverse: part_of        # the reciprocal field
      notation: wikilink      # …plus any reference-axis override, same block
      target: alias
    part_of: { cardinality: one, inverse: contents, target: id }
    link_of: off              # not a relation here — an ordinary carried field
  fields:                     # field declarations — types and controlled vocabularies
    audience:
      type: str               # what the value *is* — see "Field types" below
      values: closed          # open (folksonomy) | closed (must be a known term)
      vocabulary: '[Audiences](/vocab/audiences.md)'   # pointer to the term store — its shape says which kind
      default: friends        # what a new document opens with — see "Field types" below
    created:
      type: date              # a type alone is a complete declaration
    modified:
      stamp: edit             # prov writes the time here on each change it makes — see "Stamps" below
    status:                 # several declarations, each scoped — see "Field types" below
      - under: '[[Tasks]]'      # governs the files under this index, and no others
        values: closed
        vocabulary: '[Task statuses](/vocab/task-statuses.yaml)'
        default: open
      - under: '[[Proposals]]'
        values: closed
        vocabulary: '[Proposal statuses](/vocab/proposal-statuses.yaml)'
        default: draft
  views:                      # declared lenses — see "Views" below
    daily:
      label: Daily
      icon: calendar          # a hint for a frontend; prov never interprets it
      where: "under('[Daily](id:abc1234)') && !present(draft)"
      key: month(first(date_of_document, created))   # CEL — see "Views" below
    journal:
      key: field('written.on')                        # a field path, as `fields` writes one
  filing:                     # where a *new* record goes — see "Filing" below
    daily:
      under: '[Daily](id:abc1234)'
      field: [date_of_document, created]
      nest: year              # how deep, independent of how a view reads
    journal:
      under: '[Calendar](/Calendar/index.md)'
      field: written.on
      nest: ref               # file under the document the value links to
    photos:
      under: '[Photos](/Photos/index.md)'
      field: [date_of_document, created]
      nest: year
      kind: [image, video]    # which records it files — see "Which records an entry files"
  exports:                    # what may *leave* — see "Exports" below
    letters:
      label: Letters home
      gate:                   # the membership test, written in each document
        field: audience       # a document is in this export only if…
        value: family         # …its own `audience` declares `family`
      hold: draft             # optional — a document declaring `draft: true` waits
      view: daily             # optional arrangement — may narrow, can never widen
  id_storage: both            # registry | frontmatter | both
  workspace_id: notes         # what this workspace calls itself (omit/"" = anonymous; `prov id --workspace`)
  genesis: 3f9a…0c2a          # the digest of the history's founding revision, written by the program that keeps the history (omit = none)
  root: home.md               # which document is the root — read only from the workspace node (omit = found by the `index`/`readme` scan)

  # ── policy: how prov behaves (conventionally in prov.yaml) ──
  identity: lazy              # none (a.k.a. off) | lazy | eager
  fixity: on                 # off | on — what a checksum covers follows the document's shape
  record_deletions: true     # bool — a delete records what it destroyed
  about: structure           # off | structure — generate about.md, the page that explains this directory
  confirmations: stamp       # stamp | content — what a confirmation is measured against (Provenance §3)
  actors: free               # free | declared — whether a person is named by a link to a person document (Provenance §2)
  out_of_scope:               # directories beside the workspace that are not the workspace
    - history                 # another tool's store, a sync cache, a vendored checkout
    - .obsidian
```

Every axis is optional; an absent key keeps its default. Defaults:
`content_format: markdown`, `metadata.format: yaml`, `metadata.embed: delimited`,
`references: { notation: markdown, path_style: root, target: path, label: false }`,
`id_storage: both`, `workspace_id: ""`, `genesis:` unset, `root:` unset,
`identity: lazy`, `fixity: on`, `record_deletions: true`, `about: structure`,
`confirmations: stamp`, `actors: free`,
`out_of_scope: []`. Absent `spanning`/`relations` **definitions** ⇒ the built-in
diaryx vocabulary, so a minimal vault declares none; absent `fields` ⇒ no field
is described (every such field is ordinary carried content); absent `views` ⇒ the
workspace declares no lenses. Absent `exports` ⇒ nothing is declared exportable —
the default state of a workspace and of every document in it. The `spanning`,
relation-definition (`cardinality`/`inverse`/`means`), `fields`, `views` and
`exports` axes are the *self-description* layer — see [Spec](/docs/spec.md).

### Relation definitions overlay the built-in vocabulary

The diaryx vocabulary (`contents`/`part_of`, `links`/`link_of`,
`replaces`/`replaced_by`, `derived_from`/`derivations`, spanning `contents` —
[Spec](/docs/spec.md) §2) is always the **base**, and each `relations` entry
overlays it (`WorkspaceConfig::relation_set`):

| entry | means |
| ----- | ----- |
| a name the base does not have | a new relation, added |
| a name the base has | that relation, redefined **per field** — what the entry leaves unsaid, the base's own definition answers, so a `means:`-only gloss keeps the inverse and cardinality |
| `<name>: off` | the name is *not* a relation here; a document key by that name is an ordinary user field prov carries and never follows |

So extending the vocabulary by one pair costs one pair. Wholesale replacement is
still expressible — declare your relations and turn off the base ones you do not
use — but it has to be written down, which is the point: a block that declares
only `front_page`/`fronts` used to silently take `contents` with it, spine and
all. An entry is therefore either a **mapping of settings** or the scalar `off`;
anything else is a `check` finding naming both shapes. `check` also flags a
`spanning` relation the same surface turns off.

Retracting one of the five **pointer** names
(`config`/`registry`/`deletions`/`history`/`about`) takes it out of the
vocabulary but not out of the machinery: prov still reads the root's key by that
name to find what it points at (see "Pointers stay top-level" above).

### Views

The spanning relation is one way through the workspace: a single-parent tree,
every document in exactly one place. A **view** is a second way through the same
documents — "the entries under `Daily`, by month", "everything, by tag" — and
the same document may appear under several groups, which is precisely what the
spine cannot do.

A view is two expressions in [CEL](https://github.com/google/cel-spec), the
Common Expression Language, and a name a person can call it by:

| key      | means                                                                 |
| -------- | --------------------------------------------------------------------- |
| `where`  | a condition a document must meet. Absent = every document the workspace reaches |
| `key`    | the group, or list of groups, each document goes under. **Required** — an entry without one is not a view |
| `label`  | what a person calls it (absent = the name, humanized)                 |
| `icon`   | a glyph hint, uninterpreted                                           |

```yaml
views:
  open-tasks:
    label: Open tasks
    where: >-
      under('Tasks') && present(status) && !(status in ['done', 'dropped'])
    key: status
  activity:
    key: "[day(created), day(updated)]"   # a document under every day it has a date for
```

CEL is not prov's. Its grammar, its operators (`==`, `in`, `&&`, `!`, `?:`),
and its standard functions (`size`, `startsWith`, `matches`, `exists`, `map`,
…) are fixed by its specification, which is the reason to embed one: a view is
a way of *reading*, reading has no invariant, and a language that is
deliberately not Turing complete and has no side effects can be as expressive
as a view wants without being able to write or run forever. The reasoning is
the [views-as-queries proposal](/docs/proposals/views-as-queries/proposal-views-as-queries-v1.md).

#### What an expression sees

- **Each field is a variable of its own name** — `status`, `created`,
  `people`. A field the document does not carry is `null`, so `status ==
  'done'` is false on a document without a status rather than an error. The
  one place prov departs from plain CEL: `null` on the right of `in`, or as the
  list `exists`/`all`/`map`/`filter` walk, is an empty list, so `'Ada' in
  people` is false on a document that lists nobody.
- **`doc` is the document itself**: `doc.path`, `doc.title`, `doc.id` (its own
  `id` field, else the registry's, else `null`), `doc.meta` — the whole block,
  for a key that is not an identifier (`doc.meta['date of birth']`) or a field
  named `doc` — and **`doc.ancestors`**, every document above this one in the
  spine from the root down, each `{path, title, id}`.

#### prov's functions

| function | gives |
| --- | --- |
| `present(x)` | whether `x` carries a value — not `null`, not blank, not a list of blanks |
| `first(a, b, …)` | the first argument that is `present` — a fallback chain |
| `field('a.b')` | every value at a field path, as text: `written.on` inside a mapping, `confirmed[].by` inside every item of a list; `[]` when the path reaches nothing |
| `under('Tasks')` | whether the document is below that one in the spine — see "Scope is ancestry" |
| `year(x)`, `month(x)`, `day(x)` | the keys a date cuts to, read as EDTF — see "Grains" |
| `initial(x)`, `initial(x, n)` | the first letter, or `n` letters, upper-cased — the A–Z index |

A grain takes a value or a list, and gives a *list*, because a value can cut
to no key (`banana`, `XXXX`), one, or several (`1918/1922` at year grain).
`first` does not fall through a value that is present but uncuttable: falling
through to `created` because `date_of_document` held something unparseable
would file the document under a date it does not claim, and leaving it
ungrouped shows the bad value instead.

New functions are added by one rule — a concrete lens that cannot otherwise be
said, not a shape that seems likely to be wanted. CEL's arithmetic and string
functions are CEL's, and the embedding carries the `cel` crate's differences
from the specification with it: `size()` of a string counts bytes, and the
optional string extensions (`lowerAscii`, `split`) are not there.

#### Keys, failures, and the ungrouped bucket

A `key:` gives a value or a list; lists are flattened, each value is a group
as its text, and a document is under each distinct group once. A document
that gets no key at all — no date, or one no grain can cut — is in the
**ungrouped** bucket, reported rather than dropped, because a view whose
entries have all quietly stopped grouping is indistinguishable from an empty
archive and the difference is the whole diagnosis.

A value of a field declared `type: ref` groups by **the document it links
to**, not by how the link is spelled: `[Ruth Harris](id:abc1234)` and
`[Grandma](id:abc1234)` are one group, keyed by that document's path and
labelled by its title, so relabelling a link does not move anything between
groups. A reference that resolves to nothing groups by its text, as any other
value does. Groups are ordered by label, which for every other key is the key.

An expression can fail on one document and not another — `size(nickname)` on
a document without one, a comparison between text and a number. Such a
document is neither shown nor silently dropped: `prov views <name>` lists it
on stderr with the reason, and `--json` under `failures`. An expression that
does not parse, or calls a function neither CEL nor prov defines, is a
`check` finding at `views.<name>.where` or `.key`, and the view is not read
at all — a broken condition must not become a view of everything.

#### There is no `date` grouping

`month(first(date_of_document, created))` is a declaration *this workspace*
makes, not a convention prov blesses. A workspace that files by `taken_on`
writes that instead. A grain applies to a *value*, never to a declared type,
so it needs no `fields.<name>.type` declaration to work.

#### Scope is ancestry

A view does not walk the spine. prov walks it once, for the census, and records
each document's ancestors on its row; a view scoped to a subtree says so as a
condition — `under('Daily')`. The anchor is written as a scoped `fields`
declaration's `under:` is, and read the same way: a bare name or `[[Daily]]`
by title or file stem, `[Daily](id:abc1234)` by id, which survives a retitle
too, and `/Daily/index.md` by path. So `fields.status` under `[[Tasks]]` and a
view `under('Tasks')` mean one region of the tree, however the index is
named. It survives a move and a rename for the reason a traversal did: the
ancestry is recomputed from the spine on every run, never matched against a
path prefix, so `path starts-with "Daily/"` and the index *titled* `2026`
under `Trips/` are not what it matches. The anchor is not its own ancestor, so
an index is what its records hang under, not one of them.

A condition on its own cannot tell an anchor that names nothing from an index
with no members — `under('Taks')` is simply false everywhere — so `check`
resolves every literal anchor a view names against the workspace, as it does a
`fields` scope, and reports one that names no document or several as
`view_scope_unresolved`. An anchor computed per document (`under(doc.title)`)
has nothing to resolve until it runs, and is not checked. `doc.ancestors`
stays for what `under` does not say: `doc.ancestors.size() == 1`, or an
ancestor matched by something other than its identity.

#### Grains

A grain is a **coarsening** — any many-to-one function from a value to a group
key. The calendar is one family of them, not the subject:

| function / `nest:`  | groups                                       |
| ------------------- | -------------------------------------------- |
| `year` | `2026-07-24` → `2026`, `1913~` → `1913`, `192X` → `192X` |
| `month` | `2026-07-24` → `2026-07`, `1943-05` → `1943-05` |
| `day` | `2026-07-24` → `2026-07-24`                  |
| `initial` | `Lovelace` → `L` — the A–Z index             |
| `initial(x, 2)` / `{ initial: 2 }` | `Lovelace` → `LO`                            |

The date grains **validate** rather than slicing, so `banana` at year grain is
not the group `bana` and `20264` is not the year `2026`. What they validate
*as* is [EDTF](https://www.loc.gov/standards/datetime/) — the Library of
Congress Extended Date/Time Format, ISO 8601-2 — because an archive is mostly
approximate dates and a calendar date is only the exact case of one:

| written     | means                            | at `year`          | at `month`  |
| ----------- | -------------------------------- | ------------------ | ----------- |
| `1943-05`   | May 1943, day unknown            | `1943`             | `1943-05`   |
| `1913~`     | approximately 1913 (`?` uncertain, `%` both) | `1913` | ungrouped |
| `192X`      | some year in the 1920s           | `192X`             | ungrouped   |
| `1918/1922` | sometime between 1918 and 1922   | `1918` … `1922`, all five | ungrouped |
| `../1920`   | before 1920 (`1918/..` after)    | `1920`             | ungrouped   |
| `XXXX`      | a date, not known                | ungrouped          | ungrouped   |

The qualifier is dropped, so `1913~` files beside the `1913` that was sure of
itself — a group key names a shelf, not a value. An unspecified digit is kept
in the year, because a decade is a shelf people use (`192X` sorts between
`1929` and `1930`), and refused below it: "some month of 1943" is not a
month, so `1943-XX` is `1943` at year grain and nothing at month. A year with
no digit at all — `XXXX`, EDTF's spelling of *undated* — is the ungrouped
bucket said on purpose, which is where a frontend's "Undated" label already
points. An interval is under every group both of its ends reach, the way a
letter about two people is under both; an open or unknown end contributes
nothing, so `../1920` is under `1920` alone — under-claiming, never wrong.
An RFC 3339 instant cuts like the plain date it starts with, as it always
has. `initial` cuts by *character* (`Ålesund` → `Å`) and upper-cases,
deliberately: an index that files `ada` apart from `Ada` is not an index.

New grains are added by one rule — a concrete lens that cannot otherwise be
said, not a shape that seems likely to be wanted. A numeric `bucket` is the
obvious candidate and is deliberately absent: nobody has asked for one, and its
keys would sort lexically as `0, 10, 100, 20`, needing group ordering to become
grain-aware, which is the deferred `sort:` axis under another name.

Grains are their own crate, `prov-grain`, because they belong to neither side:
a view reads by a grain's cuts, and [filing](#filing) writes by its chain. Both
depend on it, and neither on the other.

The reading engine is the `prov-views` crate, whose dependencies are prov's
read core, `prov-grain` and the CEL interpreter, none of which can write to
the workspace it reads. Running a view is two steps, and they are worth
knowing apart: **select** answers *which documents does this view cover* and
returns a flat, deduplicated set; **group** projects that into groups and is a
pure function. So the count of documents a view covers and the count of rows it
draws are different numbers — a document under two groups is one document in
two places — and `prov views <name>` prints both. `prov views <name> --json`
gives the same answer machine-readable, each row carrying that document's
whole metadata block and its ancestors, so a consumer that replaces its own
per-file loop with a view still has what the loop was reading; `prov views
--json` lists the declarations the same way. `prov query '<where>' [--key
'<key>']` runs an expression without declaring a view — the same census, the
same evaluation — which is where a view is tried before it is written down.

What every view narrows is the **census**: every document the spine reaches
from the root, the root included, each once, in path order. `prov docs` prints
it a line each, and `prov docs --json` prints it as the same rows a view
returns — path, title, `id` (read from the document's `id` field where it
carries one and from the registry otherwise, so the column reads the same
under every `id_storage`), `ancestors`, and the metadata. It declares nothing,
so there is nothing to misspell: a consumer that wants to build its own table
over the workspace — a query engine, a shell pipeline — starts here. Reached,
not present: a file in a directory nothing links into is not a row, for the
same reason `check` does not report it (`prov_views::documents`).

#### The retired form

Views were once written with `group:` (a field or a first-non-empty chain),
`by:` (a grain), `under:` (an anchor whose subtree the view walked), `nest:`,
and a `where:` mapping of `has`/`equals`/`not`/`any-of`/`all-of`. None of
those is read now. A view still written that way is a `check` finding that
prints its replacement — `group: [a, b]` with `by: month` as `key:
month(first(a, b))`, `under:` as the ancestry condition, `has: x` as
`present(x)`, `equals: { x: v }` as `'v' in field('x')`, and `nest:` as a
`filing:` entry of the same name.

`check --fix` writes the replacement for you, and `--fix mechanical` does it
unattended: the old form said exactly what the new one says, so nothing is
being chosen. The view and its filing entry land in one write, so neither
exists without the other. The one case left to you is a `filing:` entry of
that name that already says something else — which of the two files new
records is yours to decide, and the finding says so. A program that wrote such
views on a user's behalf can upgrade just its own: each retired view is its
own finding with its own repair (`Workspace::remedies`, `RemedyKind::Upgrade`),
and applying one applies nothing else.

### Filing

A view reads. A **filing** entry says where a frontend should *write* a new
record — under which index, by which field, how deep:

| key      | means                                                                 |
| -------- | --------------------------------------------------------------------- |
| `under`  | the index new records go below, as a link — by path, `id:` or title. Absent = the root |
| `field`  | the field path, or a list tried in order, the record is filed by. Required with `nest` |
| `nest`   | a grain — how deep, through indexes titled by the cut value — or `ref`, to file under the document the value links to. Absent = directly under `under` |
| `label`  | what a person calls it                                                |
| `kind`   | the kinds of record it files: `page`, `image`, `audio`, `video`, `file`, `manifest`, or `attachment` for the four payload kinds. Absent = any |

Filing used to be a view's `nest:` key, and was split out because it is the
half that writes. The spine is single-parent, so filing needs guarantees before
anything runs that a way of reading never does, and a view carrying `nest:` had
to live inside them. MoReq2010 §1.4.5 draws the same line between
*classification* — how records become groups, a view — and *aggregation*, the
index a record actually hangs under; keeping them apart is what keeps a change
to how something reads from moving where tomorrow's entry lands.

prov describes where a record files (`FilingSpec::route`, in the
`prov-filing` crate, which shares grains with views through `prov-grain` and
does not depend on the view engine): index *titles* below `under`, or the link
the record carries. And it files one: `prov new "Tuesday" --filing daily`
(`Workspace::file` in the library) resolves the anchor, walks the indexes the
route names, makes the ones that are missing as one change set, and puts the
document in the last. The value it is filed by is the one the new document
opens with — its creation stamp, or a `--set date_of_document=…`.

Each index is found by the period it carries before the title it shows: the
July index is the child of the year whose own value, read through the entry's
`field:` chain, is `2026-07` — whether its author titled it `July`, `2026-07
index` or `07`. A child that carries no value is matched by title, the way a
route is. An index `--filing` makes is titled with the period and, for a
calendar grain, carries it in the head of the field chain
(`date_of_document: 2026-07`, a month-precision date), so the next filing finds
it the same way; it gets a directory of its own named for the period's last
part (`2026/07/index.md`, beside `06/`). A value that reaches only part of the
grain files as deep as it reaches — `2026-08` under a `day` entry lands in
August — and a document with no value for the field files directly under the
anchor, in no period: an undated scan imported today is not from today.
`--dry-run` shows what would be made.

#### What `nest:` can and cannot file

Filing builds a hierarchy of index documents, so a grain may nest only if its
coarser steps are *determined* by its finer ones — `2026-07-24` → `2026-07` →
`2026`, `Ada` → `Ad` → `A`. Every grain above chains; an arbitrary sequence of
coarsenings would not. An interval that spans several groups at the nesting
grain has several homes, and is not filed, for the reason a document with two
people is not.

The second limit is prov's spine, not taste: the field must be
**single-valued** for the document being filed. A document listing two people
has two homes and nothing can choose between them — so `nest:` over a field
declared `type: seq` is a `check` finding, and a document that turns out
multi-valued at filing time simply has no route. A view grouping by such a
field stays perfectly good; one document under several groups is the whole
point of a view.

#### Which records an entry files

An application with one way to add anything — a page, a photograph, a voice
memo, a scanned deed — needs to know where *each* goes, and an entry that
names its kinds says:

```yaml
filing:
  daily:
    under: '[Daily](id:abc1234)'
    field: [date_of_document, created]
    nest: month
    kind: page
  photos:
    under: '[Photos](/Photos/index.md)'
    field: [date_of_document, created]
    nest: year
    kind: [image, video]
```

A record's kind is prov's reading of it (`RecordKind`, `Document::record_kind`):
a manifest node is `manifest`; an attachment is the kind of its payload, from
the payload's extension — `image`, `audio`, `video`, after the IANA top-level
media types, or `file` for anything else; every other document is a `page`.
`attachment` in a `kind:` list names the four payload kinds at once.

Only an entry that **names** a kind answers for it (`filing_for_kind`; `prov new
--filed`, `prov attach --filed`). An entry naming no kinds still files whatever
it is asked to file, and which of several such entries an application uses —
Daily, an inbox — stays the application's choice, as before. Two entries naming
the same kind is a `check` finding, `FilingKindClaimedTwice`, and neither
answers: a photograph cannot be filed two ways, and prov will not pick. A word
that is not a kind drops the entry, as an unknown `nest:` does.

#### `nest: ref` — filing by reference

A grain computes the shelf from the value, and prov has to know what a year
is to do it. The other way to say where a record files is for the record to
**link to the shelf**, and for the shelf's own place in the spine to be the
rest of the chain:

```yaml
fields:
  written.on:
    type: ref
filing:
  journal:
    under: '[Calendar](/Calendar/index.md)'
    field: written.on
    nest: ref
```

A record carrying `written: { on: /Calendar/2026/09/17.md, at: "09:12" }`
files under that day node, full stop. The day already sits under its month,
which sits under its year, because that is the calendar index's own
`contents` chain — so the chain condition above is met by construction, and
prov does not know the target is a day. The same declaration files a note
under a person, a place or a project, and prov cannot tell which.

What it costs is that the shelf must exist. prov creates nothing here: a link
to a document that is not there is the ordinary broken-link finding, and
making the day node is the frontend's. The value grains stay for a workspace
that would rather not keep a node per day, or whose dates are `1913~` and
`1918/1922` and have no node to point at.

The field must be declared `type: ref`. That is what makes the value a link
— resolved, checked, rewritten when the shelf moves — rather than a string
that used to be a path; a `nest: ref` over a field the same surface does not
declare a `ref` is a `check` finding, because the filing would work on the
day it was written and break, silently, the day the shelf was moved.

`prov docs --json --body` adds the prose: each row carries a `body` string,
so the whole workspace comes out as text in one record set — for a search
table, a corpus dump, a pipeline that wants the words and not only the
fields. The body is prov's reading of it, which is the reason to ask prov
rather than `cat` the `path` column: a combined document's prose is what is
left once the metadata block is taken out, and a separated node's prose is
in the file its `content` names, so `cat` gets the wrong text both ways.
`null` where a document has no prose to carry — an attachment sidecar, a
whole-file metadata node standing for itself or for a manifest — and `""`
for a body with nothing in it, so the two stay distinguishable. Off by
default because bodies are most of a workspace's bytes, and the plain
`--json` row is unchanged: the key is absent, not `null`, when not asked
for. Still only carried, never read: what prov does with a body is hand it
over, as `prov body` does one document at a time (DESIGN §2, tier 3).

`prov search WORD...` is the view over that corpus: every document that
mentions each of the words — as substrings, case and diacritics folded, so
`alesund` finds `Ålesund` — in its title, its metadata values or its body,
best first, each with the passage around the match (`--json` for the hits
as records, `--limit` for how many). A key is left unread when it carries
structure rather than what the author wrote: every relation the workspace
declares, since a link's label is another document's title, and the
identifier keys `id`, `content`, `manifest` and `root`
(`prov_views::search`, and `prov_views::corpus` for a consumer with
structural keys of its own to add). The index is built from the files for
the one answer and kept nowhere — derived and disposable, never written
into the workspace, so the body stays carried and the census stays the
only thing read.

### Exports

Everything above reads open by default — a view with no `where:` covers the
whole workspace. An **export** is the boundary where that flips: a named,
closed-by-default set of documents that may *leave* the workspace.

```yaml
exports:
  letters:
    label: Letters home
    gate: { field: audience, value: family }
    hold: draft
    view: daily
```

The `gate` is the membership test, and it is exactly one field and one value: a
document is in the export only if its **own** metadata declares that value under
that field (`audience: family`, or a list containing it). A document that
declares nothing leaves in nothing. Matching is exact after trimming —
`audience: Family` does not pass a `value: family` gate; casing drift is what a
closed vocabulary on the gate field (`fields.audience.vocabulary`) is for, so
the typo is reported rather than forgiven. Deliberately not an any-of list and
not a condition: what makes an export auditable is that *"does this document
leave?"* is answerable by reading one field on that one document.

`hold` optionally names a second field, for the fact a gate cannot carry: that
a document is *not ready*. Who a document is for and whether it is finished are
different facts with different lifetimes — an audience is a durable property of
the text, a draft is a state that ends — and taking the audience away to say
"not yet" loses the first to say the second. So a document the gate admits that
declares the literal `true` under the hold field (`draft: true`) is **held**: not
in the export, and reported as held rather than withheld, because the author did
say it may leave. `draft: false`, an absent field, and any other value do not
hold. What the field is called is the workspace's choice; prov fixes only the
shape, and the audit property survives it — two named fields on the one
document, no list in the config to consult. A `hold` that does not name a field
makes the entry unreadable, for the same reason a missing gate does.

A hold field with a vocabulary holds by *term* as well. A term that declares
`holds: true` — a `terms:` row in a flat store, or a key on the term's own
document in a reified one — keeps back every document carrying it, as the
literal `true` does:

```yaml
# vocab/proposal-statuses.yaml
terms:
  draft:    { means: "still being argued", holds: true }
  accepted: { means: "argued and agreed" }
```

With `hold: status`, a `status: draft` proposal waits and `status: accepted`
leaves. Which states are unfinished is said once, on the terms, rather than in
every export as a list, and the audit is still read off the document — its
`status`, and the vocabulary that already defines what that status means. The
vocabulary is the one governing *that* document (see "Scoping a
declaration"), so `draft` can hold under `Proposals` and mean nothing of the
kind under `Tasks`. A retired term still holds: retiring says a value is no
longer for new content, not that what carries it is ready.

`view` optionally arranges what leaves, and it obeys a one-way valve: **an
export's set is a subset of what its gate admits, whatever the view says**. A
view may narrow the set; it can never put back a document the gate held out.
An export naming an unknown view — never declared, or one whose expressions
do not parse and so is not read — is an error, never a fall-back to the gate's
whole set: the view was written down as a bound on what leaves, and a bound
nobody can apply must fail closed. For the same reason a document the view's
condition cannot be evaluated on does not leave. (`diagnose` also flags the
unknown-view typo at author time, when `views:` and `exports:` share a
surface.)

There is no `index:` (front page) key: which page greets a reader is a
rendering concern, declared by the publish layer that owns a render. prov only
ever *plans* an export — `prov exports` lists them, `prov exports <name>`
previews what leaves, what the gate held back, what the documents themselves
are holding, and what the view scoped out, and moves nothing. Publishing, copy-out, and OCFL export are downstream
consumers of the same plan. The format and planner are the `prov-exports`
crate, which depends only on the read core and `prov-views` and so cannot
write to — or leak from — the workspace it judges.

### `out_of_scope` — what is beside the workspace but not in it

prov decides what belongs to a workspace by **reachability**: the bounded walk
from the root document (spec §8). That answers "does the graph link this?", and
for a folder nobody meant as content the answer is *no* — which is exactly the
answer a note someone forgot to link gets. The two are indistinguishable from
the inside, so a walk with only reachability to go on must descend into both or
into neither.

`out_of_scope` is how a workspace says which is which. It lists directories
that are on disk beside the root and are **not** this workspace's content:

```yaml
out_of_scope:
  - history        # a version-control store another tool keeps here
  - .obsidian      # an editor's own folder
  - vendor/upstream
```

Each entry is a directory path relative to the workspace root, `/`-separated,
with no leading slash and no `.` or `..` segment. A malformed entry is reported
by `check` and dropped rather than half-honored — the same posture
`workspace_id` has, for the same reason: a path naming somewhere outside the
workspace cannot bound a walk over it.

What the declaration buys, everywhere at once:

- `check` stops reporting the directory's interior — the containment sweep does
  not read a claim written inside it;
- `attach --all --recursive` no longer mints a sidecar beside every file in it;
- the title index does not resolve a nominal `[[Some Note]]` to a copy living
  inside it, and a walk that falls back to a full title scan does not read the
  directory's files. That holds for walks made through the bare graph too — a
  view, `search`, an export plan — because the workspace hands the declaration
  to its graph when it is built (`ReadSettings::parking`);
- `prov ignore` names the directory as **one rule**, labelled *declared out of
  scope* rather than *unreached* — a statement, not an oversight.

Nothing is hidden by it. `prov ignore` still names each declared directory,
which is what the list is for, and the files stay on disk where they were. What
changes is that prov stops reporting another tool's interior as this
workspace's problem.

Set it from the command line with a comma-separated list — the one
sequence-valued axis `prov config` will write:

```
prov config out_of_scope history,.obsidian
```

### Field types

A `fields.<name>` entry declares three independent things, and needs at least
one of them to be worth writing: a **type** (`type:` — what the value is), a
**controlled vocabulary** (`vocabulary:` + `values:` — which values are legal),
and a **starting value** (`default:` — what a new document opens with). None
implies another. `created` is a date nothing controls; an `audience` vocabulary
needs no declared type; a `status` that starts as `open` need declare nothing
else. An entry with none of the three is ignored.

`default:` is written by `prov new` into the document it makes, after the title
and the links prov authors itself, and is never read back: it is where a
document starts, not a rule about the field, so a document that changes or
unsets it is not a finding. A `--set name=value` on `new` overrides it for one
document. The value is carried as written — `default: 0` is an int, `default:
[]` an empty list — and whether it is a term of a closed vocabulary is
`check`'s question, asked of the document that ends up holding it. It lives in
the workspace rather than in a caller's flags so that a stencil can state it
and `about.md` can say it.

`<name>` is a top-level key, a dotted path into a mapping — `generated.how`
declares the act a `generated` mapping records beside its `by` and `at` (see
[Provenance](provenance.md) §1) — or a path through every item of a list:
`confirmed[].by` is the actor of each confirmation, `sources[].resource` the
target of each source. A dot is always a separator, as it is for `prov get`,
and every reader of the declaration follows it: `check` judges every value
the path reaches, a repair respells or retargets the one its finding named —
by the concrete path, `sources[2].resource`, which is how a finding spells a
site inside a list — and a `default:` is written at a key path, inside the
mapping, creating it if the document has none. A list path has no list to
fill in a new document, so a `default:` on one is carried and never written.

### Stamps

`stamp:` says prov writes the current time into the field, and when:

| `stamp:` | written |
| --- | --- |
| `create` | once, when prov makes the document (`prov new`) |
| `edit` | whenever prov changes the document's content — `edit`, `set`, `unset`, `stamp` — and the instant a confirmation is judged stale against |

```yaml
fields:
  created: { stamp: create }
  updated: { stamp: edit }
```

A stamp alone is a complete declaration, and it needs no `type:`: what prov
writes is an instant, RFC 3339 in UTC (`2026-09-28T14:03:00Z`), because prov
reads it back. The *name* is the workspace's — `updated`, `modified`,
`lastmod`.

A stamp declared `type: date` is written as the calendar date instead —
`created: 2026-09-28`, the day on the clock of whoever made the document, which
a journal shows and a person may later backdate:

```yaml
fields:
  created: { type: date, stamp: create }
```

The day depends on whose wall the moment fell on, which an instant alone does
not say, so the program doing the writing supplies its clock's UTC offset with
the time (`prov_config::Now`, and `WorkspaceConfig::stamp_value` for the value
to write); the CLI reads the offset the way `date` does. Every other type
keeps the instant. A date is checked as any `type: date` value is, and an edit
stamp that is a date orders confirmations by day (see
[Provenance](provenance.md) §3).

A document has one creation instant and one last-edit instant, so each stamp
belongs to one field: a second field claiming the same stamp is a
`repeated_stamp` finding, and the first in name order is the one written.
When a document changes is not a fact about where it sits, so a stamp is read
from the field's unscoped declaration only; one under a scope is a
`scoped_stamp` finding and is not read.

The stamps used to be top-level keys naming the field — `updated: modified`,
`created: created` — beside a separate `fields` declaration of the same
field's type. Those keys are no longer read: a workspace that still writes
them stamps nothing until it declares `stamp:` on the field. `check` reports
each as a `stamp_retired` finding, and `check --fix` moves the stamp onto the
field's declaration — beside the `type:` it already has, on the unscoped
declaration of a field declared per scope (adding one if there is none) — and
drops the old key. An empty value, the old spelling of "stamp nothing", is
simply dropped. Where another field already carries that stamp, nothing is
moved: a document has one such instant, and which field holds it is yours to
say.

### Scoping a declaration

A declaration governs the whole workspace unless it says `under:` — a link to
an index, resolved by path, by `id:`, or by title (as a filing entry's
`under:` is), and then it governs the files in that index's spanning subtree and no
others. A field may be declared several times, as a list, each entry scoped:
`status` is one closed set of terms under `Tasks` and another under
`Proposals`, opening as `open` in the one and `draft` in the other, and a
file under neither index has no `status` declaration at all — the `Tasks`
index does not open as an open task, because an index is not in its own
scope, for the reason a view's anchor is not one of its records.

Every reader of `fields` asks *where* before *what*: `check` holds a value to
the vocabulary of the declaration that governs its document, `new` writes the
starting value of the one that will govern the child, a term repair widens
the right list, and `about.md` says where each rule holds. Where scopes nest,
the deeper wins; an unscoped declaration in the same list is the fallback. A
scope whose anchor names nothing governs nothing, and `check` reports it as
it reports a view anchored on nothing.

A `vocabulary:` pointer names one of two shapes of store, and the store says
which. A **flat** vocabulary is machinery: a whole-file document carrying the
`vocabulary:` marker and a `terms:` mapping. A **reified** one is any other
document — an ordinary content index node whose spanning children are the
terms, real documents with a `part_of`, a prose body and backlinks (Spec §3,
§4). Membership is checked identically either way; what changes is where a
term's payload lives, and that `check` neither demands a config carrier of a
reified store nor offers to widen one mechanically.

A declaration used to say the shape as well, with `reify: true`. It is no
longer read: the store cannot be wrong about its own shape, and a declaration
that disagreed with it loaded no terms, which on a closed field made every
value in the workspace unknown. A `reify:` key still written is ignored.

The type vocabulary is [`fig-schema`](https://crates.io/crates/fig-schema)'s, not
one prov invents, so prov, a metadata editor, and a view engine all name types
identically instead of agreeing by convention:

| `type:`          | means                                      |
| ---------------- | ------------------------------------------ |
| `str`            | text                                       |
| `bool`           | `true` / `false`                           |
| `int` / `float`  | a number                                   |
| `date`           | a date as an archive writes one: `2026-07-24`, or EDTF — `1943-05`, `1913~`, `192X`, `1918/1922`, `XXXX` for not known. **Checked** — see below |
| `datetime`       | an instant with offset, `2026-07-24T07:32:00Z` |
| `local-datetime` | a date and time with no offset             |
| `time`           | a time of day, `07:32:00`                  |
| `ref`            | a link to another document — **read**, see below |
| `map` / `seq`    | a nested mapping or list                   |

prov carries every type but two without interpreting it — nothing in `check`
fails because a `str` or an `int` does not match its declared type. They are
there so a frontend can parse and render the field faithfully (a `date` gets a
date picker, not a text box).

**`date` is checked.** A `type: date` value must be a calendar date, an RFC
3339 instant (read as the date it starts with), or
[EDTF](https://www.loc.gov/standards/datetime/) — the reduced precision,
qualification, intervals and `XXXX` the grains table above reads. Anything
else is a `MalformedDate` finding naming the field, because a date view files
a value it cannot read as *undated*, silently, and `May 1943` and a torn-off
day must not look the same. The repair, where the prose has one reading, is
its EDTF spelling — `May 1943` → `1943-05`, `c. 1913` → `1913~`, `before
1920` → `../1920`, `unknown` → `XXXX`; where it has two (`5/12/1943`) both
are offered and neither assumed. `datetime` is not checked: it is what a
machine stamps, and a stamp with fractional seconds is not EDTF.

**`ref` is the other exception.** A field declared `type: ref` is a link site
(Spec §3): each value it reaches is resolved as a relation entry is —
censused, checked, rewritten by `mv`, relabelled by `retitle`, reported by
`rm`, restyled by `convert --links` — and its target is reached, so it is not
an orphan. It is one-way: no inverse, nothing written on the target, never
spanning. Declare a relation for a list of bare links and a `ref` for a link
that sits beside other facts about itself — `sources[].resource` with a
`title` and a `page` in the same entry. Whether a key holds links holds
workspace-wide, as a relation's name does; a `ref` that also says `under:`
is reported (`ScopedReference`) and read as a link everywhere regardless.

The date and time types map onto the underlying format's native scalars where it
has them — a TOML `created = 2026-07-24` stays a date rather than becoming a
quoted string — and to plain unquoted text where it does not, which is the YAML
frontmatter case: `created: 2026-07-24` is written correctly but reads back as a
string. That asymmetry is harmless, because a field's declaration is found by
*name*, never by inspecting the value's type.

### The two reference axes, orthogonalized

Previously `link_format` fused *notation* (bracketed vs bare) with *path
resolution*, and `reference_wrapper` added `wikilink` as a separate key — so
`link_format: plain_canonical` produced a **bare** link even though the wrapper
said "markdown." The reshaped `references` block separates the two
truly-orthogonal axes:

| `notation` | `path_style` | rendered path reference |
|---|---|---|
| `markdown` | `root` | `[Title](/path/x.md)` |
| `markdown` | `relative` | `[Title](../x.md)` |
| `bare` | `root` | `/path/x.md` |
| `bare` | `relative` | `../x.md` |
| `wikilink` | *(any)* | `[[path/x.md]]` — `path_style` shapes the inner path text |

### `canonical` is retired

`path_style` once had a third value, `canonical`, which rendered a bare
workspace-relative path (`path/x.md`). It did not survive contact with the
resolver: a bare target is resolved **relative to the document it was found
in**, so a canonical link named what it meant only from a document at the
workspace root and silently pointed somewhere else from anywhere below. The
ambiguity of a bare path is settled by committing to one meaning rather than by
tagging it, so the spelling that claimed the other meaning is gone; a
workspace-relative reference is `root`, written with the leading slash that says
so.

A workspace still configured with `canonical` keeps loading — `apply` falls back
to `root`, which renders the same path with that slash and therefore resolves
correctly from anywhere — and `check` reports the value
(`ConfigIssueKind::InvalidValue`). To restyle the documents themselves:
`prov convert <root> link_format markdown_root -r`.

`target: id` renders `[[id:…]]` / `id:…` (registers the target); `target: alias`
renders `[[Title]]` (nominal, `notation` forced to `wikilink`). `path_style`
applies to path targets only.

## Value changes from the old vocabulary

| Old (flat, top-level) | New | Note |
|---|---|---|
| `link_format: markdown_root` | `references: { notation: markdown, path_style: root }` | split into two axes |
| `reference_wrapper: markdown\|wikilink` | folded into `references.notation` | + a `bare` option |
| `reference_target` | `references.target` | unchanged values |
| `reference_label` | `references.label` | unchanged |
| `id_links: bool` | **dropped** → `references.target: id` | was "superseded by reference_target" |
| `relations.<n>.style.{wrapper,target,label}` | `relations.<n>.{notation,path_style,target,label}` | drop the `style` nesting |
| `embed_format` | `metadata.format` | grouped |
| `embed_type` | `metadata.embed` | grouped |
| `id_storage: frontmatter` (meant *both*) | `id_storage: both` | names the actual homes |
| `id_storage: frontmatter_only` | `id_storage: frontmatter` | frontmatter is the sole home |
| `identity: off` | `identity: none` | clearer — `off` still accepted as a synonym |
| `fixity: payloads` | `fixity: attachments` | says what it covers |
| `fixity: full` | `fixity: all` | attachments + bodies |
| `fixity: attachments` | `fixity: on` | coverage is no longer a scale — a `content_hash` is written wherever it covers a file of its own (an attachment's payload, a separated body), which is where `sha256sum` can reproduce it. Old spelling still read |
| `fixity: all` | **dropped** → `fixity: on` | a combined document's body hash covered a parsed substring, not a file, so it was checkable only by prov. **Not** read as a synonym: it asked for the coverage that went away, so it lands as an invalid value (default kept, `check` reports it) rather than being quietly narrowed. Hashes already on record are still verified — see the `legacy_body_hash` finding |
| `updated_field: modified` | `updated: modified` | reframed as "this field is machine-maintained" |
| `updated: modified` | `fields.modified.stamp: edit` | the stamp is a property of the field it writes. The old key is **not read**; `check` reports it and `check --fix` moves it |
| `created: made` | `fields.made.stamp: create` | likewise, and likewise not read |
| — | `spec: 1` | new version marker |
| `config`/`registry`/`deletions`/`history` pointers | unchanged, top-level | structure, not policy |
| `recycle_bin: <path>` pointer | `deletions: <path>` | there is no bin; the log records what a delete destroyed. Old spelling still read |
| `recycle_bin: <bool>` policy | `record_deletions: <bool>` | likewise. Old spelling still read |
| — | `about: structure` | new axis — the generated `about.md`, **on** by default |

## Linting (`check`)

`config::diagnose` runs over both surfaces — the root's `prov:` block and the
config document — reporting a `Finding::ConfigIssue` per key prov would
silently ignore:

- **Invalid value** on a recognized axis (e.g. `fixity: alll`, or `fixity: all`
  now that the tier is gone) — keeps the
  default; the finding lists the accepted spellings.
- **Unknown key** that is a near-miss of a real axis (e.g. `notaton`) — a likely
  typo, reported with the suggestion. A key resembling *no* axis is left alone (a
  user field), except inside the closed sub-blocks (`metadata`, `references`, a
  `relations` entry, a `fields` entry), where every key is expected to be a known
  axis. A `relations` entry additionally accepts the definition keys
  `cardinality`/`inverse`/`means`, and may be the scalar `off` instead of a
  mapping altogether; any *other* scalar is an invalid value naming both shapes.
- **Spanning invariant** — a `spanning` relation whose declared `inverse` is
  itself declared `cardinality: many` cannot form a single-parent tree (DESIGN
  §3), reported as `SpanningNotSingleParent`. A `spanning` relation the same
  surface turns `off` is reported too: the workspace named a spine it does not
  have.
- **Retired forms** — a view written with `group:`/`by:`/`under:`/`nest:` or a
  `where:` mapping (`view_retired`), and a top-level `updated:`/`created:`
  (`stamp_retired`). Neither is read. Unlike the others these have one right
  repair, the same setting in the current form, and `check --fix` writes it —
  unless it would overwrite something else the config says, which the finding
  names.
- `spec`, and the config document's own `title`/`part_of`, are whitelisted.

Beyond the two config surfaces, `check` also validates the workspace's **stores**
and **controlled fields** (see [Spec](/docs/spec.md)): a `MalformedStore` finding
for a registry/deletions/*flat*-vocabulary pointer that resolves to a markdown
document rather than a whole-file config document (a reified vocabulary is
content, so the rule does not reach it); `UnknownTerm` for a closed-field value
that is not a known term; `TermNearMiss` for an open-field value that closely
resembles one; and `MalformedDate` for a `type: date` value that is neither a
calendar date nor EDTF (see "Field types" above).

`prov config <key> <value>` runs the same `diagnose` over a one-key probe and
**refuses to write** a setting `check` would flag. Dotted keys address nested
axes: `prov config references.notation wikilink`.

Legacy top-level policy keys in the root (a diaryx-style `link_format: …` sitting
outside the `prov:` block) are **silently ignored** — treated as ordinary
user fields, not read and not flagged.

Beyond `check`, any command that opens the workspace prints a one-line stderr
reminder when config would go unread — the `diagnose` issue count (with the first
key as a teaser), and a note if a surface declares a `spec` newer than
`SPEC_VERSION`. It is suppressed by `PROV_QUIET`, and skipped on `check` and
`config` (which report config in full themselves).

## Restating existing documents (`prov convert`)

Setting a config axis governs the documents prov writes *next*. `prov convert
<file> <axis> <value>` reconciles the ones already there, per file by default
(`-r` extends it to that file's spanning subtree, so `convert <root> … -r` is the
whole-workspace case). A mixed workspace is valid and `check`-clean throughout —
how a document spells itself is its own to declare.

| Axis | Effect |
| --- | --- |
| `notation`, `path_style` | re-spell the document's own path links, in frontmatter and body; destination, label and wrapper preserved; id/external/alias targets untouched |
| `metadata.format` | re-emit the metadata block in another language, keeping its embedding shape |
| `metadata.embed` | re-emit it in another shape, keeping its language |
| `content_format` | transcode the body prose — **and rename the file** |

The fourth is the odd one, and unavoidably so. A metadata block declares its own
language *inside* the file, so those conversions rewrite in place. A body's
grammar is declared by its **filename** — that is the only place prov reads it
from, and the only place every other tool on the machine reads it from too. So
`prov convert notes.md content_format djot` transcodes the prose *and* moves
`notes.md` → `notes.dj`, retargeting every inbound reference exactly as a rename
would (`colophon:<id>` references are left alone; the registry's `id → path`
update is what keeps those resolving). A recursive sweep lands as one change set,
so a document that links to another document in the same sweep comes out pointing
at where that one actually went.

Markdown ↔ Djot converts freely: emphasis, headings and raw HTML are re-spelled
into the target grammar, and footnotes, tables, code fences and `[[wikilinks]]`
survive intact. A reference-style link is inlined (`[x][ref]` → `[x](b.md)`),
leaving its now-unused `[ref]:` definition behind. Anything with **`html` at
either end is lossy** — into HTML the authored markup is gone, out of HTML
whatever has no prose spelling survives as a raw escape or not at all — and needs
`-f`/`--force`.

A separated pair converts its **body**, not its node: the node's extension names
a metadata format, so `notes.md` moves to `notes.dj` and the node's `content`
pointer follows it. An attachment's opaque payload and a document with no body
have no prose to convert — an error when named directly, a skip when merely
swept.

Moving a whole workspace across is three commands, because each does a distinct
thing:

```
prov config content_format djot            # what new documents get
prov convert index.md content_format djot -r   # what existing ones become
prov about                                 # regenerate the page that names the root
```

The last is needed because the about page states the root's filename, which the
sweep just changed; `check` reports it as stale either way. The sweep does not
reach the about page itself — `-r` follows the spanning relation, and the page
hangs off the root's `about` pointer instead — so convert it directly
(`prov convert about.md content_format djot`) or just regenerate it, which writes
it in the configured grammar.

## Presets — a common setup, written out rather than named

A **preset** is a bundle of the configuration a common kind of workspace
needs — a vocabulary, the fields a new document starts with, the views that
answer "what is open" — that prov writes for you, in full, and then forgets. It
is a stencil, not a default: nothing in the vocabulary above says `preset:`,
and prov's reader never learns a preset's name. Once applied, the workspace is
ordinary, fully spelled-out config, readable by anyone with the spec and no
binary. The argument is
[the proposal](/docs/proposals/presets/proposal-presets-v1.md); this is what
shipped.

**On disk, a preset is a directory laid out like the root of the workspace it
will be merged into.** A node — `prov.yaml`, or any whole-file format —
carrying only the axes the preset declares, and beside it, at the paths that
config's links name, the stores those axes point at:

```
presets/tasks/
  prov.yaml             # fields: and views: — and no other key
  vocab/statuses.yaml   # the vocabulary the fields entry links
```

Because the layout is the workspace's own, `[Statuses](/vocab/statuses.yaml)`
means the same thing in the preset directory as it does after the copy. This
repository's [`presets/tasks/`](/presets/tasks/prov.yaml) is that example; its
test applies it to a scratch workspace and runs `check` over the result.

**prov ships exactly one preset, and it has no name.** It is what `init` writes
when told nothing else — a `created` field stamped on creation and an
`updated` field stamped on every edit, so that every document made here
records when, and every edit prov lands records that it did — and it is a preset rather than a hard-coded default so that a
directory can *replace* it: `init --preset <dir>` writes the directory instead.
A flag that names a field the preset also stamps (`--updated-field modified`)
wins, and the preset's own field for that stamp is not written.
There is no registry and no list of names in the binary; every other preset is
a directory, named by its path, and publishing one is `git push`.

**Applying is additive and refuses collisions.** `prov presets <dir>` prints
what applying would do — one line per config entry and per store — and touches
nothing; `--write` applies it; with no `<dir>`, both act on the built-in. Each
entry (`fields.status`, `views.open-tasks`, `fixity`) is written where the
workspace does not declare it, or declares it at its default; an entry already
declared the same way is nothing to do; an entry declared *differently* is a
collision, reported with the rest of the plan, and nothing is written, because
the two declarations mean different things and prov cannot choose. A store is
written where nothing sits at its path, skipped where the same bytes already
do, and a collision where different ones do. Applying a preset a workspace
already carries is not an error, which is how a repository can assert that its
config *is* the preset. The library exposes the same two steps
(`Workspace::plan_preset`, `apply_preset`) for a tool that sets workspaces up
itself.

## Making config explicit

Because every axis has a default, a workspace need not spell config out. For
authors who prefer nothing implicit, `prov config --setup` materializes the
full effective config into the config document (bootstrapping `prov.yaml` if
none is linked): it preserves the document's own fields and every setting already
present, and fills in the rest at their default. The layout is canonicalized
(comments in the config document are not preserved).

## `about` — the workspace's own reading instructions

`about: structure` (the default) generates **`about.md`** at the workspace root:
a short prose page telling a reader with no prior knowledge how to read *this*
directory. It is the spec **specialized** against this configuration — every
rule resolved to a concrete fact, every branch this workspace does not take left
out. Where the spec says "the block is fenced by `---`, `;;;`, or ```` ```fig
````," the generated page says "every file here opens with a `---` line."

Where the workspace declares [views](#views), the page lists them too — each
one's label, what it groups by where that can be said in words (a field, a
chain, a grain over either; anything else is called a rule and left in the
settings), and whether a `where:` leaves some files out — because a
containment tree is only one way through the files and a reader should be told
what the others are. `filing:` and `icon:` are left out: one is a writing rule
and the other a hint to a picker, and neither helps a person reading the
directory.

It is derived from configuration and from what prov accepts on read — **never**
from a scan of what the files contain. That one rule is why it is both
permanently accurate and almost never rewritten, and why a conflicted copy is
resolved by regenerating rather than merging.

The page follows the workspace's own conventions: its metadata block is written
in `metadata.format` and embedded per `metadata.embed`, and under
`metadata.embed: separate` — where no file carries a fence — it is written
**content-only**, with no block and no sidecar. Its prose is written in
`content_format`.

- `prov about` regenerates it and creates the root's `about` pointer if absent.
- `prov about --print` writes it to stdout, touching nothing.
- `prov about --check` exits non-zero when it is missing or stale.
- `prov config <key> <value>`, `--setup` and `--home` regenerate it, and so do
  the mutations that *bootstrap machinery* the page lists (the first id minted,
  the first delete, the first capture). Ordinary mutations leave it alone.
- `check` reports `AboutStale`; `check --fix` rewrites it.

`prov ignore` lists it as *bookkeeping*, so a tool recording the folder never
takes it: the page is a pure function of configuration that tool already has.

### Git

The page is derived, so a merge conflict in it is never damage — the resolution
is always to regenerate. Tell git not to try:

```gitattributes
about.md merge=ours
```

## Implementation note (internal representation)

The clean orthogonal *config surface* (`notation` × `path_style`) is mapped onto
the existing internal `(Wrapper, LinkStyle)` at the config boundary
(`config.rs`), rather than rewriting every `Wrapper`/`LinkStyle` use site.
`LinkStyle` is the full 2×2 cross-product
(`{markdown,plain} × {root,relative}`) so every `notation`/`path_style`
combination is representable — and, since `canonical` was retired, every one of
them round-trips: a link prov writes in any style resolves back to the document
it names, from wherever it was written (`link.rs`, `mod properties`).
`Notation`/`PathStyle` are config-facing enums with `compose`/`decompose` helpers
to and from `(Wrapper, LinkStyle)`. The fused-`LinkStyle` wart is thus confined
below the config layer and invisible in the frontmatter contract.
