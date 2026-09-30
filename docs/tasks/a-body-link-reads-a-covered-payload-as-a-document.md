---
title: A body link reads a covered payload as a document
description: "A file under a manifest is opaque bytes, but a body link to it makes `check` parse it — so a captured text file that happens to contain a metadata block, `content_hash` included, reports a fixity mismatch against a checksum it never had"
author: adammharris
created: 2026-09-30
updated: 2026-09-30
status: open
part_of: '[Tasks](/docs/tasks/tasks.md)'
---

# A body link reads a covered payload as a document

A manifest claims every file under its root that prov cannot read as text,
and `manifests.md` §3 says a covered file "is not a document". `check` agrees
until a document links one from its body. Then the payload joins the reachable
set and is parsed like any other document, whatever its extension, and
whatever metadata block it seems to hold is taken as its own.

Found by saving shell output as manifest-covered `.txt` files, each linked from
the conversation it came from. A command that printed an HTML page carrying a
`<script type="application/yaml">` data island, `content_hash` and all,
produced a payload `check` then called corrupt.

## Repro

On prov 0.17.0 (`1cf4e01`):

```sh
prov init ws --title T --yes && cd ws
prov new S --in index.md
mkdir s-files
printf '$ cat page.html\n\n<script type="application/yaml">\ntitle: X\ncontent_hash: sha256:00\n</script>\n' > s-files/out.txt
prov attach --manifest s-files --in s.md
prov check                                  # ok: no findings
echo '[out](/s-files/out.txt)' >> s.md && prov stamp s.md
prov check
```

The last `check` reports `s-files/out.txt: fixity mismatch` and a warning that a
document records a `content_hash` of its own body. The same happens under
`.log` and `.out`.

## Done when

A link into a manifest's root resolves to the covered file without reading it:
`check` confirms the file exists and, if needed, that the manifest lists it,
but never parses its bytes as metadata. A covered text file keeps being a
payload, whatever it links or contains.
