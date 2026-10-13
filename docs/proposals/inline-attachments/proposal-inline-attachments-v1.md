---
title: inline attachments
author: adammharris
created: 2026-10-13
updated: 2026-10-13
status: accepted
part_of: '[Proposals](/docs/proposals/proposals.md)'
derived_from:
- '[path-valued fields](/docs/proposals/path-valued-fields/proposal-path-valued-fields-v1.md)'
---
# Inline attachments — a picture's card lives in the page that shows it

## Status

**Accepted** (2026-10-13), and phase 1 of §9 is built on `main`, not yet
released: `type: payload`, the owned-payload rules of §4, the spec §4 row,
and `attach --inline`. Promotion and demotion (§6), diaryx's side (§7) and
projection (§8.1) are not. The body is left as argued. Three things settled
differently in the build:

- **The checksum key is `content_hash`, not `sha256`.** It is the key a
  sidecar uses, so a row and a card say the same thing the same way, and
  promotion (§6) can copy a row's keys verbatim. The digest is spelled as a
  sidecar's is, `sha256:…`.
- **`rm` deletes an owned file even when another document links to it**, and
  reports each such link among what the delete left dangling. §4.2 kept it.
  But a sidecar's payload already goes with its sidecar however many pages
  show it, and an owner whose file can outlive it is not one: keeping the
  file would leave it owned by nothing, which no finding can see.
- **`stamp` is the only verb besides `attach --inline` that writes a row's
  checksum.** An ordinary save does not, because hashing every picture a
  page owns on every keystroke is a cost no save should pay; a row's bytes
  are not edited through prov, so nothing a save does can make its checksum
  wrong. A mismatch is `PayloadFixityMismatch`, named at the checksum's own
  address (`attachments[0].content_hash`), and its repair is a judgment, as
  `FixityMismatch`'s is.

The CLI spelling is `prov attach FILE --in DOC --inline`, with `--field
PATH` when a workspace declares more than one `type: payload` path.

## Summary

Today every attachment is a node. `attach` mints a sidecar
(`photo.jpg.yaml`) whose `content` field names the payload, and the payload
rides along as that node's body (spec §4). That is the right shape for a
scan filed into an archive: a card of its own, with an id, a place in the
spine, and a row in every view.

It is the wrong shape for a photograph dropped into a journal entry. That
picture is part of the entry's text, not a document beside it. Diaryx says
so with a key of its own, `embedded: true`, written onto the sidecar and
read back to decide whether the entry is a page or a book. The key exists
because the file layout cannot tell the two apart.

This proposal lets a document carry its attachments' metadata **in its own
frontmatter**, one row per payload, with no sidecar:

```yaml
---
title: Fjord trip
attachments:
- content: fjord.jpg
  sha256: 2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae
  created: 2026-07-24
  people: [Ada]
---
We took the ferry at nine.

![](fjord.jpg)
```

Then "embedded" is where the metadata lives rather than a flag. A row in a
document is part of that document. A sidecar is a card standing on its own.
Moving a picture from one role to the other is an operation (§6), not a
flag flip that can drift from the truth.

## 1. Why a flag is the wrong carrier

Diaryx's `embedded:` key (`diaryx_workspace/src/index.rs`, `EMBEDDED_KEY`)
answers one question: is this attachment part of its page's text, or a
chapter of it? It has to be written once, at birth, by every caller that
knows why the file is coming in. A sidecar written before the key existed
has no answer, so the reader falls back to whether the page's body links
the payload *right now*, which is the rule the key was introduced to stop
depending on. There are now two answers to one question, and code to
reconcile them.

The flag is also a claim one file makes about its relationship to another.
Nothing checks it: a sidecar saying `embedded: true` under a parent that has
never mentioned it is a quiet inconsistency, the class of thing prov exists
to report. And it does not belong to prov at all, which is why prov has
never read it. The fact it records is structural, but it is written as
data.

Putting the metadata in the document that embeds the payload makes the fact
structural. There is nothing to write, nothing to fall back on, and nothing
that can disagree.

## 2. Spec §5 already argues for it

§5's placement rule is *inline until it grows, churns, or wants its own
cadence*. That rule is written for policy, but it describes attachments
just as well:

- A photograph in an entry is small, stable, and edited when the entry is
  edited. It is inline.
- A scan filed into an archive has its own title, its own transcription, its
  own audience and its own history. It wants its own cadence, and it earns
  a file.

At present every attachment is split, whatever it is. This proposal gives
attachments both homes, as policy already has.

## 3. What already works

The [path-valued fields](/docs/proposals/path-valued-fields/proposal-path-valued-fields-v1.md)
proposal gave prov the address grammar (`attachments[].content`) and a link
type (`type: ref`) that reaches into list items. Declared that way, a row's
payload is already a link site. In a scratch workspace on `c26378b`:

| Action | Result |
| --- | --- |
| `check` with the payload present | no finding: a `ref` resolves to a non-document file |
| `mv fjord.jpg sub/fjord.jpg` | the row and the body's `![](…)` are both rewritten |
| delete the payload | `BrokenLink` at `attachments[0].content`, beside the body's |

So resolving, rewriting and reporting the reference needs no new work. What
a `ref` does not do is **own** its target, and owning is the difference
between citing a file and carrying it.

## 4. What prov has to learn: a reference that owns its target

Two more results from the same scratch workspace show the gap:

- `mv trip.md trips/trip.md` moves the entry and rewrites its row to
  `/fjord.jpg`. The picture stays where it was. With a sidecar, the payload
  travels with its node.
- `rm fjord-trip.md` deletes the entry and leaves `fjord.jpg` behind, and
  `check` then has no findings. The picture is stranded, and nothing says
  so.

A citation (`sources[].resource`) should behave this way: deleting a note
must not delete the census sheet it cites. An attachment should not. So
this is a new type, not a new behaviour for `ref`:

```yaml
prov:
  fields:
    attachments[].content:
      type: payload
```

A **`payload`** value is a `ref` (everything in §3 holds) whose target is an
opaque file the document **owns**:

1. **It travels.** Moving the document moves each payload it owns. The
   payload's path relative to the document stays the same, as a sidecar's
   payload keeps its place relative to the sidecar. A payload outside the
   document's directory is left in place and its row rewritten, as now. The
   owner did not put it there, and moving it would surprise whoever did.
2. **It goes with its owner.** `rm` deletes the payloads its document owns,
   records them in the deletion log beside the document, and `restore`
   brings back the rows once the bytes are back. A payload that another
   document still links to (body or `ref`) is kept, and `rm` reports which
   links kept it.
3. **It is hashed.** A row's `sha256` covers a file of its own, so it passes
   the getting-started §11 test: `sha256sum` reproduces it without knowing
   about prov. `attach` writes it, `stamp` refreshes it, and `check` reports
   `FixityMismatch` against the document at the row's concrete address
   (`attachments[0].sha256`). The finding names the document, because the
   document made the claim and is the file a repair rewrites, as a
   sidecar's finding names the sidecar.
4. **It has one owner.** Two `payload` values, in one document or two,
   naming the same file is a finding, `PayloadOwnedTwice`. It is
   diagnosis-only, because the repairs (drop a row, or promote to a card that
   both link) say different things. Any number of documents may still
   *link* to an owned payload. They just do not own it.
5. **It is opaque.** It is never parsed, never a node, never in the
   registry, as with a sidecar's `content` today.

The row's other keys are carried. They are data, the same tier as any field
nobody declared, and a workspace declares them as it declares any field
(`attachments[].created: { type: date }`, `attachments[].people: { values:
closed, … }`). That is what lets the same vocabularies govern a row and a
card.

`payload` is a field type and not a fixed key. A workspace that calls them
`media` or `figures` declares that path instead, and a document may own
payloads under more than one path. The base preset declares nothing, and
diaryx's preset declares `attachments[].content`.

### Spec §4 gains a row

| Target | Declared as | Contract |
| --- | --- | --- |
| **Owned payload** | a `fields` value under `type: payload` | *not a node* — an opaque file a document carries in its own metadata, one row per file. A `ref` in every respect (censused, checked, rewritten on move), and also: moved and deleted with its owner, hashed per row, one owner per file. No id, no inverse, not in the spanning tree |

The rule for choosing, to sit beside the relation/path-valued rule: *a file
that is part of a document's text is owned by it; a file that is a record of
its own gets a card.*

## 5. What it gives up, on purpose

A row is not a node, so an inline attachment has none of what a node has:

- **No id.** `![](id:x)` and `id:<workspace>/<id>` cannot name it. It is
  reached by the path to its payload, as the body's own `![](fjord.jpg)`
  already reaches it. A picture something else needs to cite by id should
  be a card, and promoting it (§6) is how it becomes one.
- **No row in a view.** `prov docs`, `prov views` and `prov search` list
  documents, and the row's document is listed in its place. A view can still
  read the rows: `field('attachments[].people')` returns the people in an
  entry's pictures, so "entries with Ada in a photo" is one condition today.
  What no view can produce is *the photos themselves* as rows, filed by
  their own dates.
- **No inverse and no spine.** A row has no `part_of`. It is in its
  document the way a paragraph is.

The second point is the real cost, and the one an app will run into first:
a photo album across a library is a view over pictures, not over entries.
§8 leaves it open, because there are two honest answers and this proposal
does not need to pick one to be useful.

## 6. Promotion and demotion

A picture's role changes. A photograph taken out of a page and filed into a
book as a card of its own is the case diaryx's `set_embedded` exists for.
Each direction is one change set, so a crash leaves either the row or the
card, never both and never neither:

- **Promote** (row → card): mint `fjord.jpg.yaml` beside the payload, with
  `content: fjord.jpg` and every other key from the row copied verbatim, list
  it in the new parent's `contents`, and remove the row. The body's
  `![](fjord.jpg)` still resolves: it is a body reference to a payload, which
  spec §4 already censuses by path. The card gets an id the way any node
  does.
- **Demote** (card → row): copy the sidecar's keys, minus the node's own
  (`content` becomes the row's `content`; `id`, `part_of` and `title` are
  dropped), into a row on the document named, then delete the sidecar.
  Refused while anything links to the card's id, because that link would
  dangle, and the refusal names each one.

Spelling, for the CLI: `prov attach FILE --in DOC --inline` writes a row
instead of a sidecar, and `prov attach --promote fjord.jpg` (the payload names its row) /
`prov attach --demote CARD --into DOC` move between the two. The names are
the least settled part of this document.

## 7. What diaryx does with it

- **`embedded:` is retired.** `Attachment::embedded` becomes "is this a row"
  and needs no key and no fallback. `import_attachment_as(…, embedded, …)`
  becomes the choice of carrier. `set_embedded` becomes promote/demote.
- **The importer writes rows.** `attach_embed` (`diaryx_import/src/writer.rs`)
  writes a row onto the entry. A record (`write_item_landed`'s
  `Some(artifact)` arm) stays a card, as it should, since it is one.
- **Migration** folds every `embedded: true` sidecar into a row on its
  parent, through demotion and with the same refusal: a card something links
  to by id stays a card, and the migration reports it. `embedded: false`
  sidecars are cards already and are left alone.
- **The frontmatter is not shown raw.** An entry with thirty photos has
  thirty rows. The editor already hides frontmatter, and the tray already
  lists attachments, so this moves where the tray reads from, not what a
  person sees.
- **Sync** now sees a retag and a prose edit as edits to one file. Whether
  that conflicts depends on whether the merge is per-file or per-region
  (frontmatter and body), and needs checking against the sync path before
  migration ships, not after.

## 8. Open questions

1. **Pictures as rows in a view.** Two answers. (a) A view can opt into
   *projecting* a `payload` path, so each row becomes a row of its own,
   carrying its document as a column. That is a census over rows as well as
   documents, and the bigger piece of work. (b) Promote anything that needs
   to be a record. Diaryx's album files cards today (`album.rs`), so (b)
   costs nothing now, and (a) can wait for a frontend that asks for it.
2. **Where a transcription goes.** Diaryx hangs a recording's transcript
   off its card. With no card, the transcript could be a child of the
   document with a `transcribes:` naming the file, typed `ref` and not
   `payload`, since a transcript cites the file and does not own it. Or a recording with a transcript
   should be a card. The second is simpler and is probably true: a
   transcript is a second document about the file, and two documents about
   one file is what a card is for.
3. **Locators.** Should `entry.md#attachments[0]` be a way to cite one row?
   Locators are carried and never resolved, so it would cost nothing to
   allow, but a row's index changes when one above it is removed, which
   makes it a bad address. A path to the payload is a better one, and
   already works.
4. **Audience.** An inline picture inherits its document's audience, which
   is right for a photo in an entry. A per-row `audience` would be read by
   diaryx's publisher, not prov, and is not proposed here.

## 9. Phasing

1. **`type: payload`, owned-payload semantics (§4) and the spec §4 row**,
   in prov, with `attach --inline`. Diaryx can then write rows for new
   embeds while existing sidecars keep their key.
2. **Promote and demote** in prov, and diaryx's `set_embedded` rewritten
   over them.
3. **Diaryx migration** and retiring `EMBEDDED_KEY`, after the sync
   question in §7 is answered.
4. Projection (§8.1), if and when something asks.
