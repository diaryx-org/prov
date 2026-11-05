---
title: Tests leave their fixtures in the temp directory
part_of: '[Closed tasks](/docs/tasks/closed/closed.md)'
created: 2026-11-05T16:46:21.220704Z
status: done
description: "A green `cargo test --workspace` leaves 741 entries and 34 MB in the temp directory, and every run adds more, because `prov_testkit::scratch` removes only the path its own process id names"
updated: 2026-11-05T19:56:20.778270Z
---

# Tests leave their fixtures in the temp directory

**Status.** Done. `prov_testkit::scratch` returns a `Scratch`, a guard that
derefs to `Path` and removes the directory on drop, unless the thread is
panicking. Every hand-written fixture moved onto it, and `smoke.rs` keeps its
backups in a scratch directory of their own. Paths carry a per-process count
as well as the pid, because a guard removes what it holds and two tests must
not share one. `cargo test --workspace` with a fresh `TMPDIR` leaves nothing.

**Done when** `cargo test --workspace` leaves nothing in `$TMPDIR` on a run
that passes, and a test that fails still leaves its directory to be read.

## Repro

```sh
mkdir /tmp/prov-scratch
TMPDIR=/tmp/prov-scratch cargo test --workspace
ls -A /tmp/prov-scratch | wc -l     # 741
du -sh /tmp/prov-scratch            # 34M, in 5,318 files
```

Measured on `1f882e0`, with all 1,238 tests passing. Nothing removes what
one run leaves, so it adds up:

```sh
mkdir /tmp/prov-views-scratch
TMPDIR=/tmp/prov-views-scratch cargo test -p prov-views
ls -A /tmp/prov-views-scratch | wc -l     # 10
TMPDIR=/tmp/prov-views-scratch cargo test -p prov-views
ls -A /tmp/prov-views-scratch | wc -l     # 20
```

## Why

`prov_testkit::scratch` makes `prov-<scope>-<tag>-<pid>`, removing that path
first. Its comment says a stale directory from a previous run "is removed
rather than reused" and that the path is "stable across runs instead of
random". The process id makes it neither: the next run has another pid, so
it looks at another path and never meets the old one. Nothing removes the
directory at the end of a test either.

The rest is the same habit written out by hand. 29 files outside
`prov-testkit` call `temp_dir()` themselves, most of them to the same
pattern (`prov-smoke-<tag>-<pid>` in `prov-cli/tests/smoke.rs`). `Federation`
in `prov-cli/tests/follow.rs` shows the way out, a `Drop` that removes its
root, and only a few fixtures have one. `smoke.rs` also writes
`smoke-all-backup` and `smoke-all-backup.zip` beside its directory rather
than in it, so removing the directory would not take those.

## What it costs

Disk where the temp directory is a disk. Where it is a tmpfs, as on a machine
that runs agents, it is memory, and it stays until the next reboot.

## Where to start

`scratch` returning a guard that derefs to `Path` and removes the directory
on drop moves most of the workspace at once, since 30 files reach it. A guard
has to keep the directory when the thread is panicking, or a failing test
loses the state that says why. The hand-written fixtures then move onto it,
and the comment on `scratch` should stop claiming what the pid rules out.
