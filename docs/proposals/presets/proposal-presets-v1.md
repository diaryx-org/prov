---
title: presets
author: adammharris
created: 2026-09-10
updated: 2026-09-10
status: draft
part_of: '[`prov` proposals](/docs/proposals/proposals.md)'
---
# Presets — a common setup, written out rather than named

## Summary

A **preset** is a named bundle of the configuration a common kind of workspace
needs — the vocabularies, the fields, the views, the glosses — that `prov init`
can write for you. The first is `tasks`: a `status` vocabulary, the views that
answer "what is open", and the sentences `about.md` should say about it.

The proposal is one decision and one rule.

1. **A preset is a stencil, not a default.** It is written out, in full, into
   the workspace's own config, and prov's reader never learns the preset's name.
   The alternative — `preset: tasks` in `prov.yaml`, with absent axes filled
   from a bundle prov ships — is rejected, for the reason the self-hosted-kernel
   proposal already gave: the workspace carries its own reading instructions.
2. **A preset is a configuration of mechanisms prov already has.** Nothing goes
   into prov that only one preset exercises. When a preset needs something the
   config vocabulary cannot say, the question is whether that thing is a general
   mechanism; if it is, it is added as one, and if it is not, the preset does
   without.

What this proposal does **not** do: no `preset:` key that a reader has to expand;
no preset that a workspace can only partly take; no field declaration scoped to a
subtree, which is the one shape two presets in one workspace would eventually
want and which §6 defers.

## Why now

Three setups are being written by hand, and each is the same twenty lines.

**A task tracker.** The org keeps deferred work as documents — `docs/tasks/` for
a commitment with a done state, `docs/proposals/` for an argument that may
lose — with `status` in frontmatter so that a tool can read it. The tool is
`dx tasks`, and today it carries the whole model itself: which directories, which
statuses are live and which closed, how to tell an item from an example beside
it, in what order to sort. prov is asked only for one document's frontmatter at
a time. Every line of that model is something prov's config vocabulary can
already say, and saying it in the workspace rather than in `dx` is what lets
`prov check` catch `status: wontfix` and `prov views open-tasks` answer the
question without a nu script in the loop. The experiment that shows this is in
§4.

**An archive.** diaryx's format is prov's default vocabulary plus an `audience`
field with a closed vocabulary, a date chain for the daily view, and an exports
gate. diaryx writes that config itself at setup. If the format is meant to be
readable by a tool that is not diaryx — and a self-describing archive is the
whole point of prov — then the bundle that writes it belongs where the format is
public, beside `tasks`.

**A plain notes directory.** The minimal workspace, which today is `init`'s
defaults. It is already a preset in everything but name, and naming it is what
makes the other two not special.

The pull toward convenience is real: a task tracker that needs a vocabulary file
and four view declarations before `prov views` prints anything is one nobody
sets up twice. The question is only where the convenience is paid for, and it
turns out that paying it once, at `init`, costs almost nothing of what the
self-hosting rule protects.

## 1. What is true today

**prov has exactly one built-in default, and it is unnamed.** Absent `spanning`
and `relations` mean the diaryx vocabulary — `contents`/`part_of`,
`links`/`link_of` — and the spec states that in one sentence
([config vocabulary](/docs/config-vocab.md), "Relation definitions overlay the
built-in vocabulary"). It is defensible precisely because it is singular: a
reader who knows prov's spec knows what an absent axis means, and there is no
second thing it could mean.

**Everything else is spelled out, and `about.md` is derived from it.** The
generated page is "the spec specialized against this configuration" — every
rule resolved to a concrete fact, and *never* from a scan of the files
([config vocabulary](/docs/config-vocab.md), "`about`"). It already writes a
"Fields with fixed vocabularies" section and reads each relation's `means:`
gloss and each vocabulary term's. So whatever a preset writes into the config is
what `about.md` can then explain to a stranger; whatever a preset kept to itself
would be invisible there.

**The self-hosted-kernel proposal settled the direction.** The workspace carries
its own reading instructions; a token legible only to the app that wrote it is
the failure mode. Promoting diaryx's `diaryx.views` block to a top-level `views:`
axis was that argument applied to views, and a `preset:` key would be exactly
the block it removed, in a new coat.

## 2. Two shapes, and why the first

Take the tasks case. The bundle is a `status` vocabulary, a `fields` entry
pointing at it, a view or two, and the glosses. There are two places it can
live once a workspace uses it.

**A stencil.** `prov init --preset tasks` writes the bundle out — into
`prov.yaml` and `vocab/statuses.yaml`, in full — and stops. From then on the
workspace is ordinary, fully spelled-out config. prov's reader does not know the
word `tasks`; the preset was a writer-side convenience and left no trace but the
config it wrote.

**A default.** `prov.yaml` says `preset: tasks` and prov fills every absent axis
from a bundle it ships, at read time. Shorter files, and a fix to the bundle
reaches every workspace on the next upgrade.

The stencil is right, on three counts.

**Self-hosting.** A reader in twenty years with no prov binary can still tell
from a stencilled workspace that `status: dropped` was one of four closed terms,
because the vocabulary file says so. From `preset: tasks` they can tell nothing.
This is the kernel proposal's argument and it is not weaker for a preset than it
was for `diaryx.views`.

**Names are format versions.** A default's name becomes something prov must
carry forever with a fixed meaning: `tasks` can never change without breaking
every archive that named it, the same rule as a crates.io version number. A
stencil has no such debt — an archive written with last year's stencil is
exactly as readable as one written with this year's, because both are fully
stated. The one built-in default prov has survives this test only because it is
singular and unnamed; a family of named defaults is a registry.

**What is given up is correct to give up.** A default's one advantage is that
the bundle is upgradeable. But an archive's format is the archive's, not the
tool's; a workspace that wants this year's `tasks` stencil can re-run it and
diff, which is a choice its author makes, rather than a change that arrives with
a binary.

The convenience is the same either way — one command — so the trade-off between
self-hosting and convenience is paid almost entirely at `init`, and not at all
on read.

## 3. The rule that falls out

If a preset is a stencil, then a preset can only ever be a **configuration of
mechanisms prov already has**. It cannot ask prov's reader to behave differently
for it, because the reader does not know it is there.

That gives a test for every feature a preset seems to want. Either the thing is
a *mechanism* — generic, every preset would use it, it goes into prov's
vocabulary and code with its own reason — or it is *stencil* — config, a
vocabulary file, a gloss, a fixture — or it does not belong. Nothing may enter
prov that only one preset exercises.

Applied to what the tasks experiment (§4) found missing:

| the preset wants | it is a | because |
| --- | --- | --- |
| `updated` stamped when `prov set` closes a task | mechanism | `edit` already stamps it through `record_content_update`; `set` and `unset` are bare text rewrites (`prov-cli/src/main.rs`, `cmd_set`). Every workspace with an `updated:` axis wants this |
| `created` stamped when `prov new` opens one | mechanism | prov has the clock and the document does not; the `updated:` axis is the pattern, and `created:` is its sibling |
| a value at creation — `status: open` on a new task | mechanism | either `new --set key=value`, or a `fields.<name>.default:` that `new` writes; the second is a declaration the stencil can carry |
| `prov views <name> --json` | mechanism | `check --json` and `meta --format json` exist; a consumer replacing its own loop with a view needs the same |
| `sort:` on a view — newest `updated` first | mechanism, already deferred | DESIGN names it as the place the view format next grows teeth |
| the nine `status` terms, the `open-tasks` view, "a task is a commitment with a done state" | stencil | config, a vocabulary file with `means:` glosses, and the fixture that pins them |
| `status` meaning one thing under Tasks and another under Proposals | neither, yet | §6 |

## 4. The `tasks` preset, concretely

The experiment: copy this repository's documents to a scratch directory, add the
following to `prov.yaml`, add the vocabulary file, and run `check` and `views`.
No code changed.

```yaml
# prov.yaml — appended
fields:
  status:
    values: closed
    vocabulary: '[Statuses](/vocab/statuses.yaml)'
  created: { type: date }
  updated: { type: date }
views:
  open-tasks:
    label: Open tasks
    group: status
    under: '[Tasks](/docs/tasks/tasks.md)'
    where:
      not: { equals: { status: [done, dropped] } }
  proposals:
    label: Proposals
    group: status
    under: '[proposals](/docs/proposals/proposals.md)'
    where: { has: status }
  work:
    label: All work
    group: status
    where: { has: status }
```

```yaml
# vocab/statuses.yaml
title: Statuses
vocabulary:
  field: status
  values: closed
terms:
  open:        { means: "committed to, not started" }
  in-progress: { means: "someone is on it" }
  done:        { means: "resolved; the closing edit names the commit or release" }
  dropped:     { means: "will not be done; the file stays, findable by grep" }
  draft:       { means: "a proposal still being argued" }
  accepted:    { means: "argued and agreed, not yet built" }
  implemented: { means: "built; the Status section names the release" }
  deferred:    { means: "agreed in principle, not scheduled" }
  rejected:    { means: "argued and lost; the reasoning is kept" }
```

What that bought, with the binary as it is:

- `prov views open-tasks` prints the open tasks grouped by status;
  `views proposals` the proposals; `views work` every document in the
  repository that carries a `status`. That is the whole of `dx tasks`'
  `KINDS` table, its `live?` and `item?` predicates and its sort, stated in
  the workspace.
- `prov check` reports `status: wontfix` as "not a known term in this closed
  vocabulary".
- `prov set <task> status done` closes one; `prov new --in docs/tasks/tasks.md
  "<title>"` opens one, linked both ways.
- `where: { has: status }` is what keeps the example documents a proposal
  carries beside it out of the proposals view — the same distinction `dx`
  draws with `item?`, and one line instead of a function.

What it did not buy is the table in §3: the new task had no `created` and no
`status`, and closing the old one left its `updated` at the day it was written.

**One thing the model changes.** The org's rule says a closed task "leaves the
index", and this repository's `tasks.md` says "`contents` above lists what is
open". In a prov workspace that is a `check` finding — removing a done task from
`contents` makes it unreachable, and the proposals index already says so ("a
resolved proposal keeps its place in the list above, because a document nothing
reaches is a `check` finding"). Under this proposal the index's `contents` is
the spine and lists everything; the **view** is what lists what is open. The
rule and the prose change to say that; no code does.

**Where the preset lives in the repository.** As a fixture: a directory prov's
own tests initialize with `--preset tasks` and run `check` over, so that the
stencil is verified by the same reader it is written for and cannot drift from
the vocabulary it uses. This repository is also its first consumer — the config
above, committed here, is the dogfood.

## 5. What a preset contains, and what `about.md` says

A preset writes three things and no fourth.

- **Config**: `fields`, `views`, `exports`, and where the preset is a whole
  format rather than a corner, `relations` — into the workspace's config
  document, merged with what is there.
- **Stores**: the vocabulary files the `fields` entries point at, each term with
  a `means:`.
- **Glosses**: the `means:` on every relation and term the preset declares, so
  that `about.md` — which is derived from configuration and nothing else — can
  say "a task's `status` is one of `open`, `in-progress`, `done`, `dropped`"
  and what each means.

The third is the self-hosting dividend the stencil pays. A `preset:` key would
give the stranger a name; the written-out config gives them the explanation. One
small mechanism follows: `about.md` says nothing about views today, and a
workspace whose whole point is a view (`open-tasks`) should be introduced by it.
A `views` section beside the fields section, listing each view's `label` and
scope, is the generic thing; whether views take a `means:` gloss is open
question 3.

**Merging is additive and refuses collisions.** Applying a preset to a workspace
that already has config adds entries and never rewrites one: a `fields.status`
already present is a collision the command reports and stops on, because the two
declarations mean different things and prov cannot choose. This is the
conservative half of §6 — it makes two presets in one workspace *possible* when
their names do not collide and *loud* when they do.

## 6. The edge: two presets in one workspace

This repository is a docs workspace with a tasks corner and a proposals corner,
and the org's rule has both use `status` with disjoint vocabularies. A `fields`
entry is keyed by field name and is workspace-global, so the one closed
vocabulary has to admit all nine terms, and `check` cannot say that `status:
open` on a proposal is wrong.

The general shape of that is **a field declaration scoped to a subtree** —
"under Tasks, `status` means these four; under Proposals, those five" —
something like `fields.<name>.under:`, resolved by the same spanning traversal
`views.<name>.under:` uses. It is a real spec change: field declarations stop
being a property of the workspace and become a property of a region of it, and
every reader of `fields` — `check`, `about`, a metadata editor's picker — has to
learn to ask *where* before asking *what*.

This proposal defers it. The one collision in hand has a cheap fix — proposals
carry `outcome:` rather than `status:`, a one-word change to the org rule and to
`dx tasks` — and a scoping axis should wait for a second concrete case that the
rename does not cover. The `tasks` preset is written so that it does not need
the axis: one vocabulary, nine terms, and the views tell tasks from proposals by
`under:`, which already scopes.

## 7. Where the diaryx preset lives

The archive format's bundle — `audience` with its closed vocabulary and `reify`,
the `[date_of_document, created]` chain, the `daily` view, the exports gate — is
today written by diaryx at setup. If the format is meant to be read by tools
other than diaryx, the bundle is a public fact about a public format and belongs
here, as `--preset diaryx`, with diaryx's setup calling it. If it is not, diaryx
keeps writing its own config and prov ships `tasks` and `notes` only.

That is a boundary decision — moving the bundle from the private repository to
this one is a one-way publication — and this proposal does not make it. The
mechanism is the same either way.

## 8. Staging

- **Phase 0 — the mechanisms.** In their own commits, each useful without a
  preset existing: `set`/`unset` go through `record_content_update` when a
  workspace is found around the file (and stay workspace-less when not, which
  `dx tasks` depends on); a `created:` axis stamped by `new`; a value at
  creation (`new --set` or `fields.<name>.default:`, open question 2);
  `views <name> --json`; `about.md` lists views.
- **Phase 1 — the stencil.** `prov presets` lists what prov can write; `prov
  presets <name>` prints it, touching nothing, in the shape `exports <name>`
  previews; `prov presets <name> --write` applies it, additively, refusing a
  collision; `init --preset <name>` is the shorthand at creation. `tasks` and
  `notes`, as fixtures the test suite initializes and checks.
- **Phase 2 — this repository takes it.** The `tasks` config committed here;
  `tasks.md`'s prose and the org rule corrected so that `contents` is the spine
  and the view is the open list; the proposals index says the same thing it
  already does.
- **Phase 3 — `dx tasks` reads a view.** Per repository, `prov views work
  --json` replaces the per-file `prov meta` loop and the `KINDS` table, in
  repositories that are prov workspaces. Devtools work, after the above ships.
- **Unscheduled** — `sort:` on views, and the subtree-scoped field declaration
  of §6, each waiting on a second case.

## Open questions

1. **The verb.** `presets` follows `views` and `exports` (list with no name,
   act with one), and `--write` is proposed because `--apply` reads as if it
   might do more than write config. `adopt` is taken, by `init --adopt`, and
   means something else.
2. **A value at creation: flag or declaration?** `new --set status=open` puts
   the knowledge in the caller's hands; `fields.status.default: open` puts it in
   the workspace, where a stencil can carry it and `about.md` can state it. The
   second is proposed, with the flag as the override; the cost is that `default`
   is a key every reader of `fields` has to at least carry.
3. **A `means:` gloss on views and fields.** Relations and terms have one;
   fields and views do not. `about.md` can list a view from its `label` and
   scope alone, so the gloss is a nicety rather than a mechanism — but it is the
   one place a preset's author can say *why* a view exists, and that sentence is
   the thing a stranger most wants.
4. **Whether `notes` is a preset or just `init`.** Naming the minimal workspace
   makes `tasks` and `diaryx` ordinary rather than special, but a preset that
   writes nothing is odd. Proposed: it exists, it writes the `created:` and
   `updated:` axes and nothing else, and `init` with no `--preset` is `--preset
   notes`.
