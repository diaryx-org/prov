---
title: the workspace node
author: adammharris
created: 2026-09-09
updated: 2026-09-09
status: draft
part_of: '[`prov` proposals](/docs/proposals/proposals.md)'
---
# The workspace node — the config document, found without the root

## Summary

The config document is already the workspace node in everything but name. It
carries `workspace_id`, `relations`, `spanning`, `exports`, and the identity
policy. Two things are missing, and both are consequences of one fact: it can
only be reached *through* the root.

1. **Find it by convention.** A whole-file metadata document stemmed `prov` —
   top-level, else `config/`, else `.config/` — is the workspace node. Reading
   it needs no root, which makes policy readable before root-finding rather
   than after.
2. **Let it name the root.** A `root` key resolves the directory that holds two
   root candidates and no conventional stem, which today is
   `Discovery::Ambiguous` and has no escape.

This replaces the `.prov` pointer named in spec §1 rule 1, which was never
implemented and is now rejected: a bare hidden entry at the top of the tree is
clutter, and the document it would point at is one the workspace already has.

What this proposal does **not** do: no new policy keys beyond `root`; no second
policy vocabulary; no requirement that a workspace have a node at all; and no
change to `README`/`index`, which stay exactly what they are.

## Why now

[Crossing the boundary](/docs/proposals/boundary/proposal-boundary-v1.md) asks,
as its first open question, whether the `.prov` pointer of spec §1 rule 1 is
live — because if it is, it is the escape for a sub-root that cannot win its
directory's tie, and if it is not, phase 0 owes that directory an answer. It is
not live. The string appears in no source file. This document is the answer.

## 1. What is true today

### Rule 1 describes an algorithm prov does not run

Spec §1 rule 1 says the root is the file named by a one-line `.prov` pointer if
present, else the first of `README.md`, `readme.md`, `index.md` that exists.
`discover` in `prov/src/discovery.rs` does something else in four ways:

| rule 1 says | `discover` does |
| --- | --- |
| the `.prov` pointer, if present | nothing; the string is in no source file |
| then the first of `README.md`, `readme.md`, `index.md` | scans for candidates; an `index` stem wins, **then** `readme` — the order is inverted |
| the first that *exists* | a candidate must have metadata, no `part_of`, and no prov byline; a frontmatter-less `README.md` is not the root |
| `.md` | any content format, plus whole-file metadata under the conventional stem |

It also omits what happens when neither stem is present: a lone candidate wins,
and two or more is `Discovery::Ambiguous` — a refusal, not a guess. This is a
kernel rule frozen at 1.0, so the drift matters more here than elsewhere.

### The config document cannot be read without the root

`Workspace::config_path` resolves the config relation as a pointer *from the
root document*. So rule 3's "policy has two homes" is only true from one side:
a directory whose root cannot be identified has no way to read policy at all,
including the policy that would say which document is the root. The `.prov`
pointer was a bespoke second file invented to break that circle. A conventional
location for the document that already holds the policy breaks it without one.

### The exclusion already names the thing

`can_be_root` admits whole-file metadata only under the `index`/`readme` stem,
and says why: otherwise "a stray `.json`/`.yaml` config file, which is a mapping
at its root and declares no `part_of`, would masquerade as a root." That
exclusion is correct, and it is correct for exactly this reason — `prov.yaml` is
not a root, it is a *different kind of document*. The code has been describing
the workspace node by negation since it was written.

## 2. The workspace node

A **workspace node** is a document whose subject is the workspace itself rather
than a member of it. It is not in the tree: it has no `part_of` within the
workspace, it is not censused, and no spanning walk reaches it. That is what it
already is; this proposal gives it a name, a conventional home, and one new key.

```yaml
# prov.yaml
workspace_id: prov
root: README.md
spec: 1
relations:
  # ...
```

`root` is the only addition, and it is policy like the rest — so it resolves
through the ordinary layering, and a workspace that does not need it does not
write it. Everything else on the page is a key that exists today.

The distinction the split preserves is the one that argues for it: **`README`
and `index` are the human entry point and the workspace node is the machine's.**
Someone opening the directory cold should find prose that explains the place,
and that is what those names have always been for. Making the same file also
carry identity, vocabulary, and a parent edge is what forces the two roles into
one document and produces the collisions below.

## 3. Where it lives

Three locations, in precedence order:

1. `prov.*` at the top level — what this repository already does
   (`README.md` says `config: prov.yaml`), so nothing migrates.
2. `config/prov.*` — for a workspace that wants its top level clean.
3. `.config/prov.*` — the same, hidden.

`*` is any whole-file metadata format: `yaml`, `yml`, `json`, `toml`, `fig`,
`figl`, each feature-gated as it is today. The stem is fixed; the syntax is the
workspace's own, which is the same rule the rest of prov's documents follow.

**It costs almost nothing.** `discover` already calls `read_dir` once per
ancestor and filters the listing, so matching a stem is a filter over entries
already in hand — the same shape as `stem_is(path, "index")`, and the read memo
holds listings since `c435665`. `DirEntry` carries `file_type()`, so the
top-level listing itself says whether `config/` or `.config/` exist: a workspace
with neither pays no syscall, and one that opted in pays a single extra
`read_dir`. `discover` currently maps entries to paths and drops the file type;
it would keep them.

**Two nodes is a finding, not a brick.** Where the root tie refuses outright,
a duplicated workspace node should resolve by the precedence above and produce
a `check` finding. A stale `config/prov.yaml` left beside a live `prov.yaml` is
a mistake worth reporting, but it is not worth making the workspace unusable
while it is reported. The same holds for two formats in one location, which is
why the extension order above is fixed rather than left to listing order.

**A node that names a root whose own `config` points elsewhere is a finding
too.** Two policy homes disagreeing about which is the policy home is an error,
not a precedence question, and silently picking one hides it.

## 4. What it does to crossing the boundary

The federation case gets a better answer than that proposal's phase 0.

Its phase 0 loosens `is_root_candidate` so a root document may carry a *foreign*
`part_of` — the workspace says what contains it by having its root say so. Put
the parent edge on the workspace node instead and the root document's invariant
never bends: "the root has no spanning-parent" stays literally true,
`is_root_candidate` does not change, and the new ambiguity phase 0 introduces
does not exist. A repository of repositories becomes a graph of workspace nodes
whose `part_of` and `contents` are foreign ids, and `descend` walks node to node
rather than root to root — which is also the cheaper walk, since opening a peer
means reading one small metadata file rather than finding its root first.

It does **not** dissolve that proposal's second case, and the unification should
not claim it. A `docs/tasks/` index that wants to be the same file whether or
not the surrounding repository is a workspace still has one document that must
either say `part_of` or not. Moving the parent edge to a sibling workspace node
changes where that is written, not whether the index is a root. If both shapes
are wanted, both changes are wanted, and they are independent.

## 5. The proposed rule 1

> 1. **Find the root.** The directory's **workspace node** is a whole-file
>    metadata document stemmed `prov`, sought at the top level, then in
>    `config/`, then in `.config/`. If one exists and carries a `root`, that
>    names the root document. Otherwise the root is the directory's sole **root
>    candidate** — a document with metadata, no spanning-parent, and no prov
>    byline — where a candidate stemmed `index` wins, then one stemmed `readme`,
>    then a lone candidate. Two or more candidates with neither conventional
>    stem is an error, not a guess. *(Invariant: the root is the reachable
>    document with no spanning-parent, and the one that declares or points at
>    the workspace's policy (rule 3); the conventions just find it.)*

Longer than the sentence it replaces, and it is the first version that is true.
Rule 3 gains one clause: the config home may be found by convention as well as
by the root's pointer, and the two must agree.

## 6. What stays refused

- **No workspace node is required.** A workspace with a `README.md` and nothing
  else keeps working unchanged, and that stays the common case.
- **No second vocabulary.** The node's keys are the config document's keys,
  because it *is* the config document. `root` is the one addition.
- **No app-private sidecar.** The node is the same policy vocabulary in the same
  formats, readable by anything that can read the workspace. Spec §1's floor
  moves from one convention to two; it does not become opaque.
- **Not in the tree.** The node is not censused, not reached by a spanning walk,
  and carries no `part_of` within its own workspace. A foreign `part_of` on it
  is §4's business, not this proposal's.

## 7. Staging

- **Phase 0 — rule 1 says what prov does.** Rewrite spec §1 rule 1 against
  `discovery.rs` and delete the `.prov` sentence. No code, and worth doing on
  its own merits whether or not the rest lands: the README/index order is
  currently backwards in a rule frozen at 1.0.
- **Phase 1 — find the node.** Stem `prov` in the three locations, precedence,
  the duplicate finding. Policy becomes readable without the root; no existing
  workspace changes behaviour, since one that names its config from the root
  still resolves it that way.
- **Phase 2 — `root`.** The key, and the `Ambiguous` escape it provides.
- **Phase 3 — unscheduled.** The foreign `part_of` on the node, which is
  crossing the boundary's problem and closes over its phase 0.

## Open questions

1. **Does `root` point outside its directory?** `root: docs/index.md` would let
   a repository put its workspace in a subdirectory while the node sits at the
   top. Useful, and it makes the root directory and the node's directory two
   different things everywhere downstream. Proposed answer: same directory only,
   until something needs otherwise.
2. **`prov` as a stem, or the concept's name?** `prov.yaml` names the tool;
   `workspace.yaml` names what the document is, which is the framing this whole
   proposal rests on. `prov` is what exists in the wild and what this repository
   already writes, and a rename is a migration for a gain that is only legibility.
   Proposed answer: keep `prov`, and say in the spec that it is the workspace
   node so the concept has a name even where the filename does not carry it.
3. **Should `check` report a workspace that has no node?** No — a `README`-only
   workspace is the common case and correct. Noted only because the same
   question was asked of anonymous sub-roots in crossing the boundary, and the
   answer should be the same shape: silence.
4. **Does the node participate in fixity?** It is a file prov reads and writes
   and nothing gives it a `content_hash`, since it records only itself. Probably
   nothing to do, but the provenance draft's §5 rests on which documents can be
   bound to a digest, and this is a new one.
