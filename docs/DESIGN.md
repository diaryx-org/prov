---
part_of: '[prov](/README.md)'
---
# prov — design & vision

> A *self-describing plaintext workspace*: a set of documents whose structure
> lives in the documents' own embedded metadata, not in the filesystem layout or
> an app-private sidecar folder.

This document captures the reasoning behind prov — what it is, the positions
it takes, the positions it deliberately leaves open, and why. It is the crate's
north star; when a decision is unclear, it should be resolvable from here.

---

## 1. The thesis

**prov** — *Plaintext Records, Organized & Verifiable* (and the usual short form
of *provenance*) — describes itself the way its documents do. A prov workspace is
one you can hand to *any* tool and it explains itself. Follow the links declared
in each document's metadata and the whole structure unfolds, anchored by a
distinguished root that describes the whole.

The one inversion that defines the crate:

> **The edges of the document graph are declared *inside* the nodes
> (frontmatter), not by the container (the filesystem).**

That single move is the entire value proposition. A filesystem has no
self-description (structure is imposed from outside, by directory nesting). A
database has a schema, but it lives outside the data. A prov workspace keeps
the structure *in the documents*, in plaintext, in the open — portable, diffable,
and legible without the app that produced it.

## 2. Opinionated mechanism, flexible vocabulary

The crate is opinionated about *how* structure works and agnostic about *what*
the structure is called.

- **Opinionated mechanism** — links live in embedded metadata; parsing goes
  through `fig`; there is a canonical containment tree; identity is additive;
  integrity is a first-class, checkable property. These are not configurable.
- **Flexible vocabulary** — *which* frontmatter fields are links
  (`contents`/`part_of`, `links`, or an entirely different set), their
  cardinality, their inverses, and which one is the canonical tree. This is
  configured per workspace via `RelationSet`.
- **Flexible source** (planned) — where the graph comes from: frontmatter links,
  the filesystem tree, or a hybrid, behind a `StructureSource` seam. Same
  downstream graph, different intake.

Nothing about "diary", "journal", or even "contents" is baked into the core.
`RelationSet::diaryx()` is merely a preset; the test suite proves a `part`/`whole`
vocabulary works identically with zero diaryx assumptions.

### Where configuration starts and ends — *does prov read it back?*

The line above ("mechanism fixed, vocabulary flexible") raises a recurring
question every new field forces: is *this* thing configurable, and how far? The
operative test is one question — **does prov read the value back and reason
about it?** — and it sorts every field into three tiers:

1. **Mechanism — not configurable.** How structure works, that integrity is
   checkable, that identity is additive, and *the format of anything prov
   itself maintains*. A prov-maintained value must be machine-standard
   because prov has to parse, compare, and rewrite it reliably — years later,
   on another tool, after a merge. The `sha256:` fixity digest, the opaque id,
   the registry, an RFC 3339 `updated` timestamp: all live here. They are
   standardized *precisely because* they are not for human eyes but for machine
   reasoning. Owning the format means owning the *resolution* too — timestamps
   carry six fractional digits, fixed width, so two of them written in the same
   second are orderable rather than tied.
2. **Vocabulary & representation — configurable.** The *names and surface
   spellings* of those mechanisms: which fields are relations, the spanning one,
   reference styles, id storage, embed format, the *name* of the `updated` field,
   whether a feature is on. Configuring a workspace means re-spelling prov's
   fixed mechanisms for your vault — never redefining them. Essentially all
   prov config lives here.
3. **Content — not prov's business.** Everything prov merely *carries*:
   `title`, a user's own `date`, arbitrary frontmatter, the body. prov never
   reasons about these, so there is nothing to standardize and nothing to
   configure — the user owns them completely, today, just by writing them.

The tension this resolves is the seductive middle option: a prov-*maintained*
field that the user also *formats*. That combination is incoherent — the moment
prov must round-trip a value it has to understand the format, and parsing
arbitrary user formats (ambiguous, locale-dependent, lossy) is exactly the
fragility an archive cannot afford. So the real boundary is:

> **prov-maintained ⟹ prov owns the format. User-formatted ⟹ the user
> maintains it.** There is no "prov maintains a field whose format it does not
> understand."

The corollary — **store canonical, render pretty**: human-friendly formatting is
a *presentation* concern (a viewer, an export), never a storage one. prov
stores the canonical machine value (RFC 3339 `…Z`, six fractional digits); a UI
displays it however the reader likes — which is also the answer to "microseconds
in my frontmatter are noisy": that is a rendering complaint, and rendering is not
where the value lives. This is why "let users choose the timestamp format" never
becomes a storage-config question: a user who wants a pretty date authors a
`date` field prov never reads (tier 3), while the maintained `updated` field
(tier 1/2 —
fixed format, configurable *name*) stays trustworthy. The two pulls stop fighting
because they are aimed at two different fields.

So: **config exists to rename and re-spell prov's mechanisms — never to
reformat its machine-state (fixed) or to manage your content (already yours).**

## 3. Structure: a spanning tree with an overlay graph

Containment is a **tree**, not a general DAG — but that is a feature, not a
limitation, and the distinction is subtle enough to state precisely.

- Exactly one relation is marked **spanning**: single-parent, and the inverse of
  a "contains" relation. This is the workspace's *discovery spine*. Because every
  node has a unique path to a unique root, the question "what describes this
  workspace?" has one unambiguous answer. A pure DAG loses that — multiple
  parents, multiple roots, ambiguous discovery — and with it the self-describing
  property that motivates the whole crate.
- Every **other** relation may be many-to-many. Multi-membership ("this note
  belongs to two projects") is expressed through non-spanning relations, i.e. the
  overlay graph.

So the honest model is: **a single-parent containment tree (the backbone) with an
arbitrary reference graph laid over it.** The materialized index (§6) is what
makes the overlay edges as fast and first-class to query as the tree — so the
tree is a spine, never a ceiling. Nobody should *feel* limited to a hierarchy.

Framing rule: it is never "tree vs DAG." It is "one spanning relation + N
overlay relations," where cardinality is per-relation config and exactly one
relation is designated spanning.

The spanning relation, cardinalities, and inverses are now **declared in the
root's `prov:` block** (the self-description layer, `docs/spec.md`), not merely
built into a preset — so a foreign reader learns the vocabulary from the document
(`RelationSet::from_config`, falling back to the diaryx preset when nothing is
declared). The single-parent invariant is *checked*: `check` flags a spanning
relation whose declared inverse is `cardinality: many`
(`ConfigIssueKind::SpanningNotSingleParent`), catching an incoherent vocabulary
at author time rather than as a runtime `DuplicateContainment`.

## 4. Identity is a strictly-additive layer

The load-bearing architectural commitment:

> **The graph, traversal, and mutation layers operate on paths and never require
> an ID. Identity is a resolver + a registry bolted *on top* of a fully
> functional path-only workspace.**

This is what makes identity genuinely optional rather than "technically a no-op."
A move rewrites path-based frontmatter links because that is inherent to a linked
workspace; *if* an identity layer is present, it also updates `id → path` in the
registry — an optional step, gated on whether a registry exists. Nothing in
containment, traversal, ordering, or validation ever dereferences an ID.

In the type system: `Workspace<FS, Id = NoIdentity, Ix = NoIndex>`. Paths-only is
the *absence* of the subsystem — it monomorphizes out, produces no sidecar
artifact, and (crucially, see §5) writes no ID into any document. Opting in flips
a type parameter via one builder line.

### Derived vs registered — the minimal authoritative set

An ID is registered only when something creates a **durable, out-of-location
reference** to a document. Everything else stays derived and costs nothing. So:

> **The registry contains exactly the set of IDs that something external depends
> on being stable.**

This is the same idea as the original "IDs are rederivable unless published" plan,
seen from the publish side. Publishing (a `permalink`) and linking-by-ID are the
registration triggers; the other ~95% of files nothing points at pay no
identity-maintenance tax. It also shrinks the dangerous, merge-critical,
must-not-lose surface to its minimum.

### Model B — IDs are minted at registration, not derived

Two coherent schemes were considered:

- **A — derived-then-frozen.** Every file has a deterministic ID (a hash of its
  path); registration snapshots it. Requires collision handling and makes
  "stable" a happy accident of not-moving.
- **B — minted at registration (chosen).** Unregistered files have *no* opaque ID
  at all — they are addressed by path (`[./notes/file]`). The opaque ID is *born*
  the moment a document is linked-by-id or published.

B is chosen because the model falls out clean:

- Every opaque ID is authoritative **by construction** — there are no derived
  opaque IDs to reconcile, no path-hash collision dance.
- "Rederivable unless published" becomes literally true: unregistered = addressed
  by path (nothing stored); registered = minted, in the registry.
- It cleanly separates the two identity layers: the **internal prov ID**
  (minted opaque, for in-workspace stable links) and the **published permalink**
  (an ARK blade in diaryx, for external URLs). Publishing implies registration;
  linking-by-id implies registration; they are distinct events, and the internal
  ID need not equal the permalink.

The one cost — a UI cannot show a stable short handle for a file until it is
linked — is judged negligible.

### The registration lifecycle

`Registration { on_create, on_link, on_publish }` is the dial:

- **OFF** — paths only.
- **LAZY** (recommended default) — register on link-by-id or publish.
- **EAGER** — also register on create. This regrows the registry to *every* file,
  forfeiting the minimal-authoritative-set benefit; it is a legitimate choice for
  users who want stable-identity-from-birth, and it is one flag.

Registration needs **two paths**, and both must exist:

1. **Eager** — when prov itself authors an ID reference, it registers
   atomically.
2. **Reconciling** — a validation pass scans for `[[prov:id]]` references that
   arrived out-of-band (paste, `git merge`, another editor) and registers/repairs
   them. This reuses the validation module (§7).

**Known hazard:** the one unrecoverable case is an out-of-band edit that inserts a
raw ID reference and then moves the target *before* prov ever reconciles. The
durable reference was created behind prov's back; nothing can save it. The
mitigation is reconcile-on-load, and this is documented as a limitation rather
than pretended airtight.

## 5. The index: one artifact, two natures

The ID registry, the materialized graph, and the resolution cache all want to be
the *same* artifact — one `IndexStore` that prov keeps consistent as part of
its normal mutation job, serialized (via `fig`) to any supported format and
stored anywhere. That convergence is elegant, but it hides a sharp edge that the
design must respect:

> **The index fuses two natures. The graph/resolution parts are a *derived
> cache* — a pure function of the documents, always rebuildable, harmless when
> stale. The `id → path` registry (under model B, where IDs live only in the
> index) is *authoritative, non-derivable state* — it cannot be rebuilt from the
> documents.**

Consequences the implementation must honor:

- **Structurally separate the two even inside one store.** A `derived` section
  (disposable, blow-away-and-regenerate) and a `registry` section (durable,
  backed up, merge-critical). Fusing them into one undifferentiated blob loses
  the cache's safety valve.
- **The registry's write belongs in the same unit as the documents it describes.**
  This falls straight out of the two natures. A derived cache may lag: if it is
  stale, rebuild it. Authoritative state may not — a mutation that maintains three
  documents' links but loses its `id → path` update leaves every `prov:<id>`
  reference to the moved document resolving to nothing, and *nothing in the
  workspace can repair it*, because the mapping was never in the documents to
  begin with. So a mutation stages its registry write into the same `ChangeSet`
  (`prov/src/change.rs`) as its document edits, and the two land or fail
  together. The corollary is the honest half: the frontmatter *shadow* copy
  (`id_storage: frontmatter`) is derived — idempotently re-stamped from the
  registry on any later run — so it is deliberately left outside that unit. What
  can be rebuilt need not be transactional; what cannot, must be.
- **A single central index file is a merge/write-contention hotspot.** Every
  mutation on every device touches it, so every sync touches it — re-concentrating
  exactly the contention that per-file frontmatter avoids. This is why
  `IndexStore` is a trait: sync can back the registry with something
  non-file-shaped (per-doc sidecars, an append-only log, a Durable Object). When
  the store *is* a file, its on-disk format must be designed for clean diffs
  (stable/sorted ordering, one record per line).
- **Optional escape hatch.** A "stamp IDs back into frontmatter" operation gives
  the index's cleanliness as the working model *plus* a durable, portable,
  rebuildable backup. If frontmatter carries a shadow copy of the ID, the registry
  becomes rebuildable again — i.e. back to a pure cache. (This trades away some of
  model B's document-cleanliness; it is a per-deployment choice, not a crate-level
  default.) **Implemented** as the `id_storage` axis (`IdStorage`):
  `frontmatter` keeps the shadow copy *and* the cache; `frontmatter_only` drops
  the registry entirely and rebuilds id→path from a scan (`Workspace::scan_ids`),
  at the cost of tombstones. The ID then travels with the file — copy- and
  out-of-band-move-robust — since a move no longer needs a registry update.

The materialization also serves performance: today the graph is derived by
walking the spanning relation from a root on demand; an optional materialized
index (id/path → node, adjacency, precomputed inverses) serves callers doing many
queries — an LSP, a static-site builder, a TUI — without re-walking.

## 6. Wikilinks and positioning

The user-facing payoff of registered identity is stable, location-independent
links: `[[prov:ajp7eq|My file]]`. Authoring such a link, or publishing, *is*
the registration event.

This is deliberately Obsidian-shaped, with one decisive difference:

> **Obsidian, except the user owns what `.obsidian/` used to own.**

Obsidian's link-rewrite-on-rename, its graph, its block IDs — all of that
intelligence lives *in the app*, and the state lives in an opaque dotfolder the
user cannot read with another tool. prov inverts exactly that: the same
superpowers (stable IDs, backlinks, rename-safety), but the identity state is
*data the user owns* — in their tree, in any `fig` format, versioned with their
content. prov is that vault intelligence as a portable, embeddable library.

Ownership alone is not enough, though: a readable registry in an unlinked
dotfolder is still `.obsidian/` with a nicer file format. The property that
actually distinguishes a prov workspace is **reachability** — the root
document links its registry through the `registry` relation, so following the
links from the root discovers the identity state like everything else. Where
the registry lives is a fact about the workspace, declared in the workspace;
the registry document self-describes with a `title` and is validated by `check`
like any other reached file. It carries **no `part_of` back-link**, though: the
registry is *machinery*, reached one-way through the root's `registry` pointer —
not a content node in the spanning tree — so a `part_of` would assert a
tree membership it does not have (the "link target kinds" typology in
`docs/spec.md`; the same one-way rule governs the `config` document, the
recycle-bin index, and flat vocabulary stores). Because it is a
**record store** (prov re-lays-out its sorted records), it must be a *whole-file
config document* (`registry.yaml`, fig-native, …), never markdown-with-
frontmatter: prose has no stable home in a file prov re-sorts, and the whole-file
carrier makes extension→format sniffing unambiguous. prov refuses a markdown
carrier at load (`require_whole_file`) and `check` reports it (`MalformedStore`).
The same rule governs the recycle-bin index and *flat* vocabulary stores; a
*reified* vocabulary's term nodes are ordinary content and stay markdown.

## 7. Serialization and embedded formats

- **`fig` value tree is the common currency.** Access is dynamic (link fields are
  configurable, so a fixed struct will not do). The parse/serialize paths are
  serde-free — they walk `fig`'s native tree — which keeps serde out of the call
  graph and out of a WASM binary. This mirrors the proven approach in
  `diaryx_core`'s `yaml` module (including the `width(1)` block-layout fix for
  fig 2.0's flow-style default).
- **`fig` and `serde` both behind features.** `fig` has its own `serde` feature,
  so targeting the value tree as the common currency makes the backend a
  build-time choice the core never sees. `fig` is already published in multiple
  places; shipping it natively is defensible (the "fig + prov" ecosystem),
  while a `serde` backend keeps the door open for those who do not want the Zig
  toolchain `fig`'s build requires.
- **Multi-format embedded metadata.** The crate is agnostic about the *format*
  of the embedded block — YAML (`---`), JSON (`;;;`), fig-native
  (```` ```fig ````), endmatter — anything `fig` recognizes. The **fence layer**
  turned out to live in `fig` itself, not prov: `fig::detect` sniffs the
  archetype (fig 2.1, upstreamed from this project's needs), `fig::split`
  separates content from body, and `EmbedType::inner_format` couples each fence
  style to its format so invalid combinations are unspellable. prov records
  the detected `EmbedType` on every `Document` so writes **preserve the original
  format and layout** (never rewrite a ```` ```fig ```` block as YAML). prov
  feature gates (`yaml`, `json`, `fig`, …) forward to the corresponding `fig`
  feature. A useful consequence: the sidecar index need not match the document
  format — documents can be YAML while the index is fig-native for parse speed.

## 8. Validation is the sleeper feature

Integrity-checking with autofix is the rarest, most reusable asset in this space
and should be a loud, first-class feature — not a footnote. The model: a set of
`ValidationError` variants (broken spanning-parent, broken contains-reference,
orphan, cycle-where-disallowed, missing backlink, dangling/unregistered ID) plus
warnings and an autofixer. It returns findings; it does not panic. It also hosts
the reconcile pass from §4 (an unregistered `[[prov:id]]` reference is just
another finding with an autofix: register it, or flag it if it cannot resolve).

**A finding usually has more than one repair, and prov does not pick.** `check`
answers "what is wrong"; what *should be done* is frequently a judgment — which
of two containers is a document's real parent, which near-match a broken link
meant, whether a mistyped tag is drift to correct or a term to admit. So a
finding yields a list of `Remedy`s rather than a single `Fix`, each tagged with a
`Warrant`: `Derived` when the repair is a pure function of an authority
(regenerate the derived page from config, rebuild the derived index from its
directory, spell a link the way the file on disk actually is) and therefore
choosing nothing; `Judgment` when rivals exist; `Destructive` when it removes
something authored. Only `Derived` repairs may run unattended — that is what
`check --fix mechanical` is — and the *arity* of a finding's remedy list is a
consequence of this, never the criterion.

**What a repair may touch.** Frontmatter, and body spans twig's parser itself
reported as links. Not ordinary prose: the objection is that `[[…]]` may be code
(`[[None] * width]`) and a lexical scan cannot tell, which is why a wikilink —
whose span is lexical even with twig masking code — stays diagnosis-only while an
inline `[label](target)` does not. And never bytes: a repair may drop a link, but
destroying a file is what a verb asked for by name (`rm`, `history-prune`,
`empty-bin`) is for.

**Discovery is reachability-bounded.** The orphan check does not scan the whole
subtree — it inspects only the directories a linked document already occupies,
and never recursively. A subdirectory nothing links into (a vendored tree, a
nested prov workspace, a `scratch/` folder) is neither read nor reported, so
`check` stays quiet about files that were never opted in. This is the same
"invisible unless attached" rule §3's reachability applies to files, extended to
directories: a directory enters scope only through an explicit act that links
into it (`new`, `adopt`, `attach`, a `mirror` import), after which `check` keeps
it honest — and scope grows with the links. The deliberate trade is that a
document dropped into a not-yet-linked folder is invisible rather than flagged;
the alternative (flagging every stray file anywhere beneath the root) makes
prov unusable inside a larger repo. The recursive filesystem walk survives
only where it is an *explicit* import — `content_documents`/`plan_mirror` for
`init --adopt mirror`, and `attach --all --recursive` — never in steady-state
validation.

Lifting the bound is not the same as having none: `attach --all --recursive`
still refuses to descend into the directories `parked_dirs` names — the history
store's `events/` and `blobs/`, and the recycle bin's `items/`. Those are
unreached because they are prov's own bookkeeping (§4's byte-parking stores),
not because nobody has got round to linking them, and a sidecar minted beside a
blob is an orphan whose payload `history-prune` is entitled to collect.

**The title index is bounded too.** It is built *lazily* — only when a
`[[alias]]` link is actually encountered, so a path/id workspace (the diaryx
default) never scans at all — and when it is built, it is scoped to the reached
directories: a cheap path/id pre-pass (`title_scope`) collects the directories
the tree occupies, and only those are indexed. So an alias resolves within the
workspace without reading `target/`, a vendored tree, or a nested workspace at
the repo root, and a same-titled document in an unreached subtree cannot collide
with a workspace title. The one case that cannot be bounded is an **alias-addressed
spanning** relation: descending the tree then needs every title up front (the
chicken-and-egg the flat scan avoids), so `title_scope` reports it and the build
falls back to the full whole-tree scan. (The frontmatter-id registry, §5, still
scans whole-tree — bounding it has the same spanning-id coupling and is a separate
step.)

## 9. Extraction discipline & status

prov is being extracted from `diaryx_core`. Guiding rules:

- **Read + write from the start.** The valuable, hard half is safe restructuring
  with link maintenance, which `diaryx_core` already does across ~18 mutation
  ops. There is no read-only milestone; a release waits until diaryx *can* depend
  on prov.
- **A beautiful API that forces a diaryx rewrite beats an ugly one that changes
  nothing.** Design the seams for their own sake; let real diaryx usage carve the
  ergonomics.
- **Guard the public surface.** Every diaryx-specific concern — ARK minting, the
  publish/audience/gate/theme config, config migrations — stays behind the
  profile and the `IdentityPolicy` / `IndexStore` / `StructureSource` traits, so
  none of it can calcify into the public API. The dual document model
  (`diaryx_core`'s path-based frontmatter vs id-based sync records) reconciles
  here: id-based vs path-based becomes a choice of policy + resolver, not two
  parallel type hierarchies.
- **Sequence.** Extract in place → diaryx depends on a local `prov` → dogfood
  until the seams (IndexStore, format layer, registration) are proven → publish
  last.

### Current status

The pure layers are real and tested; the filesystem-driven engine is staked but
not yet ported.

| Area | Module | Status |
| --- | --- | --- |
| Embedded metadata (parse/serialize, dynamic value) | `meta` | ✅ implemented + tested (format-parametric) |
| Document splitting (frontmatter fence) | `document` | ✅ all fig archetypes via `fig::detect` (`---`, `;;;`, ```` ```fig ````, endmatter); `EmbedType` recorded per document |
| Relation vocabulary + edge/child extraction | `relation` | ✅ implemented + tested |
| Identity policy + registration triggers | `identity` | ✅ NOID xdigit+check minter (ARK lineage, no shoulder), `Trigger` events, `Workspace::register` (idempotent, policy-gated), mint-by-rejection |
| Index store (id↔path registry) | `index` | ✅ `NoIndex` + `InMemoryIndex` + persistent `FileIndex` — records live under the `registry` key of a **whole-file config document** (markdown carriers refused at load via `require_whole_file`, §5), tombstones as `id: null`, block layout, per-record preserving upserts |
| Identity storage axis (§5 escape hatch) | `config`/`workspace`/CLI | ✅ `IdStorage`, spelled `id_storage: both` (**default**: stamp each doc's own `id` field + keep the registry as a rebuildable cache) · `registry` (only in the registry document) · `frontmatter` (no registry; id→path rebuilt by `Workspace::scan_ids`, tombstones forfeited). `init` prompts frontmatter vs registry; `--id-storage frontmatter-only` reaches the third. Frontmatter storage makes identity move/copy-robust — the ID travels with the file |
| Cross-workspace references | `link`/`config`/`workspace`/`validate`/CLI | ✅ a reference names another workspace's document as `id:<workspace>/<id>`, where the qualifier is that workspace's own `workspace_id` — the one device-independent half, so the only half that lives in config. prov owns the **grammar** and stops there: a qualified reference resolves to `Target::Foreign`/`Resolution::Foreign`, is never rewritten by a move (every rewrite site now filters on `Link::is_path_target`, which is false for *every* id form rather than only the local one), never reported broken (`check` has no evidence about a workspace it cannot see, and a finding raised on none is a false positive on every such reference that each host would then filter back out), never check-verified (the foreign workspace owns its id space and need not be prov at all — a diaryx ARK blade is a different length and alphabet), and rendered as a `NodeKind::Foreign` leaf when it is spanning. There is deliberately **no peer table in `prov.yaml`**: where another workspace can be found is a fact about a device, not about an archive (`notes = ../notes` is true on one machine, and wrong *silently* elsewhere, since a peer pointing at the wrong directory still resolves to real documents) — the same reasoning that keeps the fixity cache's location out of the config. Resolution is the host's: `prov-cli` keeps a device-local peer map (`prov peer add|list|remove|resolve`, `--peers` > `PROV_PEERS` > `XDG_CONFIG_HOME` > platform default, hand-parsed `<name> <path>` lines) and follows a reference by opening the peer and reading *its* registry; diaryx resolves the same reference through published ARK permalinks. The load-bearing invariant is **self-qualification**: a reference qualified with the reading workspace's own name *is* local — resolved through the registry, check-verified, and dangling loudly when absent — which is what lets a document keep working when it is copied into the workspace it names instead of going inert at the boundary; an anonymous workspace has nothing to compare against and so treats every qualifier as foreign. A malformed `id:` body (`id:`, `id:/x`, `id:a/b/c`) is its own case rather than falling through to a path, so a typo cannot quietly become a dangling filename. Registration stays a **publish-time** contract (prov never reaches into another workspace to register on its behalf), so a reference to an unpublished foreign document can dangle — a limit stated rather than hidden, since closing it needs exactly the reaching this design refuses. The name itself is **chosen, or minted on request, and never on prov's initiative**: `prov id --workspace [NAME]` writes the name you give or (bare) mints an opaque one twice a document blade's width — width being the only uniqueness available to a name nothing can check against the workspaces it might collide with. It is idempotent and never a rename, because by the second run the name is out in references this workspace cannot see; `prov config workspace_id` stays the deliberate way to change one. `docs/reference-styles.md` § "Across workspaces" |
| Registry reachability | `relation`/`workspace` | ✅ the root links its registry via the `registry` relation (in the diaryx preset); `Workspace::registry_path` discovers it by following the link — never an app-private sidecar path |
| Config files as documents | `document`/`edit` | ✅ `.yaml`/`.yml`/`.json`/`.fig`/`.figl` parse as whole-file-metadata documents (`MetaCarrier::WholeFile`); carrier-aware `MetaEditor` edits both shapes preserving comments/format |
| Config vocabulary + homes | `config`/CLI | ✅ one nested namespace (`docs/config-vocab.md`) with two homes — nested under `prov:` in the root's frontmatter (the description home) or top-level in the dedicated config document (the policy home), precedence *config doc > root block > default*; the `config`/`registry`/`recycle_bin` **pointer relations** stay top-level as structure. Reference style is the orthogonal `references: { notation (markdown\|wikilink\|bare) × path_style (root\|relative), target, label }` (internally `LinkStyle`, the 2×2 cross-product; a `canonical` path style was retired because a bare workspace-relative path resolves directory-relative and so never named what it meant — `docs/config-vocab.md`); `metadata: { format, embed }`; `id_storage: registry\|frontmatter\|both`; a `spec` version marker. `prov config <k> <v>` addresses nested axes by dotted key and refuses to write a setting `check` would flag |
| ID links (`prov:<id>` targets) | `link`/`tree`/`validate`/`mutate` | ✅ resolve through the registry everywhere paths do; never rewritten by moves (the registry update is the maintenance); findings: `MalformedId` (check char), `DanglingId` (tombstoned vs never-issued) |
| Workspace composition + builder | `workspace` | ✅ type-flipping builder |
| Traverse (spanning tree from a root) | `tree` | ✅ `Workspace::tree`; missing/cyclic/unreadable targets are marked nodes |
| Scan (directory-driven discovery) | `workspace`/`intake` | ✅ the orphan check is **reachability-bounded** (`direct_child_files` over reached directories, §8) — quiet inside a larger repo; the recursive `Workspace::content_documents` survives only for explicit imports (`plan_mirror` → a `StructurePlan`, `attach --all --recursive`) |
| Mutation with link maintenance | `mutate` | ✅ `create`/`rename`/`delete`/`recycle`/`restore`/`empty_bin`/`adopt`/`separate`/`combine`/`duplicate` (parent entry, inverse links, re-relativization, labels kept; fig `Embed` edits). `recycle` is the recoverable delete (see the recycle-bin row); `delete` is the hard one. `adopt` links an *existing* file both ways without touching its body — the onboarding complement of `create` (`docs/init-adoption.md`, Phase 1), driving `init --adopt` and the orphan autofix. `duplicate` copies a node as a fresh sibling under the same parent (fresh name, **no** cloned ID or children — a shallow copy, so no child gains a second parent), copying a separated node's body file too. `rename`/`separate`/`combine` each move a document's *own* id from one path to another rather than minting fresh, so each checks `Workspace::move_conflict` — the destination-path half of `registration_conflict`, since the id-direction half would misread the document's own current path as a foreign claim — and refuses before touching the registry rather than silently take the id off whatever the destination already names (`IndexStore::set_path` evicts the displaced id's forward entry as a backstop either way, mirroring `register`'s fix in 11abd38). **Non-goals vs diaryx** (`convert_to_index`/`convert_to_leaf`, `attach_and_move_entry_to_parent`): these reify diaryx's *directory-shaped* containment — a node earns children by becoming a folder, attaching moves the file into the parent's directory. prov's containment is link-shaped (§3, §8): a node gains contents in place and `adopt` links without moving, so there is nothing to convert between and no move-on-attach. The external id-sync hooks (`sync_*_metadata`) are folded into the per-op index maintenance behind the `IndexStore` seam (§9) |
| Validation | `validate` | ✅ findings: broken link, case mismatch, duplicate containment, missing inverse, unreadable, malformed/dangling id, ambiguous alias, **id mismatch** + **unregistered id** (the frontmatter-storage reconcile pair), **orphan** (a content document on disk nothing reachable links to — the onboarding signal, `docs/init-adoption.md`), **fixity mismatch** (see the fixity row), **config issue** (a key in either config surface — the root's `prov:` block or the linked config document — that `WorkspaceConfig::apply` silently ignores: a misspelled key that resembles a real axis, or a recognized axis with a value prov can't parse; `config::diagnose` is the shared judgment, so `prov config <key> <value>` refuses to *write* the same, and near-miss detection leaves user-owned fields alone per §2), **config spec-ahead** (a surface declares a `spec` newer than this build understands, so newer settings may be ignored — `config::spec_ahead`, shared with the CLI's proactive warning), **spanning-not-single-parent** (a declared spanning relation whose inverse is `cardinality: many`, §3), **malformed store** (a registry/recycle/vocabulary pointer resolving to a markdown doc rather than a whole-file store, §5), **unknown term**/**term near-miss** (a closed-field value that is not a known vocabulary term / an open-field value that resembles one — the self-description layer's term-consistency pass). **history index stale** (a history shard index that has drifted from the directory it describes — the expected shape of transport damage to the store's one mutable file; see the history row), **recycled bytes missing** (a bin record whose parked bytes are gone, so the deletion it records can no longer be undone — the one pass that looks inside the *unreached* `recyclebin/items/`, since §8's walk deliberately ignores it and the loss would otherwise surface only as a raw rename failure inside `restore`; per record, so a separated document's lost body is named specifically). Autofix: missing inverse ✅; id mismatch → trust the registry (rewrite frontmatter) ✅; unregistered id → adopt into the registry ✅; fixity mismatch → re-stamp to the current bytes (confirmation-gated) ✅; history index stale → rebuild that one shard from its own directory listing ✅; orphan + config-issue + config-spec-ahead + body-link + recycled-bytes-missing findings stay diagnosis-only (dropping a bytes-less bin record would destroy the last evidence of what was deleted and foreclose the real repair — putting the bytes back from a backup) |
| Fixity (content checksums) | `fixity`, `attach`, `validate` | ✅ the archival integrity question link-checking cannot answer — *are the bytes still the bytes?* A dependency-free, NIST-vector-tested SHA-256 (WASM-clean, spelled `sha256:<hex>` so an auditor verifies it with `sha256sum` — tool-agnostic like everything else). `check` grows a bit-rot pass over the reachable set, emitting `FixityMismatch`; it honors any recorded hash regardless of the fixity *setting* (which governs what is written), never false-alarming on an unrecorded or unrecognized digest. Config axis `fixity: off \| attachments \| all` (default **attachments**). `attachments` — attachment sidecars record a `content_hash` of their bytes (frictionless: a payload is never edited, so a change is unambiguously corruption). `all` — documents additionally hash their *body* (never frontmatter, so prov's own link maintenance never disturbs it); because a body is editable, `prov edit` opens `$EDITOR` and restamps on save, and an out-of-band edit is a re-stampable finding rather than a hard error (`Workspace::restamp_fixity`) |
| Bulk attachments (manifests) | `manifest`, `attach`, `validate`, `mutate`, CLI | ✅ the sidecar-per-file trade inverted for scale (`docs/manifests.md`): one node stands for a whole directory through a **manifest** — a whole-file record store (§5's MUST) naming the directory it covers and listing every opaque file under it, each row optionally `sha256:`-hashed. Ten thousand photographs cost two documents instead of twenty thousand, which is the difference between an archive a person can open and one no editor will browse or transport will carry. `manifest` is **mutually exclusive with `content`** — a node stands for one payload or a set, and every pass asking what it covers must get one answer (a node declaring both is `ManifestConflict`, diagnosis only: which key is the mistake is a claim about intent prov has no evidence for). Rows are relative to the manifest's `root`, deliberately unlike the history store's workspace-relative manifest (`history-format.md` §3.1) — an event describes a whole workspace and has no root to be relative to, where this describes a *directory*, so moving it rewrites one line rather than ten thousand. The chain is `node --content_hash--> manifest --hash per row--> files`: the node pins the list exactly as an attachment sidecar pins its payload, which is what makes the rows worth anything, since editing one (the cheap way to make a corrupted archive look intact) breaks a checksum already recorded. `root` is claimed **completely** for opaque payloads, and completeness is the whole point: a covered file is not a document (so §8's orphan walk cannot see it) and not a link (so the census cannot either), so without it a photograph could vanish silently. Files prov *can* read are **not** claimed — they stay ordinary documents, so a manifest never shadows one and deliberate shadowing stays the single-file `attach --opaque`, where the promise is visible beside the file it is made about. `check` runs only the cheap halves — the node's pin, and drift (one directory walk, **no file reads**) reported as `ManifestDrift` — because a `check` that re-reads an archive is a `check` people stop running, and one nobody runs finds nothing; corruption *inside* a present, listed file costs a full read and is `prov manifest --verify`, run on purpose. A manifest that will not parse yields `ManifestMalformed` and *nothing else* for that directory: with no trustworthy row set, reporting every photograph as unlisted would bury the finding that matters. The rebuild autofix is `Judgment`, never `Derived` — accepting the directory as it stands writes a *lost* file out of the record as though the loss were intended, the same judgment `RestampFixity` declines to make unattended — and it carries only the node, recomputing at apply time, because the authority is a directory and a directory can change between the check and the repair. `rename` moves the node and its manifest and **leaves the archive put** (relocating ten thousand files because their index was renamed is slow at best and destructive if it half-finishes), re-spelling `root` and re-stamping the pin in the same change set; `rm` likewise removes the description and leaves the described. The residue, stated: after such a rename nothing beside the directory names its node, so the `<dir>.<ext>` convention is a probe and the `manifest`→`root` chain is the truth — `attach --manifest` pays for a census before minting (a second manifest over one archive is two rival records), while `attach`'s per-file check stays a probe, since it runs once per file under `--all` and its residue is duplicated bookkeeping rather than a contradiction. Covered files are **not** in the reachable set, so a history capture parks the *manifest*, not the photographs: damage stays detectable (every hash is on record) and is not undoable from `history/blobs/` — the alternative being to duplicate the whole archive into the store on first capture, a worse default for exactly the workspaces this exists for |
| Not re-reading and not re-hashing | `memo`, `fixity` | ✅ two memories, deliberately different in lifetime and in what they are trusted for. **Read memo** (`Workspace::read_scope`): a per-*operation* memo, so composed passes cost one read per document rather than one per pass — `check` is nine passes over one graph and read every document **three** times; it now reads each once (`check_reads_each_document_once`). Explicit rather than automatic (a memo that switched itself on would be a cache, and a cache needs invalidation); scopes nest; `commit` forgets what its change set touched; the outermost exit drops everything, so no staleness window survives the call. **Fixity cache** (`FixityCache`): device-local, persistent, keyed by workspace-relative path and validated on mtime **and** length, so a capture reads and hashes only the files whose stat moved — a capture over an unchanged workspace does no reads at all. Pure data (`encode`/`decode`, hand-framed with magic + version + stored root, every failure decoding to *nothing remembered*); prov does no I/O for it and has no notion of a location outside the workspace, so where the bytes live is the host's business — `prov-cli` keeps them under the user's cache directory (`--cache-dir` > `PROV_CACHE_DIR` > `XDG_CACHE_HOME` > platform default; `prov cache [--clear]` shows or drops one; `--no-cache` opts out). Not in the workspace (a binary cache is not part of what the archive says about itself, and two devices would fight over a file describing *this* device's disk) and **not** in `prov.yaml` (the config is device-independent; a path baked there is wrong on the second machine). The load-bearing rule is about **who may ask**: a remembered digest may decide what to do and may land somewhere content-addressed, but may never establish or verify a fixity baseline. So `history_capture` uses it — and only when the blob it names is *already parked*, which is what bounds a stale entry to "an event that misdescribes an instant" and never to bytes stored under an address that is not their digest — while `fixity_findings` and `restamp_fixity` never touch it, because bit-rot is by construction the change a stat cannot see. Disposable in DESIGN §5's sense: deleting it costs one slow capture and nothing else |
| Storage adapter + executor | `fs`, `exec` | ✅ `StdFs` + dependency-free `block_on`. The port **declares durability capabilities** (`Capabilities { atomic_replace, sync_guarantee, native_transactions }`, pessimistic by default) so the crash-safety layer adapts to each backend rather than assuming — `StdFs` reports `LOCAL_FS`, and the OPFS/IndexedDB adapters will report their own (IndexedDB's `native_transactions` earning it a bypass of the journal). `sync_guarantee` is three-valued (`SyncGuarantee { None, Ordered, Durable }`) because a backend that can order writes without draining a device's write cache — `F_BARRIERFSYNC` on Apple — has a real guarantee to offer that a boolean could only round to a lie in one direction or a needless cost in the other. `sync(path, need)` (flushes exactly `path`, file *or* directory, to the requested `Durability`) and `write_atomic` (write-temp → sync temp `Ordered` → rename → sync the *directory* `Durable`, degrading honestly where atomic rename is absent) are the two primitives; every staged write — `FileOp::Write` and `FileOp::CopyFrom` alike — lands through `write_atomic` |
| Crash-atomic change sets + journal | `change`, `journal` | ✅ **error atomicity** (in-memory unwind on any failed write) **and crash atomicity**. Per file: `write_atomic` means no document is caught half-written even by a power cut. For the whole set: a checksummed write-ahead journal — a single transient root dotfile (`.prov-journal`), written and flushed *before* any document (the commit point), removed on success — that `recover` (run by `check`) rolls forward idempotently after a crash. An interrupted set therefore always resolves consistent: fully-before on a caught error, fully-after on a crash. A set of **one** op skips the journal entirely, and the ordinary save is one op: a lone op on a backend claiming `atomic_replace` is already indivisible, so there is no multi-file window left for a journal to close, and journaling it would write, flush and delete a second file to restate a promise the op already carries. What that trades away is liveness, not safety — a crash means the op did not happen rather than being rolled forward to having happened — and for a set of one those are the only two states there are. The stale-journal refusal still applies, since a save must not slip past an earlier change recovery has yet to finish. The registry write still rides the same set (§5). One op journals a **payload by reference**: `FileOp::CopyFrom { path, source }` records the source path where `Write` would embed the bytes, so a set that puts a whole captured workspace back costs O(files) of journal instead of duplicating the entire tree into `.prov-journal` at the commit point. Replay stays deterministic only because the source is *required* to be immutable — a content-addressed history blob is that by construction, so replay finds exactly the intended bytes or fails loudly; pointed at a mutable document it would let recovery invent a state the change never intended, so the obligation sits on the caller. Remaining: an IndexedDB backend delegating to its native transaction instead of the journal |
| Recycle bin (recoverable delete) | `mutate`, `relation`, `config` | ✅ a first-class, **reachable** member (a `recycle_bin` pointer relation off the root, discovered by `Workspace::recycle_bin_path` — the registry's anti-`.obsidian/` move, §5/§6), not an app-private folder. `recycle` moves a document (bytes verbatim) into a visible `recyclebin/` and records a tombstone in its self-describing index (validated by `check`); the bytes park under an *unreached* `recyclebin/items/` so §8's orphan check ignores them — which is why `check` verifies each record's parked bytes are still there (`RecycledBytesMissing`), the one pass that looks inside that subtree. `restore` reverses it losslessly (bytes back, parent re-linked, ID re-registered); `empty_bin` is the only hard purge, always explicit. Because `id_storage` defaults to `both`, the ID travels in the document's own frontmatter, so a sync can hand it to a second document while the first sits in the bin — `restore` therefore checks `registration_conflict` in **both** directions (the ID resolving elsewhere, and the path already carrying another ID) and refuses rather than displace one, since only the author can say which document keeps it. `IndexStore::register` also maintains the id↔path bijection under a displacement it did not catch, so a collision that slips through cannot leave the registry naming two paths for one ID. All three are one journaled ChangeSet. Config axis `recycle_bin` (on by default — the safe archival posture — opt-out per workspace); the CLI routes `rm` to the bin unless `--purge`, and adds `restore`/`empty-bin`. An operation run *because* something is already broken reports its effect on integrity as a `CheckDiff` — a `check` before against one after, bucketed **fixed / introduced / pre-existing** — since a bare post-operation list cannot tell the damage it repaired from the damage it caused from the damage it inherited, and only `introduced` is a reason to stop. `check --fix` and `history-restore` both report and exit on it |
| History (captured pre-images) | `history`, `relation`, `config`, `validate` | 🟡 **Phases 0–2** (`docs/history-format.md`, proposal `docs/proposals/history/proposal-history-v3.md`). The safety net for *structural* damage an external sync transport does — a rename/move/delete touches several files at once, and a transport reconciling bytes has no idea about the graph. A fourth reachable pointer relation (`history`) off the root names a visible `history/` store — and because that pointer is one line in one mutable file, discovery falls back to the **conventional path** (`history/index.<ext>`, on disk) and nowhere else, so a transport that mangles the root cannot take an intact safety net out of prov's view while a shell and `cp` can still recover from it: one **immutable event document per capture**, holding a *full manifest* of the capture set (`path → (id?, hash)`), plus content-addressed pre-image bytes under an *unreached* `blobs/` (bare hex — never the `sha256:` prefix, which is hostile to Windows and sync clients). Full manifests rather than deltas is the load-bearing choice: every event is self-contained, so `parent` is display metadata nothing computes through, removals need no bookkeeping, and a foreign event is restorable even if its ancestors never arrived (OCFL converged on the same shape from the archival side, which is why OCFL is settled as an **export** format and never the live store — a fork is unrepresentable in contiguous `v1..vN`). The store's own documents are authored through all three of the workspace's axes — extension, prose grammar and frontmatter carrier — so an HTML workspace's store is HTML with the metadata in the same `<script>` island the rest of it uses, rather than a `.html` file holding a `;;;` fence and a literal `# History` that prov reads back fine and no other tool does; the store index's prose *names* the carrier and language a reader is looking at and spells out recovery by hand (the `blobs/ab/cdef…` split, and that a blob **is** the file), which is what makes "readable without prov" a property of the artifact rather than of a document kept elsewhere. The store is **append-only at the filesystem level**: a capture only *adds* files, and added-file/added-file is the one merge case git/Dropbox/Syncthing/iCloud all handle without conflict. Events are date-sharded (`events/<YYYY>/<MM>/`) so the mutable surface is "this month" not "forever", and `id → path` is a pure function (the date is repeated in the id) so an event resolves with every index destroyed. The per-shard indexes are the only mutable files and are a **rebuildable cache** — a mangled one is `HistoryIndexStale` with a per-shard autofix, not data loss. Blobs deliberately **do not** ride the journaled ChangeSet (the journal embeds contents, so a genesis capture would duplicate the workspace into `.prov-journal`); they go through `write_atomic`, safe because a content-addressed write is idempotent. Config axis `history: off \| manual`, default **off** — and *leave it off when the transport is git*, which already keeps every pre-image. `off` gates capture only; read/recovery verbs work regardless, since recovery must never be gated behind re-enabling a setting. The two read queries are shipped alongside: `history-show` prints an event's manifest (which *is* the effective state — no reconstruction) with each row marked when its blob is not on disk, since a manifest and its blobs sync independently and a **half-synced event is ordinary in-flight state, not damage**; that same missing-blob set is what a restore reports rather than computing its own. `history-log` is the payoff for the manifest's `id` column — a per-document lineage derived by pulling one row out of each manifest, **rename-robust** in a way no path-keyed store can be (a move is one document that changed path, not two unrelated lineages), deduped on the whole row so a rename's identical bytes cannot hide it, with a path fallback for the id-less documents (config, registry, bin index, attachment payloads) that a transport disproportionately damages — and that fallback names the stronger id query when the manifests recorded one. `history-restore` writes a captured state back, and an event is a **consistent cut** — a bad merge that broke a renamed file *and* its parent's child list hashed both in the same capture, so restoring the whole event is what actually undoes it, where a scope (paths, `--id`) is content recovery that can *reintroduce* the inconsistency if the graph is what broke. Additive by default (bad-merge damage is characteristically additive — a `.sync-conflict` copy, a rename-vs-rename landing both names — and none of it goes away by writing captured bytes over the top); `--exact` also removes the *reachable* files the manifest lacks, which cannot be scoped, lists what it would delete, and asks first. Guards run before a byte moves — blob presence per row, and `registration_conflict` in both directions, discounting a collision the restore itself resolves (which is what lets `--exact` undo a move without `--force`). Writes ride the journaled ChangeSet as `FileOp::CopyFrom`, so putting a whole workspace back costs O(file count) of journal rather than a second copy of every byte — sound precisely because a blob's path *is* the digest of its contents. Never writes inside `history/`, and a restored root that declares no pointer gets one back rather than stranding the store. Ends on a `CheckDiff` (row above): *introduced* drives the exit code. `check` validates the store through five findings: `HistoryStoreUnlinked` (a store at the conventional path that the root has stopped declaring — reported first, autofixed by re-declaring the pointer, and gated on the `history` axis because a leftover directory under `off` is not a loss; without it that failure is *completely* silent, since an undiscovered store is a subtree the walk never enters and so never reports on, orphans included), `HistoryIndexStale` (autofix, per-shard) plus the two blob findings, both falling out of one mark-and-sweep over the manifests — `HistoryBlobMissing` raised per *hash* (one lost blob is one thing to put back, and which events are thereby incomplete is `history-show`'s question), `HistoryBlobOrphaned` per sweep — plus `Unreadable`, reused unchanged from the general walk, for an event-shaped file whose document fails to load or parse. All are **diagnosis only**: nothing can synthesize bytes or a document, and the real repair retires the finding on its own, while collecting an orphan is destruction, which autofix never is. The missing-blob wording admits both of its causes — real loss and a sync still in flight — because a finding that cries corruption at a routine, self-resolving state is one users learn to ignore. `HistoryBlobOrphaned` is suppressed for the whole sweep while any event is `Unreadable`: the mark half of the mark-and-sweep is then known incomplete, and the unreadable event's own manifest might be the only thing still claiming one of those bytes. `history-prune (--keep <n> \| --before <date>)` is delete + GC and nothing else — full manifests mean dropping an event cannot make another unreadable, where a delta log would have had to rewrite an "immutable" event to re-anchor the fold. Exactly one bound, no default (a verb that deletes bytes must not do so because a flag was forgotten); **also refused if any event in the store is unreadable**, naming the file(s) — the survivors-only `referenced` set that bounds the blob sweep is exactly what an unreadable event makes an unsafe basis for deletion. Events dropped *before* blobs are collected (a crash mid-prune leaves an orphan, the benign direction; the reverse leaves manifests naming bytes that are gone), and blobs collected outside the change set — capture's asymmetry with the reason inverted, since a staged removal buffers what it deletes to be able to undo it. Indexes are rewritten only when their content would change, because every one is a file some transport carries; an emptied shard loses its index, and an event-less directory is not a shard anywhere it is rendered. `history-forget <path|id>` is the deliberate-destruction counterpart, and full manifests make it a column lookup rather than a fold: every hash the subject ever had, minus every hash anything *else* ever had. Two limits stated rather than hidden — it destroys only bytes nothing else names (content addressing means forgetting one document cannot reach into another's history, a safety property and a limit at once), and it destroys **bytes, not the record** (events are immutable, so every manifest still names the path, the id and the hash; if the *name* must go, this is not that tool). Refuses a document still in the *capture set* — the next capture would park it again — unless forced; refuses independently, for the same reason as prune, if any event in the store is unreadable — the `others` set that limit rests on would be incomplete. The destroyed hashes are tombstoned in a whole-file `history/forgotten.<ext>`, linked from the store index because `history/` is orphan-scanned and an unlinked record of what was destroyed would be reported as litter; that record is what lets `check` skip `HistoryBlobMissing` for it (a `check` that never came back to clean after a legitimate forget is how a user learns to stop reading `check`) while `show`/`restore` say *forgotten* rather than *lost*. Write-ahead: tombstone committed before the bytes are freed, blobs deleted outside the change set as in prune. Shipped: `history-capture`/`history-list`/`history-show`/`history-cat`/`history-diff`/`history-log`/`history-restore`/`history-prune`/`history-forget` + the blob findings + transport-simulation tests. `history-cat <id> <target>` is the store's missing primitive — a manifest row addresses its blob directly, so writing one captured file's pre-image to stdout is a lookup whose cost does not grow with the captures since, and it is what makes the store work with tools that are not prov (`prov history-cat E notes.md | diff - notes.md`). It follows `history-log`'s subject rule, so an id reaches a document that has since moved and a path is matched against what the *manifest* recorded — which is why a deleted document's bytes still come back. Absence stays three distinct answers (no such row / bytes not here yet / deliberately forgotten) rather than one, the same distinction `show` marks per row, and every refusal writes nothing to stdout and exits non-zero so a pipeline cannot quietly diff against an empty file. `history-diff [<a>] [<b>] [-- <path>...]` spends the full-manifest choice directly: two complete manifests compare without reading anything between them, so any two events are comparable even across devices and lines. It reports changed/moved/added/removed, **pairing moves before anything is called added or removed** by the same one-to-one rule `history-log` infers a rename with — a directory rename is one intention, not four hundred rows — and `--patch` adds a unified diff of changed *text* files only, since an added file's whole content is `history-cat`'s job and a genesis diff would otherwise print the workspace. The diff crate is a **prov-cli dependency only**: comparing manifests is hash comparison and needs nothing, and rendering a patch is presentation the library should not push onto every downstream consumer. ⏳ the documented git-hook and watch-script capture recipes; unscheduled `history-export --ocfl` via `mocfl` (decided in principle, `prov-cli`-only dep) |
| Link text + path arithmetic | `link` | ✅ labeled links, resolve/relative, lexical normalize |
| Single-document edits | `edit` | ✅ format-preserving `set`/`unset` over text; `prov edit` opens `$EDITOR` and, on a real content change, calls `Workspace::record_content_update` — one crash-safe write that restamps the fixity checksum (under `all`) and stamps the configured `updated` field (empty = off) with an RFC 3339 UTC instant the CLI supplies (the library stays clockless, DESIGN §2; the CLI's `now_rfc3339` is the workspace's one clock, microsecond-precise and fixed-width so a value is orderable as well as readable). Gated on actual change (byte-compared across the editor), so an open-and-quit stamps nothing |
| Multi-format embedded metadata | `document`/`meta` | ✅ read side (fig 2.1's `detect` + `split` *are* the fence layer); ⏳ format-preserving writes ride the mutation port |
| serde / fig backend split | — | ⏳ planned (feature gates) |
| Filesystem intake (`mirror` import) | `intake` | ✅ `plan_mirror` → `StructurePlan` (previewable) → `apply_plan` folds a directory tree into the containment tree, synthesizing folder-notes for bare dirs and reusing `create`/`adopt`; drives `init --adopt mirror` (`docs/init-adoption.md`, Phase 2). The `StructureSource` *trait* (frontmatter/hybrid sources) is deferred until a second source needs it |
| Route addressing (`mkdir -p` for containment) | `route` | ✅ `plan_route` → `RoutePlan` (previewable) → `apply_route` walks a route (`Daily/2026/2026-07` — each segment the *title* of a child of the last) from a start document and synthesizes the segments that don't resolve, reusing `intake`'s `SynthNode` + `create_titled`. Drives `prov new --under <route> -p [--layout nested\|flat]`, whose point is the recurring-entry workflow (a daily note whose month index doesn't exist yet on the 1st). Vocabulary-neutral by construction — no date, no "daily", nothing diaryx (§2): the shell supplies the policy, prov the one part a shell can't express. `Layout` governs *file placement only*, never the graph. Resolution is bounded (only the children of nodes on the route are read) and does **not** trip §8's alias-spanning hazard, since it descends from a known node rather than needing every title up front |
| Declarative views | `prov-views`/`config`/CLI | ✅ the spine is one way through the workspace (single-parent, every document in exactly one place); a **view** is a second, and the same document may appear under several groups — precisely what the spine cannot do. Declared as a top-level `views:` axis (`group`/`by`/`under`/`nest`/`label`/`icon`), so every prov tool reads the same lenses rather than each app namespacing its own block (the promotion out of diaryx's `diaryx.views`, whose real cost was that `diagnose` passes over an app block in silence — unlinted, and legible only to the app that agreed with itself where to look). **There is deliberately no `date` grouping**: `group:` names a field or an ordered chain (first non-empty wins) and `by:` is a *prefix cut over ISO-8601 text*, so a date view is those two aimed at date fields and the chain (`date_of_document` → `created` → `updated`) becomes a declaration the workspace makes rather than three field names the tool blesses — which is what lets a workspace filing by `taken_on` say so, and what stopped three programs each carrying their own copy of the chain. The cut *validates* rather than slicing (`banana` at year grain is not the group `bana`, `20264` is not 2026); a value that doesn't parse falls to the **ungrouped** bucket, reported rather than dropped, because a view whose entries have all quietly stopped grouping is indistinguishable from an empty archive and the difference is the whole diagnosis. `under:` is a **traversal** of the spanning relation, not a path filter — it survives a rename, a move and a retitle, where `path starts-with "Daily/"` does not and a title match finds `Trips/2026` just as happily; an anchor that names nothing is an error, not an empty result (and resolving is not arriving — a path anchor always resolves, so the walk's own verdict on the anchor node is what settles it). `nest:` stays independent of `by:` (MoReq2010 §1.4.5's classification-vs-aggregation split: grouping is a reading decision, filing a writing one, and a picker that reads like a display setting must not change where tomorrow's entry lands). prov **reads views and never acts on one** — a view has no invariant, so no `check` finding can come from a wrong one, which is why this is a crate beside prov rather than a feature inside it; `prov-views` depends only on `prov-graph`, so a view engine is structurally unable to write to the workspace it reads. A **grain** (`by:`/`nest:`) is likewise not a calendar but any *coarsening* — `year`/`month`/`day` cut ISO-8601 text (validating, so `banana` at year grain is not the group `bana` and `20264` is not 2026), `initial`/`{initial: n}` cut the first letters for an A–Z index (by **character**, so `Ålesund` is one letter and not a panicking byte slice, and upper-cased, since an index filing `ada` apart from `Ada` is not an index). New grains are added by the same rule as `where:` predicates — a concrete lens that cannot otherwise be said — which is why a numeric `bucket` is absent: unasked for, and its keys sort lexically as `0, 10, 100, 20`, needing grain-aware group ordering that is really the deferred `sort:` axis. **`by` and `nest` generalize unequally, and that asymmetry is prov's invariant showing through**: `by` is a *reading* operation with nothing to keep, so any coarsening will do, while `nest` *writes* into the single-parent spanning relation and so demands two more things — that the grain **chains** (each coarser key determined by the finer one: `2026-07-24` → `2026-07` → `2026`, `Ada` → `Ad` → `A`, which an arbitrary sequence of coarsenings does not), and that the grouping value is **single-valued** (a document listing two people has two homes and nothing may choose between them). Both halves ship: `ViewSpec::nest_route` returns the index *titles* to file under — exactly what `route.rs` addressing takes, so a frontend hands them to `plan_route` and never assembles a path — or `None` when the document cannot be filed; and `nest:` over a field declared `type: seq` is a `ConfigIssue::NestNotSingleValued`, diagnosis-only because its two repairs (drop the `nest`, or stop declaring the field a `seq`) say different things about the workspace and the second changes how every *other* consumer reads that field. It fires only when `fields` and `views` share a config surface, `diagnose` being per-surface. **`where:`** narrows what scope reached — `has`/`equals` plus `not`/`any-of`/`all-of`, a multi-key mapping being an implicit and — which is enough for the publishing-audience filtering still entangled in `diaryx_render` and deliberately not an expression language (formulas are the point of no return: a closed set of named predicates grows one member at a time with a reason each, a grammar cannot be taken back). It is a *separate key* from `under:` because the two fail differently — an anchor naming nothing is a broken view, a condition matching nothing is an ordinary empty answer. Three semantics resolve toward saying so rather than guessing, each having two opposite silent readings: an empty or unreadable `where:` is a finding rather than a filter (guessing publishes a workspace or hides it), `any-of: []` matches nothing while `all-of: []` matches everything, and `has` means present *and non-empty*. Running a view is two steps kept apart: **select** (I/O — scope then conditions, returning a flat deduplicated set) and **group** (a pure function, no filesystem, so every grouping question is testable without a fixture vault); a `RowSet` borrows its `Selection`, so one selection groups several ways at once and the *document* count stays distinct from the *row* count — one document under two of a multi-valued field's groups is not two documents. `prov views` lists, `prov views <name>` executes. ⏳ no `sort:` axis (ordering is ascending-lexical; a date reader wants newest-first) and no formulas — both deferred as the places the format grows teeth |
| Gated exports (what may leave) | `prov-exports`/`config`/CLI | ✅ everything else prov reads is open by default — a view with no `under:` covers the whole workspace, the spanning walk reaches everything. An **export** is the boundary where that flips: a named, **closed-by-default** set of documents that may *leave* the workspace, declared as a top-level `exports:` axis (`label`/`gate`/`view`) beside `views:`. A document is in an export only if the document *itself* declares the export's gate value (`gate: { field: audience, value: family }`), and a document that declares nothing leaves in nothing — the declaration lives in the document, so it travels with the file and still means what it meant, where view membership is a property of the workspace and cannot be. **A gate is not a kind of filter a view could grow; it is a position**: the domain every view runs over once the corpus leaves. The two also invert prov-views' whole rationale — a view has no invariant (a wrong view is a wrong grouping you fix in the picker), while an export is *all* invariant (a wrong export is a file in hands it was never meant for), which is why the valve lives in its own crate rather than as a `where:` idiom: **an export's document set is a subset of its gate's admitted set, whatever the named view says.** `plan` enforces that structurally — entries are seeded from what the gate admits and the view's selection is only ever `retain`ed against, never iterated to add — and everything else fails closed to match: an entry `parse` cannot make a gate of is not an export (it exports nothing; the lint keeps the reason, under the same parser/linter parity test prov-views uses — the silence matters more here, since a gate typo that holds documents back is one nobody notices), and a named view that is unknown or broken is an *error*, never a fall-back to the gate's whole set, because a bound that was written down and cannot be applied must not widen. **A gate is exactly one field and one value, matched exactly after trimming** — not an any-of list, not a condition: the property that makes an export auditable is that "does this document leave?" is answerable by reading one field on that one document, and every richer shape trades it away (the same one-member-at-a-time discipline as `where:` predicates and grains). Exact matching is the fail-closed direction — `audience: Family` under a `value: family` gate is *withheld*, because casing drift is the closed-vocabulary lint's finding to report (`fields.audience.vocabulary` + `UnknownTerm`/`TermNearMiss`), not the gate's to forgive; the gate field wants a closed vocabulary for exactly this reason. Deliberately **no `index:` front-page key** (a rendering concern — an OCFL or copy-out consumer has no front page; the publish layer that wants one owns a render to declare it in, which is diaryx's site layer, whose shape this generalizes — MoReq2010 keeping access control as its own service beside classification and aggregation). `compose` is pure and `plan` does the I/O (`select`/`group`'s split, reused because an invariant should live in code with nothing to mock); the crate depends on `prov-graph` + `prov-views` only, so the layer that decides what may leave is structurally unable to alter what it judges. `diagnose` translates the crate's judgment per surface, plus one cross-key check — an export arranged by a view its surface does not declare, spelled as an `InvalidValue` whose accepted spellings are the declared view names, so the typo is caught at author time rather than as a refusal at publish time. `prov exports` lists, `prov exports <name>` previews all three sides of the boundary (what leaves; the gate-admitted set the view scoped out, listed in full, since "I tagged it and it isn't there" is unexplainable from the file alone; a count of what the gate held back) and moves nothing. ⏳ every consumer of a plan — publish, copy-out, partial sync, `history-export --ocfl` — is deliberately downstream; an any-of gate and a gate-value-is-a-known-term check in `validate` (the empty-export typo, catchable once vocabulary stores are loaded) are the named next candidates |
| Vocabulary self-description (`prov/1` spec) | `config`/`relation`/`workspace` | ✅ the relation vocabulary is **declared in frontmatter**, not just a preset: `spanning` + `relations.<name>.{cardinality,inverse,means}` in the root's `prov:` block, parsed by `WorkspaceConfig::apply` and built by `RelationSet::from_config` (falling back to the diaryx preset when nothing is declared, so a minimal vault spells out nothing). `means` is a tier-3 human gloss, carried never read (§2). The single-parent invariant is linted (`SpanningNotSingleParent`). The five-rule bootstrap kernel + placement rules live in `docs/spec.md` |
| Controlled-vocabulary fields (tags, audiences) | `config`/`vocabulary`/`validate` | ✅ a `fields: { <field>: { values: open\|closed, vocabulary: <pointer>, reify } }` block turns a carried field into a *resolvable, checked* reference (§2: consistency = resolvability). A vocabulary is a **whole-file** store (a self-describing node, `Vocabulary::from_meta`) with a `terms:` map; prov reasons about term keys, each `id`, and `retired`, carrying the rest (a diaryx audience's gate/theme payload). `check` grows `vocabulary_findings` over the reachable set: `UnknownTerm` (closed) / `TermNearMiss` (open, reusing the config linter's edit-distance via `textdist`). Diagnosis-only |
| Whole-file record stores | `document`/`index`/`mutate`/`validate` | ✅ registry, recycle-bin index, and flat vocabularies must be whole-file config documents — `require_whole_file` refuses a markdown carrier at load, `check` reports it (`MalformedStore`). Prose routes to *reified* term nodes instead (§5) |
| Whole-tree backup | CLI (`prov-cli`, not the library) | ✅ `prov backup --to <path> [--zip]` — a plain, opaque, bytes-verbatim copy of the whole workspace tree (hidden files and a transient `.prov-journal` included), deliberately **outside the graph**: no pointer relation, no manifest, no config axis, no dedup. Refuses a destination that resolves inside the workspace root (self-copy) or an existing non-empty directory/file. `--zip` writes a hand-rolled, dependency-free store-only ZIP (no crate added, mirroring `fixity`'s hand-rolled SHA-256); a symlink is recreated as a symlink in the directory form, skipped with a warning in the zip form |

## 10. Open questions

1. ~~**Does the ID registry ever need to survive without its documents?**~~
   **Answered: yes, minimally — tombstones, not history.** Deleting a document
   retires its ID to a tombstone (`id: null` in the snapshot): the ID stops
   resolving but is never forgotten, so mint-by-rejection can never reissue it
   and a dangling `prov:` reference stays *diagnosable* ("that document was
   deleted" vs "never issued here"). This is cheaper than an append-only log —
   the registry stays a sorted, diff-friendly snapshot — while still refusing to
   let an ID silently change meaning. Full history/event-log stores remain
   possible behind `IndexStore` (e.g. for sync), but the file-backed default
   does not need them.
2. ~~**How first-class is the filesystem `StructureSource`**~~ **Answered: a
   genuine peer, realized concretely first.** The filesystem is a real structure
   source — `init --adopt mirror` folds a whole directory tree into the
   containment tree (synthesizing folder-notes), not merely a flat convenience
   (`intake.rs`, `docs/init-adoption.md` Phase 2). But it landed as concrete
   methods (`plan_mirror`/`apply_plan`), not a `StructureSource` trait: with one
   implementation the trait would be premature. The abstraction (peer
   frontmatter/hybrid sources) is deferred until a second source demands it — so
   the answer is "first-class in capability, un-abstracted until it pays for
   itself."
3. **Is the internal prov ID ever unified with the published permalink**, or
   do they stay two layers (internal minted opaque ID; external ARK permalink)?
   Model B keeps them separable; nothing yet forces them together.
