---
title: A `generated` pair says who and when, and not what was done
description: "`generated` records the actor and the instant a document came to exist, and nothing about the act — so a note an agent drafted, one it transcribed from a scan, and one it imported from another tool's export all read the same line, though they are different provenance"
author: adammharris
created: 2026-09-16
updated: 2026-09-16
status: done
part_of: '[Tasks](/docs/tasks/tasks.md)'
---

# A `generated` pair says who and when, and not what was done

**Status: done (2026-09-16).** Resolved by `feat(provenance): a generated
mapping records the act as how`. The key is `how` — it reads in the line
beside `by` and `at`, a stranger needs no ontology to guess it, and a
confirmation could take it unchanged — glossed as the activity a
`prov:wasGeneratedBy` points at in [Provenance](/docs/provenance.md) §1. prov
ships no terms and the `tasks` preset adds none. A workspace closes it with a
`fields: generated.how:` declaration: a declaration may now name a dotted
path into a mapping, which `check`, the `SetTerm` repair, and a `default:`
all follow — the one mechanism this needed that the task did not name, and
the reason the finding says `generated.how` rather than `generated`.
`about.md`'s byline is unchanged; §7 says why. A view grouped by
`generated.how` would want the same dotted addressing in `views`, and is not
done here.

**Where this starts.** [Provenance](/docs/provenance.md) §1 gives a document
one pair about its origin:

```yaml
generated:
  by: agent:claude-opus-5
  at: 2026-09-11T09:15:22.481093Z
```

Holding that against W3C PROV-O, whose model is *entity, agent, activity*,
shows what the pair lacks. `by` is the agent (`prov:wasAttributedTo`) and
`at` the instant (`prov:generatedAtTime`); the activity — what the agent was
*doing* when the document came to be — has no line. "A model drafted this
from a prompt", "a model transcribed this from a scan", "a program imported
this from another tool's export", and "a person wrote this" are four
different answers to "how did this come to exist", and the pair collapses
them to the actor. A reader with `cat` can tell an agent's note from a
person's, which is what §1 promised, and cannot tell a note the agent
composed from one it merely carried across — which is the distinction that
decides how much of the *content* is the agent's, and so how much a
confirmation is vouching for.

**What is settled.** The activity is a third key on the same mapping, written
once with the pair and never maintained, tier 3 in the sense that prov
carries it and does not reason about it — the same footing as the identifier
after an actor's prefix. It is not a new family, and not a change to
`confirmed`, which stays the record of a reading, not of an act.

**What is open.**

- *The key's name.* `activity` is PROV-O's word and a stranger's best guess;
  something shorter that reads in the line (`generated: {by, at, doing}`)
  may be worth the divergence. Whatever it is, the `means:` gloss names
  `prov:wasGeneratedBy` so an exporter can map it.
- *Open or closed.* A workspace that wants `transcribed` and not
  `transcription` declares the field under `fields:` with a vocabulary, as
  `status` is declared under `Tasks`; prov ships no terms of its own. Whether
  the built-in preset should suggest a handful (`drafted`, `transcribed`,
  `imported`, `converted`) is the same question as whether it should suggest
  any tags, and the answer so far has been no.
- *`about.md`.* Its `generated_by: prov <version>` byline is a `generated` in
  prov's own spelling (§7), and its activity is implicit — regenerated from
  configuration. Whether the byline grows the key, or stays the one place the
  activity is known from the file's kind, is decided with the name.
- *A confirmation's act.* PROV-O would model a confirmation as an activity
  too (`used` the entity, `wasAssociatedWith` the confirmer), and "read it",
  "proofread it", and "checked it against the source" are as different as the
  generating acts are. Out of scope here — a confirmation stays a dated,
  attributed statement — but noted so the key chosen for `generated` is one
  an entry could take later without renaming.

**Done when** `Generated` carries the third field, `read` accepts a pair
without it (every existing document lacks one), `provenance.md` §1 shows it
and says what it is for, and a `fields:` declaration can close its vocabulary
the way `status` is closed — pinned by a test in which an unknown term under
a closed declaration is a `UnknownTerm` finding on the `generated` mapping.
