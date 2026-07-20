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

The `config`, `registry`, and `recycle_bin` **pointer relations** are *not*
policy — they are structural links the root declares so the workspace unfolds
from its own root (DESIGN §6). They remain at the root's top level alongside
`part_of`/`contents`, resolved by the same link machinery. This also resolves the
`recycle_bin` name clash by location: the top-level `recycle_bin` is a *pointer*
(a path to the bin index); the `prov:`-block `recycle_bin` is a *policy* (a
bool).

```yaml
title: My Vault
author: adammharris
config: prov.yaml             # pointer (structure) — top level
registry: registry.yaml           # pointer — top level
recycle_bin: recyclebin/index.md  # pointer (a path) — top level
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
    path_style: root          # root | relative | canonical   (path targets only)
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
  fields:                     # controlled vocabularies (tags, audiences, …)
    audience:
      values: closed          # open (folksonomy) | closed (must be a known term)
      vocabulary: '[Audiences](/vocab/audiences.yaml)'   # pointer to the term store
      reify: true             # each term is its own node (backlinks, prose, stable id)
  id_storage: both            # registry | frontmatter | both
  updated: modified           # name of the machine-maintained timestamp field (omit/"" = off)

  # ── policy: how prov behaves (conventionally in prov.yaml) ──
  identity: lazy              # none (a.k.a. off) | lazy | eager
  fixity: all                # off | attachments | all
  recycle_bin: true          # bool — route delete to the recoverable bin
```

Every axis is optional; an absent key keeps its default. Defaults:
`content_format: markdown`, `metadata.format: yaml`, `metadata.embed: delimited`,
`references: { notation: markdown, path_style: root, target: path, label: false }`,
`id_storage: both`, `updated: ""`, `identity: lazy`, `fixity: attachments`,
`recycle_bin: true`. Absent `spanning`/`relations` **definitions** ⇒ the built-in
diaryx vocabulary (`RelationSet::from_config` falls back), so a minimal vault
declares none; absent `fields` ⇒ no field is controlled (every such field is
ordinary carried content). The `spanning`, relation-definition
(`cardinality`/`inverse`/`means`), and `fields` axes are the *self-description*
layer — see [Spec](/docs/spec.md).

### The two reference axes, orthogonalized

Previously `link_format` fused *notation* (bracketed vs bare) with *path
resolution* (root/relative/canonical), and `reference_wrapper` added `wikilink`
as a separate key — so `link_format: plain_canonical` produced a **bare** link
even though the wrapper said "markdown." The reshaped `references` block separates
the two truly-orthogonal axes:

| `notation` | `path_style` | rendered path reference |
|---|---|---|
| `markdown` | `root` | `[Title](/path/x.md)` |
| `markdown` | `relative` | `[Title](../x.md)` |
| `markdown` | `canonical` | `[Title](path/x.md)` |
| `bare` | `root` | `/path/x.md` |
| `bare` | `relative` | `../x.md` |
| `bare` | `canonical` | `path/x.md` |
| `wikilink` | *(any)* | `[[path/x.md]]` — `path_style` shapes the inner path text |

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
| `config`/`registry`/`recycle_bin` pointers | unchanged, top-level | structure, not policy |

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

## Making config explicit

Because every axis has a default, a workspace need not spell config out. For
authors who prefer nothing implicit, `prov config --setup` materializes the
full effective config into the config document (bootstrapping `prov.yaml` if
none is linked): it preserves the document's own fields and every setting already
present, and fills in the rest at their default. The layout is canonicalized
(comments in the config document are not preserved).

## Implementation note (internal representation)

The clean orthogonal *config surface* (`notation` × `path_style`) is mapped onto
the existing internal `(Wrapper, LinkStyle)` at the config boundary
(`config.rs`), rather than rewriting every `Wrapper`/`LinkStyle` use site.
`LinkStyle` is extended to the full 2×3 cross-product
(`{markdown,plain} × {root,relative,canonical}` — adding `MarkdownCanonical` and
`PlainRoot`) so all six `notation`/`path_style` combinations are representable.
`Notation`/`PathStyle` are config-facing enums with `compose`/`decompose` helpers
to and from `(Wrapper, LinkStyle)`. The fused-`LinkStyle` wart is thus confined
below the config layer and invisible in the frontmatter contract.
