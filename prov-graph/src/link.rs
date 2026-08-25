//! Link text — the raw strings a relation field holds, the wikilinks embedded
//! in body prose, and the path arithmetic around them.
//!
//! A link target as written in metadata is either a bare path or a
//! markdown-style labeled link (`[Design](docs/design.md)`), and its path may be
//! **relative** to the document (`notes/a.md`), **workspace-absolute** from the
//! root (`/Blog/Blog.md`), or wrapped in Markdown **angle brackets** when it
//! contains spaces (`</Creative Writing/index.md>`, `[Notes](</My Notes/x.md>)`).
//! This is prov's *link-syntax layer* — the analogue of `fig`'s format
//! layer: it recognizes the conventions a real workspace mixes and round-trips
//! them on write (spaces re-acquire their brackets). A [`Wikilink`] is the
//! body-text counterpart (`[[notes/a.md]]`, `[[colophon:ajp7eq|My file]]`).
//! Everything here is *lexical*: no filesystem
//! access, no symlink resolution, and no markdown-structure awareness (a `[[…]]`
//! inside a code span is still scanned) — resolution and code-fence discipline
//! belong to the traversal and validation layers, which can report what they
//! find.

use std::ops::Range;
use std::path::{Path, PathBuf};

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
    /// brackets (`<…>`, used when it contains spaces) is unwrapped *only* when
    /// it appears as the URL portion of a successfully parsed `[label](<target>)`
    /// — never when it wraps a bare, unlabeled value, which stays byte-literal
    /// (diaryx reads a bare `<…>` as a literal path, angle brackets and all: see
    /// [`parse_path_only`](Link::parse_path_only) and the `link_style` render
    /// tests). A `[[target]]` / `[[target|label]]` Obsidian wikilink scalar is
    /// also recognized here; use [`parse_path_only`](Link::parse_path_only) when
    /// the caller's value is a frontmatter *path* field that must not
    /// reinterpret a literal `[[…]]` string as a wikilink.
    pub fn parse(raw: &str) -> Self {
        Self::parse_impl(raw, true)
    }

    /// [`parse`](Link::parse), but never treats `"[[target]]"` as an Obsidian
    /// wikilink — such a value is left exactly as written (a bare literal, or,
    /// if it happens to also match `[label](target)`, a markdown link). This is
    /// the opt-out a frontmatter *path* field needs: diaryx's own path-value
    /// parser has no wikilink convention at all, so a workspace that stores a
    /// literal `"[[…]]"`-shaped string in a path property (unusual, but legal
    /// input data) must round-trip it untouched rather than have `parse`
    /// silently reinterpret it as a link. Every other rule of `parse` —
    /// `[label](target)` splitting, balanced parens, angle-bracket unwrapping of
    /// a parsed URL — applies unchanged.
    pub fn parse_path_only(raw: &str) -> Self {
        Self::parse_impl(raw, false)
    }

    /// Shared implementation behind [`parse`](Link::parse) and
    /// [`parse_path_only`](Link::parse_path_only); `wikilink` gates the
    /// `[[target]]` recognition branch.
    fn parse_impl(raw: &str, wikilink: bool) -> Self {
        let raw = raw.trim();
        // A wikilink scalar — `[[target]]` / `[[target|label]]` — the Obsidian
        // wrapper permitted in metadata as well as body prose. Skipped entirely
        // by `parse_path_only`.
        if wikilink && let Some(inner) = raw.strip_prefix("[[").and_then(|r| r.strip_suffix("]]")) {
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
        if let Some((label, target)) = split_markdown_link(raw) {
            return Self {
                label: Some(label),
                target,
                wikilink: false,
            };
        }
        // Bare value: stored byte-literal, angle brackets and all. Unlike the
        // markdown-link branch above, there is no URL position here to unwrap —
        // a bare `<…>`-shaped string is data, not delimiters (see `parse`'s doc
        // comment and the C2 regression tests).
        Self {
            label: None,
            target: raw.to_string(),
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

    /// This link with a different display label, keeping the target and wrapper.
    /// The retitle path uses this so `[Old Title](id:abc)` becomes
    /// `[New Title](id:abc)` when the target is renamed — the label follows the
    /// title while the (id or path) target stays exactly as written.
    pub fn with_label(&self, label: impl Into<String>) -> Self {
        Self {
            label: Some(label.into()),
            target: self.target.clone(),
            wikilink: self.wikilink,
        }
    }

    /// `true` when the target points off-workspace (a URL or mail address)
    /// rather than at a file — such links are never resolved against the
    /// filesystem or rewritten by moves.
    pub fn is_external(&self) -> bool {
        self.target.contains("://") || self.target.starts_with("mailto:")
    }

    /// The **sub-document locator** this target carries — the text after a `#`,
    /// naming a place *inside* a document rather than a document.
    ///
    /// A locator is carried, never resolved. prov strips it before resolving the
    /// target and re-attaches it on rewrite, which is the same contract §4 gives
    /// an external URL: recognized by syntax, never validated. What it *means*
    /// is the workspace's business — a verse number in a chapter, a heading
    /// slug, a line range — so a locator naming nothing is not a `check`
    /// finding. That is the price of not teaching prov every document format's
    /// internal address space.
    ///
    /// `None` for an external target (a URL's fragment belongs to the URL) and
    /// for a target that is *only* a locator (`#3`), which stays byte-literal —
    /// see [`split_locator`].
    pub fn locator(&self) -> Option<&str> {
        if self.is_external() {
            return None;
        }
        split_locator(&self.target).1
    }

    /// This link's target with any [`locator`](Self::locator) removed — the part
    /// that names a *document*, and so the only part that resolves.
    pub fn addressed_target(&self) -> &str {
        if self.is_external() {
            return &self.target;
        }
        split_locator(&self.target).0
    }

    /// This link with its document part replaced, **preserving the locator**.
    ///
    /// The rewrite passes (rename, re-relativize, restyle) use this rather than
    /// [`with_target`](Self::with_target), which sets the target verbatim: a
    /// move changes where a document lives, never which part of it was pointed
    /// at.
    pub fn with_path(&self, path: impl Into<String>) -> Self {
        self.with_target(join_locator(path, self.locator()))
    }

    /// What this link's `id:`-scheme target names — local, foreign, or
    /// malformed. `None` when the target carries no id scheme at all (a path,
    /// an alias, a URL).
    ///
    /// Any [`locator`](Self::locator) is stripped first, so `id:abc1234#2-3`
    /// names the same document as `id:abc1234`.
    pub fn id_ref(&self) -> Option<IdRef> {
        strip_id_scheme(self.addressed_target()).map(parse_id_body)
    }

    /// The stable ID this link names, when the target uses the `id:<id>`
    /// scheme (or the legacy `colophon:<id>` spelling) — the
    /// location-independent alternative to a relative path. Such targets
    /// resolve through the workspace's ID registry, never against the
    /// filesystem, and are deliberately *not* rewritten by moves: staying valid
    /// across moves is their entire point.
    ///
    /// **Local ids only.** A cross-workspace `id:<workspace>/<id>` yields
    /// `None`, because the registry this would be resolved against is not the
    /// one that issued the id — see [`id_ref`](Self::id_ref). Callers asking
    /// "must a move leave this alone?" want [`is_path_target`](Self::is_path_target),
    /// which covers every id form.
    pub fn id_target(&self) -> Option<crate::identity::Id> {
        match self.id_ref() {
            Some(IdRef::Local(id)) => Some(id),
            _ => None,
        }
    }

    /// The workspace and id this link names, when it is a cross-workspace
    /// reference (`id:<workspace>/<id>`).
    pub fn foreign_target(&self) -> Option<(String, crate::identity::Id)> {
        match self.id_ref() {
            Some(IdRef::Foreign { workspace, id }) => Some((workspace, id)),
            _ => None,
        }
    }

    /// Whether this link's target is a **path** — the only kind that says where
    /// its target lives, and so the only kind a move may rewrite.
    ///
    /// False for an external URL and for every `id:` reference alike (local,
    /// foreign, and malformed): none of them encodes a location, so
    /// re-relativizing one could only damage it. This is the predicate the
    /// rename, re-relativize and restyle passes filter on.
    pub fn is_path_target(&self) -> bool {
        !self.is_external() && self.id_ref().is_none()
    }
}

/// Try to split `raw` as a Markdown link `[label](target)` (or, when the URL
/// holds a space or paren, `[label](<target>)`). Returns the label and the
/// unwrapped target, or `None` when `raw` doesn't have this shape.
///
/// Ports diaryx_core's `link_parser::try_parse_markdown_link` byte-for-byte
/// (see module doc comment) rather than re-deriving it, because its two
/// corrected behaviors are exactly what C2/C3 need and diaryx's test suite is
/// the ground truth for their edge cases:
/// - The label is whatever sits between the first `[` and the first `]`
///   immediately followed by `(` — *not* whatever precedes the last `)` in the
///   whole string, so trailing prose after the link (`"[Title](/a.md) note"`)
///   never gets swept into the target.
/// - The target's closing paren is found by depth-counting (see
///   [`find_closing_paren`]), so a target containing its own parens
///   (`/file (1).md`, even nested) still closes at the right `)`; any text
///   after that `)` is deliberately never inspected — tolerated, not merely
///   permitted.
/// - Angle brackets are only unwrapped here, on the URL of a link that already
///   parsed as `[label](…)` — never on a bare value (that's C2; see
///   [`Link::parse`]'s doc comment and `parse_impl`'s bare-value branch).
fn split_markdown_link(raw: &str) -> Option<(String, String)> {
    if !raw.starts_with('[') {
        return None;
    }
    let close_bracket = raw.find(']')?;
    if !raw[close_bracket..].starts_with("](") {
        return None;
    }
    let label = raw[1..close_bracket].to_string();
    let after = &raw[close_bracket + 2..];
    let target = if let Some(inner) = after.strip_prefix('<') {
        // `](<target>)`: the closing `>` must be immediately followed by `)` —
        // otherwise this isn't really an angle-bracketed URL and the whole
        // markdown-link parse fails (falls through to the bare branch).
        let close_angle = inner.find('>')?;
        if inner.get(close_angle + 1..close_angle + 2) != Some(")") {
            return None;
        }
        inner[..close_angle].to_string()
    } else {
        let close_paren = find_closing_paren(after)?;
        after[..close_paren].to_string()
    };
    Some((label, target))
}

/// Find the byte offset of the `)` that balances the *implicit* open paren at
/// the start of a Markdown link URL — i.e. the first `)` encountered at
/// nesting depth zero, treating every `(` as opening one more level. A target
/// with no closing paren at all (an unterminated link) yields `None`.
///
/// This is the crux of the C3 fix: the old code demanded the link be the very
/// end of the input (`raw.strip_suffix(')')` on the whole trimmed string), so
/// `"[Title](/a.md) trailing junk"` fell through to a bare target holding the
/// entire string. Scanning for the *matching* close paren instead — and never
/// examining what follows it — makes the split correct both for a target
/// containing its own balanced parens (`/file (1).md`, `/file (a (b)).md`) and
/// for trailing prose after the link.
fn find_closing_paren(s: &str) -> Option<usize> {
    let mut depth = 0u32;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                if depth == 0 {
                    return Some(i);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
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

/// The write style for links a workspace authors — prov's analogue of
/// diaryx's `LinkFormat`, and read from the same place: the `link_format` key in
/// the root document's frontmatter (a fact declared *in* the workspace, not an
/// app-private config). Every link prov writes (autofix today; create/rename
/// in time) uses this, so a repair never introduces a foreign style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LinkStyle {
    /// `[Title](/path/file.md)` — workspace-absolute Markdown link. diaryx's
    /// default, and the most portable/self-documenting.
    #[default]
    MarkdownRoot,
    /// `[Title](../path/file.md)` — relative Markdown link.
    MarkdownRelative,
    /// `/path/file.md` — bare workspace-absolute path.
    PlainRoot,
    /// `../path/file.md` — bare relative path.
    PlainRelative,
}

impl LinkStyle {
    /// This style's notation (bracketed Markdown vs bare path) and path
    /// resolution — the two orthogonal axes the config `references.notation` /
    /// `references.path_style` keys expose (see [`Notation`] / [`PathStyle`]).
    /// `LinkStyle` is the fused internal carrier; these split it back out.
    pub fn axes(self) -> (Notation, PathStyle) {
        match self {
            Self::MarkdownRoot => (Notation::Markdown, PathStyle::Root),
            Self::MarkdownRelative => (Notation::Markdown, PathStyle::Relative),
            Self::PlainRoot => (Notation::Bare, PathStyle::Root),
            Self::PlainRelative => (Notation::Bare, PathStyle::Relative),
        }
    }

    /// The fused [`LinkStyle`] for a bracketed-vs-bare notation and a path
    /// resolution. `Wikilink` has no bare/bracketed distinction, so it maps
    /// through the Markdown family (only the path-text shape matters for it).
    pub fn from_axes(notation: Notation, path_style: PathStyle) -> Self {
        match (notation, path_style) {
            (Notation::Markdown | Notation::Wikilink, PathStyle::Root) => Self::MarkdownRoot,
            (Notation::Markdown | Notation::Wikilink, PathStyle::Relative) => {
                Self::MarkdownRelative
            }
            (Notation::Bare, PathStyle::Root) => Self::PlainRoot,
            (Notation::Bare, PathStyle::Relative) => Self::PlainRelative,
        }
    }
}

/// The syntactic form a reference is written in — the config-facing notation
/// axis (`references.notation`), orthogonal to [`PathStyle`]. This is the clean
/// split of what the internal [`Wrapper`] + `plain_`/`markdown_` [`LinkStyle`]
/// prefix fused: `Bare` is a path with no brackets, `Markdown` is `[Title](…)`,
/// `Wikilink` is `[[…]]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Notation {
    /// `[Title](target)`.
    #[default]
    Markdown,
    /// `[[target]]` / `[[target|Title]]`.
    Wikilink,
    /// A bare `target`, no brackets — what the old `plain_*` link formats wrote.
    Bare,
}

impl Notation {
    /// Parse the `references.notation` config spelling; unknown → `None`.
    pub fn from_config_str(value: &str) -> Option<Self> {
        match value {
            "markdown" => Some(Self::Markdown),
            "wikilink" => Some(Self::Wikilink),
            "bare" => Some(Self::Bare),
            _ => None,
        }
    }

    /// The `references.notation` config spelling.
    pub fn as_config_str(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Wikilink => "wikilink",
            Self::Bare => "bare",
        }
    }

    /// The internal [`Wrapper`] this notation renders through. `Markdown` and
    /// `Bare` share the Markdown wrapper (the bracket-vs-bare choice lives in the
    /// path style); `Wikilink` is its own wrapper.
    pub fn wrapper(self) -> Wrapper {
        match self {
            Self::Wikilink => Wrapper::Wikilink,
            Self::Markdown | Self::Bare => Wrapper::Markdown,
        }
    }

    /// Recover the notation from a fused [`Wrapper`] + [`LinkStyle`] — the inverse
    /// direction, for serializing an internal style back to config.
    pub fn from_wrapper(wrapper: Wrapper, style: LinkStyle) -> Self {
        match wrapper {
            Wrapper::Wikilink => Self::Wikilink,
            Wrapper::Markdown => style.axes().0,
        }
    }
}

/// The path-resolution a reference uses for a path target — the config-facing
/// `references.path_style` axis, orthogonal to [`Notation`]. Applies to path
/// targets only (id/alias ignore it).
///
/// Two resolutions, and deliberately not three. A `canonical` style once emitted
/// a *workspace*-relative path with no leading slash (`path/file.md`), which
/// [`resolve`] reads as *directory*-relative — so a canonical link resolved to
/// what it named only from a document at the workspace root, and silently
/// misresolved everywhere else. The ambiguity of a bare path is settled by
/// committing to one meaning rather than by tagging it: **bare is
/// directory-relative**, and a workspace-relative reference is spelled
/// [`Root`](Self::Root), with the slash that says so. `prov convert <root>
/// link_format markdown_root -r` restyles a workspace that used the old value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PathStyle {
    /// Workspace-absolute: `/path/file.md`.
    #[default]
    Root,
    /// Relative to the referring document: `../file.md`.
    Relative,
}

impl PathStyle {
    /// Parse the `references.path_style` config spelling; unknown → `None`.
    pub fn from_config_str(value: &str) -> Option<Self> {
        match value {
            "root" => Some(Self::Root),
            "relative" => Some(Self::Relative),
            _ => None,
        }
    }

    /// The `references.path_style` config spelling.
    pub fn as_config_str(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Relative => "relative",
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
        LinkStyle::PlainRoot => format!("/{canonical}"),
        LinkStyle::PlainRelative => relative(from.parent().unwrap_or(Path::new("")), target),
    }
}

/// A human title generated from a path's file stem: `_`/`-` become spaces and
/// each word is capitalized (`utility_index.md` → `Utility Index`). The fallback
/// when a target document declares no `title`.
pub fn path_to_title(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
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

/// Turn a human title into a filesystem-friendly filename stem — the readable
/// slug prov derives when a document is created by title (`prov new "My
/// Great Note"` → `my-great-note`). The rough inverse of
/// [`path_to_title`]: lowercase, runs of whitespace and separators
/// (space / `-` / `_` / `/`) collapsed to a single `-`, and any other
/// punctuation dropped. Unicode letters and digits are kept, so a non-ASCII
/// title still yields a legible name. A title with no slug-able characters (pure
/// punctuation) falls back to `"untitled"` so the result is always a valid stem.
///
/// The point is prov's legibility contract (DESIGN §1): a title-first
/// authoring flow that still leaves *readable paths* in the tree and in
/// path-addressed links — unlike an opaque `note-3.md`.
pub fn slug(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut pending_dash = false;
    for ch in title.chars() {
        if ch.is_alphanumeric() {
            if pending_dash {
                out.push('-');
                pending_dash = false;
            }
            out.extend(ch.to_lowercase());
        } else if ch.is_whitespace() || matches!(ch, '-' | '_' | '/') {
            // Defer the separator: only emitted if a kept character follows, so
            // leading, trailing, and repeated separators never reach `out`.
            pending_dash = !out.is_empty();
        }
        // Any other character (punctuation, symbols) is dropped.
    }
    if out.is_empty() {
        "untitled".to_string()
    } else {
        out
    }
}

/// The character that begins a [sub-document locator](Link::locator) on a link
/// target.
pub const LOCATOR_SEPARATOR: char = '#';

/// Split a target into the part naming a document and its locator.
///
/// Splits at the **first** separator, so a locator may itself contain one. Two
/// targets deliberately have no locator:
///
/// - one with no `#` at all — the ordinary case;
/// - one whose `#` is the *first* character (`#3`). That is a same-document
///   reference, and reading it as a locator on an empty path would resolve the
///   link to the containing *directory* — a silent retarget where doing nothing
///   is correct.
///
/// Purely syntactic: callers that must not split an external URL's fragment
/// guard with [`Link::is_external`] first, as [`Link::locator`] does.
///
/// The cost of the convention is a document whose *filename* contains `#`,
/// which can no longer be linked by path. That is the same trade every URL and
/// Markdown implementation makes, and the character is rare in filenames where
/// a locator is not.
pub fn split_locator(target: &str) -> (&str, Option<&str>) {
    match target.split_once(LOCATOR_SEPARATOR) {
        Some((doc, locator)) if !doc.is_empty() => (doc, Some(locator)),
        _ => (target, None),
    }
}

/// Re-attach a locator to a document target — the inverse of [`split_locator`].
/// `None` returns the target unchanged, so this is safe on a target that never
/// had one.
pub fn join_locator(target: impl Into<String>, locator: Option<&str>) -> String {
    let mut target = target.into();
    if let Some(locator) = locator {
        target.push(LOCATOR_SEPARATOR);
        target.push_str(locator);
    }
    target
}

/// The target scheme marking a link-by-ID: `id:<id>`.
pub const ID_SCHEME: &str = "id:";

/// The legacy scheme (`colophon:<id>`), still recognized on read so existing
/// workspaces keep resolving. New links are authored with [`ID_SCHEME`].
pub const LEGACY_ID_SCHEME: &str = "colophon:";

/// Strip the ID scheme from a target, accepting the current `id:` spelling or
/// the legacy `colophon:` one. `None` when the target names no ID.
pub fn strip_id_scheme(target: &str) -> Option<&str> {
    target
        .strip_prefix(ID_SCHEME)
        .or_else(|| target.strip_prefix(LEGACY_ID_SCHEME))
}

/// Render an ID as a link target (`id:<id>`).
pub fn id_target(id: &crate::identity::Id) -> String {
    format!("{ID_SCHEME}{id}")
}

/// The character separating the workspace qualifier from the id in a
/// cross-workspace reference (`id:<workspace>/<id>`).
pub const WORKSPACE_SEPARATOR: char = '/';

/// Render a cross-workspace reference as a link target (`id:<workspace>/<id>`).
pub fn foreign_id_target(workspace: &str, id: &crate::identity::Id) -> String {
    format!("{ID_SCHEME}{workspace}{WORKSPACE_SEPARATOR}{id}")
}

/// Whether `name` is a usable workspace self-name.
///
/// The constraint comes entirely from where the name is *written*: it is the
/// qualifier in an `id:<workspace>/<id>` target, so it may not contain the
/// [`WORKSPACE_SEPARATOR`] that divides it from the id, the `:` that ends the
/// scheme, or whitespace (a target is a single scalar; a space would make it
/// two). Anything else is the user's business — this is a name for humans to
/// *choose*. `prov_identity::mint_workspace_id` (reached by `prov id
/// --workspace`) can mint an opaque one for an owner with no naming authority
/// to lean on, but only when asked: a minted name satisfies this predicate like
/// any other, and nothing here can tell the two apart.
///
/// It lives beside the grammar it is a constraint on rather than in
/// `prov-config` with the [`workspace_id`] key, because it is not a policy
/// choice — every clause above is dictated by how [`IdRef`] parses, and a config
/// layer that spelled its own version of this could drift from the parser that
/// decides what a reference actually means.
///
/// Deliberately *not* checked: uniqueness across workspaces. Nothing here can
/// see another workspace, so a collision is undetectable from inside; it is the
/// resolving host's problem, and the host is the only thing with the evidence to
/// notice ([`crate::peer`]). A minted name buys its uniqueness with width
/// instead — the only currency available to something that cannot check.
///
/// [`workspace_id`]: crate::graph::ReadSettings::workspace_id
pub fn is_valid_workspace_id(name: &str) -> bool {
    !name.is_empty()
        && !name
            .chars()
            .any(|c| c == WORKSPACE_SEPARATOR || c == ':' || c.is_whitespace())
}

/// What an `id:`-scheme target names.
///
/// The scheme carries three distinguishable things, and keeping them apart is
/// the whole point: a reference prov can resolve, one it deliberately cannot,
/// and one that is broken. See `docs/reference-styles.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdRef {
    /// `id:<id>` — a document in *this* workspace, resolved through the registry.
    Local(crate::identity::Id),
    /// `id:<workspace>/<id>` — a document in the workspace named `workspace`.
    ///
    /// prov resolves this only when `workspace` is the reading workspace's own
    /// [`workspace_id`](crate::graph::ReadSettings::workspace_id), in which case it
    /// *is* local and is treated as such. Any other name is somewhere prov
    /// cannot see: the library holds no map from a workspace name to a location
    /// (that is a fact about a device, not about an archive), so the reference is
    /// carried, never rewritten, and never reported broken.
    Foreign {
        /// The workspace qualifier, exactly as written.
        workspace: String,
        /// The id, exactly as written. **Not** check-verified: the foreign
        /// workspace owns its id space and need not be a prov workspace at all
        /// (a diaryx ARK blade is a different length and alphabet), so applying
        /// prov's check character here would reject valid references.
        id: crate::identity::Id,
    },
    /// The `id:` scheme with a body that is no reference at all — an empty half
    /// (`id:`, `id:/x`, `id:ws/`) or more than one separator (`id:a/b/c`).
    ///
    /// Deliberately its own case rather than falling through to a path: the
    /// author wrote `id:`, so silently resolving the text as a filename would
    /// turn a typo into a dangling path and hide what actually went wrong.
    Malformed,
}

/// Parse the body of an `id:`-scheme target (the text after the scheme).
fn parse_id_body(body: &str) -> IdRef {
    let Some((workspace, id)) = body.split_once(WORKSPACE_SEPARATOR) else {
        return if body.is_empty() {
            IdRef::Malformed
        } else {
            IdRef::Local(crate::identity::Id(body.to_string()))
        };
    };
    if workspace.is_empty() || id.is_empty() || id.contains(WORKSPACE_SEPARATOR) {
        return IdRef::Malformed;
    }
    IdRef::Foreign {
        workspace: workspace.to_string(),
        id: crate::identity::Id(id.to_string()),
    }
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
            None => format_path(
                Wrapper::Markdown,
                style.path_style,
                from,
                to,
                title,
                style.label,
            ),
        },
        Addressing::Alias => wrap_alias(title),
        Addressing::Path => format_path(
            style.wrapper,
            style.path_style,
            from,
            to,
            title,
            style.label,
        ),
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
        Wrapper::Wikilink => wrap(
            Wrapper::Wikilink,
            &path_text(path_style, from, to),
            title,
            label,
        ),
    }
}

/// The bare path *text* a path reference points at, in the shape `path_style`
/// selects: workspace-absolute (`/canonical`), relative, or canonical.
pub fn path_text(path_style: LinkStyle, from: &Path, to: &Path) -> String {
    match path_style {
        LinkStyle::MarkdownRoot | LinkStyle::PlainRoot => format!("/{}", to.to_string_lossy()),
        LinkStyle::MarkdownRelative | LinkStyle::PlainRelative => {
            relative(from.parent().unwrap_or(Path::new("")), to)
        }
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
    /// target. Mirrors [`Link::id_target`], including its locator handling:
    /// `[[id:abc1234#2]]` names the document `abc1234`.
    pub fn id_target(&self) -> Option<crate::identity::Id> {
        strip_id_scheme(split_locator(&self.target).0).map(|id| crate::identity::Id(id.to_string()))
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
/// actually should — never [`parse_wikilinks`] directly. When `path`'s
/// extension names a format `twig` understands, every code span (fenced/inline
/// code, raw escapes) is treated as opaque *before* the lexical `[[`…`]]` scan
/// ever sees it: each prose run between code spans is scanned on its own and the
/// results stitched back into `body`-relative spans. For an unrecognized
/// extension — or if the parse fails — this is exactly [`parse_wikilinks`] over
/// the whole body, the same behavior as before code-awareness existed.
///
/// The span is **lexical either way**: twig has no wikilink concept, so it
/// supplies the mask and nothing more. A caller about to *rewrite* what a span
/// covers wants [`parsed_link_spans`] instead, which reports only spans twig
/// itself identified as links.
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

/// One link found in body prose: the parsed [`Link`] (target, label, and whether
/// it was an Obsidian `[[…]]` wikilink or a markdown/djot `[label](target)`
/// link) together with the byte [`span`](BodyLink::span) of the whole construct
/// — exactly what a rewrite replaces. The unifying body-link currency: census,
/// `check`, and the rename machinery all consume this, blind to which syntax the
/// link was written in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyLink {
    /// The parsed link — [`render`](Link::render) reproduces its original
    /// wrapper, so a retargeted `[[a]]` stays a wikilink and a `[t](a)` stays a
    /// markdown link.
    pub link: Link,
    /// Byte range of the whole link construct within the scanned body.
    pub span: Range<usize>,
}

impl BodyLink {
    /// The stable ID this link names, if any — [`Link::id_target`] on the inner
    /// link. ID targets are never rewritten by a move.
    pub fn id_target(&self) -> Option<crate::identity::Id> {
        self.link.id_target()
    }

    /// Whether this link's target is a path — [`Link::is_path_target`] on the
    /// inner link. The predicate the body-rewrite passes filter on.
    pub fn is_path_target(&self) -> bool {
        self.link.is_path_target()
    }
}

/// Scan `body` for **every** link a move or a check must account for — Obsidian
/// `[[…]]` wikilinks *and* markdown/djot `[label](target)` links — each as a
/// [`BodyLink`] in source order. This is the single body-scan seam
/// `census`/`check`/rename use; it supersedes the wikilink-only
/// [`scan_wikilinks`] for callers that must also see markdown/djot links.
///
/// Two syntaxes, two finders, both code-aware:
/// - **Wikilinks** come from the lexical [`scan_wikilinks`] scan (code spans
///   excluded at the source, so a `[[` inside a fence can never eat a later real
///   link).
/// - **Markdown/djot links** come from `twig`'s parser
///   ([`crate::content::link_spans`]): it reports the span of each real `link`
///   node, so a `[x](y)` in a code fence, an autolink, or bracket text that is
///   not a link is never returned. Each span holds exactly one link, so parsing
///   it with [`Link::parse`] cannot over-reach across a stray `)` — the
///   balanced-paren hazard the lexical parser has is structurally absent here.
///   Only inline `[label](target)` links are kept (a successful markdown parse);
///   reference-style and autolink forms are left for a later pass.
///
/// Falls back to wikilinks only when the extension names no `twig` grammar or the
/// parse fails — the same graceful degradation [`scan_wikilinks`] already has.
pub fn scan_body_links(path: &Path, body: &str) -> Vec<BodyLink> {
    let mut out: Vec<BodyLink> = scan_wikilinks(path, body)
        .into_iter()
        .map(|wl| BodyLink {
            link: Link {
                label: wl.label,
                target: wl.target,
                wikilink: true,
            },
            span: wl.span,
        })
        .collect();
    for span in parsed_link_spans(path, body) {
        let link = Link::parse(&body[span.clone()]);
        // Keep only inline `[label](target)` links (a labeled markdown parse):
        // reference/autolink spans parse to a bare or external target and are
        // skipped. Defensively drop a span overlapping a wikilink we already have.
        if link.label.is_none() || link.wikilink {
            continue;
        }
        if out
            .iter()
            .any(|b| b.span.start < span.end && span.start < b.span.end)
        {
            continue;
        }
        out.push(BodyLink { link, span });
    }
    out.sort_by_key(|b| b.span.start);
    out
}

/// The spans of markdown/djot inline links in `body`, via `twig` — empty when
/// `path`'s extension names no grammar `twig` understands or the parse fails
/// (the same degrade-to-lexical rule as `code_spans_for`).
///
/// **The "twig says it is a link" predicate.** These spans come from twig's own
/// `link` nodes, so a span here is a link an actual parser recognized, and each
/// holds exactly one link. That is a materially stronger claim than
/// [`scan_wikilinks`] can make about a `[[…]]`: twig has no wikilink concept, so
/// it only supplies the *code mask* there and the span itself is still lexical.
/// The gap matters wherever a repair is going to *write* — a lexical span may sit
/// in prose that merely looks like a link, and DESIGN §8's whole objection to
/// editing body prose is that `[[float('inf')] * width]` must never be
/// "repaired". So `validate`'s body-link remedies are offered for a span in this
/// set and no other.
pub fn parsed_link_spans(path: &Path, body: &str) -> Vec<Range<usize>> {
    let Some(format) = crate::content::ContentFormat::from_extension(path) else {
        return Vec::new();
    };
    crate::content::link_spans(body, format).unwrap_or_default()
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
            out.extend(shift_spans(
                parse_wikilinks(&body[cursor..span.start]),
                cursor,
            ));
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
        .map(|link| Wikilink {
            span: link.span.start + offset..link.span.end + offset,
            ..link
        })
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

// Lexical path handling lives in `prov-transaction`, which needs the same
// normalization to clamp a staged op to its root. Re-exported here so prov's
// read guards and its write guards demonstrably share one implementation
// rather than two that agree by inspection.
pub use prov_transaction::path::{escapes_root, normalize};

/// Resolve a link target written in `doc` to a normalized path in the same
/// coordinate system as `doc` (workspace-relative when `doc` is). A target with
/// a leading `/` is **workspace-absolute** — resolved from the root, not `doc`'s
/// directory, and never against the filesystem root; any other target is
/// relative to `doc`'s directory.
///
/// Any [sub-document locator](Link::locator) is dropped first: it names a place
/// inside the document, so it has no bearing on which file this is.
pub fn resolve(doc: &Path, target: &str) -> PathBuf {
    let (target, _) = split_locator(target);
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
    use std::path::Component;

    #[test]
    fn locator_splits_at_the_first_separator_and_round_trips() {
        assert_eq!(split_locator("chapter.md"), ("chapter.md", None));
        assert_eq!(split_locator("chapter.md#3"), ("chapter.md", Some("3")));
        assert_eq!(split_locator("id:abc1234#2-3"), ("id:abc1234", Some("2-3")));
        // First separator wins, so a locator may itself contain one.
        assert_eq!(split_locator("a.md#b#c"), ("a.md", Some("b#c")));
        // An empty locator is still a locator — the author wrote the `#`.
        assert_eq!(split_locator("a.md#"), ("a.md", Some("")));
        // A leading `#` is a same-document reference, not a locator on "".
        assert_eq!(split_locator("#3"), ("#3", None));

        for target in ["chapter.md", "chapter.md#3", "a.md#b#c", "a.md#", "#3"] {
            let (doc, locator) = split_locator(target);
            assert_eq!(join_locator(doc, locator), target, "round-trip `{target}`");
        }
    }

    #[test]
    fn a_locator_does_not_change_which_document_is_named() {
        // Path targets: the locator is dropped before resolution.
        assert_eq!(
            resolve(Path::new("bofm/1-ne-1.md"), "1-ne-2.md#5"),
            PathBuf::from("bofm/1-ne-2.md")
        );
        assert_eq!(
            resolve(Path::new("a/b.md"), "/vol/c.md#2-3"),
            PathBuf::from("vol/c.md")
        );

        // Id targets: same id with or without a locator.
        let plain = Link::parse("[1 Nephi 1](id:abc1234)");
        let located = Link::parse("[1 Nephi 1:1](id:abc1234#1)");
        assert_eq!(located.id_target(), plain.id_target());
        assert_eq!(located.id_ref(), plain.id_ref());
        assert_eq!(located.locator(), Some("1"));
        assert_eq!(located.addressed_target(), "id:abc1234");
        assert_eq!(plain.locator(), None);

        // …and a located id target is still not a path, so no move rewrites it.
        assert!(!located.is_path_target());
    }

    #[test]
    fn an_external_url_keeps_its_own_fragment() {
        let url = Link::parse("[talk](https://example.com/a#p3)");
        assert!(url.is_external());
        assert_eq!(url.locator(), None);
        assert_eq!(url.addressed_target(), "https://example.com/a#p3");
        // Untouched by a rewrite, fragment and all.
        assert_eq!(url.with_path("x.md").render(), "[talk](x.md)");
    }

    #[test]
    fn with_path_preserves_the_locator_but_with_target_does_not() {
        let link = Link::parse("[1 Nephi 1:1](../1-ne-1.md#1)");
        // The rewrite passes use `with_path`: a move changes where the document
        // lives, never which part of it was pointed at.
        assert_eq!(
            link.with_path("bofm/1-ne-1.md").render(),
            "[1 Nephi 1:1](bofm/1-ne-1.md#1)"
        );
        // `with_target` still sets the target verbatim.
        assert_eq!(
            link.with_target("bofm/1-ne-1.md").render(),
            "[1 Nephi 1:1](bofm/1-ne-1.md)"
        );
        // A wikilink keeps its wrapper and its locator alike.
        let wl = Link::parse("[[1-ne-1.md#1|1 Nephi 1:1]]");
        assert_eq!(wl.locator(), Some("1"));
        assert_eq!(wl.with_path("x.md").render(), "[[x.md#1|1 Nephi 1:1]]");
    }

    #[test]
    fn slug_makes_readable_stems_and_round_trips_the_common_case() {
        assert_eq!(slug("My Great Note"), "my-great-note");
        // Collapses/strips separators and punctuation; keeps it legible.
        assert_eq!(slug("  Hello,  World!  "), "hello-world");
        assert_eq!(slug("already-a-slug"), "already-a-slug");
        assert_eq!(slug("under_scored/and slashed"), "under-scored-and-slashed");
        assert_eq!(slug("v1.0 Release"), "v10-release");
        // Unicode letters/digits survive.
        assert_eq!(slug("Café Notes"), "café-notes");
        // No leading/trailing/double dashes ever reach the output.
        assert_eq!(slug("--x--y--"), "x-y");
        // A title with nothing slug-able still yields a valid stem.
        assert_eq!(slug("!!!"), "untitled");
        assert_eq!(slug(""), "untitled");
        // The everyday case is the inverse of path_to_title.
        assert_eq!(
            path_to_title(std::path::Path::new("my-great-note.md")),
            "My Great Note"
        );
    }

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
    fn id_refs_split_into_local_foreign_and_malformed() {
        use crate::identity::Id;
        let r = |t: &str| Link::parse(t).id_ref();
        assert_eq!(r("id:ajp7eq"), Some(IdRef::Local(Id("ajp7eq".into()))));
        assert_eq!(
            r("id:notes/ajp7eq"),
            Some(IdRef::Foreign {
                workspace: "notes".into(),
                id: Id("ajp7eq".into()),
            })
        );
        // The legacy scheme carries a qualifier just as well — an old workspace
        // gains cross-workspace references without a rewrite.
        assert_eq!(
            r("colophon:notes/ajp7eq"),
            Some(IdRef::Foreign {
                workspace: "notes".into(),
                id: Id("ajp7eq".into()),
            })
        );
        // Every way the scheme can be present but the reference absent.
        for bad in ["id:", "id:/x", "id:ws/", "id:a/b/c"] {
            assert_eq!(r(bad), Some(IdRef::Malformed), "{bad}");
        }
        // No scheme at all is not an id ref — it is a path or an alias.
        assert_eq!(r("docs/design.md"), None);
        assert_eq!(r("https://example.com/x"), None);
    }

    #[test]
    fn a_foreign_id_is_not_a_local_id() {
        // The distinction that keeps a foreign reference from being looked up in
        // the wrong registry: `id_target` is *local ids only*.
        let foreign = Link::parse("id:notes/ajp7eq");
        assert_eq!(foreign.id_target(), None);
        assert_eq!(
            foreign.foreign_target(),
            Some(("notes".into(), crate::identity::Id("ajp7eq".into())))
        );
        let local = Link::parse("id:ajp7eq");
        assert_eq!(
            local.id_target(),
            Some(crate::identity::Id("ajp7eq".into()))
        );
        assert_eq!(local.foreign_target(), None);
    }

    #[test]
    fn only_paths_are_path_targets() {
        // The predicate every rewrite pass filters on. A move may rewrite the
        // first group and must leave the second alone — including the malformed
        // id, which is a broken reference, not a filename to re-relativize.
        for path in ["docs/design.md", "/a.md", "../b.md", "My Note"] {
            assert!(Link::parse(path).is_path_target(), "{path}");
        }
        for stable in [
            "id:ajp7eq",
            "id:notes/ajp7eq",
            "colophon:notes/ajp7eq",
            "id:a/b/c",
            "https://example.com/x",
            "mailto:a@b.c",
        ] {
            assert!(!Link::parse(stable).is_path_target(), "{stable}");
        }
    }

    #[test]
    fn foreign_targets_round_trip_through_rendering() {
        let target = foreign_id_target("notes", &crate::identity::Id("ajp7eq".into()));
        assert_eq!(target, "id:notes/ajp7eq");
        assert_eq!(
            Link::parse(&target).foreign_target(),
            Some(("notes".into(), crate::identity::Id("ajp7eq".into())))
        );
        // Through a labeled wikilink too, since that is how diaryx would author
        // one — the wrapper is orthogonal to the addressing.
        let wl = Link::parse("[[id:notes/ajp7eq|My Note]]");
        assert_eq!(wl.label.as_deref(), Some("My Note"));
        assert_eq!(
            wl.foreign_target(),
            Some(("notes".into(), crate::identity::Id("ajp7eq".into())))
        );
        assert_eq!(wl.render(), "[[id:notes/ajp7eq|My Note]]");
    }

    #[test]
    fn parses_angle_bracketed_and_absolute_targets() {
        // Diaryx-style: a labeled link to an angle-bracketed, workspace-absolute
        // path containing a space.
        let l = Link::parse("[Archived Documents](</Archive/Archived documents.md>)");
        assert_eq!(l.label.as_deref(), Some("Archived Documents"));
        assert_eq!(l.target, "/Archive/Archived documents.md");
        // Round-trips: the space forces the angle brackets back on render.
        assert_eq!(
            l.render(),
            "[Archived Documents](</Archive/Archived documents.md>)"
        );

        // A bare angle-bracketed target — no `[label](…)` around it — is
        // *not* unwrapped: angle brackets are only URL delimiters inside a
        // parsed markdown link, so a bare `<…>` value stays byte-literal (C2;
        // diaryx reads a bare `<…>` as a literal path, brackets and all). This
        // used to unwrap unconditionally; see `bare_angle_bracket_value_stays_literal`
        // for the dedicated regression coverage.
        let bare = Link::parse("</Creative Writing/Creative Writing.md>");
        assert_eq!(bare.target, "</Creative Writing/Creative Writing.md>");
        assert_eq!(bare.render(), "</Creative Writing/Creative Writing.md>");

        // An absolute path without spaces needs no brackets, and stays bare.
        let plain = Link::parse("[Blog](/Blog/Blog.md)");
        assert_eq!(plain.target, "/Blog/Blog.md");
        assert_eq!(plain.render(), "[Blog](/Blog/Blog.md)");
    }

    /// C2 regression: before the fix, the bare-value fallback ran `unbracket`
    /// on the *whole* raw value, so any `<...>`-shaped bare string (not just
    /// the diaryx example above) was silently unwrapped. Now the bare branch
    /// never touches angle brackets — only a successfully parsed
    /// `[label](<target>)` URL gets unwrapped.
    #[test]
    fn bare_angle_bracket_value_stays_literal() {
        for raw in ["<notes/a.md>", "<https://example.com>", "<a (b) c>", "<>"] {
            let l = Link::parse(raw);
            assert_eq!(l.label, None);
            assert_eq!(l.target, raw, "bare angle-bracket value must be literal");
            assert_eq!(l.render(), raw);
        }
        // Contrast: the *same* angle-bracketed text, once it's the URL of an
        // actual markdown link, is unwrapped — that part of the old behavior
        // was correct and stays.
        assert_eq!(Link::parse("[x](<notes/a.md>)").target, "notes/a.md");
    }

    /// C3 regression: before the fix, the markdown-link branch demanded the
    /// *entire* trimmed input end in `)` (`raw.strip_suffix(')')`), so any
    /// trailing text after a well-formed link's closing paren made the whole
    /// value fall through to the bare branch — the complete string, including
    /// the `[label](target)` syntax, became one literal target. Balanced-paren
    /// scanning fixes both halves of that: trailing text is tolerated, and
    /// parens *inside* the target (nested, even) don't confuse the scan.
    #[test]
    fn markdown_link_split_tolerates_trailing_text_and_balanced_parens() {
        // Trailing prose after a legitimate link is ignored, not swallowed.
        let l = Link::parse("[Title](/path.md) trailing junk");
        assert_eq!(l.label.as_deref(), Some("Title"));
        assert_eq!(l.target, "/path.md");

        // A target containing its own parens still closes at the matching `)`.
        let l = Link::parse("[Explanation (1.1)](/Archive/Explanation (1.1).md)");
        assert_eq!(l.label.as_deref(), Some("Explanation (1.1)"));
        assert_eq!(l.target, "/Archive/Explanation (1.1).md");

        // Nested parens in the target keep working.
        let l = Link::parse("[File (a (b))](/path/file (a (b)).md)");
        assert_eq!(l.label.as_deref(), Some("File (a (b))"));
        assert_eq!(l.target, "/path/file (a (b)).md");

        // Trailing text *and* parens in the target, together.
        let l = Link::parse("[T](/a (1).md) and then some more words");
        assert_eq!(l.target, "/a (1).md");

        // An angle-bracketed URL still requires the `>` immediately followed by
        // `)` — trailing text after *that* `)` is likewise tolerated.
        let l = Link::parse("[Notes](</My Notes/x.md>) ignored tail");
        assert_eq!(l.target, "/My Notes/x.md");

        // An unterminated target (no closing paren at all) still falls back to
        // bare, unchanged from before.
        let unterminated = "[Title](/path.md";
        assert_eq!(Link::parse(unterminated).render(), unterminated);
    }

    /// C1: `parse_path_only` opts out of the `[[…]]` wikilink convention so a
    /// frontmatter path field can hold a literal bracket-shaped string without
    /// `Link::parse` reinterpreting it — the convention diaryx's own path-value
    /// parser never had. `parse` is unchanged (still treats it as a wikilink).
    #[test]
    fn wikilink_opt_out_keeps_bracket_literal_string() {
        let ordinary = Link::parse("[[notes/a.md]]");
        assert!(ordinary.wikilink);
        assert_eq!(ordinary.target, "notes/a.md");

        let opted_out = Link::parse_path_only("[[notes/a.md]]");
        assert!(!opted_out.wikilink);
        assert_eq!(opted_out.label, None);
        assert_eq!(opted_out.target, "[[notes/a.md]]");
        assert_eq!(opted_out.render(), "[[notes/a.md]]");

        // A pipe-labeled wikilink scalar is likewise kept as one literal bare
        // string, not split into label/target.
        let piped = Link::parse_path_only("[[notes/a.md|My Note]]");
        assert_eq!(piped.label, None);
        assert_eq!(piped.target, "[[notes/a.md|My Note]]");

        // Every other `parse` rule is unaffected: markdown links, bare paths,
        // and angle-bracket handling (both C2's literal-bare and C3's
        // balanced-paren splitting) all behave identically under the opt-out.
        assert_eq!(
            Link::parse_path_only("[Design](docs/design.md)"),
            Link::parse("[Design](docs/design.md)")
        );
        assert_eq!(
            Link::parse_path_only("notes/a.md"),
            Link::parse("notes/a.md")
        );
        assert_eq!(
            Link::parse_path_only("<notes/a.md>"),
            Link::parse("<notes/a.md>")
        );
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
            format_link(LinkStyle::PlainRoot, from, target, "ignored"),
            "/School/Archive/MATH 213 files.md"
        );
    }

    #[test]
    fn link_style_axes_round_trip_and_cover_every_combination() {
        use Notation::*;
        use PathStyle::*;
        // Every notation×path_style combination has a fused LinkStyle, and axes()
        // is its inverse — so the orthogonal config surface is lossless.
        for notation in [Markdown, Bare] {
            for path_style in [Root, Relative] {
                let style = LinkStyle::from_axes(notation, path_style);
                assert_eq!(style.axes(), (notation, path_style));
            }
        }
        // Wikilink has no bare/bracketed split, so it maps through the Markdown
        // family and its path text follows the path style.
        assert_eq!(
            LinkStyle::from_axes(Wikilink, Relative),
            LinkStyle::MarkdownRelative
        );
        assert_eq!(
            Notation::from_wrapper(Wrapper::Wikilink, LinkStyle::MarkdownRoot),
            Wikilink
        );
        assert_eq!(Notation::from_config_str("bare"), Some(Bare));
        assert_eq!(PathStyle::from_config_str("relative"), Some(Relative));
        // Retired: a bare workspace-relative path is unspellable, because a bare
        // path is directory-relative and the two cannot both be true.
        assert_eq!(PathStyle::from_config_str("canonical"), None);
        assert_eq!(LinkStyle::default(), LinkStyle::MarkdownRoot);
        assert_eq!(
            path_to_title(Path::new("Folder/utility_index.md")),
            "Utility Index"
        );
    }

    #[test]
    fn the_bare_workspace_absolute_style_renders_a_leading_slash() {
        let from = Path::new("a/b.md");
        let to = Path::new("c/d.md");
        assert_eq!(format_link(LinkStyle::PlainRoot, from, to, "D"), "/c/d.md");
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
        assert_eq!(
            links[1].id_target(),
            Some(crate::identity::Id("ajp7eq".into()))
        );
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
        let code_span = code_start..code_end;
        let kept = exclude_code_spans(links, std::slice::from_ref(&code_span));

        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].target, "notes/a.md");
    }

    #[test]
    fn relative_walks_up_and_down() {
        assert_eq!(
            relative(Path::new("docs"), Path::new("README.md")),
            "../README.md"
        );
        assert_eq!(
            relative(Path::new(""), Path::new("docs/design.md")),
            "docs/design.md"
        );
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
        assert_eq!(
            l.with_target("id:zzzzzz9").render(),
            "[[id:zzzzzz9|My File]]"
        );
    }

    #[test]
    fn id_scheme_reads_current_and_legacy_spellings() {
        assert_eq!(strip_id_scheme("id:ajp7eqb"), Some("ajp7eqb"));
        assert_eq!(strip_id_scheme("colophon:ajp7eqb"), Some("ajp7eqb"));
        assert_eq!(strip_id_scheme("notes/a.md"), None);
        // New links are authored in the `id:` spelling.
        assert_eq!(
            id_target(&crate::identity::Id("ajp7eqb".into())),
            "id:ajp7eqb"
        );
        assert_eq!(
            Link::parse("colophon:ajp7eqb").id_target().unwrap().0,
            "ajp7eqb"
        );
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
            format_reference(
                s(Wrapper::Markdown, Addressing::Path, false),
                from,
                to,
                None,
                "A"
            ),
            "[A](/Archive/a.md)"
        );
        // Wikilink + path, label off vs on.
        assert_eq!(
            format_reference(
                s(Wrapper::Wikilink, Addressing::Path, false),
                from,
                to,
                None,
                "A"
            ),
            "[[/Archive/a.md]]"
        );
        assert_eq!(
            format_reference(
                s(Wrapper::Wikilink, Addressing::Path, true),
                from,
                to,
                None,
                "A"
            ),
            "[[/Archive/a.md|A]]"
        );
        // Markdown + id: bare when unlabeled (the diaryx-shaped id link), a
        // titled markdown link when labeled.
        assert_eq!(
            format_reference(
                s(Wrapper::Markdown, Addressing::Id, false),
                from,
                to,
                Some(&id),
                "A"
            ),
            "id:ajp7eqb"
        );
        assert_eq!(
            format_reference(
                s(Wrapper::Markdown, Addressing::Id, true),
                from,
                to,
                Some(&id),
                "A"
            ),
            "[A](id:ajp7eqb)"
        );
        // Wikilink + id, no label / with label.
        assert_eq!(
            format_reference(
                s(Wrapper::Wikilink, Addressing::Id, false),
                from,
                to,
                Some(&id),
                "A"
            ),
            "[[id:ajp7eqb]]"
        );
        assert_eq!(
            format_reference(
                s(Wrapper::Wikilink, Addressing::Id, true),
                from,
                to,
                Some(&id),
                "A"
            ),
            "[[id:ajp7eqb|A]]"
        );
        // Alias is a bare-name wikilink, even if markdown was requested.
        assert_eq!(
            format_reference(
                s(Wrapper::Markdown, Addressing::Alias, false),
                from,
                to,
                None,
                "My File"
            ),
            "[[My File]]"
        );
        // Id addressing with no id available degrades to a path link.
        assert_eq!(
            format_reference(
                s(Wrapper::Wikilink, Addressing::Id, true),
                from,
                to,
                None,
                "A"
            ),
            "[A](/Archive/a.md)"
        );
    }

    #[test]
    fn reference_style_config_round_trips_and_normalizes() {
        assert_eq!(
            Wrapper::from_config_str("wikilink"),
            Some(Wrapper::Wikilink)
        );
        assert_eq!(
            Addressing::from_config_str("alias"),
            Some(Addressing::Alias)
        );
        assert_eq!(Wrapper::Wikilink.as_config_str(), "wikilink");
        assert_eq!(Addressing::Id.as_config_str(), "id");
        // markdown + alias is impossible; normalization forces wikilink.
        let n = ReferenceStyle {
            addressing: Addressing::Alias,
            ..ReferenceStyle::default()
        }
        .normalized();
        assert_eq!(n.wrapper, Wrapper::Wikilink);
        assert!(
            ReferenceStyle {
                addressing: Addressing::Id,
                ..ReferenceStyle::default()
            }
            .registers()
        );
        assert!(!ReferenceStyle::default().registers());
    }

    #[test]
    fn path_text_takes_the_path_style_shape() {
        let from = Path::new("a/b/hw.md");
        let to = Path::new("a/c/x.md");
        assert_eq!(path_text(LinkStyle::MarkdownRoot, from, to), "/a/c/x.md");
        assert_eq!(
            path_text(LinkStyle::MarkdownRelative, from, to),
            "../c/x.md"
        );
        assert_eq!(path_text(LinkStyle::PlainRoot, from, to), "/a/c/x.md");
    }

    /// Laws, rather than examples.
    ///
    /// Every test above names one input and asserts the output prov produced
    /// for it. That is the right shape for a decision (`slug("v1.0 Release")`
    /// is `"v10-release"` because someone chose it), and the wrong shape for a
    /// *law* — a claim quantified over every input, of which an example
    /// witnesses exactly one. The round-trip below is a law: a link prov
    /// authors must name the document prov meant, from wherever it was
    /// written, in whichever style the workspace declared.
    mod properties {
        use super::*;
        use proptest::prelude::*;

        /// A workspace-relative document path — up to two directories deep,
        /// short lowercase names. The alphabet is small on purpose: a
        /// counterexample is only useful if you can read it, and `a/b.md`
        /// makes the same point as `Xk9/qZ2.md` with none of the noise.
        fn doc_path() -> impl Strategy<Value = PathBuf> {
            (prop::collection::vec("[a-z]{1,3}", 0..3usize), "[a-z]{1,3}").prop_map(
                |(dirs, stem)| {
                    let mut path = PathBuf::new();
                    for dir in dirs {
                        path.push(dir);
                    }
                    path.push(format!("{stem}.md"));
                    path
                },
            )
        }

        /// A path as a *human* might write one into frontmatter: real names
        /// mixed with the `.` and `..` a relative reference is made of. Unlike
        /// [`doc_path`] this may climb above the root (`../../x`), which is
        /// legal to *write* and refused later by [`escapes_root`] — the
        /// normalizer has to survive it either way.
        fn messy_path() -> impl Strategy<Value = PathBuf> {
            prop::collection::vec(
                prop_oneof![Just(".".to_string()), Just("..".to_string()), "[a-z]{1,3}"],
                1..5usize,
            )
            .prop_map(|parts| parts.iter().collect())
        }

        /// Every style there is.
        ///
        /// This list was once four of six. The two `Canonical` styles emitted a
        /// bare *workspace*-relative path that [`resolve`] reads as
        /// *directory*-relative, so they failed the law below — and they are
        /// gone now rather than excused, which is why the strategy can name the
        /// whole enum again. A style that cannot be read back is not a style.
        fn round_tripping_style() -> impl Strategy<Value = LinkStyle> {
            prop::sample::select(vec![
                LinkStyle::MarkdownRoot,
                LinkStyle::MarkdownRelative,
                LinkStyle::PlainRoot,
                LinkStyle::PlainRelative,
            ])
        }

        proptest! {
            /// `resolve ∘ format = id`: authoring a link to `to` from the
            /// document at `from`, then reading it back the way every consumer
            /// does (`Link::parse` for the scalar, `resolve` for the path),
            /// must land on `to` again. The whole point of a style axis is
            /// that it changes how a reference is *spelled* and not what it
            /// *means* — so this holds for every style, which is a claim the
            /// enum only earned once the canonical pair was retired.
            #[test]
            fn a_formatted_link_resolves_back_to_the_document_it_names(
                from in doc_path(),
                to in doc_path(),
                style in round_tripping_style(),
            ) {
                let written = format_link(style, &from, &to, "Label");
                let got = resolve(&from, &Link::parse(&written).target);
                prop_assert_eq!(
                    &got,
                    &to,
                    "{:?} wrote `{}` in `{}`",
                    style,
                    written,
                    from.display()
                );
            }

            /// The law a move rests on: **re-relativizing a link preserves its
            /// referent.** `mutate::rename` rewrites every outbound path link
            /// of a document it moves by resolving the old text against the old
            /// location and re-rendering it against the new one
            /// (`rename.rs:236`). That is only safe if the two resolve to the
            /// same document — which is this, quantified over every pair of
            /// locations a document could move between.
            #[test]
            fn re_relativizing_a_link_preserves_the_document_it_names(
                from in doc_path(),
                to in doc_path(),
                referent in doc_path(),
            ) {
                let dir = |p: &Path| p.parent().unwrap_or(Path::new("")).to_path_buf();
                // As authored in the document at its old location.
                let written = relative(&dir(&from), &referent);
                // What `rename` writes when the document lands at `to`.
                let rewritten = relative(&dir(&to), &resolve(&from, &written));
                prop_assert_eq!(
                    resolve(&to, &rewritten),
                    referent,
                    "`{}` moving {} → {} became `{}`",
                    written,
                    from.display(),
                    to.display(),
                    rewritten
                );
            }

            /// [`normalize`] is idempotent — folding `.`/`..` a second time
            /// changes nothing. Load-bearing because resolved paths are
            /// compared for equality all over the crate (`move_conflict`, the
            /// registry's id↔path bijection, every `check` finding that says
            /// two links name the same document); a normal form that still
            /// moves under re-application would make those comparisons depend
            /// on how many times a path had been through the resolver.
            #[test]
            fn normalizing_a_path_twice_is_normalizing_it_once(path in messy_path()) {
                let once = normalize(&path);
                prop_assert_eq!(normalize(&once), once.clone(), "on `{}`", path.display());
            }

            /// A normalized path has no `.` left, and any `..` that survives is
            /// a *leading* one — the climb above the root that could not be
            /// cancelled, which is precisely what [`escapes_root`] then looks
            /// for. A `..` appearing after a normal component would mean the
            /// fold missed a cancellation, and every path comparison downstream
            /// would be reading a path that still had reducing left to do.
            #[test]
            fn a_normalized_path_keeps_only_the_climb_it_could_not_cancel(
                path in messy_path(),
            ) {
                let normalized = normalize(&path);
                let parts: Vec<_> = normalized.components().collect();
                let climb = parts
                    .iter()
                    .take_while(|c| matches!(c, Component::ParentDir))
                    .count();
                prop_assert!(
                    parts[climb..]
                        .iter()
                        .all(|c| matches!(c, Component::Normal(_) | Component::RootDir)),
                    "`{}` normalized to `{}`",
                    path.display(),
                    normalized.display()
                );
            }
        }
    }
}
