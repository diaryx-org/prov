---
title: provenance and attestation
author: adammharris
created: 2026-08-14
updated: 2026-09-02
status: draft
part_of: '[`prov` proposals](/docs/proposals/proposals.md)'
---
# Provenance — who wrote this, who checked it, and against what

## Status: still a draft (2026-09-02)

Open on purpose. §2's questions are unanswered, and phase 0 is still "carry
only" — every field here is legal tier-3 frontmatter that prov transports
untouched, so a workspace can try the vocabulary today without prov gaining a
line of code for it. Two things have moved underneath the draft since it was
written, and the first changes an answer.

**§5's digest binding is no longer nearly free, and question #2 is harder for
it.** That section reasons "because prov already stamps `sha256:`, binding is
nearly free." It was true when a checksum could cover a document's own body. It
is not now: the fixity rule of 0.11.0 (`cb0054e`, DESIGN row 464) reads
coverage off the document's *shape* and writes a `content_hash` exactly where
it covers **a file other than the one recording it** — an attachment sidecar's
payload, a separated node's prose body, a manifest node's manifest. A combined
document gets none. So `of: sha256:…` can bind on those three, and an ordinary
single-file note has no digest to name and nothing for `VerificationStale` to
fire against.

The idea survives the change — the test that decided the fixity rule is the one
this section is built on, that an outsider can check the claim with `sha256sum`
— but the questions come out differently. #2 stops being about OKF
compatibility and becomes the first question to settle, because most documents
in a workspace are combined ones and unbound-therefore-uncheckable would be
their normal state, not an interop edge. #7 is partly pre-answered: the
documents that can carry a *bound* verification are exactly the ones prov
already gives a `content_hash`. Whether an unbound `verified` is worth
recording on the rest, or whether the family should bind to something that is
not a fixity digest, is what a second draft has to decide before anything else
here can be built.

**Three citations name machinery that is gone.** §1's lineage opens "prov has
fixity, an immutable history store, a crash journal, a recycle bin": the store
was retired in 0.7.0 ([history v3's
status](/docs/proposals/history/proposal-history-v3.md)) and the bin in 0.11.0,
where a deletion log stands in its place. §3's worked example of a document
prov itself authors — a history event carrying `generated: {by:
process:prov-history, …}` — is therefore about a document nothing writes; the
point it makes holds for the pages prov does generate, `about.md` first. §10's
OKF export gate would draw `log.md` from a store that no longer exists. And §5
quotes a fixity-cache rule from DESIGN's status table that is not there any
more: the cache retired with the capture verbs that were its only consumer.

The distinction that quote illustrated is untouched, and is still the strongest
thing in this document: `check` is the attester, `verified` is the record, and
neither is evidence for the other.

> **Early draft.** Deliberately unfinished: the open questions in §2 are not
> yet answered, and several of them change the shape of everything after
> them. Nothing here is settled enough to build. Complements DESIGN §2 (the
> three tiers — the test every field below has to pass), §8 (validation),
> `docs/spec.md` §3 (`fields` and controlled vocabularies) and §4 (link
> target kinds), and the fixity row of DESIGN's status table.
>
> Prompted by a read of [Open Knowledge Format
> v0.2](https://github.com/GoogleCloudPlatform/knowledge-catalog/blob/main/okf/SPEC.md),
> whose trust vocabulary is the one thing it has that prov has no answer for
> at all.

## The design in brief

Four small frontmatter families, and one distinction that keeps them honest.

`generated: {by, at}` records how a document came to exist. `verified: [{by,
at, of}]` is an append-only list of dated, attributed confirmations that
someone looked at it and found it correct — each **bound to the fixity digest
it was made against**, so a verification cannot silently outlive the bytes it
was about. `status` and `stale_after` say where the document sits in its
lifecycle. `sources` records what it was derived from, with the path-valued
entries resolved as real reference targets rather than carried strings.

The distinction: **`check` is the attester; `verified` is the verification
record.** A verification is a stored, dated, human-or-process claim about
*meaning*, made once and preserved. An attestation is a per-run claim about
*state*, computed now and never stored — which is precisely what a `check`
finding already is. Neither substitutes for the other, so prov never writes
`verified` from a passing `check`, and a stored `verified` never suppresses a
finding.

Everything below is the argument for those sentences, and the list of things
that have to be decided before any of it can be built.

## Lineage

prov has fixity, an immutable history store, a crash journal, a recycle bin,
and about twenty `check` findings. All of them answer *what the bytes are and
whether they changed*. None of them answers *who asserted this, on what
authority, and has anyone checked*. That is a real gap in a crate whose name
is the short form of "provenance," and it is not one the existing machinery
grows into on its own — hashing a file harder never produces a claim about a
person.

OKF v0.2 arrived at the opposite balance: its trust family (`generated`,
`verified`, actor conventions, trust tiers, `status`, `stale_after`,
`sources`, and an Attested Computation type) is the centerpiece of the spec,
while link integrity is explicitly refused — "consumers MUST NOT reject
bundles for broken cross-links." The two formats are close enough in
substrate (markdown, YAML frontmatter, no central authority, no special
tooling) that its vocabulary can be read as a design already tested against
the same constraints prov works under.

This proposal takes the vocabulary and rejects the posture. The material
change is §5: because prov stamps `sha256:` and OKF does not, prov can bind a
verification to the bytes it was made against, which turns `verified` from a
free-floating assertion into a checkable one. That is the argument for prov
doing this at all rather than declaring OKF compatibility and stopping.

## The problem

Three concrete failures, none of which prov can currently name.

**A document is trusted because it is old.** A note written by a script in
2019 and one hand-checked last week are indistinguishable in a prov
workspace. `updated` says when bytes last moved, which is not the same
question and is routinely wrong in both directions — a whitespace fix bumps
it; a decade of correctness does not.

**A review does not survive an edit.** The failure mode any review vocabulary
has to survive: someone confirms a document is right, someone else edits it,
and the confirmation is still sitting there in the frontmatter, now
describing bytes that no longer exist. A vocabulary that cannot detect this
is worse than none, because it launders staleness as assurance.

**Derivation is invisible.** A document assembled from three others records
nothing about the three. When one of them is corrected, nothing points from
it to the thing that needs revisiting — and if the sources were written as
bare strings, a `move` does not rewrite them either, so the record rots
exactly where the workspace was supposed to be maintaining it.

## 2. Open questions — settle these before building

Listed first, because at least three of them change the shape of the rest.

1. **Do `sources` create edges?** If a source names a document in this
   workspace, is that a reference prov resolves, rewrites on move, and
   reports dangling — or a carried string? Recommendation in §6: an edge, via
   a generalization of `fields` to path-valued fields. This is the largest
   mechanism question here and the one most likely to be worth doing on its
   own merits even if the rest is dropped.

2. **Is `verified` bound to a digest?** §5 recommends yes, and it is the
   strongest idea in this document — but `of: sha256:…` is not in OKF, so a
   prov `verified` entry is a superset an OKF consumer would ignore, and an
   OKF `verified` entry read by prov has no digest to check against. Is
   unbound-therefore-unverifiable an acceptable state to represent, or does
   prov refuse to record a verification it cannot later invalidate?

3. **Actor grammar.** OKF spells actors three ways — `human:<id>`,
   `process:<id>`, and bare `<producer>/<version>` for agents — and asks
   consumers to classify by detecting the `human:` prefix. The bare form
   means "unprefixed" and "agent" are the same string, so a typo in a prefix
   is silently an agent. Should prov require a prefix on all three
   (`human:` / `process:` / `agent:`) and make the unprefixed form a finding?
   That is cheap and strictly safer, and it is a deliberate incompatibility.

4. **`stale_after`: absolute or relative?** OKF uses an absolute
   `YYYY-MM-DD`, which needs no arithmetic but rots — every document reaches
   its date and stays there. A relative form (`stale_after: 90d`, measured
   from the newest `verified[].at`, or `generated.at` when unverified) tracks
   the review rather than the calendar, but is no longer legible without
   evaluating it. Supporting both is easy to implement and doubles the
   surface a reader has to understand. Which?

5. **How much of this is configurable?** DESIGN §2 says field *names* are
   tier 2 and therefore renameable, as `updated` is. Six renameable names is
   a lot of new config surface for a family nobody has asked for yet.
   Recommendation: one axis, `provenance: on|off`, names fixed for now, and
   revisit renaming when a real workspace needs it — under the same reasoning
   that kept the peer table out of `prov.yaml`.

6. **Concurrent appends to `verified`.** Two devices each append an entry to
   the same YAML list and the sync transport sees a conflicting edit to one
   line. Is the list defined as sorted by `at` so that both orders are the
   same document, the way record stores are re-laid-out sorted? Frontmatter
   is not a record store, so this would be the first place prov imposes an
   order on a user-visible list.

7. **Which documents may carry this?** An attachment sidecar carrying
   `verified` is sensible — that is where a captured export's provenance
   belongs. A flat vocabulary file is a whole-file record store prov
   re-lays-out, and probably may not. Machinery generally?

8. **Verb naming.** `prov verify` reads like a fixity operation and fixity
   already owns that verb in every user's head. Something else, or a
   subcommand?

## 3. `generated` — how the document came to exist

```yaml
generated:
  by: agent:claude-opus-5
  at: 2026-08-14T09:15:22.481093Z
```

Written once, at creation, by whatever created the document. Not maintained
afterward — the pair is a fact about an event, not a mutable field, and
nothing rewrites it.

By DESIGN §2's test the two halves land in different tiers, which is worth
stating because it is the whole reason this is safe to add. `at` is
prov-maintained whenever prov is the one stamping it, so **prov owns its
format**: RFC 3339, `Z`, six fractional digits, byte-identical to `updated`.
The actor id after the prefix is tier 3 — prov carries `claude-opus-5` and
never reasons about it. The **prefix** is the only part in between: prov must
read it to derive trust tiers (§4), so the prefix set is fixed mechanism even
though everything after the colon is the user's.

Documents prov itself authors should carry this from day one — a history
event document is `generated: {by: process:prov-history, at: …}`, which costs
nothing and makes the store self-describing in the same terms as the content
around it.

## 4. Trust tiers are derived, never stored

Straight from OKF, and correct: no `trust:` field exists. A consumer computes
the tier from what is present.

| Tier | Condition |
| --- | --- |
| unverified | no `verified` key |
| machine-confirmed | `verified` entries, none by a `human:` actor |
| human-reviewed | at least one `verified` entry by a `human:` actor |

Storing a tier would be storing a conclusion, and a conclusion goes stale the
moment a new entry is appended. This is the same rule as DESIGN §5's derived
vs authoritative split: the entries are authoritative, the tier is derived,
and derived state is disposable.

## 5. Verification vs. attestation — the load-bearing distinction

OKF draws a line prov has been drawing informally for a while without naming
it. From the fixity cache rule in DESIGN's status table:

> a remembered digest may decide what to do and may never establish or verify
> a fixity baseline

That is exactly this distinction. The cached hash is an **attestation** — a
claim about an instant, cheap, disposable, never authoritative. The stamped
`sha256:` is a **verification baseline** — stored, dated, authoritative, and
the thing an attestation is checked *against*. OKF generalizes the pair:

- **Verification** confirms the *definition* — that the document says what it
  should. Doc-level, stored in the workspace, attributed, dated. Survives
  copying, syncing, and being read by a tool that has never run `check`.
- **Attestation** confirms a *single run* — that some state held just now.
  Computed at read time, never stored, meaningless once the run ends.

In prov the mapping is immediate and slightly surprising: **`check` is the
attester.** Every finding it produces is an attestation — the bytes hash to
the stamped digest, the inbound links resolve, this closed field's values are
all known terms. Findings are not stored, are recomputed every run, and
describe an instant. That is an attester by OKF's definition, already built.

Three rules follow, and they are the substance of this section.

**prov never writes `verified` from a passing `check`.** A green check
attests bytes and structure. It says nothing about whether the content is
*right*, and auto-stamping would launder a mechanical pass into a human
review claim — the exact fraud the trust tiers exist to prevent. A
verification is written when a person or a named process asks for one, never
on prov's initiative. Same shape as `prov id --workspace`: minted on request,
never unbidden.

**A stored `verified` never suppresses a finding.** You cannot verify your
way out of a fixity mismatch. Verification is about meaning, attestation is
about state, and neither is evidence for the other. Concretely: no finding
kind ever consults `verified` when deciding whether to fire.

**A verification is scoped to the bytes it was made against.** This is the
addition, and the reason this proposal exists rather than a note saying
"adopt OKF."

```yaml
verified:
- by: human:amh
  at: 2026-08-14T09:20:00.000000Z
  of: sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
```

`of` is the document's fixity digest at the moment of verification. A
verification that does not name what it verified is not checkable, and an
uncheckable assurance decays into a false one — §3's second failure. Because
prov already stamps `sha256:`, binding is nearly free, and it buys a new
finding:

> **`VerificationStale`** — the newest `verified[].of` does not match the
> document's current fixity digest. Someone confirmed this, and then it
> changed.

Not an error: an edit after a review is the ordinary course of events. It is
a *demotion* — the document falls back to whatever tier its remaining valid
entries support, usually unverified — and the finding is how a reader learns
that the assurance in the frontmatter is describing bytes that no longer
exist. The available remedies are the two real ones: re-verify at the current
digest, or leave it and accept the lower tier. There is no autofix, for the
same reason the orphan finding has none — the repair is a judgment.

Entries are never rewritten or dropped by prov. A superseded entry is history
and stays; the tier calculation just stops counting it.

## 6. `sources`, and the generalization worth having

The narrow version: a `sources` list recording what a document was derived
from, with `resource` required and everything else (`title`, `author`,
`usage_count`, `last_modified`) carried and unread.

The broad version is more interesting, and it is a prov-shaped move rather
than an OKF import. `docs/spec.md` §3 already establishes that a frontmatter
field prov merely *carries* becomes a resolvable, checked reference the
moment a `fields` entry points it at a vocabulary. That is the term-valued
case. `sources[].resource` is the same idea one type over: a string that is
really a *path*, and that therefore ought to get everything §4 already
specifies for reference targets — external URLs recognized by syntax and
never resolved, in-workspace paths rewritten on move, locators preserved,
dangling references reported.

So: extend `fields` from term-valued to **path-valued** fields.

```yaml
prov:
  fields:
    sources[].resource:
      values: path        # alongside open | closed
```

A field declared this way stops being a carried string and starts being a
target — the existing rewrite-on-move, locator, and dangling-reference
machinery applies unchanged, because it is keyed on target kind and not on
which field the target was found in.

This is worth its own proposal, and possibly worth building *before* anything
else here. It is the one piece with obvious value independent of the trust
vocabulary — every workspace has a field somewhere holding a path prov does
not know is a path, and today every one of them silently rots on the first
`move`. It also answers open question #1 in a way that adds no new concept:
path-valued fields are not a new link target kind, just the existing kinds
reached through a field prov was told about.

Open, and not answered here: whether `sources` entries participate in the
overlay graph as *edges with an inverse* — that is, whether a source document
gets a backlink listing what was derived from it. Useful, and a much larger
commitment, since it makes `sources` a relation rather than a field.

## 7. `status` and `stale_after` need no new mechanism

`status` is a closed controlled vocabulary over `draft | stable | deprecated`,
and prov already has closed controlled vocabularies:

```yaml
prov:
  fields:
    status:
      values: closed
      vocabulary: '[Statuses](/vocab/statuses.yaml)'
```

`UnknownTerm` and `TermNearMiss` come along for free
(`prov/src/validate.rs`), as does `retired:` for a status a workspace stops
using, and as does the per-term `means` gloss. A workspace that wants
`archived` or `superseded` widens its own vocabulary; nothing in prov needs
to know the term set in advance. This is the `fields` mechanism doing exactly
what it was built for, and it is the cheapest thing in this proposal.

`stale_after` is the one genuinely new evaluation — a date compared against
today, producing a finding when passed. Absolute vs. relative is open
question #4. Either way the finding is informational, and either way this
should probably wait: a staleness rule that fires on every document in a
workspace that has never used the field is a fast way to teach users to
ignore findings.

## 8. Attested Computation: declined, with one note

OKF's Attested Computation type — `runtime`, `parameters`, `executor`,
`attester`, receipts, a consumer workflow that parameterizes and executes a
query against BigQuery or dbt — is not prov's business. A plaintext archive
does not execute anything, and adopting the contract would mean adopting a
runtime seam, an execution surface, and a receipt format for a capability
prov has no reason to have.

The note is that the *shape* is familiar: a standalone document whose type
declares a contract, linked from consumers rather than nested inside them,
carrying its own independent trust state. `prov-views` is already close to
that — declarative documents describing a computation over the workspace,
referenced from elsewhere. If views ever want a trust story, this is the
prior art, and `verified` applies to a view document exactly as it applies to
any other. That is the whole overlap; the executor machinery stays out.

## 9. Non-goals

- **Signatures and crypto.** `verified: {by: human:amh}` is an assertion, not
  proof, and this proposal does not make it one. Key management inside a
  plaintext archive is a substantially harder problem with a worse failure
  mode — an archive whose signatures no longer verify because a key rotated
  is worse than one that never claimed to. prov's fixity is unsigned by
  deliberate choice and this family follows it.
- **Access control.** `status: draft` is a lifecycle marker, not a
  permission. Audience gating already exists as a `fields` vocabulary and is
  a different axis.
- **Workflow.** No review queues, no assignment, no approval states beyond
  what a dated list of confirmations naturally expresses.
- **Deriving provenance from git.** Tempting, and wrong for the same reason
  the peer table stays out of `prov.yaml`: git history is a fact about one
  clone, not about the archive, and a workspace synced over Dropbox has none.

## 10. Interop with OKF

Worth stating plainly, since the vocabulary is borrowed: this proposal does
**not** aim at OKF conformance, and adopting the field names does not make a
prov workspace an OKF bundle. The formats disagree on structure (frontmatter
edges vs. directory tree plus `index.md`), on identity (minted ids and a
registry vs. none), and on posture (findings vs. "MUST NOT reject"). Those
are not reconcilable by sharing five field names, and pretending otherwise
would be the worse outcome — a workspace that claims a conformance it does
not have.

The honest reciprocal direction is **OKF as an export target**.
`prov-exports` already does gated egress sets; an OKF gate would emit
`index.md` spines from the spanning tree, `log.md` from the history store,
and this family verbatim into the frontmatter it came from. Separate
proposal, and the one that would actually deliver interop.

## 11. Phasing

**Phase 0 — carry only.** Document the convention; write nothing, read
nothing. Every field here is already legal tier-3 frontmatter that prov
transports untouched, so a workspace can start using the vocabulary today and
find out whether it wants it. Costs one docs page and zero code. This phase
is the point of keeping the proposal at draft stage.

**Phase 1 — path-valued fields (§6).** Independently valuable, no dependency
on anything else here, and the only part that fixes a live bug rather than
adding a capability. Plausibly a proposal of its own.

**Phase 2 — `generated` and `verified` with digest binding.** The core:
stamping, the actor prefix grammar, derived trust tiers, and
`VerificationStale`. Depends on questions #2, #3, #6 and #8 being answered.

**Phase 3 — lifecycle.** `status` as a `fields` vocabulary is nearly free.
`stale_after` waits for question #4 and for evidence anyone wants it.

**Deferred indefinitely** — sources-as-relation with inverses, attested
computation, signatures, OKF export.
