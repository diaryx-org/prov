---
title: crossing the boundary
author: adammharris
created: 2026-09-09
updated: 2026-09-09
status: accepted
part_of: '[`prov` proposals](/docs/proposals/proposals.md)'
---
# Crossing the boundary — a workspace that acknowledges a parent, and a reader that descends

## Status: accepted; phase 0 landed 2026-09-09

**Phase 0 shipped through the named root, not through the candidate clause.**
`is_root_candidate` did not change. The mechanism is the one
[the workspace node](/docs/proposals/workspace-node/proposal-workspace-node-v1.md)
had already built: a sub-workspace's node names its root (`root: README.md`),
discovery trusts a named root without the candidate test, and *that* root carries
the foreign `part_of`. §1's "why nothing downstream has to change" is exactly
right and is what makes this work; only its premise moved.

Three consequences of taking that route:

- **The ambiguity regression §1 predicted does not exist.** No candidacy test was
  widened, so no directory gains a candidate, and a document with a foreign
  `part_of` that no node names is still not a root. Both are pinned by tests in
  `discovery.rs`.
- **The price is a node.** A sub-workspace must have one to be found as a
  workspace — which it wants anyway, since it is where its own `workspace_id`,
  `exports`, vocabulary and identity policy live. §2 of the workspace-node
  proposal argued this; it is the whole of the cost.
- **`named_root_contained` narrowed rather than disappearing.** It resolves the
  named root's parent and reports only when the target is local — a path, an
  unresolved local id, an alias, or a reference qualified with this workspace's
  own name, which §1 correctly says *is* local. A `Target::Foreign` parent is
  silent. That is the `Behavioural-change:` this carried, in place of the
  ambiguity one it was expecting.

The rules of §1's "Two rules that come with it" are now in
`docs/reference-styles.md` under "A workspace inside a workspace", with the shape
and the spec §1 rule 1 wording.

**Open question 2 — answered (2026-09-09): silence.** An anonymous sub-root is
legal and unreferenceable, exactly as an anonymous workspace already is, and
nothing reports it. Same shape as the workspace-node proposal's open question 3,
and for the same reason: prov mints a workspace name only on request.

**Phases 1 and 2 are in progress** — `crossing` (`open_peer`, `descend`), then
`tree --follow` / `check --follow` / `explore`. Phase 3 stays unscheduled. Open
questions 3 and 4 are still open and belong to phase 1.

The body below is left as it was argued.

## Summary

Two changes, in that order, each useful without the other.

1. **The root clause.** A root document is one whose spanning parent is *absent
   or foreign*. Today it must be absent, so a workspace cannot both root itself
   and say what contains it. This is one clause in `is_root_candidate`, two call
   sites, and a sentence in the spec.
2. **The descent.** A walk that, given a `PeerResolver`, follows a **confirmed**
   foreign spanning edge into the peer and keeps going — opt-in per command,
   composed *above* `Graph` rather than inside it.

What this proposal does **not** do, because each refusal is load-bearing and
stays: no peer table in the config; no check-verification of foreign references;
no writing across a boundary; no third type parameter on `Graph`; no following
by default.

## Why now

Two shapes people keep building by hand, neither of which prov can quite spell.

**A repository of repositories.** A directory holding many checkouts, wanting a
root that lists them. It cannot use path links: a path link makes each checkout
a subtree, which means each checkout's root needs a `part_of` naming a directory
above it, which means that checkout stops being discoverable when cloned on its
own. So the outer root either lies about the structure or has none. With foreign
ids it works today — `contents:` of `id:<name>/<id>` entries commits fine
without the checkouts present, since a name is not a path — but the tree stops
at eighteen leaves and no reader can go further.

**A subtree that wants to stand on its own.** A `docs/tasks/` directory, say,
that should be a workspace in a repository that is otherwise not one, and a
plain subtree in a repository that is. Today those are different documents: as a
subtree the index says `part_of`, as a root it must not. The author has to know
which kind of repository they are in before writing the file, and the answer
changes when the repository later adopts prov. The third possibility — a subtree
that is its own root *and* names the parent it hangs under, with its own
`exports`, its own vocabulary, and its own identity policy — is the one that
would make the document identical in both cases, and it is the one the root rule
forbids.

Both are instances of the same missing statement: **a workspace should be able
to say what contains it without ceasing to be a workspace.**

## 1. The root clause

### What is true today

```rust
fn is_root_candidate(doc: &Document) -> bool {
    doc.has_meta() && doc.meta.get("part_of").is_none() && !crate::about::is_generated(&doc.meta)
}
```

The test asks whether the *key is present*, not whether it resolves to anything
here. It has exactly two callers, both in `prov/src/discovery.rs` — `discover`,
which walks up the filesystem, and `Workspace::root_document`, which asks the
same question of one directory already known to be a root.

### The change

A root candidate's spanning parent is absent, **or names a workspace other than
this one**. In terms of the machinery: parse the value and keep the document as
a candidate when the link resolves to `Target::Foreign`.

This does not weaken the invariant in spec §1; it states it precisely for the
first time. That invariant reads "the root is the reachable document with no
spanning-parent" — and every other part of prov has always meant *no
spanning-parent within this workspace*. A foreign target resolves to something
that produces nothing, everywhere else in the codebase. Discovery is the single
place that asks the coarser question.

Self-qualification comes along for free and must: a reference qualified with the
reading workspace's own name *is* local (reference-styles, "Across workspaces"),
so an index whose `part_of` names its own workspace has a real parent and is
correctly not a root. The clause has to be written over the resolved target, not
over the spelling.

### Why nothing downstream has to change

This is the argument for doing it at all. Every consumer of a spanning parent
already behaves correctly when that parent is foreign, because every one of them
goes through a resolution that answers `Target::Path` or nothing:

- `single_target` (`prov/src/mutate/maintain.rs`) returns `Some` only for
  `Target::Path`. A foreign `part_of` yields `None`.
- `spanning_root` therefore terminates *at* a sub-root rather than climbing past
  it, which is exactly the desired answer, and it reaches that answer today with
  no change.
- `missing_containment` (`prov/src/validate.rs`) records a claim only through
  `single_target`, so a sub-root claims membership in nothing local and raises no
  `MissingContainment` — the same silence a vendored tree gets.
- The broken-link pass already files `Resolution::Foreign` with "the resolutions
  that produce nothing", by name and with a comment saying why.
- `tree` already renders a foreign spanning target as `NodeKind::Foreign`, shown
  rather than followed or dropped.
- Every rewrite site already filters on `Link::is_path_target`, false for every
  id form, so a move can never damage the edge.

The machinery for a workspace that names its parent is, in other words, entirely
built. One predicate disagrees with it.

### Two rules that come with it

**An edge into a sub-workspace is foreign, or it is not a boundary.** If the
outer root also reaches the inner directory by a path link, the inner documents
are censused by both roots, and neither is wrong. prov cannot raise a finding
about this — the outer check is reachability-bounded and only sees the inner
directory *because* the path link exists — so it is a rule for the writer, and
belongs in `docs/reference-styles.md` beside the rest of the cross-workspace
grammar.

**Which root you opened decides where the boundary is.** `spanning_root` falls
back to `root_document()` of the workspace it was given. Opened at the
repository, a sub-root's spanning root is the repository's README; opened at the
sub-root, it is the sub-root. Both are correct answers to different questions,
and the `-C` flag is how a caller chooses which one it is asking.

### The one behaviour change

Widening a candidacy test can only add candidates, and adding a candidate to a
directory that had exactly one turns `Found` into `Ambiguous`. The case: a
directory holding both a conventional root and a foreign-parented document that
is *not* stemmed `index` or `readme`. Today the second is invisible and the first
wins; afterwards the directory has two unnamed candidates and prov refuses to
guess.

The `index`/`readme` tie-break runs first and covers the common shape, so this
reaches only a workspace whose root is named something else — and such a
workspace is already one `prov about` away from the same refusal, which is the
hazard `is_generated` was added to close. It is still a real regression for a
real layout, so it is a `Behavioural-change:` trailer, not a footnote.

## 2. The descent

### Where prov stops, and why that reasoning has a floor

`prov-graph/src/peer.rs` is explicit: "Nothing here opens a workspace, and
nothing here can: reading the peer would need a second `ReadStorage` and a second
`IdIndex`, which only the host has." That is true of `prov-graph`, and it should
stay true — it is why `Graph` stays two generics wide and why no method on it
takes a resolver.

It is **not** true one layer up. `prov::discovery::build` already constructs a
second `Workspace` at an arbitrary directory out of the same storage handle:

```rust
let probe: Workspace<FS> = Workspace::builder(fs.clone()).root(&root_dir).build();
```

The storage seam is device-wide, not root-scoped. A crate that can walk *up* the
filesystem to find a root it was not given can also open a root a resolver hands
it. So descent belongs in `prov`, next to discovery, and `prov-graph` learns
nothing.

This keeps the promise `peer.rs` makes rather than breaking it. Following a
foreign reference stays "a *second step* after resolution, taken by a caller that
wants it" — the proposal is only that prov ship that second step once, instead of
each host writing it.

### The primitive

A module in `prov` — `crossing`, say — with one function that opens a peer and
one that walks:

```rust
pub async fn open_peer<FS: Storage + Clone>(
    fs: &FS,
    peers: &dyn PeerResolver,
    workspace: &str,
    trust: Trust,
) -> Result<Crossing<FS>>;
```

- Answers with the opened workspace, or with the `PeerLookup` that explains why
  not. A peer that is `Unknown`, `Mismatched`, `Anonymous` or `Unreadable` is
  never an error — a foreign reference is carried whether or not it resolves, and
  that does not change because someone asked to follow it.
- `Trust::Confirmed` is the default and only ever opens `PeerLookup::confirmed`.
  `Trust::Unverified` reaches `followable_unverified`, and exists because that
  escape is the reader's to take; `Mismatched` is refused under both, as the
  accessor already guarantees.
- **A `PeerLocation::Url` is never opened.** prov does no network I/O, and this
  is the one absolute in the module. A URL peer is an address to render, not a
  root to read.

### The walk

`descend` composes `open_peer` with the existing tree walk, replacing each
foreign spanning leaf with the peer's own tree. Its rules, which are the reason
it should exist once rather than per host:

- **A visited set, or it does not terminate.** Two workspaces may list each
  other, and an org root plus eighteen repositories that each link back is the
  expected shape rather than an exotic one.
- **Deduped on the canonicalized root directory *and* on `workspace_id`.** Two
  names for one directory and two directories claiming one name are both
  reachable states; the first is a symlinked checkout, the second is the failure
  `PeerLookup::confirm` exists to catch, arriving one hop later.
- **A depth bound**, defaulted and overridable.
- **Findings and paths namespaced by workspace.** A followed `check` is
  eighteen reports, each in its own workspace's terms, never one merged list —
  a relative path means nothing once it has crossed a root.
- **Every refusal is carried as a leaf with its reason**, so a followed tree can
  say "the peer `notes` is on record at a directory that calls itself `journal`"
  rather than silently showing the same leaf an unfollowed tree would.

### What the CLI does with it

`prov tree --follow[=DEPTH]` and `prov check --follow`, both off by default.
`explore` gains the ability to step across a foreign link it currently shows and
cannot open.

Off by default is not timidity. Reachability-boundedness is the property that
makes prov usable inside a larger repository (DESIGN §8), and a command that
crossed boundaries on its own initiative would make every invocation cost the
size of the whole federation — including, for the motivating shape, eighteen
`cargo` checkouts' worth of directories that the bound is currently protecting
everyone from.

`prov check --follow` means *run each reachable workspace's own check and report
them grouped*. It explicitly does not mean verifying foreign references, which
stays refused for the reason it always has been: a finding raised about a
workspace this device cannot see is a false positive on every such reference, on
every device that lacks it.

### What stays refused after this ships

- **No writes across a boundary.** Descent is read-only. Registration remains a
  publish-time contract — prov never reaches into another workspace to register
  on its behalf — so `mv` still rewrites no peer's inbound references and a
  reference to an unpublished foreign document can still dangle. A limit
  restated, not closed.
- **No peer table in `prov.yaml`.** A name is a fact about an archive; a
  location is a fact about a disk. Being able to follow a map does not make the
  map the archive's business.
- **No resolver on `Graph`**, and no cost to a traversal that never crosses.

## 3. What it buys

A repository of repositories gets a root whose `contents` is a manifest *and* a
graph, committed without any checkout present, and a `tree --follow` that
renders the whole federation on a machine that happens to have them.

A subtree gets to be a workspace: its own `exports` (so a publish layer can
build a site from it without the parent's config), its own vocabulary, its own
identity policy, and a `part_of` that says what it belongs to. The same file
works whether or not the surrounding repository is itself a workspace, which is
the property that makes such a subtree worth standardizing at all.

And a peer map can become a *layout* rather than a file. A resolver over "the
sibling directory named by the workspace id, confirmed by reading its config"
needs no `peers` file, and is a dozen lines over the port that already exists.
Whether prov-cli should ship one beside its hand-parsed map is an open question
below, not part of this proposal.

## 4. Staging

- **Phase 0 — the clause.** `is_root_candidate` over the resolved target; tests
  for a foreign parent, a self-qualified parent, and the new ambiguity; spec §1
  rule 1; the boundary rule in `docs/reference-styles.md`. Self-contained and
  useful with nothing else built: the third shape becomes legal, and every
  existing workspace discovers exactly as before.
- **Phase 1 — `crossing`.** `open_peer` and `descend` in `prov`. `NoPeers`
  stays the default everywhere; no existing caller changes behaviour.
- **Phase 2 — the flags.** `tree --follow`, `check --follow`, `explore` across a
  boundary.
- **Phase 3 — unscheduled.** Opt-in verification of a foreign reference *whose
  peer was opened*, where the evidence is genuinely present. Deliberately last,
  and deliberately separate, because it is the one part that risks re-introducing
  the per-device false positive.

## Open questions

1. **The `.prov` pointer — answered (2026-09-09).** It is aspirational: the
   string appears in no source file, and it is now rejected rather than
   deferred, because a bare hidden entry at the top of the tree is clutter and
   the document it would point at is one the workspace already has. What a
   tie-losing directory does instead is
   [the workspace node](/docs/proposals/workspace-node/proposal-workspace-node-v1.md) —
   the config document found by convention, carrying a `root` key. That
   proposal's §4 also argues the parent edge belongs on the node rather than on
   the root document, which would close over phase 0 here; the two are
   independent, and this one's second motivating case survives either way.
2. **Anonymous sub-roots.** A sub-root with no `workspace_id` cannot be named by
   the parent's foreign edge, so the shape only works with a name — but prov
   mints a name only on request, never on its own initiative, and that should
   not change. Proposed answer: an anonymous sub-root stays legal and
   unreferenceable, exactly as an anonymous workspace already is, and nothing
   reports it. Is silence right here, or is this the one place a hint earns its
   noise?
3. **The visited set's grain**, as above: canonical path, name, or both. Both
   costs a `canonicalize` per hop.
4. **A layout resolver in `prov-cli`**, beside the peer file — and if so, whether
   it is a fallback for a name the file does not carry, or a separate source with
   its own precedence rung.
