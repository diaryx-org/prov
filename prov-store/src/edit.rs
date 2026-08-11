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

use prov_graph::document::MetaCarrier;
use prov_graph::meta::Mapping;
use prov_graph::{Error, Result};

/// The frontmatter archetype used to synthesize a metadata block for a document
/// that has none. YAML (`---`) is the convention when compiled in; otherwise the
/// first other format that is. Exactly one arm survives `cfg` stripping, and the
/// `compile_error!` in `lib.rs` guarantees at least one does.
fn default_embed_type() -> EmbedType {
    #[cfg(feature = "yaml")]
    return EmbedType::FrontmatterYaml;
    #[cfg(all(not(feature = "yaml"), feature = "json"))]
    return EmbedType::FrontmatterJson;
    #[cfg(all(not(feature = "yaml"), not(feature = "json"), feature = "toml"))]
    return EmbedType::PlusToml;
    #[cfg(all(
        not(feature = "yaml"),
        not(feature = "json"),
        not(feature = "toml"),
        feature = "fig-lang"
    ))]
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
    /// frontmatter block in `default_embed_type`'s archetype (`---` YAML
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

/// Upsert `dotted` to a full [`Value`](prov_graph::meta::Value) — the mapping-valued
/// counterpart to [`set_in_text`], which takes only a `fig::Value` scalar. Lets a
/// caller set a whole nested block (e.g. the root's `prov:` policy block) without
/// naming `fig`, converting through the crate's `Value → fig::Value` bridge.
///
/// # How a mapping is written
///
/// One splice, whatever the shape. fig renders a mapping as block YAML and
/// creates a path's missing ancestors as block containers, so a nested set lands
/// readable — the layout, comments and key order around it preserved — even when
/// nothing along the path existed before.
///
/// It was not always one splice. Through fig 2.5.2 a block value spliced into
/// auto-created ancestors came out corrupt *and reported success*: `a: {b: - x}`,
/// which re-reads as a string rather than a list. So this function used to write
/// the value, re-parse the document, compare the value's **kind** at the path,
/// and on a mismatch fall back to writing one scalar leaf at a time, then prune
/// whatever keys the old subtree had and the new one did not.
///
/// fig 2.5.3 removed the need for all of it: ancestors are seeded as block, and
/// a splice that cannot be satisfied — into an existing *flow* container — is an
/// `Err` instead of a quiet rewrite. There is no shape the leaf-by-leaf path can
/// write that the direct one cannot, so the fallback had nothing left to catch,
/// and the pruning went with it because a direct set replaces the node whole.
///
/// The write is an **upsert of the whole subtree**: keys the document carries
/// under `dotted` that `value` does not are removed, so replacing a mapping
/// replaces it rather than merging into what was there before.
pub fn set_meta_in_text(
    text: &str,
    carrier: Option<MetaCarrier>,
    dotted: &str,
    value: &prov_graph::meta::Value,
) -> Result<String> {
    set_in_text(text, carrier, dotted, fig::Value::from(value))
}

/// The value at a dotted path in `meta`, or `None`. Mapping keys only — a
/// sequence index along the way reads as absent.
#[cfg(test)]
fn value_at<'a>(meta: &'a prov_graph::meta::Value, dotted: &str) -> Option<&'a prov_graph::meta::Value> {
    let mut current = meta;
    for part in dotted.split('.') {
        current = current.as_mapping()?.get(part)?;
    }
    Some(current)
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
/// ([`Document::parse`](prov_graph::Document::parse), via [`fig::split`]) hands back to
/// the format parser, which does not HTML-decode a `<pre><code>` island. Writing
/// what that reader expects keeps a converted value round-tripping through
/// `prov get`/`check` rather than acquiring stray `&lt;` entities.
///
/// [`serialize_mapping`]: prov_graph::meta::serialize_mapping
pub fn reformat_block(body: &str, mapping: &Mapping, target: EmbedType) -> Result<String> {
    let mut inner = prov_graph::meta::serialize_mapping(mapping, target.inner_format())?;
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
        prov_graph::document::Document::parse(path, text)
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

    #[cfg(feature = "yaml")]
    #[test]
    fn an_all_digit_id_survives_the_round_trip_as_a_string() {
        // The NOID alphabet has digits, so a minted id can look like a number.
        // Stamping one must not turn it into an integer (which would drop the
        // leading zero and make `Value::as_str` return `None` on read-back).
        let text = "---\ntitle: T\n---\nbody\n";
        let out = set_in_text(
            text,
            carrier_of("x.md", text),
            "id",
            fig::Value::Str("0123456".into()),
        )
        .unwrap();
        let back = prov_graph::document::Document::parse("x.md", &out).unwrap();
        assert_eq!(
            back.meta.get("id").and_then(prov_graph::Value::as_str),
            Some("0123456"),
            "{out}"
        );
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
        let mut view = prov_graph::meta::Mapping::new();
        view.insert("group".into(), prov_graph::meta::Value::String("date".into()));
        view.insert("by".into(), prov_graph::meta::Value::String("year".into()));

        let out = set_meta_in_text(
            text,
            Some(carrier),
            "diaryx.views.daily",
            &prov_graph::meta::Value::Mapping(view),
        )
        .expect("a three-deep mapping into a document with no `diaryx` key");

        let doc = prov_graph::Document::parse(std::path::Path::new("config.yaml"), &out).unwrap();
        let daily = value_at(&doc.meta, "diaryx.views.daily").expect("the block reads back");
        let map = daily.as_mapping().expect("a mapping");
        assert_eq!(
            map.get("group").and_then(prov_graph::meta::Value::as_str),
            Some("date")
        );
        assert_eq!(
            map.get("by").and_then(prov_graph::meta::Value::as_str),
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
        let mut view = prov_graph::meta::Mapping::new();
        view.insert("group".into(), prov_graph::meta::Value::String("date".into()));

        let out = set_meta_in_text(
            text,
            Some(carrier),
            "diaryx.views.daily",
            &prov_graph::meta::Value::Mapping(view),
        )
        .expect("replace");

        let doc = prov_graph::Document::parse(std::path::Path::new("config.yaml"), &out).unwrap();
        let map = value_at(&doc.meta, "diaryx.views.daily")
            .and_then(prov_graph::meta::Value::as_mapping)
            .expect("still a mapping");
        assert_eq!(
            map.get("group").and_then(prov_graph::meta::Value::as_str),
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
        let mut view = prov_graph::meta::Mapping::new();
        view.insert("group".into(), prov_graph::meta::Value::String("people".into()));

        let out = set_meta_in_text(
            text,
            Some(carrier),
            "diaryx.views.folks",
            &prov_graph::meta::Value::Mapping(view),
        )
        .expect("a second view");

        let doc = prov_graph::Document::parse(std::path::Path::new("config.yaml"), &out).unwrap();
        assert_eq!(
            value_at(&doc.meta, "diaryx.views.daily.group").and_then(prov_graph::meta::Value::as_str),
            Some("date"),
            "the first view survived: {out}"
        );
        assert_eq!(
            value_at(&doc.meta, "diaryx.views.folks.group").and_then(prov_graph::meta::Value::as_str),
            Some("people")
        );
    }

    /// Nested mappings flatten all the way down, however deep.
    #[cfg(feature = "yaml")]
    #[test]
    fn nested_mappings_flatten_all_the_way_down() {
        let text = "title: t\n";
        let carrier = carrier_of("config.yaml", text).unwrap();
        let mut inner = prov_graph::meta::Mapping::new();
        inner.insert("closed".into(), prov_graph::meta::Value::Bool(true));
        let mut outer = prov_graph::meta::Mapping::new();
        outer.insert("audience".into(), prov_graph::meta::Value::Mapping(inner));

        let out = set_meta_in_text(
            text,
            Some(carrier),
            "a.b",
            &prov_graph::meta::Value::Mapping(outer),
        )
        .expect("nested write");
        let doc = prov_graph::Document::parse(std::path::Path::new("config.yaml"), &out).unwrap();
        assert_eq!(
            value_at(&doc.meta, "a.b.audience.closed"),
            Some(&prov_graph::meta::Value::Bool(true)),
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
        let mut inner = prov_graph::meta::Mapping::new();
        inner.insert(
            "terms".into(),
            prov_graph::meta::Value::Sequence(vec![
                prov_graph::meta::Value::String("public".into()),
                prov_graph::meta::Value::String("private".into()),
            ]),
        );
        let mut outer = prov_graph::meta::Mapping::new();
        outer.insert("audience".into(), prov_graph::meta::Value::Mapping(inner));

        let out = set_meta_in_text(
            text,
            Some(carrier),
            "vocab",
            &prov_graph::meta::Value::Mapping(outer),
        )
        .expect("a sequence under an existing block");
        let doc = prov_graph::Document::parse(std::path::Path::new("config.yaml"), &out).unwrap();
        let terms = value_at(&doc.meta, "vocab.audience.terms")
            .and_then(prov_graph::meta::Value::as_sequence)
            .expect("the sequence reads back as a sequence");
        assert_eq!(terms.len(), 2, "{out}");
    }

    /// A list at a depth that does not exist yet. This was the shape that broke
    /// worst before fig 2.5.3 — the ancestors were created as flow, the sequence
    /// was spliced in as `nested: {terms: - public}`, and it re-read as the
    /// *string* `"- public"`. prov detected that by re-parsing and refused the
    /// write. Now the ancestors are block and the list is a list, so the assert
    /// is on the value rather than on an error message.
    #[cfg(feature = "yaml")]
    #[test]
    fn a_sequence_lands_as_a_sequence_at_a_path_with_no_parent_block() {
        let text = "title: t\n";
        let carrier = carrier_of("config.yaml", text).unwrap();
        let mut outer = prov_graph::meta::Mapping::new();
        outer.insert(
            "terms".into(),
            prov_graph::meta::Value::Sequence(vec![prov_graph::meta::Value::String("public".into())]),
        );

        let out = set_meta_in_text(
            text,
            Some(carrier),
            "deep.nested",
            &prov_graph::meta::Value::Mapping(outer),
        )
        .expect("a list with no parent block now lands");
        let doc = prov_graph::Document::parse(std::path::Path::new("config.yaml"), &out).unwrap();
        let terms = value_at(&doc.meta, "deep.nested.terms")
            .and_then(prov_graph::meta::Value::as_sequence)
            .unwrap_or_else(|| panic!("the list must read back as a list: {out}"));
        assert_eq!(terms.len(), 1, "{out}");
        assert_eq!(terms[0].as_str(), Some("public"), "{out}");
    }

    /// The other half of the same promise: a mapping written over an existing
    /// one **replaces** it. prov used to guarantee this by diffing the old
    /// subtree against the new leaves and unsetting the difference; now it rests
    /// on fig's splice replacing the node whole, which is worth pinning.
    #[cfg(feature = "yaml")]
    #[test]
    fn writing_a_mapping_replaces_the_subtree_rather_than_merging_into_it() {
        let text = "title: t\na:\n  keep: 1\n  stale: 9\n  gone:\n    deeper: 2\n";
        let carrier = carrier_of("config.yaml", text).unwrap();
        let mut fresh = prov_graph::meta::Mapping::new();
        fresh.insert("keep".into(), prov_graph::meta::Value::String("new".into()));

        let out = set_meta_in_text(
            text,
            Some(carrier),
            "a",
            &prov_graph::meta::Value::Mapping(fresh),
        )
        .expect("replace an existing block");
        let doc = prov_graph::Document::parse(std::path::Path::new("config.yaml"), &out).unwrap();
        assert_eq!(
            value_at(&doc.meta, "a.keep").and_then(prov_graph::meta::Value::as_str),
            Some("new"),
            "{out}"
        );
        assert!(value_at(&doc.meta, "a.stale").is_none(), "{out}");
        assert!(value_at(&doc.meta, "a.gone").is_none(), "{out}");
        // and the document around it is untouched
        assert_eq!(
            value_at(&doc.meta, "title").and_then(prov_graph::meta::Value::as_str),
            Some("t"),
            "{out}"
        );
    }
}
