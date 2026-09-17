---
title: A move rewrites a document's body links but not its body images
description: Workspace::rename re-relativizes `[label](target)` in the moved document's body and leaves `![alt](target)` verbatim, because the body scan collects twig's `link` nodes and an image is a different node — so a page moved across directories loses every embedded picture
author: adammharris
created: 2026-09-10
updated: 2026-09-10
status: done
part_of: '[Tasks](/docs/tasks/tasks.md)'
---

# A move rewrites a document's body links but not its body images

**Status.** Done, in `fix(rename): a move carries a document's body images`
(2026-09-10). No twig change was needed: twig-doc 3.2.1 already parses
`![…](…)` as an `image` node with the span of the whole construct, and prov
was discarding it. `BodySpans` now reports image spans from the same parse,
`scan_body_links` returns each as a `BodyLink` with `image: true` and a span
that starts after the `!` — so the three rewrites (`rename`'s own-body pass,
the inbound pass, and `convert`'s restyle) carry an image exactly as they
carry a link and cannot drop the `!` — and an empty alt text is kept. The
census and the spanning scan skip images, so `check` reports nothing new; the
separate decision below is still not made. *(Made since, in
[a-directory-moves-one-document-at-a-time](/docs/tasks/a-directory-moves-one-document-at-a-time.md):
the census reports an image by path, and a missing picture is a broken link.)*

**Repro.** A workspace with `page.md` containing

```markdown
[The photo](attachments/photo.jpg)
![A photo](attachments/photo.jpg)
![](attachments/photo.jpg)
```

and `attachments/photo.jpg` beside it. `Workspace::rename("page.md",
"page/index.md")` rewrites the first line to `/attachments/photo.jpg` (or
its relative equivalent, per `path_style`) and leaves the second and third
exactly as they were. Relative to `page/index.md` they now name
`page/attachments/photo.jpg`, which does not exist. Any reader that resolves
body media against the document's own directory shows a hole where each
picture was. `check` does not report it, because an image is not a link and
the census never saw it.

**What is there.** `prov-graph/src/link.rs`: `scan_body_links` takes its
markdown spans from `crate::content::code_and_link_spans`, which reports the
span of each twig `link` node. twig parses `![…](…)` as an `image` node, so
those spans are never returned, and the rewrite in `rename` — which walks
`scan_body_links` over the moved document — never sees them. Two smaller
things sit behind that one: the filter in `scan_body_links` drops a parsed
link whose label is `None`, and an image with no alt text would need to pass
it; and `BodyLink` has no way to say it is an image, which the census may
want to know since a payload is not a document and an image reference is
not an edge in the graph.

Same-directory moves — a retitle that renames `<slug>.md` beside itself —
are unaffected, because a relative target stays valid when the directory
does not change.

**What to do.** A move should carry every body reference whose target is a
workspace path, whichever node twig gives it. The shape is probably:

- `content::code_and_link_spans` (or a sibling) also reports `image` spans,
  tagged, so the one parse still serves both.
- `scan_body_links` keeps images as `BodyLink`s with an `image: bool` (or a
  kind), admitting an empty alt text as a label.
- `rename`'s own-body rewrite treats an image like a link. The census can
  keep ignoring images as edges, or count them as references to an
  attachment payload; that is a separate decision and this task does not
  need it made.

**Done when.** The repro's three lines all resolve after the move, a test in
`prov/src/mutate` pins it, and the spec's sentence on what a move rewrites
says images.

**Downstream.** `diaryx` promotes a page into its own folder when it gets its
first child, and refuses to promote a page that has any attachment because
of this — its test `a_cross_directory_move_does_not_carry_body_media` states
the gap as a fact. When this closes, that refusal can be narrowed to sidecar
children and the test inverted.
