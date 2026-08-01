---
title: prov
author: adammharris
created: 2026-07-06
contents:
- '[Design](docs/DESIGN.md)'
- '[Spec](/docs/spec.md)'
- '[Getting Started](docs/getting-started.md)'
- '[Config Vocab](/docs/config-vocab.md)'
- '[Init Adoption](/docs/init-adoption.md)'
- '[Next Steps](/docs/next-steps.md)'
- '[Reference Styles](/docs/reference-styles.md)'
- '[`prov` proposals](/docs/proposals/proposals.md)'
config: prov.yaml
---

# prov (Plaintext Records, Organized and Verifiable)

[![CI](https://img.shields.io/github/actions/workflow/status/diaryx-org/prov/ci.yml?branch=main)](https://github.com/diaryx-org/prov/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/prov.svg)](https://crates.io/crates/prov)
[![docs.rs](https://img.shields.io/docsrs/prov)](https://docs.rs/prov)
[![license](https://img.shields.io/crates/l/prov.svg)](#license)

A *self-describing plaintext workspace*: a set of documents whose structure lives in the documents' own embedded metadata (frontmatter), not in the filesystem layout or an app-private sidecar folder.

## Layout

- **`prov/`** — the library. Documents, relations, identity, and the workspace seam.
- **`prov-cli/`** — a thin command-line companion (the installed binary is `prov`).

## Status

Works for simple workspaces. Active work is currently taking place incorporating prov into [Diaryx](https://diaryx.org).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
