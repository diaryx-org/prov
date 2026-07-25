//! Format-preserving edits to a document's metadata, whatever carries it.
//!
//! [`MetaEditor`] dispatches on the document's [`MetaCarrier`]: a fenced block
//! is edited with fig's [`fig::Embed`] (fences and body untouched), a config
//! document with fig's [`fig::Editor`] (the whole file *is* the metadata).
//! Either way the edit is comment-preserving and byte-minimal — only the
//! changed node's bytes move — and the original carrier and format are never
//! rewritten into another.
//!
//! The workspace mutation ops build on this; the free functions are the
//! single-document surface (the CLI's `set`/`unset`).

use fig::{Embed, EmbedType, Segment};

use crate::document::MetaCarrier;
use crate::error::{Error, Result};
use crate::meta::Mapping;

/// The frontmatter archetype used to synthesize a metadata block for a document
/// that has none. YAML (`---`) is the convention when compiled in; otherwise the
/// first other format that is. Exactly one arm survives `cfg` stripping, and the
/// `compile_error!` in `lib.rs` guarantees at least one does.
fn default_embed_type() -> EmbedType {
    #[cfg(feature = "yaml")]
    return EmbedType::FrontmatterYaml;
    #[cfg(all(not(feature = "yaml"), feature = "json"))]
    return EmbedType::FrontmatterJson;
    #[cfg(all(not(feature = "yaml"), not(feature = "json"), feature = "fig-lang"))]
    return EmbedType::FrontmatterFig;
}

/// A comment-preserving editor over a document's metadata, generic over where
/// the metadata lives.
pub enum MetaEditor {
    /// Editing a fenced block inside a host file.
    Fenced(Embed),
    /// Editing a config document (the whole file is the metadata).
    Whole(fig::Editor),
}

impl MetaEditor {
    /// Open an editor over `text` for an existing carrier.
    pub fn open(text: &str, carrier: MetaCarrier) -> Result<Self> {
        Ok(match carrier {
            MetaCarrier::Fenced(kind) => MetaEditor::Fenced(Embed::open(text.as_bytes(), kind)?),
            MetaCarrier::WholeFile(format) => {
                MetaEditor::Whole(fig::Editor::open(text.as_bytes(), format)?)
            }
        })
    }

    /// Open an editor over `text`, creating the metadata block when the
    /// document has none: an explicit carrier is honored (an absent fenced
    /// block is synthesized in place), and `None` defaults to a fresh
    /// frontmatter block in [`default_embed_type`]'s archetype (`---` YAML
    /// when that feature is compiled in).
    pub fn open_or_init(text: &str, carrier: Option<MetaCarrier>) -> Result<Self> {
        Ok(match carrier {
            Some(MetaCarrier::WholeFile(format)) => {
                MetaEditor::Whole(fig::Editor::open(text.as_bytes(), format)?)
            }
            Some(MetaCarrier::Fenced(kind)) => {
                MetaEditor::Fenced(Embed::open_or_init(text.as_bytes(), kind)?)
            }
            None => MetaEditor::Fenced(Embed::open_or_init(text.as_bytes(), default_embed_type())?),
        })
    }

    /// Upsert `value` at `path` (the trailing segment must be a key).
    pub fn set_value(&mut self, path: &[Segment], value: impl Into<fig::Value>) -> Result<()> {
        match self {
            MetaEditor::Fenced(e) => e.set_value(path, value)?,
            MetaEditor::Whole(e) => e.set_value(path, value)?,
        }
        Ok(())
    }

    /// Replace the existing value at `path`.
    pub fn replace_value(&mut self, path: &[Segment], value: impl Into<fig::Value>) -> Result<()> {
        match self {
            MetaEditor::Fenced(e) => e.replace_value(path, value)?,
            MetaEditor::Whole(e) => e.replace_value(path, value)?,
        }
        Ok(())
    }

    /// Rename the key at `path`, keeping its value, position, and comments.
    pub fn replace_key(&mut self, path: &[Segment], key: &str) -> Result<()> {
        match self {
            MetaEditor::Fenced(e) => e.replace_key(path, key)?,
            MetaEditor::Whole(e) => e.replace_key(path, key)?,
        }
        Ok(())
    }

    /// Append `value` to the sequence at `path`.
    pub fn append_value(&mut self, path: &[Segment], value: impl Into<fig::Value>) -> Result<()> {
        match self {
            MetaEditor::Fenced(e) => e.append_value(path, value)?,
            MetaEditor::Whole(e) => e.append_value(path, value)?,
        }
        Ok(())
    }

    /// Delete the mapping entry at `path`.
    pub fn delete(&mut self, path: &[Segment]) -> Result<()> {
        match self {
            MetaEditor::Fenced(e) => e.delete(path)?,
            MetaEditor::Whole(e) => e.delete(path)?,
        }
        Ok(())
    }

    /// Remove the item at `index` from the sequence at `path`.
    pub fn remove_item(&mut self, path: &[Segment], index: usize) -> Result<()> {
        match self {
            MetaEditor::Fenced(e) => e.remove_item(path, index)?,
            MetaEditor::Whole(e) => e.remove_item(path, index)?,
        }
        Ok(())
    }

    /// Reorder the mapping entries at `path` (empty path = root) so `keys`
    /// come first, in that order; entries not listed keep their original
    /// relative order and follow. Unknown keys are ignored. Every entry keeps
    /// its comments and interleaved trivia.
    pub fn reorder_keys<S: AsRef<str>>(&mut self, path: &[Segment], keys: &[S]) -> Result<()> {
        match self {
            MetaEditor::Fenced(e) => e.reorder_keys(path, keys)?,
            MetaEditor::Whole(e) => e.reorder_keys(path, keys)?,
        }
        Ok(())
    }

    /// Reorder the sequence at `path` so the items at `indices` (positions in
    /// the current order) come first, in that order; items not listed keep
    /// their original relative order and follow. Out-of-range indices are
    /// ignored.
    pub fn reorder_items(&mut self, path: &[Segment], indices: &[usize]) -> Result<()> {
        match self {
            MetaEditor::Fenced(e) => e.reorder_items(path, indices)?,
            MetaEditor::Whole(e) => e.reorder_items(path, indices)?,
        }
        Ok(())
    }

    /// Render the full document text with the edits applied.
    pub fn render(&mut self) -> Result<String> {
        Ok(match self {
            MetaEditor::Fenced(e) => e.render()?.to_string(),
            MetaEditor::Whole(e) => e.source()?.to_string(),
        })
    }
}

/// Parse a dotted key path (`a.b.0.c`) into fig path segments. An all-digit
/// segment indexes a sequence; anything else names a mapping key.
pub fn key_path(dotted: &str) -> Vec<Segment<'_>> {
    dotted
        .split('.')
        .map(|part| match part.parse::<usize>() {
            Ok(index) => Segment::Index(index),
            Err(_) => Segment::Key(part),
        })
        .collect()
}

/// Interpret a CLI-provided scalar: `true`/`false`, integers, floats, and
/// `null` become their typed values; everything else stays a string.
pub fn infer_scalar(s: &str) -> fig::Value {
    match s {
        "true" => fig::Value::Bool(true),
        "false" => fig::Value::Bool(false),
        "null" | "~" => fig::Value::Null,
        _ => {
            if let Ok(i) = s.parse::<i64>() {
                fig::Value::Int(i)
            } else if let Ok(f) = s.parse::<f64>() {
                fig::Value::Float(f)
            } else {
                fig::Value::Str(s.to_string())
            }
        }
    }
}

/// Upsert `dotted` to `value` in `text`'s metadata (carrier-aware), creating
/// a YAML frontmatter block when the document has none. Returns the full
/// re-rendered document text.
pub fn set_in_text(
    text: &str,
    carrier: Option<MetaCarrier>,
    dotted: &str,
    value: fig::Value,
) -> Result<String> {
    let mut editor = MetaEditor::open_or_init(text, carrier)?;
    let path = key_path(dotted);
    match path.last() {
        // fig's `set` upserts a trailing *key*; an index-terminated path is a
        // pure replacement (there is no "insert at absent index" to upsert).
        Some(Segment::Index(_)) => editor.replace_value(&path, value)?,
        _ => editor.set_value(&path, value)?,
    }
    editor.render()
}

/// Upsert `dotted` to a full [`Value`](crate::meta::Value) — the mapping-valued
/// counterpart to [`set_in_text`], which takes only a `fig::Value` scalar. Lets a
/// caller set a whole nested block (e.g. the root's `prov:` policy block) without
/// naming `fig`, converting through the crate's `Value → fig::Value` bridge.
///
/// # Why a mapping is written leaf by leaf
///
/// A single `set` of a mapping value only lands when every ancestor of `dotted`
/// already exists in the document. fig renders a YAML mapping as *block* text
/// but auto-creates missing ancestors as *inline flow* (`a: {b: {}}`), and a
/// block value spliced into a flow container does not re-parse — so
/// `set("diaryx.views.daily", {…})` against a document with no `diaryx:` key
/// failed outright with a parse error. Seeding the ancestors as empty mappings
/// first does not help either: an empty flow map has no insertion point, so the
/// next set reports `NotFound`.
///
/// Decomposing the value into its scalar leaves sidesteps both. Each leaf is a
/// scalar splice, which fig creates ancestors for correctly at any depth, so the
/// same call now works whether the block is already there or is being
/// introduced.
///
/// The write is an **upsert of the whole subtree**: keys the document carries
/// under `dotted` that `value` does not are removed, so replacing a mapping
/// replaces it rather than merging into whatever was there before.
pub fn set_meta_in_text(
    text: &str,
    carrier: Option<MetaCarrier>,
    dotted: &str,
    value: &crate::meta::Value,
) -> Result<String> {
    // A non-mapping value is a single splice — the original path, unchanged.
    // An empty mapping goes the same way: it has no leaves to write, and `k: {}`
    // is a legitimate thing to record.
    let crate::meta::Value::Mapping(mapping) = value else {
        return set_in_text(text, carrier, dotted, fig::Value::from(value));
    };
    if mapping.is_empty() {
        return set_in_text(text, carrier, dotted, fig::Value::from(value));
    }

    // What the document holds here *before* the writes — the basis for pruning.
    let stale = meta_of(text, carrier).and_then(|meta| value_at(&meta, dotted).cloned());

    let mut leaves = Vec::new();
    collect_leaves(dotted, value, &mut leaves);
    let mut out = text.to_string();
    for (path, leaf) in &leaves {
        // Block layout first — it is what a YAML config should look like — with
        // flow as the fallback when the ancestors this write had to create are
        // themselves flow, which a block value cannot splice into.
        //
        // The fallback is chosen by **reading the result back**, not by trusting
        // the return: a block sequence spliced into a flow ancestor reports
        // success and emits text that no longer parses, so the damage would
        // otherwise surface much later as a mangled document. Scalars always
        // survive the block path; only a container leaf ever reaches the retry.
        let written = set_in_text(&out, carrier, path, fig::Value::from(*leaf))?;
        // **Read the result back rather than trusting the return.** fig
        // auto-creates a path's missing ancestors as inline flow, and a
        // block-rendered container spliced into flow reports success while
        // emitting text that no longer means what was written — a YAML sequence
        // lands as `terms: - public`, which re-reads as the string `"- public"`.
        // Scalars always survive, so only a sequence at a not-yet-existing depth
        // trips this. Flow rendering cannot rescue it either (fig cannot
        // serialize a bare sequence for YAML at all), so this is reported rather
        // than repaired: a loud error beats a document quietly rewritten into
        // something else.
        if !reads_back(&written, carrier, path, leaf) {
            return Err(Error::Structure(format!(
                "cannot write a list at {path:?}: its parent block does not exist yet, \
                 and fig would splice it into a flow container that cannot hold it. \
                 Create the parent block first, then set the list."
            )));
        }
        out = written;
    }

    // Prune what the old subtree had and the new one does not. Done after the
    // writes so a failure part-way leaves a superset rather than a hole.
    if let Some(stale) = stale {
        let fresh: std::collections::BTreeSet<&str> =
            leaves.iter().map(|(path, _)| path.as_str()).collect();
        let mut gone = Vec::new();
        collect_stale(dotted, &stale, &fresh, &mut gone);
        // Deepest first, so removing a child never invalidates a sibling's path.
        gone.sort_by_key(|path| std::cmp::Reverse(path.matches('.').count()));
        for path in gone {
            out = unset_in_text(&out, carrier, &path)?;
        }
    }
    Ok(out)
}

/// Whether `text`'s metadata still parses and carries a value of `leaf`'s shape
/// at `dotted` — the check that turns fig's silently-corrupting splice into a
/// retry.
///
/// Shape, not just presence: a block sequence spliced into a flow ancestor emits
/// `terms: - public` on one line, which still *parses* — as the string
/// `"- public"`. Only comparing the kind catches that the list stopped being a
/// list.
fn reads_back(
    text: &str,
    carrier: Option<MetaCarrier>,
    dotted: &str,
    leaf: &crate::meta::Value,
) -> bool {
    let Some(meta) = meta_of(text, carrier) else {
        return false;
    };
    let Some(found) = value_at(&meta, dotted) else {
        return false;
    };
    std::mem::discriminant(found) == std::mem::discriminant(leaf)
}

/// A document's metadata, read the way its carrier stores it.
///
/// Deliberately not `Document::parse`, which decides the carrier from a *path*:
/// this function is given the carrier and has no path to offer, and a stand-in
/// filename would silently read a whole-file config as a document with no
/// metadata at all — leaving nothing to prune, which is exactly the bug that
/// makes a replace behave like a merge.
fn meta_of(text: &str, carrier: Option<MetaCarrier>) -> Option<crate::meta::Value> {
    match carrier? {
        MetaCarrier::WholeFile(format) => crate::meta::parse_value(text, format).ok(),
        MetaCarrier::Fenced(kind) => {
            let (content, _) = fig::split(text, kind)?;
            crate::meta::parse_value(content, kind.inner_format()).ok()
        }
    }
}

/// The value at a dotted path in `meta`, or `None`. Mapping keys only — a
/// sequence index along the way reads as absent, which is right here: the caller
/// is replacing a *mapping*, and a path running through a sequence is not one
/// this upsert wrote.
fn value_at<'a>(meta: &'a crate::meta::Value, dotted: &str) -> Option<&'a crate::meta::Value> {
    let mut current = meta;
    for part in dotted.split('.') {
        current = current.as_mapping()?.get(part)?;
    }
    Some(current)
}

/// Flatten `value` into `(dotted path, leaf)` pairs — one per non-mapping leaf.
/// A sequence is a leaf: it splices as one value, and addressing into it by
/// index would make list edits depend on the order they are applied in.
fn collect_leaves<'a>(
    prefix: &str,
    value: &'a crate::meta::Value,
    out: &mut Vec<(String, &'a crate::meta::Value)>,
) {
    match value {
        crate::meta::Value::Mapping(map) if !map.is_empty() => {
            for (key, child) in map {
                collect_leaves(&format!("{prefix}.{key}"), child, out);
            }
        }
        _ => out.push((prefix.to_string(), value)),
    }
}

/// Paths under `prefix` the document still carries but the new value does not
/// define — the keys an upsert has to remove for a replace to be a replace.
fn collect_stale(
    prefix: &str,
    existing: &crate::meta::Value,
    fresh: &std::collections::BTreeSet<&str>,
    out: &mut Vec<String>,
) {
    match existing {
        crate::meta::Value::Mapping(map) if !map.is_empty() => {
            for (key, child) in map {
                collect_stale(&format!("{prefix}.{key}"), child, fresh, out);
            }
        }
        _ => {
            if !fresh.contains(prefix) {
                out.push(prefix.to_string());
            }
        }
    }
}

/// Delete the entry at `dotted` from `text`'s metadata (carrier-aware).
/// Returns the full re-rendered document text. Errors when the document has
/// no metadata or the path does not exist.
pub fn unset_in_text(text: &str, carrier: Option<MetaCarrier>, dotted: &str) -> Result<String> {
    let carrier = carrier
        .ok_or_else(|| Error::Structure("document has no embedded metadata block".into()))?;
    let mut editor = MetaEditor::open(text, carrier)?;
    editor.delete(&key_path(dotted))?;
    editor.render()
}

/// Re-emit `mapping` as a fresh metadata block of archetype `target`, placed in
/// `target`'s canonical position around the plain `body` (before it for
/// frontmatter, after it for endmatter) — the reconstruction a *format
/// conversion* performs. Unlike the comment-preserving edits above, this
/// deliberately rebuilds the block: a conversion crosses formats (a YAML comment
/// has no JSON home), so only the values survive.
///
/// The content is rendered by prov's canonical [`serialize_mapping`] — the
/// same serializer behind `prov meta --format`, so a converted block's
/// sequence and scalar layout matches the rest of the codebase (fig's per-key
/// [`Embed`] splice path renders some formats, notably fig sequences,
/// differently). The block's fences and placement come from fig, by synthesizing
/// an empty `target` block around `body` and splicing the serialized content into
/// its content slot.
///
/// The content is spliced verbatim — the same bytes prov's reader
/// ([`Document::parse`](crate::Document::parse), via [`fig::split`]) hands back to
/// the format parser, which does not HTML-decode a `<pre><code>` island. Writing
/// what that reader expects keeps a converted value round-tripping through
/// `prov get`/`check` rather than acquiring stray `&lt;` entities.
///
/// [`serialize_mapping`]: crate::meta::serialize_mapping
pub fn reformat_block(body: &str, mapping: &Mapping, target: EmbedType) -> Result<String> {
    let mut inner = crate::meta::serialize_mapping(mapping, target.inner_format())?;
    // The content slot sits between the opening fence's trailing newline and the
    // closing fence, so the content must end in exactly one newline for the close
    // fence to land on its own line.
    if !inner.ends_with('\n') {
        inner.push('\n');
    }
    // Synthesize an empty `target` block in its canonical place around `body`,
    // then replace its (empty) content slot with the serialized content: fig owns
    // the fences and placement, we own what goes between them.
    let rendered = Embed::open_or_init(body.as_bytes(), target)?
        .render()?
        .to_string();
    let content = Embed::extract(&rendered, target)?.region().content;
    let mut out = String::with_capacity(rendered.len() + inner.len());
    out.push_str(&rendered[..content.start]);
    out.push_str(&inner);
    out.push_str(&rendered[content.end..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn carrier_of(path: &str, text: &str) -> Option<MetaCarrier> {
        crate::document::Document::parse(path, text)
            .unwrap()
            .carrier
    }

    #[cfg(feature = "yaml")]
    #[test]
    fn set_preserves_comments_and_format() {
        let text = "---\n# keep me\ntitle: Old\n---\nbody\n";
        let out =
            set_in_text(text, carrier_of("x.md", text), "title", infer_scalar("New")).unwrap();
        assert_eq!(out, "---\n# keep me\ntitle: New\n---\nbody\n");
    }

    #[cfg(feature = "fig-lang")]
    #[test]
    fn set_in_a_fig_block_stays_fig() {
        let text = "```fig\ntitle = prov\n```\nbody\n";
        let out = set_in_text(
            text,
            carrier_of("x.md", text),
            "title",
            infer_scalar("renamed"),
        )
        .unwrap();
        assert!(out.starts_with("```fig\n"), "fence preserved: {out}");
        assert!(
            out.contains("title = renamed"),
            "fig dialect preserved: {out}"
        );
        assert!(out.ends_with("```\nbody\n"));
    }

    #[cfg(feature = "yaml")]
    #[test]
    fn set_edits_a_bare_config_document() {
        let text = "# workspace registry\ntitle: ID registry\nregistry:\n  abc: a.md\n";
        let out = set_in_text(
            text,
            carrier_of("registry.yaml", text),
            "registry.abc",
            infer_scalar("moved/a.md"),
        )
        .unwrap();
        assert!(out.contains("# workspace registry"), "comment kept: {out}");
        assert!(out.contains("abc: moved/a.md"), "{out}");
        assert!(!out.contains("---"), "no fences grown: {out}");
    }

    #[cfg(feature = "yaml")]
    #[test]
    fn set_creates_a_block_when_none_exists() {
        let out = set_in_text("just a body\n", None, "title", infer_scalar("T")).unwrap();
        assert!(out.starts_with("---\ntitle: T\n---\n"), "{out}");
        assert!(out.ends_with("just a body\n"));
    }

    #[cfg(feature = "yaml")]
    #[test]
    fn unset_removes_only_the_named_key() {
        let text = "---\ntitle: T\ndraft: true\n---\nbody\n";
        let out = unset_in_text(text, carrier_of("x.md", text), "draft").unwrap();
        assert_eq!(out, "---\ntitle: T\n---\nbody\n");
        assert!(unset_in_text("no meta\n", None, "x").is_err());
    }

    #[test]
    fn scalars_are_inferred() {
        assert_eq!(infer_scalar("true"), fig::Value::Bool(true));
        assert_eq!(infer_scalar("42"), fig::Value::Int(42));
        assert_eq!(infer_scalar("4.5"), fig::Value::Float(4.5));
        assert_eq!(infer_scalar("null"), fig::Value::Null);
        assert_eq!(infer_scalar("hello"), fig::Value::Str("hello".into()));
    }

    #[cfg(feature = "yaml")]
    #[test]
    fn dotted_paths_mix_keys_and_indices() {
        let text = "---\ncontents:\n- a.md\n- b.md\n---\n";
        let out = set_in_text(
            text,
            carrier_of("x.md", text),
            "contents.1",
            infer_scalar("c.md"),
        )
        .unwrap();
        assert!(out.contains("- a.md\n- c.md"), "{out}");
    }

    // ---- MetaEditor parity with fig::Embed: reorder_items/replace_key/reorder_keys ----

    #[cfg(feature = "yaml")]
    #[test]
    fn replace_key_renames_the_key_and_preserves_comments_elsewhere() {
        let text = "---\n# keep me\ntitle: Old\nauthor: me\n---\nbody\n";
        let mut editor = MetaEditor::open(text, carrier_of("x.md", text).unwrap()).unwrap();
        editor.replace_key(&key_path("title"), "name").unwrap();
        let out = editor.render().unwrap();
        assert!(out.contains("name: Old"), "{out}");
        assert!(!out.contains("title:"), "{out}");
        assert!(out.contains("# keep me"), "comment lost: {out}");
        assert!(out.contains("author: me"), "{out}");
    }

    #[cfg(feature = "yaml")]
    #[test]
    fn reorder_keys_moves_listed_keys_first_and_preserves_comments() {
        let text = "---\n# c1\ntitle: T\n# c2\nauthor: me\ndraft: true\n---\nbody\n";
        let mut editor = MetaEditor::open(text, carrier_of("x.md", text).unwrap()).unwrap();
        editor
            .reorder_keys(&[] as &[Segment], &["draft", "title"])
            .unwrap();
        let out = editor.render().unwrap();
        let draft_pos = out.find("draft:").unwrap();
        let title_pos = out.find("title:").unwrap();
        let author_pos = out.find("author:").unwrap();
        assert!(draft_pos < title_pos && title_pos < author_pos, "{out}");
        assert!(out.contains("# c1"), "comment lost: {out}");
        assert!(out.contains("# c2"), "comment lost: {out}");
    }

    #[cfg(feature = "yaml")]
    #[test]
    fn reorder_items_moves_listed_items_first_and_preserves_comments() {
        let text = "---\ncontents:\n- a # keep a\n- b # keep b\n- c # keep c\n---\nbody\n";
        let mut editor = MetaEditor::open(text, carrier_of("x.md", text).unwrap()).unwrap();
        editor
            .reorder_items(&key_path("contents"), &[2, 0])
            .unwrap();
        let out = editor.render().unwrap();
        assert!(out.contains("# keep a"), "comment lost: {out}");
        assert!(out.contains("# keep b"), "comment lost: {out}");
        assert!(out.contains("# keep c"), "comment lost: {out}");
        let a_pos = out.find("- a").unwrap();
        let b_pos = out.find("- b").unwrap();
        let c_pos = out.find("- c").unwrap();
        // indices [2, 0] -> c, a first (in that order), then the unlisted b follows.
        assert!(c_pos < a_pos && a_pos < b_pos, "{out}");
    }

    #[cfg(feature = "yaml")]
    #[test]
    fn reorder_keys_works_on_a_whole_file_config_document() {
        // Exercises the `MetaEditor::Whole` arm (a config document, not a
        // fenced block) — the same op, the other carrier.
        let text =
            "# workspace registry\ntitle: ID registry\npart_of: index.md\nregistry:\n  abc: a.md\n";
        let mut editor =
            MetaEditor::open(text, carrier_of("registry.yaml", text).unwrap()).unwrap();
        editor
            .reorder_keys(&[] as &[Segment], &["part_of"])
            .unwrap();
        let out = editor.render().unwrap();
        let part_of_pos = out.find("part_of:").unwrap();
        let title_pos = out.find("title:").unwrap();
        assert!(part_of_pos < title_pos, "{out}");
        assert!(out.contains("# workspace registry"), "comment lost: {out}");
    }

    /// **The regression this function exists for.** Setting a mapping at a path
    /// whose ancestors are absent used to fail outright: fig creates missing
    /// ancestors as inline flow, and the block-rendered YAML mapping spliced
    /// into that flow did not re-parse.
    #[cfg(feature = "yaml")]
    #[test]
    fn a_mapping_lands_at_a_path_whose_ancestors_do_not_exist_yet() {
        let text = "title: prov config\nspec: 1\n";
        let carrier = carrier_of("config.yaml", text).unwrap();
        let mut view = crate::meta::Mapping::new();
        view.insert("group".into(), crate::meta::Value::String("date".into()));
        view.insert("by".into(), crate::meta::Value::String("year".into()));

        let out = set_meta_in_text(
            text,
            Some(carrier),
            "diaryx.views.daily",
            &crate::meta::Value::Mapping(view),
        )
        .expect("a three-deep mapping into a document with no `diaryx` key");

        let doc = crate::Document::parse(std::path::Path::new("config.yaml"), &out).unwrap();
        let daily = value_at(&doc.meta, "diaryx.views.daily").expect("the block reads back");
        let map = daily.as_mapping().expect("a mapping");
        assert_eq!(
            map.get("group").and_then(crate::meta::Value::as_str),
            Some("date")
        );
        assert_eq!(
            map.get("by").and_then(crate::meta::Value::as_str),
            Some("year")
        );
        assert!(
            out.contains("title: prov config"),
            "the rest survived: {out}"
        );
    }

    /// Replacing a mapping is a replace, not a merge: a key the old block had
    /// and the new one does not is removed. Without this, clearing a view's
    /// `under:` would leave the lens scoped to an anchor nobody declared.
    #[cfg(feature = "yaml")]
    #[test]
    fn replacing_a_mapping_drops_the_keys_it_no_longer_declares() {
        let text = "title: t\ndiaryx:\n  views:\n    daily:\n      group: date\n      under: '[Daily](id:abc)'\n";
        let carrier = carrier_of("config.yaml", text).unwrap();
        let mut view = crate::meta::Mapping::new();
        view.insert("group".into(), crate::meta::Value::String("date".into()));

        let out = set_meta_in_text(
            text,
            Some(carrier),
            "diaryx.views.daily",
            &crate::meta::Value::Mapping(view),
        )
        .expect("replace");

        let doc = crate::Document::parse(std::path::Path::new("config.yaml"), &out).unwrap();
        let map = value_at(&doc.meta, "diaryx.views.daily")
            .and_then(crate::meta::Value::as_mapping)
            .expect("still a mapping");
        assert_eq!(
            map.get("group").and_then(crate::meta::Value::as_str),
            Some("date")
        );
        assert!(
            map.get("under").is_none(),
            "a dropped key must not linger: {out}"
        );
    }

    /// A sibling under the same parent is untouched — the write is scoped to
    /// the path it was given, so declaring a second view keeps the first.
    #[cfg(feature = "yaml")]
    #[test]
    fn a_sibling_mapping_survives_the_write() {
        let text = "title: t\ndiaryx:\n  views:\n    daily:\n      group: date\n";
        let carrier = carrier_of("config.yaml", text).unwrap();
        let mut view = crate::meta::Mapping::new();
        view.insert("group".into(), crate::meta::Value::String("people".into()));

        let out = set_meta_in_text(
            text,
            Some(carrier),
            "diaryx.views.folks",
            &crate::meta::Value::Mapping(view),
        )
        .expect("a second view");

        let doc = crate::Document::parse(std::path::Path::new("config.yaml"), &out).unwrap();
        assert_eq!(
            value_at(&doc.meta, "diaryx.views.daily.group").and_then(crate::meta::Value::as_str),
            Some("date"),
            "the first view survived: {out}"
        );
        assert_eq!(
            value_at(&doc.meta, "diaryx.views.folks.group").and_then(crate::meta::Value::as_str),
            Some("people")
        );
    }

    /// Nested mappings flatten all the way down, however deep.
    #[cfg(feature = "yaml")]
    #[test]
    fn nested_mappings_flatten_all_the_way_down() {
        let text = "title: t\n";
        let carrier = carrier_of("config.yaml", text).unwrap();
        let mut inner = crate::meta::Mapping::new();
        inner.insert("closed".into(), crate::meta::Value::Bool(true));
        let mut outer = crate::meta::Mapping::new();
        outer.insert("audience".into(), crate::meta::Value::Mapping(inner));

        let out = set_meta_in_text(
            text,
            Some(carrier),
            "a.b",
            &crate::meta::Value::Mapping(outer),
        )
        .expect("nested write");
        let doc = crate::Document::parse(std::path::Path::new("config.yaml"), &out).unwrap();
        assert_eq!(
            value_at(&doc.meta, "a.b.audience.closed"),
            Some(&crate::meta::Value::Bool(true)),
            "{out}"
        );
    }

    /// A sequence lands fine when its parent block already exists — the common
    /// case, and the one that must keep its block layout.
    #[cfg(feature = "yaml")]
    #[test]
    fn a_sequence_lands_under_a_parent_that_already_exists() {
        let text = "title: t\nvocab:\n  audience:\n    closed: true\n";
        let carrier = carrier_of("config.yaml", text).unwrap();
        let mut inner = crate::meta::Mapping::new();
        inner.insert(
            "terms".into(),
            crate::meta::Value::Sequence(vec![
                crate::meta::Value::String("public".into()),
                crate::meta::Value::String("private".into()),
            ]),
        );
        let mut outer = crate::meta::Mapping::new();
        outer.insert("audience".into(), crate::meta::Value::Mapping(inner));

        let out = set_meta_in_text(
            text,
            Some(carrier),
            "vocab",
            &crate::meta::Value::Mapping(outer),
        )
        .expect("a sequence under an existing block");
        let doc = crate::Document::parse(std::path::Path::new("config.yaml"), &out).unwrap();
        let terms = value_at(&doc.meta, "vocab.audience.terms")
            .and_then(crate::meta::Value::as_sequence)
            .expect("the sequence reads back as a sequence");
        assert_eq!(terms.len(), 2, "{out}");
    }

    /// **A limit this reports rather than papers over.** fig creates missing
    /// ancestors as inline flow and cannot render a bare YAML sequence at all,
    /// so a list at a depth that does not exist yet has nowhere valid to land.
    /// Left alone it wrote `terms: - public`, which re-reads as a *string*; the
    /// read-back check turns that silent rewrite into an error naming the fix.
    #[cfg(feature = "yaml")]
    #[test]
    fn a_sequence_at_a_path_with_no_parent_block_is_an_error_not_a_silent_rewrite() {
        let text = "title: t\n";
        let carrier = carrier_of("config.yaml", text).unwrap();
        let mut outer = crate::meta::Mapping::new();
        outer.insert(
            "terms".into(),
            crate::meta::Value::Sequence(vec![crate::meta::Value::String("public".into())]),
        );

        let err = set_meta_in_text(
            text,
            Some(carrier),
            "deep.nested",
            &crate::meta::Value::Mapping(outer),
        )
        .expect_err("a list with no parent block must not land silently");
        assert!(
            format!("{err}").contains("cannot write a list"),
            "the error should name what to do: {err}"
        );
    }
}
