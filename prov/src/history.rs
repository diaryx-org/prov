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
use crate::index::{Collision, IndexStore};
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

/// The file stem of the store's tombstone list, beside the store index. A
/// **whole-file** record store (`forgotten.yaml`, `.json`, `.fig`), because it is
/// a mutable record store prov edits in place — the `MalformedStore` rule the
/// registry and the bin index live under, and which an immutable event document
/// deliberately does not.
pub const FORGOTTEN_STEM: &str = "forgotten";

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

/// What a restore acts on — the whole consistent cut, or a slice of it.
///
/// The two are not equals, and the CLI help has to say so. An event is a
/// **consistent cut**: if a bad merge corrupted a renamed file *and* its parent's
/// child list, both were hashed in the same capture, so restoring
/// [`Whole`](Scope::Whole) puts the set back together — which is what actually
/// undoes the damage. A scope is a **content-recovery** tool: right when a sync
/// clobbered one file's prose, wrong when the graph broke, because writing one
/// file's old bytes back without the rest of the same corruption's footprint can
/// *reintroduce* the inconsistency history exists to fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Every row in the manifest. The only scope
    /// [`exact`](Workspace::history_restore_plan) accepts.
    Whole,
    /// Only the rows at — or under — these paths, so naming a directory restores
    /// the subtree the capture held beneath it.
    Paths(Vec<PathBuf>),
    /// Only the row carrying this id, wherever the capture found it. Rename-robust
    /// in the way [`Subject::Id`] is, and the way to reach a document whose path
    /// has since changed.
    Id(Id),
}

/// What a restore will do to one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Nothing is at that path; the captured bytes are written there.
    Create,
    /// Something else is at that path; the captured bytes replace it.
    Overwrite,
    /// The captured bytes are **already** there — nothing is written. Reported
    /// rather than dropped, because "the restore did nothing to this file" is the
    /// answer to a question a user restoring after a bad merge is actually asking.
    Unchanged,
    /// The manifest names a hash with no blob behind it, so there are no bytes to
    /// write. Ordinary rather than broken: an event document and the blobs it
    /// names travel over a transport independently, and a small document
    /// routinely lands well before the bytes. Skipped, and reported by name.
    NoBytes,
    /// Reachable now, absent from the manifest — removed only under `exact`.
    Remove,
}

impl Disposition {
    /// Sort order for a plan: what is written, then what was already right, then
    /// what cannot be, then what goes away. Read top to bottom, a plan reads as a
    /// sentence about the restore.
    fn rank(self) -> u8 {
        match self {
            Disposition::Create => 0,
            Disposition::Overwrite => 1,
            Disposition::Unchanged => 2,
            Disposition::NoBytes => 3,
            Disposition::Remove => 4,
        }
    }
}

/// One path a restore touches, and what it will do to it — the unit a
/// [`RestorePlan`] is a sequence of, on the same footing as the
/// [`FileOp`](crate::change::FileOp)s it becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreOp {
    /// The workspace-relative path.
    pub path: PathBuf,
    /// What happens to it.
    pub disposition: Disposition,
    /// The captured digest behind this op, or `None` for a
    /// [`Remove`](Disposition::Remove) — a removal comes from a path the manifest
    /// *lacks*, so there is no row and no hash behind it.
    pub hash: Option<String>,
    /// The id the manifest recorded for this path, when it recorded one.
    pub id: Option<Id>,
}

/// A registration a restore would displace, and the path whose restoration would
/// displace it.
///
/// Refused rather than resolved: two documents claim one id and only their author
/// knows which should keep it. See
/// [`registration_conflict`](Workspace::registration_conflict).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    /// The path being restored.
    pub path: PathBuf,
    /// What restoring it would displace.
    pub collision: Collision,
}

/// Everything a restore would do, computed before a byte moves.
///
/// A snapshot, not a promise: it compares the manifest against the tree as it was
/// when the plan was built, so build it, show it, and hand *that* plan to
/// [`history_restore`](Workspace::history_restore) rather than recomputing — a
/// user who confirmed a removal list is entitled to have that list be the one
/// that runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePlan {
    /// The event being restored.
    pub event: String,
    /// Every path the restore touches, sorted by [`Disposition`] then path.
    pub ops: Vec<RestoreOp>,
    /// Registrations the restore would displace. Non-empty means it refuses
    /// without `force`.
    pub conflicts: Vec<Conflict>,
}

impl RestorePlan {
    /// How many ops carry `disposition`.
    pub fn count(&self, disposition: Disposition) -> usize {
        self.ops
            .iter()
            .filter(|op| op.disposition == disposition)
            .count()
    }

    /// The paths this restore would remove — the list to show before asking.
    pub fn removals(&self) -> impl Iterator<Item = &Path> {
        self.ops
            .iter()
            .filter(|op| op.disposition == Disposition::Remove)
            .map(|op| op.path.as_path())
    }

    /// Whether the restore would write and remove nothing — the workspace already
    /// holds this capture (or holds nothing this capture can supply).
    pub fn is_noop(&self) -> bool {
        !self.ops.iter().any(|op| {
            matches!(
                op.disposition,
                Disposition::Create | Disposition::Overwrite | Disposition::Remove
            )
        })
    }
}

/// How much history a prune keeps. There is no default: an operation that
/// deletes bytes should not do so because a flag was forgotten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Retention {
    /// Keep the newest `n` events and drop everything older. The count axis:
    /// "however far back that reaches, keep this many recovery points."
    Keep(usize),
    /// Drop every event captured strictly before this instant. The age axis, and
    /// the natural way to say "everything from before the migration".
    ///
    /// A date (`2026-06-01`) or a full timestamp; both compare correctly, because
    /// a date is a *prefix* of every timestamp in that day, so an event on the
    /// named day is not before it.
    Before(String),
}

/// What a prune would drop — computed before anything is deleted.
///
/// A snapshot, like [`RestorePlan`]: build it, show it, and hand *that* to
/// [`history_prune`](Workspace::history_prune), so what runs is what the user
/// was asked about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pruned {
    /// The events to drop, oldest first. Ids, which resolve to their documents by
    /// the pure id → path function.
    pub events: Vec<String>,
    /// The blob files to collect: everything under `blobs/` that no surviving
    /// manifest names, workspace-relative and sorted.
    ///
    /// This is the same sweep [`Finding::HistoryBlobOrphaned`] reports, taken
    /// against the survivors — so a prune also collects orphans that were already
    /// there, which is exactly what that finding promises.
    pub blobs: Vec<PathBuf>,
    /// What those blobs occupy on disk.
    pub bytes: u64,
    /// How many events survive.
    pub keeping: usize,
}

impl Pruned {
    /// Whether the prune would delete nothing at all.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty() && self.blobs.is_empty()
    }
}

/// What a [`history_forget`](Workspace::history_forget) destroyed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Forgotten {
    /// The hashes whose bytes were destroyed and tombstoned.
    pub hashes: Vec<String>,
    /// The blob files deleted, workspace-relative and sorted.
    pub blobs: Vec<PathBuf>,
    /// What those blobs occupied on disk.
    pub bytes: u64,
    /// Hashes the subject named that **survive**, because some other captured
    /// path names the same bytes. Content addressing means forgetting one
    /// document cannot reach into another's history, and a report that stayed
    /// quiet about it would overstate what was destroyed.
    pub shared: Vec<String>,
}

impl Forgotten {
    /// Whether nothing was destroyed.
    pub fn is_empty(&self) -> bool {
        self.hashes.is_empty()
    }
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

/// How many fractional digits a `created` written by this version carries. Fixed,
/// never trimmed — see [`comparable`].
const FRACTION_DIGITS: usize = 6;

/// A `created` value in the form two events can be **ordered** by, whatever
/// precision each was written at.
///
/// A store outlives any one version of prov, and event documents are immutable,
/// so a store holds second-granularity timestamps (everything written before
/// microsecond precision existed) alongside sub-second ones — permanently, and
/// interleaved by sync rather than neatly separated by date. Comparing those as
/// raw strings is wrong in precisely the case that matters: `Z` (0x5A) sorts
/// after `.` (0x2E), so `…10Z` would order *after* `…10.500000Z` inside the same
/// second, inverting the two events a finer clock was introduced to tell apart.
///
/// Padding the fraction to a fixed width restores the total order without parsing
/// a calendar and without rewriting a single event — which matters, because
/// rewriting one is the operation this format does not have.
///
/// A stamp not in `…Z` form is returned untouched. prov only ever writes `Z`, and
/// an offset form is already outside the order a string comparison can give;
/// mangling it here would only hide that.
fn comparable(created: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    let Some(rest) = created.strip_suffix('Z') else {
        return Cow::Borrowed(created);
    };
    let (whole, fraction) = match rest.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (rest, ""),
    };
    if fraction.len() == FRACTION_DIGITS {
        return Cow::Borrowed(created);
    }
    let mut padded = fraction.to_string();
    padded.truncate(FRACTION_DIGITS);
    while padded.len() < FRACTION_DIGITS {
        padded.push('0');
    }
    Cow::Owned(format!("{whole}.{padded}Z"))
}

/// Reject a [`Retention::Before`] cutoff that is not a date, so a typo deletes
/// nothing rather than everything.
///
/// Only the `YYYY-MM-DD` head is checked. Anything after it is compared as text
/// against a normalized `created` ([`comparable`]), where a bare date is a prefix
/// of every timestamp in its day — which is what makes "before 2026-06-01" mean
/// "before that day started" without parsing a calendar.
fn check_cutoff(cutoff: &str) -> Result<()> {
    let ok = cutoff.len() >= 10
        && cutoff.as_bytes()[4] == b'-'
        && cutoff.as_bytes()[7] == b'-'
        && cutoff
            .bytes()
            .take(10)
            .enumerate()
            .all(|(i, b)| matches!(i, 4 | 7) || b.is_ascii_digit());
    match ok {
        true => Ok(()),
        false => Err(Error::Structure(format!(
            "`{cutoff}` is not a date — expected YYYY-MM-DD, or a full RFC 3339 timestamp"
        ))),
    }
}

/// `YYYY-MM-DD-HHMM` from an RFC 3339 UTC timestamp — the human-readable head of
/// an event id. Full precision stays in the document's `created`.
///
/// Reads only the calendar head, so a fractional-second suffix passes through
/// untouched: event ids stay minute-granular (§4 — they are for humans), and the
/// eight-hex content digest is what tells two captures in one minute apart.
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
    /// - **The generated `about.md`.** It is *derived* — a pure function of the
    ///   configuration, which this same manifest captures — so parking its bytes
    ///   stores nothing that cannot be reproduced, and a new blob would be parked
    ///   on every config change for no recovery value. Restoring an event
    ///   restores the config that determines the page, and `check` reports the
    ///   page as stale until `prov about` rewrites it from that config, which is
    ///   the same repair by a shorter route. Excluding it also removes an
    ///   ordering hazard: the first capture *bootstraps* the store, which changes
    ///   what the page says about this workspace, so a captured page would be one
    ///   the capture itself invalidated.
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
        let about = self.about_path(root_doc).await?;
        Ok(self
            .reachable_files(root_doc)
            .await?
            .into_iter()
            .filter(|p| !under(p, &store))
            .filter(|p| binned.as_ref().is_none_or(|items| !under(p, items)))
            .filter(|p| about.as_ref().is_none_or(|about| p != about))
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
        self.history_events_in(&store_index, self.history_ext(root_doc))
            .await
    }

    /// [`history_list`](Self::history_list) against a store index already in hand —
    /// so a pass that has resolved the store once does not resolve it again
    /// through the root.
    async fn history_events_in(&self, store_index: &Path, ext: &str) -> Result<Vec<Event>> {
        let events_root = store_dir(store_index).join(EVENTS_DIR);
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
        // Normalized, not raw: a store mixes the precisions of every version that
        // ever wrote into it (see [`comparable`]). The id tiebreak survives for
        // the genuine tie — two devices landing on the same microsecond — where it
        // is arbitrary but deterministic, which is all an ordering owes a fork.
        events.sort_by(|a, b| {
            comparable(&a.created)
                .cmp(&comparable(&b.created))
                .then_with(|| a.id.cmp(&b.id))
        });
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

        let mut months = self.event_months(&events_root.join(&year), ext).await?;
        months.insert(month.clone());
        cs.write(
            events_root.join(&year).join(format!("index.{ext}")),
            render_year_index(&year, &months, ext, embed)?,
        );

        let mut years = self.event_years(&events_root, ext).await?;
        years.insert(year.clone());
        let forgotten = self.history_forgotten_link(store_index).await?;
        cs.write(
            store_index,
            render_store_index(&years, ext, forgotten.as_deref(), embed)?,
        );
        Ok(())
    }

    /// What restoring `event` would do, computed **before a byte moves** — the
    /// dry run, the confirmation prompt's removal list, and the plan
    /// [`history_restore`](Self::history_restore) executes, all one value.
    ///
    /// Everything here falls out of comparing the manifest against disk: no graph
    /// walk, no projected tree. True pre-flight *validation* — checking what the
    /// restored graph would look like before writing it — is a general `--dry-run`
    /// capability for every mutation, not this one verb's private machinery; what
    /// stands in for it is that a restore runs `check` before and after and reports
    /// the difference.
    ///
    /// ## What the plan decides
    ///
    /// - **Per row: create, overwrite, unchanged, or no bytes.** A row whose blob
    ///   is absent is skipped rather than fatal — a manifest and its blobs sync
    ///   independently. A row whose bytes are *already* on disk is skipped too, so
    ///   restoring a capture the workspace already matches writes nothing at all.
    /// - **Under `exact`, what to remove**: the capture set (`history/` and the
    ///   recycle bin's items already excluded, by construction) minus the paths the
    ///   manifest holds. The honest "undo this merge entirely" tool — bad-merge
    ///   damage is characteristically *additive* (a `.sync-conflict` copy, a
    ///   rename-vs-rename landing both names), and none of it goes away by writing
    ///   captured bytes over the top. The same pass discards legitimate work done
    ///   since the capture, which is why it is opt-in and why the caller is
    ///   expected to show [`removals`](RestorePlan::removals) before running it.
    ///
    ///   **Reachable** is the operative word, and it bounds the promise: a file
    ///   nothing links is not in the capture set, so `exact` leaves it exactly
    ///   where it is and `check` reports it as an [`Orphan`](Finding::Orphan). A
    ///   restore puts a captured graph back; deciding that some unreferenced file
    ///   in a directory is rubble is not a call it gets to make. Note the timing
    ///   this implies — the plan is taken against the tree as it stands, so a
    ///   file the *restored* root would stop linking is still reachable when the
    ///   delete set is computed, and is removed.
    /// - **Which registrations it would displace.** `id_storage` defaults to
    ///   `both`, so a restored document's frontmatter carries an id the live
    ///   registry may bind elsewhere — and the target path can be free while the id
    ///   is taken, or the other way round. Both directions, via
    ///   [`registration_conflict`](Workspace::registration_conflict).
    ///
    ///   A collision the restore **itself resolves** is not reported: if the
    ///   document currently holding the id is one this restore overwrites or (under
    ///   `exact`) removes, nothing is displaced. That is what lets `--exact` undo a
    ///   move without `--force`, while an *additive* restore of the same event —
    ///   which would put the old path back and leave the new one there, two
    ///   documents spelling one id — still refuses.
    ///
    /// `exact` is rejected outright with a scope. It means "make the tree match
    /// this capture", which a slice of the capture cannot say.
    pub async fn history_restore_plan(
        &self,
        root_doc: &Path,
        event: &Event,
        scope: &Scope,
        exact: bool,
    ) -> Result<RestorePlan> {
        let root_doc = link::normalize(root_doc);
        let (store_index, _) = self.history_store_index(&root_doc).await?;

        if exact && *scope != Scope::Whole {
            return Err(Error::Structure(
                "`exact` removes every reachable file the capture does not contain, \
                 which is a statement about the whole tree — it cannot be scoped to \
                 part of one"
                    .into(),
            ));
        }

        // The rows this restore is about. `Whole` is the consistent cut; the other
        // two are content recovery, and each names something that has to be *in*
        // the manifest — a scope that selects nothing is a typo, not an empty
        // restore.
        let selected: Vec<&FileEntry> = match scope {
            Scope::Whole => event.files.iter().collect(),
            Scope::Paths(paths) => {
                let mut rows: Vec<&FileEntry> = Vec::new();
                for want in paths {
                    let want = link::normalize(want);
                    let matched = event.files.iter().filter(|f| under(&f.path, &want));
                    let before = rows.len();
                    rows.extend(matched);
                    if rows.len() == before {
                        return Err(Error::Structure(format!(
                            "{} is not in {} — `prov history-show {}` lists what it captured",
                            want.display(),
                            event.id,
                            event.id
                        )));
                    }
                }
                rows.sort_by(|a, b| a.path.cmp(&b.path));
                rows.dedup_by(|a, b| a.path == b.path);
                rows
            }
            Scope::Id(id) => {
                let rows: Vec<&FileEntry> = event
                    .files
                    .iter()
                    .filter(|f| f.id.as_ref() == Some(id))
                    .collect();
                if rows.is_empty() {
                    return Err(Error::Structure(format!(
                        "{} is not in {} — that capture recorded no document with that id",
                        id, event.id
                    )));
                }
                rows
            }
        };

        let mut ops = Vec::new();
        for file in selected {
            // Presence of the bytes first: a row prov cannot supply has no
            // disposition worth computing, and there is nothing to read.
            let parked = match blob_path(&store_index, &file.hash) {
                Ok(blob) => self.fs().try_exists(&self.root().join(blob)).await?,
                // A hash prov could not have parked names no blob that could be
                // found — missing, rather than fatal to the whole plan.
                Err(_) => false,
            };
            let live = self.root().join(&file.path);
            let disposition = if !parked {
                Disposition::NoBytes
            } else if !self.fs().try_exists(&live).await? {
                Disposition::Create
            } else if crate::fixity::digest(&self.fs().read(&live).await?) == file.hash {
                Disposition::Unchanged
            } else {
                Disposition::Overwrite
            };
            ops.push(RestoreOp {
                path: file.path.clone(),
                disposition,
                hash: Some(file.hash.clone()),
                id: file.id.clone(),
            });
        }

        if exact {
            let captured: BTreeSet<&Path> = event.files.iter().map(|f| f.path.as_path()).collect();
            for path in self.history_capture_set(&root_doc).await? {
                // The root document is never removed. A capture always holds it
                // (it is how the walk started), so this only fires for a manifest
                // that is not one — and a tree with no root is not a restored
                // workspace, it is rubble.
                if captured.contains(path.as_path()) || path == root_doc {
                    continue;
                }
                ops.push(RestoreOp {
                    path,
                    disposition: Disposition::Remove,
                    hash: None,
                    id: None,
                });
            }
        }

        // A collision only counts if it survives the restore, so the two sets the
        // restore *resolves* are needed before judging any of them.
        let written: BTreeSet<&Path> = ops
            .iter()
            .filter(|op| matches!(op.disposition, Disposition::Create | Disposition::Overwrite))
            .map(|op| op.path.as_path())
            .collect();
        let removed: BTreeSet<&Path> = ops
            .iter()
            .filter(|op| op.disposition == Disposition::Remove)
            .map(|op| op.path.as_path())
            .collect();
        let mut conflicts = Vec::new();
        for op in &ops {
            // Only a row actually being written can displace anything: an
            // `Unchanged` path already holds these bytes, and a `NoBytes` one is
            // not touched at all.
            if !matches!(op.disposition, Disposition::Create | Disposition::Overwrite) {
                continue;
            }
            let Some(id) = &op.id else { continue };
            let Some(collision) = self.registration_conflict(id, &op.path) else {
                continue;
            };
            let resolved = match &collision {
                // The id is registered elsewhere — harmless if "elsewhere" is a
                // path this restore is about to overwrite with captured content or
                // remove outright.
                Collision::Id { held_by, .. } => {
                    written.contains(held_by.as_path()) || removed.contains(held_by.as_path())
                }
                // The path is registered to a *different* id: whatever document is
                // there now, this would write over it and leave that id resolving
                // to bytes that no longer spell it. Nothing in the restore fixes
                // that.
                Collision::Path { .. } => false,
            };
            if !resolved {
                conflicts.push(Conflict {
                    path: op.path.clone(),
                    collision,
                });
            }
        }

        ops.sort_by(|a, b| {
            a.disposition
                .rank()
                .cmp(&b.disposition.rank())
                .then_with(|| a.path.cmp(&b.path))
        });
        Ok(RestorePlan {
            event: event.id.clone(),
            ops,
            conflicts,
        })
    }

    /// Execute a [`RestorePlan`]: write the captured bytes back, and — under a
    /// plan built with `exact` — remove what the capture did not hold.
    ///
    /// Takes the plan rather than recomputing it, so what runs is exactly what the
    /// caller showed and the user agreed to.
    ///
    /// ## What it never touches
    ///
    /// - **`history/` itself.** No manifest row can name a path inside the store —
    ///   the capture set is blind to it by construction — and the removal pass is
    ///   drawn from that same set, so neither half of a restore can reach in. An
    ///   `exact` restore of an old event deleting every event newer than it is the
    ///   failure this rules out.
    /// - **The root's `history` pointer.** A captured root predating the store (or
    ///   hand-edited since) must not strand it unreachable, so a restored root that
    ///   declares no pointer gets one before it is written. Present-but-different is
    ///   left alone: that is the capture's truth about where the store lived.
    /// - **The registry, as a data structure.** The registry *document* is an
    ///   ordinary captured file and comes back with the rest; nothing here edits
    ///   the in-memory index, which is why a caller must re-open the workspace
    ///   before reading it again.
    ///
    /// ## Why the bytes ride a `CopyFrom`
    ///
    /// The journal embeds file contents ([`crate::journal::encode`]), so staging a
    /// whole restored workspace as [`ChangeSet::write`] would duplicate the entire
    /// tree into `.prov-journal` at the commit point.
    /// [`FileOp::CopyFrom`](crate::change::FileOp::CopyFrom) journals the *source
    /// path* instead, and a history blob is exactly the immutable, content-addressed
    /// referent that makes replaying such a reference deterministic: the path is
    /// the digest of the contents, so the bytes found there are the bytes intended,
    /// or the file is gone and replay fails loudly.
    pub async fn history_restore(
        &mut self,
        root_doc: &Path,
        plan: &RestorePlan,
        force: bool,
    ) -> Result<()> {
        let root_doc = link::normalize(root_doc);
        let (store_index, _) = self.history_store_index(&root_doc).await?;
        if let Some(conflict) = plan.conflicts.first()
            && !force
        {
            return Err(conflict.collision.clone().into());
        }

        // Sorted by disposition, so writes are staged before removals — the order a
        // half-applied set should fail in, and the order the plan was read in.
        let mut cs = self.change();
        for op in &plan.ops {
            match op.disposition {
                Disposition::Create | Disposition::Overwrite => {
                    let hash = op.hash.as_deref().ok_or_else(|| {
                        Error::Structure(format!(
                            "{} has no captured digest to restore from",
                            op.path.display()
                        ))
                    })?;
                    let blob = blob_path(&store_index, hash)?;
                    match op.path == root_doc {
                        true => {
                            let bytes = self.fs().read(&self.root().join(&blob)).await?;
                            let text = String::from_utf8(bytes).map_err(|e| {
                                Error::Structure(format!(
                                    "the captured {} is not valid UTF-8: {e}",
                                    root_doc.display()
                                ))
                            })?;
                            cs.write(
                                &root_doc,
                                self.rooted_at_store(&root_doc, &text, &store_index)?,
                            );
                        }
                        false => {
                            cs.copy_from(&op.path, blob);
                        }
                    }
                }
                Disposition::Remove => {
                    cs.remove(&op.path);
                }
                Disposition::Unchanged | Disposition::NoBytes => {}
            }
        }
        self.commit(cs).await
    }

    /// What pruning to `retention` would drop: the events, and the blobs no
    /// surviving manifest would name.
    ///
    /// Read-only. With full manifests this is delete + GC and nothing else — no
    /// folding, no re-anchoring, no rewriting of surviving events, which under the
    /// delta design was the hardest problem in the store (a dropped event's
    /// entries could be load-bearing for later events' effective state, so pruning
    /// had to rewrite an "immutable" event, the one operation that conflicts under
    /// exactly the sync this store exists to survive).
    ///
    /// The blob sweep is [`Finding::HistoryBlobOrphaned`]'s, taken against the
    /// survivors rather than against every event — so what `check` calls an orphan
    /// and what a prune collects are the same set by construction, and a prune
    /// sweeps up the orphans that were already there.
    pub async fn history_prune_plan(
        &self,
        root_doc: &Path,
        retention: &Retention,
    ) -> Result<Pruned> {
        let root_doc = link::normalize(root_doc);
        let (store_index, exists) = self.history_store_index(&root_doc).await?;
        if !exists {
            return Ok(Pruned::default());
        }
        let events = self
            .history_events_in(&store_index, self.history_ext(&root_doc))
            .await?;

        // Events arrive oldest first, so both axes cut a prefix — but `Before`
        // states its own predicate rather than trusting that, since a store that
        // mixes timestamp precisions is exactly where an assumed sort order goes
        // wrong quietly.
        let (dropped, kept): (Vec<&Event>, Vec<&Event>) = match retention {
            Retention::Keep(n) => {
                let cut = events.len().saturating_sub(*n);
                (
                    events[..cut].iter().collect(),
                    events[cut..].iter().collect(),
                )
            }
            Retention::Before(cutoff) => {
                check_cutoff(cutoff)?;
                events
                    .iter()
                    .partition(|event| comparable(&event.created) < comparable(cutoff))
            }
        };

        let referenced: BTreeSet<PathBuf> = kept
            .iter()
            .flat_map(|event| event.files.iter())
            .filter_map(|file| blob_path(&store_index, &file.hash).ok())
            .collect();
        let mut blobs = Vec::new();
        let mut bytes = 0u64;
        for blob in self.history_blob_files(&store_index).await? {
            if referenced.contains(&blob) {
                continue;
            }
            // A size that cannot be read is not worth failing a prune over; the
            // total is a report, not a decision.
            bytes += match self.fs().metadata(&self.root().join(&blob)).await {
                Ok(meta) => meta.len(),
                Err(_) => 0,
            };
            blobs.push(blob);
        }

        Ok(Pruned {
            events: dropped.iter().map(|event| event.id.clone()).collect(),
            blobs,
            bytes,
            keeping: kept.len(),
        })
    }

    /// Execute a [`Pruned`] plan: drop the events, rebuild the indexes the drop
    /// changed, then collect the blobs.
    ///
    /// **In that order, and the order is the safety argument.** Events first means
    /// a crash mid-prune leaves blobs no manifest references — a
    /// [`Finding::HistoryBlobOrphaned`], which the next prune collects. Blobs
    /// first would leave surviving manifests naming bytes that are gone, which is
    /// real loss. The benign residue is the one prov already tolerates from
    /// capture, in the opposite direction.
    ///
    /// **Blobs do not ride the change set**, mirroring capture. There the reason
    /// is that the journal embeds contents; here it is that
    /// [`ChangeSet::remove`] buffers the bytes it deletes so it can put them
    /// back, and a GC that frees a gigabyte would hold a gigabyte in memory to do
    /// it. Deleting content-addressed bytes directly is safe for the same reason
    /// writing them is: the operation is idempotent, and a half-finished one is an
    /// orphan rather than a corruption.
    ///
    /// A surviving index is rewritten only when its content would actually change.
    /// Every index this touches is a file some transport has to carry, and a prune
    /// that rewrote five years of untouched shards would be five years of
    /// needless merge surface.
    pub async fn history_prune(&mut self, root_doc: &Path, plan: &Pruned) -> Result<()> {
        let root_doc = link::normalize(root_doc);
        let (store_index, exists) = self.history_store_index(&root_doc).await?;
        if !exists || plan.is_empty() {
            return Ok(());
        }
        let ext = self.history_ext(&root_doc);
        let embed = self.history_embed()?;
        let dropped: BTreeSet<&str> = plan.events.iter().map(String::as_str).collect();
        let events_root = store_dir(&store_index).join(EVENTS_DIR);

        let mut cs = self.change();
        for id in &plan.events {
            cs.remove(event_path(&store_index, id, ext)?);
        }

        // Rebuilt from the directory listing minus what this prune drops — the
        // same "an index is a pure function of its directory" rule capture and the
        // autofix follow, evaluated against the tree the prune is about to leave.
        let mut surviving_years = BTreeSet::new();
        for year in self.subdirs(&events_root).await? {
            let year_dir = events_root.join(&year);
            let mut surviving_months = BTreeSet::new();
            for month in self.subdirs(&year_dir).await? {
                let shard = year_dir.join(&month);
                let ids: BTreeSet<String> = self
                    .shard_event_ids(&shard, ext)
                    .await?
                    .into_iter()
                    .filter(|id| !dropped.contains(id.as_str()))
                    .collect();
                let index = shard.join(format!("index.{ext}"));
                if ids.is_empty() {
                    self.stage_index_removal(&mut cs, &index).await?;
                    continue;
                }
                surviving_months.insert(month.clone());
                self.stage_index_text(
                    &mut cs,
                    &index,
                    render_month_index(&year, &month, &ids, ext, embed)?,
                )
                .await?;
            }
            let index = year_dir.join(format!("index.{ext}"));
            if surviving_months.is_empty() {
                self.stage_index_removal(&mut cs, &index).await?;
                continue;
            }
            surviving_years.insert(year.clone());
            self.stage_index_text(
                &mut cs,
                &index,
                render_year_index(&year, &surviving_months, ext, embed)?,
            )
            .await?;
        }
        // The store index always survives: it is the root's pointer target, and a
        // store pruned to nothing is still a store — and it keeps linking the
        // tombstone list, which a prune never touches: those bytes are already
        // gone, and the record of that is not garbage.
        let forgotten = self.history_forgotten_link(&store_index).await?;
        self.stage_index_text(
            &mut cs,
            &store_index,
            render_store_index(&surviving_years, ext, forgotten.as_deref(), embed)?,
        )
        .await?;
        self.commit(cs).await?;

        for blob in &plan.blobs {
            let full = self.root().join(blob);
            // Tolerant of an already-absent blob: this runs after the commit, so a
            // re-run of an interrupted prune must be able to finish rather than
            // fail on the bytes the first run already freed.
            if self.fs().try_exists(&full).await? {
                self.fs().remove_file(&full).await?;
            }
        }
        Ok(())
    }

    /// Where the store's tombstone list lives, and whether it is there.
    ///
    /// Located by **stem**, not by the workspace's current metadata format: a
    /// workspace that switched formats after a forget must not lose track of what
    /// it destroyed, and a record of destruction is the last thing that should go
    /// quiet because a setting changed.
    async fn history_forgotten_path(&self, store_index: &Path) -> Result<(PathBuf, bool)> {
        let dir = store_dir(store_index);
        if let Ok(entries) = self.fs().read_dir(&self.root().join(&dir)).await {
            for entry in entries {
                let Some(name) = entry.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if entry.file_type().is_file()
                    && Path::new(name).file_stem().and_then(|s| s.to_str()) == Some(FORGOTTEN_STEM)
                {
                    return Ok((dir.join(name), true));
                }
            }
        }
        let ext = crate::document::whole_file_extension(self.default_embed_format());
        Ok((dir.join(format!("{FORGOTTEN_STEM}.{ext}")), false))
    }

    /// The tombstone list's path when the store has one — what a store index has
    /// to link so the record of what was destroyed is not itself an orphan.
    async fn history_forgotten_link(&self, store_index: &Path) -> Result<Option<PathBuf>> {
        let (path, present) = self.history_forgotten_path(store_index).await?;
        Ok(present.then_some(path))
    }

    /// The hashes this store has deliberately destroyed.
    ///
    /// The tombstone is what turns "these bytes are missing" into "these bytes are
    /// accounted for": [`Finding::HistoryBlobMissing`] skips a hash on this list,
    /// and the read verbs label its rows *forgotten* rather than lost. Events stay
    /// immutable — nothing rewrites a manifest — so the record of **what was
    /// captured** survives the destruction of the bytes, which is the honest
    /// bargain and has to be stated as one.
    ///
    /// Empty when there is no store, or nothing has been forgotten.
    pub async fn history_forgotten(&self, root_doc: &Path) -> Result<BTreeSet<String>> {
        let (store_index, exists) = self.history_store_index(root_doc).await?;
        if !exists {
            return Ok(BTreeSet::new());
        }
        let (path, present) = self.history_forgotten_path(&store_index).await?;
        if !present {
            return Ok(BTreeSet::new());
        }
        let Ok((_, doc)) = self.load(&path).await else {
            return Ok(BTreeSet::new());
        };
        Ok(forgotten_hashes(&doc.meta))
    }

    /// Destroy the captured bytes of one document, and record that it was
    /// deliberate.
    ///
    /// The counterpart to the retention this store creates. A document's bytes
    /// normally end at `empty_bin` or `rm --purge`; with history on, any event
    /// that captured it while it was live still holds them, and `history-restore`
    /// brings them back. This is the tool that makes that reversible act
    /// irreversible on purpose, and the full-manifest design is what makes it
    /// tractable: every hash a document ever had is a column lookup, not a fold.
    ///
    /// ## What it destroys, and what it cannot
    ///
    /// - **Only bytes nothing else names.** A hash the subject shares with another
    ///   captured path survives, and is reported in
    ///   [`shared`](Forgotten::shared). Content addressing means forgetting one
    ///   document cannot reach into another's history — which is a safety property
    ///   and a limit in the same breath.
    /// - **Bytes, not the record.** Event documents are immutable, so every
    ///   manifest still names the path, the id and the hash. If what must
    ///   disappear is the *name*, this is not that tool, and no amount of wording
    ///   should let a user believe otherwise.
    ///
    /// ## Why it refuses a live document
    ///
    /// Forgetting the captured bytes of a document still in the workspace is very
    /// nearly a no-op: the next capture parks them again. `force` proceeds anyway,
    /// for the deliberate "purge the history, keep the file" case.
    ///
    /// ## Ordering
    ///
    /// The tombstone is written and committed **before** the bytes are freed —
    /// write-ahead, like every other mutation here. `now` is the caller's
    /// timestamp, since the library keeps no clock.
    ///
    /// Blobs are deleted outside the change set for
    /// [`history_prune`](Self::history_prune)'s reason: a staged removal buffers
    /// the bytes it deletes in order to be able to put them back, which is the one
    /// thing a destruction verb must not do.
    ///
    /// A crash between the two leaves a hash tombstoned whose blob is still
    /// present. Re-running the same forget finishes the job. It is the one residue
    /// this ordering can leave, and it is the quiet one — which is the tradeoff
    /// write-ahead always makes, and worth knowing rather than worth reversing:
    /// destroying bytes before recording the intent would be the alternative.
    pub async fn history_forget(
        &mut self,
        root_doc: &Path,
        subject: &Subject,
        now: &str,
        force: bool,
    ) -> Result<Forgotten> {
        let root_doc = link::normalize(root_doc);
        let (store_index, exists) = self.history_store_index(&root_doc).await?;
        if !exists {
            return Ok(Forgotten::default());
        }
        let ext = self.history_ext(&root_doc);
        let embed = self.history_embed()?;

        // The next capture would park them again, so this would be theatre. Named
        // rather than merely refused: the user has to know *which* document, and
        // what to do about it.
        if !force && let Some(live) = self.history_subject_live(&root_doc, subject).await? {
            return Err(Error::Structure(format!(
                "{} is still in the workspace — the next capture would park its bytes \
                 again. Remove it first (`prov rm --purge`), or force this to forget \
                 the captured copies only",
                live.display()
            )));
        }

        // Every hash the subject ever had, and every hash anything *else* ever
        // had. The difference is what can go — a set subtraction, where a delta
        // log would need the ancestry folded per event to answer the same
        // question.
        let (mut mine, mut others) = (BTreeSet::new(), BTreeSet::new());
        for event in self.history_events_in(&store_index, ext).await? {
            for file in event.files {
                match subject_matches(subject, &file) {
                    true => mine.insert(file.hash),
                    false => others.insert(file.hash),
                };
            }
        }
        let shared: Vec<String> = mine.intersection(&others).cloned().collect();
        mine.retain(|hash| !others.contains(hash));
        if mine.is_empty() {
            return Ok(Forgotten {
                shared,
                ..Forgotten::default()
            });
        }
        others.clear();

        let mut blobs = Vec::new();
        let mut bytes = 0u64;
        for hash in &mine {
            let Ok(blob) = blob_path(&store_index, hash) else {
                continue;
            };
            if self.fs().try_exists(&self.root().join(&blob)).await? {
                bytes += match self.fs().metadata(&self.root().join(&blob)).await {
                    Ok(meta) => meta.len(),
                    Err(_) => 0,
                };
                blobs.push(blob);
            }
        }

        // The tombstone, re-rendered whole — a machine file, and the one mutable
        // document in the store besides the indexes. It can conflict under sync,
        // which is acceptable for an explicitly invoked, rare act of destruction.
        let (forgotten_path, present) = self.history_forgotten_path(&store_index).await?;
        let existing = match present {
            true => self
                .load(&forgotten_path)
                .await
                .ok()
                .map(|(_, doc)| doc.meta),
            false => None,
        };
        let text = render_forgotten(
            existing.as_ref(),
            &mine,
            subject,
            now,
            self.default_embed_format(),
        )?;

        let mut cs = self.change();
        cs.write(&forgotten_path, text);
        // The list has to be reachable, or `check` reports the record of what was
        // destroyed as an orphan. The store index is the only thing above it.
        let years = self
            .event_years(&store_dir(&store_index).join(EVENTS_DIR), ext)
            .await?;
        self.stage_index_text(
            &mut cs,
            &store_index,
            render_store_index(&years, ext, Some(&forgotten_path), embed)?,
        )
        .await?;
        self.commit(cs).await?;

        for blob in &blobs {
            let full = self.root().join(blob);
            if self.fs().try_exists(&full).await? {
                self.fs().remove_file(&full).await?;
            }
        }
        Ok(Forgotten {
            hashes: mine.into_iter().collect(),
            blobs,
            bytes,
            shared,
        })
    }

    /// The subject's live path, when the next capture would park its bytes again.
    ///
    /// Tested against the **capture set** rather than mere existence on disk,
    /// because that is exactly the population a capture parks — a file sitting
    /// unreachable in the tree would not come back, and refusing on its account
    /// would be refusing for a reason that is not true.
    async fn history_subject_live(
        &self,
        root_doc: &Path,
        subject: &Subject,
    ) -> Result<Option<PathBuf>> {
        let path = match subject {
            Subject::Path(path) => link::normalize(path),
            Subject::Id(id) => match self.index().resolve(id) {
                Some(path) => link::normalize(path),
                None => return Ok(None),
            },
        };
        Ok(self
            .history_capture_set(root_doc)
            .await?
            .into_iter()
            .find(|captured| *captured == path))
    }

    /// Stage an index write only when it would change the file — see
    /// [`history_prune`](Self::history_prune) on why a prune must not churn
    /// indexes it has no reason to touch.
    async fn stage_index_text(&self, cs: &mut ChangeSet, index: &Path, text: String) -> Result<()> {
        let unchanged = matches!(self.load(index).await, Ok((current, _)) if current == text);
        if !unchanged {
            cs.write(index, text);
        }
        Ok(())
    }

    /// Stage the removal of an index whose directory no longer holds any event —
    /// but only if it is actually there.
    async fn stage_index_removal(&self, cs: &mut ChangeSet, index: &Path) -> Result<()> {
        if self.fs().try_exists(&self.root().join(index)).await? {
            cs.remove(index);
        }
        Ok(())
    }

    /// A captured root document's text, with its `history` pointer restored if the
    /// capture carried none — the one edit a restore makes to bytes it is putting
    /// back verbatim.
    ///
    /// Absence is the only case it corrects. A pointer naming some *other* store
    /// index is what the workspace looked like at that capture, and rewriting it
    /// would be the restore substituting its own opinion for the manifest's.
    fn rooted_at_store(&self, root_doc: &Path, text: &str, store_index: &Path) -> Result<String> {
        let Some(relation) = self.relations().history_relation() else {
            return Ok(text.to_string());
        };
        let relation = relation.to_string();
        let doc = crate::document::Document::parse(root_doc, text)?;
        if doc.meta.get(&relation).is_some() {
            return Ok(text.to_string());
        }
        self.with_history_pointer(root_doc, text, doc.carrier, store_index)
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
        let (text, doc) = self.load(root_doc).await?;
        self.with_history_pointer(root_doc, &text, doc.carrier, store_index)
    }

    /// `text` — a root document's — with its `history` pointer set to
    /// `store_index`. Text in, text out: the capture edits the root it is about to
    /// hash, and the restore edits a root it is about to write back out of a blob,
    /// neither of which is what is on disk.
    fn with_history_pointer(
        &self,
        root_doc: &Path,
        text: &str,
        carrier: Option<MetaCarrier>,
        store_index: &Path,
    ) -> Result<String> {
        let relation = self
            .relations()
            .history_relation()
            .ok_or_else(|| Error::Structure("no history relation configured".into()))?
            .to_string();
        let root_dir = root_doc.parent().unwrap_or(Path::new(""));
        let pointer = link::relative(root_dir, store_index);
        crate::edit::set_in_text(
            text,
            carrier,
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

        let years = self.event_years(&events_root, ext).await?;
        let forgotten = self.history_forgotten_link(&store_index).await?;
        self.compare_index(
            &mut findings,
            &store_index,
            &render_store_index(&years, ext, forgotten.as_deref(), embed)?,
        )
        .await?;

        for year in &years {
            let months = self.event_months(&events_root.join(year), ext).await?;
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
        findings.extend(self.history_blob_findings(&store_index, ext).await?);
        Ok(findings)
    }

    /// The two blob findings: what the manifests promise and the store cannot
    /// deliver, and what the store holds that no manifest promises.
    ///
    /// Both fall out of one **mark-and-sweep** — union every event's `files`
    /// hashes, compare against the blob listing — which is what full manifests
    /// buy. Under a delta log the same question would require folding ancestry,
    /// and could not be answered at all for an event whose ancestors had not
    /// arrived.
    ///
    /// The honest cost: this parses every event document in the store, on every
    /// `check`. That is the price of validating a store whose authority is
    /// distributed across immutable documents rather than concentrated in an
    /// index — the same price [`history_log`](Self::history_log) pays, and for the
    /// same reason. Bounded by event count × manifest size.
    async fn history_blob_findings(&self, store_index: &Path, ext: &str) -> Result<Vec<Finding>> {
        // hash → the captured paths that named it, across every event. A manifest
        // routinely names one blob from several paths, and one blob is one thing
        // to put back, so the report is keyed by hash rather than by event.
        let mut referenced: BTreeMap<String, BTreeSet<PathBuf>> = BTreeMap::new();
        for event in self.history_events_in(store_index, ext).await? {
            for file in event.files {
                referenced.entry(file.hash).or_default().insert(file.path);
            }
        }

        // A hash on the tombstone list is absent *by record*: the bytes were
        // destroyed deliberately and that act was written down. Reporting it would
        // mean `check` never returned to clean after a legitimate forget, which is
        // how a user learns to stop reading `check` — and the whole point of
        // keeping the list is to be able to tell this state from loss.
        let forgotten = match self.history_forgotten_link(store_index).await? {
            Some(path) => match self.load(&path).await {
                Ok((_, doc)) => forgotten_hashes(&doc.meta),
                Err(_) => BTreeSet::new(),
            },
            None => BTreeSet::new(),
        };

        let mut findings = Vec::new();
        let mut promised: BTreeSet<PathBuf> = BTreeSet::new();
        for (hash, paths) in referenced {
            let missing = Finding::HistoryBlobMissing {
                store: store_index.to_path_buf(),
                hash: hash.clone(),
                paths: paths.into_iter().collect(),
            };
            // A digest prov could never have parked (a foreign scheme, a mangled
            // string) names no blob that could be found, so it reports as missing
            // rather than failing the whole check — a foreign event stays legible,
            // the same call `history_missing_blobs` makes.
            let Ok(blob) = blob_path(store_index, &hash) else {
                if !forgotten.contains(&hash) {
                    findings.push(missing);
                }
                continue;
            };
            // Recorded whether or not the bytes are there: this is the set of
            // paths the manifests *claim*, and a blob is an orphan by not being
            // claimed, not by being absent.
            promised.insert(blob.clone());
            if !self.fs().try_exists(&self.root().join(&blob)).await? && !forgotten.contains(&hash)
            {
                findings.push(missing);
            }
        }

        let orphaned: Vec<PathBuf> = self
            .history_blob_files(store_index)
            .await?
            .into_iter()
            .filter(|blob| !promised.contains(blob))
            .collect();
        if !orphaned.is_empty() {
            findings.push(Finding::HistoryBlobOrphaned {
                store: store_index.to_path_buf(),
                blobs: orphaned,
            });
        }
        Ok(findings)
    }

    /// Every file parked under `blobs/`, workspace-relative and sorted — the
    /// "sweep" half of the mark-and-sweep, shared by
    /// [`Finding::HistoryBlobOrphaned`] and by
    /// [`history_prune`](Self::history_prune)'s collector, so what `check` calls
    /// an orphan and what `prune` collects are the same set by construction.
    ///
    /// The top level as well as each `<2 hex>` shard: a transport's conflict copy
    /// of a blob can land at either. **Anything non-hidden counts**, not only
    /// well-formed digests — that cruft would never match a hash, which is
    /// precisely why listing files rather than parsing names is the right sweep. A
    /// dotfile is the transport's own bookkeeping and is left alone.
    async fn history_blob_files(&self, store_index: &Path) -> Result<Vec<PathBuf>> {
        let blobs_root = store_dir(store_index).join(BLOBS_DIR);
        let mut dirs = vec![blobs_root.clone()];
        dirs.extend(
            self.subdirs(&blobs_root)
                .await?
                .into_iter()
                .map(|prefix| blobs_root.join(prefix)),
        );
        let mut files = Vec::new();
        for dir in dirs {
            let Ok(entries) = self.fs().read_dir(&self.root().join(&dir)).await else {
                continue;
            };
            for entry in entries {
                let Some(name) = entry.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if entry.file_type().is_file() && !name.starts_with('.') {
                    files.push(dir.join(name));
                }
            }
        }
        files.sort();
        Ok(files)
    }

    /// The months under `year_dir` that actually hold an event.
    ///
    /// **A directory with no event in it is not a shard.** A change set removes
    /// files, not directories, so [`history_prune`](Self::history_prune) leaves an
    /// empty one behind every time it drops a month's last event — and a transport
    /// that deletes files can leave one too. Filtering where the indexes are
    /// *rendered* means neither capture nor `check` has to special-case it: an
    /// empty directory is invisible rather than a permanent
    /// [`Finding::HistoryIndexStale`] naming an index that should not exist.
    async fn event_months(&self, year_dir: &Path, ext: &str) -> Result<BTreeSet<String>> {
        let mut months = BTreeSet::new();
        for month in self.subdirs(year_dir).await? {
            if !self
                .shard_event_ids(&year_dir.join(&month), ext)
                .await?
                .is_empty()
            {
                months.insert(month);
            }
        }
        Ok(months)
    }

    /// The years under the store's `events/` that hold at least one month that
    /// holds at least one event. See [`event_months`](Self::event_months).
    async fn event_years(&self, events_root: &Path, ext: &str) -> Result<BTreeSet<String>> {
        let mut years = BTreeSet::new();
        for year in self.subdirs(events_root).await? {
            if !self
                .event_months(&events_root.join(&year), ext)
                .await?
                .is_empty()
            {
                years.insert(year);
            }
        }
        Ok(years)
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
                render_year_index(&year, &self.event_months(dir, ext).await?, ext, embed)
            }
            // `<store>/index.<ext>` — the store index itself.
            _ => render_store_index(
                &self.event_years(&dir.join(EVENTS_DIR), ext).await?,
                ext,
                self.history_forgotten_link(&index).await?.as_deref(),
                embed,
            ),
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

/// The store index. `forgotten` is the tombstone list's path when the store has
/// one — linked here because it is the only document above it, and an unlinked
/// record of what was destroyed would be reported as an orphan.
fn render_store_index(
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
fn forgotten_hashes(meta: &Value) -> BTreeSet<String> {
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
fn render_forgotten(
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
fn subject_matches(subject: &Subject, file: &FileEntry) -> bool {
    match subject {
        Subject::Id(id) => file.id.as_ref() == Some(id),
        Subject::Path(path) => file.path == *path,
    }
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
    fn timestamps_of_two_precisions_still_order_against_each_other() {
        // The migration hazard, stated as an assertion. A store keeps every
        // precision it was ever written at, because events are immutable and sync
        // interleaves devices — so the comparison, not the clock, is what has to
        // make them one order.
        let coarse = "2026-07-31T09:15:10Z";
        let fine = "2026-07-31T09:15:10.500000Z";
        assert!(
            coarse > fine,
            "the raw strings really are backwards — `Z` sorts after `.`"
        );
        assert!(
            comparable(coarse) < comparable(fine),
            "normalized, 09:15:10.000000 precedes 09:15:10.500000"
        );

        // Padding is to a fixed width, from either side, so a stamp written by
        // some other tool at millisecond or nanosecond precision still lands in
        // the right place.
        assert_eq!(
            comparable("2026-07-31T09:15:10Z"),
            "2026-07-31T09:15:10.000000Z"
        );
        assert_eq!(
            comparable("2026-07-31T09:15:10.5Z"),
            "2026-07-31T09:15:10.500000Z"
        );
        assert_eq!(
            comparable("2026-07-31T09:15:10.123456789Z"),
            "2026-07-31T09:15:10.123456Z"
        );
        // Already canonical: borrowed, not rebuilt.
        assert!(matches!(
            comparable("2026-07-31T09:15:10.123456Z"),
            std::borrow::Cow::Borrowed(_)
        ));
        // Not a `Z` stamp: left exactly as found rather than quietly mangled.
        assert_eq!(
            comparable("2026-07-31T09:15:10+01:00"),
            "2026-07-31T09:15:10+01:00"
        );

        // And the id is unaffected: it reads the calendar head only, so the
        // fraction changes nothing about where an event lives or what it is called.
        assert_eq!(id_stamp(coarse).unwrap(), id_stamp(fine).unwrap());
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
    fn same_second_captures_chain_in_the_order_they_happened() {
        // The bug microsecond precision exists to close: with `created` pinned to
        // the second, two captures in one second tied, the sort fell through to
        // the id — whose *middle* is the label slug — and every later event
        // recorded the alphabetically-last label as its `parent`, so
        // `history-list` reported forks that never happened.
        let dir = seed("ordering");
        let stamps = [
            ("2026-07-31T09:15:10.000000Z", "zulu"),
            ("2026-07-31T09:15:10.200000Z", "alpha"),
            ("2026-07-31T09:15:10.900000Z", "mike"),
        ];
        for (i, (now, label)) in stamps.iter().enumerate() {
            // Each capture must change something, or the second one writes nothing.
            write(
                &dir,
                "notes/a.md",
                &format!("---\ntitle: A\npart_of: '../index.md'\n---\nrevision {i}\n"),
            );
            capture(&dir, now, Some(label));
        }

        let events = block_on(ws(&dir).history_list(Path::new("index.md"))).unwrap();
        assert_eq!(
            events
                .iter()
                .map(|e| e.label.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("zulu"), Some("alpha"), Some("mike")],
            "capture order, not alphabetical order by label"
        );
        // A chain, not a fan: each event's parent is the one actually before it,
        // which is what makes a real fork mean something in `history-list`.
        assert_eq!(events[0].parent, None);
        assert_eq!(events[1].parent.as_deref(), Some(events[0].id.as_str()));
        assert_eq!(events[2].parent.as_deref(), Some(events[1].id.as_str()));
    }

    #[test]
    fn an_event_written_before_sub_second_precision_keeps_its_place() {
        // The mixed store, end to end: an event carrying a second-granularity
        // `created` (every event written before this precision existed) against
        // ones that carry a fraction. Compared raw, the old event would sort last
        // in its second and the newest-event lookup would pick it — so a later
        // capture would record a *superseded* event as its parent.
        let dir = seed("ordering-mixed");
        write(
            &dir,
            "notes/a.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\nfirst\n",
        );
        capture(&dir, "2026-07-31T09:15:10Z", Some("legacy"));
        write(
            &dir,
            "notes/a.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\nsecond\n",
        );
        capture(&dir, "2026-07-31T09:15:10.500000Z", Some("current"));

        let events = block_on(ws(&dir).history_list(Path::new("index.md"))).unwrap();
        assert_eq!(
            events
                .iter()
                .map(|e| e.label.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("legacy"), Some("current")]
        );
        assert_eq!(events[1].parent.as_deref(), Some(events[0].id.as_str()));
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
    fn lost_bytes_are_reported_once_per_hash_however_many_events_named_them() {
        let dir = seed("blob-missing");
        capture(&dir, "2026-07-31T09:00:00.000000Z", None);
        // A second capture that changes one file: everything else keeps the blob
        // the first capture parked, so one blob is now named by two manifests.
        write(
            &dir,
            "notes/a.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\nrevised\n",
        );
        capture(&dir, "2026-07-31T10:00:00.000000Z", None);

        let payload = crate::fixity::digest(b"JPEGBYTES");
        std::fs::remove_file(dir.join(blob_path(Path::new("history/index.md"), &payload).unwrap()))
            .unwrap();

        let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
        assert_eq!(
            findings,
            vec![Finding::HistoryBlobMissing {
                store: PathBuf::from("history/index.md"),
                hash: payload.clone(),
                paths: vec![PathBuf::from("notes/photo.jpg")],
            }],
            "one lost blob is one thing to put back, not one report per event"
        );
        // Both causes have to be readable in the text — a store that syncs is in
        // this state routinely, and a finding that cries corruption at a
        // self-resolving state is one people learn to ignore.
        let text = findings[0].to_string();
        assert!(
            text.contains("has not arrived yet") && text.contains("gone"),
            "{text}"
        );
        assert!(text.contains("notes/photo.jpg"), "{text}");

        // Deleting the blob left nothing behind, so there is no orphan to pair
        // with it: the two findings answer opposite questions.
        assert!(
            !findings
                .iter()
                .any(|f| matches!(f, Finding::HistoryBlobOrphaned { .. }))
        );
    }

    #[test]
    fn a_manifest_row_prov_could_never_have_parked_reports_rather_than_failing() {
        // A foreign event has to stay legible: `check` reads what arrived from
        // another device, and a digest in a scheme this build does not know is a
        // report, not a parse error that takes the whole run down.
        let dir = seed("blob-foreign");
        let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:00:00.000000Z", None)
        else {
            panic!("the first capture must write an event");
        };
        let event = event_path(Path::new("history/index.md"), &id, "md").unwrap();
        let text = read(&dir, event.to_str().unwrap());
        write(
            &dir,
            event.to_str().unwrap(),
            &text.replace(
                &crate::fixity::digest(b"JPEGBYTES"),
                "blake3:beefbeefbeefbeef",
            ),
        );

        let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
        let missing: Vec<&Finding> = findings
            .iter()
            .filter(|f| matches!(f, Finding::HistoryBlobMissing { .. }))
            .collect();
        assert_eq!(missing.len(), 1, "{findings:?}");
        assert!(
            missing[0].to_string().contains("blake3:"),
            "{:?}",
            missing[0]
        );
        // …and the blob it no longer names is now unreferenced, which is the
        // other half of the same sweep.
        assert!(
            findings
                .iter()
                .any(|f| matches!(f, Finding::HistoryBlobOrphaned { .. })),
            "{findings:?}"
        );
    }

    #[test]
    fn bytes_no_manifest_claims_are_reported_as_orphaned() {
        let dir = seed("blob-orphan");
        capture(&dir, "2026-07-31T09:00:00.000000Z", None);
        assert!(
            block_on(ws(&dir).check(Path::new("index.md")))
                .unwrap()
                .is_empty(),
            "a fresh capture claims every blob it parked"
        );

        // Cruft of the two shapes a transport actually leaves: a conflict copy
        // beside a real blob, and a stray at the top of the store. Neither could
        // ever match a hash, which is the point — this is not a digest check.
        write(&dir, "history/blobs/ab/sync-conflict-20260731", "junk");
        write(&dir, "history/blobs/stray.txt", "junk");
        // A hidden file is transport bookkeeping, not cruft prov should name.
        write(&dir, "history/blobs/.DS_Store", "junk");

        let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
        assert_eq!(
            findings,
            vec![Finding::HistoryBlobOrphaned {
                store: PathBuf::from("history/index.md"),
                blobs: vec![
                    PathBuf::from("history/blobs/ab/sync-conflict-20260731"),
                    PathBuf::from("history/blobs/stray.txt"),
                ],
            }],
            "one sweep, one finding, sorted — and the dotfile left alone"
        );
        assert!(
            findings[0].to_string().contains("history-prune"),
            "the report names the verb that collects them: {}",
            findings[0]
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

    #[test]
    fn a_captured_workspace_goes_back_from_its_blobs_without_a_journal_its_size() {
        // What restore will rest on, proved against what Phase 0 actually writes:
        // a manifest plus `blob_path` is enough to stage the whole capture set as
        // copies, and the journal that makes that set crash-atomic is bounded by
        // the file *count*, not by the size of the workspace. Staged as `write`s,
        // this same set would put every byte below into `.prov-journal` first.
        let dir = seed("restore-primitive");
        let payload = "J".repeat(256 * 1024);
        write(&dir, "notes/photo.jpg", &payload);
        let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
            panic!("the first capture must write an event");
        };

        // Damage of the shape a bad merge leaves: bytes clobbered at several paths
        // at once, which is why an event is a consistent cut rather than a file.
        write(&dir, "notes/a.md", "clobbered by a sync conflict");
        write(&dir, "notes/photo.jpg", "truncated");

        let mut w = ws(&dir);
        let event = block_on(w.history_event(Path::new("index.md"), &id))
            .unwrap()
            .unwrap();
        let store_index = Path::new("history/index.md");
        let mut cs = w.change();
        for file in &event.files {
            cs.copy_from(&file.path, blob_path(store_index, &file.hash).unwrap());
        }
        let journal = crate::journal::encode(cs.ops()).unwrap();
        assert!(
            journal.len() < 2048,
            "the journal for a {payload_len}-byte workspace should be paths only, \
             got {journal_len} bytes",
            payload_len = payload.len(),
            journal_len = journal.len()
        );
        block_on(w.commit(cs)).unwrap();

        // Byte-exact at every captured path — checked against the manifest's own
        // hashes, which is the only claim a restore actually owes.
        for file in &event.files {
            let bytes = std::fs::read(dir.join(&file.path)).unwrap();
            assert_eq!(
                crate::fixity::digest(&bytes),
                file.hash,
                "{} did not come back byte-exact",
                file.path.display()
            );
        }
        assert_eq!(read(&dir, "notes/photo.jpg").len(), payload.len());
    }

    // ── Prune ────────────────────────────────────────────────────────────────

    /// Plan and run a prune, the sequence the CLI performs.
    fn prune(dir: &Path, retention: &Retention) -> Pruned {
        let mut w = ws(dir);
        let root = Path::new("index.md");
        let plan = block_on(w.history_prune_plan(root, retention)).unwrap();
        block_on(w.history_prune(root, &plan)).unwrap();
        plan
    }

    /// Capture with `notes/a.md` rewritten first, so each capture has something to
    /// record — and so the untouched files keep sharing the blob they already
    /// parked.
    fn capture_edited(dir: &Path, now: &str, label: &str, body: &str) -> String {
        write(
            dir,
            "notes/a.md",
            &format!("---\ntitle: A\npart_of: '../index.md'\n---\n{body}\n"),
        );
        match capture(dir, now, Some(label)) {
            Captured::Written { id, .. } => id,
            Captured::Unchanged { id } => panic!("expected a new event, got {id}"),
        }
    }

    #[test]
    fn a_prune_drops_the_oldest_and_collects_only_what_nothing_still_references() {
        let dir = seed("prune-basic");
        let first = capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
        let second = capture_edited(&dir, "2026-07-31T10:00:00.000000Z", "two", "beta");
        let third = capture_edited(&dir, "2026-07-31T11:00:00.000000Z", "three", "gamma");

        // The blob only the dropped events name, and one every event names — the
        // whole correctness question a GC has to get right.
        let dropped_bytes = blob_path(
            Path::new("history/index.md"),
            &crate::fixity::digest(b"---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n"),
        )
        .unwrap();
        let shared_bytes = blob_path(
            Path::new("history/index.md"),
            &crate::fixity::digest(b"JPEGBYTES"),
        )
        .unwrap();
        assert!(dir.join(&dropped_bytes).exists() && dir.join(&shared_bytes).exists());

        let plan = prune(&dir, &Retention::Keep(1));
        assert_eq!(plan.events, vec![first, second]);
        assert_eq!(plan.keeping, 1);
        assert!(plan.bytes > 0, "the report has to name what it freed");

        assert!(
            !dir.join(&dropped_bytes).exists(),
            "bytes only the dropped events named must go"
        );
        assert!(
            dir.join(&shared_bytes).exists(),
            "bytes a surviving manifest still names must not"
        );

        // The store is valid, and the surviving event is still a complete
        // recovery point — which is the property that makes prune safe at all.
        assert_eq!(
            block_on(ws(&dir).check(Path::new("index.md"))).unwrap(),
            vec![]
        );
        let events = block_on(ws(&dir).history_list(Path::new("index.md"))).unwrap();
        assert_eq!(
            events.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec![third.as_str()]
        );
        let survivor = &events[0];
        assert!(
            block_on(ws(&dir).history_missing_blobs(Path::new("index.md"), survivor))
                .unwrap()
                .is_empty(),
            "every row of a surviving event must still have its bytes"
        );
    }

    #[test]
    fn a_prune_also_collects_the_orphans_that_were_already_there() {
        // `HistoryBlobOrphaned` points at this verb, so the two have to agree on
        // what an orphan is. They share the sweep, and this is the assertion that
        // says so.
        let dir = seed("prune-orphans");
        capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
        write(&dir, "history/blobs/ab/sync-conflict-20260731", "junk");

        let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
        assert!(matches!(
            findings.as_slice(),
            [Finding::HistoryBlobOrphaned { blobs, .. }]
                if blobs == &[PathBuf::from("history/blobs/ab/sync-conflict-20260731")]
        ));

        // Keeping every event still collects it: the sweep is "what nothing
        // references", not "what this drop orphaned".
        let plan = prune(&dir, &Retention::Keep(10));
        assert!(plan.events.is_empty());
        assert_eq!(
            plan.blobs,
            vec![PathBuf::from("history/blobs/ab/sync-conflict-20260731")]
        );
        assert_eq!(
            block_on(ws(&dir).check(Path::new("index.md"))).unwrap(),
            vec![]
        );
    }

    #[test]
    fn an_emptied_shard_leaves_no_index_and_no_finding() {
        let dir = seed("prune-shards");
        capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "july", "alpha");
        capture_edited(&dir, "2026-08-01T09:00:00.000000Z", "august", "beta");
        assert!(dir.join("history/events/2026/07/index.md").exists());

        // Drop July: its shard index goes with it, but the year survives because
        // August is still there.
        prune(&dir, &Retention::Before("2026-08-01".into()));
        assert!(!dir.join("history/events/2026/07/index.md").exists());
        assert!(dir.join("history/events/2026/index.md").exists());
        assert!(dir.join("history/events/2026/08/index.md").exists());
        assert_eq!(
            block_on(ws(&dir).check(Path::new("index.md"))).unwrap(),
            vec![]
        );

        // Now the year, too. A change set removes files rather than directories,
        // so `2026/07/` is still sitting there — and must be invisible, not a
        // permanent finding about an index that should not exist.
        prune(&dir, &Retention::Keep(0));
        assert!(!dir.join("history/events/2026/index.md").exists());
        assert!(
            dir.join("history/events/2026/07").is_dir(),
            "the empty directory is expected to linger"
        );
        assert_eq!(
            block_on(ws(&dir).check(Path::new("index.md"))).unwrap(),
            vec![],
            "an event-less directory is not a shard"
        );

        // …and the store still works: a later capture rebuilds the tree around it.
        capture_edited(&dir, "2026-09-01T09:00:00.000000Z", "after", "delta");
        assert_eq!(
            block_on(ws(&dir).check(Path::new("index.md"))).unwrap(),
            vec![]
        );
    }

    #[test]
    fn a_date_cutoff_keeps_the_day_it_names_and_a_typo_drops_nothing() {
        let dir = seed("prune-before");
        capture_edited(&dir, "2026-07-31T23:59:59.999999Z", "eve", "alpha");
        let boundary = capture_edited(&dir, "2026-08-01T00:00:00.000000Z", "dawn", "beta");
        let later = capture_edited(&dir, "2026-08-02T09:00:00.000000Z", "later", "gamma");

        // "before 2026-08-01" means before that day *started*: a bare date is a
        // prefix of every timestamp in its day, which is what makes the boundary
        // read the way a person means it without parsing a calendar.
        let w = ws(&dir);
        let plan = block_on(w.history_prune_plan(
            Path::new("index.md"),
            &Retention::Before("2026-08-01".into()),
        ))
        .unwrap();
        assert_eq!(plan.keeping, 2);
        assert!(!plan.events.contains(&boundary) && !plan.events.contains(&later));

        // A cutoff that is not a date deletes nothing rather than everything.
        let typo = block_on(w.history_prune_plan(
            Path::new("index.md"),
            &Retention::Before("yesterday".into()),
        ));
        assert!(typo.is_err(), "a typo must not be a silent full sweep");
    }

    #[test]
    fn a_prune_with_nothing_to_drop_touches_no_file() {
        let dir = seed("prune-noop");
        capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
        let index = read(&dir, "history/events/2026/07/index.md");
        let before = std::fs::metadata(dir.join("history/index.md"))
            .unwrap()
            .modified()
            .unwrap();

        let plan = prune(&dir, &Retention::Keep(5));
        assert!(plan.is_empty());
        // Every index a prune touches is a file some transport has to carry, so
        // one with nothing to do must not churn them.
        assert_eq!(read(&dir, "history/events/2026/07/index.md"), index);
        assert_eq!(
            std::fs::metadata(dir.join("history/index.md"))
                .unwrap()
                .modified()
                .unwrap(),
            before
        );
    }

    // ── Forget ───────────────────────────────────────────────────────────────

    fn forget(dir: &Path, subject: &Subject, now: &str, force: bool) -> Result<Forgotten> {
        block_on(ws(dir).history_forget(Path::new("index.md"), subject, now, force))
    }

    fn blob_of(bytes: &[u8]) -> PathBuf {
        blob_path(Path::new("history/index.md"), &crate::fixity::digest(bytes)).unwrap()
    }

    #[test]
    fn a_forget_destroys_only_the_bytes_nothing_else_names() {
        let dir = seed("forget-basic");
        // Two documents with byte-identical content, so one hash is shared — the
        // case content addressing makes possible and a naive "delete every hash
        // this path ever had" would get catastrophically wrong.
        let shared = "---\ntitle: Same\npart_of: '../index.md'\n---\ntwin\n";
        write(&dir, "notes/twin.md", shared);
        write(&dir, "notes/other.md", shared);
        relink_live(
            &dir,
            &[
                "notes/a.md",
                "notes/twin.md",
                "notes/other.md",
                "notes/photo.jpg.yaml",
            ],
        );
        capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
        // A second version of the doomed document, so forget has to reach every
        // hash it ever had rather than only the newest.
        write(
            &dir,
            "notes/twin.md",
            "---\ntitle: Same\npart_of: '../index.md'\n---\nrevised\n",
        );
        capture_edited(&dir, "2026-07-31T10:00:00.000000Z", "two", "beta");

        // Out of the workspace first: forget refuses a live document, and the
        // point here is what it destroys, not that guard.
        std::fs::remove_file(dir.join("notes/twin.md")).unwrap();
        relink_live(
            &dir,
            &["notes/a.md", "notes/other.md", "notes/photo.jpg.yaml"],
        );

        let revised = blob_of(b"---\ntitle: Same\npart_of: '../index.md'\n---\nrevised\n");
        assert!(dir.join(&revised).exists());
        let out = forget(
            &dir,
            &Subject::Path(PathBuf::from("notes/twin.md")),
            "2026-08-01T12:00:00.000000Z",
            false,
        )
        .unwrap();

        assert_eq!(out.blobs, vec![revised.clone()]);
        assert!(!dir.join(&revised).exists(), "the unique version must go");
        assert_eq!(
            out.shared.len(),
            1,
            "the version it shares with notes/other.md survives, and is reported"
        );
        assert!(
            dir.join(blob_of(shared.as_bytes())).exists(),
            "forgetting one document must not reach into another's history"
        );
        assert!(out.bytes > 0);

        // The record of *what was captured* survives the destruction of the bytes.
        // That is the bargain, and it has to be visible in the store.
        let events = block_on(ws(&dir).history_list(Path::new("index.md"))).unwrap();
        assert!(
            events
                .iter()
                .any(|e| e.files.iter().any(|f| f.path == Path::new("notes/twin.md"))),
            "events are immutable: the manifest still names it"
        );

        // Tombstoned, reachable, and clean — the record must not itself be an
        // orphan, and a deliberate destruction must not leave `check` failing.
        let tombstone = read(&dir, "history/forgotten.yaml");
        assert!(tombstone.contains("notes/twin.md") && tombstone.contains("2026-08-01T12:00:00"));
        assert!(read(&dir, "history/index.md").contains("forgotten.yaml"));
        assert_eq!(
            block_on(ws(&dir).check(Path::new("index.md"))).unwrap(),
            vec![]
        );
    }

    #[test]
    fn a_tombstoned_hash_is_accounted_for_where_a_lost_one_is_not() {
        let dir = seed("forget-findings");
        capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
        std::fs::remove_file(dir.join("notes/a.md")).unwrap();
        relink_live(&dir, &["notes/photo.jpg.yaml"]);

        forget(
            &dir,
            &Subject::Path(PathBuf::from("notes/a.md")),
            "2026-08-01T12:00:00.000000Z",
            false,
        )
        .unwrap();
        assert!(
            block_on(ws(&dir).check(Path::new("index.md")))
                .unwrap()
                .is_empty(),
            "a recorded destruction is not a finding — a `check` that never came \
             back to clean would teach the user to stop reading it"
        );
        assert_eq!(
            block_on(ws(&dir).history_forgotten(Path::new("index.md")))
                .unwrap()
                .len(),
            1
        );

        // …and the suppression is precise, not blanket: bytes that went missing
        // without a record still say so.
        std::fs::remove_file(dir.join(blob_of(b"JPEGBYTES"))).unwrap();
        let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
        assert!(
            matches!(findings.as_slice(), [Finding::HistoryBlobMissing { paths, .. }]
                if paths == &[PathBuf::from("notes/photo.jpg")]),
            "{findings:?}"
        );
    }

    #[test]
    fn a_forget_refuses_a_document_the_next_capture_would_park_again() {
        let dir = seed("forget-live");
        capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");

        let subject = Subject::Path(PathBuf::from("notes/a.md"));
        let err = forget(&dir, &subject, "2026-08-01T12:00:00.000000Z", false).unwrap_err();
        assert!(
            err.to_string().contains("notes/a.md")
                && err.to_string().contains("still in the workspace"),
            "the refusal has to name the document and say why: {err}"
        );
        assert!(
            dir.join(blob_of(
                b"---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n"
            ))
            .exists(),
            "a refused forget destroys nothing"
        );

        // Forced, for the deliberate "purge the history, keep the file" case.
        let out = forget(&dir, &subject, "2026-08-01T12:00:00.000000Z", true).unwrap();
        assert!(!out.is_empty());
    }

    #[test]
    fn forgetting_by_id_reaches_the_versions_a_path_key_would_miss() {
        let dir = seed("forget-id");
        let mut w = ws(&dir);
        let id = Id("b7k2m".into());
        w.index_mut().register(&id, Path::new("notes/a.md"));
        block_on(w.history_capture(Path::new("index.md"), "2026-07-31T09:00:00.000000Z", None))
            .unwrap();

        // The move: the same document, a second path, and a hash a path-keyed
        // forget would leave behind.
        std::fs::rename(dir.join("notes/a.md"), dir.join("notes/b.md")).unwrap();
        write(
            &dir,
            "notes/b.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\nrevised\n",
        );
        relink_live(&dir, &["notes/b.md", "notes/photo.jpg.yaml"]);
        w.index_mut().set_path(&id, Path::new("notes/b.md"));
        block_on(w.history_capture(Path::new("index.md"), "2026-07-31T10:00:00.000000Z", None))
            .unwrap();

        // Out of the workspace, so the guard is not what is under test.
        std::fs::remove_file(dir.join("notes/b.md")).unwrap();
        relink_live(&dir, &["notes/photo.jpg.yaml"]);
        w.index_mut().unregister(&id);

        let out = block_on(w.history_forget(
            Path::new("index.md"),
            &Subject::Id(id),
            "2026-08-01T12:00:00.000000Z",
            false,
        ))
        .unwrap();
        assert_eq!(out.hashes.len(), 2, "both versions, across the rename");
        assert!(
            !dir.join(blob_of(
                b"---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n"
            ))
            .exists()
        );
        assert!(
            !dir.join(blob_of(
                b"---\ntitle: A\npart_of: '../index.md'\n---\nrevised\n"
            ))
            .exists()
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

    // ── Restore ──────────────────────────────────────────────────────────────

    /// [`relink`], keeping the `history` pointer.
    ///
    /// `relink` writes the root a workspace had *before* its store existed, which
    /// is what the lineage tests want (a capture follows, and re-bootstraps the
    /// pointer). A restore has no such capture behind it: strip the pointer and
    /// the store is simply not there to restore from.
    fn relink_live(dir: &Path, contents: &[&str]) {
        let list = contents
            .iter()
            .map(|c| format!("- {c}\n"))
            .collect::<String>();
        write(
            dir,
            "index.md",
            &format!("---\ntitle: Home\nhistory: history/index.md\ncontents:\n{list}---\nroot\n"),
        );
    }

    /// Plan and run a restore in one go, on a workspace of the caller's choosing —
    /// the sequence the CLI performs, so a test exercises the shipped path rather
    /// than a convenient shortcut past it.
    fn restore(
        w: &mut Workspace<StdFs, Minter, FileIndex>,
        id: &str,
        scope: &Scope,
        exact: bool,
        force: bool,
    ) -> Result<RestorePlan> {
        let root = Path::new("index.md");
        let event = block_on(w.history_event(root, id))?.expect("the event should be in the store");
        let plan = block_on(w.history_restore_plan(root, &event, scope, exact))?;
        block_on(w.history_restore(root, &plan, force))?;
        Ok(plan)
    }

    fn dispositions(plan: &RestorePlan, want: Disposition) -> Vec<&Path> {
        plan.ops
            .iter()
            .filter(|op| op.disposition == want)
            .map(|op| op.path.as_path())
            .collect()
    }

    #[test]
    fn a_restore_puts_the_whole_consistent_cut_back_byte_exact() {
        let dir = seed("restore-cut");
        let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
            panic!("the first capture must write an event");
        };

        // Damage of the shape a bad merge leaves: several files at once, which is
        // why an event is a consistent cut rather than a file. One of them is the
        // parent's child list — the structural half a per-file undo would miss.
        write(&dir, "notes/a.md", "clobbered by a sync conflict");
        write(&dir, "notes/photo.jpg", "truncated");
        relink_live(&dir, &["notes/photo.jpg.yaml"]);

        let mut w = ws(&dir);
        let plan = restore(&mut w, &id, &Scope::Whole, false, false).unwrap();
        assert_eq!(
            dispositions(&plan, Disposition::Overwrite),
            vec![
                Path::new("index.md"),
                Path::new("notes/a.md"),
                Path::new("notes/photo.jpg")
            ]
        );
        // The sidecar was never touched, so the restore has nothing to say about
        // it — and says so, rather than rewriting bytes that already match.
        assert_eq!(
            dispositions(&plan, Disposition::Unchanged),
            vec![Path::new("notes/photo.jpg.yaml")]
        );

        let event = block_on(w.history_event(Path::new("index.md"), &id))
            .unwrap()
            .unwrap();
        for file in &event.files {
            let bytes = std::fs::read(dir.join(&file.path)).unwrap();
            assert_eq!(
                crate::fixity::digest(&bytes),
                file.hash,
                "{} did not come back byte-exact",
                file.path.display()
            );
        }
        assert!(read(&dir, "index.md").contains("notes/a.md"));
        assert!(
            block_on(ws(&dir).check(Path::new("index.md")))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_default_restore_deletes_nothing_and_exact_makes_the_tree_match() {
        let dir = seed("restore-exact");
        let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
            panic!("the first capture must write an event");
        };

        // What a sync transport actually does: leaves a second file behind, linked
        // into the graph. Writing captured bytes over the top does not remove it —
        // which is the gap `--exact` exists to close, and why the default leaving
        // it is a deliberate decision rather than an oversight.
        write(
            &dir,
            "notes/a.sync-conflict-20260731.md",
            "---\ntitle: A (conflicted copy)\npart_of: '../index.md'\n---\nalpha\n",
        );
        relink_live(
            &dir,
            &[
                "notes/a.md",
                "notes/a.sync-conflict-20260731.md",
                "notes/photo.jpg.yaml",
            ],
        );

        // Both plans off the same damaged tree, so what differs between them is the
        // flag and nothing else. Taken before either runs, because the delete set is
        // drawn from the *reachable* files: the restored root stops linking the
        // conflict copy, and a plan computed afterwards would no longer see it.
        let mut w = ws(&dir);
        let root = Path::new("index.md");
        let event = block_on(w.history_event(root, &id)).unwrap().unwrap();
        let additive =
            block_on(w.history_restore_plan(root, &event, &Scope::Whole, false)).unwrap();
        let exact = block_on(w.history_restore_plan(root, &event, &Scope::Whole, true)).unwrap();

        assert_eq!(additive.count(Disposition::Remove), 0);
        block_on(w.history_restore(root, &additive, false)).unwrap();
        assert!(
            dir.join("notes/a.sync-conflict-20260731.md").exists(),
            "the default restore must delete nothing"
        );

        assert_eq!(
            exact.removals().collect::<Vec<_>>(),
            vec![Path::new("notes/a.sync-conflict-20260731.md")]
        );
        block_on(w.history_restore(root, &exact, false)).unwrap();
        assert!(!dir.join("notes/a.sync-conflict-20260731.md").exists());

        // The one subtree the mechanism is blind to survives its own exact
        // restore: an event that deleted every event newer than it would destroy
        // the recovery points themselves.
        let event = event_path(Path::new("history/index.md"), &id, "md").unwrap();
        assert!(dir.join(event).exists(), "the store must survive --exact");
        assert!(dir.join("history/blobs").exists());
    }

    #[test]
    fn restoring_the_state_the_workspace_already_holds_writes_nothing() {
        let dir = seed("restore-noop");
        let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
            panic!("the first capture must write an event");
        };
        let before = std::fs::metadata(dir.join("notes/a.md"))
            .unwrap()
            .modified()
            .unwrap();

        let mut w = ws(&dir);
        let plan = restore(&mut w, &id, &Scope::Whole, false, false).unwrap();
        assert!(plan.is_noop(), "every row already matches the capture");
        assert_eq!(plan.count(Disposition::Unchanged), plan.ops.len());
        assert_eq!(
            std::fs::metadata(dir.join("notes/a.md"))
                .unwrap()
                .modified()
                .unwrap(),
            before,
            "an unchanged row must not be rewritten"
        );
    }

    #[test]
    fn a_row_whose_blob_never_arrived_is_skipped_by_name_not_fatal() {
        // A manifest and the blobs it names travel over a transport separately, so
        // a half-synced event is ordinary rather than broken. The rows prov *can*
        // supply still come back; the one it cannot is reported.
        let dir = seed("restore-halfsynced");
        let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
            panic!("the first capture must write an event");
        };
        let payload = crate::fixity::digest(b"JPEGBYTES");
        std::fs::remove_file(dir.join(blob_path(Path::new("history/index.md"), &payload).unwrap()))
            .unwrap();
        write(&dir, "notes/a.md", "clobbered");
        write(&dir, "notes/photo.jpg", "truncated");

        let mut w = ws(&dir);
        let plan = restore(&mut w, &id, &Scope::Whole, false, false).unwrap();
        assert_eq!(
            dispositions(&plan, Disposition::NoBytes),
            vec![Path::new("notes/photo.jpg")]
        );
        assert_eq!(
            read(&dir, "notes/a.md"),
            "---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n"
        );
        assert_eq!(
            read(&dir, "notes/photo.jpg"),
            "truncated",
            "a row with no bytes is left alone, not emptied"
        );

        // Under `--exact` the same event is refused nothing: the delete pass is
        // drawn from the manifest's paths, and a row it cannot supply is still a
        // path the manifest holds — so nothing is removed on the strength of bytes
        // that merely have not arrived.
        let mut w = ws(&dir);
        let exact = restore(&mut w, &id, &Scope::Whole, true, false).unwrap();
        assert_eq!(exact.count(Disposition::Remove), 0);
        assert!(dir.join("notes/photo.jpg").exists());
    }

    #[test]
    fn a_restore_refuses_to_displace_a_registration_unless_it_resolves_it_itself() {
        let dir = seed("restore-collision");
        let mut w = ws(&dir);
        let id = Id("b7k2m".into());
        w.index_mut().register(&id, Path::new("notes/a.md"));
        let Captured::Written { id: event, .. } =
            block_on(w.history_capture(Path::new("index.md"), "2026-07-31T09:15:22Z", None))
                .unwrap()
        else {
            panic!("the first capture must write an event");
        };

        // The document moved after the capture. Restoring additively would put the
        // old path back and leave the new one there — two documents spelling one
        // id, which only their author can arbitrate.
        std::fs::rename(dir.join("notes/a.md"), dir.join("notes/b.md")).unwrap();
        relink_live(&dir, &["notes/b.md", "notes/photo.jpg.yaml"]);
        w.index_mut().set_path(&id, Path::new("notes/b.md"));

        let ev = block_on(w.history_event(Path::new("index.md"), &event))
            .unwrap()
            .unwrap();
        let plan =
            block_on(w.history_restore_plan(Path::new("index.md"), &ev, &Scope::Whole, false))
                .unwrap();
        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(plan.conflicts[0].path, Path::new("notes/a.md"));
        assert!(matches!(
            plan.conflicts[0].collision,
            Collision::Id { ref held_by, .. } if held_by == Path::new("notes/b.md")
        ));
        let err = block_on(w.history_restore(Path::new("index.md"), &plan, false)).unwrap_err();
        assert!(matches!(err, Error::Collision(Collision::Id { .. })));
        assert!(
            !dir.join("notes/a.md").exists(),
            "a refused restore must move nothing"
        );

        // `--exact` removes the document currently holding the id, so nothing is
        // displaced and the same restore is no longer a collision at all. This is
        // the difference between "put these bytes back too" and "make the tree
        // match this capture".
        let exact =
            block_on(w.history_restore_plan(Path::new("index.md"), &ev, &Scope::Whole, true))
                .unwrap();
        assert!(
            exact.conflicts.is_empty(),
            "a collision the restore itself resolves is not a collision: {:?}",
            exact.conflicts
        );
        assert_eq!(
            exact.removals().collect::<Vec<_>>(),
            vec![Path::new("notes/b.md")]
        );
        block_on(w.history_restore(Path::new("index.md"), &exact, false)).unwrap();
        assert!(dir.join("notes/a.md").exists());
        assert!(!dir.join("notes/b.md").exists());
    }

    #[test]
    fn a_scope_restores_a_slice_and_refuses_what_the_capture_never_held() {
        let dir = seed("restore-scope");
        let mut w = ws(&dir);
        let id = Id("b7k2m".into());
        w.index_mut().register(&id, Path::new("notes/a.md"));
        let Captured::Written { id: event, .. } =
            block_on(w.history_capture(Path::new("index.md"), "2026-07-31T09:15:22Z", None))
                .unwrap()
        else {
            panic!("the first capture must write an event");
        };
        write(&dir, "notes/a.md", "clobbered");
        write(&dir, "notes/photo.jpg", "truncated");

        // A directory scope takes everything the capture held beneath it; the root
        // above it is left alone.
        let ev = block_on(w.history_event(Path::new("index.md"), &event))
            .unwrap()
            .unwrap();
        let plan = block_on(w.history_restore_plan(
            Path::new("index.md"),
            &ev,
            &Scope::Paths(vec![PathBuf::from("notes")]),
            false,
        ))
        .unwrap();
        assert_eq!(plan.ops.len(), 3, "the three files under notes/");
        assert!(!plan.ops.iter().any(|op| op.path == Path::new("index.md")));

        // An id scope reaches the one document, wherever the capture found it.
        let by_id = block_on(w.history_restore_plan(
            Path::new("index.md"),
            &ev,
            &Scope::Id(id.clone()),
            false,
        ))
        .unwrap();
        assert_eq!(
            by_id
                .ops
                .iter()
                .map(|op| op.path.as_path())
                .collect::<Vec<_>>(),
            vec![Path::new("notes/a.md")]
        );
        block_on(w.history_restore(Path::new("index.md"), &by_id, false)).unwrap();
        assert!(read(&dir, "notes/a.md").contains("alpha"));
        assert_eq!(
            read(&dir, "notes/photo.jpg"),
            "truncated",
            "a scope restores only what it names"
        );

        // A scope that selects nothing is a typo, not an empty restore.
        for scope in [
            Scope::Paths(vec![PathBuf::from("notes/never.md")]),
            Scope::Id(Id("nosuch".into())),
        ] {
            assert!(
                block_on(w.history_restore_plan(Path::new("index.md"), &ev, &scope, false))
                    .is_err()
            );
        }

        // And `exact` is a statement about the whole tree, which a slice of the
        // capture cannot make.
        assert!(
            block_on(w.history_restore_plan(
                Path::new("index.md"),
                &ev,
                &Scope::Paths(vec![PathBuf::from("notes")]),
                true,
            ))
            .is_err()
        );
    }

    #[test]
    fn a_restored_root_never_strands_the_store_unreachable() {
        // A capture always records a root that already declares the store, so this
        // is the hand-edited (or foreign) case: a manifest whose root predates the
        // pointer. Restoring it verbatim would leave `history/` unreachable —
        // invisible to `check`, and unfindable by the next restore.
        let dir = seed("restore-pointer");
        let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
            panic!("the first capture must write an event");
        };
        let rootless =
            "---\ntitle: Home\ncontents:\n- notes/a.md\n- notes/photo.jpg.yaml\n---\nroot\n";
        let hash = crate::fixity::digest(rootless.as_bytes());
        let blob = blob_path(Path::new("history/index.md"), &hash).unwrap();
        std::fs::create_dir_all(dir.join(&blob).parent().unwrap()).unwrap();
        std::fs::write(dir.join(&blob), rootless).unwrap();

        let mut w = ws(&dir);
        let mut event = block_on(w.history_event(Path::new("index.md"), &id))
            .unwrap()
            .unwrap();
        for file in &mut event.files {
            if file.path == Path::new("index.md") {
                file.hash = hash.clone();
            }
        }
        let plan =
            block_on(w.history_restore_plan(Path::new("index.md"), &event, &Scope::Whole, false))
                .unwrap();
        block_on(w.history_restore(Path::new("index.md"), &plan, false)).unwrap();

        let root = read(&dir, "index.md");
        assert!(
            root.contains("history:"),
            "a restored root must still declare the store: {root}"
        );
        assert!(
            block_on(ws(&dir).history_path(Path::new("index.md")))
                .unwrap()
                .is_some()
        );
    }
}
