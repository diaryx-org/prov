---
part_of: '[`prov` proposals](/docs/proposals/proposals.md)'
---
# History — a versioned safety net for the workspace

> Working proposal, third draft. Supersedes `proposal-snapshots.md` (v1) and
> `proposal-snapshots-v2.md`. Complements DESIGN §5 (the index: one artifact,
> two natures), §8 (validation as the sleeper feature), and the recycle bin
> (`docs/DESIGN.md` row 426), and answers part of open question #1 — "full
> history/event-log stores remain possible behind `IndexStore`, e.g. for
> sync."

## The design in brief

A reachable `history/` directory off the root holds one **immutable event
document per capture**: a full manifest of every reachable file
(`path → (id?, hash)`), plus a content-addressed **blob store** holding the
bytes, deduplicated by SHA-256. `prov history-capture` hashes the live graph
(minus `history/` itself and `recyclebin/items/`), parks any unseen bytes,
and writes one new file. Nothing is ever rewritten except a rebuildable
per-month index, so two devices capturing concurrently produce two
differently-named files that every sync transport merges without conflict.
`prov history-restore` writes a captured state back — additive by default,
exact on request — and runs `check` before and after, reporting what the
restore fixed, introduced, and inherited. `prov history-log` derives
per-document lineage from the manifests' id column. OCFL is supported as an
**export format** ([below](#ocfl-an-output-format-not-the-store)), never as
the live store.

Everything below is the argument for those sentences.

## Lineage

v1 recorded *deltas* per event, reconstructed by folding ancestry. Review
showed the fold made the `parent` DAG load-bearing and sync-hostile — a
transport can deliver a child before its parent, and `prune` had to rewrite
"immutable" events (see [Why not a delta
log](#why-not-a-delta-log-the-v1-design)). v2 moved to full manifests per
event, dissolving the fold, the head-choosing rule, and prune's
re-anchoring. v3 makes three changes: the feature is named **history**
(directory, relation, config axis, and verbs — the frozen compatibility
contract — all follow); `backup`, which v2 said should ship first, **has
shipped** (`prov-cli/src/backup.rs`, with a hand-rolled store-only ZIP in
`prov-cli/src/zip.rs`) and is no longer proposed here; and the OCFL
question, opened when the sister crate `mocfl` made an OCFL store
buildable, is settled: **output, not store**.

## The problem

prov workspaces are plaintext, so the obvious way to sync one across devices
is to point an existing transport (git, Dropbox, iCloud, Syncthing) at the
directory and let it reconcile files. That's free for ordinary content
edits — plain text merges fine.

It is not free for **structural** mutations. A rename, move, or delete
touches several files at once (the node itself, every inbound link, the
parent's child list, the id registry). If two devices perform structural
mutations concurrently, the sync transport reconciles the *bytes* with no
idea about prov's graph — the result can look like a clean merge and still
be semantically broken (stale links, duplicate containment, a dangling
registry entry), or a transport-level conflict (a Dropbox "conflicted
copy", a Syncthing `.sync-conflict` file, a botched git merge) can mangle a
file outright.

Today there is no safety net for this. The crash journal (`.prov-journal`)
protects a single device against its own interrupted writes; it has nothing
to say about damage introduced by an external sync tool reconciling two
devices. The recycle bin protects against an explicit, single-device
recoverable delete. `backup` protects against losing the workspace's
location entirely, but a backup is a whole opaque tree — it cannot answer
"which files did yesterday's merge break, and what did each look like
before." Neither covers "a merge silently broke something and I want
yesterday back," file by file.

**Audience honesty.** When the transport is git, history should stay off —
git already stores every pre-image, dedupes by content, and reconciles
concurrent histories. The feature earns its keep where the transport keeps
no history: Dropbox, Syncthing, iCloud, a synced network share. For that
audience, "just use git" is worse than it sounds — a `.git` directory
inside a cloud-synced folder is a known corruption factory, an out-of-tree
`GIT_DIR` is exactly the app-private sidecar state this project defines
itself against, and embedding or shelling out to git contradicts a codebase
that hand-rolled SHA-256 to stay dependency-free. The audience is real
*and narrow*, which is why the axis defaults off.

## Design: the history store

Follows the recycle bin's *shape* — park bytes unreached, keep a reachable
record, let `check` validate it — as a **sibling**, not an extension, and
as a convention rather than shared code (see [Rejected /
non-goals](#rejected--non-goals)).

- **A new pointer relation off the root**, `history` (alongside `registry`,
  `recycle_bin`, `config`) — one-way, no `part_of` back-link. `RelationSet`
  already exposes `registry_relation`/`config_relation`/`recycle_relation`
  as siblings (`prov/src/relation.rs:255-265`); `history_relation` is a
  fourth, discovered the way `Workspace::recycle_bin_path` discovers the
  bin (`prov/src/workspace.rs:292-297`) — a new `Workspace::history_path`
  follows that pattern.
- **A visible directory**, `history/`, holding:
  - `history/index.<ext>` — the reachable entry point: a prose document
    explaining what the directory is, linking the year shards below. A
    rebuildable cache, not the authority.
  - `history/events/<YYYY>/<MM>/` — one **immutable document per event**,
    sharded by date, with an ordinary prov index document at each level
    (`events/2026/index.<ext>`, `events/2026/07/index.<ext>`) so the whole
    subtree stays reachable and orphan-checked. See
    [sharding](#sharding-events-by-date).
  - `history/blobs/` — **unreached**, so §8's orphan check ignores it
    exactly as it already ignores `recyclebin/items/` (implicitly, by
    reachability — nothing links in, so its directories never enter
    `reached_dirs`, `prov/src/workspace.rs:530`). Pre-image bytes live
    here, named by content hash (`history/blobs/<first-2-hex>/<rest>`) —
    **bare hex, never the `sha256:` scheme prefix an event spells**: a
    colon in a filename is hostile to Windows and to more than one sync
    client. Bytes are verbatim — never re-encoded, so a restore is
    byte-exact.

Legibility is the point of the layout, not a garnish: someone who opens
`history/` uninvited should find explanation, not opacity, all the way down
to (and only to) `blobs/`.

### Each event is a full manifest

An event document records, under a `files:` key in its frontmatter, **the
complete capture set at that moment**: one entry per file, sorted by path,
each carrying the path, the content hash, and — when the document is
registered — its id. A blob is parked only when its hash is not already
present under `blobs/`; the manifest may freely reference blobs parked by
earlier events or by other devices.

```markdown
<!-- history/events/2026/07/2026-07-31-0915-pre-sync-4f2a9c1e.md -->
---
part_of: '[July 2026](index.md)'
created: 2026-07-31T09:15:22Z
parent: 2026-07-30-1804-nightly-8c1d55aa
trigger: manual
label: pre-sync
files:
  - path: notes/foo.md
    id: b7k2m
    hash: sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
  - path: notes/photo.jpg
    hash: sha256:2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae
  # ... one row per captured file, sorted by path
---
# History — 2026-07-31 09:15 (pre-sync)

Captured 412 files (2 changed since the previous event) before syncing.
Roll the workspace back to this point with:

    prov history-restore 2026-07-31-0915-pre-sync-4f2a9c1e
```

Properties this buys, each lost or complicated under the delta design:

- **Every event is self-contained.** `history-show` reads one file.
  `history-restore` needs one file plus blobs. A foreign event restores
  even if the events before it never arrived.
- **`parent` is display metadata.** It names the newest event that existed
  locally at capture time, so `history-list` can show concurrent captures
  as the fork they are — but nothing computes through it. Clock skew,
  missing parents, and interleaved arrivals are cosmetic, not correctness
  hazards. Nothing here needs a device identity to mint, store, or lose.
- **"What changed" is computed at display time**, by comparing an event's
  manifest with its predecessor's — shown in `history-list` counts and in
  the prose body `history-capture` writes. If the predecessor is absent,
  the counts are simply omitted.
- **Removals need no bookkeeping.** A path absent from the manifest was not
  in the capture set; there is no `removed:` list to maintain.

The preservation world independently converged on this shape: an OCFL
version's file list is likewise the *complete* state, with omission
meaning deletion (`mocfl/src/object.rs`, `commit`). Two designs arriving at
full-state manifests from opposite directions — sync-safety here, archival
integrity there — is good evidence the delta design should stay rejected.

On format: the manifest lives in an ordinary markdown document's
frontmatter, with a prose body, rather than in a whole-file config store.
The `MalformedStore` rule (registry, bin index, vocabularies must be
whole-file stores) targets *mutable record stores prov edits in place*; an
event is an immutable document with something to say for itself.

**Empty captures write nothing.** If the manifest `history-capture`
computes is identical to the newest existing event's, it prints that
event's id and stops — otherwise a git hook or a habitual user fills the
log with duplicates. (Identical manifests from two devices capturing the
same state concurrently are harmless either way: two events describing the
same bytes, sharing every blob.)

### The capture set

**The live graph, minus prov's two byte-parking stores.** The capture set
is the reachable file set (§8's bounded walk — the same set `check`
validates), with two exclusions, each load-bearing:

- **`history/` itself.** It is reachable off the root, so a naive "capture
  everything reachable" would capture the store inside the store: every
  event would carry rows about prior events and the churned index, no
  capture could ever be empty, and — far worse — an `--exact` restore of an
  old event would delete every event newer than it, destroying the
  recovery points themselves. The history store is the one subtree the
  mechanism is deliberately blind to, and restore never writes or deletes
  inside it.
- **`recyclebin/items/`.** Already unreached, and excluded even so, on
  purpose: bytes the user has consigned to the bin should not be *newly*
  retained by a routine capture. Note what this does and does not buy —
  see [Retention and forgetting](#retention-and-forgetting-the-honest-version).

Everything else structural stays captured — the registry, the config
document, and the recycle bin's *index*. Capturing the bin index keeps the
common case correct: a document live at capture time comes back live, and
the bin index reverts to a state that does not list it. The narrow
residue — a restored bin index naming items whose bytes were since
purged — is a condition `check` should catch regardless of history (a bin
record whose parked bytes are absent), and that finding belongs in a
separate recycle-bin change on its own footing.

**To verify before Phase 0**: that attachment *payloads* (not just their
sidecars) are in the reachable set as capture requires — attachments are
both the dominant bytes and a prime corruption target, and the capture-set
definition must be exact in the format spec, not discovered during
implementation.

### The store is append-only at the filesystem level

A single manifest file that every capture appends to would be the most
merge-conflict-prone file in the workspace — rewritten on every device on
every capture, under exactly the concurrent-sync scenario this proposal is
about. Content-addressed blobs merge perfectly because nobody rewrites
them; the record store gets the same property:

- **One immutable document per event.** `history-capture` only ever *adds*
  files — a new event document plus newly-seen blobs. Two devices capturing
  concurrently write two differently-named files, and
  added-file/added-file is the one merge case git, Dropbox, Syncthing and
  iCloud all handle without conflict.
- **Event documents carry no id.** A document with no id is legal —
  `UnregisteredId` (`prov/src/validate.rs:267`) fires only when frontmatter
  claims an id the registry lacks. Minting registry ids for events would
  make every capture write `registry.md`, reintroducing the conflict on a
  *more* load-bearing file. (The manifest's `id` column records the ids of
  *captured* documents; events themselves have none.)
- **The index documents are a rebuildable cache** — the reachability entry
  point, so they must exist and chain from the root, and therefore the
  only mutable files in the store. Tolerable precisely because they are
  derived: authority lives in the event documents, and an index is
  recoverable by scanning the directory beneath it. A conflicted index is
  a `check` finding with a mechanical autofix, not data loss — the same
  posture as the registry under `id_storage: frontmatter`, which is
  explicitly "a rebuildable cache" (`prov/src/config.rs:172`).

#### Sharding events by date

Events accumulate for the life of the workspace and are never rewritten;
prov is explicitly an archival tool where a decade is the design horizon.
Events therefore live at `history/events/<YYYY>/<MM>/<id>.<ext>`, with an
index document at each level. Three properties beyond tidiness:

- **The mutable surface shrinks from "forever" to "this month."** Per-month
  shard indexes freeze when the month ends; only the newest shard is hot.
- **Every directory stays orphan-checked.** `check` scans only directories
  that directly contain something reachable (`reached_dirs` /
  `direct_child_files`, `prov/src/workspace.rs:530`, `:495`,
  non-recursive). An index document at every level keeps the whole subtree
  in scope with no validator special-casing.
- **`id` → path stays a pure function.** The id begins `YYYY-MM-DD`, so the
  path parses straight out of the id — `history-restore` resolves with
  every index file destroyed, which is what makes "the index is only a
  cache" true rather than aspirational.

The layout is uniform even when sparse — a monthly capturer gets
directories holding one file, which is harmless; a threshold-switching
layout would have to *move* immutable events. Blobs shard too, by hash
prefix (`blobs/<first-2-hex>/`): one store fans out by content, the other
by time.

#### Event ids are for humans

Nothing sorts by the id's timestamp for correctness, which frees it to be
optimized for reading:

```
history/events/2026/07/2026-07-31-0915-pre-sync-4f2a9c1e.md
```

Date, time to the minute, the `--label` slugified (omitted when absent),
and eight hex characters — the first 8 of the SHA-256 of the event's own
canonical content. Full RFC 3339 precision lives in `created:`. The date is
repeated in the filename rather than left implicit in the `2026/07/` path
so the id stays a standalone token: quotable out of context, and
reversible to its path.

A content-derived suffix (not random) fits because prov has a
dependency-free SHA-256 and no RNG; the library stays clockless and
deterministic, taking its timestamp as an argument exactly as `recycle`
does (`prov/src/mutate.rs:902-909` — the repo's only clock is the CLI's
`now_rfc3339`). And it makes collisions *benign*: two devices producing
byte-identical events yield the same filename holding the same content —
convergence, not conflict. Two *different* same-minute, same-label events
get different suffixes; eight characters make an accidental match
negligible.

### `check` validates the store

Three new `Finding` variants alongside the existing enum
(`prov/src/validate.rs:183`), same posture as the registry's `DanglingId`:

- `HistoryBlobMissing` — a manifest names a hash with no blob behind it.
  Wording must admit two causes: real loss, and a sync still in flight (a
  small event document arrives long before a large blob). A finding that
  cries corruption at a routine, self-resolving state is one users learn
  to ignore. (A third cause after Phase 2: the blob was deliberately
  forgotten — see [Retention and
  forgetting](#retention-and-forgetting-the-honest-version) — which `check`
  distinguishes via the forget list and reports as informational, not as
  loss.)
- `HistoryBlobOrphaned` — a blob no manifest references. With full
  manifests this is plain mark-and-sweep — union every event's `files`
  hashes, subtract from the blob listing — bounded by event count ×
  manifest size. Expected transiently (a blob can arrive before its
  event); `history-prune` and `history-forget` are the durable producers,
  and both clean up after themselves, so a persistent orphan is worth
  reporting.
- `HistoryIndexStale` — a shard directory holds an event its index does
  not link, or links one that is gone. The *expected* outcome of a
  transport mangling a derived cache; autofixes by rebuilding that index
  from its own directory — the same confirmation-gated posture as every
  existing fix (missing inverse, id mismatch, unregistered id, fixity
  restamp, `StaleLabel → RelabelLink`), applied per-shard so a mangled
  `2026/07/index.<ext>` is rebuilt without touching any other month.

An event document that fails to parse is a plain `Unreadable`, unchanged.
No `HistoryParentMissing` finding exists because no correctness depends on
`parent` — a hole in the display ancestry is cosmetic.

### What `history-restore` protects — and what it doesn't

The threat is specifically structural: a rename/move/delete "touches
several files at once." Consequences for restore:

- **An event is a *consistent cut* across every file it captured
  together.** If a bad merge corrupted both a renamed file and its
  parent's child list, both were hashed in the same capture, so both are
  in the same manifest. Restoring the **whole event** — the default — puts
  the set back together, which is what actually undoes the damage.
- **Restore is *additive* by default; `--exact` makes the tree match.**
  The default writes every captured path and deletes nothing. That leaves
  a gap on purpose: bad-merge damage is characteristically additive (a
  `.sync-conflict` copy, a rename-vs-rename landing both names, a
  duplicated child entry), and none of it goes away by writing captured
  bytes over the top. `--exact` additionally removes reachable paths the
  manifest does not contain — the honest "undo this merge entirely" tool:
  gated, loud, never the default, because the same delete pass discards
  legitimate work done since the capture. Either way `history/` itself is
  never written or deleted, **and the root's `history` pointer relation is
  never removed by a restore** — a captured root predating some hand-edit
  must not strand the store unreachable.
- **Scoped restore (`history-restore <id> <path>...` or `--id <docid>`) is
  a content-recovery tool, not a structural-repair tool.** Restoring one
  file's old bytes without the rest of the same corruption's footprint can
  *reintroduce* the inconsistency history exists to fix. Right tool when a
  sync clobbered one file's prose; wrong tool when the graph broke. The
  CLI help must say this plainly.
- **Restore does not repair links or the registry — it defers to what
  already does.** §8 already defines graph inconsistency (`Finding`
  variants) and `Workspace::check` (`prov/src/validate.rs:694`) already
  finds it. Restore runs **`check` before and after and reports the
  difference** — you are restoring precisely when something is already
  broken, and a bare post-restore list cannot distinguish what the restore
  fixed, introduced, or inherited. Three buckets: **fixed**, **introduced**
  (drives the exit code), **pre-existing** (a count, not a reprint).
  `Finding` derives `PartialEq, Eq` (`prov/src/validate.rs:182`), so this
  is set arithmetic. Existing confirmation-gated autofixes remain the
  explicit next step. True pre-flight prediction — validating the
  projected tree before writing — needs `walk` to read through a staged
  `ChangeSet` the way `load_staged` already does
  (`prov/src/workspace.rs:806`); that is a general `--dry-run` capability
  for *every* mutation, built as one, not smuggled in here.
- **Up-front guards that need no graph walk** run before any bytes move: a
  path collision, an **id collision** — because `id_storage` defaults to
  `both`, the target path can be free while the id the restored
  frontmatter carries already resolves elsewhere; today's guard checks
  only the path (`prov/src/mutate.rs:1192-1197`), and `register` silently
  overwrites (`prov/src/index.rs:177`), so restore must check both and
  refuse either without `--force` — a restored path whose parent directory
  is gone, and plain counts of files to create versus overwrite. All of it
  falls out of comparing the manifest against disk.
- **A foreign event is restorable, and that is a feature.** Nothing in an
  event is device-relative — paths are workspace-relative, hashes
  absolute, blobs content-addressed. The one hazard is a manifest whose
  blobs have not all arrived yet: restore reports exactly which paths lack
  bytes (`HistoryBlobMissing`'s two causes apply) and either proceeds
  partially with a clear report or refuses under `--exact`.
  Content-addressing also makes re-parking cheap when it matters — a
  working-tree file whose hash matches a missing blob can simply be
  parked, the hash proving the bytes — a refinement to build on evidence,
  not Phase 1 machinery.

### Retention and forgetting: the honest version

**History extends retention of everything ever captured.** A document's
lifecycle is typically live → `recycle` → `empty_bin` (or live →
`rm --purge`, already a hard delete with no bin involvement,
`prov/src/mutate.rs:790`). If any event was captured while it was live —
the common case, since routine pre-sync capture is the whole point — its
bytes are in `blobs/`, and neither `empty_bin` nor `delete` touches them.
`history-restore` brings them back. So:

- The `recyclebin/items/` capture-set exclusion prevents a capture from
  *newly* parking already-binned bytes. It does **not** make purges final
  for content captured earlier, and no wording may imply it does.
- With history on, the workspace's irreversible acts are `empty_bin` and
  `rm --purge` **only for content never captured live**, plus
  `history-prune`/`history-forget` below. This goes in the CLI help beside
  the purge warning, not in a footnote — a user purging something
  sensitive must not be misled.
- **`history-forget <path|id>`** is the deliberate-destruction tool the
  full-manifest design makes tractable: delete every blob whose hash
  appears for that path/id across all manifests (and nowhere else), and
  record the forgotten hashes in `history/forgotten.<ext>` — a small
  whole-file tombstone store, mirroring the registry's `id: null`
  tombstone precedent — so events stay immutable and `check` can tell
  "forgotten on purpose" (informational) from "lost"
  (`HistoryBlobMissing`). The forget list is a mutable file and can
  conflict under sync; acceptable for an explicitly invoked, rare act of
  destruction. A restore that encounters a forgotten hash reports it by
  name.
- **`history-prune`** is the routine-hygiene counterpart: drop the oldest
  events, then garbage-collect blobs no surviving manifest references,
  then rebuild affected shard indexes. With full manifests this is
  delete + GC — no folding, no re-anchoring, no rewriting of surviving
  events. Manual, never automatic.

### `history-log`: lineage by id

The manifest's `id` column makes per-document version history a **derived
query, not a storage design**: `prov history-log <id>` walks the events,
pulls that id's row from each manifest, and dedupes consecutive hashes — a
rename-robust lineage of one document across every capture, stitched by
the identity layer doing exactly what it was built for (a path-keyed view
shows a move as two unrelated lineages; the id column shows one document).
It is strictly stronger than path-continuity history — the best an
id-less store can do (mocfl's `history` follows logical paths through the
inventory; rename the file and the lineage relies on the path being
re-linked) — because prov's ids survive any rename.

This is deliberately *not* a per-id version store: per-id chains would
cover only registered documents (the config document, bin index, registry,
and attachments are disproportionately the sync-damage victims, and
disproportionately id-less), would give no consistent cut across files,
and would either reintroduce per-id append files (the conflict-prone
shape) or degenerate back into this design sharded differently. Store by
consistent cut; query by id. If document lineage later deserves
first-class treatment, DESIGN open question #1's event-log store behind
`IndexStore` is the home, and the id column here is its
migration-friendly foundation.

## OCFL: an output format, not the store

The sister crate `mocfl` (`~/Code/mocfl`) implements the OCFL 1.1 object —
the digital-preservation world's standard for exactly this shape of store:
full-state versions, content-addressed dedup, fixity throughout, a layout
designed to be understood without the software. That is prov's own value
system, and the convergence is real (see [Each event is a full
manifest](#each-event-is-a-full-manifest)). So the question had to be
asked: why not make OCFL the store and delete this format?

**Because a fork cannot be represented in OCFL.** The decision follows
from the concurrency analysis, not from taste:

- OCFL version numbers are linear and *required* to be contiguous
  (`v1..vN`) — a constraint mocfl's hand-mapped inventory parsing enforces
  deliberately (`mocfl/src/inventory.rs`). Two devices capturing
  concurrently both mint `v4`: not a conflict handled badly, but a state
  the data model has no words for. The extensions mechanism doesn't reach
  this — an extension that forked version numbering would invalidate the
  object against every conforming validator, forfeiting the only benefit
  of the standard.
- The root `inventory.json` (plus sidecar digest) is rewritten on every
  commit — precisely the most-merge-conflict-prone-file shape [the
  append-only design](#the-store-is-append-only-at-the-filesystem-level)
  exists to eliminate.
- The failure severities are asymmetric, and for a safety net the worst
  case is the design criterion. This design's worst concurrent case: two
  extra files sit side by side, `history-list` shows a cosmetic fork.
  OCFL's worst case: the transport union-merges two different `v4/`
  directories and produces conflicted inventories — damage landing
  *inside the recovery mechanism itself*.

**What OCFL is right for is the single-writer moment: export.** A
deferred `prov history-export --ocfl <path>` folds the event store into a
genuine OCFL object via mocfl — each event becomes a version, the blob
store provides the bytes, the manifests provide the state maps. Export
happens on one machine at one moment, which is OCFL's happy path, and it
buys standards-compliance exactly where it pays: archival deposit, handing
a corpus to an institution, interop with OCFL tooling. Notes:

- **mocfl is a `prov-cli` dependency only**, following the `backup`/ZIP
  precedent — the prov library stays dependency-free and the publication
  chain (diaryx cannot release until prov publishes) is not extended by an
  unpublished crate. mocfl's dependency posture fits regardless:
  hand-rolled SHA-256 pinned to FIPS 180-4 vectors (`mocfl/src/digest.rs`,
  the same tradition as `prov/src/fixity.rs`), no serde (inventories
  hand-map through fig's JSON tree, and both crates already depend on
  fig), clockless like prov's library.
- Export is one-way. A `history-import --ocfl` is conceivable but
  unscoped — nothing in this design depends on it.
- Deferred, but **decided in principle**: it appears in
  [Phasing](#phasing) as unscheduled work whose shape is settled, so
  nothing in Phases 0–2 should foreclose it. (Nothing does: events map to
  versions and blobs to content without loss.)

## Trigger: prov doesn't run the sync, so it can't hook into it

Sync is whatever external transport the user chose; there is no event to
hook.

- **Manual**: `prov history-capture` — run by the user, or by a pre-sync
  script/git hook they wire up themselves. The primary, and for v1 the
  only, trigger.
- **Deferred**: a documented recipe for a git `pre-push` hook or a
  Syncthing/Dropbox watch script — the user's tooling, not a prov file
  watcher. Revisit only if the manual command proves forgotten in
  practice.

## CLI surface

Every existing command is a flat top-level verb (`Rm`/`Restore`/`EmptyBin`
are separate `Command` variants, `prov-cli/src/cli.rs:408-430`; `Config`
takes positionals, `cli.rs:498`). History follows: flat verbs, no nested
subcommand group.

- `prov history-capture [--label <text>]` — hash the capture set, build
  the manifest, park newly-seen blobs, write one event document into its
  `<YYYY>/<MM>/` shard (recording the newest local event as `parent`, for
  display), link it from the shard's index, print the id. If the manifest
  equals the newest event's, print that id and write nothing. Adds files
  only; modifies nothing but the current month's rebuildable index (and,
  on a new month, creates the shard index and links it upward — pure
  addition). Also runs `check` and *prints* (never blocks on) findings: a
  capture of a known-broken workspace is still useful, but a
  silently-dirty "safe rollback point" is false confidence — symmetric
  with how restore ends.
- `prov history-list` — events newest first: id, timestamp, trigger,
  label, changed/removed counts vs. the named `parent` (omitted when the
  parent is absent), marking forks so a concurrent capture on another
  device is visible rather than silently flattened.
- `prov history-show <id>` — print the manifest (it *is* the effective
  state; no reconstruction), with per-file blob presence so a half-synced
  event is legible before anyone restores it.
- `prov history-log <id>` — per-document lineage, above.
- `prov history-restore <id> [<path>...] [--id <docid>] [--exact]
  [--force]` — semantics above. Non-empty *introduced* findings bucket ⇒
  non-zero exit.
- `prov history-prune [--keep <n>]` — delete + GC + index rebuild, above.
- `prov history-forget <path|id>` — deliberate destruction, above
  (Phase 2).
- `prov history-export --ocfl <path>` — deferred, above.

All history verbs stage their writes through the same journaled
`Workspace::change`/`commit` path as every mutation
(`prov/src/workspace.rs:800`, `:842`) — with one caveat the journal spike
must settle first (see [Before Phase 0](#before-phase-0)).

## Config axis

Mirrors the `fixity`/`recycle_bin` precedent — a named axis on
`WorkspaceConfig`:

- `history: off | manual` — a small closed enum, same shape as `Fixity`
  (`Off | Payloads | Full`, `prov/src/config.rs:211`), parsed in
  `WorkspaceConfig::apply`, serialized back, added to the `TOP_KEYS` list
  the config linter walks (`prov/src/config.rs:903-916`) — so a misspelled
  `history: manaul` becomes a near-miss finding for free.
- Default **off** (unlike `recycle_bin`, which defaults on): history adds
  ongoing storage the user hasn't asked for, and a manual-only trigger
  means an "on" default buys nothing until the user is in the habit
  anyway. `manual` is the only "on" value for now; the axis exists so a
  future automatic trigger is a new value, not a new key.
- **Semantics of `off`, stated:** `history-capture` refuses (with a
  pointer to the axis). **Read and recovery verbs — `list`, `show`, `log`,
  `restore`, `export` — work regardless of the axis**: recovery must never
  be gated behind re-enabling a setting, least of all on the machine that
  just suffered the damage. `check` validates an existing store
  regardless — it validates what is reachable, and the store is reachable.
  `prune` and `forget` also work when off (turning the feature off must
  not strand bytes you can no longer clean up).
- **When the transport is git, leave it off** — the single most useful
  sentence the docs can carry, and what makes default-off a considered
  scope rather than timidity.

## Rejected / non-goals

- **A dotfolder** — DESIGN opens on "not... an app-private sidecar folder"
  (the pre-§1 epigraph), and §6 frames prov as "Obsidian, except the user
  owns what `.obsidian/` used to own." Anything prov maintains must be
  reachable and self-describing.
- **A folder outside the workspace** — the other way to dodge recursion,
  and worse: out-of-tree state is a durability dependency (lost to a
  reinstall, decoupled by a copy), it is the sidecar anti-pattern wearing
  a different path, and it does not sync — which forfeits foreign-event
  recovery, the feature that costs this design nothing. The recursion is
  solved by the [capture-set exclusion](#the-capture-set) instead.
- **OCFL as the live store** — forks unrepresentable, root inventory
  rewritten per commit, asymmetric worst case. Adopted as an export
  instead. See [OCFL](#ocfl-an-output-format-not-the-store).
- **A delta log with fold-based reconstruction** — v1's design; see
  below.
- **Per-id version chains as the storage shape** — coverage gaps, no
  consistent cut, and a conflict-prone or degenerate store. Kept as a
  query ([`history-log`](#history-log-lineage-by-id)).
- **Folding history into the recycle bin** — different trigger, different
  meaning of "restore"; a sibling, not an extension.
- **A single append-to manifest file** — the most merge-conflict-prone
  file in the workspace, under exactly the scenario this addresses.
- **Device identity for events** — every way of minting one is worse than
  not needing one: a `~/.config/prov/id` is a durability dependency that
  forks history silently when lost; hostname/MAC-derived ids are unstable
  and leak machine identifiers into synced plaintext. With nothing
  computing through `parent`, there is nothing device identity would buy.
  (A future display-only `origin:` field stays possible and must stay
  non-load-bearing.)
- **A shared blob/record-store primitive extracted from the recycle bin** —
  the bin's ops (`prov/src/mutate.rs:898-1346`) are per-document tombstone
  operations wired to spanning-relation and parent-link editing; history
  hashes a whole set and parks by content hash. A reusable *shape*
  (visible directory, unreached bytes, reachable record, `check`
  validates), not reusable *code*. The bin's tested code is left
  untouched.
- **A full operation log** — replaying `ChangeSet`s protects the wrong
  layer: prov's own mutations, already covered by the crash journal, not
  the real threat — a sync tool landing bytes prov never wrote. It would
  also require a correctness-critical inverse for every one of ~14
  mutation kinds (`combine`'s is genuinely ambiguous) before protecting
  anything. Not ruled out forever — DESIGN open question #1 leaves the
  door open behind `IndexStore` — but out of scope here.
- **Automatic capture-before-sync** — requires a per-transport file
  watcher; a different, larger project. Manual only, for now.
- **Full-tree ZIP as the event format** — opaque, no dedup; kept for
  `backup`, where opacity and simplicity are exactly what's wanted.

### Why not a delta log (the v1 design)

Recording only what changed per event, with reconstruction by folding the
event's ancestry, was v1's shape. Rejected on review:

- The fold makes `parent` load-bearing, and added-file sync gives no
  ordering guarantee between event documents — a transport can deliver a
  head whose ancestry has holes, at which point neither restore nor a new
  capture (which must fold the head it diffs against) is computable.
- `prune` becomes the hardest problem in the design: a pruned event's
  entries can be load-bearing for later events' effective state, so
  pruning must fold the dropped prefix into a new genesis — rewriting an
  "immutable" event, the one operation that can conflict under the exact
  scenario the store exists for.
- The fold is a genuinely new shape for prov. The registry — the closest
  existing analogue — is a sorted map re-rendered wholesale
  (`prov/src/index.rs:236`), not a log anything folds. Full manifests are
  the established shape — in prov, and in OCFL.
- The claimed benefit (diff-friendly events) is weaker than it looks: two
  sorted manifests diff cleanly with any diff tool, and "what changed" is
  computable at display time. Delta encoding optimizes text size for a
  scale — tens of thousands of files, many captures a day — that
  contradicts the tool's own profile, and the bytes that dominate storage
  (blobs) dedupe identically under both designs.

### Why per-file, content-addressed pre-images

Matches the actual risk (specific files corrupted by a bad merge, not "the
whole tree is gone" — that's `backup`) at the lowest cost: only novel
content parks a new blob, and identical bytes across events dedupe for
free once addressed by hash. It reuses infrastructure that exists: the
fixity module (dependency-free SHA-256, `sha256:<hex>`,
`prov/src/fixity.rs`) provides the hashing primitive; the recycle bin
establishes the "visible directory, unreached bytes, reachable record"
shape.

The honest cost is the **first** capture, which parks a pre-image of every
file in the capture set — a full second copy of the workspace, inside the
workspace, that the transport then uploads. Steady state is cheap in
*storage*; genesis is not, and steady-state *time* is O(workspace bytes)
per capture until the hashing short-circuit lands. Two refinements are
deliberately deferred until the cost bites, because neither touches the
format: copy-on-write cloning (`clonefile`/`FICLONE`) behind a fourth
`Capabilities` flag (`prov/src/fs.rs:321` — the struct currently has
exactly three), and a size+mtime short-circuit so unchanged files are not
re-hashed on every capture.

## Before Phase 0

Settle these first; each is small, and each changes what Phase 0 writes
down:

1. **The journal spike.** Verify what `.prov-journal` records per op
   (`prov/src/change.rs:234` is where `ChangeSet::apply` engages it) and
   whether a genesis capture or whole-workspace restore — orders of
   magnitude more bytes per changeset than any existing mutation — fits it
   comfortably. Expected split, to confirm: event + index writes ride the
   journaled `ChangeSet`; blob copies go through `write_atomic` directly,
   safe because content-addressed writes are idempotent under replay. If
   the journal embeds file contents, this split is mandatory, not
   optional.
2. **Pin the capture set in a format spec.** Event-document format,
   manifest schema (`path`/`id`/`hash`, sort order, canonicalization for
   the suffix hash), id grammar, and the exact capture-set definition —
   including confirming attachment payloads are reachable. The format is
   the compatibility contract; immutable files cannot be retrofitted.
3. **The transport-simulation test harness.** The feature's entire claim
   is surviving external transports, so Phase 0's tests must simulate one:
   two workspace copies, concurrent captures, a directory merge (union of
   added files, a fabricated `.sync-conflict-…` file, a clobbered shard
   index), then assert `capture`/`list`/`check` behave. No prior feature
   needed this fixture; this one is defined by it.
4. **The recycle-bin missing-bytes finding** — a bin record whose parked
   bytes are absent — lands as its own change, before history, since the
   bin-index capture story leans on `check` catching that state.

## Phasing

- **Phase 0 — event store + capture + list.** The `history` relation,
  visible directory, the manifest-per-event format per the spec above, the
  date-sharded layout with per-shard rebuildable indexes and
  `HistoryIndexStale` (the index is a rebuildable cache from the first
  commit or it is not one at all), `history-capture`/`history-list`, the
  config axis, and the transport-simulation tests. Shard from the start:
  the id-to-path derivation is part of the format. No restore yet — this
  proves capture under real merge conditions before anything depends on
  it. No recycle-bin refactor is implied.
- **Phase 1 — restore + show + log.** `history-show` (trivial now),
  `history-log`, `history-restore` — additive default, `--exact`, scoped
  paths and `--id` documented as content-recovery, the up-front path *and*
  registry-id collision guards, missing-blob reporting, the before/after
  `check` diff (fixed / introduced / pre-existing), never touching
  `history/` or the root's history pointer. Crash atomicity reuses
  `Workspace::commit` per the settled journal split. Staged-`ChangeSet`
  validation for a true pre-flight `--dry-run` is explicitly *not* here —
  a general capability for every mutation, built as one.
- **Phase 2 — hygiene and destruction.** `history-prune` (delete + GC +
  index rebuild), `history-forget` with the `forgotten.<ext>` tombstone
  store, `HistoryBlobMissing`/`HistoryBlobOrphaned`, and the documented
  git-hook / watch-script recipes.
- **Unscheduled but decided in principle:** `history-export --ocfl` via
  mocfl, as a `prov-cli`-only dependency
  ([OCFL](#ocfl-an-output-format-not-the-store)). Build when there is a
  corpus worth depositing; nothing in Phases 0–2 forecloses it.
- **Deliberately unscheduled** (correct, but built on evidence, none
  touching the format): copy-on-write blob cloning behind a fourth
  `Capabilities` flag; the size+mtime hashing short-circuit; re-parking a
  missing blob from a matching working-tree file; staged-`ChangeSet`
  pre-flight validation.

## Open questions

1. **Retention default for `prune`** — keep-forever until asked, or a
   default `--keep`? Needs a real usage pattern; deferred until after
   Phase 0 ships. (With no fold, pruning by age or count is well-defined
   regardless of forks, though dropping another device's only capture of
   some state is still a judgment call worth a confirmation prompt.)
2. **Should `history-capture --label` be encouraged toward a small
   vocabulary** (`pre-sync`, `nightly`, `pre-migration`) via the existing
   controlled-vocabulary fields mechanism, or stay free-form? Free-form
   for v1; the fields machinery makes the upgrade cheap if labels prove
   load-bearing.
3. **Does `backup` eventually want an `--exclude` for `history/blobs/`?**
   A workspace with a large blob store doubles the backup size for bytes
   that are themselves already copies. Plausible, but it compromises
   "opaque whole tree" — defer until someone actually hits it.
