---
title: Tasks
description: Deferred work with a done state — a bug is a task with a repro
author: adammharris
created: 2026-09-09
updated: 2026-09-09
part_of: '[prov](/README.md)'
contents:
- '[A retitle censuses the whole workspace to find its inbound links](/docs/tasks/retitle-censuses-the-whole-workspace.md)'
---

# Tasks

Deferred work, one file per item, each with a `status` of `open`,
`in-progress`, `done` or `dropped`. `contents` above lists what is open;
closing a task is setting its status and naming the commit that resolved it,
not deleting the file. A bug is a task with a repro. Arguments for a change
that may lose are [proposals](/docs/proposals/proposals.md), not tasks.
