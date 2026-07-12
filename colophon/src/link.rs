//! Link text — the raw strings a relation field holds, the wikilinks embedded
//! in body prose, and the path arithmetic around them.
//!
//! A link target as written in metadata is either a bare path or a
//! markdown-style labeled link (`[Design](docs/design.md)`), and its path may be
//! **relative** to the document (`notes/a.md`), **workspace-absolute** from the
//! root (`/Blog/Blog.md`), or wrapped in Markdown **angle brackets** when it
//! contains spaces (`</Creative Writing/index.md>`, `[Notes](</My Notes/x.md>)`).
//! This is colophon's *link-syntax layer* — the analogue of `fig`'s format
//! layer: it recognizes the conventions a real workspace mixes and round-trips
//! them on write (spaces re-acquire their brackets). A [`Wikilink`] is the
//! body-text counterpart (`[[notes/a.md]]`, `[[colophon:ajp7eq|My file]]`).
//! Everything here is *lexical*: no filesystem
//! access, no symlink resolution, and no markdown-structure awareness (a `[[…]]`
//! inside a code span is still scanned) — resolution and code-fence discipline
//! belong to the traversal and validation layers, which can report what they
//! find.

use std::ops::Range;
use std::path::{Component, Path, PathBuf};

/// A parsed link string: an optional human label and the target it points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The display label, when written as `[label](target)` or `[[target|label]]`.
    pub label: Option<String>,
    /// The target exactly as written (a relative path, an `id:<id>` handle, or a
    /// URL for overlay relations that point off-workspace).
    pub target: String,
    /// `true` when the scalar was written as an Obsidian wikilink
    /// (`[[target]]` / `[[target|label]]`) rather than a markdown link or bare
    /// target — preserved so [`render`](Link::render) round-trips the wrapper.
    pub wikilink: bool,
}

impl Link {
    /// Parse a raw link string. `[label](target)` yields both parts; anything
    /// else is a bare target with no label. A target wrapped in Markdown angle
    /// brackets (`<…>`, used when it contains spaces) is unwrapped, so
    /// [`target`](Link::target) always holds the logical path — leading `/` and
    /// all — never the delimiters.
    pub fn parse(raw: &str) -> Self {
        let raw = raw.trim();
        // A wikilink scalar — `[[target]]` / `[[target|label]]` — the Obsidian
        // wrapper permitted in metadata as well as body prose.
        if let Some(inner) = raw.strip_prefix("[[").and_then(|r| r.strip_suffix("]]")) {
            let (target, label) = match inner.split_once('|') {
                Some((target, label)) => (target.trim(), Some(label.trim().to_string())),
                None => (inner.trim(), None),
            };
            return Self {
                label,
                target: target.to_string(),
                wikilink: true,
            };
        }
        if let Some(rest) = raw.strip_prefix('[')
            && let Some(inner) = rest.strip_suffix(')')
            && let Some((label, target)) = inner.split_once("](")
        {
            return Self {
                label: Some(label.to_string()),
                target: unbracket(target),
                wikilink: false,
            };
        }
        Self {
            label: None,
            target: unbracket(raw),
            wikilink: false,
        }
    }

    /// Render back to a writable link string. A labeled link keeps its label and
    /// wraps the URL in Markdown angle brackets when it holds a space or paren
    /// (so `]` / `)` in the path cannot break parsing); a bare target is emitted
    /// verbatim — brackets belong *inside* `[label](…)`, never around a bare
    /// value (matching diaryx, which reads a bare `<…>` as a literal path).
    pub fn render(&self) -> String {
        match (&self.label, self.wikilink) {
            (Some(label), true) => format!("[[{}|{label}]]", self.target),
            (None, true) => format!("[[{}]]", self.target),
            (Some(label), false) => format!("[{label}]({})", emit_target(&self.target)),
            (None, false) => self.target.clone(),
        }
    }

    /// This link with a different target, keeping the label and wrapper. The
    /// rename path uses this so `[Design](old.md)` becomes `[Design](new.md)`,
    /// never a bare `new.md`.
    pub fn with_target(&self, target: impl Into<String>) -> Self {
        Self {
            label: self.label.clone(),
            target: target.into(),
            wikilink: self.wikilink,
        }
    }

    /// `true` when the target points off-workspace (a URL or mail address)
    /// rather than at a file — such links are never resolved against the
    /// filesystem or rewritten by moves.
    pub fn is_external(&self) -> bool {
        self.target.contains("://") || self.target.starts_with("mailto:")
    }

    /// The stable ID this link names, when the target uses the `id:<id>`
    /// scheme (or the legacy `colophon:<id>` spelling) — the
    /// location-independent alternative to a relative path. Such targets
    /// resolve through the workspace's ID registry, never against the
    /// filesystem, and are deliberately *not* rewritten by moves: staying valid
    /// across moves is their entire point.
    pub fn id_target(&self) -> Option<crate::identity::Id> {
        strip_id_scheme(&self.target).map(|id| crate::identity::Id(id.to_string()))
    }
}

/// Strip one pair of Markdown angle-bracket delimiters (`<target>` → `target`),
/// the convention for a target containing spaces. Any other target is returned
/// unchanged.
fn unbracket(target: &str) -> String {
    target
        .strip_prefix('<')
        .and_then(|inner| inner.strip_suffix('>'))
        .unwrap_or(target)
        .to_string()
}

/// The writable spelling of a Markdown-link URL: wrapped in angle brackets when
/// it holds a space or parenthesis (which would otherwise break `[label](url)`),
/// bare otherwise. For URLs *inside* `[label](…)` only.
fn emit_target(target: &str) -> String {
    if target.contains([' ', '(', ')']) {
        format!("<{target}>")
    } else {
        target.to_string()
    }
}

/// The write style for links a workspace authors — colophon's analogue of
/// diaryx's `LinkFormat`, and read from the same place: the `link_format` key in
/// the root document's frontmatter (a fact declared *in* the workspace, not an
/// app-private config). Every link colophon writes (autofix today; create/rename
/// in time) uses this, so a repair never introduces a foreign style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LinkStyle {
    /// `[Title](/path/file.md)` — workspace-absolute Markdown link. diaryx's
    /// default, and the most portable/self-documenting.
    #[default]
    MarkdownRoot,
    /// `[Title](../path/file.md)` — relative Markdown link.
    MarkdownRelative,
    /// `../path/file.md` — bare relative path.
    PlainRelative,
    /// `path/file.md` — bare workspace-relative (canonical) path.
    PlainCanonical,
}

impl LinkStyle {
    /// Parse the `link_format` frontmatter value (diaryx's snake_case spelling).
    /// Unknown or absent → `None`, so callers can fall back to the default.
    pub fn from_config_str(value: &str) -> Option<Self> {
        match value {
            "markdown_root" => Some(Self::MarkdownRoot),
            "markdown_relative" => Some(Self::MarkdownRelative),
            "plain_relative" => Some(Self::PlainRelative),
            "plain_canonical" => Some(Self::PlainCanonical),
            _ => None,
        }
    }

    /// The `link_format` config spelling (the inverse of [`from_config_str`]).
    ///
    /// [`from_config_str`]: LinkStyle::from_config_str
    pub fn as_config_str(self) -> &'static str {
        match self {
            Self::MarkdownRoot => "markdown_root",
            Self::MarkdownRelative => "markdown_relative",
            Self::PlainRelative => "plain_relative",
            Self::PlainCanonical => "plain_canonical",
        }
    }
}

/// Format a link to `target` (a workspace-relative canonical path) as written in
/// the document at `from`, in `style`, with `title` (used only by the Markdown
/// styles). This is what keeps an authored link native to the workspace.
pub fn format_link(style: LinkStyle, from: &Path, target: &Path, title: &str) -> String {
    let canonical = target.to_string_lossy();
    match style {
        LinkStyle::MarkdownRoot => {
            format!("[{title}]({})", emit_target(&format!("/{canonical}")))
        }
        LinkStyle::MarkdownRelative => {
            let rel = relative(from.parent().unwrap_or(Path::new("")), target);
            format!("[{title}]({})", emit_target(&rel))
        }
        LinkStyle::PlainRelative => relative(from.parent().unwrap_or(Path::new("")), target),
        LinkStyle::PlainCanonical => canonical.into_owned(),
    }
}

/// A human title generated from a path's file stem: `_`/`-` become spaces and
/// each word is capitalized (`utility_index.md` → `Utility Index`). The fallback
/// when a target document declares no `title`.
pub fn path_to_title(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    stem.split(['_', '-', ' '])
        .filter(|w| !w.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The target scheme marking a link-by-ID: `id:<id>`.
pub const ID_SCHEME: &str = "id:";

/// The legacy scheme (`colophon:<id>`), still recognized on read so existing
/// workspaces keep resolving. New links are authored with [`ID_SCHEME`].
pub const LEGACY_ID_SCHEME: &str = "colophon:";

/// Strip the ID scheme from a target, accepting the current `id:` spelling or
/// the legacy `colophon:` one. `None` when the target names no ID.
pub fn strip_id_scheme(target: &str) -> Option<&str> {
    target.strip_prefix(ID_SCHEME).or_else(|| target.strip_prefix(LEGACY_ID_SCHEME))
}

/// Render an ID as a link target (`id:<id>`).
pub fn id_target(id: &crate::identity::Id) -> String {
    format!("{ID_SCHEME}{id}")
}

/// The syntactic wrapper a reference is written in — the first of the two style
/// axes (see `docs/reference-styles.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Wrapper {
    /// The diaryx/markdown family: `[Title](target)` or a bare target, with the
    /// exact path rendering governed by [`ReferenceStyle::path_style`].
    #[default]
    Markdown,
    /// The Obsidian wikilink: `[[target]]` / `[[target|label]]`.
    Wikilink,
}

impl Wrapper {
    /// Parse the `reference_wrapper` config spelling; unknown → `None`.
    pub fn from_config_str(value: &str) -> Option<Self> {
        match value {
            "markdown" => Some(Self::Markdown),
            "wikilink" => Some(Self::Wikilink),
            _ => None,
        }
    }

    /// The `reference_wrapper` config spelling.
    pub fn as_config_str(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Wikilink => "wikilink",
        }
    }
}

/// What a reference addresses its target *by* — the second style axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Addressing {
    /// By path — rewritten on every move; rendering follows
    /// [`ReferenceStyle::path_style`].
    #[default]
    Path,
    /// By durable `id:<id>` handle — move-stable; authoring one registers the
    /// target (the link-by-id trigger).
    Id,
    /// By the target's title/name, resolved nominally through the title index —
    /// readable but not move/rename-safe, and never registers. Implies
    /// [`Wrapper::Wikilink`].
    Alias,
}

impl Addressing {
    /// Parse the `reference_target` config spelling; unknown → `None`.
    pub fn from_config_str(value: &str) -> Option<Self> {
        match value {
            "path" => Some(Self::Path),
            "id" => Some(Self::Id),
            "alias" => Some(Self::Alias),
            _ => None,
        }
    }

    /// The `reference_target` config spelling.
    pub fn as_config_str(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Id => "id",
            Self::Alias => "alias",
        }
    }
}

/// How a durable reference is spelled: a [`Wrapper`], an [`Addressing`], whether
/// an `id` link carries a title label, and the path rendering used when
/// addressing by path. This is the per-workspace default *and* the per-relation
/// override (see [`crate::relation::Relation::style`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReferenceStyle {
    /// The syntactic wrapper.
    pub wrapper: Wrapper,
    /// What the reference addresses its target by.
    pub addressing: Addressing,
    /// Whether an `id` wikilink carries a `|Title` label (a maintained cache of
    /// the target's title). Ignored for markdown (its `[Title]` is intrinsic)
    /// and for `alias` (the target string *is* the title).
    pub label: bool,
    /// Path rendering for [`Addressing::Path`] — ignored otherwise.
    pub path_style: LinkStyle,
}

impl Default for ReferenceStyle {
    /// Markdown path links in the default [`LinkStyle`] — the pre-existing
    /// behavior, so an unconfigured workspace is unchanged.
    fn default() -> Self {
        Self {
            wrapper: Wrapper::Markdown,
            addressing: Addressing::Path,
            label: false,
            path_style: LinkStyle::default(),
        }
    }
}

impl ReferenceStyle {
    /// Normalize impossible combinations: `alias` has no markdown spelling (there
    /// is no locator to put in `[Title](…)`), so a markdown+alias request becomes
    /// wikilink+alias.
    pub fn normalized(mut self) -> Self {
        if self.addressing == Addressing::Alias {
            self.wrapper = Wrapper::Wikilink;
        }
        self
    }

    /// Whether authoring in this style is a *link-by-id* — the trigger that
    /// registers the target. Only `id` addressing registers.
    pub fn registers(self) -> bool {
        self.addressing == Addressing::Id
    }
}

/// Render a durable reference from the document at `from` to `to` (titled
/// `title`) in `style`. `id` must be `Some` when the style addresses by id
/// (the caller registers the target first); it is ignored otherwise. Returns the
/// exact scalar to store in metadata (a wikilink scalar keeps its `[[…]]` — the
/// metadata writer is responsible for any format-level quoting).
pub fn format_reference(
    style: ReferenceStyle,
    from: &Path,
    to: &Path,
    id: Option<&crate::identity::Id>,
    title: &str,
) -> String {
    let style = style.normalized();
    match style.addressing {
        // No id available (identity off / does not register on link) degrades to
        // a path link, mirroring the pre-existing `authored_target` fallback.
        Addressing::Id => match id {
            Some(id) => wrap(style.wrapper, &id_target(id), title, style.label),
            None => format_path(Wrapper::Markdown, style.path_style, from, to, title, style.label),
        },
        Addressing::Alias => wrap_alias(title),
        Addressing::Path => format_path(style.wrapper, style.path_style, from, to, title, style.label),
    }
}

/// Render a path reference in `wrapper` at `path_style`.
fn format_path(
    wrapper: Wrapper,
    path_style: LinkStyle,
    from: &Path,
    to: &Path,
    title: &str,
    label: bool,
) -> String {
    match wrapper {
        // Preserve the exact markdown/plain behavior (labeled vs bare) that
        // `LinkStyle` already encodes — the `label` axis does not apply here.
        Wrapper::Markdown => format_link(path_style, from, to, title),
        Wrapper::Wikilink => wrap(Wrapper::Wikilink, &path_text(path_style, from, to), title, label),
    }
}

/// The bare path *text* a path reference points at, in the shape `path_style`
/// selects: workspace-absolute (`/canonical`), relative, or canonical.
pub fn path_text(path_style: LinkStyle, from: &Path, to: &Path) -> String {
    match path_style {
        LinkStyle::MarkdownRoot => format!("/{}", to.to_string_lossy()),
        LinkStyle::MarkdownRelative | LinkStyle::PlainRelative => {
            relative(from.parent().unwrap_or(Path::new("")), to)
        }
        LinkStyle::PlainCanonical => to.to_string_lossy().into_owned(),
    }
}

/// Wrap a resolved `target` (already scheme-/path-formatted) in `wrapper`,
/// attaching `title` as a label when `with_label`. A markdown reference without
/// a label is emitted bare (`id:xxx`) — the diaryx-shaped id link.
fn wrap(wrapper: Wrapper, target: &str, title: &str, with_label: bool) -> String {
    match (wrapper, with_label) {
        (Wrapper::Wikilink, true) => format!("[[{target}|{title}]]"),
        (Wrapper::Wikilink, false) => format!("[[{target}]]"),
        (Wrapper::Markdown, true) => format!("[{title}]({})", emit_target(target)),
        (Wrapper::Markdown, false) => emit_target(target),
    }
}

/// An alias reference: the title itself, as a bare-name wikilink.
fn wrap_alias(title: &str) -> String {
    format!("[[{title}]]")
}

/// A wikilink embedded in a document's body: `[[target]]` or, with an Obsidian
/// pipe label, `[[target|label]]`.
///
/// This is the body-text sibling of a metadata [`Link`]. The `target` is either
/// a path (`[[notes/a.md]]`, the identity-free Diaryx-style link that moves
/// rewrite) or a `colophon:<id>` reference (`[[colophon:ajp7eq]]`, the
/// location-independent Obsidian-style link that moves leave alone — the
/// registry update is its maintenance). Which one a workspace mints is a policy
/// choice; discovering the span is not, so the scanner is neutral between them.
///
/// [`span`](Wikilink::span) is the byte range of the whole `[[…]]` construct in
/// the source body — exactly what a rewrite replaces, so a path retarget can
/// splice a new target back in without re-parsing the prose around it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wikilink {
    /// The target as written between `[[` and the `|` (or the closing `]]`),
    /// trimmed of surrounding whitespace.
    pub target: String,
    /// The display label after `|`, when written `[[target|label]]`.
    pub label: Option<String>,
    /// Byte range of the entire `[[…]]` span within the scanned body.
    pub span: Range<usize>,
}

impl Wikilink {
    /// Build a wikilink from the raw inner text (between `[[` and `]]`) and its
    /// span. Splits an Obsidian `target|label` on the first `|`; an empty target
    /// (e.g. `[[]]` or `[[ | x ]]`) is not a link and yields `None`.
    fn from_inner(inner: &str, span: Range<usize>) -> Option<Self> {
        let (target, label) = match inner.split_once('|') {
            Some((target, label)) => (target.trim(), Some(label.trim().to_string())),
            None => (inner.trim(), None),
        };
        if target.is_empty() {
            return None;
        }
        Some(Self {
            target: target.to_string(),
            label,
            span,
        })
    }

    /// Render back to `[[target]]` / `[[target|label]]`. Surrounding whitespace
    /// inside the brackets is not preserved — the rendered form is canonical.
    pub fn render(&self) -> String {
        match &self.label {
            Some(label) => format!("[[{}|{label}]]", self.target),
            None => format!("[[{}]]", self.target),
        }
    }

    /// This wikilink with a different target, keeping the label — the move path
    /// uses it to rewrite a *path* target while leaving the display text intact.
    /// (ID targets are never rewritten; that is the whole point of using one.)
    pub fn with_target(&self, target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            label: self.label.clone(),
            span: self.span.clone(),
        }
    }

    /// The stable ID this wikilink names, when its target uses the `id:<id>`
    /// scheme (or the legacy `colophon:<id>` spelling) — `None` for a plain path
    /// target. Mirrors [`Link::id_target`].
    pub fn id_target(&self) -> Option<crate::identity::Id> {
        strip_id_scheme(&self.target).map(|id| crate::identity::Id(id.to_string()))
    }
}

/// Scan body prose for every `[[…]]` wikilink, in source order, each carrying
/// its byte span. Purely lexical: unclosed `[[` is ignored, the first following
/// `]]` closes the span, and no markdown structure (code spans, escapes) is
/// interpreted — a higher layer decides whether a match in a code fence counts.
pub fn parse_wikilinks(body: &str) -> Vec<Wikilink> {
    let mut out = Vec::new();
    let mut base = 0; // byte offset of `rest` within `body`
    let mut rest = body;
    while let Some(open_rel) = rest.find("[[") {
        let open = base + open_rel;
        let after_open = open + 2;
        let Some(close_rel) = body[after_open..].find("]]") else {
            break; // no closing delimiter anywhere ahead — nothing more to find
        };
        let close = after_open + close_rel;
        if let Some(link) = Wikilink::from_inner(&body[after_open..close], open..close + 2) {
            out.push(link);
        }
        base = close + 2;
        rest = &body[base..];
    }
    out
}

/// Keep only the wikilinks in `links` whose span does not overlap any of
/// `code_spans` — the code-awareness DESIGN §8 asks for: a `[[…]]` that is
/// really code (inside a fenced/inline code span) must never be treated as a
/// link.
///
/// **Caveat:** this only helps for a `links` list in which every real
/// wikilink was already found as its own match. It cannot rescue a real
/// `[[…]]` that [`parse_wikilinks`]' greedy "next `]]` wins" scan has already
/// merged into one bogus match together with an unrelated `[[` earlier in
/// the same code span — by the time that happens, the real link was never
/// emitted as a separate [`Wikilink`] to keep. [`scan_wikilinks`] avoids the
/// problem at the source (it never lets a lexical scan cross a code span in
/// the first place) and is what `census`/`check`/rename actually use; reach
/// for this function only when you already have a trustworthy `Vec<Wikilink>`
/// (e.g. from a segment [`scan_wikilinks`] itself produced) and just need the
/// range check.
pub fn exclude_code_spans(links: Vec<Wikilink>, code_spans: &[Range<usize>]) -> Vec<Wikilink> {
    links
        .into_iter()
        .filter(|link| {
            !code_spans
                .iter()
                .any(|cs| cs.start < link.span.end && link.span.start < cs.end)
        })
        .collect()
}

/// Scan `body` for wikilinks the way `census`/`check`/the rename machinery
/// actually should — never [`parse_wikilinks`] directly. When colophon was
/// built with the `content` feature and `path`'s extension names a format
/// `twig` understands, every code span (fenced/inline code, raw escapes) is
/// treated as opaque *before* the lexical `[[`…`]]` scan ever sees it: each
/// prose run between code spans is scanned on its own and the results
/// stitched back into `body`-relative spans. Without that feature, or for an
/// unrecognized extension, this is exactly [`parse_wikilinks`] over the whole
/// body — the same behavior as before code-awareness existed.
///
/// Scanning prose runs *separately*, rather than scanning the whole body and
/// filtering the results (what [`exclude_code_spans`] alone can do), matters:
/// [`parse_wikilinks`]' greedy scan finds each `[[` a *later* `]]`, code or
/// not, closes — so one stray `[[` in a code block (a Python
/// `[[float('inf')] * width ...]`, DESIGN §8's motivating example, life-sized)
/// can eat every `]]` after it, including a real `[[gone.md]]` further down
/// the body, merging them into one bogus match that swallows the real link
/// whole. No post-hoc filter can get that link back — it was never emitted
/// as its own match. Keeping code spans out of the scan in the first place
/// is the only fix; this function is that fix.
pub fn scan_wikilinks(path: &Path, body: &str) -> Vec<Wikilink> {
    match code_spans_for(path, body) {
        Some(spans) if !spans.is_empty() => scan_outside_spans(body, &merge_spans(spans)),
        _ => parse_wikilinks(body),
    }
}

/// Sort-then-merge overlapping/adjacent ranges. `code_spans_for`'s sources
/// don't currently nest or overlap (code-block/verbatim/raw nodes are AST
/// leaves), but merging first keeps [`scan_outside_spans`] correct even if
/// that ever changes, and collapses touching spans into one gap-free skip.
fn merge_spans(mut spans: Vec<Range<usize>>) -> Vec<Range<usize>> {
    spans.sort_by_key(|s| s.start);
    let mut out: Vec<Range<usize>> = Vec::with_capacity(spans.len());
    for span in spans {
        match out.last_mut() {
            Some(prev) if span.start <= prev.end => prev.end = prev.end.max(span.end),
            _ => out.push(span),
        }
    }
    out
}

/// Run [`parse_wikilinks`] independently on each run of `body` outside
/// `code_spans` (sorted, non-overlapping), then shift each match's span back
/// to `body`-relative coordinates before stitching the runs' results
/// together in source order. This is what keeps a `[[` inside a code span
/// from ever being in the same scan as prose that follows it.
fn scan_outside_spans(body: &str, code_spans: &[Range<usize>]) -> Vec<Wikilink> {
    let mut out = Vec::new();
    let mut cursor = 0;
    for span in code_spans {
        if cursor < span.start {
            out.extend(shift_spans(parse_wikilinks(&body[cursor..span.start]), cursor));
        }
        cursor = cursor.max(span.end);
    }
    if cursor < body.len() {
        out.extend(shift_spans(parse_wikilinks(&body[cursor..]), cursor));
    }
    out
}

fn shift_spans(links: Vec<Wikilink>, offset: usize) -> Vec<Wikilink> {
    links
        .into_iter()
        .map(|link| Wikilink { span: link.span.start + offset..link.span.end + offset, ..link })
        .collect()
}

fn code_spans_for(path: &Path, body: &str) -> Option<Vec<Range<usize>>> {
    let format = crate::content::ContentFormat::from_extension(path)?;
    // A twig failure degrades to "no spans" rather than aborting the scan —
    // code-awareness is a refinement, the purely lexical scan above is
    // always a safe fallback. An unrecognized extension is `None` (via
    // `from_extension`), scanning the whole body as before.
    crate::content::code_spans(body, format).ok()
}

/// Lexically normalize a relative path: drop `.` components and fold
/// `parent/..` pairs. Leading `..` components (escaping the workspace root)
/// are kept — the caller decides whether that is an error.
pub fn normalize(path: impl AsRef<Path>) -> PathBuf {
    let mut out: Vec<Component> = Vec::new();
    for component in path.as_ref().components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                _ => out.push(component),
            },
            other => out.push(other),
        }
    }
    out.iter().collect()
}

/// Resolve a link target written in `doc` to a normalized path in the same
/// coordinate system as `doc` (workspace-relative when `doc` is). A target with
/// a leading `/` is **workspace-absolute** — resolved from the root, not `doc`'s
/// directory, and never against the filesystem root; any other target is
/// relative to `doc`'s directory.
pub fn resolve(doc: &Path, target: &str) -> PathBuf {
    if let Some(from_root) = target.strip_prefix('/') {
        return normalize(from_root);
    }
    let dir = doc.parent().unwrap_or(Path::new(""));
    normalize(dir.join(target))
}

/// The relative path string that reaches `to` from `from_dir` (both normalized,
/// same coordinate system). Rendered with forward slashes — link targets are
/// text, not platform paths.
pub fn relative(from_dir: &Path, to: &Path) -> String {
    let from: Vec<&std::ffi::OsStr> = from_dir.iter().collect();
    let to_parts: Vec<&std::ffi::OsStr> = to.iter().collect();
    let common = from
        .iter()
        .zip(to_parts.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let mut parts: Vec<String> = Vec::new();
    for _ in common..from.len() {
        parts.push("..".to_string());
    }
    for part in &to_parts[common..] {
        parts.push(part.to_string_lossy().into_owned());
    }
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_labeled_and_bare_links() {
        let l = Link::parse("[Design](docs/design.md)");
        assert_eq!(l.label.as_deref(), Some("Design"));
        assert_eq!(l.target, "docs/design.md");
        assert_eq!(l.render(), "[Design](docs/design.md)");

        let bare = Link::parse("notes/a.md");
        assert_eq!(bare.label, None);
        assert_eq!(bare.render(), "notes/a.md");
    }

    #[test]
    fn odd_shapes_fall_back_to_bare() {
        // A target with brackets but not the [label](target) shape.
        for raw in ["[unclosed](x", "no[mid](x)", "[]"] {
            assert_eq!(Link::parse(raw).render(), raw);
        }
    }

    #[test]
    fn with_target_keeps_the_label() {
        let l = Link::parse("[Design](old.md)").with_target("new.md");
        assert_eq!(l.render(), "[Design](new.md)");
    }

    #[test]
    fn external_links_are_flagged() {
        assert!(Link::parse("https://example.com/x").is_external());
        assert!(Link::parse("[me](mailto:a@b.c)").is_external());
        assert!(!Link::parse("docs/design.md").is_external());
    }

    #[test]
    fn parses_angle_bracketed_and_absolute_targets() {
        // Diaryx-style: a labeled link to an angle-bracketed, workspace-absolute
        // path containing a space.
        let l = Link::parse("[Archived Documents](</Archive/Archived documents.md>)");
        assert_eq!(l.label.as_deref(), Some("Archived Documents"));
        assert_eq!(l.target, "/Archive/Archived documents.md");
        // Round-trips: the space forces the angle brackets back on render.
        assert_eq!(l.render(), "[Archived Documents](</Archive/Archived documents.md>)");

        // A bare angle-bracketed target is unwrapped on read (lenient) and
        // written back bare (diaryx reads a bare `<…>` as a literal path).
        let bare = Link::parse("</Creative Writing/Creative Writing.md>");
        assert_eq!(bare.target, "/Creative Writing/Creative Writing.md");
        assert_eq!(bare.render(), "/Creative Writing/Creative Writing.md");

        // An absolute path without spaces needs no brackets, and stays bare.
        let plain = Link::parse("[Blog](/Blog/Blog.md)");
        assert_eq!(plain.target, "/Blog/Blog.md");
        assert_eq!(plain.render(), "[Blog](/Blog/Blog.md)");
    }

    #[test]
    fn formats_links_in_each_workspace_style() {
        let from = Path::new("School/MATH 213/hw.md");
        let target = Path::new("School/Archive/MATH 213 files.md");
        // MarkdownRoot: absolute, titled, angle-bracketed for the space.
        assert_eq!(
            format_link(LinkStyle::MarkdownRoot, from, target, "MATH 213 Files"),
            "[MATH 213 Files](</School/Archive/MATH 213 files.md>)"
        );
        // MarkdownRelative: relative, titled.
        assert_eq!(
            format_link(LinkStyle::MarkdownRelative, from, target, "MATH 213 Files"),
            "[MATH 213 Files](<../Archive/MATH 213 files.md>)"
        );
        // Plain styles: bare, no title.
        assert_eq!(
            format_link(LinkStyle::PlainRelative, from, target, "ignored"),
            "../Archive/MATH 213 files.md"
        );
        assert_eq!(
            format_link(LinkStyle::PlainCanonical, from, target, "ignored"),
            "School/Archive/MATH 213 files.md"
        );
    }

    #[test]
    fn link_style_reads_the_config_spelling_and_titles_fall_back_to_the_path() {
        assert_eq!(LinkStyle::from_config_str("markdown_root"), Some(LinkStyle::MarkdownRoot));
        assert_eq!(LinkStyle::from_config_str("plain_canonical"), Some(LinkStyle::PlainCanonical));
        assert_eq!(LinkStyle::from_config_str("nonsense"), None);
        assert_eq!(LinkStyle::default(), LinkStyle::MarkdownRoot);
        assert_eq!(path_to_title(Path::new("Folder/utility_index.md")), "Utility Index");
        assert_eq!(path_to_title(Path::new("README.md")), "README");
    }

    #[test]
    fn resolves_workspace_absolute_paths_from_the_root() {
        // A leading slash means "from the workspace root", regardless of where
        // the linking document sits — and never the filesystem root.
        assert_eq!(
            resolve(Path::new("Meta/Meta files.md"), "/Blog/Blog.md"),
            PathBuf::from("Blog/Blog.md")
        );
        assert_eq!(
            resolve(Path::new("deep/nested/doc.md"), "/Resume.md"),
            PathBuf::from("Resume.md")
        );
        // Relative targets still resolve against the document's own directory.
        assert_eq!(
            resolve(Path::new("Meta/Meta files.md"), "../Blog/Blog.md"),
            PathBuf::from("Blog/Blog.md")
        );
    }

    #[test]
    fn normalizes_dot_and_dotdot() {
        assert_eq!(normalize("a/./b/../c.md"), PathBuf::from("a/c.md"));
        assert_eq!(normalize("../up.md"), PathBuf::from("../up.md"));
        assert_eq!(normalize("a/b/../../x.md"), PathBuf::from("x.md"));
    }

    #[test]
    fn resolves_against_the_documents_directory() {
        assert_eq!(
            resolve(Path::new("docs/index.md"), "../README.md"),
            PathBuf::from("README.md")
        );
        assert_eq!(
            resolve(Path::new("README.md"), "docs/design.md"),
            PathBuf::from("docs/design.md")
        );
    }

    #[test]
    fn scans_bare_and_labeled_wikilinks_with_spans() {
        let body = "see [[notes/a.md]] and [[colophon:ajp7eq|My file]] here";
        let links = parse_wikilinks(body);
        assert_eq!(links.len(), 2);

        assert_eq!(links[0].target, "notes/a.md");
        assert_eq!(links[0].label, None);
        assert_eq!(&body[links[0].span.clone()], "[[notes/a.md]]");
        assert_eq!(links[0].id_target(), None);

        assert_eq!(links[1].target, "colophon:ajp7eq");
        assert_eq!(links[1].label.as_deref(), Some("My file"));
        assert_eq!(&body[links[1].span.clone()], "[[colophon:ajp7eq|My file]]");
        assert_eq!(links[1].id_target(), Some(crate::identity::Id("ajp7eq".into())));
    }

    #[test]
    fn wikilink_scan_trims_and_skips_degenerate_shapes() {
        // Whitespace inside the brackets is trimmed on both sides of the pipe.
        let trimmed = parse_wikilinks("x [[  notes/a.md  |  Label  ]] y");
        assert_eq!(trimmed[0].target, "notes/a.md");
        assert_eq!(trimmed[0].label.as_deref(), Some("Label"));

        // Empty target and unclosed openers are not links.
        assert!(parse_wikilinks("nothing [[]] here").is_empty());
        assert!(parse_wikilinks("[[ | orphan label ]]").is_empty());
        assert!(parse_wikilinks("dangling [[notes/a.md without close").is_empty());
    }

    #[test]
    fn wikilink_render_round_trips_and_retargets() {
        let link = &parse_wikilinks("[[old.md|Design]]")[0];
        assert_eq!(link.render(), "[[old.md|Design]]");
        // Retarget keeps the label — the rename path relies on this.
        assert_eq!(link.with_target("new.md").render(), "[[new.md|Design]]");

        let bare = &parse_wikilinks("[[old.md]]")[0];
        assert_eq!(bare.render(), "[[old.md]]");
    }

    #[test]
    fn exclude_code_spans_drops_only_overlapping_wikilinks() {
        let body = "see [[notes/a.md]] and `[[not/a/link]]` too";
        let links = parse_wikilinks(body);
        assert_eq!(links.len(), 2, "the lexical scanner has no code awareness");

        let code_start = body.find('`').unwrap();
        let code_end = body.rfind('`').unwrap() + 1;
        let kept = exclude_code_spans(links, &[code_start..code_end]);

        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].target, "notes/a.md");
    }

    #[test]
    fn relative_walks_up_and_down() {
        assert_eq!(relative(Path::new("docs"), Path::new("README.md")), "../README.md");
        assert_eq!(relative(Path::new(""), Path::new("docs/design.md")), "docs/design.md");
        assert_eq!(relative(Path::new("a/b"), Path::new("a/b/c.md")), "c.md");
        assert_eq!(relative(Path::new("a/b"), Path::new("a/b")), ".");
    }

    #[test]
    fn parses_and_round_trips_wikilink_scalars_in_metadata() {
        // A metadata scalar written as a wikilink resolves through the same
        // Link path as a markdown one, and round-trips its wrapper.
        let l = Link::parse("[[id:ajp7eqb|My File]]");
        assert!(l.wikilink);
        assert_eq!(l.label.as_deref(), Some("My File"));
        assert_eq!(l.target, "id:ajp7eqb");
        assert_eq!(l.id_target(), Some(crate::identity::Id("ajp7eqb".into())));
        assert_eq!(l.render(), "[[id:ajp7eqb|My File]]");

        let bare = Link::parse("[[notes/a.md]]");
        assert!(bare.wikilink);
        assert_eq!(bare.label, None);
        assert_eq!(bare.render(), "[[notes/a.md]]");
        // Retarget keeps the wikilink wrapper and label.
        assert_eq!(l.with_target("id:zzzzzz9").render(), "[[id:zzzzzz9|My File]]");
    }

    #[test]
    fn id_scheme_reads_current_and_legacy_spellings() {
        assert_eq!(strip_id_scheme("id:ajp7eqb"), Some("ajp7eqb"));
        assert_eq!(strip_id_scheme("colophon:ajp7eqb"), Some("ajp7eqb"));
        assert_eq!(strip_id_scheme("notes/a.md"), None);
        // New links are authored in the `id:` spelling.
        assert_eq!(id_target(&crate::identity::Id("ajp7eqb".into())), "id:ajp7eqb");
        assert_eq!(Link::parse("colophon:ajp7eqb").id_target().unwrap().0, "ajp7eqb");
    }

    #[test]
    fn format_reference_renders_each_style() {
        let from = Path::new("notes/hw.md");
        let to = Path::new("Archive/a.md");
        let id = crate::identity::Id("ajp7eqb".into());
        let s = |wrapper, addressing, label| ReferenceStyle {
            wrapper,
            addressing,
            label,
            path_style: LinkStyle::MarkdownRoot,
        };

        // Markdown + path → the classic LinkStyle rendering.
        assert_eq!(
            format_reference(s(Wrapper::Markdown, Addressing::Path, false), from, to, None, "A"),
            "[A](/Archive/a.md)"
        );
        // Wikilink + path, label off vs on.
        assert_eq!(
            format_reference(s(Wrapper::Wikilink, Addressing::Path, false), from, to, None, "A"),
            "[[/Archive/a.md]]"
        );
        assert_eq!(
            format_reference(s(Wrapper::Wikilink, Addressing::Path, true), from, to, None, "A"),
            "[[/Archive/a.md|A]]"
        );
        // Markdown + id: bare when unlabeled (the diaryx-shaped id link), a
        // titled markdown link when labeled.
        assert_eq!(
            format_reference(s(Wrapper::Markdown, Addressing::Id, false), from, to, Some(&id), "A"),
            "id:ajp7eqb"
        );
        assert_eq!(
            format_reference(s(Wrapper::Markdown, Addressing::Id, true), from, to, Some(&id), "A"),
            "[A](id:ajp7eqb)"
        );
        // Wikilink + id, no label / with label.
        assert_eq!(
            format_reference(s(Wrapper::Wikilink, Addressing::Id, false), from, to, Some(&id), "A"),
            "[[id:ajp7eqb]]"
        );
        assert_eq!(
            format_reference(s(Wrapper::Wikilink, Addressing::Id, true), from, to, Some(&id), "A"),
            "[[id:ajp7eqb|A]]"
        );
        // Alias is a bare-name wikilink, even if markdown was requested.
        assert_eq!(
            format_reference(s(Wrapper::Markdown, Addressing::Alias, false), from, to, None, "My File"),
            "[[My File]]"
        );
        // Id addressing with no id available degrades to a path link.
        assert_eq!(
            format_reference(s(Wrapper::Wikilink, Addressing::Id, true), from, to, None, "A"),
            "[A](/Archive/a.md)"
        );
    }

    #[test]
    fn reference_style_config_round_trips_and_normalizes() {
        assert_eq!(Wrapper::from_config_str("wikilink"), Some(Wrapper::Wikilink));
        assert_eq!(Addressing::from_config_str("alias"), Some(Addressing::Alias));
        assert_eq!(Wrapper::Wikilink.as_config_str(), "wikilink");
        assert_eq!(Addressing::Id.as_config_str(), "id");
        // markdown + alias is impossible; normalization forces wikilink.
        let n = ReferenceStyle { addressing: Addressing::Alias, ..ReferenceStyle::default() }.normalized();
        assert_eq!(n.wrapper, Wrapper::Wikilink);
        assert!(ReferenceStyle { addressing: Addressing::Id, ..ReferenceStyle::default() }.registers());
        assert!(!ReferenceStyle::default().registers());
    }

    #[test]
    fn path_text_takes_the_path_style_shape() {
        let from = Path::new("a/b/hw.md");
        let to = Path::new("a/c/x.md");
        assert_eq!(path_text(LinkStyle::MarkdownRoot, from, to), "/a/c/x.md");
        assert_eq!(path_text(LinkStyle::MarkdownRelative, from, to), "../c/x.md");
        assert_eq!(path_text(LinkStyle::PlainCanonical, from, to), "a/c/x.md");
    }
}
