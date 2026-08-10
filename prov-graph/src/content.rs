//! Body-prose parsing via `twig` — prov's answer to the `content_format`
//! knob deferred in `docs/next-steps.md`, and the ingredient that makes
//! body-link findings code-aware (DESIGN §8's principle: a `[[…]]` that is
//! really code, e.g. `[[inf] * n for _ in range(m)]]` inside backticks, must
//! never be treated as a link).
//!
//! `twig` (a sister Zig-backed project, path-dependent for now) parses
//! Markdown/Djot into a shared AST. [`render_html`] and [`code_spans`] are
//! direct FFI calls into it — `twig`'s C ABI exposes `twig_document_render_html`
//! and the generic `twig_document_query`, no subprocess involved. (`code_spans`
//! used to bind a code-block-specific accessor; twig has since matured to a
//! single generic query API, so it now selects the code-bearing node kinds
//! itself.) `twig` is a required dependency, so these are always available.
//!
//! Pair [`code_spans`] with [`crate::link::scan_wikilinks`] (which is what
//! actually uses it) to keep a body-link scan from ever treating code as
//! prose.

use std::path::Path;

/// Which body-prose grammar a document is written in. Maps to a `twig`
/// [`twig::Format`] one-to-one; kept as prov's own type so callers can name
/// a format without depending on `twig` directly, e.g. for the `content_format`
/// config knob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentFormat {
    Markdown,
    Djot,
    Html,
}

impl ContentFormat {
    /// Infer the content format from a path's extension. `None` for anything
    /// unrecognized (including config extensions, which have no body).
    pub fn from_extension(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()? {
            "md" | "markdown" => Some(Self::Markdown),
            "dj" | "djot" => Some(Self::Djot),
            "html" | "htm" => Some(Self::Html),
            _ => None,
        }
    }

    fn twig_format(self) -> twig::Format {
        match self {
            Self::Markdown => twig::Format::Markdown,
            Self::Djot => twig::Format::Djot,
            Self::Html => twig::Format::Html,
        }
    }

    /// The canonical file extension for this grammar (no leading dot) — what a
    /// freshly authored document's filename gets when prov derives a name
    /// from a title (`prov new "A Title"`). The inverse of the primary
    /// [`from_extension`](Self::from_extension) spelling.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Djot => "dj",
            Self::Html => "html",
        }
    }

    /// The `content_format` config-document spelling for this grammar.
    pub fn as_config_str(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Djot => "djot",
            Self::Html => "html",
        }
    }

    /// Parse a `content_format` config value. Unknown → `None` (keep default).
    pub fn from_config_str(value: &str) -> Option<Self> {
        match value {
            "markdown" | "md" => Some(Self::Markdown),
            "djot" | "dj" => Some(Self::Djot),
            "html" | "htm" => Some(Self::Html),
            _ => None,
        }
    }
}

/// Parse `body` as `format` with `twig`. Shared by [`render_html`] and
/// [`code_spans`] so both go through the same error mapping.
fn parse(body: &str, format: ContentFormat) -> crate::error::Result<twig::Document> {
    twig::Document::parse_str(body, format.twig_format())
        .map_err(|e| crate::error::Error::Content(format!("twig parse: {e}")))
}

/// Transcode Markdown source into `format`.
///
/// The one move behind every page prov *authors* rather than reads. Generated
/// prose — `prov`'s `about`'s page, the history store's index and event bodies —
/// is written as Markdown in the Rust source, where it is legible to whoever
/// maintains it, and converted here to whatever grammar the workspace actually
/// uses. Without this, an HTML workspace ends up holding `.html` files whose
/// bodies are literal `# Heading` Markdown: prov reads them back fine, and every
/// other tool in the world does not.
///
/// Markdown is returned untouched — twig's serializer is idempotent here, but
/// round-tripping would buy nothing and risks reflowing prose the caller
/// deliberately wrapped.
pub fn transcode(body: &str, format: ContentFormat) -> crate::error::Result<String> {
    if format == ContentFormat::Markdown {
        return Ok(body.to_string());
    }
    let mut doc = parse(body, ContentFormat::Markdown)?;
    let out = doc
        .serialize(format.twig_format())
        .map_err(|e| crate::error::Error::Content(format!("twig serialize: {e}")))?;
    String::from_utf8(out)
        .map_err(|e| crate::error::Error::Content(format!("twig produced non-UTF-8: {e}")))
}

/// Parse `body` as `format` and render it to HTML, via `twig`'s FFI.
pub fn render_html(body: &str, format: ContentFormat) -> crate::error::Result<String> {
    let mut doc = parse(body, format)?;
    let html = doc
        .render_html()
        .map_err(|e| crate::error::Error::Content(format!("twig render: {e}")))?;
    String::from_utf8(html)
        .map_err(|e| crate::error::Error::Content(format!("twig produced non-UTF-8 HTML: {e}")))
}

/// The AST node kinds `twig` parses as opaque code — inline code spans
/// (`verbatim`), fenced/indented code blocks (`code_block`), and raw
/// inline/block escapes (`raw_inline` / `raw_block`). twig's selector grammar
/// has no union combinator, so [`code_spans`] queries these one at a time.
const CODE_KINDS: [&str; 4] = ["verbatim", "code_block", "raw_inline", "raw_block"];

/// The byte ranges in `body` that `twig` parses as code (inline code spans,
/// fenced code blocks, raw inline/block escapes) — everything a link scan
/// should treat as opaque. Built from `twig`'s generic query API
/// (`twig_document_query`) by selecting each code-bearing node kind
/// (`CODE_KINDS`) and taking its whole span; see
/// [`crate::link::scan_wikilinks`]. Spans are returned sorted by start offset.
pub fn code_spans(
    body: &str,
    format: ContentFormat,
) -> crate::error::Result<Vec<std::ops::Range<usize>>> {
    let mut doc = parse(body, format)?;
    let mut spans = Vec::new();
    for kind in CODE_KINDS {
        let matches = doc
            .query(kind)
            .map_err(|e| crate::error::Error::Content(format!("twig query {kind}: {e}")))?;
        spans.extend(matches.into_iter().map(|m| m.span));
    }
    spans.sort_by_key(|s| s.start);
    Ok(spans)
}

/// The byte ranges in `body` that `twig` parses as inline links — the whole
/// `[text](target)` construct of each `link` node, in source order. This is the
/// syntax-aware, code-aware complement to prov's lexical `[[…]]` scan: twig
/// never reports a `[x](y)` inside a code fence, an autolink's angle brackets,
/// or bracket text that is not actually a link, so a body-link scan built on
/// these spans cannot mistake prose or code for a link. The caller slices each
/// span and parses it with [`crate::link::Link::parse`] to read the target —
/// each span holds exactly one link, so the parse never over-reaches.
///
/// Reference-style and autolink forms also surface as `link` nodes; the caller
/// keeps only the inline `[label](target)` ones (a successful markdown parse),
/// which is the form prov can resolve and rewrite in place.
pub fn link_spans(
    body: &str,
    format: ContentFormat,
) -> crate::error::Result<Vec<std::ops::Range<usize>>> {
    let mut doc = parse(body, format)?;
    let mut spans: Vec<_> = doc
        .query("link")
        .map_err(|e| crate::error::Error::Content(format!("twig query link: {e}")))?
        .into_iter()
        .map(|m| m.span)
        .collect();
    spans.sort_by_key(|s| s.start);
    Ok(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_format_from_extension() {
        assert_eq!(
            ContentFormat::from_extension(Path::new("a.md")),
            Some(ContentFormat::Markdown)
        );
        assert_eq!(
            ContentFormat::from_extension(Path::new("a.markdown")),
            Some(ContentFormat::Markdown)
        );
        assert_eq!(
            ContentFormat::from_extension(Path::new("a.dj")),
            Some(ContentFormat::Djot)
        );
        assert_eq!(
            ContentFormat::from_extension(Path::new("a.djot")),
            Some(ContentFormat::Djot)
        );
        assert_eq!(
            ContentFormat::from_extension(Path::new("a.html")),
            Some(ContentFormat::Html)
        );
        assert_eq!(
            ContentFormat::from_extension(Path::new("a.htm")),
            Some(ContentFormat::Html)
        );
        assert_eq!(ContentFormat::from_extension(Path::new("a.yaml")), None);
        assert_eq!(ContentFormat::from_extension(Path::new("noext")), None);
    }

    #[test]
    fn extension_matches_the_primary_from_extension_spelling() {
        // The derived filename's extension must round-trip back to the same format.
        for f in [
            ContentFormat::Markdown,
            ContentFormat::Djot,
            ContentFormat::Html,
        ] {
            let name = format!("derived.{}", f.extension());
            assert_eq!(ContentFormat::from_extension(Path::new(&name)), Some(f));
        }
    }

    #[test]
    fn renders_markdown_to_html_via_twig_ffi() {
        let html = render_html("# hi\n", ContentFormat::Markdown).unwrap();
        assert_eq!(html, "<h1>hi</h1>\n");
    }

    #[test]
    fn renders_djot_to_html_via_twig_ffi() {
        let html = render_html("_hi_\n", ContentFormat::Djot).unwrap();
        assert_eq!(html, "<p><em>hi</em></p>\n");
    }

    #[test]
    fn renders_html_via_twig_ffi() {
        let html = render_html("<p>hi</p>", ContentFormat::Html).unwrap();
        assert!(html.contains("hi"));
    }

    #[test]
    fn link_spans_find_inline_links_but_not_code_or_prose() {
        let body = "See [the doc](notes/a.md) and `[not](a link)` and plain [text].";
        let spans = link_spans(body, ContentFormat::Markdown).unwrap();
        // The real inline link is found, its span the whole `[label](target)`.
        let want = body.find("[the doc](notes/a.md)").unwrap();
        assert!(
            spans
                .iter()
                .any(|s| s.start == want && &body[s.clone()] == "[the doc](notes/a.md)"),
            "expected the inline link span, got {spans:?}"
        );
        // The backtick-wrapped `[not](a link)` is code, not a link.
        let code_at = body.find("[not]").unwrap();
        assert!(
            !spans.iter().any(|s| s.contains(&code_at)),
            "code must not be a link: {spans:?}"
        );
        // `[text]` with no destination is not a link either.
        let bracket_at = body.find("[text]").unwrap();
        assert!(
            !spans.iter().any(|s| s.contains(&bracket_at)),
            "bare brackets are not a link"
        );
    }

    #[test]
    fn link_spans_read_djot_links_too() {
        let body = "Here is [a link](../b.dj) inline.\n";
        let spans = link_spans(body, ContentFormat::Djot).unwrap();
        assert!(
            spans
                .iter()
                .any(|s| &body[s.clone()] == "[a link](../b.dj)"),
            "djot inline link span, got {spans:?}"
        );
    }

    #[test]
    fn code_spans_cover_verbatim_but_not_prose() {
        let body = "See [[colophon:abc123]] and `[[inf] * n for _ in range(m)]]` here.";
        let spans = code_spans(body, ContentFormat::Markdown).unwrap();

        // The plain wikilink is untouched by any code span...
        let wikilink_span = body.find("[[colophon:abc123]]").unwrap()
            ..body.find("[[colophon:abc123]]").unwrap() + "[[colophon:abc123]]".len();
        assert!(
            !spans
                .iter()
                .any(|cs| cs.start < wikilink_span.end && wikilink_span.start < cs.end)
        );

        // ...but the backtick-wrapped one is inside exactly one code span.
        let code_start = body.find('`').unwrap();
        let code_end = body.rfind('`').unwrap() + 1;
        assert!(
            spans
                .iter()
                .any(|cs| cs.start <= code_start && code_end <= cs.end)
        );
    }
}
