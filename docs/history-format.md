---
part_of: '[prov](/README.md)'
---
# The history store — format specification

> The compatibility contract for `prov history-*`. Event documents are
> **immutable**: once written they are never rewritten, so nothing here can be
> retrofitted. Everything in this file is normative; the reasoning behind it is
> in [the proposal](/docs/proposals/history/proposal-history-v3.md).

Implemented in `prov/src/history.rs`. Phase 0 of the proposal covers the store,
`history-capture` and `history-list`; Phase 1 adds the read-only queries over it
(`history-show`, `history-log`, §10) and `history-restore` (§11).
`history-prune` and `history-forget` are a later phase that reads this same
format.

## 1. Where the store lives

The workspace root declares its history store through the **`history` pointer
relation** — a fourth structural pointer beside `registry`, `config` and
`recycle_bin`, one-way, with no `part_of` back-link. The pointer target is the
store's index document.

The conventional layout, and the one `history-capture` bootstraps:

```
history/
  index.<ext>                     the store index — the reachable entry point
  events/
    2026/
      index.<ext>                 the year shard index
      07/
        index.<ext>               the month shard index
        2026-07-31-0915-pre-sync-4f2a9c1e.md      an event document
  blobs/
    9f/86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
```

`<ext>` is the workspace's content-format extension (`md` for the Markdown
default) — the index documents and event documents are **content documents**
with frontmatter and a prose body, not whole-file metadata stores. The
`MalformedStore` rule does not apply to them: it targets mutable record stores
prov re-lays-out in place, and an event is an immutable document with something
to say for itself.

Only the store index's *path* is discovered from the root pointer. Every other
path in the store is derived: the shard directories from an event id (§4), the
blob path from a hash (§5).

### Reachability

`history/index.<ext>` links each year index through the spanning relation; each
year index links its month indexes; each month index links its event documents.
Every index below the store index carries the spanning inverse (`part_of`) back
to its parent index, as does every event document. The store index itself
carries **no** `part_of` — it is reached one-way through the root's `history`
pointer, exactly as the registry and recycle-bin indexes are.

This keeps the whole subtree inside `check`'s bounded walk: `check` scans only
directories that directly contain something reachable, so an index document at
every level is what puts every shard directory in scope with no validator
special-casing.

`history/blobs/` is deliberately **unreached** — nothing links into it, so its
directories never enter the reachable set and the orphan check ignores it, the
same way it already ignores `recyclebin/items/`.

## 2. The capture set

The capture set is **the reachable file set, minus prov's two byte-parking
stores**:

- Start from the reachable set `check` computes from the root: the root
  document, every path a census link resolves to (any relation, a body
  wikilink, or an id through the registry), and every `content` target — which
  is what puts **attachment payloads** in the set, since an attachment sidecar
  points `content` at its payload.
- Keep only paths that exist on disk as files.
- **Exclude everything under the store index's own directory** (`history/`).
  Capturing the store inside the store would mean no capture could ever be
  empty, and an `--exact` restore of an old event would delete every event
  newer than it — destroying the recovery points themselves.
- **Exclude everything under `recyclebin/items/`** (the `items` directory
  beside the recycle-bin index, wherever the root's `recycle_bin` pointer puts
  it). Already unreached; excluded even so, so that bytes the user consigned to
  the bin are not *newly* retained by a routine capture.

Everything else structural stays in: the registry document, the config
document, and the recycle bin's *index*.

Capturing the index but not the items keeps the common case correct — a
document live at capture time comes back live, and the bin index reverts to a
state that does not list it. The narrow residue is a restored bin index naming
items whose bytes were purged in the meantime, which `check` reports per record
as `RecycledBytesMissing`. That finding is not specific to history (a partial
sync produces the same state) and does not depend on it.

Exclusion is by directory prefix on the normalized, workspace-relative path.

## 3. The event document

One document per capture. Frontmatter:

| Key | Required | Meaning |
|---|---|---|
| `part_of` | yes | Spanning inverse to the month shard index. |
| `created` | yes | RFC 3339 UTC timestamp of the capture. |
| `trigger` | yes | How the capture was invoked. `manual` is the only Phase 0 value. |
| `label` | no | The `--label` text verbatim (the *slug* of it appears in the id). |
| `parent` | no | The id of the newest event that existed locally at capture time. |
| `files` | yes | The manifest — the complete capture set (§3.1). |

```markdown
---
part_of: '[July 2026](index.md)'
created: 2026-07-31T09:15:22Z
trigger: manual
label: pre-sync
parent: 2026-07-30-1804-nightly-8c1d55aa
files:
  - path: notes/foo.md
    id: b7k2m
    hash: sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
  - path: notes/photo.jpg
    hash: sha256:2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae
---
# History — 2026-07-31 09:15 (pre-sync)

Captured 412 files (2 changed since the previous event) before syncing.
```

**`parent` is display metadata.** Nothing computes through it. A missing or
skewed parent is cosmetic — it costs `history-list` its changed-file counts and
nothing else.

**Event documents carry no `id` field.** A document with no id is legal;
minting registry ids for events would make every capture write `registry.md`,
which is exactly the merge-conflict shape the store exists to avoid. The
manifest's `id` column records the ids of *captured* documents; events
themselves have none.

### 3.1 The manifest

`files` is a sequence of mappings, **sorted by `path`**, byte-wise ascending on
the UTF-8 path string. One entry per captured file:

- `path` — workspace-relative, `/`-separated, normalized. Required.
- `id` — the document's registered id. **Omitted entirely** when the document is
  not registered.
- `hash` — `sha256:<64 lowercase hex>`, the digest of the file's bytes exactly
  as `prov::fixity::digest` computes it. Required.

A path absent from the manifest was not in the capture set. There is no
`removed:` list: omission *is* deletion, and "what changed" is computed at
display time by comparing a manifest with its predecessor's.

## 4. Event ids

```
2026-07-31-0915-pre-sync-4f2a9c1e
└──┬───┘ └┬─┘ └───┬───┘ └───┬──┘
  date   time    slug     digest
```

`<YYYY>-<MM>-<DD>-<HHMM>[-<label-slug>]-<8 hex>`, where:

- **date and time** are UTC, derived from `created`, zero-padded. The date is
  repeated here rather than left implicit in the `2026/07/` path so the id is a
  standalone token: quotable out of context, and reversible to its path.
- **label slug** is `prov::link::slug` of the `--label` text, **omitted along
  with its separating hyphen when there is no label**. A label that slugs to
  the empty string is treated as absent.
- **digest** is the first 8 lowercase hex characters of the SHA-256 of the
  event's canonical form (§4.1).

The id **is** the event document's file stem. Its path is a pure function of
it:

```
history/events/<YYYY>/<MM>/<id>.<ext>
```

taking `<YYYY>` and `<MM>` from the id's own leading `YYYY-MM-`. This is what
makes "the index is only a cache" true rather than aspirational — every id
resolves to a path with every index file destroyed.

A content-derived suffix (rather than a random one) keeps the library clockless
and deterministic and makes collisions **benign**: two devices that produce
byte-identical events produce the same filename holding the same content, which
is convergence rather than conflict.

### 4.1 Canonical form

The bytes hashed for the digest suffix. Deliberately **independent of the
metadata serialization format**, so the same workspace state yields the same id
whether frontmatter is YAML, JSON or fig.

Lines, in this exact order, each terminated by a single `\n` (U+000A), fields
separated by a single TAB (U+0009):

```
created<TAB><created>
trigger<TAB><trigger>
label<TAB><label>            ← omitted when there is no label
parent<TAB><parent>          ← omitted when there is no parent
file<TAB><path><TAB><id><TAB><hash>     ← one per manifest entry, in manifest order
```

- The `label` line carries the **raw** label text, not the slug.
- An unregistered file's `id` field is the **empty string**, so the line has the
  same four-field shape either way.
- Paths use `/` separators regardless of host platform.

The digest is `prov::fixity::digest` of those bytes; the id suffix is
characters 7..15 of the resulting `sha256:<hex>` string — i.e. the first 8 hex
characters of the digest.

## 5. Blobs

Every captured file's bytes are parked, content-addressed, at:

```
history/blobs/<first 2 hex>/<remaining 62 hex>
```

**Bare hex — never the `sha256:` scheme prefix an event spells.** A colon in a
filename is hostile to Windows and to more than one sync client.

Bytes are stored **verbatim**: never re-encoded, never normalized, so a restore
is byte-exact. A blob is written only when its path does not already exist; the
manifest may freely reference blobs parked by earlier events or by other
devices.

Blob writes do **not** ride the journaled `ChangeSet`. The journal embeds file
contents (`prov::journal::encode`), so a genesis capture riding the change set
would write a second copy of the entire workspace into `.prov-journal`. Blobs
go through `Storage::write_atomic` directly instead, which is safe precisely
because a content-addressed write is idempotent: replaying it can only write
the same bytes to the same path. The event document and the shard indexes —
small, and the part that must land together — do ride the `ChangeSet`.

## 6. What a capture writes

`history-capture` **only ever adds files**, except for the rebuildable indexes:

| File | Written |
|---|---|
| `history/blobs/<..>` | one per newly-seen hash, never rewritten |
| `history/events/<Y>/<M>/<id>.<ext>` | new, never rewritten |
| `history/events/<Y>/<M>/index.<ext>` | rewritten to link the new event |
| `history/events/<Y>/index.<ext>` | rewritten only when the month is new |
| `history/index.<ext>` | rewritten only when the year is new |
| the root document | written only on the first capture, to add the pointer |

Added-file/added-file is the one merge case git, Dropbox, Syncthing and iCloud
all handle without conflict. The index documents are the only mutable files in
the store, and they are **a rebuildable cache**: authority lives in the event
documents, and any index is recoverable by scanning the directory beneath it.

**An empty capture writes nothing.** If the computed manifest is identical —
same paths, same ids, same hashes — to the newest existing event's, capture
prints that event's id and stops. Otherwise a git hook or a habitual user fills
the log with duplicates.

The **newest existing event** is the maximum by `(created, id)` over every
event in the store. That is also what a new event records as its `parent`.

## 7. Validation

`check` validates the store like any other reachable member. Phase 0 adds one
finding:

- **`HistoryIndexStale`** — a shard directory holds an event (or a sub-index)
  its index document does not link, or links one that is gone. This is the
  *expected* outcome of a transport mangling a derived cache. It autofixes by
  rebuilding that one index from its own directory listing, per-shard, so a
  mangled `2026/07/index.<ext>` is repaired without touching any other month —
  the same confirmation-gated posture as every existing fix.

An event document that fails to parse is a plain `Unreadable`, unchanged. There
is no `HistoryParentMissing`: no correctness depends on `parent`, so a hole in
the display ancestry is cosmetic.

`HistoryBlobMissing` and `HistoryBlobOrphaned` arrive with Phase 2, alongside
the `history-prune`/`history-forget` operations that are their durable
producers.

## 8. The config axis

`history: off | manual`, on the same footing as `fixity` and `recycle_bin`.

- **Default `off`.** History adds ongoing storage the user has not asked for,
  and a manual-only trigger means an "on" default buys nothing until the user
  is in the habit anyway. `manual` is the only "on" value for now; the axis
  exists so a future automatic trigger is a new *value*, not a new key.
- **`off` gates capture only.** `history-capture` refuses, with a pointer to
  the axis. Read and recovery verbs work regardless: recovery must never be
  gated behind re-enabling a setting, least of all on the machine that just
  suffered the damage. `check` validates an existing store regardless — it
  validates what is reachable, and the store is reachable.
- **When the transport is git, leave it off.** git already stores every
  pre-image, dedupes by content, and reconciles concurrent histories. This
  feature earns its keep where the transport keeps no history: Dropbox,
  Syncthing, iCloud, a synced network share.

## 9. Retention — stated plainly

**History extends retention of everything ever captured.** If any event
captured a document while it was live, its bytes are in `blobs/`, and neither
`empty_bin` nor `rm --purge` touches them.

The `recyclebin/items/` exclusion (§2) prevents a capture from *newly* parking
already-binned bytes. It does **not** make purges final for content captured
earlier. With history on, `empty_bin` and `rm --purge` are irreversible only
for content that was never captured live.

## 10. Reading the store

Two read-only queries. Neither writes anything, and both work regardless of the
config axis (§8) — including on a workspace whose store arrived entirely from
another device.

### 10.1 `history-show <id>` — one event

The id resolves to its document by §4 alone, with no index consulted. The
manifest **is** the effective state, so there is nothing to reconstruct: `show`
prints the event's frontmatter fields and its rows.

Each row is marked when the blob its hash names (§5) is not on disk. An event
document and the blobs it names travel over the transport independently, and a
small document routinely lands well before the bytes — so a **half-synced event
is ordinary in-flight state, not damage**, and has to be legible under a read
verb before anything acts on it. Presence is tested once per distinct hash, since
a manifest routinely names one blob from several paths. A row whose hash is not a
digest prov could have parked names no blob that could be found, so it counts as
missing rather than failing the read: a foreign event stays legible.

This is deliberately **not** a `check` finding. Phase 2 owns `HistoryBlobMissing`,
which needs the `forgotten.<ext>` tombstone store to tell deliberate destruction
from loss. A restore reports the same set rather than computing its own.

### 10.2 `history-log <target>` — one document's lineage

Pull the subject's row out of each manifest in **capture order** (`created`, then
id) and keep only the events where that row changed. Nothing in the store is
keyed by document: this is a derived query over the `id` column, at the cost of
one pass over every event.

- **The subject is an id wherever one exists** — including an id given
  explicitly, which is never resolved through the live registry, so a deleted
  document still answers. This is the rename-robust key: a move is one document
  that changed path, where a path-keyed view shows two unrelated lineages.
- **A path is the fallback**, for the documents that carry no id — the config
  document, the registry, the recycle-bin index, an attachment payload. Those are
  disproportionately what a sync transport damages, so the weaker key has to
  exist. A path-keyed lineage stops at any rename; when the rows it *does* find
  carry an id, the query says so and names the stronger one.
- **Dedupe is on the whole row** — path, id and hash. A rename leaves the bytes
  byte-identical, so deduping on the hash alone would swallow precisely the event
  the `id` column exists to surface. A document acquiring an id is a point too:
  the row changed.
- **Omission is deletion** (§3.1). An event that does not name the subject
  records it as gone — but only once the document has been seen, so a lineage
  starts where its document does.
- **Forks interleave rather than branch.** This is a display; `history-list` is
  where a concurrent capture on another device is named as a fork.

## 11. Restoring from the store

`history-restore <id> [<path>...] [--id <docid>] [--exact] [--force]` writes a
captured state back. It works regardless of the config axis (§8), for the same
reason the read verbs do.

### 11.1 What an event restores *as*

An event is a **consistent cut**. If a bad merge corrupted a renamed file and its
parent's child list, both were hashed in the same capture, so both are in the
same manifest — restoring the whole event puts the set back together, which is
what actually undoes the damage.

A **scope** (paths, or `--id`) is therefore a different tool wearing the same
verb: content recovery, not structural repair. Writing one file's old bytes back
without the rest of the same corruption's footprint can *reintroduce* the
inconsistency history exists to fix. Right when a sync clobbered one file's
prose; wrong when the graph broke. A path scope takes everything the capture held
at or beneath it; a scope that selects no row is an error, not an empty restore.

### 11.2 Additive by default, exact on request

The default writes every selected path and **deletes nothing**. That leaves a gap
on purpose: bad-merge damage is characteristically *additive* — a
`.sync-conflict` copy, a rename-vs-rename landing both names, a duplicated child
entry — and none of it goes away by writing captured bytes over the top.

`--exact` additionally removes **reachable** paths (§2's capture set: `history/`
and `recyclebin/items/` already excluded) the manifest does not contain. It is
the honest "undo this merge entirely" tool, and the same pass discards legitimate
work done since the capture — so it is opt-in, it lists what it would remove, and
it asks first on a terminal.

Two boundaries the wording has to keep:

- **It cannot be scoped.** "Make the tree match this capture" is a statement
  about the whole tree; a slice of the capture cannot make it. Refused.
- **Reachable is the operative word.** A file nothing links is not in the capture
  set, so `--exact` leaves it and `check` reports it as an orphan. A restore puts
  a captured graph back; deciding that some unreferenced file is rubble is not a
  call it gets to make. The plan is computed against the tree as it stands, so a
  file the *restored* root would stop linking is still reachable when the delete
  set is taken, and is removed.

### 11.3 The plan, and the guards that need no graph walk

Everything below falls out of comparing the manifest against disk. Each selected
row is one of:

| Disposition | Meaning |
|---|---|
| create | nothing is at that path |
| overwrite | something else is at that path |
| unchanged | the captured bytes are already there — nothing is written |
| no bytes | the manifest's hash names no blob in this store — skipped, by name |

A `no bytes` row is **ordinary, not broken** (§10.1's two causes: in flight, or
lost), and it is skipped rather than fatal — including under `--exact`, where the
path is still one the manifest holds and so is never removed for want of bytes
that merely have not arrived.

Before a byte moves, restore also refuses what only the author can arbitrate: a
**registration it would displace**, in either direction, since `id_storage`
defaults to `both` and so a restored document's frontmatter carries an id the
live registry may bind elsewhere — or the target path may be bound to a different
id while the id itself is free. `--force` proceeds anyway.

A collision the restore *itself* resolves is not reported: if the document
currently holding the id is one this restore overwrites or (under `--exact`)
removes, nothing is displaced. This is what lets `--exact` undo a move without
`--force`, while an additive restore of the same event — which would put the old
path back and leave the new one there, two documents spelling one id — still
refuses.

### 11.4 What it never touches

- **`history/` itself.** No manifest row can name a path inside the store (§2),
  and the delete set is drawn from that same set — so neither half of a restore
  can reach in. An `--exact` restore of an old event deleting every event newer
  than it is the failure this rules out.
- **The root's `history` pointer.** A restored root that declares no pointer gets
  one before it is written, so a captured root predating the store cannot strand
  it unreachable. A pointer naming some *other* index is left alone: that is the
  capture's truth about where the store lived.
- **The registry, as a data structure.** The registry *document* is an ordinary
  captured file and comes back with the rest; nothing edits the index in place.

### 11.5 How it ends

Restore does not repair links or the registry — §7 already defines graph
inconsistency and `check` already finds it. So restore runs **`check` before and
after and reports the difference** in three buckets: **fixed**, **introduced**,
**pre-existing** (a count, not a reprint). You restore precisely when something
is already broken, and a bare list of findings afterwards cannot say which of
them the restore caused. A non-empty *introduced* bucket exits non-zero;
`prov check --fix` is the explicit next step.

Writes ride the journaled change set as **copies from the blob**, not as embedded
bytes: the journal records the source path, so restoring a whole workspace costs
O(file count) of journal rather than a second copy of every byte. A
content-addressed blob is exactly the immutable referent that makes replaying
such a reference deterministic — the path is the digest of the contents.
