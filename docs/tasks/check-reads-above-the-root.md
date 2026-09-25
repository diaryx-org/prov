---
title: '`check` reads a directory above the root to judge a link that climbs out of it'
part_of: '[Tasks](/docs/tasks/tasks.md)'
created: 2026-09-25T00:55:37.835923Z
status: open
description: 'A `contents: ../x.md` reports a broken link or an unreadable document depending on whether `x.md` exists beside the workspace — `exact_name` lists the parent directory of the root before `load` refuses the path'
---

# `check` reads a directory above the root to judge a link that climbs out of it

**Repro.** A workspace whose root lists a document above itself:

```yaml
# ws/README.md
title: Home
contents:
- ../outside.md
```

With nothing at `outside.md` beside `ws/`, `prov -C ws check` reports

```
README.md: broken contents[0] link: ../outside.md
```

Create an empty `outside.md` beside `ws/` — outside the workspace — and the
same command, on the same workspace, reports instead

```
../outside.md: unreadable: path escapes the workspace root: ../outside.md
```

**Why.** The census resolves a path target with `exact_name`
(`prov-graph/src/graph/census.rs`), which reads the listing of the target's
parent directory — here `ws/..` — to ask whether the name is there. Only
afterwards does the walk descend and `Graph::load` refuse the path with
`Error::Escape`. So the escape guard protects the read of a *file* above the
root but not the listing of a *directory* above it, and what that listing
says leaks into the finding. A workspace's findings should be a function of
the workspace.

Found by the Bend port in `spike/bend`, which reproduces prov's findings byte
for byte and so reproduces this too: its host lists whatever directory it is
asked for, as `exact_name` does.

**Done when** a target whose normalized path escapes the root resolves
without reading anything outside the root — reported one way whatever is
beside the workspace, most plainly as the `Unreadable` escape it already is
when the file exists, or as its own finding — with a test that creates the
outside file and checks the finding does not change.
