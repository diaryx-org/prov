---
title: A manifest has no add-only verb
description: "Putting files into a covered directory leaves `update_manifest` as the only way to record them, which drops rows for files that have gone and re-hashes every file under the root"
author: adammharris
created: 2026-10-02
updated: 2026-10-02
status: open
part_of: '[Tasks](/docs/tasks/tasks.md)'
---

# A manifest has no add-only verb

A host that writes new files into a manifest-covered directory, such as a
photo album that grows, has one way to record them: `update_manifest`. That
verb rebuilds the rows from the directory as it is now, which costs two
things an add should not:

- **A missing file loses its row.** A listed file that is not on disk (deleted
  by hand, or not yet brought onto this device by a sync client) is dropped
  from the manifest. The manifest is the only record that the file existed, so
  an add that rebuilds over it forgets the loss without reporting it. A host
  can refuse while `manifest_status` reports anything `missing`, and diaryx's
  album add does. But that turns a partly-synced album into one nobody can
  add to.
- **A hashed manifest is re-hashed in full.** `build_manifest` re-reads every
  covered file, by design (a remembered digest must not establish a baseline).
  Adding two photographs to an album of two thousand reads two thousand files.
  On a vault in iCloud Drive that downloads every one.

## Done when

`Workspace` has a verb, say `extend_manifest(node, paths)`, that:

- appends rows for the named paths under the node's root, hashing only those
  when the manifest is hashed. It refuses a path outside the root, a readable
  document, or one already listed;
- leaves every existing row, missing or not, as it was;
- re-stamps the node's `content_hash` over the rewritten manifest in the same
  change set, as `update_manifest` does;
- writes nothing when given nothing.

The CLI gets it as `prov attach --manifest --add <path>...` or similar.
