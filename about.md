---
title: How this workspace is organized
generated_by: prov 0.3.2
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
| `link_of` | documents that cross-reference this one | many | `links` |
| `links` | arbitrary cross-references to other documents | many | `link_of` |

Both halves of a pair are kept in step: if A lists B under one, B names A
under its opposite. If you edit one half by hand and not the other,
nothing is lost — the pair is simply inconsistent until someone repairs
it.

`part_of` holds exactly one target, which is what makes the spine a tree
with a single top. `link_of` and `links` are laid over that tree and may
point anywhere; follow them for meaning, never to discover what is here.

## Files that are not part of the tree

Following `contents` will never reach one file here. That is deliberate,
not an omission. `README.md` points at it directly, through a key named
`config`, and it points at nothing in return. It holds this directory's
settings — the file this page was generated from (`prov.yaml`).

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
- **Checksums.** Attachment payloads record a `content_hash`, written as
  `sha256:<hex>` so any checksum tool can verify it. Document bodies are
  not hashed.
- **Deleting.** Deleted documents go to a recycle bin and can be brought
  back until it is emptied.
- **Timestamps.** No modification times are maintained. Any date you find
  in a file was written by a person.

## What is safe to change

All of it. It is your text, and the structure is in the text.

Two fields are worth leaving alone, both because something else may
already depend on them:

- **`id`** — a permanent handle. Ids are never reissued, even after a
  document is deleted, so a reference to a deleted document can still be
  told apart from a reference to something that never existed.
- **`content_hash`** — a checksum. Changing it by hand asserts something
  about the bytes that may not be true.

The relation fields — `contents`, `part_of`, `link_of` and `links` — are
meant to be edited by hand. That is the whole point of keeping them in the
files.

---

<sub>Generated by prov 0.3.2 from this workspace's configuration. Edits to
this file will be overwritten — change `prov.yaml` instead, or run `prov
about` to rewrite this page. The scheme these files follow is called prov;
its specification lived at <https://github.com/diaryx-org/prov>, but you
do not need it to read this directory.</sub>
