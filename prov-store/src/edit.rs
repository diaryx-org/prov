//! Format-preserving edits to a document's metadata, whatever carries it.
//!
//! [`MetaEditor`] dispatches on the document's [`MetaCarrier`]: a frontmatter
//! block is edited through twig's [`Embed`] (fences and the
//! body around it untouched), a config document with fig's [`fig::Editor`] (the
//! whole file *is* the metadata). Either way the edit is comment-preserving and
//! byte-minimal — only the changed node's bytes move — and the original carrier
//! and format are never rewritten into another.
//!
//! The workspace mutation ops build on this; the free functions are the
//! single-document surface (the CLI's `set`/`unset`).

use fig::Segment;
use prov_graph::twig::meta::Embed;

use prov_graph::document::{MetaCarrier, frontmatter_block};
use prov_graph::field::Address;
use prov_graph::meta::Mapping;
use prov_graph::{Error, MetaFormat, Result};

/// `body` with a frontmatter block in `format` above it, holding `content`
/// (which ends its last line, or is empty). The fences are the ones twig writes
/// and reads: `---` for YAML, `+++` for TOML, and `---<name>` closed by `---`
/// for the others.
fn frontmatter_text(format: MetaFormat, content: &str, body: &str) -> String {
    let (open, close) = match format {
        MetaFormat::Yaml => ("---", "---"),
        MetaFormat::Toml => ("+++", "+++"),
        MetaFormat::Json => ("---json", "---"),
        MetaFormat::Figl => ("---figl", "---"),
    };
    format!("{open}\n{content}{close}\n{body}")
}

/// A comment-preserving editor over a document's metadata, generic over where
/// the metadata lives.
pub enum MetaEditor {
    /// Editing a frontmatter block inside a host file.
    Fenced(Embed),
    /// Editing a config document (the whole file is the metadata), in the
    /// format its extension names.
    Whole(fig::Editor, MetaFormat),
}

impl MetaEditor {
    /// Open an editor over `text` for an existing carrier.
    pub fn open(text: &str, carrier: MetaCarrier) -> Result<Self> {
        Ok(match carrier {
            MetaCarrier::Fenced(_) => {
                let (_, block) = frontmatter_block(text).ok_or_else(|| {
                    Error::Structure("document has no embedded metadata block".into())
                })?;
                MetaEditor::Fenced(Embed::open(text.as_bytes(), &block)?)
            }
            MetaCarrier::WholeFile(format) => {
                MetaEditor::Whole(fig::Editor::open(text.as_bytes(), format.fig())?, format)
            }
        })
    }

    /// Open an editor over `text`, creating the metadata block when the
    /// document has none: an explicit carrier is honored (an absent
    /// frontmatter block is synthesized above the first line, in the carrier's
    /// format), and `None` defaults to a fresh `---` YAML block.
    pub fn open_or_init(text: &str, carrier: Option<MetaCarrier>) -> Result<Self> {
        let format = match carrier {
            Some(MetaCarrier::WholeFile(format)) => {
                return Ok(MetaEditor::Whole(
                    fig::Editor::open(text.as_bytes(), format.fig())?,
                    format,
                ));
            }
            Some(MetaCarrier::Fenced(format)) => format,
            None => MetaFormat::Yaml,
        };
        if let Some((_, block)) = frontmatter_block(text) {
            return Ok(MetaEditor::Fenced(Embed::open(text.as_bytes(), &block)?));
        }
        // No block yet: write an empty one above the first line and open that.
        // Empty is no content at all, except in JSON, where an empty document
        // does not parse and an empty object is the nothing an edit starts from.
        let seed = if format == MetaFormat::Json {
            "{}\n"
        } else {
            ""
        };
        let seeded = frontmatter_text(format, seed, text);
        let (_, block) = frontmatter_block(&seeded).ok_or_else(|| {
            Error::Structure(format!(
                "a new {format} frontmatter block did not read back"
            ))
        })?;
        Ok(MetaEditor::Fenced(Embed::open(seeded.as_bytes(), &block)?))
    }

    fn editor_mut(&mut self) -> &mut fig::Editor {
        match self {
            MetaEditor::Fenced(e) => e.editor_mut(),
            MetaEditor::Whole(e, _) => e,
        }
    }

    /// Upsert `value` at `path` (the trailing segment must be a key).
    pub fn set_value(&mut self, path: &[Segment], value: impl Into<fig::Value>) -> Result<()> {
        Ok(self.editor_mut().set_value(path, value)?)
    }

    /// Replace the existing value at `path`.
    pub fn replace_value(&mut self, path: &[Segment], value: impl Into<fig::Value>) -> Result<()> {
        Ok(self.editor_mut().replace_value(path, value)?)
    }

    /// Rename the key at `path`, keeping its value, position, and comments.
    pub fn replace_key(&mut self, path: &[Segment], key: &str) -> Result<()> {
        Ok(self.editor_mut().rename_key(path, key)?)
    }

    /// Append `value` to the sequence at `path`.
    pub fn append_value(&mut self, path: &[Segment], value: impl Into<fig::Value>) -> Result<()> {
        Ok(self.editor_mut().append_value(path, value)?)
    }

    /// Delete the mapping entry at `path`.
    pub fn delete(&mut self, path: &[Segment]) -> Result<()> {
        Ok(self.editor_mut().delete_key(path)?)
    }

    /// Remove the item at `index` from the sequence at `path`. A list that
    /// loses its last item stays, written as `[]`: the key is kept because a
    /// document's spanning key is what says it is an index, even with no
    /// children, and fig already empties a flow list, a JSON array and a TOML
    /// array to `[]`. A YAML block list alone would be left as a bare key,
    /// which reads as null.
    pub fn remove_item(&mut self, path: &[Segment], index: usize) -> Result<()> {
        self.editor_mut().delete_item(path, index)?;
        if self.is_null_at(path)? {
            self.replace_value(path, fig::Value::Seq(Vec::new()))?;
        }
        Ok(())
    }

    /// The format the metadata is edited in.
    fn format(&self) -> Option<MetaFormat> {
        match self {
            MetaEditor::Fenced(e) => MetaFormat::from_fig(e.dialect()),
            MetaEditor::Whole(_, format) => Some(*format),
        }
    }

    /// Whether the value at `path` in the edited metadata now reads as null —
    /// the trace an emptied YAML block list leaves.
    fn is_null_at(&mut self, path: &[Segment]) -> Result<bool> {
        let Some(format) = self.format() else {
            return Ok(false);
        };
        let source = self.editor_mut().source()?.to_owned();
        let Ok(meta) = prov_graph::meta::parse_value(&source, format) else {
            return Ok(false);
        };
        let mut value = &meta;
        for segment in path {
            let next = match segment {
                Segment::Key(key) => value.get(key),
                Segment::Index(i) => value.as_sequence().and_then(|s| s.get(*i)),
            };
            match next {
                Some(next) => value = next,
                None => return Ok(false),
            }
        }
        Ok(value.is_null())
    }

    /// Reorder the mapping entries at `path` (empty path = root) so `keys`
    /// come first, in that order; entries not listed keep their original
    /// relative order and follow. Unknown keys are ignored. Every entry keeps
    /// its comments and interleaved trivia.
    pub fn reorder_keys<S: AsRef<str>>(&mut self, path: &[Segment], keys: &[S]) -> Result<()> {
        Ok(self.editor_mut().reorder_keys(path, keys)?)
    }

    /// Reorder the sequence at `path` so the items at `indices` (positions in
    /// the current order) come first, in that order; items not listed keep
    /// their original relative order and follow. Out-of-range indices are
    /// ignored.
    pub fn reorder_items(&mut self, path: &[Segment], indices: &[usize]) -> Result<()> {
        Ok(self.editor_mut().reorder_items(path, indices)?)
    }

    /// Render the full document text with the edits applied.
    pub fn render(&mut self) -> Result<String> {
        Ok(match self {
            MetaEditor::Fenced(e) => String::from_utf8(e.render()?)
                .map_err(|_| Error::Structure("an edited frontmatter block is not UTF-8".into()))?,
            MetaEditor::Whole(e, _) => e.source()?.to_string(),
        })
    }
}

/// Parse a key path into the [`Address`] an edit is made at — the grammar of
/// [`Address::parse_command_line`]: `a.b[0].c` or `a.b.0.c`, each naming item
/// 0 of the list at `a.b`. An error when the path has a `[]` in it, which
/// names every item of a list rather than one value.
pub fn key_path(dotted: &str) -> Result<Address> {
    Address::parse_command_line(dotted).ok_or_else(|| {
        Error::Structure(format!(
            "`{dotted}` names every item of a list; an edit takes one value, `[n]` for item n"
        ))
    })
}

/// Interpret a CLI-provided scalar: `true`/`false`, integers, floats, and
/// `null` become their typed values; everything else stays a string.
///
/// A number carrying a **redundant leading zero** (`08`, `007`, `-04`) is not a
/// number here, because typing it does not round-trip: `08` parses to `8` and
/// the padding is gone for good, with no CLI spelling that gets it back. Padding
/// is meaningful in precisely the data this tool holds — a month `08`, a day
/// `04`, an all-digit id — so its presence is the evidence that the author meant
/// text. A lone `0` and a fraction's `0.5` are canonical and still type.
///
/// This is the general form of a hazard callers have been dodging one at a time:
/// [`identity`](prov_graph::identity) warns that an id may be all digits, and
/// prov-cli stamps ids as explicit string scalars to route around this function.
/// Those call sites are still right to be explicit — a value that must *never*
/// be typed should not depend on inference — but the trap is no longer here.
pub fn infer_scalar(s: &str) -> fig::Value {
    match s {
        "true" => fig::Value::Bool(true),
        "false" => fig::Value::Bool(false),
        "null" | "~" => fig::Value::Null,
        _ if is_zero_padded(s) => fig::Value::Str(s.to_string()),
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

/// Interpret a CLI-provided value: a flow list (`[public]`, `[a, b]`, `[]`)
/// becomes a sequence of [`infer_scalar`]ed items, and anything else is
/// [`infer_scalar`]'s.
///
/// The list form is what a person writes for a list-valued field —
/// `prov set note.md audience '[public]'` — and storing it as the string
/// `"[public]"` gave the field a value no reader of it takes for a list. An
/// item in quotes (`["a, b", 'c']`) is that text exactly, which is also how to
/// keep an item from being typed. A value with a bracket inside it
/// (`[[alias]]`, `[a, [b]]`) is not a flat list and stays a string, so a
/// wikilink set as a value is still a wikilink.
pub fn infer_value(s: &str) -> fig::Value {
    let trimmed = s.trim();
    let Some(inner) = trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    else {
        return infer_scalar(s);
    };
    if inner.contains(['[', ']', '\n']) {
        return infer_scalar(s);
    }
    let Some(items) = split_flow_items(inner) else {
        return infer_scalar(s);
    };
    fig::Value::Seq(items)
}

/// The items of a flow list's interior, or `None` when a quote is left open.
fn split_flow_items(inner: &str) -> Option<Vec<fig::Value>> {
    let mut items = Vec::new();
    if inner.trim().is_empty() {
        return Some(items);
    }
    let mut chars = inner.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let item = match chars.peek() {
            Some(&quote @ ('"' | '\'')) => {
                chars.next();
                let mut text = String::new();
                loop {
                    match chars.next()? {
                        c if c == quote => break,
                        c => text.push(c),
                    }
                }
                while chars.next_if(|c| c.is_whitespace()).is_some() {}
                if chars.peek().is_some_and(|c| *c != ',') {
                    return None;
                }
                fig::Value::Str(text)
            }
            _ => {
                let mut text = String::new();
                while let Some(c) = chars.next_if(|c| *c != ',') {
                    text.push(c);
                }
                infer_scalar(text.trim())
            }
        };
        items.push(item);
        if chars.next().is_none() {
            return Some(items);
        }
    }
}

/// Whether `s` opens with a zero a numeric parse would silently discard — `08`,
/// `-007`, `00`. The test is "zero followed by another digit", so the `0` of `0`
/// and of `0.5` (information, not padding) is not caught.
fn is_zero_padded(s: &str) -> bool {
    let mut digits = s.strip_prefix(['-', '+']).unwrap_or(s).chars();
    digits.next() == Some('0') && digits.next().is_some_and(|c| c.is_ascii_digit())
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
    let address = key_path(dotted)?;
    let path = address.segments();
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
fn value_at<'a>(
    meta: &'a prov_graph::meta::Value,
    dotted: &str,
) -> Option<&'a prov_graph::meta::Value> {
    let mut current = meta;
    for part in dotted.split('.') {
        current = current.as_mapping()?.get(part)?;
    }
    Some(current)
}

/// Delete the entry at `dotted` from `text`'s metadata (carrier-aware). A
/// path ending in an index (`contents[0]`) takes that item out of its list.
/// Returns the full re-rendered document text. Errors when the document has
/// no metadata or the path does not exist.
pub fn unset_in_text(text: &str, carrier: Option<MetaCarrier>, dotted: &str) -> Result<String> {
    let carrier = carrier
        .ok_or_else(|| Error::Structure("document has no embedded metadata block".into()))?;
    let mut editor = MetaEditor::open(text, carrier)?;
    let address = key_path(dotted)?;
    match address.as_item() {
        Some((list, index)) => editor.remove_item(&list.segments(), index)?,
        None => editor.delete(&address.segments())?,
    }
    editor.render()
}

/// Re-emit `mapping` as a fresh frontmatter block in `target` above the plain
/// `body` — the reconstruction a *format conversion* performs. Unlike the
/// comment-preserving edits above, this deliberately rebuilds the block: a
/// conversion crosses formats (a YAML comment has no JSON home), so only the
/// values survive.
///
/// The content is rendered by prov's canonical [`serialize_mapping`] — the
/// same serializer behind `prov meta --format`, so a converted block's
/// sequence and scalar layout matches the rest of the codebase (fig's per-key
/// splice renders some formats, notably figl sequences, differently).
///
/// [`serialize_mapping`]: prov_graph::meta::serialize_mapping
pub fn reformat_block(body: &str, mapping: &Mapping, target: MetaFormat) -> Result<String> {
    let mut inner = prov_graph::meta::serialize_mapping(mapping, target)?;
    // The content must end in exactly one newline for the closing fence to land
    // on its own line.
    if !inner.ends_with('\n') {
        inner.push('\n');
    }
    Ok(frontmatter_text(target, &inner, body))
}

/// Re-house `text`'s existing frontmatter block as a `target` one carrying
/// `mapping` — the conversion [`reformat_block`] performs for a document that
/// has no block yet, for one that does. Every host byte outside the block
/// survives. A document with no frontmatter gets a new block, as
/// [`reformat_block`] would give it.
pub fn retype_block(text: &str, mapping: &Mapping, target: MetaFormat) -> Result<String> {
    match prov_graph::Document::split(text) {
        Some((_, _, before, after)) => Ok(format!(
            "{before}{}",
            reformat_block(after, mapping, target)?
        )),
        None => reformat_block(text, mapping, target),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn carrier_of(path: &str, text: &str) -> Option<MetaCarrier> {
        prov_graph::document::Document::parse(path, text)
            .unwrap()
            .carrier
    }

    #[test]
    fn set_preserves_comments_and_format() {
        let text = "---\n# keep me\ntitle: Old\n---\nbody\n";
        let out =
            set_in_text(text, carrier_of("x.md", text), "title", infer_scalar("New")).unwrap();
        assert_eq!(out, "---\n# keep me\ntitle: New\n---\nbody\n");
    }

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

    #[test]
    fn set_in_a_figl_block_stays_figl() {
        let text = "---figl\ntitle = prov\n---\nbody\n";
        let out = set_in_text(
            text,
            carrier_of("x.md", text),
            "title",
            infer_scalar("renamed"),
        )
        .unwrap();
        assert_eq!(out, "---figl\ntitle = renamed\n---\nbody\n");
    }

    #[test]
    fn set_in_a_toml_block_keeps_its_plus_fences() {
        let text = "+++\ntitle = \"prov\"\n+++\nbody\n";
        let out = set_in_text(
            text,
            carrier_of("x.md", text),
            "title",
            infer_scalar("renamed"),
        )
        .unwrap();
        assert_eq!(out, "+++\ntitle = \"renamed\"\n+++\nbody\n");
    }

    #[test]
    fn a_new_block_is_written_in_the_carriers_format() {
        for (format, open) in [
            (MetaFormat::Yaml, "---\n"),
            (MetaFormat::Toml, "+++\n"),
            (MetaFormat::Json, "---json\n"),
            (MetaFormat::Figl, "---figl\n"),
        ] {
            let out = set_in_text(
                "body\n",
                Some(MetaCarrier::Fenced(format)),
                "title",
                infer_scalar("T"),
            )
            .unwrap();
            assert!(out.starts_with(open), "{format}: {out}");
            assert!(out.ends_with("body\n"), "{format}: {out}");
            let doc = prov_graph::Document::parse("x.md", &out).unwrap();
            assert_eq!(doc.carrier, Some(MetaCarrier::Fenced(format)), "{out}");
            assert_eq!(doc.body, "body\n", "{out}");
        }
    }

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

    #[test]
    fn set_creates_a_block_when_none_exists() {
        let out = set_in_text("just a body\n", None, "title", infer_scalar("T")).unwrap();
        assert!(out.starts_with("---\ntitle: T\n---\n"), "{out}");
        assert!(out.ends_with("just a body\n"));
    }

    #[test]
    fn unset_removes_only_the_named_key() {
        let text = "---\ntitle: T\ndraft: true\n---\nbody\n";
        let out = unset_in_text(text, carrier_of("x.md", text), "draft").unwrap();
        assert_eq!(out, "---\ntitle: T\n---\nbody\n");
        assert!(unset_in_text("no meta\n", None, "x").is_err());
    }

    #[test]
    fn removing_the_last_item_of_a_block_list_leaves_an_empty_list() {
        // fig empties a flow list, a JSON array and a TOML array to `[]`, but
        // a block list to `contents:` with nothing under it — a null, not a
        // list. Every shape now ends the same way.
        for (name, text, want) in [
            (
                "x.md",
                "---\ntitle: T\ncontents:\n- a.md\nnext: 1\n---\nbody\n",
                "---\ntitle: T\ncontents: []\nnext: 1\n---\nbody\n",
            ),
            (
                "x.md",
                "---\ntitle: T\ncontents: [a.md]\n---\nbody\n",
                "---\ntitle: T\ncontents: []\n---\nbody\n",
            ),
            (
                "x.yaml",
                "title: T\ncontents:\n  - a.md\nnext: 1\n",
                "title: T\ncontents: []\nnext: 1\n",
            ),
        ] {
            let out = unset_in_text(text, carrier_of(name, text), "contents[0]").unwrap();
            assert_eq!(out, want, "{name}");
        }
        // A list that keeps an item is untouched beyond the one removed.
        let text = "---\ncontents:\n- a.md\n- b.md\n---\n";
        let out = unset_in_text(text, carrier_of("x.md", text), "contents[0]").unwrap();
        assert_eq!(out, "---\ncontents:\n- b.md\n---\n");
    }

    #[test]
    fn scalars_are_inferred() {
        assert_eq!(infer_scalar("true"), fig::Value::Bool(true));
        assert_eq!(infer_scalar("42"), fig::Value::Int(42));
        assert_eq!(infer_scalar("4.5"), fig::Value::Float(4.5));
        assert_eq!(infer_scalar("null"), fig::Value::Null);
        assert_eq!(infer_scalar("hello"), fig::Value::Str("hello".into()));
    }

    #[test]
    fn a_flow_list_is_a_list() {
        let strs = |items: &[&str]| {
            fig::Value::Seq(items.iter().map(|s| fig::Value::Str((*s).into())).collect())
        };
        assert_eq!(infer_value("[public]"), strs(&["public"]));
        assert_eq!(
            infer_value("[public, friends]"),
            strs(&["public", "friends"])
        );
        assert_eq!(infer_value("[]"), fig::Value::Seq(Vec::new()));
        assert_eq!(
            infer_value("[1, true, 08]"),
            fig::Value::Seq(vec![
                fig::Value::Int(1),
                fig::Value::Bool(true),
                fig::Value::Str("08".into())
            ])
        );
        assert_eq!(infer_value(r#"["a, b", '1']"#), strs(&["a, b", "1"]));
        // Not a flat list: kept as the text it is.
        assert_eq!(
            infer_value("[[Some Note]]"),
            fig::Value::Str("[[Some Note]]".into())
        );
        assert_eq!(
            infer_value("[draft] notes"),
            fig::Value::Str("[draft] notes".into())
        );
        assert_eq!(
            infer_value(r#"["open]"#),
            fig::Value::Str(r#"["open]"#.into())
        );
        assert_eq!(infer_value("42"), fig::Value::Int(42));
    }

    #[test]
    fn a_list_set_from_the_cli_is_written_as_a_list() {
        let text = "---\ntitle: T\n---\n";
        let out = set_in_text(
            text,
            carrier_of("x.md", text),
            "audience",
            infer_value("[public]"),
        )
        .unwrap();
        assert!(
            !out.contains("'[public]'") && !out.contains("\"[public]\""),
            "{out}"
        );
        let doc = prov_graph::document::Document::parse("x.md", &out).unwrap();
        assert_eq!(
            doc.meta
                .get("audience")
                .and_then(|v| v.as_sequence())
                .map(|s| s.len()),
            Some(1),
            "{out}"
        );
    }

    #[test]
    fn zero_padding_survives_inference() {
        // `prov set <doc> title 08` on a month index used to write `title: 8`,
        // silently breaking the route that addressed it as "08" — and there was
        // no way to spell the string back. Padding means the author meant text.
        assert_eq!(infer_scalar("08"), fig::Value::Str("08".into()));
        assert_eq!(infer_scalar("007"), fig::Value::Str("007".into()));
        assert_eq!(infer_scalar("-04"), fig::Value::Str("-04".into()));
        assert_eq!(infer_scalar("00"), fig::Value::Str("00".into()));

        // A leading zero that carries information is still a number: a lone `0`,
        // and the one every fraction below 1 opens with.
        assert_eq!(infer_scalar("0"), fig::Value::Int(0));
        assert_eq!(infer_scalar("0.5"), fig::Value::Float(0.5));
        assert_eq!(infer_scalar("-0.25"), fig::Value::Float(-0.25));
    }

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

    // ---- MetaEditor parity across carriers: reorder_items/replace_key/reorder_keys ----

    #[test]
    fn replace_key_renames_the_key_and_preserves_comments_elsewhere() {
        let text = "---\n# keep me\ntitle: Old\nauthor: me\n---\nbody\n";
        let mut editor = MetaEditor::open(text, carrier_of("x.md", text).unwrap()).unwrap();
        editor
            .replace_key(&key_path("title").unwrap().segments(), "name")
            .unwrap();
        let out = editor.render().unwrap();
        assert!(out.contains("name: Old"), "{out}");
        assert!(!out.contains("title:"), "{out}");
        assert!(out.contains("# keep me"), "comment lost: {out}");
        assert!(out.contains("author: me"), "{out}");
    }

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

    #[test]
    fn reorder_items_moves_listed_items_first_and_preserves_comments() {
        let text = "---\ncontents:\n- a # keep a\n- b # keep b\n- c # keep c\n---\nbody\n";
        let mut editor = MetaEditor::open(text, carrier_of("x.md", text).unwrap()).unwrap();
        editor
            .reorder_items(&key_path("contents").unwrap().segments(), &[2, 0])
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
    #[test]
    fn a_mapping_lands_at_a_path_whose_ancestors_do_not_exist_yet() {
        let text = "title: prov config\nspec: 1\n";
        let carrier = carrier_of("config.yaml", text).unwrap();
        let mut view = prov_graph::meta::Mapping::new();
        view.insert(
            "group".into(),
            prov_graph::meta::Value::String("date".into()),
        );
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
    #[test]
    fn replacing_a_mapping_drops_the_keys_it_no_longer_declares() {
        let text = "title: t\ndiaryx:\n  views:\n    daily:\n      group: date\n      under: '[Daily](id:abc)'\n";
        let carrier = carrier_of("config.yaml", text).unwrap();
        let mut view = prov_graph::meta::Mapping::new();
        view.insert(
            "group".into(),
            prov_graph::meta::Value::String("date".into()),
        );

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
    #[test]
    fn a_sibling_mapping_survives_the_write() {
        let text = "title: t\ndiaryx:\n  views:\n    daily:\n      group: date\n";
        let carrier = carrier_of("config.yaml", text).unwrap();
        let mut view = prov_graph::meta::Mapping::new();
        view.insert(
            "group".into(),
            prov_graph::meta::Value::String("people".into()),
        );

        let out = set_meta_in_text(
            text,
            Some(carrier),
            "diaryx.views.folks",
            &prov_graph::meta::Value::Mapping(view),
        )
        .expect("a second view");

        let doc = prov_graph::Document::parse(std::path::Path::new("config.yaml"), &out).unwrap();
        assert_eq!(
            value_at(&doc.meta, "diaryx.views.daily.group")
                .and_then(prov_graph::meta::Value::as_str),
            Some("date"),
            "the first view survived: {out}"
        );
        assert_eq!(
            value_at(&doc.meta, "diaryx.views.folks.group")
                .and_then(prov_graph::meta::Value::as_str),
            Some("people")
        );
    }

    /// Nested mappings flatten all the way down, however deep.
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
    #[test]
    fn a_sequence_lands_as_a_sequence_at_a_path_with_no_parent_block() {
        let text = "title: t\n";
        let carrier = carrier_of("config.yaml", text).unwrap();
        let mut outer = prov_graph::meta::Mapping::new();
        outer.insert(
            "terms".into(),
            prov_graph::meta::Value::Sequence(vec![prov_graph::meta::Value::String(
                "public".into(),
            )]),
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

    /// The conversion `prov convert <file> metadata.format …` performs: the
    /// block changes language and fences, the values carry over, and the body
    /// is untouched.
    #[test]
    fn retyping_a_block_changes_its_language_and_keeps_the_body() {
        let text = "---\ntitle: T\ntags:\n- a\n---\n# Body\n";
        let doc = prov_graph::document::Document::parse("x.md", text).unwrap();
        let mapping = doc.meta.as_mapping().unwrap();
        for format in MetaFormat::ALL {
            let out = retype_block(text, mapping, format).unwrap();
            let back = prov_graph::document::Document::parse("x.md", &out).unwrap();
            assert_eq!(back.carrier, Some(MetaCarrier::Fenced(format)), "{out}");
            assert_eq!(back.meta, doc.meta, "{out}");
            assert_eq!(back.body, "# Body\n", "{out}");
        }
    }

    #[test]
    fn reformatting_a_body_gives_it_a_block_above() {
        let mut mapping = Mapping::new();
        mapping.insert("title".into(), prov_graph::Value::String("T".into()));
        let out = reformat_block("# Body\n", &mapping, MetaFormat::Json).unwrap();
        assert!(out.starts_with("---json\n"), "{out}");
        let back = prov_graph::document::Document::parse("x.md", &out).unwrap();
        assert_eq!(back.carrier, Some(MetaCarrier::Fenced(MetaFormat::Json)));
        assert_eq!(back.body, "# Body\n");
    }
}
