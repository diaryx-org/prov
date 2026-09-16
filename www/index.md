---
title: prov
nav_title: prov
nav_order: 10
description: 'prov — Plaintext Records, Organized and Verifiable. A self-describing plaintext workspace: documents whose structure lives in their own embedded metadata, with identity, relations, and fixity.'
audience: public
part_of: '[prov](/README.md)'
id: j7g9kq0
---
<section class="pj-head">
  <div class="wrap">
    <p><a class="crumb" href="../about/#projects">diaryx.org / projects /</a></p>
    <div class="pj-title" style="margin-top: 1rem">
      <h1>prov</h1>
      <span class="pj-tags">
        <span class="tag-chip">Rust</span>
        <span class="tag-chip">MIT / Apache-2.0</span>
      </span>
    </div>
    <p class="pj-tagline">
      Plaintext Records, Organized and Verifiable — a self-describing
      workspace of plain documents.
    </p>
  </div>
</section>

<section class="pj-main">
<div class="wrap pj-layout reveal">
<div class="pj-body">

A prov workspace is a set of documents whose structure lives in the
documents' own embedded metadata (frontmatter) — not in the
filesystem layout, and not in an app-private sidecar folder. The
files describe themselves, so anything that can read text can read
the whole organization.

- **Documents.** Plain Markdown with YAML frontmatter; no database, no proprietary container.
- **Relations.** An index lists what belongs to it; each child names its parent. Both halves written down, in the files.
- **Identity.** Permanent opaque identifiers minted into the record itself, stable across renames, moves, and republication.
- **Fixity.** Content hashes that make silent drift detectable instead of assumed absent.
- **A thin CLI.** The `prov` binary for working with workspaces without any app at all.

## Where it fits

prov is the vault engine underneath [Diaryx](id:org/80k72t9) — the
part that makes a folder of Markdown behave like an archive. It
stands on [fig](id:fig/gns15jg) for the frontmatter and
[twig](id:twig/wxmq0ww) for the documents. Standalone, it's a
Rust library anyone can build on.

## Status

Works for simple workspaces; active development is incorporating
it into Diaryx. Released to crates.io as eleven crates via one
tagged commit.

</div>
<aside class="pj-aside">
<div class="install">
<span class="install-head">Install</span>
<div class="cmd">brew install diaryx-org/tap/prov <small>CLI</small></div>
<div class="cmd">cargo install prov-cli <small>CLI</small></div>
<div class="cmd">cargo add prov <small>library</small></div>
</div>
<div class="facts">
<div class="row"><span class="k">Language</span><span class="v">Rust</span></div>
<div class="row"><span class="k">Built on</span><span class="v"><a href="../fig/index.md">fig</a> · <a href="../twig/index.md">twig</a> · <a href="https://github.com/diaryx-org/moid">moid</a></span></div>
<div class="row"><span class="k">Used by</span><span class="v"><a href="../index.html">Diaryx</a> — the vault engine</span></div>
<div class="row"><span class="k">Source</span><span class="v"><a href="https://github.com/diaryx-org/prov">github.com/diaryx-org/prov</a></span></div>
<div class="row"><span class="k">Packages</span><span class="v"><a href="https://crates.io/crates/prov">crates.io/crates/prov</a> · <a href="https://docs.rs/prov">docs.rs/prov</a></span></div>
<div class="row"><span class="k">License</span><span class="v">MIT or Apache-2.0</span></div>
</div>
</aside>
</div>
</section>
