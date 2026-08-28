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
   that exists. *(Invariant: the root is the reachable document with no
   spanning-parent, and the one that declares or points at the workspace's
   policy (rule 3); the name convention just finds it without scanning.)*
2. **Read its metadata block.** Split frontmatter from body by fence — `---`
   (YAML), `;;;` (JSON), or a ```` ```fig ```` block. The block is a key→value map.
3. **Read the policy, from both homes.** Workspace policy is one vocabulary
   with two homes: the root's `prov:` key, and the **config document** the root
   names through a top-level `config` pointer (`config: prov.yaml`), where the
   same keys sit at *top level* because the whole document is policy. Resolve
   per key, `config document > root prov: block > default`. A workspace may use
   either home or both, so a reader that consults only one will miss policy that
   is really there.

   `spec` is an integer naming which version of these rules applies. A higher
   number than you know means you may still traverse structure (rules 4–5 are
   stable) but should treat unknown policy keys as opaque.
4. **Read `relations` + `spanning`** — resolved across both homes as in rule 3.
   These declare the graph vocabulary (§2). Absent ⇒ the default vocabulary:
   `contents`/`part_of` containment, spanning `contents`.
5. **Unfold.** From the root, follow the field named by `spanning` to reach
   every node; each node's own block repeats the process. Non-spanning relations
   are the overlay graph.

Everything past rule 3 is *learned from the workspace*, not known in advance —
which is what lets the vocabulary vary per workspace without a foreign reader
needing to know it beforehand.

## 2. Self-describing the vocabulary

The relation vocabulary is declared in **either policy home** (rule 3) — shown
here in the root's `prov:` block, where it nests under `prov:`; in a config
document the identical keys sit at top level, with no `prov:` wrapper. Each
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

The same vocabulary in a config document drops the wrapper — `spec: 1`,
`spanning: contents`, `relations:` at top level — which is how this repository's
own `prov.yaml` spells it. Neither form is preferred; `prov config --home
<root|sidecar>` moves policy between them without changing what it means. See
[Config vocabulary](/docs/config-vocab.md) for the full key list and the
precedence chain.

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
a known one (casing/spelling drift). Both offer a repair to choose from rather
than one to apply: respell the value, or widen the vocabulary to admit it. A
*retired* term is never offered the second — writing a bare `term:` over it would
un-retire it and destroy the `id` and `means` it carries.

### The vocabulary file

A flat vocabulary is a **whole-file config document** (§5) — a self-describing
node (a `title`) declaring a `vocabulary` marker and a `terms:` mapping. Like the
registry it is *machinery*, reached one-way through the field's `vocabulary`
pointer, so it carries no `part_of` back-link (see §4):

```yaml
# vocab/audiences.yaml
title: Audiences
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
whose `contents` are term nodes — ordinary *content* containment, so each term is a
real node (with `part_of`, a prose body, and backlinks); only the *flat* form is a
whole-file machinery store.

## 4. Link target kinds

prov's graph is **homogeneous**: every node is a plaintext document with an
embedded metadata block. Heterogeneity — binaries, machinery, external
resources, controlled terms — never enters the graph as a node; it is *referenced*
through a typed field on a real node, and the field's kind is the contract. There
is no such thing as "linking a non-content file directly." The kinds:

| Target | Declared as | Contract |
| --- | --- | --- |
| **Content node** | a relation (`contents`/`part_of`/`links`/your vocabulary) | in the graph; two-way (inverse maintained); ID-able; rewritten on move; orphan-checked |
| **Machinery** | a one-way pointer relation (`registry`, `config`, `recycle_bin`, `history`, a *flat* `fields` `vocabulary`) | plaintext, reached *from the root only*; **no inverse, no `part_of` back-link, not ID'd as content, not orphan-checked, not in the spanning tree** |
| **Opaque payload** | the `content` field | *not a node* — the bytes are the body of a sidecar node (an attachment); hashed for fixity, never parsed |
| **A directory of opaque payloads** | the `manifest` field (exclusive with `content`) | *not nodes* — one node stands for the whole set through a manifest store listing every opaque file under a directory it claims completely, each row optionally hashed; the node hashes the manifest. Not in the graph, not orphan-checked, never parsed. See [Manifests](/docs/manifests.md) |
| **Controlled term** | a `fields` value | resolved by term *key* against the field's vocabulary, checked (§3) — not traversed, and no more traversed when the terms are nodes (below) than when they are rows |
| **Reified vocabulary** | a `fields` `vocabulary` pointer under `reify: true` — the index node, and its spanning children as terms | *ordinary content*: in the graph; two-way (inverse maintained); ID-able, and the term node's own id **is** the term's; rewritten on move; orphan-checked. Reached twice over — down the spanning tree like any node, and through the `fields` pointer, which is configuration naming content rather than a pointer to machinery |
| **Generated prose** | a one-way pointer relation (`about`) | plaintext in the workspace's *content* format; reached from the root only; **no inverse, no `part_of`, no id, not in the spanning tree, not orphan-checked**; rewritten **whole** by prov and never merged; a pure function of configuration, therefore **discardable** — deleting it loses nothing |
| **External** | a URL | recognized by syntax, never resolved or validated |
| **A place inside a document** | a `#locator` suffix on any target | *not a target of its own* — the document part resolves normally; the locator is carried, never resolved or validated, and preserved across moves |

Four consequences worth stating outright:

- **A non-plaintext file is wrapped, not linked.** To bring an image or PDF into
  the workspace, `attach` mints a sidecar (`photo.jpg.yaml`) — an ordinary content
  node whose `content` field names the opaque payload. The graph stays all-plaintext;
  the binary rides along as a node's body.
- **At scale, the wrapper is shared.** One sidecar per file is the right shape
  for the file you thought about and the wrong one for the archive you dumped:
  ten thousand photographs would mean ten thousand documents, which no editor can
  browse and no sync transport carries cheaply. A `manifest` node stands for the
  directory instead, and the difference from a sidecar is a *claim of
  completeness* — the manifest names every opaque file under its root, so a file
  that appears or vanishes is drift prov reports. Nothing else can report it:
  covered files are not documents, so the orphan walk cannot see them, and not
  links, so the census cannot either. Files prov *can* read are deliberately not
  claimed, which is what keeps a manifest from ever shadowing a document.
- **Opacity is declared, not inferred.** A payload is normally opaque because its
  extension is not one prov reads, but a sidecar may also say so outright with
  `attachment: true`, and that marker wins. It is how a *specimen* is carried — an
  example document, a fixture, a captured export, whose metadata block is an
  exhibit rather than a claim about this workspace (`attach --opaque`). A reader
  must not interpret a declared payload as a document: its links are not edges,
  its `title` does not answer a nominal reference, its `id` does not enter the
  registry. Contrast a *separated* node, whose `content` names a prose body that
  **is** a document in its own right: the marker is what distinguishes the two
  when the extension no longer can.
- **A locator is not a node.** Verse 1 of a chapter is not a document, so it is
  not in the graph — it is a *place* named on an edge that lands on the chapter.
  This is what lets the reading unit and the addressing unit differ: one document
  per chapter, one address per verse, without minting a node for every address.
  The edge is real (inverse maintained, rewritten on move, orphan-checked); only
  the sub-document part is opaque. See
  [Reference styles](/docs/reference-styles.md#locators--pointing-inside-a-document).
- **Machinery is reached one-way.** The root points *down* at a machine file
  through its typed pointer (`config: prov.yaml`); the machine file declares no
  back-link. It is not content — not in the containment tree, not ID'd, not
  orphan-checked — so a `part_of` back to the root would assert a tree membership
  it does not have. Its only self-description is a human `title`.
- **Generated prose has machinery's shape but not its carrier.** `about.md` is
  reached one-way from the root, carries no `part_of` and no id, and stays out of
  the spanning tree — machinery in every structural respect. It is a separate
  kind because §5's MUST does not fit it: record stores are whole-file config
  documents precisely because prov re-lays-out their sorted records and prose has
  no stable home there, whereas this file is *entirely* prose, written in the
  workspace's content format. What makes that safe is the other half of the
  contract — prov rewrites it whole from configuration, so it is derived rather
  than authoritative, and a conflicted copy is regenerated rather than merged.

## 5. Where things live — placement rules

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

## 6. Versioning

`spec` is an integer. New keys are added *additively* under the same spec until a
breaking reshape bumps it. A reader may always traverse structure across a spec
gap (rules 1–2, 4–5 are stable); only policy interpretation is spec-gated. `check`
warns when a surface declares a `spec` newer than the build understands
(`ConfigSpecAhead`). Pre-1.0 the number is fixed at 1 and unenforced; at 1.0 the
kernel freezes and the contract takes effect.
