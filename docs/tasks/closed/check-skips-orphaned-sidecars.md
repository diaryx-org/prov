---
title: check skips orphaned sidecars
description: "The orphan sweep and the missing-containment pass consider only files with a prose extension, so an attachment sidecar nothing lists, or one whose parent has dropped it, is never reported"
author: adammharris
created: 2026-10-05
updated: 2026-10-06
status: done
part_of: '[Closed tasks](/docs/tasks/closed/closed.md)'
---

# check skips orphaned sidecars

**Status.** Done, in `fix(validate): report an attachment sidecar nothing
reaches` (2026-10-06). Both passes now take whole-file documents too and keep
the ones that read as attachment sidecars; machinery stays out, and on 20,000
documents and 2,000 cards `check` timed the same as before.

`spec.md` §4 calls an attachment's sidecar "an ordinary content node", and a
content node is orphan-checked. `check` does not check this one. Both passes
that look for documents nothing reaches choose their population by
extension. The orphan sweep (`validate.rs`, `orphans`) keeps the files in
reached directories that `ContentFormat::from_extension` recognizes, and
the missing-containment pass (`missing_containment`) walks
`content_documents`, which draws the same line. A sidecar is
`photo.jpg.yaml`, a whole-file metadata document, so it is not in either.
Unlisting a card from its parent leaves a node that names a parent, has a
payload and a checksum, and that nothing reaches, and `check` says nothing.

A plain `.md` in the same position is reported, which is the comparison
that makes this a bug and not a scope decision.

## Repro

At `98e6157`:

```sh
prov init ws --title T --yes && cd ws
printf '\xff\xd8\x01' > photo.jpg && prov attach photo.jpg
prov check                       # ok: no findings
# drop the card from the parent's `contents` by hand
perl -0pi -e 's/contents:\n- .*photo\.jpg\.yaml.*\n//' index.md
prov check                       # ok: no findings
printf -- '---\ntitle: Loose\npart_of: "[T](/index.md)"\n---\n' > loose.md
prov check                       # reports loose.md, still not photo.jpg.yaml
```

## Done when

An attachment sidecar that nothing reachable links is a `Finding::Orphan`,
and one that names a parent which does not list it is a
`Finding::MissingContainment` with the same autofix a document gets. Other
whole-file documents stay out of both passes: configuration, the registry,
vocabularies, manifest stores and generated prose are machinery, not content,
and are not orphan-checked. The test is the repro above, plus one showing
that a `prov.yaml` or a manifest store beside the root is still not reported.
Telling a sidecar from machinery means reading each whole-file document in
the scanned directories, so the cost on a large workspace should be measured
before it lands.
