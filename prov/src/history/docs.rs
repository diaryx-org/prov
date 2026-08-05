use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::identity::Id;
use crate::link;
use crate::meta::{Mapping, Value};

use super::event_id::*;
use super::layout::*;
use super::model::*;
use super::paths::*;
use super::{EVENTS_DIR, FORGOTTEN_STEM, TRIGGER_MANUAL};

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
    embed: fig::EmbedType,
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
    crate::edit::reformat_block(&format!("# {title}\n\n{prose}\n"), &map, embed)
}

/// The prose body of the store index — the "opened `history/` uninvited" case.
/// Legibility is the point of the layout, not a garnish.
pub(super) const STORE_PROSE: &str = "\
This directory is `prov`'s **history store**: a safety net for damage an
external sync transport can do to the workspace's structure.

Each capture writes one immutable document under `events/<year>/<month>/`,
recording the complete set of files that existed at that moment — every path
with its content hash, and its id when it has one. The bytes themselves live
under `blobs/`, named by content hash and shared between captures, so identical
content is stored once.

Nothing here is ever rewritten except these index files, which are a cache: the
event documents are the authority, and any index can be rebuilt by listing the
directory beneath it (`prov check` reports and repairs a stale one).

Capture a new event with `prov history-capture`; list what is here with
`prov history-list`.";

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
    ext: &str,
    forgotten: Option<&Path>,
    embed: fig::EmbedType,
) -> Result<String> {
    let mut entries: Vec<(String, String)> = years
        .iter()
        .map(|year| (year.clone(), format!("{EVENTS_DIR}/{year}/index.{ext}")))
        .collect();
    if let Some(path) = forgotten
        && let Some(name) = path.file_name().and_then(|n| n.to_str())
    {
        entries.push(("Forgotten".into(), name.to_string()));
    }
    render_index("History", None, &entries, STORE_PROSE, embed)
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
    ext: &str,
    embed: fig::EmbedType,
) -> Result<String> {
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
        embed,
    )
}

pub(super) fn render_month_index(
    year: &str,
    month: &str,
    ids: &BTreeSet<String>,
    ext: &str,
    embed: fig::EmbedType,
) -> Result<String> {
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
        embed,
    )
}
