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
Phase 2 adds the blob findings, `history-prune` (§12) and `history-forget`
(§13).

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

### How the store's documents are authored

The store is authored the way the rest of the workspace is — all three axes
together, from the workspace's own configuration:

| Axis | Source |
|---|---|
| extension | the root document's content format (`md`, `dj`, `html`) |
| prose body | that same grammar — Markdown source, transcoded |
| frontmatter carrier | the workspace's `embed_style` + `default_embed_format` |

All three or none: a store that took its extension from the workspace and its
body from a hardcoded default is a `.html` file holding a literal `# History`,
which prov reads back and no other tool does. An HTML workspace's store is HTML,
with the metadata in the same `<script>` island the workspace's other documents
use.

One consequence for a third-party reader, stated plainly: **the manifest is
written in whatever metadata language this workspace uses** — YAML, JSON, TOML
or fig — and carried in whatever fence that workspace embeds with. There is no
single answer to "what parses an event document"; there is a per-workspace
answer, and the store index says which one. It names the carrier and the
language in its own prose, so a reader who opened `history/` and nothing else
does not have to recognize a fence to find the manifest. That paragraph also
spells out the blob layout (§5) and that a blob *is* the file, so recovery by
hand needs no other document.

Only the store index's *path* is discovered from the root pointer. Every other
path in the store is derived: the shard directories from an event id (§4), the
blob path from a hash (§5).

### Finding the store when the root has stopped declaring it

The pointer is the store's only declared location, and it is one line in one
mutable file — exactly the kind of thing a transport mangles. So discovery has a
second step, and only one:

1. The root's `history` pointer.
2. Failing that, **`history/index.<ext>` if it is on disk** — the conventional
   path, and nothing else. A store the root declared somewhere unusual and then
   stopped declaring is not recoverable by guessing, and a filesystem sweep for
   anything store-shaped is how a backup copy gets adopted as the live one.

A store found the second way is read normally — recovery must never be gated
behind repairing the thing that broke — and `check` reports the missing pointer
as `HistoryStoreUnlinked` (§7). A capture re-declares it, adopting the store
rather than bootstrapping a second one beside it.

Without that step the failure is silent in every direction at once: descent into
the store is through the pointer, so an undeclared store is a subtree the walk
never enters, which means not even an orphan is reported about it. `history-list`
would print nothing while the events sat on disk — a state a shell and `cp` can
still recover from, and prov could not.

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
stores and its one derived page**:

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
- **Exclude the generated `about.<ext>`** (wherever the root's `about` pointer
  puts it). It is *derived* — a pure function of the configuration, which this
  same manifest captures — so parking its bytes stores nothing that cannot be
  reproduced, and a new blob would be parked on every config change for no
  recovery value. Restoring an event restores the config that determines the
  page, and `check` reports the page stale until `prov about` rewrites it from
  that config: the same repair by a shorter route. It also removes an ordering
  hazard, since the first capture *bootstraps* the store, which changes what the
  page says about this workspace — a captured page would be one the capture
  itself invalidated.

Everything else structural stays in: the registry document, the config
document, and the recycle bin's *index*.

A reader validating a manifest against a live tree should expect exactly these
three absences, and no others.

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
| `created` | yes | RFC 3339 UTC timestamp of the capture, to **microsecond** precision (§3.2). |
| `trigger` | yes | How the capture was invoked. `manual` is the only Phase 0 value. |
| `label` | no | The `--label` text verbatim (the *slug* of it appears in the id). |
| `parent` | no | The id of the newest event that existed locally at capture time. |
| `files` | yes | The manifest — the complete capture set (§3.1). |

```markdown
---
part_of: '[July 2026](index.md)'
created: 2026-07-31T09:15:22.481903Z
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

### 3.2 `created`, and how two events are ordered

`created` is written with **exactly six fractional digits** — microseconds, never
trimmed:

```
2026-07-31T09:15:22.481903Z
```

Sub-second precision exists for one reason: so that two captures inside the same
second can be *ordered*. Everything that reads the store in capture order —
`history-list`, `history-log`, and a capture choosing the `parent` it records —
takes the maximum by `(created, id)` (§6), and at second granularity two captures
in one second tie and fall through to the id, whose middle is the label slug. The
observable result was an event ordering alphabetically by label and a
`history-list` reporting forks that never happened.

Two rules follow, and a reader that skips either gets the order wrong:

- **Fixed width, always.** A trimmed fraction defeats the point: `…10.1Z` against
  `…10.12Z` compares `Z` (0x5A) with `2` (0x32) at the second fraction digit, so
  the shorter one sorts later. Writers emit six digits or none.
- **Normalize before comparing.** A store keeps every precision it was ever
  written at — event documents are immutable, and sync interleaves devices rather
  than separating them by version — so second-granularity stamps and microsecond
  ones coexist permanently. Compared raw they invert, because `Z` (0x5A) sorts
  after `.` (0x2E) and `…10Z` would land *after* `…10.500000Z` inside its own
  second. A reader pads the fraction to six digits before comparing; a stamp not
  in `…Z` form is left alone. Nothing is rewritten, which is what makes this a
  widening of the format rather than a break in it.

The id's `HHMM` head is unaffected: it reads the calendar head only, so ids stay
minute-granular (§4) and the digest suffix is what tells two captures in one
minute apart.

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
event in the store, comparing `created` **normalized** to six fractional
digits (§3.2). That is also what a new event records as its `parent`. The id
tiebreak survives for the genuine tie — two devices landing on the same
microsecond — where it is arbitrary but deterministic, which is all an
ordering owes a fork.

## 7. Validation

`check` validates the store like any other reachable member, through five
findings.

- **`HistoryStoreUnlinked`** — a store is at the conventional path (§1), the
  `history` axis is on, and the **root does not point at it**. Reported first,
  because everything below is about the store's contents and this says prov
  cannot see the store from the root at all. Autofixes by re-declaring the
  pointer: metadata-only, and unambiguous because the finding only ever fires
  for the one path prov would itself have used. Conditioned on the axis on
  purpose — a workspace with `history: off` and a leftover `history/` directory
  has lost nothing, and a finding there would be prov objecting to a directory
  the user is entitled to leave alone.
- **`HistoryIndexStale`** — a shard directory holds an event (or a sub-index)
  its index document does not link, or links one that is gone. This is the
  *expected* outcome of a transport mangling a derived cache. It autofixes by
  rebuilding that one index from its own directory listing, per-shard, so a
  mangled `2026/07/index.<ext>` is repaired without touching any other month —
  the same confirmation-gated posture as every existing fix.
- **`HistoryBlobMissing`** — a manifest names a hash with no blob behind it, so
  the files captured under it cannot be restored from this store. Raised **per
  hash**, not per event: one lost blob is one thing to put back, and a store
  where fifty events captured the same unchanged file should say so once. Which
  *events* are thereby incomplete is `history-show`'s question (§10.1), and it
  already marks the rows.
- **`HistoryBlobOrphaned`** — bytes under `blobs/` that no manifest names. One
  finding per sweep, listing them sorted. **Suppressed for the whole sweep**
  while any event in the store is `Unreadable` (below): the mark half of the
  mark-and-sweep is then known incomplete, not merely small, and reporting an
  orphan on that basis would name bytes an unreadable event's own manifest
  might still be the only thing claiming.
- **`Unreadable`** — an event-shaped file (§4's id, plus the extension)
  exists but its document fails to load or parse. The same finding `check`'s
  general walk raises for any other document it cannot read, reused here
  unchanged rather than duplicated under a history-specific name. Diagnosis
  only: nothing can synthesize the document back.

Both blob findings come from one **mark-and-sweep**: union every event's `files`
hashes and compare against the blob listing. That is what full manifests buy —
under a delta log the same question would require folding ancestry, and could
not be answered at all for an event whose ancestors had not arrived. The cost is
one parse of every event document per `check`, which is the price of a store
whose authority is distributed across immutable documents rather than
concentrated in an index.

**Both are diagnosis only, and for the same reason in opposite directions.**
Nothing can synthesize missing bytes, and the real repair — letting the
transport finish, or restoring `blobs/` from a backup — retires the finding on
its own; the only "fix" available would be deleting the manifest rows that name
the hash, destroying the record of what was captured in order to silence a
report about it. And collecting an orphan is *destruction*, which autofix is
never (§ `Fix` is metadata-only): `history-prune` is where bytes are deleted,
deliberately and on request.

Two things the wording is load-bearing about:

- **`HistoryBlobMissing` has two causes and must admit both.** Bytes genuinely
  lost, and a sync still in flight — an event document and its blobs travel
  separately, and a small document routinely lands well before the megabytes it
  points at. A finding that cries corruption at a routine, self-resolving state
  is one users learn to ignore. (`history-forget` adds a third — deliberately
  forgotten — which `check` tells apart via the forget list and reports as
  informational rather than as loss.)
- **`HistoryBlobOrphaned` is expected transiently**, because a blob can arrive
  before the event that references it. `history-prune` and `history-forget` are
  the durable producers and both collect after themselves, which is what makes a
  *persistent* orphan worth reporting. Anything non-hidden under `blobs/` counts,
  not only well-formed digests: a transport's conflict copy of a blob is exactly
  the cruft this should surface, and it would never match a hash.

A manifest row whose hash is not a digest prov could have parked (a foreign
scheme, a mangled string) reports as `HistoryBlobMissing` rather than failing the
run — a foreign event has to stay legible. An event document that fails to parse
is a plain `Unreadable`, unchanged. There is no `HistoryParentMissing`: no
correctness depends on `parent`, so a hole in the display ancestry is cosmetic.

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

Two verbs make it irreversible again: `history-prune` (§12) by age or count, and
`history-forget` (§13) for one document deliberately.

## 10. Reading the store

Three read-only queries. None writes anything, and all work regardless of the
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

Pull the subject's row out of each manifest in **capture order** (`created`
normalized per §3.2, then id) and keep only the events where that row changed. Nothing in the store is
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

### 10.3 `history-cat <id> <target>` — one captured file's bytes

Resolve the subject's row in the event's manifest and write the blob its `hash`
names (§5) to standard output, verbatim.

A **lookup, not a reconstruction**: a manifest row addresses its bytes directly,
so the cost is one read however many captures have happened since. This is the
other half of what full manifests buy — §10.1 prints what a capture *recorded*,
and this produces what it *holds*, which is what makes the store usable from
outside prov entirely:

    prov history-cat <id> notes.md | diff - notes.md

Bytes, not text. A capture set holds whatever the workspace holds, and an
attachment payload is not UTF-8; nothing is transcoded, and no trailing newline
is added that the capture did not have.

- **The subject follows §10.2's rule** — an id wherever one exists, a path
  otherwise. A path is matched against the path the manifest **recorded**, which
  is what the document was called at that capture and not necessarily what it is
  called now. A path that no longer exists on disk therefore still answers, which
  is how a deleted document's bytes come back.
- **Absence is reported in three kinds, never as one.** A subject the manifest
  has no row for (the document did not exist, or was outside the capture set); a
  row whose blob is not on disk (§10.1's ordinary in-flight state, not damage);
  and a row whose blob was deliberately destroyed (§13 — the tombstone is what
  separates the two, and the record outliving the bytes is that verb's stated
  bargain). Collapsing them would report a routinely half-synced event as loss.
- **Every refusal writes nothing to standard output** and exits non-zero, so a
  pipeline fails rather than silently comparing against an empty file.

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
| case only | the captured bytes are already there, under a spelling that differs from the manifest's only by case |
| unchanged | the captured bytes are already there — nothing is written |
| no bytes | the manifest's hash names no blob in this store — skipped, by name |

A `no bytes` row is **ordinary, not broken** (§10.1's two causes: in flight, or
lost), and it is skipped rather than fatal — including under `--exact`, where the
path is still one the manifest holds and so is never removed for want of bytes
that merely have not arrived.

**Case identity, on a filesystem that folds it.** A row's "on disk" check and
`--exact`'s removal set both have to agree about *which* file a path names, and a
case-insensitive filesystem (APFS, NTFS) is where a byte-exact string compare and
a real `try_exists` lookup can disagree: the manifest's `notes/A.md` and a
sync-renamed `notes/a.md` on disk are the same file to the filesystem, but not to
a naive string comparison. Restore resolves every row's *actual* on-disk spelling
once and uses that resolved identity everywhere — so a row the probe finds only
under a different case is never a candidate for `--exact` removal, and is instead
renamed in place to the manifest's own spelling (`case only` above; `overwrite`
does the same rename when the bytes changed too). A manifest that itself holds
two paths differing only by case — a state only a case-sensitive filesystem can
produce — is refused outright on a filesystem that folds case, rather than let
the second row's write silently clobber the first. None of this changes anything
on a filesystem that does not fold case: two such paths are simply two ordinary,
unrelated files there.

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

## 12. Pruning

`history-prune (--keep <n> | --before <date>)` drops the oldest captures and
collects the bytes no surviving capture references. **Manual, never automatic**,
and irreversible. It works regardless of the config axis (§8): turning the
feature off must not strand bytes you can no longer clean up.

With full manifests this is **delete plus garbage collection and nothing else**.
Every event is self-contained, so dropping one cannot make another unreadable —
no folding, no re-anchoring, and above all no rewriting of surviving events,
which under a delta log was the hardest problem in the store: a pruned event's
entries could be load-bearing for later events' effective state, so pruning had
to rewrite an "immutable" event, the one operation that conflicts under exactly
the sync this store exists to survive.

### 12.1 The bound

**Exactly one is required, and there is no default.** An operation that deletes
bytes should not do so because a flag was forgotten; a bare `history-prune`
refuses and reports what the store holds, since that is what tells you which
bound you want.

- `--keep <n>` — keep the newest `n` events, drop the rest. The count axis.
- `--before <date>` — drop every event captured **strictly before** that
  instant. A bare date is a *prefix* of every timestamp in its day, so an event
  on the named day is kept: `--before 2026-06-01` means "before that day
  started". Compared against `created` normalized per §3.2, so a store mixing
  precisions cuts in the right place. A cutoff that is not a `YYYY-MM-DD` head
  is refused, so a typo drops nothing rather than everything.

**Also refused: any event document in the store that fails to load or
parse.** §12.2's blob collection is the survivors' manifests taken as a bound
on what is still referenced; an unreadable event's manifest is invisible to
that bound, so its blobs would be indistinguishable from orphans and would be
collected — deleted, permanently, by a prune whose bound only looked
*complete*. The refusal names the unreadable file(s), the same read that
raises `Unreadable` (§7). Repair or restore them, or let the transport
finish syncing, then retry.

### 12.2 What it deletes, and in what order

1. **The event documents**, and the shard indexes the drop empties.
2. **The blobs** no surviving manifest names.

The order is the safety argument. Events first means a crash mid-prune leaves
blobs nothing references — a `HistoryBlobOrphaned`, which the next prune
collects. Blobs first would leave surviving manifests naming bytes that are
gone, which is real loss. The residue is benign in one direction and damage in
the other, so the order is not a preference.

**Blobs do not ride the journaled change set**, mirroring capture (§6) with the
reason inverted: there, the journal embeds contents, so parking a genesis
capture through it would duplicate the workspace into `.prov-journal`; here, a
staged removal buffers the bytes it deletes so it can put them back, and a GC
freeing a gigabyte would hold a gigabyte to do it. Deleting content-addressed
bytes directly is safe for the same reason writing them is — the operation is
idempotent, and a half-finished one is an orphan rather than a corruption.

The blob sweep is `HistoryBlobOrphaned`'s (§7), taken against the survivors. So
the two agree by construction, and a prune also collects orphans that were
already there — which is what that finding points here for.

### 12.3 Indexes, and the directories left behind

Surviving indexes are rebuilt from their own directory listing minus what the
prune drops — the same "an index is a pure function of its directory" rule
capture and the autofix follow. An index is **rewritten only when its content
would actually change**: every one is a file some transport has to carry, and a
prune that rewrote five years of untouched shards would be five years of
needless merge surface.

A shard that loses its last event loses its index document too, and so does a
year that loses its last shard. The store index always survives — it is the
root's pointer target, and a store pruned to nothing is still a store.

**A directory with no event in it is not a shard.** A change set removes files,
not directories, so an emptied `2026/07/` lingers; a transport that deletes
files can leave one too. Every place an index is *rendered* ignores event-less
directories, so a leftover is invisible rather than a permanent
`HistoryIndexStale` naming an index that should not exist.

### 12.4 What prune does not decide

Dropping an event can destroy the only copy of some content — including content
another device captured and this one never had live. Nothing in the store can
arbitrate that, so `history-prune` lists what it would drop and confirms before
acting, and offers `--dry-run` to see the list without any of it happening.

## 13. Forgetting

`history-forget <path|id>` destroys one document's captured bytes and records
that it was deliberate. Works regardless of the config axis (§8).

This is the counterpart to the retention the store creates (§9). A document's
bytes normally end at `empty_bin` or `rm --purge`; with history on, any event
that captured it while it was live still holds them, and `history-restore`
brings them back. This makes that irreversible on purpose. Full manifests are
what make it tractable: every hash a document ever had is a column lookup across
the events, not a fold.

### 13.1 Two limits, both load-bearing

- **It destroys only bytes nothing else names.** A hash the subject shares with
  another captured path survives, and is reported. Content addressing means
  forgetting one document cannot reach into another's history — a safety
  property and a limit in the same breath.
- **It destroys bytes, not the record.** Event documents are immutable, so every
  manifest still names the path, the id and the hash. **If what has to disappear
  is the name, this is not that tool**, and no wording may let a user believe
  otherwise.

The subject follows `history-log`'s rule (§10.2): an id wherever one exists,
since that is what survives a rename and therefore reaches versions a path key
would miss; a path only for the documents that carry no id.

### 13.2 It refuses a live document

Forgetting the captured bytes of a document still in the workspace is very
nearly a no-op — the next capture parks them again. Refused by default, naming
the document, with `--force` for the deliberate "purge the history, keep the
file" case.

"Live" means **in the capture set** (§2), not merely present on disk: that is
exactly the population a capture parks, so a file sitting unreachable in the
tree would not come back, and refusing on its account would be refusing for a
reason that is not true.

### 13.3 It refuses while any event is unreadable

The `mine`/`others` split (§13.1's first limit) is computed over every event's
manifest — that is the whole mechanism content addressing buys, a hash the
subject shares with anything else survives. An event document that fails to
load or parse contributes nothing to `others`, so a hash it shared with the
subject would read as belonging to the subject alone and be destroyed: bytes
another, unreadable document's history still names, gone because that
document could not currently be read to say so.

Refused rather than guessed, naming the unreadable file(s) — the same read
that raises `Unreadable` (§7). Repair or restore them, or let the transport
finish syncing, then retry.

### 13.4 `forgotten.<ext>`

A **whole-file record store** beside the store index, under the `MalformedStore`
rule the registry and the bin index live under — it is a mutable record store
prov edits in place, which an immutable event document deliberately is not. One
row per destroyed hash:

```yaml
title: Forgotten
forgotten:
  - hash: sha256:b7d98ce3…
    at: 2026-08-01T23:41:04.300145Z
    subject: notes/secrets.md
```

- **It is linked from `history/index.<ext>`.** `history/` is orphan-scanned (the
  store index is reachable, so its directory is in the walk's reached set), so an
  unlinked tombstone would be reported as an orphan — the record of what was
  destroyed, flagged as litter.
- **Recording the subject leaks nothing.** Every manifest already names that
  path or id beside that hash, because events are immutable. Without it the list
  cannot answer why anything on it is there.
- **Re-forgetting a hash keeps the first row.** *When* it was destroyed is the
  fact worth preserving, and a re-run finishing an interrupted forget must not
  rewrite it.
- It is located by **stem**, not by the workspace's current metadata format: a
  workspace that switched formats after a forget must not lose track of what it
  destroyed.
- It is a mutable file and can conflict under sync. Acceptable for an explicitly
  invoked, rare act of destruction.

### 13.5 What the tombstone buys

A hash on the list is absent **by record**, so:

- `check` does not raise `HistoryBlobMissing` for it (§7). Reporting it would
  mean `check` never returned to clean after a legitimate forget, which is how a
  user learns to stop reading `check` — and telling this state from loss is the
  entire reason the list exists. The suppression is precise: bytes that went
  missing *without* a record still say so.
- `history-show` marks the row **forgotten** rather than *bytes missing*, and
  `history-restore` names it: absent by decision reads differently from absent by
  accident.

### 13.6 Ordering

The tombstone is written and committed **before** the bytes are freed —
write-ahead, like every other mutation here — and the blobs are deleted outside
the change set, for §12.2's reason: a staged removal buffers the bytes it deletes
in order to be able to put them back, which is the one thing a destruction verb
must not do.

A crash between the two leaves a hash tombstoned whose blob is still present.
Re-running the same forget finishes the job. That is the residue this ordering
can leave, and it is the quiet one — the tradeoff write-ahead always makes. The
alternative is destroying bytes before recording the intent.
