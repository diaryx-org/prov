---
part_of: '[prov](/README.md)'
---
# Reference styles

How a durable reference — a relation target in metadata, or (eventually) a body
link — is *spelled*. prov exposes every option as configuration; a curated
frontend (diaryx) picks one. This is the identity-vs-readability dial made
explicit.

## The axes

A reference style is a **notation**, an **addressing** (`target`), an optional
**label**, and — for path targets — a **path_style** (`docs/config-vocab.md`,
`references:` block). Notation and path resolution are orthogonal: notation says
*how the reference is delimited* (bracketed markdown, wikilink, or bare), path
style says *how a path is resolved* (root / relative).

| notation | addressing | written form | durable (move/rename-safe)? | readable raw? |
|---|---|---|---|---|
| `markdown` | `path` | `[Title](/notes/a.md)` (path per `path_style`) | ❌ rewritten on move | ✅ |
| `bare` | `path` | `/notes/a.md` (path per `path_style`) | ❌ rewritten on move | ✅ |
| `markdown` | `id` | `[Title](id:ajp7eq)` | ✅ | ✅ (title shown) |
| `wikilink` | `id` (no label) | `[[id:ajp7eq]]` | ✅ | ❌ opaque id |
| `wikilink` | `id` (label) | `[[id:ajp7eq\|Title]]` | ✅ | ✅ |
| `wikilink` | `alias` | `[[Title]]` | ❌ nominal, index-resolved | ✅ |
| `wikilink` | `path` | `[[notes/a.md]]` | ❌ rewritten on move | ✅ |

- **`id`** addresses by the durable `id:<id>` handle. Authoring one *registers*
  the target (the link-by-id trigger, DESIGN §4). Survives moves untouched — the
  registry update is the maintenance.
- **`alias`** addresses by the target's title/name, resolved nominally through a
  title index. Readable, but not move/rename-safe, and it never registers. The
  weakest-but-prettiest option.
- **`path`** is the classic diaryx form; `path_style` chooses the rendering
  (`root` = `/a.md`, `relative` = `../a.md`), and `notation` chooses whether it is
  bracketed (`markdown`) or bare (`bare`). There is deliberately no third,
  slashless workspace-relative spelling: a bare target resolves against the
  document it sits in, so `a.md` written in `notes/x.md` means `notes/a.md`, and a
  style that rendered workspace-relative paths that way meant something different
  from what it resolved to (`docs/config-vocab.md`, "`canonical` is retired").
- The **label** on an `id` wikilink (`|Title`) is a *cached copy* of the target's
  title — cosmetic, refreshable. `check` can flag a stale one (`StaleLabel`,
  staged) and refresh it, turning "fallible cache" into "maintained cache."

The `id:` scheme replaces the older `prov:` spelling (de-branded, shorter,
still explicit and diagnosable, and — unlike a `prov://` URL — it does not
collide with `is_external`'s `://` check). `prov:` is still recognized on
read for backward compatibility.

`alias` implies `wikilink` (neither `markdown` nor `bare` can address by bare
name), so an `alias` request always renders as a wikilink.

## Up ≠ down: style is per-relation

Style is resolved **per relation**, with the workspace default as the fallback.
"Different links going down (`contents`) vs up (`part_of`)" is just two relations
each carrying their own style — no new concept, because a link is stored as *two
independent fields in two files* (`A.contents → B` and `B.part_of → A`), and each
side is authored in its own relation's style. On read the resolver is
style-agnostic: it takes whatever it finds and resolves it.

```yaml
# workspace default (root `prov:` block / config document)
references:
  notation: wikilink
  target: id
  label: true

relations:
  contents: { notation: wikilink, target: alias }   # DOWN: reads like a TOC
  part_of:  { notation: markdown, target: id }       # UP: durable bookkeeping
```

### Consequences (chosen deliberately, not stumbled into)

1. **Registration follows the `id` direction.** Whichever direction is `id`-style
   is the link-by-id that registers. With `part_of: id` + `contents: alias`, every
   non-root node registers *its parent* → internal nodes get IDs, pure leaves may
   not.
2. **Bidirectional reconcile tie-break.** When a stored inverse pair disagrees,
   the `id` side is durable and the `alias`/`path` side is fallible — trust the
   `id` side.
3. **Don't make both spanning directions `alias`.** Then nothing is durable,
   nothing registers, and a title rename dangles the structure both ways. `check`
   should warn.

## Alias resolution

A nominal (`alias`) reference is resolved through a **title index** — a derived
`name → document` map (`title.rs`) built by a flat filesystem scan of the
workspace, deliberately independent of link resolution so alias links can
themselves be *spanning* (`contents: alias`) without a chicken-and-egg. A
document is indexed under both its `title` and its file stem, so `[[My File]]`
(by title) and `[[my-file]]` (by stem) both find it.

Resolution outcomes (`Workspace::resolve_link_with`, threaded through `tree` and
`check`):

- **Unique** — the one document with that name; resolves like any path.
- **Ambiguous** — several documents share the name; surfaced as
  `Target::AmbiguousAlias` / `NodeKind::AmbiguousAlias` / a
  `Finding::AmbiguousAlias` from `check`. A nominal link cannot choose, so this
  is a diagnosable error rather than a silent pick.
- **Unknown** — no document claims it; falls through to a path, so it reads as a
  missing/broken link exactly as before aliases existed.

Only *alias-shaped* targets (a bare name — no path separator, no extension) are
looked up; paths and `id:` targets are never diverted.

## Across workspaces

A reference can name a document in *another* workspace by qualifying the id with
that workspace's name:

```
id:notes/97jx77t                       # bare
[Recipe Index](id:notes/97jx77t)       # markdown
[[id:notes/97jx77t|Recipe Index]]      # wikilink, labeled
```

The qualifier is the target workspace's own `workspace_id` — the one piece of
this that is genuinely a fact about an archive, so the one piece that lives in
its config (`docs/config-vocab.md`). A workspace with no name is **anonymous**:
it can hold foreign references, but no reference can be recognized as pointing
back at it. The name may not be empty or contain `/`, `:` or whitespace, because
it has to survive being written in the position above; a value that cannot is
reported (`MalformedWorkspaceId`) and ignored rather than half-honored.

### What prov does, and where it stops

prov owns the **grammar**; it does not own **resolution**. A qualified reference
resolves to `Target::Foreign` / `Resolution::Foreign` and stops there:

- **Never rewritten.** A move re-relativizes only path targets
  (`Link::is_path_target`), and a qualifier names a workspace, not a directory.
  Re-relativizing one could only damage it.
- **Never reported broken.** `check` has no evidence about a workspace it cannot
  see. A finding raised on no evidence is a false positive on *every* such
  reference, which every host would then have to filter back out — and a `check`
  that must be filtered is one nobody reads.
- **Never check-verified.** The foreign workspace owns its id space and need not
  be a prov workspace at all: a diaryx ARK blade is a different length and
  alphabet, so applying prov's check character would reject valid references.
  (A *local* id still is verified — see the self-qualification rule below.)
- **A leaf in the tree.** A spanning link to another workspace renders as
  `NodeKind::Foreign` rather than being followed or dropped: the structure
  really does leave the building, and a reader deserves to see it.

There is deliberately **no peer table in `prov.yaml`**. Where some other
workspace can be found is a property of a device, not of an archive — `notes =
../notes` is true on exactly one machine, and worse than being wrong elsewhere it
would be wrong *silently*, since a peer resolving to the wrong directory resolves
to real documents. This is the same reasoning that keeps the fixity cache's
location out of the config (DESIGN §5). A name is a fact about an archive; a
location is a fact about a disk.

Resolution therefore belongs to the host. `prov-cli` keeps a device-local peer
map (`prov peer add <name> <dir>`, resolved `--peers` > `PROV_PEERS` >
`XDG_CONFIG_HOME/prov/peers` > the platform default), and `prov peer resolve
id:notes/97jx77t` follows one by opening that workspace and reading *its*
registry. diaryx resolves the same reference through its published ARK
permalinks instead. Neither map is prov's business, and nothing depends on
either: a foreign reference is carried whether or not it resolves.

### Self-qualification — the rule with teeth

A reference qualified with the reading workspace's **own** name is not foreign.
It is local, and resolves through the registry exactly as the unqualified
spelling would:

```yaml
# read inside the workspace whose workspace_id is `notes`
links: [id:notes/97jx77t]   # ≡ id:97jx77t — resolved, verified, and dangling loudly if absent
# read anywhere else
links: [id:notes/97jx77t]   # foreign — carried, silent
```

This is what makes a qualified reference survive being *copied into* the
workspace it names, instead of going inert at the boundary. It is also the
invariant that justifies the feature living in prov at all: a workspace must
recognize its own name, and only prov is in a position to enforce that.

An anonymous workspace has nothing to compare against, so it treats every
qualifier as foreign — it must not guess that `id:notes/…` means itself.

### What is not covered

Registration stays a **publish-time** contract, as in diaryx's ROADMAP: prov
never reaches into another workspace to register an id on its behalf. So a
reference to an *unpublished* foreign document can dangle, and nothing here will
notice. That is a real limit, stated rather than hidden — closing it would
require workspace A to reach workspace B, which is exactly the reaching this
design refuses.

## Implementation status

- ✅ `ReferenceStyle` renderer + parsing (`link.rs`); the config-facing
  `Notation` (`markdown`/`wikilink`/`bare`) and `PathStyle`
  (`root`/`relative`) axes compose to the internal `Wrapper` +
  extended `LinkStyle` (the 2×2 cross-product); `id:` scheme with legacy
  `prov:` read.
- ✅ Per-relation `style` on `Relation` (`relation.rs`).
- ✅ Workspace default + config keys (`config.rs`), authoring seam
  (`authored_target`) resolves per-relation style.
- ✅ `alias` **resolution** via the title index (`title.rs`), wired through
  `tree` and `check` (unique / ambiguous / unknown).
- ✅ Per-relation styles declared in a config surface: a `relations:
  { <name>: { notation, path_style, target, label } }` block, each axis optional
  and overlaid on the workspace default (`RelationStyleConfig` +
  `WorkspaceConfig::resolved_relation_styles` in `config.rs`;
  `RelationSet::with_styles` in `relation.rs`; wired through the CLI's workspace
  builder).
- ✅ `prov init` surfaces the model *wrapper-first*: `--wrapper`
  (markdown / wikilink) and `--link-style` set the workspace `notation`/
  `path_style`, then `--reference` picks the addressing (`path` / `id` / `alias`
  / `split` — the up≠down diaryx shape). The choices write the `references`
  defaults and, for `split`, the `relations` block. `id`/`split` are gated on
  `--identity` ≠ none.
- ✅ **Cross-workspace references** (§ "Across workspaces"): the `workspace_id`
  config axis + `is_valid_workspace_id` (`config.rs`); `IdRef`
  (`Local`/`Foreign`/`Malformed`) and `Link::is_path_target` (`link.rs`);
  `Target::Foreign` / `Resolution::Foreign` / `NodeKind::Foreign`, with
  self-qualification resolving locally in both resolvers; the five rewrite sites
  in `mutate` filtering on `is_path_target`; and the CLI's device-local peer map
  (`prov peer list|add|remove|resolve`, `peer.rs`).
- ⏳ **Staged:** `StaleLabel` finding + label refresh in `validate.rs`.
  Body-prose reference restyle during the `mutate` port.
