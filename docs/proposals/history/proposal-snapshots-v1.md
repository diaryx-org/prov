---
part_of: '[`prov` proposals](/docs/proposals/proposals.md)'
---
# Snapshots — a pre-sync safety net

> Working proposal. What prov should do to make multi-device sync recoverable
> when it goes wrong. Complements DESIGN §5 (the index's two natures), §8
> (validation as the sleeper feature), and the recycle bin (`docs/DESIGN.md`
> row 426), and answers part of open question #1 — "full history/event-log
> stores remain possible behind `IndexStore`, e.g. for sync."

## The problem

prov workspaces are plaintext, so the obvious way to sync one across devices
is to point an existing transport (git, Dropbox, iCloud, Syncthing) at the
directory and let it reconcile files. That's free for ordinary content edits —
plain text merges fine.

It is not free for **structural** mutations. A rename, move, or delete
touches several files at once (the node itself, every inbound link, the
parent's child list, the id registry). If two devices perform structural
mutations concurrently, the sync transport reconciles the *bytes* with no idea
about prov's graph — the result can look like a clean merge and still be
semantically broken (stale links, duplicate containment, a dangling registry
entry), or a transport-level conflict (a Dropbox "conflicted copy", a Syncthing
`.sync-conflict` file, a botched git merge) can mangle a file outright.

Today there is no safety net for this. The crash journal (`.prov-journal`,
DESIGN §9-adjacent) protects a single device against its own interrupted
writes; it has nothing to say about damage introduced by an external sync
tool reconciling two devices. The recycle bin protects against an explicit,
single-device delete. Neither covers "a merge silently broke something and I
want yesterday back."

## Position

Two separate features answer two separate failure modes. Building them as one
mechanism would blur both:

1. **Snapshots** — an in-workspace, reachable, granular pre-image store,
   captured around sync boundaries, so a bad merge can be inspected and rolled
   back file-by-file. This is the bulk of this proposal.
2. **Backup** — a plain, opaque, whole-tree copy to an arbitrary filesystem
   location, for redundancy against losing the workspace's location entirely
   (a dead disk, a deleted cloud folder). Deliberately simple, deliberately
   outside the reachable graph.

Only **Snapshots** is config-gated (`snapshots: off | manual`, [Config
axis](#config-axis)) — mirroring how `recycle_bin` is already a config toggle,
not a hardcoded behavior. **Backup** deliberately has no config axis at all: it
is an imperative, one-off action the user invokes, not a standing behavior
prov applies on every mutation, so there is nothing to turn on or off (see
[Design: `backup`](#design-backup) and [Config axis](#config-axis) below).

### Why not a dotfolder

Rejected outright. DESIGN §1 opens on "not... an app-private sidecar folder,"
and §6 frames prov as "Obsidian, except the user owns what `.obsidian/` used
to own." A hidden dotfolder for snapshot state is exactly the anti-pattern the
whole design inverts. Anything prov maintains must be **reachable** — linked
from the root like the registry and recycle bin — and **self-describing** —
a whole-file document `check` can validate, not an opaque blob store only
prov's own code understands.

### Why not a full-tree ZIP

Simple, but throws away the one thing prov workspaces have going for them:
legibility. A ZIP is opaque until unzipped; you can't see what a snapshot
contains without extracting and diffing it, which cuts against "store
canonical, render pretty" and against `check`'s whole reason for existing —
reasoning about workspace state without a side channel. It also fully
duplicates git if the transport already is git.

### Why not a full operation log

More elegant in the abstract — replay `ChangeSet`s instead of storing bytes —
but it only protects against **prov's own mutations** misbehaving, which the
crash journal already covers reasonably well. It does nothing for the actual
threat here, which is **external** damage: a sync tool's merge landing bytes
prov never wrote through a `ChangeSet` at all. It would also require a
correctness-critical, tested inverse for every one of the ~10 mutation kinds
(`combine`'s inverse is genuinely ambiguous) before it protects anything.

### Why per-file, content-addressed pre-images

Matches the actual risk (specific files corrupted by a bad merge, not "the
whole tree is gone" — that's what Backup is for) at the lowest cost: after the
first capture, only files that actually changed get a new pre-image, and
identical pre-images across multiple snapshot events dedupe for free once
addressed by content hash. It also reuses infrastructure that already exists
rather than inventing a new one: the fixity module (SHA-256, dependency-free,
`sha256:<hex>`) already gives prov a hashing primitive; the recycle bin
already establishes the "visible directory, unreached bytes, reachable
record" shape.

The honest cost is the **first** capture, which parks a pre-image of every
file in the capture set — a full second copy of the workspace, inside the
workspace, that the transport then uploads. Steady state is cheap; genesis is
not. Attachments dominate that number, so a workspace of prose barely notices,
and the copy can later be made near-free by copy-on-write cloning
(`clonefile`, `FICLONE`) behind a fourth `Capabilities` flag
(`prov/src/fs.rs:321`) — semantically a true copy, unlike a hardlink, which an
in-place editor would silently corrupt. A pure optimization with no format
implications, so it can wait until the cost bites.

## Design: the `snapshots` mechanism

Follows the recycle bin's *shape* as closely as the semantics allow, as a
**sibling**, not an extension — restore's meaning ("give back what was
deleted") shouldn't be muddied by a mechanism where nothing was deleted, only
a pre-image was parked. That shape ("park bytes unreached + reachable record
+ `check` validates it") is a convention both features follow, not shared
code — see [Rejected/non-goals](#rejected--non-goals) for why the two aren't
built on one extracted primitive.

- **A new pointer relation off the root**, `snapshots` (alongside `registry`,
  `recycle_bin`, `config`) — one-way, no `part_of` back-link. `RelationSet`
  already exposes `registry_relation`/`config_relation`/`recycle_relation` as
  siblings (`prov/src/relation.rs:255-265`); `snapshots_relation` is a fourth,
  discovered the same way `Workspace::recycle_bin_path` already discovers the
  bin (`prov/src/workspace.rs:275`) — a new `Workspace::snapshots_path` follows
  that exact pattern.
- **A visible directory**, `snapshots/`, holding:
  - `snapshots/index.<ext>` — the reachable entry point: a prose document
    explaining what the directory is, linking the year shards below. **A
    rebuildable cache, not the authority** — see [the store is
    append-only](#the-store-is-append-only-at-the-filesystem-level) below.
  - `snapshots/events/<YYYY>/<MM>/` — one **immutable document per snapshot
    event**, self-describing in prose, **sharded by date** so no single
    directory accumulates years of captures. Each level carries its own index
    document (`events/2026/index.<ext>`, `events/2026/07/index.<ext>`) —
    ordinary prov nodes with `part_of`, link-shaped containment like any other
    subtree, not a second mechanism. See [sharding](#sharding-events-by-date).
  - `snapshots/blobs/` — **unreached**, so §8's orphan check ignores it
    exactly as it already ignores `recyclebin/items/`. Pre-image bytes live
    here, named by content hash (`snapshots/blobs/<first-2-hex>/<rest>`) —
    **bare hex, never the `sha256:` scheme prefix an event spells**: a
    colon in a filename is hostile to Windows and to more than one sync
    client, and prov already carries `CaseMismatch` because it takes path
    portability seriously. Bytes are verbatim — never re-encoded, so a restore
    is byte-exact.
- **Each event records *changes*, not a full listing.** `snapshot-create`
  still hashes the whole capture set each time (bounded reachability, §8 —
  `check`'s orphan walk, less the exclusions below), but it only *records* what
  differs from the workspace's **effective state as of the event it captured
  against**: a file whose hash changed, or that became newly reachable, goes
  in `changed`; a path that was reachable then and is not now goes in
  `removed`. A blob is only parked when its hash is not already present.

  **The capture set is the live graph, minus prov's two byte-parking stores.**
  Reachability alone is the wrong boundary, in both directions:

  - `snapshots/` is itself reachable off the root, so a naive "hash everything
    reachable" would capture the snapshot store *inside* the snapshot store.
    Every event would carry entries about the previous event and the churned
    index — defeating the point of being able to read an event and see what
    changed — and, far worse, restoring an old snapshot would restore an *old
    snapshot index*, one that knows nothing of any later capture. Under an
    `--exact` restore that deletes uncaptured paths, rolling back would
    **destroy the newer recovery points**. The snapshot store is the one
    subtree the mechanism is deliberately blind to.
  - `recyclebin/items/` is excluded for the opposite reason: it is *already*
    unreached, and it must stay outside the capture set even so. `empty_bin`
    is the workspace's only hard delete (§9), and if snapshots silently
    retained purged bytes prov could never actually forget anything — someone
    who empties the bin to destroy something sensitive would be misled.
    Snapshots do not make `empty_bin` reversible, and that is a deliberate
    boundary, not an oversight; it belongs in the CLI help beside the purge
    warning. See [what `restore` doesn't
    protect](#what-restore-actually-protects--and-what-it-doesnt).

  Everything else structural stays captured — the registry, the config
  document, and the recycle bin's *index*. Capturing the bin index is what
  makes the common case correct: a document that was live at capture time
  comes back live, and the bin index reverts to a state that does not list it.
  The one residue is narrow and known: if `empty_bin` ran since the snapshot,
  the restored index names items whose bytes are gone. A bin record whose
  parked bytes are absent is a condition `check` should catch regardless of
  whether this feature ever ships, so it belongs in a separate recycle-bin
  change rather than here.

  ```markdown
  <!-- snapshots/events/2026/07/2026-07-23-1410-pre-sync-4f2a.md -->
  ---
  part_of: '[July 2026](index.md)'
  created: 2026-07-23T14:10:55Z
  parent: 2026-07-22-0903-nightly-8c1d
  trigger: manual
  label: pre-sync
  changed:
    - path: notes/foo.md
      hash: sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
  removed:
    - path: notes/old-name.md
  ---
  # Snapshot — 2026-07-23 14:10 (pre-sync)

  Pre-image of 1 changed and 1 removed file, captured before syncing.
  Roll the workspace back to this point with:

      prov snapshot-restore 2026-07-23-1410-pre-sync-4f2a
  ```

  A **full-listing-per-event** design (the whole capture set re-recorded on
  every `create`, not just re-hashed) would directly contradict [why per-file,
  content-addressed pre-images](#why-per-file-content-addressed-pre-images)
  below: blobs would still dedupe, but the recorded *text* would grow
  `O(files × snapshot count)`, turning a diff-friendly record into a full-tree
  dump on every sync and defeating the point of being able to `git diff` or
  eyeball an event to see what actually changed.

  The cost moves to reconstruction: `snapshot-show`/`snapshot-restore` need
  the *effective* file set as of a given event, computed by **folding that
  event's ancestry** — start from an empty `path → hash` map, walk `parent`
  pointers back to genesis, then apply each event's `changed` entries (insert
  or overwrite) and drop its `removed` paths on the way forward. This is
  proportional to ancestry length × average delta size, not total file count ×
  event count. It is a genuinely new shape for prov rather than an extension
  of an existing one: the id registry is a *sorted map* re-rendered wholesale
  — one record per line, sorted by id (`prov/src/index.rs:236`, `:495`) — not
  an append log anything folds. A blob referenced by more than one event's
  effective state is stored once regardless.

### The store is append-only at the filesystem level

The mechanism above is what makes snapshots survive the very thing they exist
to protect against. A single `manifest.<ext>` that every `snapshot-create`
appends to would be **the most merge-conflict-prone file in the workspace** —
rewritten on every device on every capture, under exactly the concurrent-sync
scenario this proposal is about. Content-addressed blobs merge perfectly
because nobody ever rewrites them; the fix is to give the record store the
same property:

- **One immutable document per event.** `snapshot-create` only ever *adds*
  files — a new event document plus any newly-seen blobs. It never modifies an
  existing one. Two devices capturing concurrently write two differently-named
  files, and added-file/added-file is the one merge case git, Dropbox,
  Syncthing and iCloud all handle without conflict.
- **Event documents carry no id.** A document with no id is legal — the
  `UnregisteredId` finding (`prov/src/validate.rs:267`) fires only when
  frontmatter claims an id the registry lacks. This matters: minting registry
  ids for events would make every `snapshot-create` write `registry.md`,
  reintroducing the same conflict on a *more* load-bearing file. Corrupting
  the snapshot index costs a rebuild; corrupting the registry costs the
  workspace's identity layer.
- **The index documents are a rebuildable cache.** They are the reachability
  entry point, so they must exist and must chain from the root — and they are
  therefore the only mutable files left. That is tolerable precisely because
  they are *derived*: authority lives in the event documents, and an index is
  recoverable by scanning the directory beneath it. A conflicted index is a
  `check` finding with a mechanical autofix, not data loss. prov already
  relies on this posture — under `id_storage: frontmatter` the registry is
  explicitly "a rebuildable cache" (`prov/src/config.rs:172`) with authority
  in the documents. Sharding by date (below) shrinks even that surface: only
  the current month's index is ever written.
- **The log is a DAG, and `parent` says so.** Making the store conflict-free
  does not make its *semantics* linear: two devices capturing concurrently are
  branches, and ordering them into one sequence by wall-clock is a fiction
  clock skew will eventually expose. Each event therefore records the head it
  captured against. `snapshot-show <id>` folds that event's ancestry, which is
  well-defined however the two devices' files interleave on disk;
  `snapshot-list` shows the fork honestly. There are no merge events — a
  concurrent branch stays a branch, and every event on it stays fully
  restorable, which is all a recovery tool owes anyone. The `parent` pointer
  *is* the causality record, so nothing here needs a device identity to mint,
  store, or lose.

#### Sharding events by date

Events accumulate for the life of the workspace and are never rewritten, so a
flat `snapshots/events/` is a directory that only grows. At a few captures a
day that is on the order of a thousand files a year — fine for a filesystem,
useless to browse, and prov is explicitly an archival tool where a decade is
the design horizon. Events therefore live at
`snapshots/events/<YYYY>/<MM>/<id>.<ext>`, with an index document at each
level. Three properties, beyond tidiness:

- **The mutable surface shrinks from "forever" to "this month."** A single
  flat index would be rewritten by every capture on every device for the life
  of the workspace. Per-month shards freeze the moment the month ends —
  permanently immutable, never a merge candidate again. Only the newest shard
  is hot, which extends the append-only property the events already have to
  the one mutable file the design still needs.
- **Every directory stays orphan-checked.** `check` only scans directories
  that *directly contain* something reachable (`reached_dirs` is the parent
  set of the reachable files, scanned non-recursively —
  `prov/src/workspace.rs:530`, `:495`). A bare `2026/` holding only
  subdirectories would be a blind spot: a stray file dropped there is
  invisible. An index document at every level keeps the whole subtree in
  scope, with no special-casing in the validator.
- **`id` → path stays a pure function.** The id already begins `YYYY-MM-DD`,
  so the path is parsed straight out of the id — no lookup table, no index
  consulted. `prov snapshot-restore 2026-07-23-1410-pre-sync-4f2a` resolves
  with every index file destroyed, which is what makes "the index is only a
  cache" true rather than aspirational: recovery never depends on it.

The layout is uniform even when it is sparse. A user who snapshots monthly
gets directories holding a single file, which is harmless; the alternative — a
flat layout that switches to shards past some threshold — would have to
*move* existing events, breaking both their immutability and the id-to-path
identity above. A rule with no exceptions is worth more than the saved
directories.

Blobs shard too, by hash prefix rather than date (`blobs/<first-2-hex>/`), for
the same reason and by the same convention: one store fans out by content, the
other by time.

#### Event ids are for humans

Because `parent` carries causality, the id's timestamp is a **pure human
affordance** — nothing sorts by it for correctness. That frees it to be
optimized for reading:

```
snapshots/events/2026/07/2026-07-23-1410-pre-sync-4f2a.md
```

Date, time to the minute, the `--label` slugified (omitted when absent), and
four hex characters. Full RFC 3339 precision lives in `created:`; minute
granularity in the filename is plenty when it is only ever read by a person.

The date is deliberately repeated in the filename rather than left implicit in
the `2026/07/` path: the id has to stay a **standalone token** the CLI accepts
and a human can quote out of context, and the redundancy is what makes the
id-to-path derivation above work in reverse — copy an event document anywhere
and it still says what it is.

The suffix is the **first 4 hex of the SHA-256 of the event's own canonical
content**, not a random value. Three reasons that fits: prov already has a
dependency-free SHA-256 and no RNG; the library stays clockless and
deterministic, taking its timestamp as an argument exactly as `recycle` does
(`prov/src/mutate.rs:903-909`); and it makes collisions *benign* rather than
dangerous. Two devices producing byte-identical events at the same minute
produce the same filename holding the same content, which merges as one file —
convergence, not collision. Any real difference yields a different suffix.

Legibility is the point of the whole layout, not a garnish: someone who opens
`snapshots/` uninvited should find a prose index explaining what it is,
event documents that name what they captured and print the command to roll it
back, and only then an opaque `blobs/`. The blobs stay content-addressed —
mirroring original paths the way `recyclebin/items/` does would break dedup,
which is the one thing making this affordable — but each event maps
`path → hash`, so a human can always walk from "what changed" to the bytes.
- **`check` validates it** like any other reachable store, with three new
  `Finding` variants added alongside the existing enum
  (`prov/src/validate.rs:183`), same posture as the registry's `DanglingId`:
  - `SnapshotBlobMissing` — an event names a hash with no blob behind it. Its
    wording must admit two causes: real loss, and a sync still in flight (a
    foreign event document is small and arrives long before a large blob).
    Both are indistinguishable without device ids, which is the trade taken
    deliberately elsewhere; a finding that cries corruption at a routine,
    self-resolving state is one users learn to ignore.
  - `SnapshotBlobOrphaned` — a blob no event's effective state references,
    computed by folding *every* head's ancestry, not any single event's
    delta. Orphan detection therefore folds the whole DAG on every `check` —
    bounded by event count × delta size rather than workspace size, but worth
    stating plainly because `check` runs far more often than `snapshot-show`.
  - `SnapshotIndexStale` — a shard directory holds an event its index does not
    link, or an index links one that is gone. Because the indexes are a
    derived cache, this is the *expected* outcome of a transport mangling one,
    and it autofixes by rebuilding that index from its own directory — the
    same confirmation-gated posture as `UnregisteredId`'s adopt-into-registry
    fix. Repair is per-shard and local: a mangled `2026/07/index.<ext>` is
    rebuilt from the events beside it without reading, or risking, any other
    month. An event document that fails to parse is a plain `Unreadable`,
    unchanged.

### What `restore` actually protects — and what it doesn't

This is the question the earlier draft under-specified, and it's the one that
actually determines whether this proposal works: **what does "protect
structural mutations" mean for `restore`, concretely?**

The threat is specifically structural — DESIGN's own framing is that a
rename/move/delete "touches several files at once" (the node, every inbound
link, the parent's child list, the registry). That has direct consequences for
restore's design that the original draft left implicit:

- **A snapshot event is only a *consistent* pre-image across the files it
  captured together.** If a bad merge corrupted both a renamed file and its
  parent's child list, both were reachable and hashed in the same
  `snapshot-create` pass, so both have pre-images in the same event.
  Restoring the **whole snapshot** — the default — puts every one of those
  files back together, which is what actually undoes the damage.
- **Restore is *additive* by default; `--exact` makes the tree match.** The
  default writes every captured path and **deletes nothing**. That leaves a
  gap on purpose: bad-merge damage is characteristically *additive* — a
  Syncthing `.sync-conflict-…` copy, a rename-vs-rename landing both the old
  and the new name, a duplicated child entry — and none of those go away by
  restoring captured bytes over the top. `--exact` additionally removes paths
  the snapshot does not contain, making the reachable tree match the event
  exactly. That is the honest "undo this merge entirely" tool, and it is
  gated, loud, and never the default, because the same delete pass also
  discards legitimate work done since the capture. Either way `snapshots/`
  itself is never written or deleted (see the capture set above) — an
  `--exact` restore that pruned uncaptured paths would otherwise delete every
  event newer than the one being restored.
- **Restoring a single path (`snapshot-restore <id> <path>`) is a
  content-recovery tool, not a structural-repair tool.** Putting one file's
  old bytes back without also restoring whatever else the same corruption
  touched can *reintroduce* the inconsistency snapshots exist to fix — e.g.
  restoring a renamed file's bytes at its old path while the parent's
  (unrestored) child list still points at the new path leaves a dangling
  entry. Scoped restore is the right tool when a sync mangled one file's
  *content* (a Dropbox `.conflicted copy` clobbering prose); it is the wrong
  tool for "the graph itself" broke. The CLI help text and any docs must say
  this plainly rather than leave it implied by the flag's mere existence.
- **`restore` does not repair links or the registry itself — it defers to
  what already does.** §4/§8 already define what "the graph is inconsistent"
  means (`Finding::BrokenLink`, `DuplicateContainment`, `MissingInverse`,
  `DanglingId`, … — `prov/src/validate.rs:183`), and `Workspace::check`
  (`prov/src/validate.rs:694`) already walks the reachable set to find it.
  Rather than inventing a second, snapshot-specific repair pass, `restore`
  defers to it — surfacing drift immediately instead of relying on the user to
  remember a separate `prov check`. This is diagnosis, not autofix: `check`'s
  existing confirmation-gated autofixes (missing inverse, id mismatch,
  unregistered id, fixity mismatch) stay the explicit next step, unchanged.

  **But it runs `check` twice and reports the difference**, because you are
  restoring *precisely when something is already broken*: a bare post-restore
  list is a wall of findings with no way to tell which the restore introduced,
  which it fixed, and which were always there. Checking before and after gives
  three buckets — **fixed**, **introduced** (the one that should drive the
  exit code), and **pre-existing** (a count, not a reprint). `Finding` derives
  `PartialEq, Eq` (`prov/src/validate.rs:182`), so this is set arithmetic, not
  new analysis.

  True **pre-flight** prediction — validating the projected tree before
  writing — is better still and deliberately out of scope: it needs `walk` to
  read through a staged `ChangeSet` the way `load_staged` already does
  (`prov/src/workspace.rs:806`), which would give *every* mutation an honest
  `--dry-run`. That is a general capability to build as one, not to smuggle in
  behind snapshots.
- **Some of what restore can predict needs no graph walk at all**, and those
  checks run up front, before any bytes move: a path collision, an id
  collision (below), a restored path whose parent is no longer present, and
  the plain counts of files to be created versus overwritten. All of that
  falls out of the folded effective state compared against what is on disk, so
  `restore` can refuse or prompt before doing anything rather than reporting
  it in the postmortem.
- **A restore can collide with the registry, not just the filesystem.** The
  existing "refuses to overwrite" guard (mirroring `recycle`/`restore`,
  `prov/src/mutate.rs:1194`) only checks whether something occupies the
  target *path*. Because `id_storage` defaults to `both` (frontmatter +
  registry, §9 status table), the path can be free while the *id* the
  restored file's frontmatter carries already resolves elsewhere (the
  document was re-created or renamed again since the snapshot was taken).
  Restore must check both a path collision and an id collision, and refuse
  either without `--force`.

- **What restore does not protect, stated plainly.** Snapshots do not make
  `empty_bin` reversible: binned bytes are outside the capture set on purpose
  (see the capture set above), so a snapshot taken while a document sat in the
  bin cannot bring that document's content back after a purge. `empty_bin`
  remains the workspace's one irreversible act, snapshots on or off. Users
  will assume otherwise unless told, which makes this CLI help text, not a
  footnote.
- **A foreign event is restorable, and that is a feature.** Nothing in an
  event is device-relative — hashes are absolute and blobs are
  content-addressed — so an event that arrived from another device folds and
  restores exactly like a local one. That is the recovery path when this
  device's copy is mangled and the other's is clean, and it costs nothing to
  support because the design already earned it by not minting device ids.
  **Known hazard**: the transport may deliver a small event document long
  before its large blobs, and capturing on top of a partially-arrived head
  inherits its holes — a path unchanged between that head and the new event is
  not re-recorded, so its bytes stay wherever they were, which is nowhere yet.
  Content-addressing makes this cheaply fixable when it matters (a working-tree
  file whose hash matches a missing blob can simply be parked, the hash
  proving the bytes), but that is a refinement to build on evidence, not
  Phase 0 machinery.

Given that, the concrete answer to "what's the best way to protect structural
mutations" is: **capture cheaply and broadly — the whole live graph, every
manual snapshot — and put the repair responsibility on the validation
machinery that already exists and is tested, rather than having restore
reinvent it.** Snapshots' entire job is making sure pre-merge bytes are still
around to fold back in as a *consistent set*; `check` already knows how to
tell the user what is wrong, and the before/after diff is what turns that into
an answer about *this restore* rather than about the workspace in general.

### Trigger: prov doesn't run the sync, so it can't hook into it

prov has no sync process of its own — sync is whatever external transport the
user already chose. That means snapshots can't be triggered automatically by
"before a merge happens"; there is no event to hook. The realistic trigger
surface:

- **Manual**: `prov snapshot-create` — the user (or a pre-sync script/git
  hook they wire up themselves) runs this before invoking their sync tool.
  This is the primary, and for v1 the *only*, trigger.
- **Deferred**: a documented recipe for wiring `prov snapshot-create` into a
  git `pre-push`/`pre-merge-commit` hook, or a Syncthing/Dropbox folder-watch
  script — left to the user's own tooling rather than prov reimplementing a
  file watcher. Revisit only if the manual command proves to be forgotten in
  practice.

### CLI surface

Every existing `prov-cli` command is a flat top-level verb: `Rm`/`Restore`/
`EmptyBin` are three separate `Command` variants
(`prov-cli/src/cli.rs:408-430`), not `recycle rm`/`recycle restore`; `Config`
takes positional args, not a subcommand (`prov-cli/src/cli.rs:498`). Nothing
in the CLI nests a subcommand under a noun today. Snapshots follows that
precedent rather than introducing the first nested-subcommand group — five new
top-level verbs, not one grouped `snapshot <verb>`:

- `prov snapshot-create [--label <text>]` — fold the chosen head's ancestry,
  hash the capture set, diff against it, park any newly-seen blob by hash,
  write a new event document into its `<YYYY>/<MM>/` shard naming that head as
  its `parent`, link it from that shard's index, print the snapshot id. Adds
  files only; modifies nothing but the current month's (rebuildable) index —
  and on the first capture of a new month, creates that shard's index and
  links it upward, which is again pure addition.

  **Choosing the head**: after a sync, `events/` may hold several heads, and
  there is deliberately no device-local state remembering which one is
  "mine" — that was the point of not minting device ids. Take the newest head
  by `created`. What makes that safe rather than arbitrary is dedup: picking a
  head another device wrote yields a *larger delta* (more paths differ from
  the current workspace), but nearly every one of those blobs is already
  parked, so **a badly-chosen parent costs manifest text, never bytes.**
  Correctness is unaffected either way, since restore folds whatever ancestry
  the event actually names.

  **Nothing changed**: if the diff is empty, write no event — print the
  existing head's id and stop. Otherwise a git hook or a habitual user fills
  the log with empty captures. Note this is only *possible* because the
  capture set excludes `snapshots/`; while the store captured itself, the
  churning index meant no capture was ever empty.
- `prov snapshot-list` — events newest first: one line each (id, timestamp,
  trigger, label, counts of files changed/removed), marking any point where
  the DAG forks so a concurrent capture on another device is visible rather
  than silently flattened into the sequence.
- `prov snapshot-show <id>` — the *effective* file set as of `<id>` (the
  folded result, not just that event's delta), for inspection before
  restoring.
- `prov snapshot-restore <id> [<path>...] [--exact]` — whole-snapshot by
  default, and **additive**: captured paths are written, nothing is deleted.
  `--exact` also removes paths the snapshot does not contain, making the tree
  match the event. Scoping to specific paths is a content-recovery escape
  hatch, not a structural-repair one (see
  [above](#what-restore-actually-protects--and-what-it-doesnt)). Refuses on a
  path *or* id-registry collision without `--force`, alongside the other
  up-front checks that need no graph walk. Runs `Workspace::check` before and
  after, reporting findings **fixed / introduced / pre-existing** rather than
  an undifferentiated post-restore list; a non-empty *introduced* bucket is
  what a non-zero exit code should mean. Never writes or deletes inside
  `snapshots/`.
- `prov snapshot-prune [--keep <n>]` — drop the oldest events and
  now-unreferenced blobs; manual, never automatic, so nothing disappears
  without the user asking. Must fold forward before deleting: a `changed`
  entry in a pruned event can still be load-bearing for a later event's
  effective state (see [open question 3](#open-questions)) — naively deleting
  the event and only *its own* blobs can silently break `snapshot-show`/
  `snapshot-restore` on events that survive the prune. The DAG makes the
  re-anchoring well-defined: fold the dropped prefix into the oldest surviving
  event and rewrite its `parent` to nothing (a new genesis). This is the one
  operation that rewrites an existing event document, so it is also the one
  that can conflict under concurrent sync — an acceptable trade for an
  explicitly-invoked hygiene command, but a reason `prune` must never become
  automatic.

## Design: `backup` (separate, deliberately simple)

- `prov backup --to <path>` — copy (or, with `--zip`, archive) the whole
  workspace tree to an arbitrary filesystem location. No relation, no
  manifest, no reachability rule, no dedup — the entire point is surviving
  loss of the workspace's own location, so it must not depend on anything
  living inside that location.
- Not config-gated the way snapshots/recycle-bin are, since it's an
  imperative action a user runs when they want it, not a standing behavior
  prov applies on every mutation.

## Config axis

Mirrors the `fixity`/`recycle_bin` precedent — a named axis in the config
document / root `prov:` block:

- `snapshots: off | manual` — a small closed enum on `WorkspaceConfig`, the
  same shape as `Fixity` (`Off | Payloads | Full`, `prov/src/config.rs:211`,
  held in `pub fixity: Fixity` at `config.rs:320`) rather than a raw string. A
  `Snapshots` enum (`Off | Manual`) alongside `pub recycle_bin: bool`
  (`config.rs:317`) follows the same pattern: parsed in
  `WorkspaceConfig::apply` (~`config.rs:620`), serialized back
  (~`config.rs:751`), and added to the axis-key list `check`'s config linter
  already walks (~`config.rs:807`, `882`) — so a misspelled `snapshots: manaul`
  becomes a near-miss finding for free, same as any other axis.
  Default **off** for v1 (unlike `recycle_bin`, which defaults on): snapshots
  add ongoing storage the user hasn't necessarily asked for, and the
  manual-only trigger means an "on" default buys nothing until the user is
  already in the habit of running `prov snapshot-create` — better to have them
  opt in deliberately. (`manual` is the only value while the trigger surface
  is manual-only; the axis is still worth having now so a future automatic
  trigger is a new value, not a new key.)

**When the transport is git, leave it off.** This is worth saying outright
rather than leaving users to infer it: git already stores every pre-image,
already dedupes by content hash, and already reconciles concurrent histories
as a DAG — snapshots would duplicate all three, inside a repository, where
every parked blob becomes another committed object. The feature earns its keep
where the transport keeps no history of its own: Dropbox, Syncthing, iCloud,
a synced network share. Naming the case where the answer is "you already have
this" makes the default-off posture a considered scope, not timidity, and it
is the single most useful sentence the eventual docs can carry.

`backup` has no config axis — see above.

## Rejected / non-goals

- **A dotfolder** — contradicts DESIGN §1. See above.
- **Folding snapshots into the recycle bin** — different trigger, different
  meaning of "restore"; kept as a sibling.
- **A single append-to `manifest.<ext>`** — the obvious shape, and the one
  that quietly breaks under the exact scenario this proposal addresses. See
  [the store is append-only](#the-store-is-append-only-at-the-filesystem-level).
- **Device identity for snapshot events** — sharding the record store per
  device would work, but every way of minting a device id is worse than not
  needing one. A `~/.config/prov/id` is not the `.obsidian/` anti-pattern (it
  is outside the workspace), but it is a *durability* dependency: lose it to a
  reinstall or a fresh container and history forks silently, undetectably.
  Hostname- or MAC-derived ids are unstable *and* leak machine identifiers
  into a plaintext file that gets synced. Delegating identity to the library
  consumer suits the `IndexStore`/`IdentityPolicy` seams in general, but buys
  nothing here, since prov-cli is a consumer too and would still need an
  answer — a seam the reference implementation cannot satisfy is a deferral,
  not a seam. Immutable event documents plus a `parent` pointer get the same
  guarantees with nothing to mint, store, or lose. (A future `origin:` field
  for display-only device attribution stays possible, and must stay
  non-load-bearing for the fold.)
- **A shared blob/record-store *primitive* extracted from the recycle bin.** The
  recycle bin's `recycle`/`restore`/`empty_bin` (`prov/src/mutate.rs:905-1310`)
  are single-document operations built around one tombstone record per
  delete, wired directly to the spanning relation and parent-link editing.
  Snapshots' unit of work is different — hash the whole reachable set, diff
  against a fold of prior events, park by content hash — so there is no
  reusable *code* here, only a reusable *shape* (visible directory, unreached
  blob store, reachable records `check` validates). No extracted
  shared primitive is scoped into Phase 0; the recycle bin's tested code is
  left untouched.
- **A full operation/event log as the primary mechanism** — protects the
  wrong layer (prov's own mutations, already covered by the crash journal),
  not the real threat (external sync damage). Not ruled out forever — DESIGN's
  open question #1 leaves the door open behind `IndexStore` — but out of scope
  here.
- **Automatic snapshot-before-sync** — prov has no sync process to hook into;
  automating this means reimplementing a file watcher per transport, which is
  a different, much larger project. Manual command only, for now.
- **Full-tree ZIP as the snapshot format** — opaque, no dedup, duplicates git.
  Kept for `backup`, where opacity and simplicity are exactly what's wanted.

## Open questions

1. ~~Should `snapshot restore` go through the same journaled `ChangeSet` +
   crash-atomic write path as every other mutation?~~ **Resolved: yes, and
   it's free.** `Workspace::commit` (`prov/src/workspace.rs:842`) and
   `Workspace::change` (`workspace.rs:800`) are already generic — every
   mutation, including `recycle`'s own `restore`, stages a `ChangeSet` and
   calls `commit`, which is where the journal (`.prov-journal`) and crash
   atomicity live (§9). Snapshot restore isn't a special case needing new
   atomicity work; it stages its file writes through the same path recycle
   bin's `restore` already uses (`mutate.rs`, the `restore` function around
   line 1140).
2. ~~Does a snapshot need to record the id registry's state at capture time?~~
   **Resolved: no.** See [What `restore` actually protects](#what-restore-actually-protects--and-what-it-doesnt)
   above — restore's contract is byte-recovery plus surfacing
   `Workspace::check` findings, not registry surgery. The one id-specific
   hazard restore introduces (a path that's free but whose id now resolves
   elsewhere) is handled by the registry-collision guard described there, not
   by recording extra registry state in an event.
3. **Retention policy defaults for `prune`** — keep-forever until asked, or a
   sensible default cap (e.g. last 20)? Needs a real usage pattern to answer
   well; deferred to after Phase 0 ships. The *mechanism* is settled (fold the
   dropped prefix into the oldest surviving event and re-anchor it as a new
   genesis; a naive "delete the event and its own blobs" is silently
   corrupting), but two questions remain: what the default `--keep` is, and
   what `prune` does with a **forked** DAG — dropping a branch's tip is
   discarding a recovery point that only ever existed on one device, which
   argues for pruning by ancestry depth per head rather than by global event
   count.
4. **Should `snapshot-create` warn when `check` is already dirty at capture
   time?** A snapshot of an already-broken workspace is still useful (the
   user may be capturing deliberately, mid-fix), so it shouldn't *refuse* —
   but silently snapshotting known-bad state could give false confidence that
   "the last snapshot is a safe rollback point." Leaning toward: run
   `Workspace::check` as part of `snapshot-create` too, and print (never
   block on) findings, symmetric with how `snapshot-restore` ends.

## Phasing

- **Phase 0 — event store + create + list.** `snapshots` relation, visible
  directory, the per-event document format *and its fold/reconstruction logic*
  (`snapshot-show` depends on folding even before restore exists — worth
  proving in Phase 0 rather than deferring the hard part), the date-sharded
  layout with per-shard indexes as a rebuildable cache and their
  `SnapshotIndexStale` rebuild, `prov snapshot-create`/`snapshot-list`. Fold
  along `parent` from the start: a linear-log shortcut here is exactly the
  assumption that fails on the second device. Shard from the start too: the
  `id`-to-path derivation is part of the format, and retrofitting it means
  moving event documents that are supposed to be immutable. No restore yet —
  this proves the capture and reconstruction path before anything depends on
  it. No recycle-bin refactor is implied (see
  [Rejected/non-goals](#rejected--non-goals)).
- **Phase 1 — restore.** `prov snapshot-show`/`snapshot-restore`, additive by
  default with `--exact`, scoped-path restore (documented as content-only, not
  structural-repair), the up-front collision guards, and the before/after
  `check` diff reporting fixed / introduced / pre-existing. Open question 1
  (crash atomicity) is resolved, not deferred work — restore reuses
  `Workspace::commit` as-is. Staged-`ChangeSet` validation for true pre-flight
  `--dry-run` is explicitly *not* here: it is a general capability for every
  mutation and should be built as one.
- **Phase 2 — hygiene.** `prov snapshot-prune` (including the fold-forward
  re-anchoring from open question 3), the `check` findings for missing and
  orphaned blobs (`SnapshotBlobMissing`/`SnapshotBlobOrphaned`), and a
  documented git-hook recipe for semi-automatic triggering.
  `SnapshotIndexStale` lands earlier, in Phase 0 — the index is a rebuildable
  cache from the first commit or it is not one at all.
- **Deliberately unscheduled.** Three refinements are correct but should be
  built on evidence rather than anticipation, and none touches the format:
  copy-on-write blob cloning; re-parking a missing blob from a working-tree
  file whose hash matches; and staged-`ChangeSet` validation for a true
  pre-flight `--dry-run` (which belongs to every mutation, not to this one).
- **Not in this proposal.** A `check` finding for a recycle-bin record whose
  parked bytes are gone. Real, and reachable without snapshots ever existing —
  so it is a recycle-bin change on its own footing.
- **`backup`** ships independently at any point — no dependency on the
  phases above.
