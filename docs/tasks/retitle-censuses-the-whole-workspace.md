---
title: A retitle censuses the whole workspace to find its inbound links
description: Workspace::retitle (and rename) read every reachable document on every call to find the handful that link to the target, so a one-key edit costs O(workspace) reads — seconds on a large workspace, under whatever lock the consumer holds
author: adammharris
created: 2026-09-09
updated: 2026-09-10
status: done
part_of: '[Tasks](/docs/tasks/tasks.md)'
---

# A retitle censuses the whole workspace to find its inbound links

**Status.** Done, 2026-09-10, in `perf(mutate): retitle and rename find
their inbound links through a stat-validated index kept across verbs`. The
index is `prov/src/workspace/inbound.rs`; DESIGN §5 and the §9 table say where
it lives and what invalidates it. Of the two shapes below, the first was taken
— an index on the `Workspace`, dropped or updated by every set that lands
through `apply_set` — with one addition the shape as written was missing:
prov is not the only writer of a workspace (a consumer's own filesystem
handle, a sync from another device), so every ask **stats** every document
the index knows before trusting it, and any change drops it. A stat opens
nothing, which is what makes it the right probe on a coordinated filesystem.
The second shape was not taken: a consumer's sidebar walk covers the spanning
relation only, so it cannot say who links to a document over any other
relation, and would have needed the census as a fallback anyway. `rename`
and `retitle` share the ask (`inbound_sources`), as required, and
`a_second_retitle_reads_only_what_it_writes` pins the done state over a
counting backend.

**Repro.** On a workspace of a couple of thousand reachable documents, call
`Workspace::retitle` on a leaf that nothing but its parent links to. It reads
every reachable document before it writes two. A consumer measured the
whole-workspace survey the same walk amounts to at about 4.9 s on a
2,000-document workspace on local disk; on a coordinated filesystem (iCloud
Drive) every one of those reads is a coordinated read, and the whole of it
runs while the consumer holds its session lock, so the next keystroke waits
behind a title change.

**What is there.** `prov/src/mutate/retitle.rs`: `retitle` edits the title
key in place through `MetaEditor` — that part is one read and one write — and
then calls `collect_inbound_relabels`, which finds the spanning root, runs
`census(&root)` over everything reachable from it, and filters the census's
entries to those whose resolution lands on `path`. `rename` does the same
through `collect_inbound_rewrites` in `maintain.rs`, and the comment at the
top of `retitle` says so: "the same double read `rename` makes, for the same
reason". The read-scope memo (`read_scope`) collapses the second read of each
source inside one verb into a memo hit, so the cost is *one* census per call,
not two — but it is a whole census, every call, whatever the count of inbound
links turns out to be, because nothing that survives a verb knows the inverse
of the link graph.

The title-only contract is right and not in question here. Whether a retitle
should also move the file is the consumer's decision and `rename` is there
for it; this task is about what finding the inbound set costs.

**What to do.** Make the inbound set cheap to ask for after the first time
it is computed, so that a retitle of a document with *k* inbound links on an
*n*-document workspace costs O(*k*) reads rather than O(*n*). The shape is a
design question and the reason this is a task rather than a fix:

- An inverse-link index held across verbs on the `Workspace`, invalidated by
  the store's own notion of a write (the transaction commit is the one place
  every mutation passes through), and rebuilt lazily on the next ask.
- Or an index the consumer can hand back in — the FFI consumers already walk
  the containment tree for their sidebar and could carry the inverse edges
  they saw — with the census as the fallback when none is offered.
- Either way `rename` and `retitle` should share it, since they share the
  question.

A frontmatter-only census (reading each document's metadata block and not
its body) is not the answer on its own: it still touches every file, and on a
coordinated filesystem the open is the cost, not the bytes.

**Done when.** A second retitle on an unchanged workspace reads no document
it does not write, a test in `prov/src/mutate` pins that with a counting
storage, and `docs/DESIGN.md` says where the inverse index lives and what
invalidates it.
