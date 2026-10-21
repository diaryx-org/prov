//! Documents — a plaintext file with an embedded metadata block and a body,
//! or a config file whose *entire content* is the metadata.
//!
//! The two shapes are one model: a config file is simply a document whose
//! metadata carrier is the whole file and whose body is empty. Both parse to
//! the same [`Document`], link through the same relations, and participate in
//! traversal, validation, and mutation identically — which is what lets a
//! workspace mix prose documents and config documents in one tree.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::meta::{self, Value};

use crate::format::MetaFormat;

/// A document's prose body, and the file it was read from.
///
/// The two halves travel together because a *separated* document keeps them in
/// different files: the text comes from the `content` target while the metadata
/// stays in the node, so a caller that renders the prose needs to know which
/// path declares its grammar. For a combined document [`path`](Body::path) is
/// simply the document's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Body {
    /// The prose itself. Empty for a document that has none — a config
    /// document standing for itself, or a manifest node standing for a
    /// directory.
    pub text: String,
    /// The file the prose lives in: the document's own path when combined, its
    /// [`content_path`](Document::content_path) when separated. What
    /// [`ContentFormat::from_extension`](crate::content::ContentFormat::from_extension)
    /// should be asked about.
    pub path: PathBuf,
}

/// Where a document's metadata physically lives — recorded at parse time so a
/// write can preserve the original carrier exactly (a `+++` TOML block is never
/// rewritten as `---` YAML; a bare `.yaml` file never grows fences).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaCarrier {
    /// A frontmatter block opening a host file, with the prose body after it:
    /// `---` YAML, `+++` TOML, or `---<lang>` in any of the four formats
    /// (`---json`, `---figl`, …). twig locates it (`twig::meta`) and keeps its
    /// fences byte for byte on an edit.
    Fenced(MetaFormat),
    /// The entire file is the metadata (a config document); the body is empty.
    /// The format comes from the file extension.
    WholeFile(MetaFormat),
}

impl MetaCarrier {
    /// The format the metadata is written in.
    pub fn format(&self) -> MetaFormat {
        match self {
            MetaCarrier::Fenced(format) | MetaCarrier::WholeFile(format) => *format,
        }
    }
}

/// The whole-file metadata format implied by `path`'s extension, if any.
/// These are the extensions prov treats as config documents: `.yaml`/`.yml`,
/// `.json`, `.toml`, and `.figl` (or fig 5's `.fig`).
pub fn whole_file_format(path: &Path) -> Option<MetaFormat> {
    MetaFormat::from_extension(path.extension()?.to_str()?)
}

/// Enforce that a **record store** at `path` (the id registry, the recycle-bin
/// index, a flat vocabulary) is a whole-file config document, returning its
/// format. A [`MetaCarrier::Fenced`] carrier — markdown frontmatter — is
/// rejected with [`Error::MarkdownStore`](crate::error::Error::MarkdownStore): prov re-lays-out these stores as a
/// sorted record list (DESIGN §5), so human prose has no stable home in them and
/// unambiguous extension→format sniffing depends on the carrier being whole-file.
/// The single choke point every store loader passes through, so the rule cannot
/// be enforced in one place and forgotten in another.
pub fn require_whole_file(path: &Path, carrier: MetaCarrier) -> Result<MetaFormat> {
    match carrier {
        MetaCarrier::WholeFile(format) => Ok(format),
        MetaCarrier::Fenced(_) => Err(crate::error::Error::MarkdownStore(path.to_path_buf())),
    }
}

/// Whether prov can read `path` as text — a recognized body format
/// (Markdown/Djot/HTML) or a whole-file metadata format (YAML/JSON/…). Its
/// negation is an **opaque payload**: a file prov treats as bytes (an image,
/// a PDF, a font, any binary) and never parses. An *attachment* is exactly a
/// whole-file metadata sidecar whose `content` points at such a payload, which
/// is how an arbitrary file gains workspace-linked metadata without being able
/// to carry frontmatter itself.
pub fn is_opaque_payload(path: &Path) -> bool {
    crate::content::ContentFormat::from_extension(path).is_none()
        && whole_file_format(path).is_none()
}

/// The canonical whole-file extension for a metadata `format` — the inverse of
/// [`whole_file_format`]. Used when materializing a whole-file metadata document
/// (a config/registry sidecar, or the metadata half of a *separated* document):
/// `yaml`, `json`, `toml`, `figl`.
pub fn whole_file_extension(format: MetaFormat) -> &'static str {
    format.extension()
}

/// The frontmatter carrier for `format` — the block a new document gets when it
/// inherits no parent block and the workspace default is `format`. twig writes
/// its fences: `---` for YAML, `+++` for TOML, `---json` and `---figl` for the
/// others.
pub fn frontmatter_carrier(format: MetaFormat) -> MetaCarrier {
    MetaCarrier::Fenced(format)
}

/// The *family* a workspace authors embedded metadata in — the "embed type" the
/// CLI's `init` prompts for, one level above the [`MetaCarrier`]. A family plus a
/// [`MetaFormat`] resolves to a carrier through [`embed_carrier`]: (`Delimited`,
/// TOML) is a `+++` block, and (`Separate`, JSON) is a whole-file `.json`
/// sidecar. It is what the config document records (via `prov`'s
/// `WorkspaceConfig`) so a workspace stays self-describing about *how* its
/// metadata is embedded, not just which format it is written in.
///
/// Through prov 0.18 there were three more: fenced code blocks (with
/// ```` ```endmatter ````), HTML `<script>` islands and HTML `<pre><code>`
/// blocks, and `;;;` JSON was a delimiter. fig 6 took embedding out of fig, and
/// twig, which locates a document's metadata now, reads frontmatter alone; a
/// workspace config naming one of the others reads as the default, with a
/// config finding saying so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedStyle {
    /// Delimited frontmatter: `---` YAML, `+++` TOML, `---json`, `---figl`.
    Delimited,
    /// Metadata kept in a sibling *whole-file* document, joined to a plain body
    /// file by a `content` attribute — neither file carries fences.
    Separate,
}

impl EmbedStyle {
    /// The `embed_type` config-document spelling for this style.
    pub fn as_config_str(self) -> &'static str {
        match self {
            EmbedStyle::Delimited => "delimited",
            EmbedStyle::Separate => "separate",
        }
    }

    /// Parse an `embed_type` config value. Unknown → `None` (keep the default).
    pub fn from_config_str(value: &str) -> Option<Self> {
        Some(match value {
            "delimited" => EmbedStyle::Delimited,
            "separate" => EmbedStyle::Separate,
            _ => return None,
        })
    }
}

/// The [`EmbedStyle`] *family* a carrier belongs to — the inverse (on the style
/// axis) of [`embed_carrier`], which resolves a `(style, format)` pair back to a
/// carrier. Pairing it with a new metadata format is how a *format conversion*
/// keeps a document's embedding shape while changing only its frontmatter
/// language: classify the current carrier, then [`embed_carrier`] the same style
/// with the target format.
pub fn embed_style_of(carrier: MetaCarrier) -> EmbedStyle {
    match carrier {
        MetaCarrier::Fenced(_) => EmbedStyle::Delimited,
        MetaCarrier::WholeFile(_) => EmbedStyle::Separate,
    }
}

/// Resolve an [`EmbedStyle`] + metadata `format` to the carrier a new document
/// should get: a frontmatter block in `format`, or a whole-file sidecar in it.
/// Every pair has one.
pub fn embed_carrier(style: EmbedStyle, format: MetaFormat) -> MetaCarrier {
    match style {
        EmbedStyle::Delimited => MetaCarrier::Fenced(format),
        EmbedStyle::Separate => MetaCarrier::WholeFile(format),
    }
}

/// The metadata block of `text`, located by twig, and the metadata format its
/// fence names. `None` when `text` has no block, its opening fence is never
/// closed, or the fence names a language prov does not read (a `---zon`
/// block is prose to prov, as it would be to a Markdown renderer).
///
/// The block is the frontmatter opening `text`, found by parsing it as
/// Markdown, which reads frontmatter whatever the host's own grammar is: it is
/// the document's first lines and nothing an extension changes. A text with no
/// frontmatter whose first non-blank byte opens a tag is an HTML page, and its
/// block is the first `<script type="application/<lang>">` data island in it,
/// found by parsing it as HTML. An island may sit anywhere, typically in
/// `<head>`, and is edited where it sits.
pub fn frontmatter_block(text: &str) -> Option<(MetaFormat, twig::meta::Block)> {
    let block = located(text, twig_markdown::format()).or_else(|| {
        text.trim_start()
            .starts_with('<')
            .then(|| located(text, twig_html::format()))
            .flatten()
    })?;
    let format = MetaFormat::from_fig(twig::meta::dialect_for(&block.lang)?)?;
    Some((format, block))
}

/// `text`'s front metadata block, parsing it as `format`.
fn located(text: &str, format: twig::Format) -> Option<twig::meta::Block> {
    let parsed = twig::meta::parse_host(text.as_bytes(), format).ok()?;
    twig::meta::find(&parsed.doc, twig::meta::Where::Front)
}

/// A parsed document: its path, its embedded metadata, and its body text.
///
/// Metadata is stored as a dynamic [`Value`] (a mapping, or [`Value::Null`] when
/// the document has no frontmatter) because link fields are configurable and
/// therefore accessed dynamically.
#[derive(Debug, Clone)]
pub struct Document {
    /// Path this document was read from (workspace-relative or absolute — the
    /// caller decides; prov does not interpret it here).
    pub path: PathBuf,
    /// Parsed embedded metadata.
    pub meta: Value,
    /// Everything outside the metadata block (the host prose). Empty for a
    /// config document.
    pub body: String,
    /// Where the metadata was found, or `None` when the document has no
    /// (well-formed) metadata. Preserved on write.
    pub carrier: Option<MetaCarrier>,
}

impl Document {
    /// Parse a document from its full text.
    ///
    /// If `path` has a config extension (`.yaml`, `.yml`, `.json`, `.fig`,
    /// `.figl`), the entire text is the metadata and the body is empty.
    /// Otherwise the document's frontmatter is located by twig
    /// ([`frontmatter_block`]) — `---` YAML, `+++` TOML, or `---<lang>` — and
    /// parsed in the format its fence names. If there is no (well-formed) block, `meta`
    /// is [`Value::Null`] and the whole text is the body. An unterminated
    /// opening fence is treated as no metadata — we do not guess where it
    /// ends.
    pub fn parse(path: impl Into<PathBuf>, text: &str) -> Result<Self> {
        let path = path.into();
        if let Some(format) = whole_file_format(&path) {
            let meta = meta::parse_value(text, format)?;
            return Ok(Self {
                path,
                meta,
                body: String::new(),
                carrier: Some(MetaCarrier::WholeFile(format)),
            });
        }
        let (meta, body, carrier) = match Self::split(text) {
            Some((carrier, block, before, after)) => (
                meta::parse_value(block, carrier.format())?,
                // Both host sides, in file order. Frontmatter leaves `before`
                // empty; keeping the join is what keeps `body` honest should a
                // block ever sit mid-document again.
                [before, after].concat(),
                Some(carrier),
            ),
            // No block, or one never closed: an unterminated fence degrades to
            // "no metadata", and the whole text is the body.
            None => (Value::Null, text.to_owned(), None),
        };
        Ok(Self {
            path,
            meta,
            body,
            carrier,
        })
    }

    /// Zero-copy counterpart to [`parse`](Self::parse): locate a fenced
    /// metadata block in `text` without parsing it, returning the
    /// [`MetaCarrier`] found and the three slices it borrows from `text` —
    /// `(meta_block, body_before, body_after)` — the same location `parse`
    /// builds its owned, parsed [`Value`] from.
    ///
    /// The body comes back in **two** pieces because a block need not sit at an
    /// edge. Frontmatter always leaves `body_before` empty today, but through
    /// prov 0.18 an HTML `<script>` island could sit below a `<head>`, and a
    /// caller splicing text back together wants the two sides separate, since
    /// only their offsets say where the block sat. Concatenated in order they
    /// are [`Document::body`].
    ///
    /// Only recognizes a *fenced* carrier — a whole-file (config) document has
    /// no split to offer, since its entire text already is the metadata; a
    /// caller steering by path extension (as `parse` does via
    /// [`whole_file_format`]) should check that first. Returns `None` when
    /// `text` opens no frontmatter, or its opening fence has no matching
    /// close (an unterminated fence degrades to "no metadata", matching
    /// `parse`).
    ///
    /// The caller who wants the parsed [`Value`] should use [`parse`](Self::parse)
    /// instead; this exists for one who wants to defer parsing to their own
    /// deserializer, or just needs the raw borrowed text (e.g. to detect which
    /// archetype a document uses without allocating).
    pub fn split(text: &str) -> Option<(MetaCarrier, &str, &str, &str)> {
        let (format, block) = frontmatter_block(text)?;
        // twig's span ends with the closing fence itself; its line ending is the
        // block's too, not the body's first byte.
        let rest = &text[block.span.end..];
        let end = block.span.end
            + if rest.starts_with("\r\n") {
                2
            } else {
                usize::from(rest.starts_with('\n'))
            };
        Some((
            MetaCarrier::Fenced(format),
            &text[block.body],
            &text[..block.span.start],
            &text[end..],
        ))
    }

    /// The document's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `true` if the document declares any embedded metadata mapping.
    pub fn has_meta(&self) -> bool {
        self.meta.as_mapping().is_some()
    }

    /// The raw `content` attribute — the relative path to a *separated*
    /// document's body file — or `None` for an ordinary (combined) document
    /// whose body is [`self.body`](Document::body). A separated document is a
    /// whole-file metadata document (`.yaml`/`.json`/`.figl`) that points at its
    /// prose body in a sibling file, keeping both halves plain text and linked.
    pub fn content_attr(&self) -> Option<&str> {
        self.meta.get("content").and_then(Value::as_str)
    }

    /// The raw `manifest` attribute — the relative path to the manifest
    /// document listing the files this node stands for — or `None` for a node
    /// that stands for itself.
    ///
    /// The bulk counterpart of [`content_attr`](Document::content_attr) and
    /// **mutually exclusive** with it: a node covers one payload or a set of
    /// them, never both. See [`manifest`](crate::manifest) for the record shape.
    pub fn manifest_attr(&self) -> Option<&str> {
        self.meta
            .get(crate::manifest::MANIFEST_KEY)
            .and_then(Value::as_str)
    }

    /// `true` when this document is a **manifest node**: it declares a
    /// `manifest` pointer, so the files it stands for are listed there rather
    /// than being a single `content` payload.
    pub fn is_manifest_node(&self) -> bool {
        self.manifest_attr().is_some()
    }

    /// `true` when this document declares *both* `content` and `manifest` —
    /// a node claiming to be a single payload's sidecar and a whole
    /// directory's at once. Neither reading is safe to pick, so the pair is
    /// reported rather than resolved.
    pub fn manifest_conflicts(&self) -> bool {
        self.content_attr().is_some() && self.manifest_attr().is_some()
    }

    /// The path of this document's separated body file — its `content` target
    /// joined onto its own directory — or `None` for a combined document, whose
    /// prose is [`self.body`](Document::body).
    ///
    /// Plain path joining, deliberately: `content` names a file *beside* the
    /// node (§5's placement rule), so this is the one link-ish value with no
    /// root-absolute spelling to honour, and staying free of the workspace-root
    /// coordinate is what lets a caller holding a real filesystem path — the
    /// CLI reading a file it was handed — resolve it the same way a caller
    /// holding a workspace-relative one does. Matches the resolution
    /// `attach`'s reverse lookup and the mutation verbs already make.
    pub fn content_path(&self, doc_path: &Path) -> Option<PathBuf> {
        let dir = doc_path.parent().unwrap_or(Path::new(""));
        Some(crate::link::normalize(dir.join(self.content_attr()?)))
    }

    /// The path of the file that actually holds this document's prose: its
    /// [`content_path`](Document::content_path) when separated, and `doc_path`
    /// itself when combined.
    ///
    /// This is the path whose extension declares the body's grammar
    /// ([`ContentFormat::from_extension`](crate::content::ContentFormat::from_extension)).
    /// Reading that off a separated document's *own* path asks a `.yaml` node
    /// what grammar its prose is in, and the honest answer — "none, a config
    /// file has no body" — is the wrong question rather than a wrong answer.
    pub fn body_path(&self, doc_path: &Path) -> PathBuf {
        self.content_path(doc_path)
            .unwrap_or_else(|| doc_path.to_path_buf())
    }

    /// `true` when this document is an **attachment sidecar**: a whole-file
    /// metadata document whose `content` points at an [opaque
    /// payload](is_opaque_payload) rather than a prose body. Recognized two ways,
    /// so a hand-written sidecar need not be verbose: an explicit `attachment:
    /// true` flag (what `prov`'s `Workspace::attach` writes),
    /// **or** a `content` target whose extension prov cannot read as text.
    ///
    /// A *separated prose* document (`content` → a `.md`/`.dj`/`.html` body) is
    /// deliberately **not** an attachment: its body is a prov document in its
    /// own right, scanned for links and titles; an attachment's payload is bytes
    /// prov never opens.
    pub fn is_attachment(&self) -> bool {
        match self.content_attr() {
            None => false,
            Some(content) => {
                self.meta.get("attachment").and_then(Value::as_bool) == Some(true)
                    || is_opaque_payload(Path::new(content))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_path_joins_the_target_onto_the_node_s_own_directory() {
        let doc = Document::parse("notes/a.yaml", "title: A\ncontent: a.md\n").unwrap();
        assert_eq!(
            doc.content_path(Path::new("notes/a.yaml")),
            Some(PathBuf::from("notes/a.md"))
        );
        assert_eq!(
            doc.body_path(Path::new("notes/a.yaml")),
            PathBuf::from("notes/a.md")
        );
    }

    /// The CLI resolves by real filesystem path, so the join must not assume a
    /// workspace-root coordinate.
    #[test]
    fn content_path_resolves_an_absolute_node_path_as_absolute() {
        let doc = Document::parse("/vault/notes/a.yaml", "title: A\ncontent: a.md\n").unwrap();
        assert_eq!(
            doc.content_path(Path::new("/vault/notes/a.yaml")),
            Some(PathBuf::from("/vault/notes/a.md"))
        );
    }

    #[test]
    fn a_combined_document_has_no_content_path_and_is_its_own_body_path() {
        let doc = Document::parse("notes/a.md", "---\ntitle: A\n---\nprose\n").unwrap();
        assert_eq!(doc.content_path(Path::new("notes/a.md")), None);
        assert_eq!(
            doc.body_path(Path::new("notes/a.md")),
            PathBuf::from("notes/a.md")
        );
    }

    #[test]
    fn parses_yaml_frontmatter_and_body() {
        let text = "---\ntitle: Root\ncontents:\n- a.md\n---\n# Body\n\nhello\n";
        let doc = Document::parse("index.md", text).unwrap();
        assert_eq!(doc.meta.get("title").and_then(Value::as_str), Some("Root"));
        assert_eq!(doc.body, "# Body\n\nhello\n");
        assert_eq!(doc.carrier, Some(MetaCarrier::Fenced(MetaFormat::Yaml)));
        assert!(doc.has_meta());
    }

    #[test]
    fn parses_figl_frontmatter_under_either_name() {
        for fence in ["---figl", "---fig"] {
            let text = format!("{fence}\ntitle = prov\ncontents = [docs/design.md]\n---\n# Body\n");
            let doc = Document::parse("README.md", &text).unwrap();
            assert_eq!(doc.meta.get("title").and_then(Value::as_str), Some("prov"));
            assert_eq!(doc.body, "# Body\n");
            assert_eq!(doc.carrier, Some(MetaCarrier::Fenced(MetaFormat::Figl)));
        }
    }

    #[test]
    fn parses_json_and_toml_frontmatter() {
        let doc =
            Document::parse("note.md", "---json\n{\"title\": \"Root\"}\n---\nbody\n").unwrap();
        assert_eq!(doc.meta.get("title").and_then(Value::as_str), Some("Root"));
        assert_eq!(doc.carrier, Some(MetaCarrier::Fenced(MetaFormat::Json)));
        assert_eq!(doc.body, "body\n");

        let doc = Document::parse("note.md", "+++\ntitle = \"Root\"\n+++\nbody\n").unwrap();
        assert_eq!(doc.meta.get("title").and_then(Value::as_str), Some("Root"));
        assert_eq!(doc.carrier, Some(MetaCarrier::Fenced(MetaFormat::Toml)));
    }

    /// The archetypes fig 5 read and twig does not are prose now: a document
    /// carrying one reads as having no metadata, its whole text the body. An
    /// HTML `<script>` data island is not among them: twig reads it again.
    #[test]
    fn the_retired_archetypes_read_as_prose() {
        for text in [
            ";;;\n{\"title\": \"Root\"}\n;;;\nbody\n",
            "```fig\ntitle = prov\n```\n# Body\n",
            "# Body first\n```endmatter\ntitle: Tail\n```\n",
        ] {
            let doc = Document::parse("note.md", text).unwrap();
            assert_eq!(doc.carrier, None, "{text:?}");
            assert_eq!(doc.body, text);
        }
    }

    #[test]
    fn a_fence_naming_a_language_prov_does_not_read_is_prose() {
        let text = "---zon\n.{ .title = \"x\" }\n---\nbody\n";
        let doc = Document::parse("note.md", text).unwrap();
        assert_eq!(doc.carrier, None);
        assert_eq!(doc.body, text);
    }

    #[test]
    fn a_config_file_is_a_document_whose_content_is_all_metadata() {
        let text = "title: ID registry\npart_of: index.md\nregistry:\n  abc: a.md\n";
        let doc = Document::parse("registry.yaml", text).unwrap();
        assert_eq!(
            doc.meta.get("title").and_then(Value::as_str),
            Some("ID registry")
        );
        assert_eq!(
            doc.meta.get("part_of").and_then(Value::as_str),
            Some("index.md")
        );
        assert_eq!(doc.body, "");
        assert_eq!(doc.carrier, Some(MetaCarrier::WholeFile(MetaFormat::Yaml)));
        assert!(doc.has_meta());
    }

    #[test]
    fn a_fig_config_file_parses_the_dialect() {
        let text = "title = settings\npart_of = index.md\n";
        for path in ["settings.figl", "settings.fig"] {
            let doc = Document::parse(path, text).unwrap();
            assert_eq!(
                doc.meta.get("title").and_then(Value::as_str),
                Some("settings")
            );
            assert_eq!(doc.carrier, Some(MetaCarrier::WholeFile(MetaFormat::Figl)));
        }
    }

    #[test]
    fn embed_style_config_str_round_trips() {
        for style in [EmbedStyle::Delimited, EmbedStyle::Separate] {
            assert_eq!(
                EmbedStyle::from_config_str(style.as_config_str()),
                Some(style)
            );
        }
        assert_eq!(EmbedStyle::from_config_str("nonsense"), None);
        // The styles twig does not read are no longer spellings.
        for retired in ["code_block", "html_script", "html_code"] {
            assert_eq!(EmbedStyle::from_config_str(retired), None);
        }
    }

    #[test]
    fn embed_carrier_resolves_style_and_format_to_a_carrier() {
        for format in MetaFormat::ALL {
            let fenced = embed_carrier(EmbedStyle::Delimited, format);
            assert_eq!(fenced, MetaCarrier::Fenced(format));
            assert_eq!(embed_style_of(fenced), EmbedStyle::Delimited);
            let whole = embed_carrier(EmbedStyle::Separate, format);
            assert_eq!(whole, MetaCarrier::WholeFile(format));
            assert_eq!(embed_style_of(whole), EmbedStyle::Separate);
        }
    }

    #[test]
    fn no_frontmatter_is_all_body() {
        let doc = Document::parse("note.md", "# Just a note\n").unwrap();
        assert!(doc.meta.is_null());
        assert_eq!(doc.body, "# Just a note\n");
        assert_eq!(doc.carrier, None);
        assert!(!doc.has_meta());
    }

    #[test]
    fn unterminated_fence_is_not_frontmatter() {
        let text = "---\ntitle: oops\nno closing fence\n";
        let doc = Document::parse("x.md", text).unwrap();
        assert!(doc.meta.is_null());
        assert_eq!(doc.body, text);
        assert_eq!(doc.carrier, None);
    }

    #[test]
    fn split_borrows_yaml_frontmatter_and_body_without_parsing() {
        let text = "---\ntitle: Root\n---\n# Body\n\nhello\n";
        let (carrier, meta, before, after) = Document::split(text).unwrap();
        assert_eq!(carrier, MetaCarrier::Fenced(MetaFormat::Yaml));
        assert_eq!(meta, "title: Root\n");
        assert_eq!(before, "", "frontmatter has no host text above it");
        assert_eq!(after, "# Body\n\nhello\n");
        let body = after;
        // Byte-identical to what `parse` extracts, just unparsed and borrowed.
        let doc = Document::parse("x.md", text).unwrap();
        assert_eq!(doc.body, body);
    }

    #[test]
    fn split_handles_crlf_line_endings() {
        let text = "---\r\ntitle: Root\r\n---\r\nbody\r\n";
        let (carrier, meta, before, after) = Document::split(text).unwrap();
        assert_eq!(carrier, MetaCarrier::Fenced(MetaFormat::Yaml));
        assert_eq!(meta, "title: Root\r\n");
        assert_eq!(before, "");
        assert_eq!(after, "body\r\n");
    }

    #[test]
    fn split_is_none_with_no_frontmatter() {
        assert_eq!(Document::split("# Just a note\n"), None);
    }

    #[test]
    fn split_is_none_for_an_unterminated_fence() {
        let text = "---\ntitle: oops\nno closing fence\n";
        assert_eq!(Document::split(text), None);
    }

    #[test]
    fn split_recognizes_a_non_yaml_carrier() {
        let text = "---figl\ntitle = prov\n---\n# Body\n";
        let (carrier, meta, before, after) = Document::split(text).unwrap();
        assert_eq!(carrier, MetaCarrier::Fenced(MetaFormat::Figl));
        assert_eq!(meta, "title = prov\n");
        assert_eq!(before, "");
        assert_eq!(after, "# Body\n");
    }

    #[test]
    fn an_html_page_reads_its_script_island_as_its_metadata() {
        let text = "<script type=\"application/yaml\">\ntitle: Page\n</script>\n<p>Body</p>\n";
        let doc = Document::parse("index.html", text).unwrap();
        assert_eq!(doc.carrier, Some(MetaCarrier::Fenced(MetaFormat::Yaml)));
        assert_eq!(doc.meta.get("title").and_then(Value::as_str), Some("Page"));
        assert_eq!(doc.body, "<p>Body</p>\n");
    }

    #[test]
    fn an_island_in_head_splits_the_page_around_it() {
        let text = "<html>\n<head>\n<script type=\"application/json\">\n{\"title\": \"T\"}\n</script>\n</head>\n</html>\n";
        let (carrier, meta, before, after) = Document::split(text).unwrap();
        assert_eq!(carrier, MetaCarrier::Fenced(MetaFormat::Json));
        assert_eq!(meta, "{\"title\": \"T\"}\n");
        assert_eq!(before, "<html>\n<head>\n");
        assert_eq!(after, "</head>\n</html>\n");
    }

    #[test]
    fn a_page_script_that_is_not_an_island_is_body() {
        let text = "<script>let a = 1;</script>\n<p>Body</p>\n";
        assert_eq!(Document::split(text), None);
    }

    #[test]
    fn a_djot_document_reads_its_frontmatter_too() {
        let text = "---\ntitle: Dj\n---\n_body_\n";
        let doc = Document::parse("note.dj", text).unwrap();
        assert_eq!(doc.meta.get("title").and_then(Value::as_str), Some("Dj"));
        assert_eq!(doc.body, "_body_\n");
    }

    #[test]
    fn crlf_fences_are_handled() {
        let text = "---\r\ntitle: Root\r\n---\r\nbody\r\n";
        let doc = Document::parse("x.md", text).unwrap();
        assert_eq!(doc.carrier, Some(MetaCarrier::Fenced(MetaFormat::Yaml)));
        assert_eq!(doc.body, "body\r\n");
        // Exact scalar — fig treats \r\n as a single line break.
        assert_eq!(doc.meta.get("title").and_then(Value::as_str), Some("Root"));
    }
}
