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
//! **The feature itself is [`prov_history`]** — the model, the store's layout,
//! and every verb. It reaches this workspace through two host traits
//! (`HistoryReadHost`, `HistoryWriteHost`), which [`Workspace`](crate::Workspace)
//! implements; nothing in that crate names `prov`.
//!
//! What is left here is the integration:
//!
//! - the [`Workspace`](crate::Workspace) methods below, each a one-line forward
//!   through [`history_store`](crate::Workspace::history_store) (or
//!   `history_store_mut`), kept so existing call sites are unchanged;
//! - `HistoryIssue` → [`Finding`](crate::validate::Finding), and the remedies
//!   in [`crate::remedy`];
//! - the composition with the recycle bin, the generated about page and the
//!   configuration, which is what the host traits' `history_exclusions`,
//!   `history_captures` and `registration_conflict` carry across the boundary.
//!
//! The tests stay here too, and deliberately: they are integration tests over a
//! real workspace — they run `check`, apply a [`Fix`](crate::Fix), warm a
//! [`FixityCache`](crate::FixityCache), recycle a document — so they exercise
//! the moved logic through exactly the composition it has to survive. The
//! fixtures they share are in `support`.

mod capture;
mod check;
mod forget;
mod prune;
mod read;
mod restore;
mod store;

pub use prov_history::{
    BLOBS_DIR, Captured, Conflict, Disposition, EVENTS_DIR, Event, FORGOTTEN_STEM, FileEntry,
    Forgotten, HISTORY_DIR, HistoryIssue, HistoryStore, Latest, Presence, Pruned, RestoreOp,
    RestorePlan, Retention, Scope, StoreLocation, Subject, Summary, TRIGGER_MANUAL, Version,
    blob_path, event_path, shard_of, store_dir,
};

#[cfg(test)]
mod support;
