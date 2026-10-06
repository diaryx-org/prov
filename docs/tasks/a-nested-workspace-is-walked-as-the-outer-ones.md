---
title: A nested workspace is walked as the outer one's
description: "A directory holding its own workspace node is read by the outer workspace's ignore walk, id scan and title scan as though it were the outer's, a path link into it is not a finding, a move across it is not refused, and nothing maps its `workspace_id` to its directory"
author: adammharris
created: 2026-10-06
updated: 2026-10-06
status: done
part_of: '[Tasks](/docs/tasks/tasks.md)'
---

# A nested workspace is walked as the outer one's

> **Done, 2026-10-06**, by `feat(graph): stop at a nested workspace, and judge
> it by its own list` and the three commits after it. Each item below is
> built and tested; `docs/reference-styles.md` § "A workspace inside a
> workspace" says how the boundary is held.

The prov half of diaryx's accepted proposal *A book someone else wrote is a
workspace inside the library* (diaryx
`docs/proposals/a-book-someone-else-wrote-is-a-workspace-inside-the-library.md`).
A held text — the Book of Mormon is the case — sits in a directory of the
library with its own node, `workspace_id` and relations, and the library lists
it by a foreign `contents` entry. The [boundary
proposal](/docs/proposals/boundary/proposal-boundary-v1.md) made that shape
legal; the walks below do not yet respect it.

## What is wrong now

- **The ignore walk rules the whole directory `Unreached`**
  (`prov/src/workspace/ignore.rs`), because the outer graph reaches nothing in
  it through a foreign edge. diaryx's skiplist writes that rule into
  `history/skipped/`, so the held text is left out of history and out of
  Diaryx Cloud.
- **`scan_ids` and the full `scan_titles` descend into it**, so the outer
  workspace can read inner ids and titles as its own.
- **A path link from outside into it is not a finding**, though it bridges
  two censuses. The reference-styles rule against it is "for the writer, not
  a check".
- **A rename or move across it is not refused.** The moved document's id
  stays in the outer registry pointing into the inner directory.
- **Nothing maps `workspace_id` to the directory**, so `id:<ws>/<id>` to a
  nested workspace resolves only through a device's recent list.

## Done when

- The ignore walk, meeting a nested node, takes the inner workspace's own
  ignore list for that subtree: the inner reachable files are not ignored,
  and the inner `history/`, recycle bin and dotfiles are.
- `scan_ids` and `scan_titles` stop at a nested node.
- `check` reports a path link that resolves into a nested node's directory.
- A rename or move whose source and destination lie on different sides of a
  nested node is refused, naming copy-plus-delete.
- The walk records each nested node it meets as `workspace_id → directory`,
  offered behind the existing `PeerResolver`, and two nested workspaces
  declaring one name in one library are refused as ambiguous.
- Each has a test on a library holding a nested workspace.

historica already leaves a nested `history/` store out of the outer record
(historica decision 0084), so the inner store needs no rule from here for
that; the ignore list still owes the inner workspace's other exclusions. The
diaryx half (nearest-first resolver, marks on a foreign target, the shelf
entry) waits on this.
