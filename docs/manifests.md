---
part_of: '[prov](/README.md)'
---
# Manifests — one node for a directory of files

> The bulk attachment. How a prov workspace describes ten thousand photographs
> without minting ten thousand documents, precisely enough that a reader knowing
> only this page can verify the archive by hand. Complements spec §4 (link
> target kinds) and DESIGN §5 (stores).

## 1. Why the sidecar stops working

`attach` gives an arbitrary file workspace-linked metadata by minting a sidecar
beside it: `photo.jpg` gains `photo.jpg.yaml`, an ordinary content node whose
`content` field names the payload. The graph stays all-plaintext and the binary
rides along as a node's body (spec §4).

That trade is excellent for the file you thought about and absurd for the
archive you dumped. A directory of ten thousand photographs would become twenty
thousand files: an editor no one can browse, a sync transport carrying ten
thousand tiny documents, and a containment list ten thousand entries long, all
to say the same sentence ten thousand times.

A **manifest** says it once. One node stands for the whole directory, and one
record store lists what is in it.

## 2. The shape

Two documents, beside the directory they describe:

```yaml
# photos.yaml — the node. An ordinary content document: titled, linked,
# id-able, in the spanning tree.
title: Photos
part_of: '[Archive](/index.md)'
manifest: photos.manifest.yaml
content_hash: sha256:1299004…
```

```yaml
# photos.manifest.yaml — the record store. Machinery: reached one way,
# through the node's `manifest` pointer. No `part_of`, no id.
title: Photos — manifest
root: photos/
files:
  - path: 2019/IMG_0001.jpg
    hash: sha256:2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae
    title: Mum at the lake
    date_of_document: 2019-07-04
  - path: 2019/IMG_0002.jpg
    hash: sha256:fcde2b2edba56bf408601fb721fe9b5c338d10ee429ea04fae5511b68fbf8fb9
```

### 2.1 The node

| Key | Required | Meaning |
|---|---|---|
| `manifest` | yes | The manifest document, relative to the node. **Mutually exclusive with `content`.** |
| `content_hash` | no | `sha256:<hex>` of the *manifest document's* bytes (§4). |

`manifest` and `content` are exclusive because a node stands for one payload or
for a set, never both, and every pass asking "what does this node cover" must
get one answer. A node declaring both is `ManifestConflict` — diagnosis only,
since which key is the mistake is a statement about what the node was meant to
be, and prov has no evidence for it.

A manifest node is otherwise an ordinary content document. It is in the spanning
tree, it takes an id, it is renamed and deleted like any other.

### 2.2 The manifest document

A **whole-file config document** (`.yaml`/`.json`/`.figl`, never markdown) —
spec §5's MUST, which a list of rows prov re-lays-out plainly falls under.

| Key | Required | Meaning |
|---|---|---|
| `title` | no | Human gloss. prov writes `<node title> — manifest`. |
| `root` | yes | The covered directory, relative to *this document's* directory. |
| `files` | yes | The rows. Absent or null means an empty directory. |
| `verbatim` | no | `true` claims every file under `root`, documents included (§3.1). Absent means `false`; any value but a boolean is refused. |
| `exclude` | no | Patterns for files under `root` the manifest does not claim (§3.2). A list of strings; a pattern outside the syntax is refused. |

Each row:

- `path` — the file, relative to **`root`** (not the workspace), `/`-separated,
  normalized. Required. A row that climbs out of `root` is refused.
- `hash` — `sha256:<64 lowercase hex>`, the digest of the file's bytes exactly as
  `prov::fixity::digest` computes it, so `sha256sum` verifies it independently.
  Optional (§3).
- **any other key** — a field of that one file: a title someone wrote, a
  caption, the day it was taken. Optional, any value, kept as written. A
  refresh keeps a row's fields while its file is there (§5), and a scatter
  puts them back on the file's sidecar (§7).

A row's fields are **data, not links**. The graph does not read links inside a
manifest — that is what keeps ten thousand rows off every walk — so a link
written on a row is one no `rename` rewrites and no `check` reports broken.
Nor is a row a document: views and queries see the node, not its rows. A fact
that needs to be a link, or to be found by a view, belongs on a sidecar.

Rows are sorted by `path`, byte-wise ascending on the `/`-joined UTF-8 string —
**not** `Path` ordering, which compares component-wise and disagrees (`a.jpg` vs
`a/b.jpg`: joined, `.` sorts before `/`). A manifest read back off disk keeps
whatever order it was written in; only manifests prov writes are sorted.

**Rows are relative to `root`, and that is the one place this diverges from the
history store's manifest** (`history-format.md` §3.1, workspace-relative). The
divergence is the point: an event manifest describes a whole workspace and has
no root to be relative to, where this one is *about* a directory. Moving that
directory then rewrites one line instead of ten thousand.

## 3. What a manifest claims

**`root` is claimed completely, for opaque payloads only.**

- Every file under `root` that prov cannot read as text — recursively, hidden
  entries skipped — must have a row. One that does not is *drift* prov reports.
- Files prov **can** read (a `.md` note among the photographs, a `.yaml` store)
  are not claimed. They stay ordinary documents: censused, linked, orphan-checked.
  A manifest never shadows a document — deliberate shadowing stays the
  single-file affair `attach --opaque` is for, where the promise is visible in
  the sidecar beside the file it is made about.
- A nested manifest's directory belongs to its own node, so two manifests can
  never claim one file.

### 3.1 Verbatim: claiming the documents too

Some directories are one artifact, not an archive: a built web app, whose
`index.html` sits beside its script bundle because the build put it there, and
is no more a page of the workspace than the bundle is. `verbatim: true`, written
in the manifest by `attach DIR --manifest --verbatim`, claims **every** file
under `root`, the readable ones included:

- each one has a row, hashed like any other, and one without a row is drift;
- none of them is a document: its title answers no `[[alias]]`, an `id` in it
  registers nothing, and a link into the directory does not make it an orphan;
- a refresh keeps the flag, as it keeps the hashing mode, and `--add` takes a
  readable file only when it is set;
- `scatter` refuses it — one card per file would shadow each document singly,
  a promise `attach --opaque` makes on purpose.

It is opt-in, and recorded where the claim is made, because it is still the
exception to the rule above: an archive with a note among its photographs
keeps that note a document. The promise is visible in the manifest the way an
opaque attachment's is visible in its sidecar.

### 3.2 Exclude: what something else keeps there

An archive on a NAS shares its directories with the NAS. A Synology writes an
`@eaDir/` of thumbnails beside every photograph it indexes, an
`…@SynoEAStream` file for each one's extended attributes, and a `#recycle/`
at the top of the share; a Mac leaves `.DS_Store`, Windows `Thumbs.db`. None
of it is the archive, and all of it changes whenever that other program runs.
Listed, it would be most of the rows (on one real share, 664 of 805); left
unlisted, every thumbnail would be drift.

`exclude`, written by `attach DIR --manifest --exclude PATTERN`, leaves such
files out:

```yaml
root: photo/
exclude:
- '@eaDir/'
- '*@SynoEAStream'
- /#recycle/
files:
- …
```

- an excluded file has no row, and is not drift; an excluded directory is
  **not walked**, so a thumbnail tree costs no listing;
- the patterns are in the manifest, so the node's pin covers them, every
  refresh keeps them, and `check` reads the same set the rows were built
  from. Editing them by hand and running `prov manifest --update` re-reads
  the directory under the new set;
- `--add` refuses a file a pattern leaves out.

The syntax is a subset of gitignore's, so a list can be copied from one. A
pattern with no `/`, or only a trailing one, names an entry at any depth
(`@eaDir`, `*.tmp`); one with a `/` before its end is anchored to `root`
(`/#recycle`, `2019/raw`); a trailing `/` matches directories only. `*`
matches within one name, `?` one character, `**` as a whole name any number
of directories, and `\` makes the next character literal. Negation (`!`) and
character classes are refused when the manifest is read rather than taken
to mean less than they would in a gitignore.

### 3.3 Links: an archive kept somewhere else

The machine an archive lives on is not always where its workspace should be.
A NAS share that a photo service also indexes is better left exactly as it
is, and its workspace kept in a share of its own. A symbolic link joins the
two: the workspace holds `photo → /volume1/photo`, and the manifest's `root`
is `photo/`. The link is the one thing that knows where the archive is on
this machine; the workspace says nothing about it.

- The covered walk **follows links**: one to a file is listed as that file,
  and its row's hash is of the bytes it leads to; one to a directory is
  walked as that directory. A link that leads nowhere is not a file and is
  not listed — a photograph replaced by a dangling link is reported missing.
- More than eight linked directories on one path is taken for a loop and
  refused, before the system's own limit would end the walk quietly with
  every file listed once per lap.
- `prov ignore` rules a linked root out as *claimed*, as the link it is
  (`/photo`, without the trailing `/` a directory gets), since that is what
  a tool reading the folder meets and the only rule gitignore matches it by.

### 3.4 Hashing a file larger than memory

An archive's largest files are its videos, and a home server may have less
memory than one of them. A file over 64 MiB is hashed a piece at a time
(8 MiB at once) through the storage port's `read_chunks`; smaller files are
read whole and hashed on every core while the next is read. The digest is
the same either way, and a backend that cannot read in parts reads the file
whole, as before. This holds for every checksum prov records over a payload:
manifest rows, attachment sidecars, inline rows, and `check`.

Completeness is what makes the interesting question answerable. A covered file is
not a document, so §8's orphan walk cannot see it, and it is not a link, so the
census cannot either. Without a claim over the whole directory, a photograph
could vanish and no prov surface would ever say so.

**Hashes are optional, per row.** A manifest with no hashes is an *inventory* —
what is supposed to be here. One with hashes is that plus a fixity baseline.
Hashing reads every file, at mint and at every refresh, which is a real cost over
an archive and a reasonable one to decline for a directory of scans while still
wanting checksums everywhere else (`attach --manifest --no-hash`). A refresh
**preserves the mode**: an inventory that quietly began recording digests would
be claiming a guarantee nobody asked for.

## 4. The chain

```text
photos.yaml  --content_hash-->  photos.manifest.yaml  --hash per row-->  photos/**
```

The node hashes the manifest exactly as an attachment sidecar hashes its payload,
and that is what makes the per-row digests worth anything: editing a row is the
cheap way to make a corrupted archive look intact, and it breaks a checksum the
node has already recorded. Verifying by hand, without prov:

```sh
# the node pins the manifest
sha256sum photos.manifest.yaml
# the manifest pins the files (rows are relative to root)
cd photos && sha256sum -c <(…)   # or read the rows and check what you care about
```

The pin covers the whole manifest, a row's fields included, so editing a
caption by hand leaves it behind until `prov manifest --update` re-records it
(§5).

The node's pin is written whenever the workspace records checksums at all
(`fixity: on`, the default), independently of whether the rows carry hashes:
pinning one small file costs nothing, and it is what the rest hangs from. It is
the same shape every other checksum prov writes has — a small file vouching for
a file beside it, which `sha256sum` can check without prov.

## 5. What `check` does, and what it does not

| Pass | Cost | Finds |
|---|---|---|
| `check` — the node's `content_hash` | one small read | the manifest was edited or damaged |
| `check` — drift | one directory walk, **no file reads** | a listed file is gone; an unlisted file appeared |
| `check` — malformed | one read | the manifest will not parse as one |
| `prov manifest --verify` | **a full read of the archive** | a present, listed file whose bytes changed |

The split is the design. A `check` that re-reads ten thousand photographs is a
`check` people stop running, and a check nobody runs finds nothing. So the cheap
passes run always and the expensive one runs when you ask — on purpose, or on a
schedule.

A malformed manifest yields *only* that finding for its directory: with no
trustworthy row set there is nothing to compare a listing against, and reporting
every photograph as unlisted would bury the finding that matters under ten
thousand that do not.

**`--update` keeps what a row says.** Rebuilding the rows from the directory
drops only what is no longer there: a row whose file is still present keeps its
fields, bytes changed or not, and one whose file has gone takes them with it.
When the rows already agree and only the manifest's text has changed — a
caption edited by hand — the manifest is left as written and the node is
re-pinned over it.

**`--add` records only what it is told.** A host that has just written files
into a covered directory names them: `prov manifest TARGET --add FILE...`
(`Workspace::extend_manifest` in the library) appends a row for each, hashing
only those when the manifest is hashed, and re-pins the node in the same change
set. Every other row is left as it was — a listed file that is missing may be
gone or merely not synced to this device yet, and an add is no place to decide
which — so a partly-synced archive can still grow, and adding two photographs
to two thousand reads two. It refuses a file outside the covered directory, a
document, one the directory scan would never list, or one already listed.

**The repair is never automatic.** Rebuilding a manifest accepts the directory as
it stands — including a file that has *vanished*, which it writes out of the
record as though the loss were intended. That is the judgment `FixityMismatch`
declines to make on the author's behalf, on the same evidence, so
`check --fix mechanical` reports drift and moves on.

## 6. Moving, deleting, and recording

**`rename` moves the node and its manifest; the covered directory stays put.** A
separated document's body *is* its content and travels with it; a manifest is a
description of files that exist on their own terms, and relocating ten thousand
photographs because their index was renamed is a filesystem operation nobody
asked for — slow at best, destructive if it half-finishes. The manifest's `root`
is re-spelled from where it now sits (`root: ../photos/`) and the node's pin is
re-stamped in the same change set.

One consequence, stated plainly: after such a rename **nothing beside the
directory names its node**. The `<dir>.<ext>` convention is a fast path, not the
truth; the `manifest` → `root` chain is what is authoritative. `prov manifest`
accepts the directory, the node or the manifest document for exactly this reason,
and `attach --manifest` asks the authoritative question (a census) before minting,
so a covered directory can never quietly acquire a second manifest.

**`rm` removes the node and the manifest and leaves the archive.** What is left
behind is an uncovered directory — exactly what it was before anything described
it.

**A recording tool takes the manifest, not the photographs.** Covered files are
not in the reachable set: they are opaque bytes, never orphan candidates, and
adding ten thousand paths to every walk would make an archive pay on each of
them for a check none can fail. `prov ignore` says the same thing to whatever
records the folder — a claimed directory is one `/photos/` rule, filed as
*claimed by a manifest* — so damage to a covered file stays **detectable**
(every hash is on record in the recorded manifest) but is not undoable from
that tool's store. The alternative was duplicating the whole archive into it on
the first recording, which is a worse default for the only workspaces this
feature exists for. Keep the bytes safe the way the rest of an archive is kept
safe: a backup, on separate media.

## 7. Gathering attachments, and scattering them again

Photographs are often attached one at a time, before anyone knows they are an
album. `gather` turns a set of attachment sidecars into a manifest: the
payloads move into a new directory, the sidecars are removed, and one node
takes the first sidecar's place in their index. `scatter` is the reverse: one
sidecar per covered file, written beside it.

A field every sidecar carries with **one** value moves onto the node, since it
was a fact about all of them. A field only some carry, or carry differently —
a date set on one photograph, a title someone wrote — moves onto the rows of
the ones that have it (§2.2). `scatter` puts a row's fields back on its sidecar,
a row's `title` in place of the one the file name gives.

The two shapes still do not hold the same things. A sidecar is a document, and a
row is not, so both verbs first list what the other shape cannot carry, and
refuse while any of it is left:

| Loss | Accepted with |
|---|---|
| a field holding a link, which a row cannot (§2.2), unless every sidecar shares it; or one named `path` or `hash` | `--discard FIELD` |
| a row field named for a sidecar's own bookkeeping (`content`, `id`, …), on the way out | `--discard FIELD` |
| a link to a sidecar, or to its file, from anywhere but its index — a transcript, a page embedding the picture | never — remove the link first |
| records contained by a sidecar | never — move them first |

`--discard FIELD` on a gather also drops a field that would have gone onto a
row. Bookkeeping is not lost either: the sidecar's `id` (nothing may link to
it), its link up, `content`, `attachment`, the workspace's edit stamp, and a
title that is only the file name read as one. Each sidecar's `content_hash` becomes its row's digest, so a
gathered manifest is a fixity baseline without re-reading what was already
pinned.

`scatter` by default **keeps the node**, as the index the new sidecars go under:
it loses its `manifest` pointer and gains them as children, and keeps its
title, its id and every link to it, so nothing is lost. `--into INDEX` removes
the node instead and puts the sidecars under that index — in the node's place,
when it is the node's own parent — each carrying the node's fields. Its title,
and any link to it, are then the losses. A row's own field wins over a node
field of the same name on that row's sidecar. A directory that no longer agrees
with its manifest is not scattered: bring the record up to date first, so that
a missing file is a decision rather than a side effect.

Each verb is one change set: moved, written and removed together.

## 8. Commands

| Command | What it does |
|---|---|
| `attach DIR --manifest [--in P] [--no-hash] [--verbatim] [--exclude PATTERN]...` | cover a directory: mint the node and the manifest, link the node under a parent; `--verbatim` claims its documents too (§3.1), `--exclude` leaves out what something else keeps there (§3.2) |
| `manifest TARGET` | what the manifest says, and whether the directory still agrees (no file reads) |
| `manifest TARGET --update` | rebuild the rows from the directory as it is now, keeping each row's fields, re-stamping the node |
| `manifest TARGET --add FILE...` | append rows for the named files only, checksumming just those; every other row stays, its file present or not (§5) |
| `manifest TARGET --verify` | re-read every listed file and compare its checksum |
| `gather ATTACHMENT... --into DIR [--title T] [--discard FIELD] [--dry-run]` | attachments of one index into one manifest over `DIR` (§7) |
| `scatter TARGET [--into INDEX] [--discard FIELD] [--dry-run]` | a manifest back into one attachment per file (§7) |

`TARGET` is the covered directory, the node, or the manifest document.
