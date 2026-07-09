```fig
title = colophon
author = adammharris
created = 2026-07-06
contents = [[Design](docs/DESIGN.md)]
```

# colophon

A *self-describing plaintext workspace*: a set of documents whose structure lives in the documents' own embedded metadata (frontmatter), not in the filesystem layout or an app-private sidecar folder.

The name is the point. A *colophon* is the note in which a book describes its own making — the type, the paper, the press. A colophon workspace is one you can hand to any tool and it explains itself: follow the links in the metadata and the whole structure unfolds, with a distinguished root that describes the whole.

## Layout

- **`colophon/`** — the library. Documents, relations, identity, and the workspace seam.
- **`colophon-cli/`** — a thin command-line companion (the installed binary is `colophon`).

## Filesystem

colophon is generic over the small async [`colophon::Storage`](colophon/src/fs.rs) trait, which mirrors the slice of `std::fs` the scan/traverse/mutate engine needs. Implement it over `std::fs`, `tokio::fs`, or a browser filesystem (OPFS/IndexedDB) — the workspace never learns which.

## Status

Works for simple workspaces.

Development resumed, now that [Twig](https://github.com/adammharris/twig) has gained the ability to parse markdown document structure. Currently Twig is a path dependency: `../twig/bindings/rust` so you will have to `git clone` and have `cargo` and `zig` toolchains in order for Colophon to build.

## License

Not licensed for now.