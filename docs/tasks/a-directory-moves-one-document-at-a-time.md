---
title: A directory moves one document at a time, and a payload's references do not move at all
description: Workspace::rename moves one document and maintains every reference the census sees; a book — a folder note and the directory under it — has no verb, so a consumer moves it as N renames, and a body reference to an attachment payload is in no census, so it is left pointing where the payload used to be
author: adammharris
created: 2026-09-16
updated: 2026-09-16
status: open
part_of: '[Tasks](/docs/tasks/tasks.md)'
---

# A directory moves one document at a time, and a payload's references do not move at all

**Repro.** A workspace with

```
index.md                 contents: [a/a.md, b/b.md]
a/a.md                   contents: [chapter.md, attachments/photo.jpg.yaml]
                         body: ![](attachments/photo.jpg)
a/chapter.md             body: ![](attachments/photo.jpg)
a/attachments/photo.jpg
a/attachments/photo.jpg.yaml
b/b.md
```

Move the book `a/` under `b/` with what the API offers, which is one
`rename` per document:

1. `rename("a/a.md", "b/a/a.md")` — the parent's entry, the chapter's and
   the sidecar's `part_of`, all follow; the moved body's image is respelled
   for where the payload *is*, `/a/attachments/photo.jpg`.
2. `rename("a/chapter.md", "b/a/chapter.md")` — likewise; its image is now
   `/a/attachments/photo.jpg` too.
3. `rename("a/attachments/photo.jpg.yaml", "b/a/attachments/photo.jpg.yaml")`
   — the payload travels beside its node, the parent's `contents` entry is
   retargeted, and **nothing else is**: the two `![](/a/attachments/photo.jpg)`
   written in steps 1 and 2 name a file that has just left.

Every page in the book has lost every picture, and `check` reports nothing,
because an image is not an edge. The same thing happens to one page and one
picture without a directory in sight: `rename` a sidecar out of a page's
`attachments/` and the page's `![](…)` of its payload stays where it was.

**What is there.** `prov/src/mutate/rename.rs`: `rename` is one document —
inbound rewrites from `inbound_sources`, the mover's own re-relativisation,
`plan_body_move` for the body or payload, the registry — landed as one change
set. `prov/src/mutate/maintain.rs` already has
`collect_inbound_rewrites_multi`, the inbound half of a *set* of moves landing
together, folding every applicable move through one text per source so a mover
that references another mover comes out right; it is `pub(super)` and used only
by `convert_content_format`. The census (`workspace/inbound.rs`,
`prov_graph::graph`) records document-to-document edges; `scan_body_links`
returns images with `image: true` since the body-images fix, and the census
"can keep ignoring images as edges, or count them as references to an
attachment payload; that is a separate decision" —
[rename-leaves-body-images-behind](/docs/tasks/rename-leaves-body-images-behind.md)
left it unmade. This is the decision.

**What to do.** Two things, and the second is what makes the first sufficient.

- A verb that moves a directory as one change set — `move_tree(from_dir,
  to_dir)`, or `rename` accepting a directory. Every document under
  `from_dir` is a mover; `collect_inbound_rewrites_multi` does the inbound
  half in one census; each mover's own links are re-relativised as `rename`
  does today; loose files nothing describes move as bytes; the registry
  follows every id; and it all lands or none of it does. A consumer then
  sees one directory move rather than N file moves, which matters on a
  synced filesystem, and pays one census rather than N.
- A body reference to a payload counts as a reference to the payload's
  sidecar for the purpose of a move: when `photo.jpg.yaml` moves and carries
  `photo.jpg`, every `![](…photo.jpg)` and `[…](…photo.jpg)` that resolves to
  the payload's old path is respelled for its new one — in the multi-move
  collector and in single-document `rename` alike. Whether the census also
  *reports* those as edges (so `check` can find a dangling image) is the
  same decision's other half and can follow.

**Done when.** The repro's book, moved by one call, has every image
resolving; a sidecar renamed away from its page leaves the page's embed
resolving; both are pinned by tests in `prov/src/mutate`; and the spec's
sentence on what a move rewrites says payload references.

**Downstream.** diaryx needs this. Its "Move to…" (`DiaryxWorkspace::move_under`,
`feat(app): a page, a book, or an attachment can be moved to another book`)
moves a book as N `rename`s and an attachment as one, and then does the
payload fix-up itself — `retarget_payload_links`, built on
`scan_body_links` and `link::resolve`, respelling every body reference in a
moved document that resolves to a path that just moved. It works, and it is
prov's job done in the wrong crate: a half-moved book is consistent but
untidy rather than never seen, a synced vault carries N moves, and diaryx
knows something about link maintenance it should not have to. When this
closes, `move_directory` and `retarget_payload_links` in
`diaryx_workspace/src/workspace.rs` become one call, and
`a_book_moves_with_its_directory_and_its_pictures` and
`an_attachment_moves_with_its_payload_and_the_old_page_keeps_showing_it`
there are the tests that say the call is enough.
