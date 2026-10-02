---
title: A filing entry cannot say what kind of record it files
description: "`filing.<name>` says where records go and what they are filed by, but not which records, so a host with one + for text, photos, recordings and files can only file them all the same way"
author: adammharris
created: 2026-10-02
updated: 2026-10-02
status: open
part_of: '[Tasks](/docs/tasks/tasks.md)'
---

# A filing entry cannot say what kind of record it files

A `FilingSpec` has `label`, `under`, `field` and `nest`. It says where a new
record goes and what it is filed by. It does not say which records it is for.

A host can offer one add for every kind of record: text, a photograph, a
recording, a drawing, an opaque file. Diaryx's + does this. Such a host has no
way to ask "where does a photograph go?" Every kind lands where the one
filing entry, or the host's own "where you are", puts it. A library that wants
photographs in a Photos book by date taken, and pages in Daily by the day
written, has to declare two entries and leave the host to guess which applies.

## Done when

- A filing entry can name the kinds of record it files, in a way a reader of
  `config` understands without prov. A `kind:` list is one candidate:
  `page`, or a payload kind (`image`, `video`, `audio`, `file`) from the same
  classification `fileKind` and payload `kind_of` use, or a `manifest`.
- prov answers "the filing entry for a new record of kind K", with a defined
  rule for an entry that names no kinds (it files anything) and for two
  entries that both claim K (a diagnosed conflict, as other filing issues are).
- `diagnose_filing` reports an unknown kind.
- The CLI's `new --filing` and `Workspace::file` take the kind, or infer it
  from the payload when one is given.
