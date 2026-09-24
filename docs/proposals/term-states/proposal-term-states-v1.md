---
title: term states, following views, and shelves
created: 2026-09-24
updated: 2026-09-24
status: draft
part_of: '[Proposals](/docs/proposals/proposals.md)'
---
# Term states, following views, and shelves — the rest of the tasks model

## Status

**Draft.** Argued 2026-09-24. Nothing here is built. The four open questions
are decided, and the body says so where it had asked them: no reserved state
names; grain ordering is left to a proposal of its own; `deferred` is a closed
state; and `state` holds on an open vocabulary as on a closed one.

## Summary

The [presets](/docs/proposals/presets/proposal-presets-v1.md) proposal moved
the tasks model into prov's configuration: the two `status` vocabularies, each
scoped to its index, the `open-tasks`, `proposals` and `work` views, and the
starting value `new` writes. What it left behind is small and specific, and
it is still kept outside prov, by the org's `dx`: which terms mean *finished*,
the listing across every workspace, and filing a closed item on a shelf. This
proposal brings those three in, as general mechanisms, and is three decisions.

1. **A vocabulary may group its terms into declared states.** A `states:`
   block names them; a term names at most one with `state:`. It is a machine-
   readable fact about a term, beside the `means:` gloss prov carries and never
   reads. prov gives no state name a meaning of its own; whatever uses a state
   names it. A view reads states with an `in-state:` condition and a `state`
   grain.
2. **`prov views <name> --follow` runs each reached workspace's own view of
   that name**, grouped by workspace, the way `check --follow` runs each one's
   own check. No view definition crosses a boundary, only the reader.
3. **A shelf is declared, and `prov shelve` files onto it.** A `shelves:`
   entry says which documents under which index move under which other one;
   the verb moves them with the machinery `mv --in` already uses, inside one
   workspace and never across.

What this does **not** do: no state name prov interprets by itself; no
expression language in `where:`; no write across a boundary; no rewriting of
another workspace's prose when a document moves, which the answer to is a
reference by `id:`, not a better rewriter.

## Why now

`dx tasks` reads `prov views work --json` in every repository that is a prov
workspace — the presets proposal's Phase 3 — and still carries four pieces of
the model itself:

- **Which terms are closed.** A table naming `done`, `dropped`,
  `implemented` and `rejected`, beside the vocabularies that already list
  them. The `open-tasks` view carries a third copy, as a `not: { any-of: … }`
  over the two task terms. Add `wontfix` to the task vocabulary and `check`
  accepts it at once, while the view goes on listing a `wontfix` task as open,
  and nothing says so.
- **In what order to list them.** A rank, because groups sort ascending by key
  and `accepted` sorts before `draft`.
- **Where every workspace is.** A manifest walk, one `prov views` per
  repository, where prov's peer map and `descend` already know the answer.
- **Filing what is closed.** A shelf directory and index made on first use,
  `prov mv --in` per item, and a pass over every other checkout rewriting the
  paths that named the old place.

Each of the four is a thing a workspace could say about itself, and one said
by a tool outside the workspace is one a stranger reading the archive cannot
see. That is the self-hosting argument the presets proposal already made, for
the part of the model it did not reach.

## 1. What is true today

**A term has three keys prov reads.** The spec's vocabulary file (§3): prov
reasons about the term keys, each term's `id`, and `retired`; every other key —
`means`, a diaryx audience's `gate` — is payload it carries untouched.
`retired` is the precedent for a flag prov acts on, and it describes the term
itself: known, and no longer valid for new content.

**`where:` has two predicates and three combinators, on purpose.** `has`,
`equals`, `not`, `any-of`, `all-of`, and a rule for adding a sixth: a concrete
lens that cannot otherwise be said. Grains have the same rule.

**`check --follow` and `tree --follow` cross boundaries by reading.** Each
workspace the origin reaches through `descend` is opened as if the command had
been run inside it, and reported grouped, because a path in one workspace's
terms means nothing in another's. Nothing crosses on write.

**This repository's tasks index files closed tasks on a shelf.**
`docs/tasks/closed/closed.md`, titled `Closed tasks`, is `part_of` `Tasks`, so
a closed task stays under `Tasks` for its vocabulary and every view, while
`Tasks`' own `contents` lists what is live. `dx shelve` made it.

## 2. States

### The declaration

A vocabulary may declare its states, and a term may name one:

```yaml
# vocab/task-statuses.yaml
title: Task statuses
vocabulary: { field: status, values: closed }
states:
  live:   { means: "still to be picked up" }
  closed: { means: "finished; the closing edit names what resolved it" }
terms:
  open:        { means: "committed to, not started",   state: live }
  in-progress: { means: "someone is on it",            state: live }
  done:        { means: "resolved; …",                 state: closed }
  dropped:     { means: "will not be done; …",         state: closed }
```

- **Declared, so it can be checked.** A term naming a state the vocabulary
  does not declare is a finding, with the near-miss prov already computes for
  terms: `state: closd` is caught, not read as a fourth state.
- **At most one per term.** States partition the terms, which is what makes a
  state a grain (below). A term wanting several labels wants tags, which is
  another feature.
- **None is allowed.** A term with no `state`, or a value that is no term, is
  in no state. `not: { in-state: { status: closed } }` therefore keeps it —
  the direction that shows an unconverted document rather than hiding it.
- **Opt-in by declaring.** `state` is read only in a vocabulary that has a
  `states:` block. Without one it stays payload, as it is today, so a
  vocabulary already carrying a `state:` key of its own means nothing new.
- **Reified vocabularies too.** A reified vocabulary's terms are nodes; its
  index node carries `states:`, and each term node's `state` field names one.

### Why a state and not a flag

The first draft of this was `closed: true` on a term. It answers the one
question every consumer asks today, and cannot be misspelled. A declared
state answers the same question and more: a vocabulary can say *live* and
*closed*, or *proposed*, *active* and *settled*, and a view can group by
the answer. The cost of that generality — a name a tool must agree on — is
paid by the checking above and by §2's rule on meaning, below.

### What a state means: nothing, to prov

prov gives no state name a meaning. A view that filters on `closed`, a shelf
that files what is `closed`, and a tool that reads the `tasks` preset all
name the state they mean, and the preset is where the convention lives: its
two vocabularies both call their finished terms `closed`. A tool that follows
the preset reads that name; a workspace that did not take the preset has not
promised it.

The alternative — a handful of reserved names prov defines, `closed` among
them — lets a tool act on any workspace without knowing its preset. It is
rejected (open question 1): a name prov defines is a meaning prov imposes on
every vocabulary, and the reader of a state is always something that can
name the one it means.

### Reading states in a view

**A condition, `in-state`.** `in-state: { status: closed }` matches a document
whose `status` is a term in the `closed` state — any element of it, for a
list, as `equals` does.

```yaml
views:
  open-tasks:
    label: Open tasks
    group: status
    under: '[[Tasks]]'
    where:
      has: status
      not: { in-state: { status: closed } }
```

By the rule for predicates this has to be a lens that cannot otherwise be
said, and strictly it can: a closed vocabulary is a finite list, and `not:
{ any-of: [equals …] }` over its closed terms selects the same documents.
What cannot otherwise be said is *durably*: the list in the view is a second
copy of a fact the vocabulary owns, and it goes stale the day a term is added.
`in-state` is that list, derived.

It is resolved when the view is loaded, into exactly that `any-of` over the
terms the field's declarations put in the state, so `Condition::matches` keeps
seeing metadata alone. A field declared several times, `under:` several
indexes, contributes the union — correct as long as one term name does not
sit in different states under different indexes, and `check` reports it when
it does (`StateConflict`). A state no declaration of the field names is a view
issue, as an unreadable `where:` is.

**A grain, `state`.** A grain is a many-to-one function from a value to a
group key; a term's state is one. `group: status` with `by: state` groups
tasks into `live` and `closed` instead of four statuses. This one is a lens
that cannot otherwise be said: no condition turns four groups into two.

Its keys sort as every grain's do, ascending, which puts `closed` before
`live`. Sorting them in the order the vocabulary declares them is grain-aware
ordering — the deferred `sort:` axis under another name, which the grains
section already says a new grain must not smuggle in. It is not done here
(open question 2); `sort:` is a proposal of its own, and this grain is one
of its cases.

## 3. `views --follow`

`prov views <name> --follow [depth]` executes the named view in the origin and
in every workspace `descend` reaches from it, and prints each result under its
workspace's header, as `check --follow` does. With `--json`, an array of
`{ workspace, declares, root_dir, view }`, the `view` being what `views <name>
--json` prints for that workspace.

**Each workspace's own view.** The origin's definition is not applied to a
peer's documents. It could not be, soundly: its `under: '[[Tasks]]'` and its
`in-state` resolve against the origin's config and vocabularies, and a peer's
`Tasks`, `status` and `closed` are the peer's to define. So the name is the
only thing that crosses, and a peer that declares no view by that name is
narrated — "`fig` declares no view `open-tasks`" — rather than failed, as a
refused boundary is.

**Read-only**, in every peer.

With this, "every open task and unresolved proposal in the org" is one command
from any root whose containment reaches the rest by foreign reference.

## 4. Shelves

### The declaration

```yaml
shelves:
  closed-tasks:
    under: '[[Tasks]]'
    when: { in-state: { status: closed } }
    into: '[[Closed tasks]]'
```

`under` and `into` are anchors, resolved as a view's `under:` is; `when` is a
`where:` condition. A document in `under`'s spanning subtree, and not already
in `into`'s, that matches `when` belongs on the shelf.

### The verb

`prov shelve [name] [--dry-run]` moves every document that belongs on a
declared shelf, or on the one named. Each move is the one `mv --in` makes:
the file goes into the shelf's directory, is reparented under the shelf, and
every inbound link prov can read follows it. If `into` resolves to nothing,
the shelf index is made first — `new <title> --in <under>`, beside `under`'s
directory in a `closed/` of its own — and reported. `--dry-run` says what
would move and moves nothing.

It moves and does not decide: nothing's `status` is written, and a document
is shelved because it already says it is closed. So it can run in the commit
that closed the task, or later.

### What it does not do

**It does not cross a boundary.** Filing a peer's tasks is the peer's to do.
Across many workspaces, it is run in each.

**It does not rewrite prose it cannot read as a link.** `dx shelve` rewrites a
moved task's path wherever another checkout spells it — `prov/docs/tasks/x.md`
in a sibling's prose, a GitHub URL — because those are path mentions, and a
path moves. The answer prov already has is a reference that does not: an
`id:` link, local or foreign, survives the move with nothing to rewrite. So
the `tasks` preset's guidance becomes *link a task from elsewhere by `id:`*,
and the rewriting pass is not ported.

**It is not a finding.** A closed task not yet on its shelf breaks nothing
`check` exists to report. If a list of them is wanted, it is a view.

## 5. The `tasks` preset, after

- Both vocabularies declare `live` and `closed`, and every term names its
  state. For tasks, `open` and `in-progress` are live and `done` and
  `dropped` closed. For proposals, `draft` and `accepted` are live, and
  `implemented`, `rejected` and `deferred` closed — a deferred proposal is
  not open work, and `in-state: { status: closed }` beside `equals: {
  status: deferred }` finds it again (open question 3).
- `open-tasks` filters `not: { in-state: { status: closed } }`, and lists no
  term by name.
- A `closed-tasks` shelf, as in §4. No proposal shelf: resolved proposals keep
  their place in `Proposals` until a repository's directory needs otherwise,
  and a workspace that wants one declares it.
- Re-applied here, and a test applies it again and finds nothing to add, as
  Phase 2 of the presets proposal did.

A workspace that took the preset before this re-applies it with `prov presets
<dir> --write`. The merge is additive and refuses a collision, so a workspace
that edited its own `open-tasks` keeps its copy and is told so.

## 6. What `dx` keeps

`dx tasks` becomes a formatter over `prov views open-tasks --follow --json`
from the org's root, and `dx tasks <repo> <item>` becomes `prov show`. It drops
its status table, its rank, its item heuristic, its per-file fallback for
repositories that are not workspaces, and `dx shelve` — which is `prov shelve`
run in each repository. What it keeps is what is the org's alone: which
repositories exist, and how to print them. That is devtools work, after this
ships, and is tracked where devtools are.

The one change in coverage: `--follow` reaches the workspaces the org root
names, and nothing else. A repository with tasks and no prov workspace, or one
the org root does not list, stops appearing — which is the org's rule for
joining, enforced by the tool rather than stated beside it.

## 7. Staging

- **Phase 1 — states.** The `states:` block and a term's `state`, read only
  beside a `states:` block; the undeclared-state finding and `StateConflict`;
  `in-state` resolved at load; the `state` grain; `about.md` saying what each
  state holds. The spec's vocabulary file section and the config vocabulary's
  `where:` and grain tables.
- **Phase 2 — `views --follow`.** Independent of Phase 1; the shape of
  `check_across`.
- **Phase 3 — shelves.** The `shelves:` axis and `prov shelve`. Needs Phase 1
  only for the preset's `when:`; the mechanism takes any condition.
- **Phase 4 — the preset.** §5, and this repository re-applies it; its
  `tasks.md` names `prov shelve` where it names `dx shelve`.
- **Phase 5 — devtools.** §6, outside this repository.

## Open questions

1. **Reserved state names.** *Decided: none.* prov gives no state name a
   meaning; the preset carries the convention, and whatever reads a state
   names it (§2).
2. **How `state`-grain groups sort.** *Decided: not here.* Ascending by key,
   as every grain today. Declaration order — and whether a closed
   vocabulary's groups follow its term order under `group:` with no grain,
   which would change what existing views print — is the deferred `sort:`
   axis, and goes to a proposal of its own.
3. **The proposal vocabulary's split.** *Decided: `deferred` is closed*,
   beside `implemented` and `rejected`. Agreed and unscheduled is not open
   work; a view filtering `in-state: { status: closed }` and `equals: {
   status: deferred }` retrieves the deferred ones (§5).
4. **`state` on an open vocabulary.** *Decided: allowed.* A known term's
   state is read the same way whether unknown values are admitted or not,
   and an unknown value is in no state either way.
