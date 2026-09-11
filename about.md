---
title: How this workspace is organized
generated_by: prov 0.11.1
---

# How this workspace is organized

This directory is a set of plain text files that describe their own
structure. Nothing about how they fit together is kept outside them — no
database, no index, no hidden folder. Each file states, in a block at the
top of itself, what it belongs to and what belongs to it. Follow those
statements and the whole directory unfolds.

Nobody wrote this page. It was produced by reading this directory's own
settings, so it describes what the files actually declare rather than what
someone remembered.

## Start at `README.md`

`README.md` is the root. Everything else here hangs off it, directly or
through something else that does.

## Every file opens with a metadata block

A file begins with a line containing three dashes:

```
---
title: Some Document
part_of: '[Label](/path/from/here.md)'
---

The rest of the file is the document itself.
```

Everything between that line and the next `---` is the file's metadata,
written in YAML. Everything after it is the document.

The order of the keys is for your benefit, not the machine's. Some files
are metadata all the way down, with no document part. You can tell by the
extension: `.yaml`, `.yml`, `.json`, `.toml`, `.fig`, and `.figl` files
have no separate body and no fence.

## How to read a reference

References here are written like a Markdown link:

```
part_of: '[Label](/path/from/here.md)'
```

The text in brackets is decoration — a human label, safe to change. The
target is in the parentheses.

A target beginning with `/` is a path from **this directory**, the top of
the workspace; it is not a path from the root of your filesystem. Anything
else is a path relative to the file you found it in. Fold `.` and `..`
yourself, and do not follow symlinks.

Other spellings mean the same thing and are understood wherever a
reference can appear, so you may meet them in files someone edited by
hand:

| written | called |
| --- | --- |
| `[[/path/from/here.md]]` | a wikilink holding a path |
| `/path/from/here.md` | a bare target |

A target containing `://`, or beginning with `mailto:`, points outside
this directory and is never resolved. A target naming a file that is not
here is simply broken — worth noting, not a reason to stop reading.

## How the files relate to each other

Four relations are used here. Follow **`contents`** from `README.md` to
reach every document; that is the spine, and every file sits at exactly
one place along it.

| relation | means | how many | its opposite |
| --- | --- | --- | --- |
| `contents` | documents contained by this one | many | `part_of` |
| `part_of` | the document that contains this one | one | `contents` |
| `links` | arbitrary cross-references to other documents | many | `link_of` |
| `link_of` | documents that cross-reference this one | many | `links` |

Both halves of a pair are kept in step: if A lists B under one, B names A
under its opposite. If you edit one half by hand and not the other,
nothing is lost — the pair is simply inconsistent until someone repairs
it.

`part_of` holds exactly one target, which is what makes the spine a tree
with a single top. `links` and `link_of` are laid over that tree and may
point anywhere; follow them for meaning, never to discover what is here.

## Fields with fixed vocabularies

One field does not hold free text. Its permitted values are listed in
files of their own. Where the table names a place, the rule holds for the
files under that index and no others; a file elsewhere may hold anything
in the field, or nothing.

| field | where | rule | values listed in |
| --- | --- | --- | --- |
| `status` | under `Tasks` | **closed** — every value must appear in the list | `/vocab/task-statuses.yaml` |
| `status` | under `Proposals` | **closed** — every value must appear in the list | `/vocab/proposal-statuses.yaml` |

A closed field is worth taking seriously: a value not on the list is an
error rather than a new category.

## Other ways through the files

The arrangement described above puts every document in exactly one place,
which is what lets the whole directory be walked from its top. Three other
ways of reading the same documents are written down here as well. Each
gathers the files into groups by something the files themselves say, and
none of them is a second copy of anything: a document can turn up under
several groups, or under none, and still sit in the one place the
arrangement above gives it.

| what it is called | grouped by | covers | shows everything it covers |
| --- | --- | --- | --- |
| Open tasks | what the file says under `status` | what is filed under `Tasks`, however deep | no |
| Proposals | what the file says under `status` | what is filed under `Proposals`, however deep | no |
| All work | what the file says under `status` | every file here | no |

Where that last column says no, a further condition is set on the grouping
— a value a file has to carry, or one it must not — and files in range
that do not meet it are left out. The condition itself is written in this
directory's settings rather than repeated here; the files it hides are
still ordinary files, reachable the way everything else here is.

## Files that are not part of the tree

Following `contents` will never reach some of the files here. That is
deliberate, not an omission. `README.md` points at each of them directly,
through a key that names what it is, and none of them points back.

| key in `README.md` | what it points at |
| --- | --- |
| `config` | this directory's settings — the file this page was generated from (`prov.yaml`) |
| `fields.status.vocabulary` | the permitted values of `status` under `Tasks` (`/vocab/task-statuses.yaml`) |
| `fields.status.vocabulary` | the permitted values of `status` under `Proposals` (`/vocab/proposal-statuses.yaml`) |

Files reached that way are machinery: they are not documents in the tree,
they carry no `part_of`, and they are not counted when something asks what
this workspace contains.

A binary file is never part of the tree either. To bring in an image or a
PDF, a small text file is created beside it that names it under a
`content` key; that text file is the document, and the binary rides along
as its payload. Everything in the tree is plain text, always.

## Conventions in this workspace

- **Identity.** A document earns a permanent id the first time something
  links to it by id, or when it is published — not before. The id is
  stamped into the document's own metadata, and a registry file mirrors
  it. Because the id lives in the file, it survives being moved or copied.
- **Checksums.** Anything kept in a file of its own — an attachment's
  payload, a document whose prose sits beside it — is recorded with a
  `content_hash` in the small file that points at it, written as
  `sha256:<hex>` so any checksum tool can verify it independently: run
  `sha256sum` on the file and compare. A document that keeps its prose
  inline is not checksummed here; whatever backs up or version-controls
  this directory is what says its text changed.
- **Deleting.** A deletion destroys the file. What is kept is a record of
  it — where the document sat, what it was called, and which document
  listed it — so that if you get the file back from wherever this
  directory is backed up or version-controlled, the record says what it
  was part of. Nothing here can give you the file itself back.
- **Timestamps.** Two fields hold a UTC instant in RFC 3339 form
  (`1974-03-02T14:05:00Z`): `created` is written when a document is made
  and not touched after that, and `updated` is maintained automatically as
  the document changes. Any other date you find in a file was written by a
  person.
- **Starting values.** A document made here opens with `status` set to
  `open` (under `Tasks`) and `status` set to `draft` (under `Proposals`).
  That is where it starts, not a rule it has to keep: a file that says
  something else was changed on purpose.

## What is safe to change

All of it. It is your text, and the structure is in the text.

Three fields are worth leaving alone, each because something else may
already depend on them:

- **`id`** — a permanent handle. Ids are never reissued, even after a
  document is deleted, so a reference to a deleted document can still be
  told apart from a reference to something that never existed.
- **`content_hash`** — a checksum. Changing it by hand asserts something
  about the bytes that may not be true.
- **`updated`** — maintained automatically, and in a fixed format.

The relation fields — `contents`, `part_of`, `links` and `link_of` — are
meant to be edited by hand. That is the whole point of keeping them in the
files.

---

<sub>Generated by prov from this workspace's configuration. Edits to this
file will be overwritten — change `prov.yaml` instead, or run `prov about`
to rewrite this page. The scheme these files follow is called prov; its
specification lived at <https://github.com/diaryx-org/prov>, but you do
not need it to read this directory.</sub>
