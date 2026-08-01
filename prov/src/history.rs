//! History — a versioned safety net for the workspace.
//!
//! prov workspaces are plaintext, so the obvious way to sync one across devices
//! is to point an existing transport (git, Dropbox, iCloud, Syncthing) at the
//! directory and let it reconcile files. That is free for ordinary content
//! edits. It is *not* free for **structural** mutations: a rename, move or
//! delete touches several files at once (the node, every inbound link, the
//! parent's child list, the id registry), and a transport reconciling the bytes
//! with no idea about prov's graph can produce a clean-looking merge that is
//! semantically broken.
//!
//! Nothing prov already has covers that. The crash journal ([`crate::journal`])
//! protects a single device against its own interrupted writes; the recycle bin
//! protects an explicit, single-device delete; `backup` protects against losing
//! the workspace's location entirely — but a backup is a whole opaque tree, and
//! cannot answer "which files did yesterday's merge break, and what did each
//! look like before."
//!
//! ## The shape
//!
//! A reachable `history/` directory off the root holds one **immutable event
//! document per capture** — a full manifest of every reachable file
//! (`path → (id?, hash)`) — plus a content-addressed **blob store** holding the
//! bytes, deduplicated by SHA-256. [`history_capture`] hashes the live graph
//! (minus `history/` itself and `recyclebin/items/`), parks any unseen bytes,
//! and writes one new file.
//!
//! Two properties do all the work:
//!
//! - **Every event is a full manifest, not a delta.** An event is
//!   self-contained: nothing folds through its ancestry, so `parent` is display
//!   metadata and a foreign event restores even if the events before it never
//!   arrived. Removals need no bookkeeping — a path absent from the manifest was
//!   not in the capture set.
//! - **The store is append-only at the filesystem level.** A capture only *adds*
//!   files (a new event document, newly-seen blobs), and added-file/added-file is
//!   the one merge case git, Dropbox, Syncthing and iCloud all handle without
//!   conflict. The only mutable files are the per-shard index documents, and
//!   those are a **rebuildable cache**: authority lives in the event documents,
//!   and any index is recoverable by scanning the directory beneath it. A
//!   conflicted index is a [`Finding::HistoryIndexStale`] with a mechanical
//!   autofix, not data loss.
//!
//! The format is pinned in `docs/history-format.md` — event documents are
//! immutable, so it is a compatibility contract that cannot be retrofitted.
//!
//! ## Audience honesty
//!
//! When the transport is **git**, history should stay off: git already stores
//! every pre-image, dedupes by content, and reconciles concurrent histories. The
//! feature earns its keep where the transport keeps no history — Dropbox,
//! Syncthing, iCloud, a synced network share. That audience is real *and
//! narrow*, which is why [`History`](crate::config::History) defaults off.
//!
//! [`history_capture`]: crate::Workspace::history_capture

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::change::ChangeSet;
use crate::document::MetaCarrier;
use crate::error::{Error, Result};
use crate::fs::Storage;
use crate::identity::Id;
use crate::index::IndexStore;
use crate::link;
use crate::meta::{Mapping, Value};
use crate::validate::Finding;
use crate::workspace::Workspace;

/// The directory the first capture bootstraps the store into, relative to the
/// workspace root. Only a *default*: the store's real location is whatever the
/// root's `history` pointer names, and every path below it is derived from that.
pub const HISTORY_DIR: &str = "history";

/// The subdirectory of the store holding date-sharded event documents.
pub const EVENTS_DIR: &str = "events";

/// The subdirectory of the store holding content-addressed pre-image bytes.
/// Deliberately **unreached** — nothing links into it, so §8's orphan check
/// ignores it exactly as it already ignores `recyclebin/items/`.
pub const BLOBS_DIR: &str = "blobs";

/// The `trigger` recorded by a capture the user asked for. The only Phase 0
/// value: prov does not run the sync, so there is no event for it to hook.
pub const TRIGGER_MANUAL: &str = "manual";

/// One row of an event's manifest: a captured file, its content hash, and — when
/// the document is registered — its id.
///
/// The `id` column is what makes per-document lineage a *derived query* rather
/// than a storage design: a path-keyed view shows a move as two unrelated
/// lineages, where the id shows one document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// The captured file, workspace-relative and normalized.
    pub path: PathBuf,
    /// The document's registered id, or `None` when it carries none.
    pub id: Option<Id>,
    /// The content digest, spelled `sha256:<hex>` as [`crate::fixity::digest`]
    /// produces it.
    pub hash: String,
}

/// One capture: a full manifest of the capture set at a moment, plus the display
/// metadata that lets [`history_list`](Workspace::history_list) narrate it.
///
/// Immutable once written. Everything a restore needs is here plus the blobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// The event id — also the document's file stem, and a pure function of its
    /// path (see [`event_path`]).
    pub id: String,
    /// Where the event document lives, workspace-relative.
    pub path: PathBuf,
    /// RFC 3339 UTC timestamp of the capture.
    pub created: String,
    /// How the capture was invoked ([`TRIGGER_MANUAL`]).
    pub trigger: String,
    /// The `--label` text, verbatim.
    pub label: Option<String>,
    /// The newest event that existed locally at capture time.
    ///
    /// **Display metadata only.** Nothing computes through it, so clock skew, a
    /// missing parent and interleaved arrivals are cosmetic rather than
    /// correctness hazards — which is exactly why no device identity is needed to
    /// mint, store, or lose.
    pub parent: Option<String>,
    /// The complete capture set at that moment, sorted by path.
    pub files: Vec<FileEntry>,
}

impl Event {
    /// This event's manifest as a path → (id, hash) map, for diffing against
    /// another event's.
    fn manifest(&self) -> BTreeMap<&Path, (&Option<Id>, &str)> {
        self.files
            .iter()
            .map(|f| (f.path.as_path(), (&f.id, f.hash.as_str())))
            .collect()
    }

    /// How this event's capture set differs from `previous`: `(changed, removed)`
    /// — files whose hash differs or that are newly present, and files `previous`
    /// held that this one does not.
    pub fn diff(&self, previous: &Event) -> (usize, usize) {
        let (mine, theirs) = (self.manifest(), previous.manifest());
        let changed = mine
            .iter()
            .filter(|(path, (_, hash))| theirs.get(*path).is_none_or(|(_, old)| old != hash))
            .count();
        let removed = theirs.keys().filter(|p| !mine.contains_key(*p)).count();
        (changed, removed)
    }

    /// A human-facing one-liner for the event: its date, time and label.
    pub fn describe(&self) -> String {
        match &self.label {
            Some(label) => format!("{} ({label})", display_stamp(&self.id)),
            None => display_stamp(&self.id),
        }
    }
}

/// What a capture did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Captured {
    /// A new event was written.
    Written {
        /// The new event's id.
        id: String,
        /// How many files the manifest records.
        files: usize,
        /// How many blobs this capture newly parked (the rest were already
        /// present, deduplicated by content).
        blobs: usize,
        /// Files changed and removed relative to the previous event, when there
        /// was one to compare against.
        diff: Option<(usize, usize)>,
    },
    /// The computed manifest was identical to the newest existing event's, so
    /// nothing was written — otherwise a git hook or a habitual user fills the
    /// log with duplicates.
    Unchanged {
        /// The event that already describes this exact state.
        id: String,
    },
}

/// What one event's manifest said about one document — the unit a lineage
/// reports a change in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presence {
    /// Captured: the manifest row, whole.
    At {
        /// Where the document lived at that capture, workspace-relative.
        path: PathBuf,
        /// The id the manifest recorded for it, or `None` when it carried none.
        ///
        /// Carried even for a lineage that was *found* by path, because it is
        /// what tells a path-keyed query that a stronger one exists.
        id: Option<Id>,
        /// Its content digest, spelled `sha256:<hex>`.
        hash: String,
    },
    /// Absent from that capture set — deleted, or moved out of the reachable
    /// graph, between the previous event and this one. There is no removal list
    /// to consult: in a full manifest, **omission is deletion**.
    Gone,
}

/// One point in a document's lineage: an event whose manifest recorded a state
/// different from the event before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    /// The event that recorded this state.
    pub event: String,
    /// That event's `created` timestamp.
    pub created: String,
    /// That event's label, verbatim.
    pub label: Option<String>,
    /// What that event's manifest said about the document.
    pub state: Presence,
}

/// What a lineage query follows a document *by*.
///
/// The two are not equals. An id survives a rename, so following one yields the
/// lineage of a document; a path is the fallback for the documents that carry no
/// id — and those (the config document, the registry, the recycle-bin index, an
/// attachment payload) are disproportionately the victims of the sync damage
/// this store exists to survive, so the weaker key still has to exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    /// A registry id, read from the manifest's `id` column. Never resolved
    /// through the live registry — the point of the query is that it answers for
    /// documents that are no longer there to resolve.
    Id(Id),
    /// A workspace-relative path. A rename before or after the run shows up as a
    /// separate lineage; that is the nature of a path key, not a defect here.
    Path(PathBuf),
}

// ── Layout: id ⇄ path, hash → blob ───────────────────────────────────────────

/// The shard directory an event id belongs in, relative to the store's `events/`
/// directory: `<YYYY>/<MM>`, parsed straight out of the id's own leading
/// `YYYY-MM-`.
///
/// This is what makes "the index is only a cache" true rather than aspirational:
/// an id resolves to a path with every index file destroyed.
pub fn shard_of(id: &str) -> Result<PathBuf> {
    let bad = || Error::Structure(format!("`{id}` is not a history event id"));
    let (year, rest) = id.split_once('-').ok_or_else(bad)?;
    let (month, _) = rest.split_once('-').ok_or_else(bad)?;
    if year.len() != 4
        || month.len() != 2
        || !year.bytes().all(|b| b.is_ascii_digit())
        || !month.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(bad());
    }
    Ok(PathBuf::from(year).join(month))
}

/// Where the event document for `id` lives, given the store's index document.
/// A pure function of the id — the whole point of repeating the date in it.
pub fn event_path(store_index: &Path, id: &str, ext: &str) -> Result<PathBuf> {
    Ok(store_dir(store_index)
        .join(EVENTS_DIR)
        .join(shard_of(id)?)
        .join(format!("{id}.{ext}")))
}

/// Where the blob for `hash` lives: `blobs/<first-2-hex>/<rest>`.
///
/// **Bare hex, never the `sha256:` scheme prefix an event spells** — a colon in a
/// filename is hostile to Windows and to more than one sync client.
pub fn blob_path(store_index: &Path, hash: &str) -> Result<PathBuf> {
    let hex = hash.strip_prefix("sha256:").ok_or_else(|| {
        Error::Structure(format!("`{hash}` is not a sha256 digest prov can park"))
    })?;
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Structure(format!("`{hash}` is not a sha256 digest")));
    }
    let (prefix, rest) = hex.split_at(2);
    Ok(store_dir(store_index)
        .join(BLOBS_DIR)
        .join(prefix)
        .join(rest))
}

/// The store's directory — the index document's own parent.
pub fn store_dir(store_index: &Path) -> PathBuf {
    store_index
        .parent()
        .unwrap_or(Path::new(HISTORY_DIR))
        .to_path_buf()
}

// ── The event id ─────────────────────────────────────────────────────────────

/// The bytes an event's id digest is taken over — its **canonical form**.
///
/// Deliberately independent of the metadata serialization format, so the same
/// workspace state yields the same id whether frontmatter is YAML, JSON or fig.
/// Tab-separated fields, one per line; see `docs/history-format.md` §4.1.
fn canonical_bytes(
    created: &str,
    trigger: &str,
    label: Option<&str>,
    parent: Option<&str>,
    files: &[FileEntry],
) -> Vec<u8> {
    let mut out = String::new();
    out.push_str(&format!("created\t{created}\n"));
    out.push_str(&format!("trigger\t{trigger}\n"));
    if let Some(label) = label {
        out.push_str(&format!("label\t{label}\n"));
    }
    if let Some(parent) = parent {
        out.push_str(&format!("parent\t{parent}\n"));
    }
    for file in files {
        let id = file.id.as_ref().map(|i| i.0.as_str()).unwrap_or("");
        out.push_str(&format!(
            "file\t{}\t{id}\t{}\n",
            slash_path(&file.path),
            file.hash
        ));
    }
    out.into_bytes()
}

/// Mint the event id: `<YYYY>-<MM>-<DD>-<HHMM>[-<label-slug>]-<8 hex>`.
///
/// The suffix is content-derived rather than random — prov has a dependency-free
/// SHA-256 and no RNG, and the library stays clockless and deterministic, taking
/// its timestamp as an argument exactly as `recycle` does. It also makes
/// collisions *benign*: two devices producing byte-identical events yield the
/// same filename holding the same content, which is convergence rather than
/// conflict.
fn mint_id(
    created: &str,
    trigger: &str,
    label: Option<&str>,
    parent: Option<&str>,
    files: &[FileEntry],
) -> Result<String> {
    let stamp = id_stamp(created)?;
    let digest = crate::fixity::digest(&canonical_bytes(created, trigger, label, parent, files));
    let short = &digest["sha256:".len().."sha256:".len() + 8];
    Ok(match label.map(link::slug) {
        Some(slug) => format!("{stamp}-{slug}-{short}"),
        None => format!("{stamp}-{short}"),
    })
}

/// `YYYY-MM-DD-HHMM` from an RFC 3339 UTC timestamp — the human-readable head of
/// an event id. Full precision stays in the document's `created`.
fn id_stamp(created: &str) -> Result<String> {
    let bad = || Error::Structure(format!("`{created}` is not an RFC 3339 UTC timestamp"));
    let bytes = created.as_bytes();
    if bytes.len() < 16 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[13] != b':' {
        return Err(bad());
    }
    let digits = |range: std::ops::Range<usize>| {
        created
            .get(range)
            .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))
            .ok_or_else(bad)
    };
    Ok(format!(
        "{}-{}-{}-{}{}",
        digits(0..4)?,
        digits(5..7)?,
        digits(8..10)?,
        digits(11..13)?,
        digits(14..16)?
    ))
}

/// `2026-07-31 09:15` read back out of an event id — the display form of the
/// stamp `id_stamp` encoded.
fn display_stamp(id: &str) -> String {
    let parts: Vec<&str> = id.splitn(5, '-').collect();
    match parts.as_slice() {
        [y, m, d, hm, ..] if hm.len() == 4 => format!("{y}-{m}-{d} {}:{}", &hm[..2], &hm[2..]),
        _ => id.to_string(),
    }
}

/// The label slug an id carries, if any — everything between the time and the
/// digest suffix. Lets an index document label its entries without opening every
/// event, which matters because two captures in the same minute would otherwise
/// read identically.
fn label_slug(id: &str) -> Option<String> {
    let parts: Vec<&str> = id.split('-').collect();
    let [_, _, _, _, rest @ ..] = parts.as_slice() else {
        return None;
    };
    // The last segment is the digest; anything before it is the slug.
    match rest.len() {
        0 | 1 => None,
        n => Some(rest[..n - 1].join("-")),
    }
}

/// How an index document names one event: its timestamp, plus the label slug
/// when it has one.
fn display_entry(id: &str) -> String {
    match label_slug(id) {
        Some(slug) => format!("{} ({slug})", display_stamp(id)),
        None => display_stamp(id),
    }
}

/// A path spelled with `/` separators regardless of host platform — what goes
/// into a manifest and into the canonical form, so an event minted on Windows and
/// one minted on Linux describe the same state identically.
fn slash_path(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Whether `path` sits inside `dir` (or *is* it) — the capture-set exclusion
/// test, applied to normalized workspace-relative paths.
fn under(path: &Path, dir: &Path) -> bool {
    dir.as_os_str().is_empty() || path == dir || path.starts_with(dir)
}

// ── Reading the store ────────────────────────────────────────────────────────

/// Parse an event document's frontmatter into an [`Event`], or `None` when it is
/// not one (no `files` manifest, or no `created`).
fn parse_event(path: &Path, id: &str, meta: &Value) -> Option<Event> {
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
fn render_index(
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
const STORE_PROSE: &str = "\
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

// ── The operations ───────────────────────────────────────────────────────────

impl<FS: Storage, IdP, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// The extension the store's documents are authored with — the root
    /// document's own content format, falling back to Markdown when the root is a
    /// whole-file metadata document (which has no prose body to inherit).
    fn history_ext(&self, root_doc: &Path) -> &'static str {
        crate::ContentFormat::from_extension(root_doc)
            .unwrap_or(crate::ContentFormat::Markdown)
            .extension()
    }

    /// The fenced-frontmatter archetype the store's documents are authored in —
    /// the workspace's own metadata format, so a fig workspace's history reads
    /// like the rest of it.
    fn history_embed(&self) -> Result<fig::EmbedType> {
        match crate::document::frontmatter_carrier(self.default_embed_format()) {
            MetaCarrier::Fenced(embed) => Ok(embed),
            // `frontmatter_carrier` only ever returns a fenced archetype.
            _ => Err(Error::Structure(
                "history events need a fenced frontmatter carrier".into(),
            )),
        }
    }

    /// The store index document: the one the root's `history` pointer names, or —
    /// when the root declares none yet — where the first capture will put it.
    /// The `bool` is whether the store already exists.
    async fn history_store_index(&self, root_doc: &Path) -> Result<(PathBuf, bool)> {
        Ok(match self.history_path(root_doc).await? {
            Some(path) => (path, true),
            None => (
                PathBuf::from(HISTORY_DIR).join(format!("index.{}", self.history_ext(root_doc))),
                false,
            ),
        })
    }

    /// The **capture set**: the live graph, minus prov's two byte-parking stores.
    ///
    /// [`reachable_files`](Workspace::reachable_files) — §8's bounded walk, the
    /// same population `check` validates — with two exclusions, each load-bearing:
    ///
    /// - **`history/` itself.** It is reachable off the root, so a naive "capture
    ///   everything reachable" would capture the store inside the store: no
    ///   capture could ever be empty, and an exact restore of an old event would
    ///   delete every event newer than it, destroying the recovery points
    ///   themselves. The store is the one subtree the mechanism is deliberately
    ///   blind to.
    /// - **`recyclebin/items/`.** Already unreached, and excluded even so, on
    ///   purpose: bytes the user has consigned to the bin should not be *newly*
    ///   retained by a routine capture.
    ///
    /// Everything else structural stays in — the registry, the config document,
    /// and the recycle bin's *index*. Capturing the bin index keeps the common
    /// case correct: a document live at capture time comes back live, and the bin
    /// index reverts to a state that does not list it.
    pub async fn history_capture_set(&self, root_doc: &Path) -> Result<Vec<PathBuf>> {
        let (store_index, _) = self.history_store_index(root_doc).await?;
        let store = store_dir(&store_index);
        let binned = self
            .recycle_bin_path(root_doc)
            .await?
            .map(|index| store_dir(&index).join("items"));
        Ok(self
            .reachable_files(root_doc)
            .await?
            .into_iter()
            .filter(|p| !under(p, &store))
            .filter(|p| binned.as_ref().is_none_or(|items| !under(p, items)))
            .collect())
    }

    /// Every event in the store, oldest first (by `created`, then id).
    ///
    /// Read by **scanning the shard directories**, not by following the index
    /// documents — the indexes are a rebuildable cache, so a mangled one must not
    /// be able to hide an event that is sitting right there. A document that does
    /// not parse, or that carries no manifest, is skipped rather than fatal.
    pub async fn history_list(&self, root_doc: &Path) -> Result<Vec<Event>> {
        let (store_index, exists) = self.history_store_index(root_doc).await?;
        if !exists {
            return Ok(Vec::new());
        }
        let ext = self.history_ext(root_doc);
        let events_root = store_dir(&store_index).join(EVENTS_DIR);
        let mut events = Vec::new();
        for year in self.subdirs(&events_root).await? {
            for month in self.subdirs(&events_root.join(&year)).await? {
                let shard = events_root.join(&year).join(&month);
                for id in self.shard_event_ids(&shard, ext).await? {
                    let path = shard.join(format!("{id}.{ext}"));
                    let Ok((_, doc)) = self.load(&path).await else {
                        continue;
                    };
                    if let Some(event) = parse_event(&path, &id, &doc.meta) {
                        events.push(event);
                    }
                }
            }
        }
        events.sort_by(|a, b| a.created.cmp(&b.created).then(a.id.cmp(&b.id)));
        Ok(events)
    }

    /// One event by id, resolved through the **pure id → path function** rather
    /// than through any index — so an event answers for itself with every index
    /// document in the store destroyed.
    ///
    /// `Ok(None)` when the store holds no such event (including when there is no
    /// store yet). An error when `id` is not an event id at all, or when the
    /// document is sitting there but is not an event.
    pub async fn history_event(&self, root_doc: &Path, id: &str) -> Result<Option<Event>> {
        let (store_index, exists) = self.history_store_index(root_doc).await?;
        if !exists {
            return Ok(None);
        }
        let path = event_path(&store_index, id, self.history_ext(root_doc))?;
        if !self.fs().try_exists(&self.root().join(&path)).await? {
            return Ok(None);
        }
        let (_, doc) = self.load(&path).await?;
        parse_event(&path, id, &doc.meta)
            .map(Some)
            .ok_or_else(|| Error::Structure(format!("`{id}` is not a history event document")))
    }

    /// The captured paths in `event` whose pre-image bytes are **not** parked in
    /// the store — the "this event is half-synced" report.
    ///
    /// A manifest and the blobs it names travel over the transport
    /// independently, and a small event document routinely lands well before a
    /// hundred megabytes of bytes it points at. That is ordinary in-flight state
    /// rather than damage, which is exactly why it has to be legible under a
    /// *read* verb before anyone asks a restore to act on it — and why a restore
    /// reports this same set rather than computing its own.
    ///
    /// Presence is tested once per distinct hash, not once per row: a manifest
    /// routinely names one blob from several paths, and a workspace is captured
    /// whole. A row whose hash prov could not have parked in the first place
    /// (a foreign digest, a mangled string) names no blob that could be found, so
    /// it counts as missing rather than failing the whole read.
    pub async fn history_missing_blobs(
        &self,
        root_doc: &Path,
        event: &Event,
    ) -> Result<BTreeSet<PathBuf>> {
        let (store_index, _) = self.history_store_index(root_doc).await?;
        let mut seen: BTreeMap<&str, bool> = BTreeMap::new();
        let mut missing = BTreeSet::new();
        for file in &event.files {
            let present = match seen.get(file.hash.as_str()) {
                Some(present) => *present,
                None => {
                    let present = match blob_path(&store_index, &file.hash) {
                        Ok(blob) => self.fs().try_exists(&self.root().join(blob)).await?,
                        Err(_) => false,
                    };
                    seen.insert(&file.hash, present);
                    present
                }
            };
            if !present {
                missing.insert(file.path.clone());
            }
        }
        Ok(missing)
    }

    /// One document's lineage across every capture, oldest first: pull its row
    /// out of each manifest in turn, and keep only the events where that row
    /// *changed*.
    ///
    /// This is the payoff for the manifest's `id` column, and it is a **derived
    /// query, not a storage design** — nothing in the store is keyed by document,
    /// and nothing here writes. Following a [`Subject::Id`] makes the lineage
    /// rename-robust in a way no path-keyed store can be: a move shows as one
    /// document that changed path, where a path-keyed view shows two unrelated
    /// lineages that happen to abut.
    ///
    /// Consecutive events are deduped on the **whole manifest row** — path, id
    /// and hash — not on the hash alone. A rename leaves the bytes
    /// byte-identical, so a hash-only dedupe would swallow precisely the event
    /// that following an id exists to surface. Including the id means a document
    /// acquiring one is a point too, which is right: the row changed.
    ///
    /// An event that does not mention the subject records [`Presence::Gone`], but
    /// only once the document has been seen, so a lineage starts where its
    /// document does rather than with a run of absences. Events are walked in
    /// capture order (`created`, then id), so concurrent captures on two devices
    /// interleave rather than branching — this is a display, and `history-list`
    /// is where forks are named.
    ///
    /// Cost is one pass over every event document in the store. That is the
    /// honest price of storing by consistent cut and querying by document, and it
    /// is why this is a query rather than an index.
    pub async fn history_log(&self, root_doc: &Path, subject: &Subject) -> Result<Vec<Version>> {
        let mut log: Vec<Version> = Vec::new();
        for event in self.history_list(root_doc).await? {
            let row = event.files.iter().find(|file| match subject {
                Subject::Id(id) => file.id.as_ref() == Some(id),
                Subject::Path(path) => &file.path == path,
            });
            let state = match row {
                Some(file) => Presence::At {
                    path: file.path.clone(),
                    id: file.id.clone(),
                    hash: file.hash.clone(),
                },
                None => Presence::Gone,
            };
            match log.last() {
                // The document did not exist yet when this capture was taken.
                None if state == Presence::Gone => continue,
                Some(previous) if previous.state == state => continue,
                _ => {}
            }
            log.push(Version {
                event: event.id,
                created: event.created,
                label: event.label,
                state,
            });
        }
        Ok(log)
    }

    /// Capture the workspace: hash the capture set, park newly-seen blobs, and
    /// write one immutable event document into its `<YYYY>/<MM>` shard.
    ///
    /// `now` is the caller-supplied RFC 3339 UTC timestamp (the CLI passes the
    /// current time). The library takes it as an argument rather than reading a
    /// clock, so the op stays deterministic — the same convention `recycle` uses.
    ///
    /// **Adds files only**, except the current month's rebuildable index (and, on
    /// a new month or year, the shard index above it — itself pure addition). If
    /// the computed manifest equals the newest existing event's, nothing is
    /// written and [`Captured::Unchanged`] names the event that already describes
    /// this state.
    ///
    /// ## Why blobs do not ride the change set
    ///
    /// The event document and the shard indexes are staged in one journaled
    /// [`ChangeSet`], because they must land together. **Blobs are not**: the
    /// journal embeds file contents ([`crate::journal::encode`]), so a genesis
    /// capture riding the change set would write a second whole copy of the
    /// workspace into `.prov-journal`. They go through
    /// [`Storage::write_atomic`] directly instead, which is safe precisely
    /// because a content-addressed write is idempotent — replaying it can only
    /// write the same bytes to the same path.
    ///
    /// Blobs are parked *before* the change set lands, so the failure mode is an
    /// orphaned blob (reported by `check`, collected by `history-prune`) rather
    /// than an event whose bytes are missing.
    pub async fn history_capture(
        &mut self,
        root_doc: &Path,
        now: &str,
        label: Option<&str>,
    ) -> Result<Captured> {
        let root_doc = link::normalize(root_doc);
        let ext = self.history_ext(&root_doc);
        let embed = self.history_embed()?;
        let (store_index, store_exists) = self.history_store_index(&root_doc).await?;
        let label = label.map(str::trim).filter(|l| !l.is_empty());

        // Bootstrapping the store *edits the root* (it gains the `history`
        // pointer), so that edit is computed up front and the manifest hashes the
        // post-edit bytes. Otherwise the very first event would record a root
        // predating its own store — and restoring it exactly would strand the
        // store unreachable, which is the one thing a restore must never do.
        let root_pointer = match store_exists {
            true => None,
            false => Some(self.history_pointer_text(&root_doc, &store_index).await?),
        };

        // The manifest: one row per captured file, sorted by path. The capture set
        // is already a sorted set, so the manifest inherits that order.
        //
        // Each file's bytes are hashed and parked in the same pass, then dropped —
        // a workspace is captured whole, so accumulating every file's contents to
        // park them afterwards would hold the entire workspace in memory. Parking
        // *before* the change set lands also fixes the failure mode the right way
        // round: an interrupted capture leaves an orphaned blob (reported by
        // `check`, collected by `history-prune`) rather than an event whose bytes
        // are missing.
        let mut files = Vec::new();
        let mut parked = 0usize;
        for path in self.history_capture_set(&root_doc).await? {
            let bytes = match &root_pointer {
                Some(text) if path == root_doc => text.clone().into_bytes(),
                _ => self.fs().read(&self.root().join(&path)).await?,
            };
            let hash = crate::fixity::digest(&bytes);
            // Content-addressed, so a hash already on disk *is* the same bytes —
            // nothing to rewrite, and two devices parking the same content
            // converge instead of conflicting.
            let blob = self.root().join(blob_path(&store_index, &hash)?);
            if !self.fs().try_exists(&blob).await? {
                if let Some(dir) = blob.parent() {
                    self.fs().create_dir_all(dir).await?;
                }
                self.fs().write_atomic(&blob, &bytes).await?;
                parked += 1;
            }
            let id = self.index().id_for_path(&path);
            files.push(FileEntry { path, id, hash });
        }

        // The newest local event: what a new capture compares against, and what
        // it records as `parent` (display metadata — nothing computes through it).
        let existing = self.history_list(&root_doc).await?;
        let newest = existing.last();
        if let Some(previous) = newest
            && previous.files == files
        {
            return Ok(Captured::Unchanged {
                id: previous.id.clone(),
            });
        }
        let parent = newest.map(|e| e.id.as_str());
        let id = mint_id(now, TRIGGER_MANUAL, label, parent, &files)?;
        let event_rel = event_path(&store_index, &id, ext)?;

        let diff = newest.map(|previous| {
            let event = Event {
                id: id.clone(),
                path: event_rel.clone(),
                created: now.to_string(),
                trigger: TRIGGER_MANUAL.to_string(),
                label: label.map(str::to_owned),
                parent: parent.map(str::to_owned),
                files: files.clone(),
            };
            event.diff(previous)
        });

        // The event document. `part_of` points at its own shard index; the event
        // carries no `id` field — minting registry ids for events would make every
        // capture write `registry.<ext>`, the conflict-prone shape this store
        // exists to avoid.
        let mut map = Mapping::new();
        map.insert(
            "part_of".into(),
            Value::String(format!("[{}](index.{ext})", shard_title(&id))),
        );
        map.insert("created".into(), Value::String(now.to_string()));
        map.insert("trigger".into(), Value::String(TRIGGER_MANUAL.to_string()));
        if let Some(label) = label {
            map.insert("label".into(), Value::String(label.to_string()));
        }
        if let Some(parent) = parent {
            map.insert("parent".into(), Value::String(parent.to_string()));
        }
        map.insert(
            "files".into(),
            Value::Sequence(
                files
                    .iter()
                    .map(|f| {
                        let mut row = Mapping::new();
                        row.insert("path".into(), Value::String(slash_path(&f.path)));
                        if let Some(id) = &f.id {
                            row.insert("id".into(), Value::String(id.0.clone()));
                        }
                        row.insert("hash".into(), Value::String(f.hash.clone()));
                        Value::Mapping(row)
                    })
                    .collect(),
            ),
        );
        let summary = match diff {
            Some((changed, removed)) => format!(
                "Captured {} file(s) — {changed} changed, {removed} removed since \
                 the previous event.",
                files.len()
            ),
            None => format!(
                "Captured {} file(s). This is the first event in the store.",
                files.len()
            ),
        };
        let body = format!(
            "# History — {}\n\n{summary}\n\nRoll the workspace back to this point with:\n\n    \
             prov history-restore {id}\n",
            Event {
                id: id.clone(),
                path: event_rel.clone(),
                created: now.to_string(),
                trigger: TRIGGER_MANUAL.to_string(),
                label: label.map(str::to_owned),
                parent: None,
                files: Vec::new(),
            }
            .describe()
        );
        let event_text = crate::edit::reformat_block(&body, &map, embed)?;

        let mut cs = self.change();
        cs.write(&event_rel, event_text);
        self.stage_history_indexes(&mut cs, &store_index, &id, ext, embed)
            .await?;
        if let Some(text) = root_pointer {
            cs.write(&root_doc, text);
        }
        self.commit(cs).await?;

        Ok(Captured::Written {
            id,
            files: files.len(),
            blobs: parked,
            diff,
        })
    }

    /// Stage a rebuild of every index document on the path from the store root
    /// down to `id`'s month shard, each rendered from its own directory listing
    /// (plus the event this capture is adding, which is not on disk yet).
    ///
    /// Rebuilding rather than surgically appending is what keeps "the index is a
    /// cache" honest: capture and the [`Fix::RebuildHistoryIndex`] autofix run the
    /// same code, so a repaired index is byte-identical to a freshly written one.
    ///
    /// [`Fix::RebuildHistoryIndex`]: crate::Fix::RebuildHistoryIndex
    async fn stage_history_indexes(
        &self,
        cs: &mut ChangeSet,
        store_index: &Path,
        id: &str,
        ext: &str,
        embed: fig::EmbedType,
    ) -> Result<()> {
        let shard = shard_of(id)?;
        let (year, month) = shard_parts(&shard)?;
        let events_root = store_dir(store_index).join(EVENTS_DIR);
        let shard_dir = events_root.join(&shard);

        let mut ids = self.shard_event_ids(&shard_dir, ext).await?;
        ids.insert(id.to_string());
        cs.write(
            shard_dir.join(format!("index.{ext}")),
            render_month_index(&year, &month, &ids, ext, embed)?,
        );

        let mut months = self.subdirs(&events_root.join(&year)).await?;
        months.insert(month.clone());
        cs.write(
            events_root.join(&year).join(format!("index.{ext}")),
            render_year_index(&year, &months, ext, embed)?,
        );

        let mut years = self.subdirs(&events_root).await?;
        years.insert(year.clone());
        cs.write(store_index, render_store_index(&years, ext, embed)?);
        Ok(())
    }

    /// The root document's text with its `history` pointer at the store index —
    /// authored the first time only, as a plain relative path (the same shape
    /// `recycle` gives the bin pointer), comment- and format-preservingly.
    ///
    /// Computed rather than staged directly so the capture can hash *this* text
    /// into its own manifest, and so the pointer still lands in the same
    /// [`ChangeSet`] as the event — a store written without the pointer would be
    /// unreachable, and invisible to `check`.
    async fn history_pointer_text(&self, root_doc: &Path, store_index: &Path) -> Result<String> {
        let relation = self
            .relations()
            .history_relation()
            .ok_or_else(|| Error::Structure("no history relation configured".into()))?
            .to_string();
        let (text, doc) = self.load(root_doc).await?;
        let root_dir = root_doc.parent().unwrap_or(Path::new(""));
        let pointer = link::relative(root_dir, store_index);
        crate::edit::set_in_text(
            &text,
            doc.carrier,
            &relation,
            crate::edit::infer_scalar(&pointer),
        )
    }

    /// The event ids in one shard directory: every `*.<ext>` file that is not the
    /// shard's own index. Directory-driven, so it sees exactly what is there.
    async fn shard_event_ids(&self, shard: &Path, ext: &str) -> Result<BTreeSet<String>> {
        let suffix = format!(".{ext}");
        let index = format!("index.{ext}");
        let mut ids = BTreeSet::new();
        let Ok(entries) = self.fs().read_dir(&self.root().join(shard)).await else {
            return Ok(ids);
        };
        for entry in entries {
            let Some(name) = entry.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !entry.file_type().is_file() || name.starts_with('.') || name == index {
                continue;
            }
            if let Some(stem) = name.strip_suffix(&suffix)
                && is_event_id(stem)
            {
                ids.insert(stem.to_string());
            }
        }
        Ok(ids)
    }

    /// The immediate subdirectory names of `dir`, sorted. An unreadable or absent
    /// directory is empty, not an error — the store is grown lazily.
    async fn subdirs(&self, dir: &Path) -> Result<BTreeSet<String>> {
        let mut names = BTreeSet::new();
        let Ok(entries) = self.fs().read_dir(&self.root().join(dir)).await else {
            return Ok(names);
        };
        for entry in entries {
            let Some(name) = entry.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if entry.file_type().is_dir() && !name.starts_with('.') {
                names.insert(name.to_string());
            }
        }
        Ok(names)
    }

    /// Validate the history store: every index document against the directory it
    /// describes, emitting one [`Finding::HistoryIndexStale`] per index that has
    /// drifted.
    ///
    /// The store's interior needs its own pass rather than riding `check`'s
    /// general walk, because descent is **spanning-only**: the root reaches the
    /// store index through the one-way `history` pointer, and the walk does not
    /// descend a non-spanning edge. That is the right default for every other
    /// pointer-reached store, and it means the shard directories are neither
    /// scanned for orphans nor validated — so history validates them here, from
    /// the directories themselves, which is also what makes the check immune to
    /// the very staleness it is looking for.
    pub async fn history_findings(&self, root_doc: &Path) -> Result<Vec<Finding>> {
        let (store_index, exists) = self.history_store_index(root_doc).await?;
        if !exists {
            return Ok(Vec::new());
        }
        let ext = self.history_ext(root_doc);
        let embed = self.history_embed()?;
        let events_root = store_dir(&store_index).join(EVENTS_DIR);
        let mut findings = Vec::new();

        let years = self.subdirs(&events_root).await?;
        self.compare_index(
            &mut findings,
            &store_index,
            &render_store_index(&years, ext, embed)?,
        )
        .await?;

        for year in &years {
            let months = self.subdirs(&events_root.join(year)).await?;
            self.compare_index(
                &mut findings,
                &events_root.join(year).join(format!("index.{ext}")),
                &render_year_index(year, &months, ext, embed)?,
            )
            .await?;
            for month in &months {
                let shard = events_root.join(year).join(month);
                let ids = self.shard_event_ids(&shard, ext).await?;
                self.compare_index(
                    &mut findings,
                    &shard.join(format!("index.{ext}")),
                    &render_month_index(year, month, &ids, ext, embed)?,
                )
                .await?;
            }
        }
        Ok(findings)
    }

    /// Compare one index document against what it *should* say, by the set of
    /// entries each declares. Compared on the resolved link set rather than the
    /// raw text, so hand-edited prose or a reordered block is not "stale" — only a
    /// genuinely missing or surplus entry is.
    async fn compare_index(
        &self,
        findings: &mut Vec<Finding>,
        index: &Path,
        expected_text: &str,
    ) -> Result<()> {
        let expected = match crate::document::Document::parse(index, expected_text) {
            Ok(doc) => index_entries(index, &doc.meta),
            Err(_) => Vec::new(),
        };
        let actual = match self.load(index).await {
            Ok((_, doc)) => index_entries(index, &doc.meta).into_iter().collect(),
            // No index where one is owed. Only a finding if the directory has
            // something to describe — an empty store is simply not there yet.
            Err(_) if expected.is_empty() => return Ok(()),
            Err(_) => BTreeSet::new(),
        };
        let expected: BTreeSet<PathBuf> = expected.into_iter().collect();
        let missing: Vec<PathBuf> = expected.difference(&actual).cloned().collect();
        let extra: Vec<PathBuf> = actual.difference(&expected).cloned().collect();
        if !missing.is_empty() || !extra.is_empty() {
            findings.push(Finding::HistoryIndexStale {
                index: index.to_path_buf(),
                missing,
                extra,
            });
        }
        Ok(())
    }

    /// The text one history index document *should* hold, rebuilt from the
    /// directory it describes — the repair behind [`Fix::RebuildHistoryIndex`].
    ///
    /// Takes only the index's own path: which of the three index kinds it is
    /// falls out of where it sits relative to the store's `events/` directory, so
    /// the repair needs neither the root document nor the `history` pointer —
    /// which matters, because a workspace whose *store index* was mangled is
    /// exactly when you want to rebuild without depending on it.
    ///
    /// Per-shard by construction: a mangled `2026/07/index.<ext>` is rebuilt from
    /// that one directory's listing, touching no other month.
    ///
    /// [`Fix::RebuildHistoryIndex`]: crate::Fix::RebuildHistoryIndex
    pub async fn history_index_text(&self, index: &Path) -> Result<String> {
        let index = link::normalize(index);
        let ext = index
            .extension()
            .and_then(|e| e.to_str())
            .ok_or_else(|| Error::Structure(format!("{} has no extension", index.display())))?;
        let embed = self.history_embed()?;
        let dir = index.parent().unwrap_or(Path::new(""));

        // Locate the store's `events/` directory by name, walking up from the
        // index. Its absence means this *is* the store index.
        let depth_below_events = dir
            .components()
            .rev()
            .position(|c| c.as_os_str() == EVENTS_DIR);
        match depth_below_events {
            // `<store>/events/<year>/<month>/index.<ext>`
            Some(2) => {
                let (year, month) = shard_parts(
                    dir.parent()
                        .and_then(|p| p.parent())
                        .map(|events| dir.strip_prefix(events).unwrap_or(dir))
                        .unwrap_or(dir),
                )?;
                render_month_index(
                    &year,
                    &month,
                    &self.shard_event_ids(dir, ext).await?,
                    ext,
                    embed,
                )
            }
            // `<store>/events/<year>/index.<ext>`
            Some(1) => {
                let year = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                render_year_index(&year, &self.subdirs(dir).await?, ext, embed)
            }
            // `<store>/index.<ext>` — the store index itself.
            _ => render_store_index(&self.subdirs(&dir.join(EVENTS_DIR)).await?, ext, embed),
        }
    }
}

/// The `<year>`/`<month>` pair of a shard path, or an error when it is not one.
fn shard_parts(shard: &Path) -> Result<(String, String)> {
    let parts: Vec<String> = shard
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    match parts.as_slice() {
        [year, month] => Ok((year.clone(), month.clone())),
        _ => Err(Error::Structure(format!(
            "{} is not a history shard directory",
            shard.display()
        ))),
    }
}

/// The month-shard title an event's `part_of` label uses: `July 2026`.
fn shard_title(id: &str) -> String {
    match shard_of(id).ok().as_deref().map(shard_parts) {
        Some(Ok((year, month))) => format!("{} {year}", month_name(&month)),
        _ => "History".to_string(),
    }
}

/// The English month name for a two-digit month, or the digits themselves when
/// they are not a month (a hand-made directory prov did not write).
fn month_name(month: &str) -> &str {
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
fn index_entries(index: &Path, meta: &Value) -> Vec<PathBuf> {
    meta.get("contents")
        .map(Value::link_strings)
        .unwrap_or_default()
        .iter()
        .map(|raw| link::resolve(index, &crate::link::Link::parse(raw).target))
        .collect()
}

/// Whether `stem` is a well-formed event id: `YYYY-MM-DD-HHMM[-slug]-<8 hex>`.
///
/// The gate that keeps a transport's leavings out of the store. A
/// `.sync-conflict-20260731-091600` copy of an event or an index ends in six
/// digits rather than eight hex characters, so it is litter beside the store
/// rather than a phantom event — which matters, because an index rebuilt to
/// *include* the conflict copy would enshrine the damage it is repairing.
fn is_event_id(stem: &str) -> bool {
    let parts: Vec<&str> = stem.split('-').collect();
    let [year, month, day, time, rest @ ..] = parts.as_slice() else {
        return false;
    };
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    let Some(digest) = rest.last() else {
        return false;
    };
    digits(year, 4)
        && digits(month, 2)
        && digits(day, 2)
        && digits(time, 4)
        && digest.len() == 8
        && digest.bytes().all(|b| b.is_ascii_hexdigit())
}

fn render_store_index(
    years: &BTreeSet<String>,
    ext: &str,
    embed: fig::EmbedType,
) -> Result<String> {
    let entries: Vec<(String, String)> = years
        .iter()
        .map(|year| (year.clone(), format!("{EVENTS_DIR}/{year}/index.{ext}")))
        .collect();
    render_index("History", None, &entries, STORE_PROSE, embed)
}

fn render_year_index(
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

fn render_month_index(
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

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::*;
    use crate::exec::block_on;
    use crate::fs::StdFs;
    use crate::identity::Minter;
    use crate::index::FileIndex;

    #[test]
    fn an_event_id_is_reversible_to_its_shard_path() {
        let id = "2026-07-31-0915-pre-sync-4f2a9c1e";
        assert_eq!(shard_of(id).unwrap(), Path::new("2026").join("07"));
        assert_eq!(
            event_path(Path::new("history/index.md"), id, "md").unwrap(),
            Path::new("history/events/2026/07/2026-07-31-0915-pre-sync-4f2a9c1e.md")
        );
        // The point of repeating the date in the id: it resolves with every
        // index file destroyed.
        assert!(shard_of("not-an-event-id").is_err());
    }

    #[test]
    fn a_blob_path_is_bare_hex_never_the_scheme_prefix() {
        let hash = crate::fixity::digest(b"hello");
        let path = blob_path(Path::new("history/index.md"), &hash).unwrap();
        let spelled = path.to_string_lossy();
        assert!(
            !spelled.contains(':'),
            "a colon in a blob filename is hostile to Windows and to sync clients: {spelled}"
        );
        let hex = hash.strip_prefix("sha256:").unwrap();
        assert_eq!(
            path,
            Path::new("history/blobs").join(&hex[..2]).join(&hex[2..])
        );
        assert!(blob_path(Path::new("history/index.md"), "blake3:beef").is_err());
    }

    #[test]
    fn the_id_stamp_reads_the_timestamp_and_survives_a_round_trip() {
        assert_eq!(id_stamp("2026-07-31T09:15:22Z").unwrap(), "2026-07-31-0915");
        assert!(id_stamp("yesterday").is_err());
        assert_eq!(
            display_stamp("2026-07-31-0915-pre-sync-4f2a9c1e"),
            "2026-07-31 09:15"
        );
        // Two captures in the same minute must not read identically in an index,
        // so the entry carries the label slug the id already encodes.
        assert_eq!(
            display_entry("2026-07-31-0915-pre-sync-4f2a9c1e"),
            "2026-07-31 09:15 (pre-sync)"
        );
        assert_eq!(
            label_slug("2026-07-31-0915-pre-sync-4f2a9c1e"),
            Some("pre-sync".into())
        );
        assert_eq!(label_slug("2026-07-31-0915-4f2a9c1e"), None);
        assert_eq!(
            display_entry("2026-07-31-0915-4f2a9c1e"),
            "2026-07-31 09:15"
        );
    }

    fn entry(path: &str, hash_of: &[u8]) -> FileEntry {
        FileEntry {
            path: PathBuf::from(path),
            id: None,
            hash: crate::fixity::digest(hash_of),
        }
    }

    #[test]
    fn the_canonical_form_ignores_the_serialization_format() {
        // Two devices, same state, same timestamp — the id must converge, which
        // is what makes a collision benign rather than a conflict.
        let files = vec![entry("a.md", b"a"), entry("b.md", b"b")];
        let one = mint_id("2026-07-31T09:15:22Z", TRIGGER_MANUAL, None, None, &files).unwrap();
        let two = mint_id("2026-07-31T09:15:22Z", TRIGGER_MANUAL, None, None, &files).unwrap();
        assert_eq!(one, two);

        // A different capture set is a different event.
        let changed = vec![entry("a.md", b"a"), entry("b.md", b"CHANGED")];
        let three = mint_id("2026-07-31T09:15:22Z", TRIGGER_MANUAL, None, None, &changed).unwrap();
        assert_ne!(one, three);
        // …and so is a different parent, so two devices forking from different
        // points do not collide.
        let forked = mint_id(
            "2026-07-31T09:15:22Z",
            TRIGGER_MANUAL,
            None,
            Some("2026-07-30-1804-nightly-8c1d55aa"),
            &files,
        )
        .unwrap();
        assert_ne!(one, forked);
    }

    #[test]
    fn a_label_is_slugged_into_the_id_and_omitted_when_absent() {
        let files = vec![entry("a.md", b"a")];
        let labeled = mint_id(
            "2026-07-31T09:15:22Z",
            TRIGGER_MANUAL,
            Some("Pre Sync!"),
            None,
            &files,
        )
        .unwrap();
        assert!(
            labeled.starts_with("2026-07-31-0915-pre-sync-"),
            "{labeled}"
        );
        let bare = mint_id("2026-07-31T09:15:22Z", TRIGGER_MANUAL, None, None, &files).unwrap();
        assert!(bare.starts_with("2026-07-31-0915-"), "{bare}");
        // Both still parse back to the same shard.
        assert_eq!(shard_of(&labeled).unwrap(), shard_of(&bare).unwrap());
    }

    #[test]
    fn diff_counts_changed_and_removed_against_the_previous_manifest() {
        let previous = Event {
            id: "p".into(),
            path: PathBuf::new(),
            created: "2026-07-30T00:00:00Z".into(),
            trigger: TRIGGER_MANUAL.into(),
            label: None,
            parent: None,
            files: vec![entry("a.md", b"a"), entry("gone.md", b"g")],
        };
        let current = Event {
            files: vec![entry("a.md", b"CHANGED"), entry("new.md", b"n")],
            ..previous.clone()
        };
        // `a.md` changed, `new.md` is new → 2; `gone.md` is removed → 1.
        assert_eq!(current.diff(&previous), (2, 1));
    }

    #[test]
    fn the_capture_set_exclusion_is_by_directory_prefix() {
        let store = Path::new("history");
        assert!(under(Path::new("history"), store));
        assert!(under(Path::new("history/index.md"), store));
        assert!(under(Path::new("history/events/2026/07/x.md"), store));
        // A sibling that merely shares a prefix is not inside it.
        assert!(!under(Path::new("historybook.md"), store));
        assert!(!under(Path::new("notes/a.md"), store));
    }

    // ── Store-level tests, over a real filesystem ────────────────────────────

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prov-history-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn read(dir: &Path, rel: &str) -> String {
        std::fs::read_to_string(dir.join(rel)).unwrap()
    }

    fn ws(dir: &Path) -> Workspace<StdFs, Minter, FileIndex> {
        Workspace::builder(StdFs)
            .root(dir)
            .identity(Minter::lazy(42))
            .index(FileIndex::new(fig::Format::Yaml))
            .build()
    }

    /// A small workspace: a root, two notes, and an attachment (payload plus
    /// sidecar) — so the capture set covers the shapes that actually matter.
    fn seed(tag: &str) -> PathBuf {
        let dir = tempdir(tag);
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- notes/a.md\n- notes/photo.jpg.yaml\n---\nroot\n",
        );
        write(
            &dir,
            "notes/a.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n",
        );
        write(&dir, "notes/photo.jpg", "JPEGBYTES");
        write(
            &dir,
            "notes/photo.jpg.yaml",
            "title: Photo\npart_of: '../index.md'\ncontent: photo.jpg\n",
        );
        dir
    }

    fn capture(dir: &Path, now: &str, label: Option<&str>) -> Captured {
        block_on(ws(dir).history_capture(Path::new("index.md"), now, label)).unwrap()
    }

    fn event_ids(dir: &Path) -> Vec<String> {
        block_on(ws(dir).history_list(Path::new("index.md")))
            .unwrap()
            .into_iter()
            .map(|e| e.id)
            .collect()
    }

    #[test]
    fn a_capture_bootstraps_the_store_and_captures_attachment_payloads() {
        let dir = seed("capture-basic");
        let Captured::Written { id, files, .. } =
            capture(&dir, "2026-07-31T09:15:22Z", Some("pre-sync"))
        else {
            panic!("the first capture must write an event");
        };

        // The root now points at the store, so it is reachable — the whole
        // anti-`.obsidian/` move.
        assert!(
            read(&dir, "index.md").contains("history:"),
            "the root must declare the store: {}",
            read(&dir, "index.md")
        );
        // The id resolves to its path with no index consulted.
        let event = event_path(Path::new("history/index.md"), &id, "md").unwrap();
        assert!(dir.join(&event).exists(), "{} missing", event.display());

        // The capture set is the reachable file set: root, note, sidecar, and —
        // the one that is easy to get wrong — the attachment *payload*, which is
        // reached through the sidecar's `content` pointer rather than a relation.
        let manifest = read(&dir, event.to_str().unwrap());
        for expected in [
            "index.md",
            "notes/a.md",
            "notes/photo.jpg",
            "notes/photo.jpg.yaml",
        ] {
            assert!(
                manifest.contains(expected),
                "{expected} should be captured:\n{manifest}"
            );
        }
        assert_eq!(files, 4);

        // Every captured file's bytes are parked, addressed by content, with no
        // colon anywhere in the path.
        let payload_hash = crate::fixity::digest(b"JPEGBYTES");
        let blob = blob_path(Path::new("history/index.md"), &payload_hash).unwrap();
        assert_eq!(read(&dir, blob.to_str().unwrap()), "JPEGBYTES");
    }

    #[test]
    fn the_store_is_never_captured_into_itself() {
        // The recursion the whole design turns on: capturing the store inside the
        // store would mean no capture could ever be empty, and an exact restore
        // would delete the recovery points themselves.
        let dir = seed("capture-recursion");
        capture(&dir, "2026-07-31T09:15:22Z", None);
        let set = block_on(ws(&dir).history_capture_set(Path::new("index.md"))).unwrap();
        assert!(
            set.iter().all(|p| !p.starts_with("history")),
            "the store must be invisible to the mechanism: {set:?}"
        );
        // And that is exactly what makes the no-op capture reachable.
        let second = capture(&dir, "2026-07-31T10:00:00Z", None);
        assert!(
            matches!(second, Captured::Unchanged { .. }),
            "an unchanged workspace must write nothing, got {second:?}"
        );
    }

    #[test]
    fn an_unchanged_workspace_writes_no_second_event() {
        let dir = seed("capture-empty");
        let first = capture(&dir, "2026-07-31T09:15:22Z", None);
        let Captured::Written { id, .. } = first else {
            panic!("expected a first event")
        };
        // A different clock and a different label — still the same *state*, so
        // still nothing to record. Otherwise a git hook fills the log.
        let again = capture(&dir, "2026-07-31T11:00:00Z", Some("nightly"));
        assert_eq!(again, Captured::Unchanged { id: id.clone() });
        assert_eq!(event_ids(&dir), vec![id.clone()]);

        // Change one byte and it captures again.
        write(
            &dir,
            "notes/a.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\nalpha edited\n",
        );
        let third = capture(&dir, "2026-07-31T12:00:00Z", None);
        let Captured::Written {
            diff: Some((changed, removed)),
            blobs,
            ..
        } = third
        else {
            panic!("a changed workspace must capture")
        };
        assert_eq!((changed, removed), (1, 0));
        // Only the changed file's bytes are new — the rest deduplicate for free.
        assert_eq!(blobs, 1);
        assert_eq!(event_ids(&dir).len(), 2);
    }

    #[test]
    fn the_first_event_records_the_root_that_already_declares_the_store() {
        // The bootstrap capture edits the root (it gains the `history` pointer),
        // so the manifest must hash the *post-edit* bytes. Otherwise event #1
        // describes a root predating its own store, and restoring it exactly
        // would strand the store unreachable — the one thing a restore must never
        // do. It is also what lets the very next capture be a no-op.
        let dir = seed("capture-pointer");
        let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
            panic!("expected an event")
        };
        let events = block_on(ws(&dir).history_list(Path::new("index.md"))).unwrap();
        let root_row = events[0]
            .files
            .iter()
            .find(|f| f.path == Path::new("index.md"))
            .expect("the root is in the capture set");
        let on_disk = crate::fixity::digest(read(&dir, "index.md").as_bytes());
        assert_eq!(
            root_row.hash, on_disk,
            "event {id} must record the root as the capture left it"
        );
        // And the parked blob is those same bytes, so a restore is byte-exact.
        let blob = blob_path(Path::new("history/index.md"), &root_row.hash).unwrap();
        assert_eq!(read(&dir, blob.to_str().unwrap()), read(&dir, "index.md"));
    }

    #[test]
    fn a_transport_conflict_copy_is_not_mistaken_for_an_event() {
        // Litter beside the store must not become a phantom event — an index
        // rebuilt to *include* a conflict copy would enshrine the damage.
        assert!(is_event_id("2026-07-31-0915-pre-sync-4f2a9c1e"));
        assert!(is_event_id("2026-07-31-0915-4f2a9c1e"));
        assert!(!is_event_id(
            "2026-07-31-0915-one-1d1beacc.sync-conflict-20260731-091600"
        ));
        assert!(!is_event_id("index.sync-conflict-20260731-091600"));
        assert!(!is_event_id("index"));
        assert!(!is_event_id("notes"));
    }

    #[test]
    fn a_capture_leaves_check_clean() {
        let dir = seed("capture-check");
        capture(&dir, "2026-07-31T09:15:22Z", Some("pre-sync"));
        let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
        assert!(
            findings.is_empty(),
            "a capture must leave the workspace valid: {findings:?}"
        );
    }

    #[test]
    fn a_new_month_grows_the_shard_tree_without_rewriting_old_shards() {
        let dir = seed("capture-shard");
        capture(&dir, "2026-07-31T09:15:22Z", None);
        let july = read(&dir, "history/events/2026/07/index.md");

        write(&dir, "notes/b.md", "---\ntitle: B\n---\nbeta\n");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- notes/a.md\n- notes/b.md\n- notes/photo.jpg.yaml\n\
             history: history/index.md\n---\nroot\n",
        );
        write(
            &dir,
            "notes/b.md",
            "---\ntitle: B\npart_of: '../index.md'\n---\nbeta\n",
        );
        capture(&dir, "2026-08-01T09:00:00Z", None);

        // The new month is its own shard, linked from the year index; July's
        // shard index is untouched — the mutable surface is "this month", not
        // "forever".
        assert!(dir.join("history/events/2026/08/index.md").exists());
        assert_eq!(read(&dir, "history/events/2026/07/index.md"), july);
        assert!(read(&dir, "history/events/2026/index.md").contains("08/index.md"));
        assert!(
            block_on(ws(&dir).check(Path::new("index.md")))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn binned_bytes_are_not_newly_retained_by_a_routine_capture() {
        // The exclusion is narrow and worth pinning: a capture must not park
        // bytes the user has consigned to the bin. (It emphatically does *not*
        // make a purge final for content captured while it was live — that is
        // documented, not tested here, because it is a non-guarantee.)
        let dir = seed("capture-bin");
        write(
            &dir,
            "recyclebin/index.yaml",
            "title: Recycle Bin\ndeleted: []\n",
        );
        write(&dir, "recyclebin/items/notes/old.md", "binned bytes\n");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- notes/a.md\n- notes/photo.jpg.yaml\n\
             recycle_bin: recyclebin/index.yaml\n---\nroot\n",
        );
        let set = block_on(ws(&dir).history_capture_set(Path::new("index.md"))).unwrap();
        assert!(
            set.iter().all(|p| !p.starts_with("recyclebin/items")),
            "binned bytes must not be captured: {set:?}"
        );
        // The bin *index* is captured, though — that is what makes a restore put
        // a live document back as live.
        assert!(
            set.contains(&PathBuf::from("recyclebin/index.yaml")),
            "the bin index is ordinary structural state: {set:?}"
        );
    }

    // ── Transport simulation ─────────────────────────────────────────────────
    //
    // The feature's entire claim is surviving an external sync transport, so
    // these simulate one: two workspace copies, concurrent captures, and a
    // directory merge that unions added files, drops in a `.sync-conflict-…`
    // file, and clobbers a shard index.

    /// Copy every file under `from` into `to`, adding what is missing and leaving
    /// what is already there — the union-of-added-files merge that git, Dropbox,
    /// Syncthing and iCloud all perform without conflict.
    fn merge_into(from: &Path, to: &Path) {
        fn walk(dir: &Path, base: &Path, to: &Path) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let rel = path.strip_prefix(base).unwrap().to_path_buf();
                if path.is_dir() {
                    walk(&path, base, to);
                } else if !to.join(&rel).exists() {
                    let dest = to.join(&rel);
                    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
                    std::fs::copy(&path, &dest).unwrap();
                }
            }
        }
        walk(from, from, to);
    }

    #[test]
    fn concurrent_captures_on_two_devices_merge_without_conflict() {
        // Two devices, same starting state, each captures locally. Because a
        // capture only *adds* files, the transport's union merge produces both
        // events side by side — the whole point of the append-only design.
        let one = seed("transport-one");
        let two = tempdir("transport-two");
        merge_into(&one, &two);

        // Device one edits and captures.
        write(
            &one,
            "notes/a.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\nfrom device one\n",
        );
        let Captured::Written { id: id_one, .. } =
            capture(&one, "2026-07-31T09:15:22Z", Some("one"))
        else {
            panic!("device one must capture")
        };
        // Device two edits differently and captures — same minute, no coordination.
        write(
            &two,
            "notes/a.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\nfrom device two\n",
        );
        let Captured::Written { id: id_two, .. } =
            capture(&two, "2026-07-31T09:15:22Z", Some("two"))
        else {
            panic!("device two must capture")
        };
        assert_ne!(id_one, id_two, "different content must mint different ids");

        // The transport reconciles: every added file lands in device one's copy.
        merge_into(&two, &one);

        // Both events survive, and both devices' pre-images are present.
        let ids = event_ids(&one);
        assert!(
            ids.contains(&id_one) && ids.contains(&id_two),
            "a merge must not lose either device's event: {ids:?}"
        );
        for bytes in [b"from device one".as_slice(), b"from device two".as_slice()] {
            let hash = crate::fixity::digest(
                format!(
                    "---\ntitle: A\npart_of: '../index.md'\n---\n{}\n",
                    String::from_utf8_lossy(bytes)
                )
                .as_bytes(),
            );
            let blob = blob_path(Path::new("history/index.md"), &hash).unwrap();
            assert!(
                one.join(&blob).exists(),
                "both devices' pre-images must survive the merge: {}",
                blob.display()
            );
        }
    }

    #[test]
    fn a_merged_shard_index_is_reported_stale_and_rebuilt_from_its_directory() {
        // The one mutable file in the store is the shard index, so it is the one
        // a transport can mangle. That must be a finding with a mechanical fix,
        // never data loss — which is exactly what "the index is a cache" buys.
        let one = seed("transport-index");
        let two = tempdir("transport-index-two");
        merge_into(&one, &two);

        write(
            &one,
            "notes/a.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\none\n",
        );
        capture(&one, "2026-07-31T09:15:22Z", Some("one"));
        write(
            &two,
            "notes/a.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\ntwo\n",
        );
        capture(&two, "2026-07-31T09:16:00Z", Some("two"));

        // Merge device two's *event* across but let the transport clobber the
        // shard index with device two's copy — which knows nothing of device
        // one's event. This is the realistic damage: last-writer-wins on the
        // only file both devices rewrote.
        merge_into(&two, &one);
        std::fs::copy(
            two.join("history/events/2026/07/index.md"),
            one.join("history/events/2026/07/index.md"),
        )
        .unwrap();
        // …and drop in the conflict copy such a transport leaves behind.
        write(
            &one,
            "history/events/2026/07/index.sync-conflict-20260731-091600.md",
            "---\ntitle: July 2026\n---\nconflicted copy\n",
        );

        // Both events are still listed: `history-list` reads the directories, so
        // a mangled index cannot hide an event that is sitting right there.
        assert_eq!(
            event_ids(&one).len(),
            2,
            "the events are the authority, not the index"
        );

        // `check` names it, and the fix rebuilds that one shard.
        let findings = block_on(ws(&one).check(Path::new("index.md"))).unwrap();
        let stale: Vec<_> = findings
            .iter()
            .filter(|f| matches!(f, Finding::HistoryIndexStale { .. }))
            .collect();
        assert_eq!(stale.len(), 1, "expected one stale shard: {findings:?}");

        let mut w = ws(&one);
        let fix = block_on(w.suggest_fix(stale[0])).unwrap().expect("a fix");
        block_on(w.apply_fix(&fix)).unwrap();

        let after = block_on(ws(&one).check(Path::new("index.md"))).unwrap();
        assert!(
            !after
                .iter()
                .any(|f| matches!(f, Finding::HistoryIndexStale { .. })),
            "the rebuild should have settled the index: {after:?}"
        );
        let rebuilt = read(&one, "history/events/2026/07/index.md");
        for id in event_ids(&one) {
            assert!(
                rebuilt.contains(&id),
                "the rebuilt index must list every event in its directory: {rebuilt}"
            );
        }
    }

    #[test]
    fn a_capture_after_a_merge_records_the_merged_state() {
        // The end-to-end claim: after a transport has done its worst, a capture
        // still runs and still records a consistent cut.
        let one = seed("transport-after");
        let two = tempdir("transport-after-two");
        merge_into(&one, &two);
        capture(&one, "2026-07-31T09:00:00Z", None);
        capture(&two, "2026-07-31T09:00:00Z", None);
        merge_into(&two, &one);

        write(
            &one,
            "notes/a.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\npost-merge\n",
        );
        let outcome = capture(&one, "2026-07-31T10:00:00Z", Some("post-merge"));
        let Captured::Written { id, .. } = outcome else {
            panic!("a post-merge capture must write: {outcome:?}")
        };
        // Its parent is the newest event that existed locally — display metadata,
        // but it should still be recorded.
        let events = block_on(ws(&one).history_list(Path::new("index.md"))).unwrap();
        let latest = events.iter().find(|e| e.id == id).unwrap();
        assert!(latest.parent.is_some(), "a parent should be recorded");
        assert!(
            latest
                .files
                .iter()
                .any(|f| f.path == Path::new("notes/a.md")),
            "the merged state must be in the manifest"
        );
    }

    // ── Reading one event: `history-show` ────────────────────────────────────

    #[test]
    fn an_event_resolves_by_id_with_every_index_destroyed() {
        let dir = seed("show-resolve");
        let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", Some("pre-sync"))
        else {
            panic!("the first capture must write an event");
        };
        // The indexes are a cache. Burn all three; the id still resolves, because
        // its path is a pure function of it.
        for index in [
            "history/index.md",
            "history/events/2026/index.md",
            "history/events/2026/07/index.md",
        ] {
            std::fs::remove_file(dir.join(index)).unwrap();
        }
        let event = block_on(ws(&dir).history_event(Path::new("index.md"), &id))
            .unwrap()
            .expect("the event must resolve without any index");
        assert_eq!(event.id, id);
        assert_eq!(event.label.as_deref(), Some("pre-sync"));
        assert_eq!(event.files.len(), 4);

        // An id that names nothing is absence, not an error; a string that is not
        // an event id at all is an error.
        assert!(
            block_on(ws(&dir).history_event(Path::new("index.md"), "2026-07-31-0000-deadbeef"))
                .unwrap()
                .is_none()
        );
        assert!(block_on(ws(&dir).history_event(Path::new("index.md"), "yesterday")).is_err());
    }

    #[test]
    fn missing_blobs_name_the_paths_a_restore_could_not_recover() {
        let dir = seed("show-blobs");
        let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
            panic!("the first capture must write an event");
        };
        let event = block_on(ws(&dir).history_event(Path::new("index.md"), &id))
            .unwrap()
            .unwrap();
        assert!(
            block_on(ws(&dir).history_missing_blobs(Path::new("index.md"), &event))
                .unwrap()
                .is_empty(),
            "a capture parks every file's bytes"
        );

        // The half-synced case: the event document arrived, one blob did not.
        let payload = crate::fixity::digest(b"JPEGBYTES");
        let blob = blob_path(Path::new("history/index.md"), &payload).unwrap();
        std::fs::remove_file(dir.join(&blob)).unwrap();
        let missing =
            block_on(ws(&dir).history_missing_blobs(Path::new("index.md"), &event)).unwrap();
        assert_eq!(
            missing.into_iter().collect::<Vec<_>>(),
            vec![PathBuf::from("notes/photo.jpg")],
            "only the file whose bytes are gone should be reported"
        );

        // A row prov could never have parked reports as missing rather than
        // failing the read — a foreign event must stay legible.
        let foreign = Event {
            files: vec![FileEntry {
                path: PathBuf::from("notes/a.md"),
                id: None,
                hash: "blake3:beef".into(),
            }],
            ..event
        };
        assert_eq!(
            block_on(ws(&dir).history_missing_blobs(Path::new("index.md"), &foreign))
                .unwrap()
                .len(),
            1
        );
    }

    // ── Lineage: `history-log` ───────────────────────────────────────────────

    /// Re-point the root at `contents`, so a rename is visible to the reachable
    /// walk the capture set is taken from.
    fn relink(dir: &Path, contents: &[&str]) {
        let list = contents
            .iter()
            .map(|c| format!("- {c}\n"))
            .collect::<String>();
        write(
            dir,
            "index.md",
            &format!("---\ntitle: Home\ncontents:\n{list}---\nroot\n"),
        );
    }

    #[test]
    fn a_lineage_follows_an_id_through_a_rename_no_path_key_could() {
        let dir = seed("log-rename");
        let mut w = ws(&dir);
        let id = Id("b7k2m".into());
        w.index_mut().register(&id, Path::new("notes/a.md"));
        let take = |w: &mut Workspace<StdFs, Minter, FileIndex>, now: &str| {
            block_on(w.history_capture(Path::new("index.md"), now, None)).unwrap()
        };
        take(&mut w, "2026-07-31T09:00:00Z");

        // The move: same bytes, new path. A path-keyed store shows two unrelated
        // lineages here; the id column shows one document that moved.
        std::fs::rename(dir.join("notes/a.md"), dir.join("notes/b.md")).unwrap();
        relink(&dir, &["notes/b.md", "notes/photo.jpg.yaml"]);
        w.index_mut().set_path(&id, Path::new("notes/b.md"));
        take(&mut w, "2026-07-31T10:00:00Z");

        // An edit at the new path.
        write(
            &dir,
            "notes/b.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\nrevised\n",
        );
        take(&mut w, "2026-07-31T11:00:00Z");

        // …and a capture that changes nothing about this document, which must not
        // add a point to its lineage.
        write(&dir, "notes/photo.jpg", "OTHERBYTES");
        take(&mut w, "2026-07-31T12:00:00Z");

        let log = block_on(w.history_log(Path::new("index.md"), &Subject::Id(id.clone()))).unwrap();
        let paths: Vec<&Path> = log
            .iter()
            .map(|v| match &v.state {
                Presence::At { path, .. } => path.as_path(),
                Presence::Gone => Path::new("(gone)"),
            })
            .collect();
        assert_eq!(
            paths,
            vec![
                Path::new("notes/a.md"),
                Path::new("notes/b.md"),
                Path::new("notes/b.md")
            ],
            "the move must be a point in the lineage, and the untouched capture must not"
        );
        // Deduping on the hash alone would have swallowed the move: the bytes did
        // not change when the path did.
        let (Presence::At { hash: first, .. }, Presence::At { hash: second, .. }) =
            (&log[0].state, &log[1].state)
        else {
            panic!("both points should be present states");
        };
        assert_eq!(first, second, "a rename leaves the bytes identical");

        // The same document asked for by its old *path*: the lineage fragments at
        // the move, which is the nature of a path key. But the row it does find
        // still remembers the id — which is what lets the weaker query hand the
        // caller the stronger one instead of quietly under-reporting.
        let by_path = block_on(w.history_log(
            Path::new("index.md"),
            &Subject::Path(PathBuf::from("notes/a.md")),
        ))
        .unwrap();
        assert!(matches!(
            &by_path[0].state,
            Presence::At { id: Some(found), .. } if *found == id
        ));
        assert_eq!(
            by_path.last().unwrap().state,
            Presence::Gone,
            "a path-keyed lineage sees the move as the document disappearing"
        );
    }

    #[test]
    fn a_lineage_records_a_deletion_and_a_return() {
        let dir = seed("log-gone");
        let mut w = ws(&dir);
        let id = Id("b7k2m".into());
        w.index_mut().register(&id, Path::new("notes/a.md"));
        let take = |w: &mut Workspace<StdFs, Minter, FileIndex>, now: &str| {
            block_on(w.history_capture(Path::new("index.md"), now, None)).unwrap()
        };
        take(&mut w, "2026-07-31T09:00:00Z");

        // Out of the reachable graph and off disk.
        std::fs::remove_file(dir.join("notes/a.md")).unwrap();
        relink(&dir, &["notes/photo.jpg.yaml"]);
        take(&mut w, "2026-07-31T10:00:00Z");

        // Back again — which is what a restore looks like from the lineage's side.
        write(
            &dir,
            "notes/a.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n",
        );
        relink(&dir, &["notes/a.md", "notes/photo.jpg.yaml"]);
        take(&mut w, "2026-07-31T11:00:00Z");

        let log = block_on(w.history_log(Path::new("index.md"), &Subject::Id(id))).unwrap();
        assert_eq!(log.len(), 3);
        assert!(matches!(log[0].state, Presence::At { .. }));
        // Omission *is* deletion: there is no removal list to have consulted.
        assert_eq!(log[1].state, Presence::Gone);
        assert!(matches!(log[2].state, Presence::At { .. }));
        assert_eq!(log[2].created, "2026-07-31T11:00:00Z");
    }

    #[test]
    fn an_id_less_document_still_has_a_lineage_by_path() {
        // The documents with no id — the config document, the registry, the bin
        // index, an attachment payload — are disproportionately what a sync
        // transport damages, so the weaker key has to work.
        let dir = seed("log-path");
        capture(&dir, "2026-07-31T09:00:00Z", None);
        write(&dir, "notes/photo.jpg", "OTHERBYTES");
        capture(&dir, "2026-07-31T10:00:00Z", None);

        let log = block_on(ws(&dir).history_log(
            Path::new("index.md"),
            &Subject::Path(PathBuf::from("notes/photo.jpg")),
        ))
        .unwrap();
        assert_eq!(log.len(), 2, "the payload's bytes changed once");
        let Presence::At { hash, .. } = &log[1].state else {
            panic!("the payload should be present in the second event");
        };
        assert_eq!(*hash, crate::fixity::digest(b"OTHERBYTES"));

        // A subject no event ever captured has an empty lineage, not an error.
        assert!(
            block_on(ws(&dir).history_log(
                Path::new("index.md"),
                &Subject::Path(PathBuf::from("notes/never.md")),
            ))
            .unwrap()
            .is_empty()
        );
    }
}
