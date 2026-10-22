---
title: prov's own docs fail prov check
part_of: '[Tasks](/docs/tasks/tasks.md)'
created: 2026-10-22T09:20:08.499373Z
status: open
description: An out-of-band edit to a sidecar's payload left a stale content_hash on main, and nothing in CI runs prov check on prov's own workspace to catch it
updated: 2026-10-22T09:20:32.761063Z
---

# prov's own docs fail prov check

## Repro

From a checkout of `main` at `8247d29` or later:

```console
$ prov check
docs/proposals/self-hosted-kernel/examples/ORGANIZATION.md.yaml: fixity mismatch — content changed since its checksum was recorded (bit-rot, or an out-of-band edit)
1 finding(s)
$ echo $?
1
```

`sha256sum` agrees, without prov:

```console
$ sha256sum docs/proposals/self-hosted-kernel/examples/ORGANIZATION.md
771ed06098efacbf2b0e2916957fe4037307841ec1e2d39076315958d0b79196  …
$ grep content_hash docs/proposals/self-hosted-kernel/examples/ORGANIZATION.md.yaml
content_hash: sha256:c5a978f122db7a569ab908a40b2f76cc72760a5fe92d59e69986e1fd96cf1d79
```

## How it got there

`8247d29` (`build(deps)!: move to fig 7 and twig 6`) rewrote the example's
`;;;` JSON block as `---json` frontmatter, along with the prose that describes
it, as part of retiring `;;;`. The edit is deliberate; it was made outside
prov, so the sidecar's `content_hash` still pins the `a56b770` bytes.

It reached `main` because no CI job runs `prov check` over prov's own
workspace. `.config/ship.figl` runs fmt, clippy, the tests, the guide
transcripts, package isolation and MSRV, and none of them reads `docs/`.

## Done when

- The sidecar's `content_hash` matches its payload again (`prov stamp` on
  `ORGANIZATION.md`, once the `8247d29` text is confirmed as the one meant),
  and `prov check` at the root reports no findings.
- A job in `.config/ship.figl` runs the freshly built `prov check` over the
  repository, so the next out-of-band edit to a covered file fails CI instead
  of landing.
