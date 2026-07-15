# Next steps — working notes

Deferred items from the identity / wikilink / link-syntax work, so we don't lose
them. Not curated design (that's `DESIGN.md`); this is a scratch backlog.

## Identity & backlinks

- **Step 4 — gated malformed-id autofix.** The one document-repairing heal: when
  the census finds a malformed `colophon:<id>` near an edge the registry resolves
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

- **Authoring `[[colophon:id]]` wikilinks.** The write side of the original
  idea #2: mint via `Trigger::Link`, drop the target into body prose. Closes the
  loop — the whole census/rename/backlink stack was built to support this.

## Autofix (DESIGN §8 — the sleeper feature)

Principle established: **autofix edits metadata only, never body prose** — a
`[[…]]` that is really code (`[[inf] * n for _ in range(m)]]`) must never be
"repaired", and structure-aware body editing is a later layer. So body-link
findings are diagnosis-only; frontmatter findings are fixable.

- ✅ **Missing inverse** — `suggest_fix` / `apply_fix` + interactive
  `colophon check --fix`. Adds the back-link, style-matched (absolute vs
  relative) to how the parent referenced the child; declines when the child
  already claims a different parent (contested).
- **Contested containment** (`… already contained elsewhere`, or a MissingInverse
  whose child claims another parent). The interesting interactive case: present
  the conflict and let the human pick — (a) make this the real parent [set the
  child's `part_of` here + drop the other's spanning entry], (b) demote this
  container's link from spanning → an overlay relation, (c) remove it. Needs a
  richer `Fix` (RemoveEntry / RetargetEntry) and a multi-choice prompt.
- **Broken frontmatter link** — offer removal, or a fuzzy relink when a
  similarly-named file exists. (Body broken links stay diagnosis-only.)
- **Non-interactive `--fix`** (apply all safe) for scripting once the safe set is
  trusted; today `--fix` is interactive (EOF → skip).

## Body parsing (`twig`)

The library colophon was waiting on to parse file bodies now exists:
[`twig`](https://github.com/adammharris/twig), a sister Zig-backed project
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
  (`twig/bindings/rust/twig/src/lib.rs`). colophon's `content::render_html`/
  `code_spans` are direct calls into that — no subprocess. `colophon render
  <file>` (colophon-cli, same feature) exercises rendering end-to-end.
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
  not done: needs a `colophon-cli` rebuilt with `--features content` to
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
reachability move, applied to policy. Lazily materialized (`colophon config <k>
<v>` creates + links `colophon.yaml` on first write); absent config = all
defaults. `link_format` precedence: config doc > root frontmatter (diaryx compat)
> default.

- ✅ `config` relation + `config_path`/`config_get`; CLI `config` get/set/print
  with `ensure_config` bootstrap; autofix + `find_root` read from it.
- ✅ **Typed `WorkspaceConfig`** (`config.rs`): `link_format`, `identity`,
  `id_links`, `embed_format`, with `paths_only()`/`stable_ids()` presets and
  `apply`/`from_meta`/`to_mapping` round-trip. The CLI builds the whole
  workspace from it, so **Diaryx and Obsidian are each just a config** —
  verified: `colophon id` refuses under Diaryx / mints under Obsidian;
  `colophon new` authors id links under Obsidian and a move leaves them
  untouched (registry does the maintenance). `colophon config` prints all knobs.
- ✅ **id-link authoring** (`Workspace::authored_target`): `create` and autofix
  author `colophon:<id>` (registering the target) when `id_links` is on and
  identity registers on a link, else a path in the link style. `create` mints
  IDs → `cmd_new` bootstraps the registry first when it will mint.
- ✅ **`default_embed_format`** wired into `create` (new-doc archetype default).
- ✅ **`content_format`** — the body-prose grammar, a full `WorkspaceConfig` field
  (`markdown`/`djot`/`html`), persisted by `init` (from `--content`) and read back
  like every other knob. `ContentFormat::extension()` gives the canonical file
  extension, so **title-primary `colophon new "A Title"`** derives a readable
  filename (`link::slug(title).<content-ext>`) beside the parent while recording
  the real title in metadata; `--as <path>` / `--ext <e>` override the derived
  name (DESIGN §1 legibility — a slug, never an opaque `note-3.md`). The
  title-primary library seam is `Workspace::create_with_title`.
- **More config knobs.** `vocabulary` (a named `RelationSet` preset, later a full
  spec).
- **`colophon config preset diaryx|obsidian`** — write a whole preset via
  `WorkspaceConfig::to_mapping` (the round-trip is already there).
- **Route `rename`'s path rewrites through the link style too.** `create` and
  autofix now author via the style/id seam; rename's inbound path rewrites still
  emit relative. Fold them through `format_link` for full consistency.
- **Builder threading smell.** Each new knob (`link_style`, `id_links`,
  `default_embed_format`) is hand-threaded through `identity()`/`index()`.
  Consider a shared inner `settings` struct the type-flipping methods carry whole.
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
  2. **Metadata language** (`embed_format` yaml↔fig↔json, `embed_style`) — reserialize
     frontmatter via `meta::serialize_mapping`; `embed_style: separate` already *is*
     `separate`/`combine`. Comment loss across formats is the caveat.
  3. **Content transcode** (`content_format` md↔djot) — twig `Document::serialize`
     transcodes (proven: md→djot), *plus* a `.md→.dj` rename whose inbound-link
     cascade is `rename`'s existing job. The heavy, lossy one — gate behind `-f`.
  4. **Identity migration** (`id_storage`, `identity`) — stamp/strip ids, build/drop
     the registry; some directions destructive (identity→off breaks id links).
- **Un-abstract until the 2nd engine (DESIGN §10 discipline).** `convert_link_style`
  is a concrete method, not a `Migration` trait. Extract the shared plan-then-apply
  seam (reuse the `StructurePlan` preview pattern) only when engine 2 lands to
  justify it. `restyle_frontmatter_links` is a near-sibling of `rerelativize` (move
  vs restyle); a shared `map_frontmatter_links(…, render)` could unify them then.

## Routes (`route.rs`)

Landed: `colophon new --under Daily/2026/2026-07 -p`. The position taken, so it
doesn't get relitigated: **the workflow is not colophon's to own.** A `daily`
command would bake diaryx vocabulary into the core (§2/§9), and a workflow DSL in
`colophon.yaml` would be worse — it would restate, in config, a fact the links
*already declare* (where daily entries live), which is the authoritative-vs-derived
confusion §5 warns about, while the genuinely non-derivable half (a date format)
is a fact about the *user*, not the workspace, and so can't live in a document
that's versioned and shared with the content. The split: colophon supplies the
primitive a shell can't express (find-or-create nodes, linked both ways, registry
maintained); a two-line alias supplies the dates.

- **`--layout`'s default is `nested`, and that's a judgment call.** Flat is
  consistent with `create`'s beside-the-parent rule, but at depth it piles every
  generation into one directory and two routes sharing a segment name
  (`Daily/2026`, `Projects/2026`) collide on one filename. `-p` exists for deep
  routes, so nested wins. Note the *terminal* document is unaffected either way —
  it always lands beside its resolved parent — so this never contradicts `create`.
- **Route addressing is `new`-only so far.** `mv`, `attach`, `duplicate`, and
  `adopt` all name a parent by path and would take `--under` the same way. Worth
  doing once the segment/route surface has proven out; `route_segments` +
  `plan_route` are already the whole seam.
- **The synthesis seam is still un-extracted (deliberate, but the debt is now
  real).** `route.rs` reuses `intake`'s `SynthNode` and both end in the same
  `create_titled` loop, so this *is* the second consumer the "un-abstract until
  the 2nd engine" rule was waiting for (§10 discipline). It was left concrete
  because the two differ in the ways that matter — a plan of one chain vs. a
  forest, abort-on-failure vs. collect-and-continue — and a premature
  `Plan`/`Apply` trait would have to paper over both. Revisit when a third
  synthesizer appears, or when `--under` spreads to the other mutations.
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
  onto a route whose file already exists on disk but isn't linked fails with
  `already exists` (from `create`). Correct — link-shaped containment means an
  unlinked file isn't in the tree — but the fix is `adopt`, and the error doesn't
  say so. Worth naming the remedy in the message.

## Mutation

- **`delete` autofix.** `delete` now *diagnoses* inbound danglers; optionally
  offer to remove/rewrite them (careful — a link records intent).

## Link-syntax layer (this session's thread)

- ✅ **Workspace `LinkStyle`** — colophon's analogue of diaryx's `LinkFormat`
  (`markdown_root` / `markdown_relative` / `plain_relative` / `plain_canonical`),
  read from the root's `link_format` frontmatter, honored by autofix (titled,
  style-native links). `link.rs` now has `format_link` + `path_to_title`; render
  brackets only *inside* `[label](…)`, matching diaryx.
- **Route create/rename through `LinkStyle` too.** They still emit bare relative
  paths directly; they should use `format_link(self.link_style(), …)` so *all*
  authoring is style-consistent (and `mv` becomes style-faithful — the earlier
  round-trip-faithfulness item folds into this).
- **Own the link-syntax layer in colophon (don't publish a 3rd crate).** Having
  now read diaryx's `link_parser` (~1900 lines, well-tested: parse/canonicalize/
  format-in-4-styles/convert/relative/title), the clean end-state per DESIGN §9
  is colophon *owning* this and diaryx depending on colophon — not a speculative
  shared crate. **Decisions taken (this session):**
  - **Model — colophon's `ReferenceStyle` is canonical; diaryx rewrites onto it.**
    colophon's axes (`Wrapper` × `Addressing` × `LinkStyle`) already *subsume*
    diaryx's flat `LinkFormat`: each of its 4 variants is
    `Wrapper::Markdown × Addressing::Path × {one LinkStyle}`. diaryx maps its enum
    as a thin compat shim on its own side and deletes `link_parser.rs`. The
    id/alias/wikilink axes are colophon-native, no diaryx equivalent.
  - **Bare paths — `resolve()` stays `bare = directory-relative`** (which already
    matches diaryx's legacy `Ambiguous` reading), so **no `PathType` machinery** is
    ported: the ambiguity is settled by committing to one meaning, not tagging it.
    Retire/redefine `plain_canonical`, whose current "bare = *root*-relative" claim
    is a latent bug — `path_text(PlainCanonical)` emits a root-relative bare path
    but `resolve()` reads bare as dir-relative, so those links resolve correctly
    only for documents at the workspace root.
  - **Migration wrinkle this creates.** diaryx's `plain_canonical` *means*
    bare-root-relative, which colophon will no longer offer — so a diaryx workspace
    on `plain_canonical` can't just remap the enum; its links resolve differently
    under colophon's resolver. `colophon relink --to markdown_root` is the bridge
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
