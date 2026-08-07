use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::identity::Id;
use crate::index::Collision;

use super::event_id::*;
use super::paths::*;

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
/// metadata that lets [`history_list`](crate::Workspace::history_list) narrate it.
///
/// Immutable once written. Everything a restore needs is here plus the blobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// The event id — also the document's file stem, and a pure function of its
    /// path (see [`event_path`](super::event_path)).
    pub id: String,
    /// Where the event document lives, workspace-relative.
    pub path: PathBuf,
    /// RFC 3339 UTC timestamp of the capture.
    pub created: String,
    /// How the capture was invoked ([`TRIGGER_MANUAL`](super::TRIGGER_MANUAL)).
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
    /// The complete capture set at that moment. A manifest this library writes
    /// is sorted by [`path_sort_key`] (§3.1); one read back off disk keeps
    /// whatever order it was written in — an event's id is the one it was
    /// minted with, never re-derived, so an older row order is not an error.
    pub files: Vec<FileEntry>,
}

impl Event {
    /// This event's manifest as a path → (id, hash) map, for diffing against
    /// another event's, and for comparing two manifests **by content rather
    /// than by row order** ([`manifest_of`]).
    fn manifest(&self) -> BTreeMap<&Path, (&Option<Id>, &str)> {
        manifest_of(&self.files)
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
    /// [`exact`](crate::Workspace::history_restore_plan) accepts.
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
    /// The captured bytes are already there too, but under a spelling that
    /// differs from the manifest's only by case — a case-insensitive filesystem
    /// found them via `notes/a.md` for a row the manifest holds as `notes/A.md`,
    /// say. Nothing about the content is wrong, so nothing is overwritten; the
    /// file is renamed in place to the manifest's own spelling, which is the
    /// captured truth about where it lived. Distinct from
    /// [`Unchanged`](Disposition::Unchanged) because a rename *is* a write — a
    /// plan reporting no-op here would be wrong, and so would an `--exact` pass
    /// that deleted the very file this row just claimed.
    CaseOnly,
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
    pub(super) fn rank(self) -> u8 {
        match self {
            Disposition::Create => 0,
            Disposition::Overwrite => 1,
            Disposition::CaseOnly => 2,
            Disposition::Unchanged => 3,
            Disposition::NoBytes => 4,
            Disposition::Remove => 5,
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
    /// The on-disk path this row's file is actually found under right now, when
    /// that differs from `path` **only by case** — set for
    /// [`Overwrite`](Disposition::Overwrite) and
    /// [`CaseOnly`](Disposition::CaseOnly) rows a case-insensitive filesystem
    /// resolved to a differently-spelled entry, `None` for everything else
    /// (including every row on a filesystem that does not fold case, where this
    /// cannot arise). [`history_restore`](crate::Workspace::history_restore) renames it
    /// to `path` before writing, so the workspace ends up holding the exact
    /// spelling the manifest recorded rather than silently keeping the old one.
    pub rename_from: Option<PathBuf>,
}

/// A registration a restore would displace, and the path whose restoration would
/// displace it.
///
/// Refused rather than resolved: two documents claim one id and only their author
/// knows which should keep it. See
/// [`registration_conflict`](crate::Workspace::registration_conflict).
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
/// [`history_restore`](crate::Workspace::history_restore) rather than recomputing — a
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
                Disposition::Create
                    | Disposition::Overwrite
                    | Disposition::CaseOnly
                    | Disposition::Remove
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
/// [`history_prune`](crate::Workspace::history_prune), so what runs is what the user
/// was asked about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pruned {
    /// The events to drop, oldest first. Ids, which resolve to their documents by
    /// the pure id → path function.
    pub events: Vec<String>,
    /// The blob files to collect: everything under `blobs/` that no surviving
    /// manifest names, workspace-relative and sorted.
    ///
    /// This is the same sweep [`Finding::HistoryBlobOrphaned`](crate::validate::Finding::HistoryBlobOrphaned) reports, taken
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

/// What a [`history_forget`](crate::Workspace::history_forget) destroyed.
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

/// The newest event in a store, named without reading the rest of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Latest {
    /// The event's id.
    pub id: String,
    /// Its `created` timestamp, verbatim as the document spells it — so a caller
    /// comparing it against another event compares like with like, whatever
    /// precision each was written at.
    pub created: String,
}

/// What a store holds, answered from directory listings rather than from its
/// contents — the shape of the store, not the history in it.
///
/// The question this exists for is "is a capture due?", which a host asks on
/// every open and which [`history_list`](crate::Workspace::history_list) is the
/// wrong way to answer: that parses **every** event document, and each holds one
/// row per file in the workspace, so asking routinely costs O(events × files).
/// This walks the shard tree and reads at most one document — see
/// [`history_summary`](crate::Workspace::history_summary) for which one, and why
/// one is enough.
///
/// Deliberately *not* carrying the store's size on disk. A [`DirEntry`] names no
/// length, so totalling bytes means one `metadata` call per blob — precisely the
/// per-file cost over a file-provider backend that this type exists to avoid.
/// [`history_store_bytes`](crate::Workspace::history_store_bytes) answers that
/// separately, and says in its own name that it is the expensive one.
///
/// [`DirEntry`]: crate::fs::DirEntry
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    /// Whether a store was found at all — declared by the root, or sitting at
    /// the conventional path with no pointer to it.
    pub store_exists: bool,
    /// How many event-shaped files the shard tree holds, **including any that no
    /// longer parse**. A count of event *slots*, matching the way
    /// `history_events_in` counts a document a transport tore in transit: the
    /// file is still evidence that a capture happened, even when nothing in it
    /// can be trusted.
    pub events: usize,
    /// The newest event, or `None` when the store holds none that can be read.
    pub latest: Option<Latest>,
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

#[cfg(test)]
mod tests {
    use super::super::TRIGGER_MANUAL;
    use super::super::support::entry;
    use super::*;

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
}
