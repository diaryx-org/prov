---
part_of: '[prov](/README.md)'
---
# prov/1 — the workspace spec

> How a prov workspace describes itself, precisely enough that a tool knowing
> only this page can unfold it. Complements DESIGN §1 (self-description), §2
> (mechanism vs vocabulary), §3 (spanning tree), §5 (stores), §6 (reachability).

A prov workspace is *self-describing*: its structure lives in the documents' own
frontmatter, not in the filesystem layout or an app-private sidecar. "Self-
describing" has an irreducible floor, though — a reader must share *some*
convention to bootstrap. This page is that floor, kept as small as it can be.

Status: pre-1.0. The `spec` marker is fixed at **1** and not yet enforced; at 1.0
the kernel below freezes and the marker becomes a compatibility contract.

## 1. The bootstrap kernel (frozen at 1.0)

Given a directory, a reader that knows only these five rules can traverse any
prov workspace:

1. **Find the root.** The root document is the file named by a one-line `.prov`
   pointer if present, else the first of `README.md`, `readme.md`, `index.md`
   that exists. *(Invariant: the root is the reachable document carrying a
   `prov:` key with no spanning-parent; the name convention just finds it without
   scanning.)*
2. **Read its metadata block.** Split frontmatter from body by fence — `---`
   (YAML), `;;;` (JSON), or a ```` ```fig ```` block. The block is a key→value map.
3. **Read `prov.spec`.** An integer naming which version of these rules applies.
   A higher number than you know means you may still traverse structure (rules
   4–5 are stable) but should treat unknown policy keys as opaque.
4. **Read `prov.relations` + `prov.spanning`.** These declare the graph
   vocabulary (§2). Absent ⇒ the default vocabulary: `contents`/`part_of`
   containment, spanning `contents`.
5. **Unfold.** From the root, follow the field named by `prov.spanning` to reach
   every node; each node's own block repeats the process. Non-spanning relations
   are the overlay graph.

Everything past rule 3 is *learned from the document*, not known in advance —
which is what lets the vocabulary vary per workspace without a foreign reader
needing to know it beforehand.

## 2. Self-describing the vocabulary

The relation vocabulary is declared in the root's `prov:` block. Each
`relations.<name>` entry may carry structural definition keys, and one relation
is named spanning:

```yaml
prov:
  spec: 1
  spanning: contents          # the single-parent discovery spine (§3)
  relations:
    contents:
      means: "documents contained by this one"   # human gloss — carried, never read
      cardinality: many                          # many | one
      inverse: part_of                           # the reciprocal field
    part_of:
      means: "the document that contains this one"
      cardinality: one
      inverse: contents
    see_also:
      cardinality: many
      inverse: see_also        # a symmetric overlay relation
```

This is a faithful serialization of the in-memory `RelationSet`
(`prov/src/relation.rs`): prov reads `cardinality`, `inverse`, and `spanning`;
`means` is a tier-3 gloss it carries so a *person* reading the frontmatter learns
the vocabulary too. A `relations` entry may also carry reference-style keys
(`notation`/`path_style`/`target`/`label`) — the two halves share one block.

**The single-parent invariant (§3).** The relation named by `spanning` must have
an inverse declared with `cardinality: one` — that is what makes the spine a
single-parent tree with a unique root. `check` flags a spanning relation whose
inverse is `cardinality: many` (`ConfigIssueKind::SpanningNotSingleParent`);
multi-parent membership belongs on a *non-spanning* overlay relation, which may
be many-to-many.

**Graceful degradation.** A workspace that declares no `relations` uses the
built-in diaryx vocabulary unchanged (`RelationSet::from_config`), so a minimal
hand-authored vault spells out nothing. The declaration is what a workspace adds
to be legible to a foreign reader.

## 3. Controlled vocabularies (`fields`)

A frontmatter field prov merely *carries* (a bare `tags:` string) becomes a
*resolvable, checked reference* the moment a `fields` entry points it at a
vocabulary. This is DESIGN §2's rule — consistency is a property of resolvability
— applied to file-to-term references (tags, audiences, statuses).

```yaml
prov:
  fields:
    tags:
      values: open            # folksonomy: unknown values allowed, near-misses warn
      vocabulary: '[Tags](/vocab/tags.yaml)'
    audience:
      values: closed          # every value must be a known term (privacy-critical)
      vocabulary: '[Audiences](/vocab/audiences.yaml)'
      reify: true             # each term is its own node (backlinks, prose, stable id)
```

`check` then verifies every value of that field over the reachable document set
(§8): a **closed** field emits `UnknownTerm` for any value not a known term; an
**open** field emits `TermNearMiss` only when an unknown value closely resembles
a known one (casing/spelling drift). Diagnosis only — no autofix.

### The vocabulary file

A vocabulary is a **whole-file config document** (§4) — a self-describing node
(`title`, `part_of` back toward the root) declaring a `vocabulary` marker and a
`terms:` mapping:

```yaml
# vocab/audiences.yaml
title: Audiences
part_of: '[My Vault](/README.md)'
vocabulary:
  field: audience
  values: closed
terms:
  public:
    id: aud_7x2q              # stable identity — the label can change, id-refs survive
    means: "Anyone; safe to publish"
  friends:
    id: aud_k9fp
    means: "People I know personally"
    gate: circle:friends      # ← arbitrary payload: carried, never read (tier 3)
  archived_2024:
    retired: true             # known but no longer valid; never silently reissued
```

prov reasons about the term *keys*, each term's `id`, and `retired`; every other
key (`means`, `gate`) is tier-3 payload it transports untouched — which is how a
diaryx audience hangs gate/theme config off a term prov still validates
membership in. A **reified** vocabulary (`reify: true`) is instead an index node
whose `contents` are term nodes — ordinary containment, so each term gets a prose
body and backlinks; only the *flat* form is a whole-file store.

## 4. Where things live — placement rules

Because reachability makes an inline block and a linked file *semantically
identical* (both unfold from the root), where a thing lives is an ergonomic
choice, never an architectural one. Two rules:

- **MUST — record stores are whole-file config documents.** The id registry, the
  recycle-bin index, and *flat* vocabularies are files prov re-lays-out as sorted
  records (DESIGN §5). Prose has no stable home there, so these must be
  `.yaml`/`.json`/`.figl` documents, never markdown-with-frontmatter. prov
  refuses a markdown carrier at load (`require_whole_file`) and `check` reports it
  (`MalformedStore`). The format is the file's own (its extension) — never
  declared beside the link.

- **SHOULD — inline until it grows, churns, or wants its own cadence.** Keep
  small, stable, human-curated policy in the root's `prov:` block. Split a thing
  into its own linked file when it (1) grows unboundedly, (2) is rewritten by prov
  on ordinary mutations (a merge/contention hotspot), or (3) wants a different
  edit cadence than the root's content. A 3-term audience set with no payload
  stays inline; a curated vocabulary with per-term config earns a file.

  For *workspace policy* specifically, the choice is reversible at any time:
  `prov config --home root` inlines all policy into the root block and drops the
  sidecar; `prov config --home sidecar` moves it into `prov.yaml` and clears the
  block. Both homes read identically, so this only relocates bytes.

## 5. Versioning

`spec` is an integer. New keys are added *additively* under the same spec until a
breaking reshape bumps it. A reader may always traverse structure across a spec
gap (rules 1–2, 4–5 are stable); only policy interpretation is spec-gated. `check`
warns when a surface declares a `spec` newer than the build understands
(`ConfigSpecAhead`). Pre-1.0 the number is fixed at 1 and unenforced; at 1.0 the
kernel freezes and the contract takes effect.
