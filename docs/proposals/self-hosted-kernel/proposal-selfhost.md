---
title: self-hosted kernels
author: adammharris
created: 2026-07-31
updated: 2026-08-01
status: implemented (phases 0 and 1)
part_of: '[`prov` proposals](/docs/proposals/proposals.md)'
contents:
- '[Prov 1.draft](/docs/proposals/self-hosted-kernel/examples/prov-1.draft.md.yaml)'
- '[ORGANIZATION](/docs/proposals/self-hosted-kernel/examples/ORGANIZATION.md.yaml)'
---
# Self-hosted kernels — the workspace carries its own reading instructions

> **Implemented.** Phases 0 and 1 have shipped: `prov/src/about.rs` holds the
> generator, `prov about [--check] [--print]` the CLI surface, and `AboutStale`
> its `check` finding. The two attached examples are no longer sketches — they
> are the generator's actual output, copied verbatim, and `/about.md` is this
> repository's live page. What the build changed is recorded under
> [What implementation settled](#what-implementation-settled).
>
> Working proposal, first full draft. Supersedes the stub of 2026-07-31; its
> two sketches are revised in place as the worked examples below. Complements
> DESIGN §1 (self-description), §2 (the three tiers), §5 (derived vs
> authoritative), and `docs/spec.md` §1 (the bootstrap kernel) and §4 (link
> target kinds) — which it asks to change in two places.
>
> The two attached examples are the design's actual output:
> `examples/prov-1.draft.md` is what this repository would generate;
> `examples/ORGANIZATION.md` is what a workspace with its own vocabulary
> would generate. They exist to be compared.

## The design in brief

A prov workspace generates **`about.md`**: a short prose document, reached
one-way from the root through a new `about` pointer relation, that tells a
reader with no prior knowledge how to read *this* directory. It is not a copy
of `docs/spec.md`. It is the spec **specialized** against this workspace's
configuration — every rule resolved to a concrete fact, every branch this
workspace does not take deleted. Where the spec says "the block is fenced by
`---`, `;;;`, or ```` ```fig ````," the generated document says "every file
here opens with a `---` line."

It is generated from **config** and from **what prov accepts on read** — never
from a scan of what the files currently contain. That single rule is what
keeps it both permanently accurate and almost never rewritten: config changes
rarely, so the file changes rarely, so it is not a sync hotspot. It is
derived, so a conflict in it is never damage — the resolution is always
regenerate, never merge.

One config axis (`about: off | structure`), one CLI verb (`prov about`), one
new `check` finding, and one new row in the spec's link-target typology.

Everything below is the argument for those paragraphs.

## The problem: self-description has a floor, and the floor is off-site

`docs/spec.md` states the honest limit up front:

> "Self-describing" has an irreducible floor, though — a reader must share
> *some* convention to bootstrap. This page is that floor, kept as small as it
> can be.

The floor is five rules. The trouble is *where they live*: in this repository,
on a forge, in a document the workspace does not contain. A prov workspace
handed to a stranger is self-describing only to the extent the stranger can
obtain `docs/spec.md`. That is a dependency on an institution surviving —
exactly the dependency the project's posture refuses everywhere else
(plaintext over sidecars, tool-agnostic `sha256:` digests, a hand-rolled
SHA-256 rather than a crate).

DESIGN §1 makes the promise plainly: "A prov workspace is one you can hand to
*any* tool and it explains itself." Today it explains its *structure* — the
links are in the documents, visibly — but not its *conventions*: what the
links mean, how they are spelled, which files are in the tree and which are
not. Those live in `prov.yaml`, which is machine-facing and assumes the reader
already knows what its keys mean.

So the gap is narrow and specific: **a person who opens the directory with no
prior knowledge cannot learn to read it from the directory.**

## The move: specialize, don't vendor

The obvious fix is to ship a copy of the spec in every workspace. That is
worse than it looks, and the reason determines everything about the artifact.

A copy of the spec is written for the population of all prov workspaces. Its
reader is assumed to want to write a tool. Every rule it states is a *general*
rule, with branches for cases this workspace does not exercise:

> **Read its metadata block.** A document is a metadata block plus a body. The
> block is separated from the body by a fence at the top of the file — `---`
> for YAML, `;;;` for JSON, or an opening ```` ```fig ```` line.

A reader holding one directory has no use for two of those three branches, and
no way to tell which applies without checking. The generality is not merely
surplus; it is *work transferred to the reader*. Worse, the framing ("in any
prov/1 workspace…") appeals to an institution the reader cannot consult, on
behalf of a comparison they cannot make — they have this directory and nothing
else.

The alternative is to **resolve the rules against the configuration** and emit
the residue:

> Every file here opens with a `---` line. Everything between it and the next
> `---` is the file's metadata, written in YAML. The rest is the document.

Nothing is lost operationally, two branches are gone, and the sentence is
about *this directory* rather than about prov. That is the whole design.

The name is meant literally: the kernel is **self-hosted** when the traversal
rules are resident in — and specialized to — the artifact they describe. A
vendored copy of the spec is not self-hosted; it is the spec, filed nearby.

### What specialization deletes

Against this repository's own `prov.yaml`, mechanically:

| The general rule | Becomes |
| --- | --- |
| root is `.prov`'s target, else `README.md`/`readme.md`/`index.md` | "the root is `README.md`" |
| fence is `---` / `;;;` / ```` ```fig ```` | "every file opens with `---`" |
| block is YAML / JSON / TOML / fig | "the metadata is YAML" |
| relations are whatever `relations` declares | the four this workspace declares, in a table |
| spanning is whatever `spanning` names | "follow `contents` from the root" |
| ids may live in frontmatter, the registry, or both | "each document carries its own `id`, and the registry mirrors it" |
| fixity covers nothing / attachments / everything | "checksums are recorded for attachments" |
| deletion is hard or routed to a bin | "deletes are recoverable until the bin is emptied" |

A consequence worth naming, because it is a quality signal: **the document's
length tracks how unusual the workspace is.** A workspace on defaults gets a
short page — there is little to say beyond "the root is `README.md`, follow
`contents`." A workspace with a bespoke vocabulary gets a longer one, because
there is genuinely more a stranger must be told. The two attached examples
exist to demonstrate exactly this, and should be read side by side. If a
default workspace produces four pages, the generator is padding.

### Specialize on config, and on what prov will read — never on the corpus

The tempting third source is the corpus itself: scan the files and describe
what is actually there. Resist it, with one carefully-drawn exception absorbed
into the second source.

The motivating case is real. Config declares `notation: markdown`, so prov
*writes* `[Label](/path/x.md)`. But prov *reads* wikilinks and bare targets
too, and these files are hand-editable by design — so a stranger told only
about `[Label](…)` will trip over a `[[…]]` that a human typed. Config alone
does not warn them.

The fix is not to scan. It is to describe **what prov will accept when
reading**, which is knowable without touching a single file:

> References here are written `[Label](/path/from/here.md)` — the label is
> decoration, the target is in the parentheses. Two other spellings are also
> understood wherever a reference is expected, and you may find them in
> hand-edited files: `[[path/x.md]]` and a bare `path/x.md`.

This is strictly better than scanning. It costs no traversal, it never goes
stale, and it is *more* honest: a spelling absent today can appear tomorrow
without anything being regenerated, so a scan-derived claim ("no wikilinks
occur here") is a promise the document cannot keep.

Hence the generation rule, the load-bearing constraint of this proposal:

> **`about` is a function of the workspace's configuration and of prov's own
> read behavior. It never reports what the files currently contain.**

Everything downstream — the freshness story, the sync story, the cheapness of
regenerating on a config write — falls out of this one line. It is also what
keeps the artifact honestly named: a description of the directory's
*organization*, not an inventory of its *contents*. An inventory is a
manifest, and manifests are [history's job](#relationship-to-historys-manifests).

## The artifact

### Where it lives: a fourth link-target kind

`about.md` sits at the workspace root and is reached one-way from the root
document through a new **`about` pointer relation** — a peer of `registry`,
`config`, and `recycle_bin`. It declares no `part_of`.

Mechanically this is the smallest possible change. `RelationSet` already
exposes `registry_relation`/`config_relation`/`recycle_relation`/
`history_relation` as siblings (`prov/src/relation.rs:271-289`);
`about_relation` is a fifth. `Workspace::about_path` follows
`Workspace::history_path` exactly.

**On the name.** `about` over `organization` on one decisive point:
`organisation`/`organization`. A hand-typed config key with a live spelling
variant is a support burden forever, and accepting both is worse than choosing
a word that has none. `about` also matches the verb (`prov about`), and its
one mis-cue — web "About" means *about the project*, not *about the structure*
— is corrected instantly by the file's own title.

The typology is where the real work is. `docs/spec.md` §4 offers five kinds,
and this file fits none of them:

- Not a **content node** — it has no `part_of`, earns no id, and must not
  appear in `prov tree` beside the user's own writing. It is not theirs.
- Not **machinery** as §4 currently defines it. It has machinery's *shape*
  (one-way from the root, no inverse, not in the spanning tree, not
  orphan-checked), but §5's MUST says record stores are whole-file config
  documents — `.yaml`/`.json`/`.figl`, never markdown — precisely because prov
  re-lays-out their sorted records and prose has no stable home there.
  `about.md` is *entirely* prose, in the workspace's content format, and prov
  rewrites it whole.

So the proposal adds a sixth row rather than straining an existing one:

| Target | Declared as | Contract |
| --- | --- | --- |
| **Generated prose** | a one-way pointer relation (`about`) | plaintext in the workspace's *content* format; reached from the root only; **no inverse, no `part_of`, no id, not in the spanning tree, not orphan-checked**; rewritten **whole** by prov and never merged; a pure function of configuration, therefore **discardable** — deleting it loses nothing |

That last clause is not decoration. It is the same two-natures distinction
DESIGN §5 draws for the index, and it is what makes the freshness section
below cheap rather than fraught.

### The honest limit: this is orientation, not bootstrap

A circularity has to be admitted plainly, because a reader will notice it and
a proposal that papers over it deserves the skepticism.

`about.md` explains how to read a metadata block. But it is *found* by reading
the root document's metadata block, through a pointer named `about`. So it
cannot bootstrap a parser: anything able to follow the pointer already knew
the thing the file explains.

That is fine, because the reader this is for is a **person**, not a parser. A
person opening the directory sees a file called `about.md` next to `README.md`
and opens it — no pointer traversal, no parsing, no convention beyond a
filename. The pointer exists so *prov* can find the file to regenerate and
validate it, and so the file is reachable rather than loose in the tree; it is
not the reader's way in.

Two consequences follow, both requirements rather than nice-to-haves:

1. **The default filename is load-bearing.** `about.md` at the workspace root,
   beside the root document. The pointer may name any path — this is a prov
   workspace, placement is ergonomic (spec §5) — but the *default* must be the
   most guessable name in the most guessable place, because guessability is
   the actual entry point.
2. **The file must not assume it was reached through the pointer.** It opens
   by naming the root document explicitly ("this directory's root is
   `README.md`") rather than saying "the root you came from."

The floor `docs/spec.md` describes does not disappear here. It moves: from
"you must obtain a specification" down to "you must be able to open a text
file and read English." That is a floor an archive can actually stand on.

### The byline is a warrant

Both sketches opened with a variant of "generated by prov; edits will be
overwritten; change `prov.yaml` instead." The instinct to keep it is right,
and the reason is better than housekeeping: **it is this document's
authorship**, and it makes a claim a human byline cannot.

A person's name asserts *someone said this*, which may be stale, mistaken, or
written years before the workspace drifted out from under it. "Derived from
this workspace's own settings" asserts *this was read off the files
themselves* — which is checkable, and which prov does in fact check (below).
For a reader deciding whether to trust the page, the derived byline is the
stronger of the two.

So it goes where authorship goes — in the metadata block, as a real field:

```yaml
---
title: How this workspace is organized
generated_by: prov 0.1.0
---
```

`generated_by` is prov-maintained, so prov owns its format (DESIGN §2): tool
name and version, nothing else. Deliberately **not** the `author` field — that
is tier-3, user-owned, and prov writing into it would muddy exactly the
boundary §2 exists to draw.

The body then says the same thing once, in a sentence, because "nobody wrote
this; it was read off the files" is worth stating outright to a stranger.

**Split off the other half.** "Edits will be overwritten — change `prov.yaml`
instead" is addressed to today's collaborator, not to the stranger. Same file,
different audience; only the first half is the byline. It belongs in the
footer, alongside the one concession to generality worth making: a single line
naming the scheme and where its specification lived, for the reader who wants
to write a tool rather than read the workspace.

### Operationally complete, generally incomplete

The test for whether a sentence earns its place, stated once so the generator
has a rule rather than a taste:

> The document must be **operationally complete** — a reader can traverse the
> workspace using nothing else — and **generally incomplete** — silent about
> every option this workspace does not exercise.

The stranger has no prov repository either. Whatever they need in order to
*read this directory* must be present. Whatever they would need in order to
*write a general prov tool* must not be, because that is exactly the material
that makes the page feel like someone else's manual.

## Freshness

### Regeneration triggers

Because the content is a function of configuration, exactly one trigger
matters:

- **A config write regenerates it.** `prov config <key> <value>` stages the
  `about.md` rewrite into the same `ChangeSet` as the config edit. Same for
  `prov config --home root|sidecar`, and for `init`, which writes the first
  copy.
- **`prov about` regenerates on demand** — the escape hatch for a file edited,
  deleted, or mangled out of band.
- **Nothing else does.** Ordinary mutations (`new`, `mv`, `rm`, `attach`)
  leave it alone, because nothing they change is described in it.

Riding the config write's `ChangeSet` is a convenience, not a requirement.
DESIGN §5's rule applies directly — "what can be rebuilt need not be
transactional" — so if it complicates the change set, dropping it out costs
only a `check` finding.

### This is not the registry's hotspot problem

DESIGN §5 names the worry precisely, and it is the strongest objection to
putting a generated file in the tree:

> **A single central index file is a merge/write-contention hotspot.** Every
> mutation on every device touches it, so every sync touches it —
> re-concentrating exactly the contention that per-file frontmatter avoids.

That argument is about the **registry**, and it turns on the registry being
authoritative and non-derivable: a conflict there is unrepairable damage,
because the `id → path` mapping was never in the documents to begin with.

`about.md` is the other nature, and both halves dissolve:

- **Write contention** — it is not written on mutation, only on config change.
  A file that changes when configuration changes is not a hotspot under any
  definition. This is the payoff of the generation rule; had corpus facts been
  included, every `new` would have rewritten it and the objection would stand.
- **Merge conflict** — resolved by *regeneration*, never by merge. A Dropbox
  conflicted copy, a Syncthing `.sync-conflict-…`, a git conflict: throw both
  sides away and rebuild from config. Nothing can be lost, because nothing in
  the file is a fact about anything but `prov.yaml`.

For git specifically, ship the recipe rather than making people work it out:

```gitattributes
about.md merge=ours
```

### `check` and `--fix`

The byline claims the page was derived from the workspace. `check` is what
keeps that claim true.

- A new finding, **`AboutStale`**: the file the `about` pointer resolves to
  does not match what prov would generate from the current configuration.
  Detected by generating into memory and comparing.
- **Autofixed** by regeneration — the same shape as the fixity re-stamp
  (`Workspace::restamp_fixity`), and safe for the same reason: the correct
  content is fully determined, so there is no judgment to get wrong. Unlike
  the fixity re-stamp it needs no confirmation gate, because nothing
  user-authored can be destroyed.
- A **missing** `about.md` where the pointer names one is the same finding. A
  workspace with `about: off` and no pointer is silent — not a finding.

This is also the pre-ship gate. "Run `check` before handing the workspace to
someone" already earns its keep; this adds one more thing it guarantees.

## Config axis

```yaml
about: structure
```

| value | the document describes | rewritten when |
| --- | --- | --- |
| `off` | — (no file, no pointer) | never |
| `structure` *(default)* | the root and the spine; how a file is fenced; how a reference is written and what else is read; the relation vocabulary; what is machinery and not in the tree; the id, checksum, and deletion conventions | configuration changes |

An enum at two values rather than a bool, for three reasons: `about: structure`
self-describes better than `about: true` in a file a stranger may read; it
matches the house pattern (`fixity: off | attachments | all`,
`identity: none | lazy | eager`); and it leaves room for a third value without
a breaking reshape.

**Default `structure`, not `off`** — unlike `history`, which defaults off
because its audience is narrow. This one costs a few hundred bytes and one
file, and it is the direct expression of DESIGN §1. A workspace that explains
itself to a stranger *by default* is the entire thesis; making it opt-in
concedes it.

The value that was cut, recorded here rather than shipped: an `all` that
additionally reported corpus facts — file counts, which formats and spellings
actually occur. The counts rot on every mutation to tell a reader what they
can see by looking, and the "what actually occurs" half is better served by
describing what prov will *read*
([above](#specialize-on-config-and-on-what-prov-will-read--never-on-the-corpus)).
Nothing here forecloses it if a real need appears.

## CLI surface

```
prov about [--check] [--print]
```

- bare — regenerate `about.md` from the current configuration and write it,
  creating the root's `about` pointer if absent.
- `--check` — exit non-zero if the file is stale or missing, printing a diff.
  For CI in a repository that wants the file guaranteed current.
- `--print` — write the generated document to stdout, touching nothing.

`init` writes the first copy as part of the initial `ChangeSet`, so the very
first workspace a user makes already explains itself.

## What this forces in `docs/spec.md`

Generating the document is what exposes these. Both are latent conflicts
today; neither is caused by this proposal.

### Root discovery disagrees with itself

The spec and the first sketch state different rules:

| source | rule |
| --- | --- |
| `docs/spec.md` §1 rule 1 | the file named by a one-line `.prov` pointer if present, else the first of `README.md`, `readme.md`, `index.md` that exists |
| `examples/prov-1.draft.md` (pre-revision) | a file with a metadata block declaring no `part_of`; ties broken toward stem `index`, then `readme`; ambiguity is a stop condition |

These are not two spellings of one rule. The spec's is a *filename* rule with
an invariant in parentheses; the sketch's is a *property* rule with filenames
as tie-breaks — and they disagree on precedence between `index` and `readme`.

**Recommendation: the spec's rule stands, and the generated document states
neither.** It states the answer: "this directory's root is `README.md`." The
procedure is only interesting to a tool that must find the root without being
told, which is precisely the reader `about.md` is not written for. The
sketch's `part_of`-based rule should be dropped rather than reconciled — it
requires parsing every file in the directory to find one, which is the scan
DESIGN §8 spent real design effort eliminating everywhere else.

### Flat versus `prov:`-nested keys

`docs/spec.md` §2 shows the vocabulary nested under a `prov:` key
(`prov.spec`, `prov.relations`, `prov.spanning`), and rule 3 says to read
`prov.spec`. This repository's own `prov.yaml` spells them flat (`spec: 1`,
`spanning: contents`, `relations:`), and both sketches quote the flat form.

Both are correct, and `docs/config-vocab.md` explains why — the two homes
(nested under `prov:` in the root's frontmatter, top-level in the dedicated
config document) are the same vocabulary in two places, resolved *config doc >
root block > default*. The spec is simply written as though only the first home
exists.

**Recommendation: spec §1 rule 3 and §2 name both homes explicitly.** A
documentation fix, not a design change — but it must be settled *before* the
generator ships, since the generated text has to describe whichever home the
workspace uses, and getting it wrong writes a false statement into every
workspace.

## Relationship to history's manifests

Worth stating because the question recurs, and because it nearly pulled this
proposal off course: should `about` list the files?

No. That is a manifest, and prov already has one. Every history event records
"one entry per file, sorted by path, each carrying the path, the content hash,
and — when the document is registered — its id" (`proposal-history-v3.md`;
Phase 0 has shipped — `history_relation`, `Workspace::history_path`,
`HistoryCapture`, `HistoryList`). An `about` that enumerated files would be a
second implementation of that.

The division holds cleanly:

| | answers | derived from | changes when |
| --- | --- | --- | --- |
| `about` | *how do I read this?* | configuration | configuration changes |
| history manifest | *what is here, and are the bytes intact?* | the corpus | every capture |

They meet at exactly one point: when `history` is enabled, `about` gains a
sentence noting that a per-file record with checksums lives in `history/`, and
where. A pointer, not a copy — the same relationship it already has to
`registry`, `config`, and `recycle_bin`. That sentence is config-derived like
everything else, so it costs the freshness story nothing.

## Rejected / non-goals

- **Vendoring `docs/spec.md` into every workspace.** The whole argument; see
  [The move](#the-move-specialize-dont-vendor). A copy is not self-hosted, it
  is the spec filed nearby, and it hands the reader branches they can neither
  use nor evaluate.
- **A machine-readable kernel** — a formal grammar or schema in the workspace
  that a foreign *tool* could bootstrap from, rather than prose a person
  reads. Genuinely interesting, and the widest reading of the title, but it
  answers a different question (how does an unfamiliar tool parse this?) for a
  different audience, and would be a strictly larger proposal built on this
  one. Nothing here forecloses it: the `about` pointer and the generated-prose
  row are the same seam a machine-readable sibling would hang off.
- **Corpus statistics** (file counts, average lengths, format censuses). Rots
  on every mutation; tells the reader what they can see by looking; and it is
  what would have made the file a sync hotspot. See the
  [config axis](#config-axis).
- **Listing the files.** A manifest, not a description of organization, and
  [history's job](#relationship-to-historys-manifests).
- **Making it a content node** (`part_of` back to the root, an id, a place in
  `prov tree`). It is not the user's writing, and putting it in the spanning
  tree would make `prov tree` lie about what the workspace contains.
- **Merging it on conflict.** Never. Regenerate. Merging a derived file is
  work performed to produce, at best, what regeneration would have produced
  anyway.
- **A `--format` flag** (emit the same content as JSON/YAML for tooling). The
  audience is a human reading prose; a machine should read `prov.yaml`, where
  the facts actually live. This is the machine-readable-kernel question
  wearing a smaller hat.
- **Localization.** Real, and a genuine gap for a 100-year artifact written
  only in English — but it is a translation-infrastructure project, not a
  generator feature, and English-only is honest rather than pretending
  otherwise.
- **Regenerating on every mutation.** Considered and dropped once the
  generation rule made it pointless: no ordinary mutation changes anything the
  document says.

## What implementation settled

Recorded because each was decided *by building it*, and two of them contradict
what this proposal assumed.

**The metadata block follows the workspace's carrier — including having none.**
The page's own block is written in `metadata.format` and embedded per
`metadata.embed`, so it never contradicts the sentence it contains. Under
`metadata.embed: separate`, where no file in the workspace carries a fence, the
page is written **content-only**: no block, and no sidecar either. A two-file
`about` is worse for the reader it exists for, and the sidecar would carry
nothing prov reads back. The `title` survives as the `# ` heading and the byline
as the footer.

**Staleness compares the body only.** This is what makes content-only mode work
— there is no block to compare — and it retires a problem the proposal did not
see: with the block included, `generated_by: prov 0.3.2` would make *every
workspace on earth* stale on a version bump, firing `check` everywhere and
rewriting files whose prose is identical. A byline naming an older version is
harmless. Hand-reflowing the prose *does* read as stale, correctly: the page says
its edits are overwritten, and regenerating is free.

**Pointer bootstrap is a regeneration trigger too.** The proposal's "a config
write, and nothing else" was wrong, because the page describes the machinery the
root points at — and those pointers are created lazily by ordinary mutations: the
first id minted writes the registry, the first delete writes the recycle bin, the
first capture writes the history store. Each of those now refreshes the page. The
freshness story survives intact, because the refresh **writes only when the page
would actually change**, so it is a no-op on every subsequent mutation and the
hotspot objection still does not apply.

**`history-capture` does not capture the page.** It is a pure function of the
config the same manifest already records, so parking its bytes stores nothing
that cannot be reproduced, and a new blob would be parked on every config change
for no recovery value. Excluding it also removes an ordering hazard: the first
capture bootstraps the store, which changes what the page says, so a captured
page would be one the capture itself invalidated.

**An absent page is not a broken link.** The `about` pointer resolving to nothing
is reported as `AboutStale`, never as `BrokenLink`. The page is derived and
discardable, so an absent one is a page waiting to be written rather than a
reference to something lost — and a generic broken-link fix would invite the
wrong repair.

**Worked examples are generated, never written.** Both attached examples are
`prov about` output copied verbatim, which is the only real check on prose
quality. The generator's samples use the workspace's *own* vocabulary — a page
for a `sections`/`section_of` workspace shows `section_of`, never the diaryx
default — because a sample showing a key the reader's files do not use teaches
them the wrong key.

**The open questions, as resolved.** (1) The body format is described always, in
one clause, rather than conditionally on being non-default — which dissolves the
wart rather than answering it. (2) The effective vocabulary is stated as fact,
per the proposal's lean. (3) `--check` was kept: phase 0 shipped before
`AboutStale` existed, so it was the only staleness detector, and by the time the
finding landed the flag was already there. (4) Nested workspaces remain out of
scope.

## Phasing

- **Phase 0 — the generator. Shipped.** The `about` relation, `Workspace::about_path`,
  the `about: off | structure` axis, `prov about [--check] [--print]`,
  generation from `WorkspaceConfig`, and `init` writing the first copy.
  Regeneration wired into `prov config`'s `ChangeSet`. The generated-prose row
  added to `docs/spec.md` §4, and the two spec reconciliations
  [above](#what-this-forces-in-docsspecmd) settled first — the text is written
  into every workspace, so it must not be written wrong.
- **Phase 1 — validation. Shipped.** `AboutStale` and its autofix; `prov check` reports
  it, `--fix` regenerates. The `.gitattributes` recipe documented.
- **Deliberately unscheduled:** the machine-readable kernel; an `about: all`
  with corpus facts; localization. Each is correct to defer, and none changes
  the pointer relation, the axis spelling, or the typology row — so none is
  foreclosed.

## Open questions

1. **Does `about` describe the *body* format at all?** This workspace is
   Markdown (`content_format: markdown`), which a reader will infer from the
   files. But a Djot or HTML workspace is less obvious, and a stranger who
   assumes Markdown will misread it. Probably one sentence, generated only when
   `content_format` is not the default — but that makes the document's content
   depend on the default, which is a small wart.
2. **What does the file say when the workspace declares no vocabulary at all?**
   `RelationSet::from_config` falls back to the diaryx preset, so the *facts*
   are known — but the workspace has not *declared* them, and a future prov
   could change its default. Does `about` state the preset as fact (accurate
   today, potentially a lie later), or say "not declared; the tool's built-in
   vocabulary applies" (accurate forever, useless to a stranger)? Leaning
   toward stating it as fact and letting `AboutStale` catch the drift — but the
   drift is only caught by a prov new enough to have changed.
3. **Should `prov about --check` be part of `check` proper, or stay separate?**
   Phase 1 has `check` report `AboutStale`, which makes `--check` redundant
   except for exit-code granularity in CI. Possibly drop the flag.
4. **Does a nested prov workspace's `about` mention its parent?** A workspace
   inside a larger repository is invisible to the outer one by §8's
   reachability rule, and vice versa. A stranger opening the outer directory
   may never learn the inner one exists. Out of scope here, but the
   self-description gap is real and this proposal does not close it.
