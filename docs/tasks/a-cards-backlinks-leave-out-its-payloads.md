---
title: A card's backlinks leave out the references to its payload
description: "`backlinks_to` a sidecar lists only the references that name the sidecar, so a page that embeds the picture is not among the places that use it"
author: adammharris
created: 2026-10-05
updated: 2026-10-05
status: open
part_of: '[Tasks](/docs/tasks/tasks.md)'
---

# A card's backlinks leave out the references to its payload

An attachment is one node with two handles: the sidecar, which a relation or
an `id:` reference names, and the payload, which a body embed or a download
link names. A move already treats a reference to either as a reference to the
node and respells both. "Who links here?" does not. `backlinks_to` the sidecar
inverts the census at exactly one path, so it returns the parent's `contents`
entry and any `id:` reference, and leaves out every `![](photo.jpg)` that
actually shows the picture. `backlinks_to` the payload returns the opposite
half. A host listing "used in" for a card has to ask twice and merge the
answers itself, and it has no way to tell from a `Backlink` which handle a
reference used.

It was left alone because two callers depend on the present meaning.
`plan_scatter` asks for a manifest node's backlinks to count what a scatter
would leave pointing at nothing, and `prov backlinks <file>` prints the
references to exactly the path it was given, each marked `id` or `path`.
Merging the two halves into `backlinks_to` would change both answers under
them, and the CLI's output has no column to say which handle a line used.

## Repro

```sh
prov init ws --title T --yes && cd ws
printf '\xff\xd8' > photo.jpg && prov attach photo.jpg
prov new Page --in index.md
echo '![](/photo.jpg)' >> page.md
prov backlinks photo.jpg.yaml    # index.md  contents[0]  path
prov backlinks photo.jpg         # page.md   body         path
```

`page.md`, which shows the picture, is not among the card's backlinks, and
the parent that lists the card is not among the payload's.

## Done when

A caller can ask for every reference to an attachment, through either handle,
in one call, and tell from each `Backlink` whether it named the sidecar or the
payload. This could be a `Backlink` field (`via_payload`, say) and a
node-level query beside `backlinks_to`, leaving `backlinks_to` as it is for
`plan_scatter` and `prov backlinks`, or it could be a change to
`backlinks_to` once both callers are moved off it. Either way a test covers a card reached
by its parent's `contents`, by an `id:` reference and by a body embed of the
payload.
