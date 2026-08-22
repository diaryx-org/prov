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
- **The dedicated config document** (`prov.<ext>`, the `config`-relation
  target), where keys sit at **top level** (the whole document is policy, so no
  wrapper is needed). This is the **policy** home — how prov behaves.

This mirrors the `.prettierrc` / `package.json` `"prettier"` duality: a tool's
config sits bare in its own file and namespaced in a shared one. Precedence, both
applied over the defaults:

```
default  <  root `prov:` block  <  config document (top-level)
```

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

The `config`, `registry`, `recycle_bin`, `history`, and `about` **pointer relations** are
*not* policy — they are structural links the root declares so the workspace
unfolds from its own root (DESIGN §6). They remain at the root's top level
alongside `part_of`/`contents`, resolved by the same link machinery. This also
resolves the `recycle_bin` and `history` name clashes by location: the top-level
key is a *pointer* (a path to the bin index / the history store index); the
`prov:`-block key of the same name is a *policy* (a bool / an enum).

```yaml
title: My Vault
author: adammharris
config: prov.yaml             # pointer (structure) — top level
registry: registry.yaml           # pointer — top level
recycle_bin: recyclebin/index.md  # pointer (a path) — top level
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
  spec: 1                     # vocabulary version marker (integer)

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
    contents:
      means: "documents contained by this one"   # human gloss — carried, never read
      cardinality: many       # one | many
      inverse: part_of        # the reciprocal field
      notation: wikilink      # …plus any reference-axis override, same block
      target: alias
    part_of: { cardinality: one, inverse: contents, target: id }
  fields:                     # field declarations — types and controlled vocabularies
    audience:
      type: str               # what the value *is* — see "Field types" below
      values: closed          # open (folksonomy) | closed (must be a known term)
      vocabulary: '[Audiences](/vocab/audiences.yaml)'   # pointer to the term store
      reify: true             # each term is its own node (backlinks, prose, stable id)
    created:
      type: date              # a type alone is a complete declaration
  views:                      # declared lenses — see "Views" below
    daily:
      label: Daily
      icon: calendar          # a hint for a frontend; prov never interprets it
      group: [date_of_document, created]  # field, or a chain (first non-empty wins)
      by: month               # a grain — cut the value coarser (year/month/day/initial)
      under: '[Daily](id:abc1234)'        # scope: the subtree below this index
      where:                  # conditions a document in scope must also meet
        not: { has: draft }
      nest: year              # how deep a *new* entry is filed, independent of `by`
  exports:                    # what may *leave* — see "Exports" below
    letters:
      label: Letters home
      gate:                   # the membership test, written in each document
        field: audience       # a document is in this export only if…
        value: family         # …its own `audience` declares `family`
      view: daily             # optional arrangement — may narrow, can never widen
  id_storage: both            # registry | frontmatter | both
  updated: modified           # name of the machine-maintained timestamp field (omit/"" = off)
  workspace_id: notes         # what this workspace calls itself (omit/"" = anonymous; `prov id --workspace`)

  # ── policy: how prov behaves (conventionally in prov.yaml) ──
  identity: lazy              # none (a.k.a. off) | lazy | eager
  fixity: all                # off | attachments | all
  recycle_bin: true          # bool — route delete to the recoverable bin
  history: off               # off | manual — maintain the skiplist scoping a historica store
  about: structure           # off | structure — generate about.md, the page that explains this directory
```

Every axis is optional; an absent key keeps its default. Defaults:
`content_format: markdown`, `metadata.format: yaml`, `metadata.embed: delimited`,
`references: { notation: markdown, path_style: root, target: path, label: false }`,
`id_storage: both`, `updated: ""`, `workspace_id: ""`, `identity: lazy`,
`fixity: attachments`, `recycle_bin: true`, `history: off`, `about: structure`. Absent `spanning`/`relations` **definitions** ⇒ the built-in
diaryx vocabulary (`RelationSet::from_config` falls back), so a minimal vault
declares none; absent `fields` ⇒ no field is described (every such field is
ordinary carried content); absent `views` ⇒ the workspace declares no lenses.
The `spanning`, relation-definition
Absent `exports` ⇒ nothing is declared exportable — the default state of a
workspace and of every document in it. The `spanning`, relation-definition
(`cardinality`/`inverse`/`means`), `fields`, `views` and `exports` axes are the
*self-description* layer — see [Spec](/docs/spec.md).

### Views

The spanning relation is one way through the workspace: a single-parent tree,
every document in exactly one place. A **view** is a second way through the same
documents — "the entries under `Daily`, by month", "everything, by tag" — and
the same document may appear under several groups, which is precisely what the
spine cannot do.

| key      | means                                                                 |
| -------- | --------------------------------------------------------------------- |
| `group`  | a field name, or a list of field names tried in order (first non-empty wins). **Required** — an entry without one is not a view |
| `by`     | a **grain** — cut the chosen value coarser before grouping (see below) |
| `under`  | a link to an index; the view covers its whole spanning subtree. Absent = the whole workspace |
| `where`  | conditions a document in scope must also meet. Absent = everything scope reaches |
| `nest`   | a grain — how deep a *new* entry is filed. Only grains that chain, and only single-valued fields |
| `label`  | what a person calls it (absent = the name, humanized)                 |
| `icon`   | a glyph hint, uninterpreted                                           |

#### `where:` — conditions

Two predicates and three combinators, and deliberately no more:

| key       | means                                                        |
| --------- | ------------------------------------------------------------ |
| `has`     | the field is present and carries a non-empty value; a list means all of them |
| `equals`  | the field carries this value — any element of it, for a list. Compared as text |
| `not`     | the inverse of the condition it wraps                        |
| `any-of`  | at least one of a list of conditions                         |
| `all-of`  | every one of a list of conditions                            |

A mapping with several keys is an implicit **and**, which is the shape a real
condition usually takes:

```yaml
where:
  has: audience
  equals: { audience: public }
```

This stops well short of an expression language, on purpose. Formulas are the
point of no return for a view format: a closed set of named predicates can grow
one member at a time, each with a reason, and a grammar cannot be taken back
once views are in the wild. The rule for adding a predicate is a concrete lens
that cannot otherwise be said.

Three cases resolve toward *saying so* rather than guessing, because each has
two opposite silent readings. An empty or unreadable `where:` is not a filter —
it is a `check` finding, since guessing would either publish a workspace or hide
it. `any-of: []` matches nothing and `all-of: []` matches everything (vacuous
truth both times, and the reading where an empty disjunction cannot publish by
accident). And `has` means present *and non-empty*, since a field written blank
has nothing to group or display.

#### The rest

Scope and conditions are separate keys because they fail differently: an anchor
that names nothing is a **broken view** and errors, while a condition that
matches nothing is an ordinary empty answer.

Three things are load-bearing, and each is a place an obvious shortcut is wrong.

**There is no `date` grouping.** `group:` names fields and `by:` cuts values, so
a date view is those two things pointed at date fields — the chain
`[date_of_document, created]` above is a declaration *this workspace* makes, not
a convention prov blesses. A workspace that files by `taken_on` writes that
instead. A grain applies to a *value*, never to a declared type, so it needs no
`fields.<name>.type` declaration to work; a value a grain cannot cut does not
group, rather than grouping wrongly.

#### Grains

A grain is a **coarsening** — any many-to-one function from a value to a group
key. The calendar is one family of them, not the subject:

| `by:` / `nest:`  | groups                                       |
| ---------------- | -------------------------------------------- |
| `year`           | `2026-07-24` → `2026`                        |
| `month`          | `2026-07-24` → `2026-07`                     |
| `day`            | `2026-07-24` → `2026-07-24`                  |
| `initial`        | `Lovelace` → `L` — the A–Z index             |
| `{ initial: 2 }` | `Lovelace` → `LO`                            |

The date grains **validate** rather than slicing, so `banana` at year grain is
not the group `bana` and `20264` is not the year `2026`; anything after the cut
is ignored, so an RFC 3339 instant cuts like the plain date it starts with.
`initial` cuts by *character* (`Ålesund` → `Å`) and upper-cases, deliberately:
an index that files `ada` apart from `Ada` is not an index.

New grains are added by one rule — a concrete lens that cannot otherwise be
said, not a shape that seems likely to be wanted. A numeric `bucket` is the
obvious candidate and is deliberately absent: nobody has asked for one, and its
keys would sort lexically as `0, 10, 100, 20`, needing group ordering to become
grain-aware, which is the deferred `sort:` axis under another name.

#### What `nest:` can and cannot file

`nest:` takes the same grains, but not every grain and not every field, because
it **writes**. Filing builds a hierarchy of index documents, so a grain may nest
only if its coarser steps are *determined* by its finer ones — `2026-07-24` →
`2026-07` → `2026`, `Ada` → `Ad` → `A`. Every grain above chains; an arbitrary
sequence of coarsenings would not.

The second limit is prov's spine, not taste. `nest:` files into the spanning
relation, which is single-parent, so the grouping field must be **single-valued**
for the document being filed. A document listing two people has two homes and
nothing can choose between them — so `nest:` on a field declared `type: seq` is
a `check` finding, and a document that turns out multi-valued at filing time
simply has no route. Grouping by such a field stays perfectly good; one document
under several groups is the whole point of a view. Only the filing half is
constrained.

**`under:` is a traversal, not a path filter.** The scope is resolved by walking
the spanning relation below the anchor, so it survives a rename, a move and a
retitle — where `path starts-with "Daily/"` would not, and where matching an
index *titled* `2026` finds the one under `Trips/` just as happily.

**`nest:` is independent of `by:`.** Grouping is a reading decision and filing is
a writing one; a picker that reads like a display setting must not silently
change where tomorrow's entry lands. A view may group finer than it files.

prov reads views and never acts on one — a view has no invariant, so no `check`
finding can come from a wrong one, and `nest:` describes where a frontend should
file a record rather than something prov goes and does. They live in the config
so that every tool over the workspace reads the same lenses instead of each app
keeping its own block. `prov views` lists them; `prov views <name>` executes one.

The format and its engine are the `prov-views` crate, which depends only on
prov's read core and so cannot write to the workspace it reads. Running a view
is two steps, and they are worth knowing apart: **select** answers *which
documents does this view cover* (scope, then conditions) and returns a flat,
deduplicated set; **group** projects that into groups and is a pure function.
So the count of documents a view covers and the count of rows it draws are
different numbers — a document under two of a multi-valued field's groups is one
document in two places — and `prov views <name>` prints both.

### Exports

Everything above reads open by default — a view with no `under:` covers the
whole workspace. An **export** is the boundary where that flips: a named,
closed-by-default set of documents that may *leave* the workspace.

```yaml
exports:
  letters:
    label: Letters home
    gate: { field: audience, value: family }
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

`view` optionally arranges what leaves, and it obeys a one-way valve: **an
export's set is a subset of what its gate admits, whatever the view says**. A
view may narrow the set; it can never put back a document the gate held out.
An export naming an unknown or broken view is an error, never a fall-back to
the gate's whole set — the view was written down as a bound on what leaves,
and a bound nobody can apply must fail closed. (`diagnose` also flags the
unknown-view typo at author time, when `views:` and `exports:` share a
surface.)

There is no `index:` (front page) key: which page greets a reader is a
rendering concern, declared by the publish layer that owns a render. prov only
ever *plans* an export — `prov exports` lists them, `prov exports <name>`
previews what leaves, what the gate held back, and what the view scoped out,
and moves nothing. Publishing, copy-out, and OCFL export are downstream
consumers of the same plan. The format and planner are the `prov-exports`
crate, which depends only on the read core and `prov-views` and so cannot
write to — or leak from — the workspace it judges.

### Field types

A `fields.<name>` entry declares two independent things, and needs at least one
of them to be worth writing: a **type** (`type:` — what the value is) and a
**controlled vocabulary** (`vocabulary:` + `values:` — which values are legal).
Neither implies the other. `created` is a date nothing controls; an `audience`
vocabulary needs no declared type. An entry with neither is ignored.

The type vocabulary is [`fig-schema`](https://crates.io/crates/fig-schema)'s, not
one prov invents, so prov, a metadata editor, and a view engine all name types
identically instead of agreeing by convention:

| `type:`          | means                                      |
| ---------------- | ------------------------------------------ |
| `str`            | text                                       |
| `bool`           | `true` / `false`                           |
| `int` / `float`  | a number                                   |
| `date`           | a calendar date, `2026-07-24`              |
| `datetime`       | an instant with offset, `2026-07-24T07:32:00Z` |
| `local-datetime` | a date and time with no offset             |
| `time`           | a time of day, `07:32:00`                  |
| `ref`            | a link to another document                 |
| `map` / `seq`    | a nested mapping or list                   |

prov carries the type without interpreting it — nothing in `check` fails because
a value does not match its declared type. It is there so a frontend can parse and
render the field faithfully (a `date` gets a date picker, not a text box).

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
| `updated_field: modified` | `updated: modified` | reframed as "this field is machine-maintained" |
| — | `spec: 1` | new version marker |
| `config`/`registry`/`recycle_bin`/`history` pointers | unchanged, top-level | structure, not policy |
| — | `history: off` | new axis — historica-store skiplist maintenance, off by default |
| — | `about: structure` | new axis — the generated `about.md`, **on** by default |

## Linting (`check`)

`config::diagnose` runs over both surfaces — the root's `prov:` block and the
config document — reporting a `Finding::ConfigIssue` per key prov would
silently ignore:

- **Invalid value** on a recognized axis (e.g. `fixity: alll`) — keeps the
  default; the finding lists the accepted spellings.
- **Unknown key** that is a near-miss of a real axis (e.g. `notaton`) — a likely
  typo, reported with the suggestion. A key resembling *no* axis is left alone (a
  user field), except inside the closed sub-blocks (`metadata`, `references`, a
  `relations` entry, a `fields` entry), where every key is expected to be a known
  axis. A `relations` entry additionally accepts the definition keys
  `cardinality`/`inverse`/`means`.
- **Spanning invariant** — a `spanning` relation whose declared `inverse` is
  itself declared `cardinality: many` cannot form a single-parent tree (DESIGN
  §3), reported as `SpanningNotSingleParent`.
- `spec`, and the config document's own `title`/`part_of`, are whitelisted.

Beyond the two config surfaces, `check` also validates the workspace's **stores**
and **controlled fields** (see [Spec](/docs/spec.md)): a `MalformedStore` finding
for a registry/recycle/vocabulary pointer that resolves to a markdown document
rather than a whole-file config document; `UnknownTerm` for a closed-field value
that is not a known term; and `TermNearMiss` for an open-field value that closely
resembles one.

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

The skiplist (`prov history-skips`) deliberately rules it out of recording:
the page is a pure function of configuration that is itself recorded.

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
