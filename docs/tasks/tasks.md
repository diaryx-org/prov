---
title: Tasks
description: Deferred work with a done state — a bug is a task with a repro
author: adammharris
created: 2026-09-09
updated: 2026-09-17
part_of: '[prov](/README.md)'
contents:
- '[A retitle censuses the whole workspace to find its inbound links](/docs/tasks/retitle-censuses-the-whole-workspace.md)'
- '[A move rewrites a document''s body links but not its body images](/docs/tasks/rename-leaves-body-images-behind.md)'
- '[A finding carries no severity, and a relation site no item index](/docs/tasks/findings-carry-no-severity-or-item-index.md)'
- '[A `generated` pair says who and when, and not what was done](/docs/tasks/generated-records-no-activity.md)'
- '[No relation lets a document say it replaces, or was derived from, another](/docs/tasks/no-relation-says-replaces-or-derived-from.md)'
- '[A directory moves one document at a time, and a payload''s references do not move at all](/docs/tasks/a-directory-moves-one-document-at-a-time.md)'
- '[A date is exact or unknown, and nothing in between](/docs/tasks/a-date-is-exact-or-unknown.md)'
---

# Tasks

Deferred work, one file per item, each with a `status` of `open`,
`in-progress`, `done` or `dropped`. Closing a task is setting its status and
naming the commit that resolved it, not deleting the file — and not delisting
it either: a closed task keeps its place in `contents` above, as a resolved
[proposal](/docs/proposals/proposals.md) does, because a document nothing
reaches is a `check` finding. What is open is what `prov views open-tasks`
lists — the `status` vocabulary and the view are the `tasks` preset this
repository ships in `presets/tasks/` — and what `dx tasks` reads off the
`status` key. A bug is a task with a repro. Arguments for a change that may
lose are proposals, not tasks.
