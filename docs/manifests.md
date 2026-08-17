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

Each row:

- `path` — the file, relative to **`root`** (not the workspace), `/`-separated,
  normalized. Required. A row that climbs out of `root` is refused.
- `hash` — `sha256:<64 lowercase hex>`, the digest of the file's bytes exactly as
  `prov::fixity::digest` computes it, so `sha256sum` verifies it independently.
  Optional (§3).

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

The node's pin is written whenever the workspace's `fixity` axis covers payloads
(the default), independently of whether the rows carry hashes: pinning one small
file costs nothing, and it is what the rest hangs from.

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

**The repair is never automatic.** Rebuilding a manifest accepts the directory as
it stands — including a file that has *vanished*, which it writes out of the
record as though the loss were intended. That is the judgment `FixityMismatch`
declines to make on the author's behalf, on the same evidence, so
`check --fix mechanical` reports drift and moves on.

## 6. Moving, deleting, and history

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

**A history capture parks the manifest, not the photographs.** Covered files are
not in the reachable set: they are opaque bytes, never orphan candidates, and
adding ten thousand paths to every walk would make an archive pay on each of them
for a check none can fail. So damage to a covered file stays **detectable** —
every hash is on record in the captured manifest — but is not undoable from
`history/blobs/`. The alternative was duplicating the whole archive into the
history store on its first capture, which is a worse default for the only
workspaces this feature exists for. Keep the bytes safe the way the rest of an
archive is kept safe: a backup, on separate media.

## 7. Commands

| Command | What it does |
|---|---|
| `attach DIR --manifest [--in P] [--no-hash]` | cover a directory: mint the node and the manifest, link the node under a parent |
| `manifest TARGET` | what the manifest says, and whether the directory still agrees (no file reads) |
| `manifest TARGET --update` | rebuild the rows from the directory as it is now, re-stamping the node |
| `manifest TARGET --verify` | re-read every listed file and compare its checksum |

`TARGET` is the covered directory, the node, or the manifest document.
