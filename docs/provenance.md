---
part_of: '[prov](/README.md)'
---
# Provenance — who wrote this, and who has confirmed it since

> The keeper's own ledger. How a document says how it came to exist, and how
> the person keeping the workspace records that they read it and found it
> right — precisely enough that a reader with `cat` can tell a note an agent
> wrote last night from one a person checked, and can tell when that check
> stopped describing the document in front of them. Complements DESIGN §2 (the
> three tiers), spec §3 (`fields`), and the fixity row of DESIGN's status table.

## 1. Two families

```yaml
generated:
  by: agent:claude-opus-5
  at: 2026-09-11T09:15:22.481093Z
confirmed:
- by: amh
  at: 2026-09-11T09:20:00.000000Z
```

**`generated`** records how the document came to exist. It is written once, by
whatever created the document, and never maintained afterward — the pair is a
fact about an event. prov reads it only to say what kind of actor wrote the
document, and never writes it into a document of yours.

**`confirmed`** is an append-only list of dated, attributed statements that
someone read the document and found it correct. `prov confirm <doc>` appends
one. Nothing else writes the list, and nothing ever rewrites or drops an entry:
a second confirmation by the same person is a second fact, and an entry the
document has since moved out from under is history.

Both are ordinary frontmatter. A workspace that never runs `confirm` has no
entries, no findings, and nothing to configure — there is no axis for this.

## 2. Actors

A bare actor is a person. A non-human carries one of two prefixes:

| Written | Means |
| --- | --- |
| `amh`, `Adam Harris` | a person |
| `agent:claude-opus-5` | a model, acting with judgment |
| `process:nightly-build` | a program, acting without it |

The burden sits with the party that can bear it mechanically. A workspace
full of one person's own work writes no boilerplate, and a tool is code that
does not forget a prefix once told. What that accepts is that a tool which
omits its prefix is counted as a person — a bug in one tool, fixed once,
against a `human:` on every line forever.

**Where a person's name comes from.** Who is at the keyboard is a fact about a
device, not about the archive — the same reasoning that keeps the peer map out
of `prov.yaml`. `prov confirm` takes `--by <ACTOR>`, then `PROV_ACTOR`, then a
one-line `actor` file beside the device-local peer map (`prov peer list` prints
where that is). With none of those it refuses rather than writing an
unattributed entry, because an unattributed assurance is exactly what this
family exists to replace.

## 3. What a confirmation is bound to

A confirmation is **stale** when the document changed after it was made. The
record of a change is the workspace's own `updated` stamp (`updated:
modified` in config, the field `edit`, `set`, and `stamp` write): an entry
whose `at` is older than the document's `updated` instant describes a document
that no longer exists. Both are the same clock in the same fixed-width RFC 3339
spelling, so the comparison is one a reader makes by eye — *confirmed at
09:20, changed at 10:00, or not*.

Where the document records a `content_hash` — an attachment sidecar, a
separated node, a manifest node — the entry also names the digest on record:

```yaml
content: photo.jpg
content_hash: sha256:9f86d0…
confirmed:
- by: amh
  at: 2026-09-11T09:20:00.000000Z
  of: sha256:9f86d0…
```

and is stale too once that digest moves. For those three shapes the guarantee
is about bytes, and an outsider checks it with `sha256sum`; for everything else
it is about the stamp, and an outsider checks it by reading two dates.

**What this accepts, stated plainly.** An edit made outside prov that never
bumps the stamp is invisible here. That is the hole `updated` already has, and
a confirmation inherits it rather than adding to it. A workspace that keeps no
`updated` field gets staleness on the digest-bearing shapes only, which is
fair: it has already said it does not track edits.

**`prov confirm` never stamps `updated`.** Appending an entry is bookkeeping
about the document, not an edit of it, and a verb that bumped the stamp it is
measured against would make every confirmation fresh by construction.

**A drifted document is refused.** If the checksum on record no longer matches
the bytes, that is already a fixity mismatch, and a confirmation cannot vouch
its way past one. `prov stamp <doc>` restates the checksum; then confirm what
is actually there.

## 4. The tier, derived and never stored

| Tier | Condition |
| --- | --- |
| unconfirmed | no entry still stands |
| machine-confirmed | entries stand, every actor prefixed |
| human-confirmed | at least one standing entry by a person |

An entry *stands* when the document has not changed since it. Storing the
tier would be storing a conclusion, and a conclusion goes stale the moment an
entry is appended or the document edited — so it is computed from what is
present, the way trust is derived everywhere else in prov, and `prov confirm
<doc> --show` prints it along with each entry and whether it stands.

## 5. What `check` says

**`confirmation_stale`** — the document's newest confirmation was made against
a version that no longer exists, and nothing has confirmed it since. Someone
confirmed this, and then it changed.

Not an error: an edit after a review is the ordinary course of events. It is a
demotion, and the finding is how a reader learns that the assurance in the
frontmatter is describing a document that is not there any more. The remedies
are the two real ones — confirm again, or leave it and accept being
unconfirmed — and there is no autofix, for the reason the orphan finding has
none: the repair is a judgment. A document confirmed again after the edit
keeps its older entries as history and is not reported for them.

## 6. The distinction that keeps this honest

**`check` is the attester; `confirmed` is the record.** Every finding `check`
produces is a claim about *state* — the bytes hash to the digest, the links
resolve, the closed field's values are known terms — computed now and thrown
away. A confirmation is a stored claim about *meaning*, made once by a person
or a named process and kept. Three rules follow:

- **prov never writes a confirmation from a passing `check`.** A green check
  says nothing about whether the content is right, and auto-stamping would
  launder a mechanical pass into a person's word. A confirmation is written
  when someone asks for one, never on prov's initiative.
- **A stored confirmation never suppresses a finding.** No finding kind
  consults `confirmed` when deciding whether to fire. You cannot confirm your
  way out of a fixity mismatch.
- **A confirmation is a claim, not proof.** It is a line in the document,
  written by whoever can edit the document, and exactly as trustworthy as the
  `author` field beside it. A signed record that the document's keeper cannot
  edit — a reviewer vouching for a revision, in their own history — is a
  different tool's subject, and the verb here is named so as not to be mistaken
  for it.

## 7. Which documents

Any content document — a note, an attachment sidecar, a separated node, a
manifest node, a reified vocabulary term. Not machinery: a registry, a
deletion log, or a flat vocabulary is a whole-file store prov re-lays-out, and
a confirmation on one would be a claim about a file prov itself rewrites.
`about.md` is likewise refused — it is rewritten whole from configuration, and
its `generated_by` byline is its `generated` in prov's own spelling.
