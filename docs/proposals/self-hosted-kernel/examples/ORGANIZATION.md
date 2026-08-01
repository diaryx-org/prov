---
title: How this workspace is organized
generated_by: prov 0.1.0
---

<!--
  EXAMPLE — not a live file.

  This is what `prov about` would generate for a workspace that declares its
  own relation vocabulary and departs from the defaults nearly everywhere:
  JSON metadata, references addressed by id rather than path, no registry,
  permanent deletes, controlled vocabularies, and a history store.

  Compare it with prov-1.draft.md, the same generator run against this
  repository. Neither page was hand-tuned for length. A stranger genuinely
  needs to be told more about this directory, so the generated page is
  longer — and if a default workspace ever produced a page this long, the
  generator would be padding.
-->

# How this workspace is organized

This directory is a set of plain text files that describe their own
structure. Nothing about how they fit together is kept outside them — no
database, no index, no hidden folder. Each file states, in a block at the
top of itself, what it belongs to and what belongs to it.

Nobody wrote this page. It was produced by reading this directory's own
settings, so it describes what the files actually declare rather than what
someone remembered.

Read the section on references before anything else. Files here point at
each other by permanent identifier rather than by filename, which is
unusual and which nothing else in the directory will explain to you.

## Start at `README.md`

`README.md` is the root. Everything else here hangs off it, directly or
through something else that does.

## Every file opens with a metadata block

A file begins with a line containing three semicolons:

```
;;;
{
  "title": "Minutes of the Board, 1974-03-02",
  "id": "k9fp2qc",
  "section_of": "[[prov:aj7eqx|Board Records]]"
}
;;;

The rest of the file is the document itself.
```

Everything between that line and the next `;;;` is the file's metadata,
written as a single JSON object. Everything after it is the document. The
order of the keys is for your benefit, not the machine's.

Some files are metadata all the way down, with no document part. You can
tell by the extension: `.yaml`, `.yml`, `.json`, `.toml`, `.fig`, and
`.figl` files have no separate body and no fence.

## How to read a reference

References here are written as double-bracketed links addressed by
**identifier**, not by filename:

```
"section_of": "[[prov:aj7eqx|Board Records]]"
```

The text after the `|` is decoration — a human label, safe to change. The
part that matters is `prov:aj7eqx`, and it names a *document*, not a
location.

**To resolve one, search this directory for the file whose `id` field is
`aj7eqx`.** There is no index to consult and no lookup table; every
document carries its own id in its metadata block, and that is the only
place the mapping exists. A plain text search for the identifier will find
it. This is why files can be renamed and moved here without breaking
anything: nothing points at a filename.

An identifier is permanent. It is never reissued, even after a document is
deleted, so a reference that resolves to nothing means "this document is
gone," never "this identifier belongs to something else now."

Two other spellings are understood wherever a reference can appear, so you
may meet them in files someone edited by hand:

| written | called |
| --- | --- |
| `[[minutes/1974-03-02.md]]` | a wikilink holding a path, not an id |
| `[Minutes](minutes/1974-03-02.md)` | a Markdown link |

Where a path does appear: one beginning with `/` is a path from **this
directory**, the top of the workspace — not from the root of your
filesystem. Anything else is relative to the file you found it in. Fold `.`
and `..` yourself, and do not follow symlinks. A target containing `://`,
or beginning with `mailto:`, points outside this directory and is never
resolved.

## How the files relate to each other

Six relations are used here. Follow **`sections`** from `README.md` to
reach every document; that is the spine, and every file sits at exactly one
place along it.

| relation | means | how many | its opposite |
| --- | --- | --- | --- |
| `sections` | records filed under this one | many | `section_of` |
| `section_of` | the record this one is filed under | one | `sections` |
| `cites` | records this one draws on | many | `cited_by` |
| `cited_by` | records that draw on this one | many | `cites` |
| `supersedes` | the record this one replaces | one | `superseded_by` |
| `superseded_by` | the record that replaces this one | one | `supersedes` |

Both halves of a pair are kept in step: if A lists B under `sections`, B
names A under `section_of`. If you edit one half by hand and not the other,
nothing is lost — the pair is simply inconsistent until someone repairs it.

`section_of` holds exactly one target, which is what makes the spine a tree
with a single top. `cites` and `supersedes` are laid over that tree and may
point anywhere; follow them for meaning, never to discover what is here.

## Fields with fixed vocabularies

Two fields do not hold free text. Their permitted values are listed in
files of their own.

| field | rule | values listed in |
| --- | --- | --- |
| `audience` | **closed** — every value must appear in the list | `/vocab/audiences.yaml` |
| `tags` | **open** — any value is allowed; the list records the ones in use | `/vocab/tags.yaml` |

Each of those files maps a term to a short explanation and a permanent
identifier of its own. A term marked `retired: true` was valid once and is
not reissued.

`audience` being closed is worth taking seriously: a value not on the list
is an error rather than a new category, because the field governs who a
record may be shown to.

## Files that are not part of the tree

Following `sections` will never reach some of the files here. That is
deliberate, not an omission. `README.md` points at each of them directly,
through a key that names what it is, and none of them points back.

| key in `README.md` | what it points at |
| --- | --- |
| `config` | this directory's settings — the file this page was generated from |
| `history` | a record of past states of this directory (below) |
| `fields.audience.vocabulary` | the list of permitted audiences |
| `fields.tags.vocabulary` | the list of tags in use |

Files reached that way are machinery: they are not documents in the tree,
they carry no `section_of`, and they are not counted when something asks
what this workspace contains.

A binary file is never part of the tree either. To bring in an image or a
scan, a small text file is created beside it that names it under a
`content` key; that text file is the document, and the binary rides along
as its payload. Everything in the tree is plain text, always.

## The history store

`history/` holds a record of what this directory contained at particular
moments. Each entry lists every file present at that moment, with a
checksum of its contents, and `history/blobs/` holds the bytes themselves,
named by checksum.

It exists so that damage — a bad merge between two copies of this
directory, a file mangled in transit — can be identified and undone. If you
are trying to work out whether something is missing or has been altered,
that is where to look. Entries are written and never edited afterward.

## Conventions in this workspace

- **Identity.** Every document is given a permanent id when it is created,
  stored in its own metadata block. There is no registry file; the ids in
  the documents are the only record, which is why they survive being moved
  or copied.
- **Checksums.** Every document records a `content_hash` of its body, and
  every attachment payload one of its bytes, written as `sha256:<hex>` so
  any checksum tool can verify it independently. Editing a document's text
  outside this tool will leave its checksum stale — repairable, not lost.
- **Deleting.** There is no recycle bin here. A deletion is immediate and
  permanent; a past state can still be recovered from `history/`.
- **Timestamps.** A field named `modified` is maintained automatically and
  holds a UTC instant in RFC 3339 form (`1974-03-02T14:05:00Z`). Any other
  date you find in a file was written by a person.

## What is safe to change

All of it. It is your text, and the structure is in the text.

Three fields are worth leaving alone, each because something else may
already depend on them:

- **`id`** — a permanent handle, and here the *only* record of which
  document is which. Every reference in this directory resolves through it.
  Change one and every link to that document stops resolving; reuse one and
  two documents become indistinguishable.
- **`content_hash`** — a checksum. Changing it by hand asserts something
  about the bytes that may not be true.
- **`modified`** — maintained automatically, and in a fixed format.

The relation fields — `sections`, `section_of`, `cites`, `cited_by`,
`supersedes`, `superseded_by` — are meant to be edited by hand. That is the
whole point of keeping them in the files.

---

<sub>Generated by prov 0.1.0 from this workspace's configuration. Edits to
this file will be overwritten — change the configuration instead, or run
`prov about` to rewrite this page. The scheme these files follow is called
prov; its specification lived at <https://github.com/diaryx-org/prov>, but
you do not need it to read this directory.</sub>
