---
part_of: id:dx4hxn744/kv2bv2m
title: prov
author: adammharris
created: 2026-07-06
contents:
- '[Design](docs/DESIGN.md)'
- '[Spec](/docs/spec.md)'
- '[Getting Started](docs/getting-started.md)'
- '[Querying your files](/docs/querying.md)'
- '[Config Vocab](/docs/config-vocab.md)'
- '[Init Adoption](/docs/init-adoption.md)'
- '[Next Steps](/docs/next-steps.md)'
- '[Reference Styles](/docs/reference-styles.md)'
- '[History Format](/docs/history-format.md)'
- '[Manifests](/docs/manifests.md)'
- '[Provenance](/docs/provenance.md)'
- '[Releasing](/docs/releasing.md)'
- '[Changelog](/docs/CHANGELOG.md)'
- '[Proposals](/docs/proposals/proposals.md)'
- '[Tasks](/docs/tasks/tasks.md)'
- '[Author''s notes](/authors-notes.md)'
- '[prov on diaryx.org](/www/index.md)'
- '[Audiences](/vocab/audiences.md)'
config: prov.yaml
about: about.md
registry: registry.yaml
id: 1ch2991
slug: prov
updated: 2026-10-13T23:51:28.854290Z
---

# prov (Plaintext Records, Organized and Verifiable)

[![CI](https://img.shields.io/github/actions/workflow/status/diaryx-org/prov/ci.yml?branch=main)](https://github.com/diaryx-org/prov/actions/workflows/ci.yml)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

A *self-describing plaintext workspace*: a set of documents whose structure lives in the documents' own embedded metadata (frontmatter), not in the filesystem layout or an app-private sidecar folder.

## Layout

- **`prov/`** — the library. Documents, relations, identity, and the workspace seam.
- **`prov-cli/`** — a thin command-line companion (the installed binary is `prov`).
- **`prov-testkit/`** — filesystem scratch helpers the tests share. Never published.
- **`xtask/`** — the one CI job that is more than a command, `cargo xtask package-isolation`.

## Development

`ship ci` runs everything CI runs, in the order CI runs it; `ship ci --list` lists the individual jobs and `ship ci <job>` runs one. The [workflow](.github/workflows/ci.yml) does nothing but ask `ship ci --matrix` what the jobs are and run each one, so changing CI means editing the `job` list in [`.config/ship.figl`](.config/ship.figl) and nothing else.

Releases are one command: `ship <patch|minor|major|x.y.z>` bumps the workspace version, cuts the changelog's unreleased region into a released section, commits and tags — and pushes only when given `--push`, because the push is what ships the Homebrew binaries. prov is not on crates.io: a Rust project depends on it from git, `prov = { git = "https://github.com/diaryx-org/prov", branch = "main" }`. See [Releasing](docs/releasing.md).

Building needs [Zig](https://ziglang.org) 0.16 on `PATH` — prov's `fig` and `twig-doc` dependencies are Zig-backed, and their build scripts run `zig build`. `nix develop` provides it, as does the `flake.nix` dev shell.

## Status

Works for simple workspaces. Active work is currently taking place incorporating prov into [Diaryx](https://diaryx.org).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
