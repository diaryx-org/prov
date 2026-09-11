---
title: provenance v2 — who vouched for this, and against what
author: adammharris
created: 2026-09-11
updated: 2026-09-11
status: draft
part_of: '[Proposals](/docs/proposals/proposals.md)'
---
# Provenance v2 — who wrote this, who vouched for it, and against what

## Status: draft (2026-09-11)

Second draft. [v1](proposal-provenance-v1.md) took its trust vocabulary from
Open Knowledge Format and left eight questions open; its own status note then
recorded that the fixity rule of 0.11.0 had taken away the answer it leaned on
hardest. This draft settles the three that blocked building anything, and
changes the shape of the family around them:

- **A vouch is bound to the `updated` stamp, not to a fixity digest.** A
  digest exists only where a checksum covers a file other than the one
  recording it, and a combined document — most of any workspace — has none.
  The question a vouch has to survive is *did this change after someone
  confirmed it*, and the `updated` stamp is the record of exactly that. Where a
  digest does exist it is named as well, so the three digest-bearing shapes
  stay byte-checkable.
- **A bare actor is a human; a non-human must carry a prefix.** v1 asked for a
  prefix on every actor, which is a `human:` on every line in a workspace
  where every line is a person's. The burden goes instead to the party that
  can bear it mechanically: tools are code, and code does not forget a prefix
  once told.
- **The verb is `vouch` and the field is `vouched`.** OKF's `verified` reads
  as though something was checked mechanically, which is the confusion §5 of
  v1 existed to prevent. *Vouch* says what the act is: a person staking their
  name on a document being right. It is intentional, and it is honest about
  being an assertion rather than proof.

Path-valued fields — v1's §6, the generalization of `fields` that would make
`sources` a real reference — are not settled here and are not this proposal's
to settle. They fix a live bug on their own and belong in a proposal of their
own; §7 says only what this family needs from them. `status` as a lifecycle
vocabulary has meanwhile been claimed by [presets](/docs/proposals/presets/proposal-presets-v1.md),
whose `tasks` preset ships one, and this draft stops claiming it.

Still open, and listed in §11: whether the `vouched` list is sorted so two
devices appending at once produce the same document, the exact form of
`stale_after`, and whether the family gets a config axis at all. None of them
changes the shape of what is below.

> Complements DESIGN §2 (the three tiers — the test every field below has to
> pass), §8 (validation), `docs/spec.md` §3 (`fields` and controlled
> vocabularies) and §4 (link target kinds), and the fixity and
> single-document-edit rows of DESIGN's status table.

## The design in brief

Two small frontmatter families, one verb, one finding, and one distinction
that keeps them honest.

`generated: {by, at}` records how a document came to exist. `vouched: [{by,
at, of?}]` is an append-only list of dated, attributed confirmations that
someone read the document and found it correct. A vouch is **stale** when the
document's `updated` stamp is newer than the vouch's `at` — someone confirmed
this, and then it changed — and `check` says so with a finding. `prov vouch`
appends an entry; nothing else writes one.

The distinction: **`check` is the attester; `vouched` is the record.** A vouch
is a stored, dated claim about *meaning*, made once by a person or a named
process and preserved. An attestation is a per-run claim about *state*,
computed now and never stored — which is what every `check` finding already
is. Neither substitutes for the other: prov never writes a vouch from a
passing `check`, and a stored vouch never suppresses a finding.

## 1. Lineage

prov has fixity, a crash journal, a deletion log, and some two dozen `check`
findings. All of them answer *what the bytes are and whether they changed*.
None answers *who asserted this, on what authority, and has anyone looked*.
That is a real gap in a crate whose name is the short form of "provenance",
and not one the existing machinery grows into on its own — hashing a file
harder never produces a claim about a person.

OKF v0.2 arrived at the opposite balance: its trust family — `generated`,
`verified`, actor conventions, derived trust tiers, `status`, `stale_after`,
`sources`, and an Attested Computation type — is the centerpiece of the spec,
while link integrity is explicitly refused. The two formats are close enough
in substrate (markdown, YAML frontmatter, no central authority, no special
tooling) that its vocabulary can be read as a design already tested against
the constraints prov works under.

v1 took the vocabulary and rejected the posture. This draft keeps that stance
and diverges further in spelling — `vouched` for `verified`, bare-is-human for
bare-is-agent — for reasons §10 sets out. The material addition is still the
binding in §5: OKF's `verified` floats free of the bytes it was made against,
and prov's does not.

## 2. The problem

Three concrete failures, none of which prov can currently name.

**A document is trusted because it is old.** A note written by a script in
2019 and one hand-checked last week are indistinguishable in a prov
workspace. `updated` says when bytes last moved, which is a different question
and is routinely wrong in both directions — a whitespace fix bumps it, a decade
of correctness does not.

**A review does not survive an edit.** Someone confirms a document is right,
someone else edits it, and the confirmation is still sitting in the
frontmatter describing bytes that no longer exist. A vocabulary that cannot
detect this is worse than none, because it launders staleness as assurance.

**Derivation is invisible.** A document assembled from three others records
nothing about the three. When one is corrected, nothing points from it to the
thing that needs revisiting — and if the sources were written as bare strings,
a `move` does not rewrite them either.

The first two are this proposal's. The third is the path-valued fields
proposal's, and §7 says why.

## 3. `generated` — how the document came to exist

```yaml
generated:
  by: agent:claude-opus-5
  at: 2026-09-11T09:15:22.481093Z
```

Written once, at creation, by whatever created the document, and not
maintained afterward — the pair is a fact about an event, not a mutable field.

By DESIGN §2's test the two halves land in different tiers, which is the whole
reason this is safe to add. `at` is prov-maintained whenever prov is the one
stamping it, so prov owns its format: RFC 3339, `Z`, six fractional digits,
byte-identical to `updated`, supplied by the CLI's clock because the library
is clockless. The actor after any prefix is tier 3 — prov carries
`claude-opus-5` and never reasons about it. The **prefix** is the one part in
between: prov reads it to derive a trust tier (§4), so the prefix set is fixed
mechanism even though everything after the colon is the user's.

Prose prov itself writes carries this from day one. `about.md` is rewritten
whole from configuration on every regeneration (spec §4, *generated prose*),
and each rewrite is a fresh creation, so its `generated` is re-stamped
`{by: process:prov, at: …}` each time. That is consistent with "written once"
because the page is not edited, only replaced.

## 4. Actors, and the tier they imply

An actor is a string. A **bare** actor — `amh`, `adam`, `Adam Harris` — is a
person. A non-human actor carries one of two prefixes:

| Prefix | Means | Example |
| --- | --- | --- |
| *(none)* | a person | `by: amh` |
| `agent:` | a model, acting with judgment | `by: agent:claude-opus-5` |
| `process:` | a program, acting without it | `by: process:prov` |

This inverts OKF, where the bare form is the *agent* form and a consumer
classifies by looking for `human:`. v1 proposed requiring a prefix on all
three and reporting the bare form, which is strictly safest and was rejected
for what it costs: a `human:` on every line of a workspace where every line is
a person's, forever, to guard against a tool that forgets its prefix once.
Tools are code. The prefix rule is a rule for whoever writes one, and prov's
own stamps always carry it.

What this accepts: a tool that omits its prefix is counted as a person. That
is the direction OKF guards against, and it is a bug in one tool that gets
fixed once, against boilerplate in every document that never goes away.

**Trust tiers are derived, never stored.** No `trust:` field exists; a reader
computes the tier from what is present, and only from entries that are not
stale (§5):

| Tier | Condition |
| --- | --- |
| unvouched | no live `vouched` entry |
| machine-vouched | live entries, every actor prefixed |
| human-vouched | at least one live entry with a bare actor |

Storing a tier would be storing a conclusion, and a conclusion goes stale the
moment an entry is appended or the document edited. Same rule as DESIGN §5's
derived-vs-authoritative split: the entries are authoritative, the tier is
disposable.

**Where a person's name comes from.** `prov vouch` needs an actor, and who is
at the keyboard is a fact about a device, not about the archive — the
reasoning that keeps the peer table out of `prov.yaml`. So it is a flag with a
device-local default: `--by` > `PROV_ACTOR` > a device-local config file, the
precedence the peer map already uses. It is not a workspace config key. A
vouch with no actor from any of those is refused rather than written
anonymously, since an unattributed vouch is the free-floating assertion this
family exists to replace.

**An actors vocabulary is opt-in, and is not the mechanism.** A workspace that
wants typo protection, or wants each actor to be a node with prose and
backlinks, declares the actor field as a `fields` entry pointing at a
vocabulary in the ordinary way, and gets `UnknownTerm`, `TermNearMiss`, and
`reify` for nothing new. The prefix stays the grammar and the vocabulary
validates membership in it — a term is `amh` or `agent:claude-opus-5`, and
prov reads no `kind` key off a term. Tier derivation does not depend on the
vocabulary being declared, because a tier that exists only when config is
present is a tier most workspaces would not have. The one thing this needs
that `fields` cannot yet say is a path into a list — `vouched[].by` — which is
the same extension path-valued fields need for `sources[].resource`, and is
that proposal's to add.

## 5. Vouching vs. attesting — the load-bearing distinction

prov has been drawing this line informally since fixity shipped. The stamped
`content_hash` is a stored, dated baseline; a `check` run compares the bytes
to it and reports, and the comparison is thrown away. OKF names the halves:

- **Verification** confirms the *definition* — that the document says what it
  should. Document-level, stored in the workspace, attributed, dated.
  Survives copying, syncing, and being read by a tool that has never run
  `check`. Here, a *vouch*.
- **Attestation** confirms a *single run* — that some state held just now.
  Computed at read time, never stored, meaningless once the run ends.

In prov the mapping is immediate: **`check` is the attester.** Every finding
it produces is an attestation — the bytes hash to the stamped digest, the
inbound links resolve, this closed field's values are all known terms.
Findings are not stored, are recomputed every run, and describe an instant.

Three rules follow.

**prov never writes a vouch from a passing `check`.** A green check attests
bytes and structure. It says nothing about whether the content is *right*, and
auto-stamping would launder a mechanical pass into a human's claim — the exact
fraud the tiers exist to prevent. A vouch is written when a person or a named
process asks for one, never on prov's initiative. Same shape as `prov id
--workspace`: minted on request, never unbidden.

**A stored vouch never suppresses a finding.** You cannot vouch your way out
of a fixity mismatch. A vouch is about meaning, a finding is about state, and
neither is evidence for the other. Concretely: no finding kind ever consults
`vouched` when deciding whether to fire.

**A vouch is scoped to the document as it stood.** This is the addition, and
the reason this proposal exists rather than a note saying "adopt OKF".

```yaml
updated: 2026-09-11T09:12:40.006113Z
vouched:
- by: amh
  at: 2026-09-11T09:20:00.000000Z
```

A vouch is **stale** when the document's `updated` instant is newer than the
vouch's `at`. Both are the same clock in the same fixed-width format, so the
comparison is a string comparison a reader makes by eye, and the rule is
statable in one sentence with no hashing in it: *confirmed at 09:20, changed
after that, or not*. Equal instants are not stale. A document whose `updated`
field is empty or absent has nothing saying it changed, and its vouches
stand.

v1 bound to the fixity digest instead, and its own status note records why
that stopped being possible: since 0.11.0 a `content_hash` is written exactly
where it covers *a file other than the one recording it* — an attachment
sidecar's payload, a separated node's prose body, a manifest node's manifest —
and a combined document gets none. The alternative of hashing the whole file
fails on its own terms, because appending the vouch changes the file, so the
hash could only describe a file nobody can reproduce afterward.

Where a digest does exist, the vouch names it too:

```yaml
content: photo.jpg
content_hash: sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
vouched:
- by: amh
  at: 2026-09-11T09:20:00.000000Z
  of: sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
```

`of` is written when and only when the document carries a `content_hash`, and
is the digest on record at the moment of vouching. A vouch is then also stale
when `of` does not match the current `content_hash`. For the three shapes that
have one, the guarantee is about bytes and an outsider checks it with
`sha256sum`; for everything else it is about the stamp, and an outsider checks
it by reading two dates. Both pass the test the fixity rule was decided by.

What binding to `updated` accepts, stated plainly: an edit made outside prov —
another editor, a sync merge — that never bumps the stamp is invisible to this
rule. That is the hole `updated` already has and the workspace has chosen to
live with; a vouch inherits it rather than adding to it. A workspace with the
`updated` axis off gets staleness on the digest-bearing shapes only, which is
fair, since it has already said it does not track edits.

**`prov vouch <doc>` never stamps `updated`.** Appending a vouch is not an
edit to the content, and a verb that bumped the stamp it is compared against
would make every vouch fresh by construction.

The finding:

> **`VouchStale`** — a `vouched` entry's `at` is older than the document's
> `updated`, or its `of` does not match the current `content_hash`. Someone
> confirmed this, and then it changed.

Not an error: an edit after a review is the ordinary course of events. It is a
*demotion* — the document falls back to whatever tier its remaining live
entries support, usually unvouched — and the finding is how a reader learns
that the assurance in the frontmatter describes a document that no longer
exists. The remedies are the two real ones: vouch again, or leave it and
accept the lower tier. There is no autofix, for the reason the orphan finding
has none — the repair is a judgment.

Entries are never rewritten or dropped by prov. A stale entry is history and
stays; the tier calculation just stops counting it.

## 6. The verb

```
prov vouch <doc> [--by <actor>]
```

Appends one entry to `vouched`, with `at` from the CLI's clock and `by`
resolved as §4 says, and `of` when the document carries a digest. Refuses a
machinery store (§9) and a document it cannot parse. A second vouch by the
same actor on an unchanged document is appended, not deduplicated — it is a
second confirmation, and the list is the record of confirmations.

Why not `verify`: fixity owns the word — `prov manifest --verify` re-reads an
archive against its hashes — and a user who has learned that meaning would
expect `prov verify` to check something rather than assert it. Why not
`attest`: by §5's own distinction attesting is what `check` does. Why not
`certify`: a certificate is a thing one presents as proof, and §8 is explicit
that a vouch is not one. *Vouch* is what a person does when they put their
name to a claim they cannot prove, which is exactly the act.

## 7. `sources`, deferred to path-valued fields

v1's §6 argued that a `sources[].resource` string that is really a path ought
to get everything spec §4 gives reference targets — rewritten on move,
locators preserved, dangling reported — through a generalization of `fields`
from term-valued to path-valued. That argument holds, is independent of
everything here, and is the one piece that fixes a live bug rather than adding
a capability. It should be its own proposal, and possibly built first.

This family needs two things from it, and both are named here so that
proposal can count them as consumers: a field path into a list
(`sources[].resource`, `vouched[].by`), and the rule that a declared
path-valued field is a target and not a string. Nothing else in this document
depends on `sources`.

## 8. Non-goals

- **Signatures and crypto.** `vouched: [{by: amh}]` is an assertion, not
  proof, and this proposal does not make it one. Key management inside a
  plaintext archive is a harder problem with a worse failure mode — an archive
  whose signatures no longer verify because a key rotated is worse than one
  that never claimed to. prov's fixity is unsigned by deliberate choice and
  this family follows it. The verb's name is chosen to say so.
- **Access control.** A lifecycle marker is not a permission. Audience gating
  already exists as a `fields` vocabulary and is a different axis.
- **Workflow.** No review queues, no assignment, no approval states beyond
  what a dated list of confirmations naturally expresses.
- **Deriving provenance from git.** Tempting, and wrong for the reason the
  peer table stays out of `prov.yaml`: git history is a fact about one clone,
  not about the archive, and a workspace synced over Dropbox has none.
- **Lifecycle vocabulary.** `status` is a closed `fields` vocabulary and needs
  nothing from this proposal; the presets proposal ships one. `stale_after`
  is the one genuinely new evaluation there, and waits (§11).
- **Attested Computation.** OKF's type — `runtime`, `parameters`, `executor`,
  receipts — is not prov's business. A plaintext archive executes nothing.
  The one note is that the *shape* is familiar: a standalone document whose
  type declares a contract, linked from consumers rather than nested inside
  them, carrying its own trust state. `prov-views` is close to that, and a
  vouch applies to a view document exactly as to any other. That is the whole
  overlap.

## 9. Which documents may carry this

Any content document — a note, an attachment sidecar, a separated node, a
manifest node, a reified vocabulary term. A sidecar is the case that motivated
`of`: a captured export's provenance belongs on the record that pins it.

Not machinery. A flat vocabulary, the registry, the deletion log, and a
manifest store are whole-file record stores prov re-lays-out, and a vouch on
one would be a claim about a file prov rewrites. `prov vouch` refuses them.
`about.md` is generated prose, rewritten whole; it carries `generated` (§3)
and cannot be vouched, since prov would discard the vouch on the next
regeneration.

## 10. Interop with OKF

Stated plainly, since the vocabulary is borrowed: this proposal does not aim
at OKF conformance, and now diverges from it in spelling as well as posture.
The field is `vouched`, not `verified`; a bare actor is a person, not an
agent; `of` is not in OKF at all. An OKF consumer reading a prov workspace
would see neither family, and a prov reader given an OKF bundle would carry
`verified` as tier-3 frontmatter and derive no tier from it. That is the
honest state: two vocabularies that share an ancestor, rather than one
workspace claiming a conformance it does not have.

The reciprocal direction remains **OKF as an export target**. `prov-exports`
already does gated egress sets; an OKF gate would emit `index.md` spines from
the spanning tree and translate this family into OKF's spelling on the way
out — `vouched` to `verified`, a bare actor to `human:`, `of` dropped.
Separate proposal, and the one that would actually deliver interop.

## 11. Still open

Smaller than v1's list, and none of them changes the shape above.

1. **Concurrent appends to `vouched`.** Two devices each append an entry and
   the sync transport sees a conflicting edit to one line. Defining the list
   as sorted by `at` would make both orders the same document, the way record
   stores are re-laid-out sorted — but frontmatter is not a record store, and
   this would be the first user-visible list prov imposes an order on.
   Recommendation: do not sort; let the clash be an ordinary sync conflict,
   which is what it is.
2. **`stale_after`: absolute or relative?** An absolute date needs no
   arithmetic and rots; a relative span measured from the newest live vouch
   tracks the review rather than the calendar and is not legible without
   evaluating it. Waits for a workspace that wants it, and for the answer to
   be found by using it.
3. **A config axis.** v1 proposed `provenance: on|off`. With `vouched` written
   only on request and `generated` written only by tools that choose to, there
   may be nothing to switch: a workspace that never runs `prov vouch` has no
   entries and no findings. The axis is deferred until something needs it.

## 12. Phasing

**Phase 0 — carry only.** Document the convention; write nothing, read
nothing. Every field here is legal tier-3 frontmatter prov transports
untouched, so a workspace can use the vocabulary today and find out whether
it wants it. Costs one docs page and zero code, and is where this draft
stands.

**Phase 1 — `generated`, `vouched`, `prov vouch`, `VouchStale`.** The core:
the actor grammar of §4, the staleness rule of §5, the verb of §6, and the
derived tier. Depends on nothing outside this document. `about.md` gains its
`generated` stamp in the same release.

**Phase 2 — an actors vocabulary.** Needs a field path into a list, which is
path-valued fields' to add; lands when that does.

**Separately** — path-valued fields, as their own proposal (§7).

**Deferred** — `stale_after`, a config axis, sources-as-relation with
inverses, OKF export.
