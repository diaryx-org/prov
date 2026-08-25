---
part_of: '[prov](/README.md)'
---
# Next steps — working notes

Deferred items from the identity / wikilink / link-syntax work, so we don't lose
them. Not curated design (that's `DESIGN.md`); this is a scratch backlog.

## What the property tests turned up

`proptest` is a dev-dependency, and five modules carry a `mod properties` beside
their example tests: `link` (reference round-trips, path arithmetic), `mutate`
(sequences of verbs against `check`), `index` (the id↔path bijection),
`identity` (the check character), and `textdist` (the metric axioms). Each
states a claim the prose already made and quantifies it. A sixth,
`fixity::cache` (the hand-rolled binary frame), went with the cache itself.

Five defects found, **all fixed**:

- **Both canonical link styles failed `resolve ∘ format = id`** — not just
  `plain_canonical`, which is what this file had said for months. `PathStyle` is
  now `root | relative`; see the link-syntax section below.

- **`delete`/`recycle` on a separated node's body** stranded its `content:`
  pointer silently. Both now refuse a body as a subject and name the node
  instead; `--force` proceeds and reports the pointer it stranded, which the
  dangler census could never see (`content` is neither a relation nor a body
  link). `maintain::content_owner` answers "whose body is this", bounded to the
  body's own directory — every body prov authors sits beside its node.

- **A displaced id was forgotten rather than tombstoned,** so `is_known` — the
  mint-by-rejection predicate — went true → false and the id became reissuable
  while the displaced document still spelled it. Two operations reached it.
  `FileIndex::register` now retires what it displaces.

- **A re-registered id was held live *and* tombstoned** (the `restore`-from-bin
  path), disagreeing with what `render` writes. `register` now clears the
  tombstone — safe only because of the fix above, without which a restored id
  that was later displaced would have had no tombstone left. The two had to land
  together; the first attempt at this one alone regressed the other, and the
  property caught it in three operations.

- **`recycle` resurrected a parentless document.** Found while fixing the
  separated-body guard, and older than it: the bin bootstrap wrote its pointer
  into `spanning_root(subject)`, which for a document with no parent *is the
  subject*, so the change set renamed the file into the bin and then wrote it
  straight back — `prov rm` on an orphan left it in place carrying a
  `recycle_bin:` key, beside a copy in the bin. Orphans are exactly what a user
  bins, since `check` reports them as the onboarding signal.

Two behaviours the laws had to be told about, both correct as they stand:
`delete` reports rather than rewrites the references it strands (so the law is
"introduces nothing it did not report", which also tests that the report is
*complete*), and `duplicate` leaves a parentless copy unattached by its own
documented contract.

The `mutate` law is still scoped to the spanning tree's **nodes** rather than
every file on disk. With the separated-body guard in place a body subject is now
refused rather than mishandled, so widening the domain is mostly a question of
what `rename` and `retitle` owe a file that is content but not a node — worth
trying, and likely to surface a `rename`-shaped sibling of the `delete` finding.

Not yet done: `journal`/`change` have no properties, and they are the obvious
next site — replay idempotence, and "an interrupted set resolves fully-before or
fully-after" quantified over generated change sets rather than the hand-placed
`FailAtWrite` fixtures.

## Identity & backlinks

- **Step 4 — gated malformed-id autofix.** The one document-repairing heal: when
  the census finds a malformed `prov:<id>` near an edge the registry resolves
  uniquely, offer to restore it. Directional invariant: forward links are ground
  truth; the index heals *toward* them; it rewrites a document *only* for a
  dangling id it can resolve from its own record. Everything else: report.

- **Route C — persist the backlink map.** Where "id-backlink registration"
  finally has a home. Store the census-derived backlink map in the index's
  *derived* section (structurally separate from the authoritative `id → path`
  registry, per DESIGN §5), plus a `Reconciled` report (backlink added/dropped,
  out-of-band id-link registered). The census is its self-heal.

- **Frontmatter id-shadow (DESIGN §5 escape hatch).** Stamp a forward id-link's
  id into the *source's* frontmatter so the forward-link truth is complete in the
  nodes: backlinks become fully derivable, the §4 out-of-band hazard becomes
  recoverable, and self-healing goes total — no central authoritative residue.
  The thesis-aligned alternative to Route C.

- **Authoring `[[prov:id]]` wikilinks.** The write side of the original
  idea #2: mint via `Trigger::Link`, drop the target into body prose. Closes the
  loop — the whole census/rename/backlink stack was built to support this.

## Autofix (DESIGN §8 — the sleeper feature)

Lives in **`prov/src/remedy.rs`**, carved out of `validate.rs`: a finding says
what is wrong, a remedy says what could be done about it, and only the second
knows how to change a document. `validate.rs` is now a findings view and nothing
else — `Finding`, `CheckDiff`, `check` and its eight sub-passes — which is the
same line that keeps `graph` ignorant of `Finding`, drawn one layer up. It also
retires the precondition `graph/mod.rs` had recorded for a `Graph<FS>`
carve-out; that split is unblocked, and still unwarranted (a `Graph` handle
would have to thread through every mutation verb, and each of them reads and
writes in the same breath).

Principle **restated**: a repair edits structure — frontmatter, or a span *twig's
own parser* reported as a link — and never ordinary prose; and it never deletes a
file. The old formulation ("metadata only, never body prose") was a proxy for the
real objection, which is that a `[[…]]` may be code (`[[inf] * n for _ in
range(m)]]`) and a lexical scan cannot tell. `link::parsed_link_spans` can, so
`[label](target)` in prose is now repairable while `[[…]]` stays diagnosis-only —
twig has no wikilink concept, so a wikilink span is lexical either way.

- ✅ **Remedies replace the one-answer signature.** `suggest_fix`'s
  `&Finding -> Option<Fix>` could hold one repair, so a finding with two
  defensible ones got none — and the split that created tracked how settled each
  repair was, not any property of findings. `remedies() -> Vec<Remedy>` is the
  general surface; `suggest_fix` survives as a view over it (the first
  non-destructive remedy). Each remedy carries a `RemedyKind` slug and a
  `Warrant`: `Derived` (a pure function of an authority — safe unattended),
  `Judgment` (rivals exist), `Destructive` (removes something authored).
- ✅ **Contested containment**, both ends of it — `DuplicateContainment` and a
  `MissingInverse` whose child claims another parent — now offer (a) make this the
  real parent (delegating to `reparent`, which repoints the child *and* drops the
  rival's entry in one change set) and (b) drop this container's entry. Option
  (c), demoting a spanning link to an overlay relation, is still unbuilt.
- ✅ **Broken frontmatter link** — a retarget per directory-local near-match, plus
  removal. Body links too, under the twig rule above.
- ✅ **`Orphan` is a remedy like any other**, offering every container above it
  nearest-first, which retires the CLI's hardcoded "adopt under the root" — that
  literal existed only because a `-> Option<Fix>` had nowhere to ask.
- ✅ **Non-interactive `--fix mechanical`** applies every `Derived` remedy and
  prompts for nothing. Bare `--fix` is still interactive (EOF → skip).
- ✅ Also gained remedies: `CaseMismatch` (`Derived` — the on-disk name is in the
  finding), `MalformedId`/`DanglingId`, `AmbiguousAlias` (one per candidate),
  `IdMismatch` (both sides — "trust the document" was already implemented as
  `Fix::RegisterId` and simply unreachable), `UnknownTerm`/`TermNearMiss`
  (respell, or widen the vocabulary — never over a *retired* term, which would
  un-retire it and destroy its id and gloss), and `ConfigIssue`.

Still open here:

- **Restore-from-backup for `FixityMismatch`.** Restoring is the arm prov
  cannot decide, and a version-control store beside the workspace can actually
  perform it — but prov no longer knows such a store exists, so the honest
  shape is a finding message that names the situation rather than a `Fix` that
  shells out to a tool prov cannot see. Until then `FixityMismatch` offers only
  the re-stamp, marked `Judgment`.
- **`DemoteEntry`** — spanning → overlay, contested containment's third answer.
- **`MalformedStore`** — migrating a markdown store to a whole-file carrier
  creates a file and rewrites a pointer, so it is a mutation verb, not a fix.
- **The `link_strings` index skew.** `Value::link_strings` filters non-string
  sequence items, so a position taken from it is not the fig index —
  `[a, 3, b]` yields `["a", "b"]`, and `remove_item(key, 1)` would delete `3`.
  `entry_index` + `remove_item` carry that skew at three sites
  (`delete`/`reparent`/`recycle`); harmless while relation sequences hold only
  strings. The new `written_entry_index` enumerates the raw sequence instead.
- **A persisted fix policy.** "All of this kind" lasts one run. A `fix:` block in
  the workspace config mapping finding kind → remedy kind would make a choice
  durable, in the same self-describing way every other knob works.

## Body parsing (`twig`)

The library prov was waiting on to parse file bodies now exists:
[`twig`](https://github.com/diaryx-org/twig), a sister Zig-backed project
(document formats, the way `fig` is for config formats). Wired in as a path
dependency for now (`../twig/bindings/rust/twig` from the workspace root) —
switch to a published version once `twig`'s Rust bindings have proven out.

- ✅ **`content.rs` + `ContentFormat`.** `ContentFormat::from_extension`
  (`.md`/`.markdown` → Markdown, `.dj`/`.djot` → Djot) needs no feature; it's
  the type the deferred `content_format` config knob (below) will store.
- ✅ **`content` feature — real FFI, both `render_html` and `code_spans`.**
  `twig`'s C ABI gained `twig_document_code_spans` alongside
  `twig_document_render_html` (a `TwigSpan{start,end}` array, one entry per
  `verbatim`/`code_block`/`raw_inline`/`raw_block` AST node —
  `twig/src/c_abi.zig`, header at `twig/bindings/c/include/twig.h`), and its
  Rust bindings a matching `Document::code_spans() -> Vec<Range<usize>>`
  (`twig/bindings/rust/twig/src/lib.rs`). prov's `content::render_html`/
  `code_spans` are direct calls into that — no subprocess. `prov render
  <file>` (prov-cli, same feature) exercises rendering end-to-end.
- ✅ **Wired into `census`/`check`/rename — and it had to be more than a
  post-filter.** `link::scan_wikilinks(path, body)` is the one entry point
  `validate.rs`'s `walk` and `mutate.rs`'s two rename-time body-rewrite
  helpers call (never `parse_wikilinks` directly). A real vault turned up why
  a simple "filter matches that overlap a code span" post-filter
  (`exclude_code_spans`, kept as a narrower utility with its caveat spelled
  out) can't do this alone: `parse_wikilinks`'s greedy "next `]]` wins, code
  or not" scan lets one stray `[[` inside a fenced Python block
  (`[[float('inf')] * width for _ in range(m + 1)]`) eat every `]]` *after*
  it in the document — including a real `[[gone.md]]` further down — merging
  them into one bogus match that swallows the real link whole before any
  span-overlap filter ever sees it separately. `scan_wikilinks` fixes this at
  the source: it treats each code span as opaque *before* scanning, running
  `parse_wikilinks` independently on each prose run between (and around) code
  spans and stitching the results back into `body`-relative spans, so a
  code-block bracket can never be in the same scan as prose that follows it.
  `validate::tests::check_does_not_flag_python_list_comprehensions_in_a_code_
  block_as_broken_links` reproduces the real report life-sized. No config
  knob was added — it's automatic whenever `content` is compiled in and the
  extension is recognized, degrading silently to the old unfiltered scan
  otherwise (feature off, unrecognized extension, or a twig failure). Still
  not done: needs a `prov-cli` rebuilt with `--features content` to
  actually take effect — not a default feature yet, since it pulls in the
  path-dependent `twig` (no released version to depend on by default).
  Whether it should become default once `twig` is published is open.
- ✅ **`twig`'s generic query surface is now exposed at the C boundary** — the
  hoped-for selector export landed. `twig_document_query` (Rust:
  `Document::query(selector)`, a CSS-lite selector reaching *every* node kind,
  returning `QueryMatch { span, kind }`) replaced the code-kind-specific
  accessor `code_spans` used to bind; `code_spans` now selects the code kinds
  itself over the generic API. Crucially for link ownership, twig also exposes a
  flat-node array (`Editor::nodes() -> [FlatNode]`) whose `destination:
  Option<String>` carries each `link`/`image` node's target. ✅ **Consumed:**
  `content::link_spans` queries `link` nodes for their spans, and
  `link::scan_body_links` slices each span and parses it with `Link::parse` (the
  span is authoritative, so no `destination` lookup is needed and no
  balanced-paren scan can over-reach). This is what made link-syntax **Stage 2**
  land (see below). Still unused from this surface: `image` nodes and the
  `destination`/reference-link path — a follow-up when non-inline links matter.

## Workspace config (the `config` relation)

Established: **workspace config is a reachable, self-describing document linked
from the root via a well-known `config` relation** — the registry's §6
reachability move, applied to policy. Lazily materialized (`prov config <k>
<v>` creates + links `prov.yaml` on first write); absent config = all
defaults. `link_format` precedence: config doc > root frontmatter (diaryx compat)
> default.

- ✅ `config` relation + `config_path`/`config_get`; CLI `config` get/set/print
  with `ensure_config` bootstrap; autofix + `find_root` read from it.
- ✅ **Typed `WorkspaceConfig`** (`config.rs`): `link_format`, `identity`,
  `id_links`, `embed_format`, with `paths_only()`/`stable_ids()` presets and
  `apply`/`from_meta`/`to_mapping` round-trip. The CLI builds the whole
  workspace from it, so **Diaryx and Obsidian are each just a config** —
  verified: `prov id` refuses under Diaryx / mints under Obsidian;
  `prov new` authors id links under Obsidian and a move leaves them
  untouched (registry does the maintenance). `prov config` prints all knobs.
- ✅ **id-link authoring** (`Workspace::authored_target`): `create` and autofix
  author `prov:<id>` (registering the target) when `id_links` is on and
  identity registers on a link, else a path in the link style. `create` mints
  IDs → `cmd_new` bootstraps the registry first when it will mint.
- ✅ **`default_embed_format`** wired into `create` (new-doc archetype default).
- ✅ **`content_format`** — the body-prose grammar, a full `WorkspaceConfig` field
  (`markdown`/`djot`/`html`), persisted by `init` (from `--content`) and read back
  like every other knob. `ContentFormat::extension()` gives the canonical file
  extension, so **title-primary `prov new "A Title"`** derives a readable
  filename (`link::slug(title).<content-ext>`) beside the parent while recording
  the real title in metadata; `--as <path>` / `--ext <e>` override the derived
  name (DESIGN §1 legibility — a slug, never an opaque `note-3.md`). The
  title-primary library seam is `Workspace::create_with_title`. Existing documents
  are reconciled by `convert <file> content_format <grammar>` (engine 3 below) —
  the knob governs what gets *written next*, the convert governs what is already
  there, and the two are deliberately independent (a mixed-grammar workspace is
  valid, since every reader takes the grammar from the file's own extension).
- **More config knobs.** `vocabulary` (a named `RelationSet` preset, later a full
  spec).
- **`prov config preset diaryx|obsidian`** — write a whole preset via
  `WorkspaceConfig::to_mapping` (the round-trip is already there).
- **Route `rename`'s path rewrites through the link style too.** `create` and
  autofix now author via the style/id seam; rename's inbound path rewrites still
  emit relative. Fold them through `format_link` for full consistency.
- ✅ **Builder threading smell — fixed by `Settings`.** Each knob (`link_style`,
  `id_links`, `default_embed_format`, …) had been hand-threaded through four
  field lists that all had to agree: `WorkspaceBuilder::identity`, `::index`,
  `::build`, and `Workspace`'s hand-written `Clone`. Ten of them now live in a
  `Settings` struct the type-flipping methods carry whole, so those four sites
  no longer mention any knob and `Workspace` went from 17 fields to 8. Note what
  the risk actually was: an *omitted* field was always a compile error, so the
  hazard was a line reading `workspace_id: String::new()` where it meant
  `self.workspace_id` — same type, no error, one knob silently defaulted for
  whoever called that method. `every_setting_survives_the_builder_type_flips`
  pins it. `Settings::from(&WorkspaceConfig)` is the payoff: the CLI's workspace
  construction went from ten builder calls to one, and a knob added to the config
  now reaches the workspace without touching `prov-cli`. It lives in
  `workspace.rs` because `workspace` already depends on `config`, and the reverse
  edge would be a cycle for no gain.
- **Custom registration combos.** `identity` serializes as `off`/`lazy`/`eager`;
  a non-preset trigger set falls back to `lazy` on write. Represent as a
  sub-mapping if custom combos ever matter.
- **Config doc's own `part_of` style.** On first creation it's written in the
  link style active *before* the setting applies (default markdown-root), which
  can differ from the value just set. Cosmetic; rewrite it in the final style.
- **Generalize "workspace resource via well-known relation."** Registry + config
  are the same shape (reachable, self-describing, lazily materialized). Codify a
  small reserved-relation spine; a derived-index cache (Route C) is the next
  instance. Also: refactor `ensure_registry` to share this bootstrap shape.

## Config conversion (per-file `convert`)

Established: setting a config axis governs *new* documents; a parallel **`convert`
mutation** reconciles *existing* ones — the workspace can "restate itself" in a
different style/format/grammar while structure is preserved (§6 reachability + §7
format-agnosticism, made an action). Decided this session:

- **Per-file by default (DESIGN §8), not workspace-wide.** `convert <file> <axis>
  <value>` restyles only what *that* document declares; `-r` extends to its
  spanning subtree (so `convert <root> … -r` is the whole-workspace case). No
  `--all`. `-f`/force is reserved for the lossy/destructive directions. A mixed
  style across the tree is valid and `check`-clean.
- **One command surface over ~4 distinct engines** (not one uniform transform):
  1. ✅ **Reference re-authoring** (`link_format`, and later `reference_wrapper/
     target/label`, `relation_styles`) — re-spell links, frontmatter *and* body,
     destination/label/wrapper preserved, id/external/alias skipped.
     `Workspace::convert_link_style` + `restyle_frontmatter_links`/
     `restyle_body_links`; CLI `convert <file> link_format <style> [-r]`. Only the
     `link_format` axis so far; the other reference axes are the natural next add.
  2. ✅ **Metadata language** (`metadata.format` yaml↔fig↔json↔toml, `metadata.embed`) —
     reserialize the block via `reformat_block`, resolving a target `EmbedType` from
     the document's *other* axis. `convert_meta_format` / `convert_meta_embed`.
     Comment loss across formats is the caveat; `separate` stays out of scope (a
     move, not a re-fence).
  3. ✅ **Content transcode** (`content_format`) — `convert_content_format`.
     `content::transcode` took a `from` parameter (it had assumed Markdown source,
     which only its two generated-page callers wanted), and the `.md → .dj` rename
     rides on the inbound-link machinery `rename` already had. What the plan
     underestimated: the cascade is **not** simply "`rename`'s existing job,"
     because a recursive sweep moves many documents *at once* and a mover may link
     to a fellow mover. Running the single-move collector per file yields two texts
     for such a document, each computed from disk and so each missing the other's
     rewrite, and the last one staged wins. Hence
     `collect_inbound_rewrites_multi`: one census for the whole set, one
     accumulated text per source. Force-gated on `html` at either end (via
     `ContentFormat::is_lossy_to`) rather than on the whole axis — md↔djot proved
     high-fidelity (emphasis, headings and raw HTML re-spelled; footnotes, tables,
     fences and `[[wikilinks]]` intact), with reference-style links inlined and
     their `[ref]:` definitions orphaned as the one wart. A separated pair converts
     its *body* and repoints the node's `content`.
  4. **Identity migration** (`id_storage`, `identity`) — stamp/strip ids, build/drop
     the registry; some directions destructive (identity→off breaks id links).
- **Un-abstract until the 2nd engine (DESIGN §10 discipline).** Three engines in and
  the `Migration` trait still has not earned itself: engine 2 shares a
  plan-then-apply *shape* with engine 1 but no code (`reformat_sweep` vs
  `convert_link_style`), and engine 3 shares neither — it plans a set of moves up
  front precisely because it cannot decide file-by-file. What did get extracted is
  the piece two verbs genuinely both needed: `rewrite_inbound_text`, the text-level
  half of `rewrite_inbound_doc`, so a caller folding several moves through one
  document is not forced back to the filesystem between them.
  `restyle_frontmatter_links` is still a near-sibling of `rerelativize` (move vs
  restyle); a shared `map_frontmatter_links(…, render)` could unify those two.
- ✅ **Pointer-reached documents keep their inbound links.** Found while converting
  the about page, but never convert's bug: `spanning_root` walks `part_of` *up*
  from the named file, and a document declaring none roots that walk at *itself*.
  That is right for the workspace root and wrong for every other parentless
  document — and prov authors one, the about page, which hangs off the root's
  `about` pointer. Every caller of that walk was affected: `rename`, `convert`,
  `retitle`, `delete`/`recycle` saw a one-document workspace and left the root's
  pointer naming a path that had moved, and `remedy`'s config and vocabulary
  lookups read *defaults* rather than the workspace's own settings. The fix is one
  condition — a walk that never moved yields to `Workspace::root_document`, the
  `discover` judgment (index > readme > lone candidate, extracted into `can_be_root`
  / `declares_no_parent` / `choose_root` so both callers share one rule) asked of a
  workspace already located. Costs nothing on the common path: a document *with* a
  parent climbs to a genuine root and never asks. An ambiguous or rootless
  directory keeps the old answer, there being nothing better to give.

## Routes (`route.rs`)

Landed: `prov new --in-title Daily/2026/2026-07 -p`. The position taken, so it
doesn't get relitigated: **the workflow is not prov's to own.** A `daily`
command would bake diaryx vocabulary into the core (§2/§9), and a workflow DSL in
`prov.yaml` would be worse — it would restate, in config, a fact the links
*already declare* (where daily entries live), which is the authoritative-vs-derived
confusion §5 warns about, while the genuinely non-derivable half (a date format)
is a fact about the *user*, not the workspace, and so can't live in a document
that's versioned and shared with the content. The split: prov supplies the
primitive a shell can't express (find-or-create nodes, linked both ways, registry
maintained); a two-line alias supplies the dates.

- **`--layout`'s default is `nested`, and that's a judgment call.** Flat is
  consistent with `create`'s beside-the-parent rule, but at depth it piles every
  generation into one directory and two routes sharing a segment name
  (`Daily/2026`, `Projects/2026`) collide on one filename. `-p` exists for deep
  routes, so nested wins. Note the *terminal* document is unaffected either way —
  it always lands beside its resolved parent — so this never contradicts `create`.
- **Route addressing is `new`-only so far.** `mv`, `attach`, `duplicate`, and
  `adopt` all name a parent by path and would take `--in-title` the same way. Worth
  doing once the segment/route surface has proven out; `route_segments` +
  `plan_route` are already the whole seam.
- **The synthesis seam is still un-extracted (deliberate, but the debt is now
  real).** `route.rs` reuses `intake`'s `SynthNode` and both end in the same
  `create_titled` loop, so this *is* the second consumer the "un-abstract until
  the 2nd engine" rule was waiting for (§10 discipline). It was left concrete
  because the two differ in the ways that matter — a plan of one chain vs. a
  forest, abort-on-failure vs. collect-and-continue — and a premature
  `Plan`/`Apply` trait would have to paper over both. Revisit when a third
  synthesizer appears, or when `--in-title` spreads to the other mutations.
- **Title matching is exact and case-sensitive.** `Daily/2026` won't find a node
  titled `daily`. Deliberate (addressing that guesses is worse than addressing
  that misses), but a `--fuzzy`/case-insensitive fallback that *reports* what it
  matched is a plausible ergonomic follow-up.
- **`title_text` coerces non-string scalars.** A hand-written `title: 2026` is a
  YAML integer, so route matching compares scalar *text*, not just
  `Value::as_str` — otherwise a route would synthesize a second `2026` beside a
  perfectly good one. If title-matching spreads (`title.rs`'s index does the same
  job for aliases), this coercion should probably move there and be shared.
- **An unlinked file in the way is an honest error, not a silent adopt.** `-p`
  onto a route whose file already exists on disk but isn't linked now refuses
  during the plan (`assert_vacant`) rather than mid-write. The old note here said
  "the fix is `adopt`, and the error doesn't say so" — that was wrong twice over:
  `adopt` is a library call and an `init` flag, **never a subcommand**, so naming it
  would prescribe a cure the CLI cannot dispense. The message now states the problem
  and offers only the remedy that exists (route to the title). Re-add the adopt
  clause if and when `prov adopt` is real — see below.
- **`assert_vacant` refuses; it deliberately does not reuse.** A route segment that
  lands on a directory already holding a node stops with that node's title. The
  tempting next step — resolve the segment *to* that node — is the one thing this
  must not do: the segment only "matched" because its slug equalled a directory
  name, which would make file layout load-bearing for graph addressing (§3) and
  leave routes meaning something other than what they spell. The cost is typing the
  real title once, and the error prints it.
- **The refusal is not "one index per directory."** That's diaryx's rule, and
  re-importing it as a lint would be directory-thinking: containment is link-shaped,
  so a directory may hold as many nodes as it likes and prov has no opinion.
  `assert_vacant` fires only where synthesis is *forced to pick a filename by
  slugging a title*, which is the one place the directory genuinely constrains the
  graph. Nowhere else should grow a version of this check.

## Reparenting (`reparent`, `mv --in`)

Landed. The axis it establishes, which is the part worth keeping straight:

| verb | path | place in tree |
| --- | --- | --- |
| `mv A B` | **changes** | preserved |
| `reparent A --in P` | preserved | **changes** |
| `mv A B --in P` | changes | changes |

The orthogonality *is* the design. Containment is link-shaped (§3), so a node may
live in any directory and relocating the file is a separate decision — which is why
`reparent` needs no `Layout` flag (moving is `mv`'s job) and why `mv --in` is pure
convenience rather than a third concept. `mv` runs first inside it, because `rename`
retargets inbound links and the reparent must then find the parent at the document's
*new* path.

`Workspace::reparent` is the verb `adopt` deliberately refuses to be: adopt is
additive and declines a child that already claims a different parent ("a contested
containment a human must resolve"); reparent *replaces* the claim. An unparented
child is accepted, which makes reparent a superset — so `prov adopt` was never
added, since `reparent --in` already links an orphan.

- **The two directions are judged separately** (fixed 2026-08-18). A child whose
  `part_of` already named the target used to return success having written
  nothing, while the parent went on not listing it — so the document stayed
  unreachable and the repair reported that it had worked. That is the state most
  orphans are in, not a rare one: `reparent` over 64 of them changed nothing,
  exited 0 on every one, and left all 64 orphaned. Now whichever half is missing
  is written and the other left alone (exactly `adopt`), nothing is written only
  when both hold, and the return value (`Reparented::{Moved, Linked, Unchanged}`)
  says which — so a caller need not report a move that did not happen.
- **A dangling old parent is not an error** (fixed 2026-08-18). `single_target`
  answers with a path, not with a promise that a file is at it, and an `id:`
  reference outlives its document by design (the registry keeps resolving it).
  Step 3 used to `load` that path and abort on a bare `ENOENT`, which made the
  verb refuse precisely in the state it is most needed for — the workaround being
  to hand-clear the stale key first. There is nothing to remove, so it is now
  skipped.

- **It is atomic against errors, detectable against crashes.** Three documents
  change, and they land as one `ChangeSet` (`prov/src/change.rs`), so an I/O
  failure at any of them unwinds the rest: no error leaves the child contained
  twice. A crash is still a crash — unwinding is driven from memory — so the write
  *order* still earns its keep, chosen so every window a crash could expose is a
  finding `check` reports: repoint the child (→ `MissingInverse` if it stops there),
  add the new entry (→ `DuplicateContainment`), remove the old one last. Removing
  first would leave a child pointing at a parent that forgot it — which was, when
  the order was chosen, the one state in this set `check` did *not* look for, and
  is exactly why it is last. `Finding::MissingContainment` now names that state
  too, for an unreached child, which makes the ordering belt-and-braces rather
  than the only thing standing between that window and silence; it stays as it
  is. Closing the crash window needs a journal and an `fsync` seam on `Storage`;
  nothing else will.
- **The cycle check is a walk, not a census.** Reparenting a node under its own
  descendant is refused by walking `part_of` up from the new parent. Cheap and
  bounded, but note *why* it must be refused rather than reported: the detached pair
  still claims itself in both directions, so nothing looks broken from inside the
  loop — it simply becomes unreachable, and per the orphan gap below, unreachable is
  precisely what `check` cannot see.
- **`reparent` moves one node, not a selection.** No `-r`, no globs; a subtree moves
  by moving its root, which is the whole point of a spanning tree. Bulk reparenting
  (every `2026-07-*` under a new month) is a shell loop today and probably should
  stay one.
- **A path passed to `--in-title` is caught, but only because a file is decisive.**
  `--in-path` takes a path and `--in-title` takes a route, and the two are spelled
  identically — so in a workspace whose index titles mirror their directory names
  (`2026` in `2026/`), a path handed to `--in-title` resolves segment after segment as
  a route and dies only on the filename, reading as "the route nearly worked". The
  check is narrow on purpose: *the string names an existing **file***. Routes name
  nodes by title, so a route that is also a filename means the wrong flag; and a
  directory (`Daily/2026/07`) must not trip it, since that is a perfectly good route
  and the exact workflow the feature exists for. Nothing softer would be safe — the
  slug of `index.md` is `indexmd`, so `-p` would have cheerfully created
  `daily/2026/07/indexmd/index.md` titled "index.md".

## The CLI target grammar

Landed, replacing `--in-path`/`--in-title` (which had themselves replaced
`--in`/`--under`/`--parent` days earlier — the churn is itself the evidence). A
document argument now declares its addressing mode in the **value**:

| spelling | mode | needs a workspace |
| --- | --- | --- |
| `daily.md` | path | no |
| `@Daily/2026/08` | route of titles from the root (bare `@` = the root) | yes |
| `id:fpk38j` (or legacy `prov:`) | registry handle | yes |

This is not a new idea — it is the library's own. `Addressing::{Path, Id, Alias}`
and `Link::parse` have always disambiguated a target by its syntax; the CLI had
been reinventing the same distinction as flag names, one layer up, incompatibly.

Why flags could never have worked, stated once so it isn't retried:

- **Cost is N modes × M slots.** Two flags covered two of three modes on one
  argument. Adding ids meant `--in-id`, and `Addressing` is an open enum.
- **It could only ever reach the parent.** A *subject* positional has no flag to
  spare, so every subject was path-only — `reparent <PATH>`, `rm <PATH>`,
  `show <FILE>`, ~17 arguments in all. Backwards: the subject is the thing you
  know by meaning ("the July 14 entry"), the destination is the thing you'd more
  plausibly know by path.
- **`--in-title` mislabelled itself.** In the library, title-addressing is
  `Addressing::Alias` — *one* name, resolved globally through `TitleIndex`, with
  §8's scan hazard. A route is a *bounded walk through many titles*: different
  mechanism, different hazard, and plural. The flag name papered over a real
  distinction that `@` leaves visible.

Design notes worth keeping:

- **Root discovery is lazy, and that is load-bearing.** `show`/`links`/`meta`/
  `get`/`body`/`render`/`set`/`unset` were workspace-*free*: they read a file, from
  anywhere, workspace or not. Resolving every argument through a workspace would
  have quietly destroyed that. A path resolves with no root; only `@`/`id:` discover
  one, because only they make the argument mean a *node* rather than a file.
- **`-p` is refused on a non-route `--in`** rather than ignored. A path or id names
  something that must already exist; `-p` beside one is a mistake, not a no-op.
- **Subjects never synthesize.** Only a `--in` destination may be created, and only
  with `-p`. A subject that does not resolve is a mistake, never an instruction.
- **`ArgGroup` disappeared entirely.** The mutually-exclusive placement group only
  existed to stop two flags naming the same thing. A grammar makes that
  unrepresentable — good evidence the shape is right rather than merely different.
- **`@` needs an escape and has one:** a file literally called `@foo.md` is
  `./@foo.md`, since only a *leading* `@` is stripped. Pinned by test.

Still flag-shaped and unfinished: `attach`'s `<PAYLOAD>` stays a path on purpose (a
binary has no title and no id), and `mv`'s `<TO>` stays a path (it names a location
that does not exist yet — there is nothing to address). `new`'s positional is a
title for the *new* document, not a reference. Those three are correct as paths; the
rest of the surface now speaks one grammar.

## CLI test coverage is one file old

`prov-cli/tests/targets.rs` is the **first** test over the binary; before it,
every CLI behaviour — flag vocabulary, output, exit codes, the interview — was
untested, which is why `--in`/`--in-title` confusion shipped twice. The library is well
covered and the CLI is not, and the gap is not cosmetic: the bugs that reached a real
workspace this cycle (a route silently synthesizing a competing spine, an error
naming a nonexistent `adopt` command, a path accepted as a route) were *all* CLI-layer
and *all* invisible to library tests, because in each case the library did exactly
what it was asked. Worth extending to the other placement-taking commands and to
`init`'s adoption paths.

## Index naming is hardcoded in three places

Not configurable anywhere — no `WorkspaceConfig` field, three independent literals:
`intake::existing_node` (`"index"`/`"readme"`), the CLI's `pick_root_candidate`
(same pair), and `route::synth_path` (`format!("index.{ext}")`). Two different sins
hide here and should be separated before either is fixed:

- **Detecting *someone else's* index by filename is the bug.** `existing_node` asks
  "which file is this directory's node?" and answers by name. Its own test proves
  what that's worth: the fixture's `notes/index.md` is `---\ntitle: Notes Home\n---`
  — no `contents`, no `part_of`, structurally identical to the `notes/leaf.md`
  beside it. It wins on its name alone. But no structure means no index: the check
  isn't detection at all, it's **collision-avoidance with mirror's own synthesis
  target** wearing detection's clothes. The honest rewrite is a structural predicate
  (`route::declares_containment` is one already) plus *adopt-the-file-at-the-synth-path*
  — after which the name only matters because it's the name prov itself chose.
  Note this changes `readme` handling: a structureless `README.md` stops being a
  folder node and becomes a child. That's correct, and it's exactly why the stems
  belong in config as the *user's* declared convention rather than core's guess.
- **Naming what prov *creates* is legitimate but still shouldn't be a literal.**
  `synth_path`'s `index.{ext}` is an authoring default like `default_embed_format`,
  and belongs beside it in `WorkspaceConfig`.

## Orphan detection can't see disconnected islands

✅ **Mostly done — `Finding::MissingContainment`.** `validate::orphans` scans only
"the directories the reachable set occupies (their direct children), never
descending into unreached subdirectories." So a subtree that is internally
well-linked but attached to *nothing* was invisible: in a real workspace,
`School/Archive/MATH113/` holds `math113.md` plus ~20 children all correctly linked
to each other, no `contents` entry anywhere points into the directory, and `check`
reported **zero** findings there while flagging 180 orphans that happen to lie in
already-reached directories. Worse than a miscount: `check` came back clean with 86
files sitting outside the reachable set, so the safety net said "safe" about
content nothing was recording.

This is §8 turned on itself: discovery is reachability-bounded, but *"what is
unreachable?"* is precisely the question a reachability bound cannot answer, and
the gap was silent, which is the worst property for a check to have.

What landed is the half that needs no flag, because it has no false positives to
guard against. `validate::missing_containment` scans **unbounded** and reports only
documents that *claim membership*: an unreached document whose own `part_of`
resolves to a document the tree reaches, which does not list it back. The claim is
the evidence that survives the bound — it is written down inside the island rather
than in the tree — and it is exactly what a vendored copy, a nested prov workspace
and a `scratch/` folder do not have, so §8's trade is preserved where it was
actually protecting something. Membership is a closure (an island's interior claims
the island, not the tree, so a stack of unlinked years resolves in one run), only
each island's entry point is reported, and the repair is `Derived` — one
`check --fix mechanical` reattaches the whole subtree.

What is still open: an island that claims **nothing at all** — a folder of notes
with no `part_of` anywhere in it — remains invisible, and so does one whose top
claims a parent that is *gone* (the claim has to land in the reachable set to be
evidence of anything, and a dangling one is indistinguishable from a stray copy of
someone else's document). Finding those still needs the opt-in unbounded report
(`check --unreached`?) with a message that says which
of the two questions it answered. `prov ignore` names them today — an unreached
file is exactly what earns a rule, and it reads `# unreached` under `--why` —
so the one diagnostic that catches them is not hidden behind enabling anything.

## Mutation

- **`delete` autofix.** `delete` now *diagnoses* inbound danglers; optionally
  offer to remove/rewrite them (careful — a link records intent).

## Link-syntax layer (this session's thread)

- ✅ **Workspace `LinkStyle`** — prov's analogue of diaryx's `LinkFormat`
  (`markdown_root` / `markdown_relative` / `plain_relative` / `plain_canonical`),
  read from the root's `link_format` frontmatter, honored by autofix (titled,
  style-native links). `link.rs` now has `format_link` + `path_to_title`; render
  brackets only *inside* `[label](…)`, matching diaryx.
- **Route create/rename through `LinkStyle` too.** They still emit bare relative
  paths directly; they should use `format_link(self.link_style(), …)` so *all*
  authoring is style-consistent (and `mv` becomes style-faithful — the earlier
  round-trip-faithfulness item folds into this).
- **Own the link-syntax layer in prov (don't publish a 3rd crate).** Having
  now read diaryx's `link_parser` (~1900 lines, well-tested: parse/canonicalize/
  format-in-4-styles/convert/relative/title), the clean end-state per DESIGN §9
  is prov *owning* this and diaryx depending on prov — not a speculative
  shared crate. **Decisions taken (this session):**
  - **Model — prov's `ReferenceStyle` is canonical; diaryx rewrites onto it.**
    prov's axes (`Wrapper` × `Addressing` × `LinkStyle`) already *subsume*
    diaryx's flat `LinkFormat`: each of its 4 variants is
    `Wrapper::Markdown × Addressing::Path × {one LinkStyle}`. diaryx maps its enum
    as a thin compat shim on its own side and deletes `link_parser.rs`. The
    id/alias/wikilink axes are prov-native, no diaryx equivalent.
  - **Bare paths — `resolve()` stays `bare = directory-relative`** (which already
    matches diaryx's legacy `Ambiguous` reading), so **no `PathType` machinery** is
    ported: the ambiguity is settled by committing to one meaning, not tagging it.
    ✅ **Done — both canonical styles retired.** The claim "bare = *root*-relative"
    was a latent bug: `path_text` emitted a workspace-relative bare path but
    `resolve()` reads bare as dir-relative, so those links resolved correctly only
    for a document at the workspace root. It was *both* canonical styles, not just
    the bare one — `resolve` never sees the wrapper, so `[Label](a.md)` written in
    `a/a.md` resolved to `a/a.md`, itself. This entry said `plain_canonical` alone
    until the `resolve ∘ format = id` property test found the other half, which is
    the argument for that test in one line: the law was about a *style axis*, and a
    hand-written example can only ever witness one point on it. `PathStyle` is now
    `root | relative`, `LinkStyle` the 2×2 cross-product, and the law holds
    unscoped for every style. A workspace still configured with `canonical` loads
    with a fallback to `root` (same path, plus the slash that makes the reading
    explicit) and a `check` finding; `prov convert <root> link_format markdown_root
    -r` restyles the documents.
  - **Migration wrinkle this creates.** diaryx's `plain_canonical` *means*
    bare-root-relative, which prov will no longer offer — so a diaryx workspace
    on `plain_canonical` can't just remap the enum; its links resolve differently
    under prov's resolver. `prov relink --to markdown_root` is the bridge
    (rewrites bare-root paths to `/`-prefixed), so the converter is the cutover
    tool, not merely a convenience.
  - **Scope — full port, including body `[text](path)` link resolution.** Two
    landable stages with a clean seam:
    - *Stage 1 (twig-independent):* the `plain_canonical` fix and balanced-paren
      path parsing (`find_closing_paren`) for frontmatter/longer strings still
      pending. ✅ The style *converter* landed as **per-file `convert`** (see
      "Config conversion" below), not a workspace-wide `relink` — the `link_format`
      axis is done; converting a diaryx `plain_canonical` workspace to
      `markdown_root` (the cutover bridge) is now `convert <root> link_format
      markdown_root -r`. Between these, diaryx can drop most of `link_parser.rs`.
    - ✅ *Stage 2 (body links) — done.* Real markdown/djot `[label](target)`
      links in body prose are now first-class alongside `[[wikilinks]]`.
      `content::link_spans` queries twig for `link`-node spans (code-aware:
      never a `[x](y)` inside a fence, an autolink, or non-link brackets);
      `link::scan_body_links` unifies those with the lexical wikilink scan into
      one `BodyLink { link: Link, span }` currency. Because twig hands back the
      exact span of each link, `Link::parse` reads each one in isolation — the
      **balanced-paren hazard is structurally absent** on the body side (Stage 1
      still needs `find_closing_paren` for frontmatter/longer strings). The three
      consumers (`census`/`check`, `title_scope`, the rename body-rewrite
      helpers) all moved onto `scan_body_links`, so in one pass: `check`
      diagnoses broken markdown/djot body links, backlinks include them, and
      `rename` re-relativizes them (wrapper-preserving — a markdown link stays
      markdown) while sparing id/external targets and code fences. Inline links
      only for now; reference-style/autolink and `image` nodes are a follow-up.
      Remaining Stage 1 (converter/`relink`, `find_closing_paren`,
      `plain_canonical` fix) is still what lets diaryx delete `link_parser.rs`.
