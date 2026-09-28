---
title: Tasks
description: Deferred work with a done state — a bug is a task with a repro
author: adammharris
created: 2026-09-09
updated: 2026-09-17
part_of: '[prov](/README.md)'
contents:
- '[Closed tasks](/docs/tasks/closed/closed.md)'
---

# Tasks

Deferred work, one file per item, each with a `status` of `open`,
`in-progress`, `done` or `dropped`. Closing a task is setting its status and
naming the commit that resolved it, not deleting the file. A closed task then
moves to [Closed tasks](/docs/tasks/closed/closed.md) (`dx shelve`), which is
part of this index, so it stays reachable and under `Tasks` for the vocabulary
and every view, and `contents` above lists what is live. What is open is what
`prov views open-tasks`
lists — the `status` vocabulary and the view are the `tasks` preset this
repository ships in `presets/tasks/` — and what `dx tasks` reads off the
`status` key. A bug is a task with a repro. Arguments for a change that may
lose are proposals, not tasks.
