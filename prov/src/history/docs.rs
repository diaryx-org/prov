use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::content::{ContentFormat, transcode};
use crate::error::Result;
use crate::identity::Id;
use crate::link;
use crate::meta::{Mapping, Value};

use super::event_id::*;
use super::layout::*;
use super::model::*;
use super::paths::*;
use super::{BLOBS_DIR, EVENTS_DIR, FORGOTTEN_STEM, TRIGGER_MANUAL};

/// How the store's documents are authored: the extension they carry, the grammar
/// their prose is written in, and the carrier their frontmatter rides in.
///
/// One value rather than three parameters, because they are one decision and
/// separating them is how a `.html` store came to hold Markdown bodies.
/// Resolved by [`history_authoring`](crate::Workspace::history_authoring).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Authoring {
    /// The file extension every document in the store gets.
    ///
    /// Owned rather than `&'static str` because the index-rebuild repair reads it
    /// off the file it is repairing (it runs without the root document), and a
    /// hand-made store spelled `.markdown` must keep being scanned as
    /// `.markdown` — canonicalizing it there would rebuild the index from an
    /// empty listing and delete every entry in it.
    pub ext: String,
    /// The body grammar their prose is transcoded into.
    pub content: ContentFormat,
    /// The frontmatter carrier their metadata rides in.
    pub embed: fig::EmbedType,
}

/// Parse an event document's frontmatter into an [`Event`], or `None` when it is
/// not one (no `files` manifest, or no `created`).
pub(super) fn parse_event(path: &Path, id: &str, meta: &Value) -> Option<Event> {
    let created = meta.get("created").and_then(Value::as_str)?.to_string();
    let rows = meta.get("files").and_then(Value::as_sequence)?;
    let mut files = Vec::with_capacity(rows.len());
    for row in rows {
        let (Some(p), Some(hash)) = (
            row.get("path").and_then(Value::as_str),
            row.get("hash").and_then(Value::as_str),
        ) else {
            continue;
        };
        files.push(FileEntry {
            path: link::normalize(p),
            id: row
                .get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(|s| Id(s.trim().to_string())),
            hash: hash.to_string(),
        });
    }
    Some(Event {
        id: id.to_string(),
        path: path.to_path_buf(),
        created,
        trigger: meta
            .get("trigger")
            .and_then(Value::as_str)
            .unwrap_or(TRIGGER_MANUAL)
            .to_string(),
        label: meta
            .get("label")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned),
        parent: meta
            .get("parent")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned),
        files,
    })
}

// ── Rendering the (rebuildable) index documents ──────────────────────────────

/// Render one index document: a title, an optional `part_of` up-link, a
/// `contents` list, and a prose body explaining what the reader is looking at.
///
/// The body is authored as Markdown here and transcoded into the workspace's own
/// grammar, so an HTML workspace gets HTML rather than a `.html` file holding a
/// literal `# History`.
///
/// Links inside the store are authored as **plain relative paths**, deliberately
/// bypassing the workspace's reference style. An id-addressing style would
/// register every event in the registry, which would make each capture rewrite
/// `registry.<ext>` — reintroducing the merge conflict on a *more* load-bearing
/// file than the one the append-only design exists to eliminate.
pub(super) fn render_index(
    title: &str,
    up: Option<(&str, &str)>,
    entries: &[(String, String)],
    prose: &str,
    style: &Authoring,
) -> Result<String> {
    let mut map = Mapping::new();
    map.insert("title".into(), Value::String(title.to_string()));
    if let Some((label, target)) = up {
        map.insert(
            "part_of".into(),
            Value::String(format!("[{label}]({target})")),
        );
    }
    map.insert(
        "contents".into(),
        Value::Sequence(
            entries
                .iter()
                .map(|(label, target)| Value::String(format!("[{label}]({target})")))
                .collect(),
        ),
    );
    let body = transcode(&format!("# {title}\n\n{prose}\n"), style.content)?;
    crate::edit::reformat_block(&body, &map, style.embed)
}

/// How a document in this store opens, in one clause a reader can act on —
/// naming the fence *and* the language inside it.
///
/// The store index is where someone who opened `history/` uninvited starts, and
/// "the manifest is in the frontmatter" is only useful to a reader who already
/// knows which of six carriers this workspace writes. Specialized rather than
/// enumerated, the same move [`crate::about`] makes for the workspace at large:
/// state the one branch that applies here.
fn carrier_opening(embed: fig::EmbedType) -> String {
    use fig::EmbedType as E;
    let name = crate::about::format_name(embed.inner_format());
    match embed {
        E::FrontmatterYaml => format!("between the `---` lines at the top, written in {name}"),
        E::FrontmatterJson => format!("between the `;;;` lines at the top, written in {name}"),
        E::PlusToml => format!("between the `+++` lines at the top, written in {name}"),
        E::MdFrontmatterJson | E::MdFrontmatterToml | E::MdFrontmatterFig => {
            format!("between the `---` lines at the top, written in {name}")
        }
        E::FencedYaml | E::FencedJson | E::FencedToml | E::FrontmatterFig => {
            format!("in the fenced code block at the top, written in {name}")
        }
        E::EndmatterYaml => format!("in the `endmatter` block at the *end*, written in {name}"),
        E::HtmlScriptYaml | E::HtmlScriptJson | E::HtmlScriptToml | E::HtmlScriptFig => {
            format!("inside the `<script>` tag at the top, written in {name}")
        }
        E::HtmlCodeYaml | E::HtmlCodeJson | E::HtmlCodeToml | E::HtmlCodeFig => format!(
            "inside the `<pre><code>` block at the top, written in {name} \
             (with `<` and `&` HTML-encoded)"
        ),
    }
}

/// The prose body of the store index — the "opened `history/` uninvited" case.
/// Legibility is the point of the layout, not a garnish.
///
/// Two things it must do that a static string cannot. It **names the carrier**
/// this workspace actually writes, so a reader knows where the manifest is
/// without having to recognize a fence. And it spells out **recovery by hand**:
/// the blob layout, and the fact that a blob *is* the file. That paragraph is
/// what makes the store readable without prov rather than merely
/// well-documented somewhere else — the reader who most needs it is the one
/// whose workspace is broken, and telling them to run a verb is telling them to
/// trust the thing that just failed them.
/// Wrapped after interpolation, through [`crate::about`]'s helper: the carrier
/// clause varies in length by a factor of three across the archetypes, so a
/// hand-wrapped paragraph would be tidy for YAML and ragged for an HTML island.
pub(super) fn store_prose(style: &Authoring) -> String {
    let paragraphs = [
        crate::about::para(
            "This directory is `prov`'s **history store**: a safety net for damage \
             an external sync transport can do to the workspace's structure.",
        ),
        crate::about::para(&format!(
            "Each capture writes one immutable document under \
             `{EVENTS_DIR}/<year>/<month>/`, recording the complete set of files \
             that existed at that moment. That record is its `files` list, {} — one \
             entry per file, giving the path, its SHA-256 content hash, and its id \
             when it has one.",
            carrier_opening(style.embed),
        )),
        crate::about::para(&format!(
            "The bytes themselves live under `{BLOBS_DIR}/`, named by content hash \
             and shared between captures, so identical content is stored once. A \
             hash of `sha256:abcdef…` is the file `{BLOBS_DIR}/ab/cdef…` — first \
             two hex characters for the directory, the remaining sixty-two for the \
             filename, and never the `sha256:` prefix itself.",
        )),
        crate::about::para(
            "**Recovering a file without prov.** A blob is the file: the exact \
             bytes, uncompressed and unencoded. Find the path in an event's `files` \
             list, take its hash, and copy that blob back over the file. \
             `sha256sum` (or `shasum -a 256`) on a blob prints the name it is \
             stored under, so anything here can be checked against its own \
             filename with no other tool.",
        ),
        crate::about::para(
            "Nothing here is ever rewritten except these index files, which are a \
             cache: the event documents are the authority, and any index can be \
             rebuilt by listing the directory beneath it (`prov check` reports and \
             repairs a stale one).",
        ),
        crate::about::para(
            "Capture a new event with `prov history-capture`; list what is here \
             with `prov history-list`.",
        ),
    ];
    paragraphs.join("\n")
}

/// The month-shard title an event's `part_of` label uses: `July 2026`.
pub(super) fn shard_title(id: &str) -> String {
    match shard_of(id).ok().as_deref().map(shard_parts) {
        Some(Ok((year, month))) => format!("{} {year}", month_name(&month)),
        _ => "History".to_string(),
    }
}

/// The English month name for a two-digit month, or the digits themselves when
/// they are not a month (a hand-made directory prov did not write).
pub(super) fn month_name(month: &str) -> &str {
    match month {
        "01" => "January",
        "02" => "February",
        "03" => "March",
        "04" => "April",
        "05" => "May",
        "06" => "June",
        "07" => "July",
        "08" => "August",
        "09" => "September",
        "10" => "October",
        "11" => "November",
        "12" => "December",
        other => other,
    }
}

/// The workspace-relative paths an index document's `contents` links resolve to.
/// Compared as a link *set* rather than as text, so hand-edited prose or a
/// reordered block is not "stale" — only a genuinely missing or surplus entry is.
pub(super) fn index_entries(index: &Path, meta: &Value) -> Vec<PathBuf> {
    meta.get("contents")
        .map(Value::link_strings)
        .unwrap_or_default()
        .iter()
        .map(|raw| link::resolve(index, &crate::link::Link::parse(raw).target))
        .collect()
}

/// The store index. `forgotten` is the tombstone list's path when the store has
/// one — linked here because it is the only document above it, and an unlinked
/// record of what was destroyed would be reported as an orphan.
pub(super) fn render_store_index(
    years: &BTreeSet<String>,
    forgotten: Option<&Path>,
    style: &Authoring,
) -> Result<String> {
    let ext = style.ext.as_str();
    let mut entries: Vec<(String, String)> = years
        .iter()
        .map(|year| (year.clone(), format!("{EVENTS_DIR}/{year}/index.{ext}")))
        .collect();
    if let Some(path) = forgotten
        && let Some(name) = path.file_name().and_then(|n| n.to_str())
    {
        entries.push(("Forgotten".into(), name.to_string()));
    }
    render_index("History", None, &entries, &store_prose(style), style)
}

/// The hashes a tombstone document records.
pub(super) fn forgotten_hashes(meta: &Value) -> BTreeSet<String> {
    meta.get(FORGOTTEN_STEM)
        .and_then(Value::as_sequence)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get("hash").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The tombstone document, re-rendered whole with `hashes` added.
///
/// Each row records the hash, when it was forgotten, and the subject it was
/// forgotten for. The subject leaks nothing the store does not already hold —
/// every manifest still names that path or id beside that hash, because events
/// are immutable — and without it the list cannot answer why anything on it is
/// there.
pub(super) fn render_forgotten(
    existing: Option<&Value>,
    hashes: &BTreeSet<String>,
    subject: &Subject,
    now: &str,
    format: fig::Format,
) -> Result<String> {
    let mut rows: Vec<Value> = existing
        .and_then(|meta| meta.get(FORGOTTEN_STEM))
        .and_then(Value::as_sequence)
        .map(<[Value]>::to_vec)
        .unwrap_or_default();
    let already: BTreeSet<String> = rows
        .iter()
        .filter_map(|row| row.get("hash").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    let named = match subject {
        Subject::Id(id) => format!("id:{id}"),
        Subject::Path(path) => slash_path(path),
    };
    for hash in hashes {
        // Re-forgetting a hash keeps the *first* record: when it was destroyed is
        // the fact worth preserving, and a re-run finishing an interrupted forget
        // must not rewrite that.
        if already.contains(hash) {
            continue;
        }
        let mut row = Mapping::new();
        row.insert("hash".into(), Value::String(hash.clone()));
        row.insert("at".into(), Value::String(now.to_string()));
        row.insert("subject".into(), Value::String(named.clone()));
        rows.push(Value::Mapping(row));
    }
    let mut map = Mapping::new();
    map.insert("title".into(), Value::String("Forgotten".into()));
    map.insert(FORGOTTEN_STEM.into(), Value::Sequence(rows));
    crate::meta::serialize_mapping(&map, format)
}

/// Whether a manifest row is one the subject names.
pub(super) fn subject_matches(subject: &Subject, file: &FileEntry) -> bool {
    match subject {
        Subject::Id(id) => file.id.as_ref() == Some(id),
        Subject::Path(path) => file.path == *path,
    }
}

pub(super) fn render_year_index(
    year: &str,
    months: &BTreeSet<String>,
    style: &Authoring,
) -> Result<String> {
    let ext = style.ext.as_str();
    let entries: Vec<(String, String)> = months
        .iter()
        .map(|month| {
            (
                format!("{} {year}", month_name(month)),
                format!("{month}/index.{ext}"),
            )
        })
        .collect();
    render_index(
        year,
        Some(("History", &format!("../../index.{ext}"))),
        &entries,
        &format!("Captures taken during {year}, one directory per month."),
        style,
    )
}

pub(super) fn render_month_index(
    year: &str,
    month: &str,
    ids: &BTreeSet<String>,
    style: &Authoring,
) -> Result<String> {
    let ext = style.ext.as_str();
    let entries: Vec<(String, String)> = ids
        .iter()
        .map(|id| (display_entry(id), format!("{id}.{ext}")))
        .collect();
    let title = format!("{} {year}", month_name(month));
    render_index(
        &title,
        Some((year, &format!("../index.{ext}"))),
        &entries,
        &format!(
            "Every capture taken in {title}. Each entry is one immutable event \
             document recording the complete file set at that moment."
        ),
        style,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(content: ContentFormat, embed: fig::EmbedType) -> Authoring {
        Authoring {
            ext: content.extension().to_string(),
            content,
            embed,
        }
    }

    /// The store index is where a reader who opened `history/` uninvited starts,
    /// and "the manifest is in the frontmatter" is useless to someone who does not
    /// already know which of six carriers this workspace writes. So the prose names
    /// the fence *and* the language, resolved rather than enumerated.
    #[test]
    fn the_store_prose_names_the_carrier_the_reader_is_actually_looking_at() {
        // The prose is wrapped *after* the carrier clause is interpolated (that is
        // the point — the clause varies threefold in length), so a phrase can land
        // across a line break. Assert against the collapsed text.
        let flowed = |embed, content| {
            store_prose(&style(content, embed))
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        };

        let yaml = flowed(fig::EmbedType::FrontmatterYaml, ContentFormat::Markdown);
        assert!(
            yaml.contains("between the `---` lines at the top, written in YAML"),
            "{yaml}"
        );

        let island = flowed(fig::EmbedType::HtmlScriptJson, ContentFormat::Html);
        assert!(
            island.contains("inside the `<script>` tag at the top, written in JSON"),
            "{island}"
        );

        let fig_block = flowed(fig::EmbedType::FrontmatterFig, ContentFormat::Markdown);
        assert!(
            fig_block.contains("in the fenced code block at the top, written in fig"),
            "{fig_block}"
        );

        // And every variant teaches recovery by hand: the shard split, and that a
        // blob *is* the file. The reader who most needs this is the one whose
        // workspace is broken, so it cannot be "run a verb".
        for prose in [&yaml, &island, &fig_block] {
            assert!(prose.contains("blobs/ab/cdef"), "{prose}");
            assert!(prose.contains("A blob is the file"), "{prose}");
            assert!(prose.contains("sha256sum"), "{prose}");
        }
    }

    /// A `.html` store must be HTML, not a `.html` file with a Markdown body in
    /// it. prov reads the latter back fine, which is exactly why nothing caught it.
    #[test]
    fn an_html_store_index_is_html_all_the_way_down() {
        let years = BTreeSet::from(["2026".to_string()]);
        let html = render_store_index(
            &years,
            None,
            &style(ContentFormat::Html, fig::EmbedType::HtmlScriptJson),
        )
        .unwrap();

        assert!(
            html.starts_with("<script type=\"application/json\">"),
            "{html}"
        );
        assert!(html.contains("<h1>History</h1>"), "{html}");
        assert!(html.contains("<p>"), "{html}");
        assert!(
            !html.contains("\n# History"),
            "a Markdown heading leaked into an HTML document: {html}"
        );
        // The link into the year shard survives transcoding as a real anchor.
        assert!(html.contains("events/2026/index.html"), "{html}");
    }
}
