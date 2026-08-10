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
//!   conflicted index is a [`Finding::HistoryIndexStale`](crate::validate::Finding::HistoryIndexStale) with a mechanical
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
//!
//! ## Where the code lives
//!
//! The module is split by *what a reader is after*, not by type:
//!
//! - `model` — the vocabulary every operation is spelled in: an [`Event`] and
//!   its manifest rows, and the outcome of each verb.
//! - `layout` — the store's shape on disk: id ⇄ shard, hash → blob.
//! - `event_id` — the canonical form an event's id is a digest of, and the
//!   timestamp arithmetic that keeps ids orderable across precisions.
//! - `paths` — the path and manifest helpers the canonical form is built from.
//! - `docs` — the store's own documents: parsing an event's frontmatter, and
//!   rendering the rebuildable index and tombstone caches.
//! - `read`, `capture`, `restore`, `prune`, `forget`, `check` — one module per
//!   verb, each an `impl Workspace` block.
//! - `store` — the plumbing those verbs share: staging an index write, the
//!   root's `history` pointer, walking shards.
//!
//! Tests sit in each file's own `mod tests`, as elsewhere in the crate. The
//! fixtures they share — a seeded workspace, a capture, a torn event — are in
//! `support`, which no sibling could own without every other one reaching
//! across for it.

mod capture;
mod check;
mod docs;
mod event_id;
mod forget;
mod layout;
mod model;
mod paths;
mod prune;
mod read;
mod restore;
mod store;

pub use prov_history::{HistoryIssue, HistoryStore};

pub use layout::{StoreLocation, blob_path, event_path, shard_of, store_dir};
pub use model::{
    Captured, Conflict, Disposition, Event, FileEntry, Forgotten, Latest, Presence, Pruned,
    RestoreOp, RestorePlan, Retention, Scope, Subject, Summary, Version,
};

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

#[cfg(test)]
mod support;
