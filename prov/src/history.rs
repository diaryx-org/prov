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
//! (`HistoryReadHost`, `HistoryWriteHost`), which [`Workspace`]
//! implements; nothing in that crate names `prov`.
//!
//! What is left here is the integration:
//!
//! - the [`Workspace`] methods below, each a one-line forward
//!   through [`history_store`](crate::Workspace::history_store) (or
//!   `history_store_mut`), kept so existing call sites are unchanged;
//! - `HistoryIssue` → [`Finding`](crate::validate::Finding), and the remedies
//!   in [`crate::remedy`];
//! - the composition with the recycle bin, the generated about page and the
//!   configuration, which is what the host traits' `history_exclusions`,
//!   `history_captures` and `registration_conflict` carry across the boundary.
//!
//! The tests follow that same line. Everything about the *store* — capture,
//! restore, prune, forget, the reads, and the [`HistoryIssue`]s it reports —
//! is tested in [`prov_history`], against a host built to its two traits and
//! nothing more, so those tests cannot come to depend on anything history is
//! defined not to know. What stays here is what only exists here: the
//! `HistoryIssue` → [`Finding`](crate::Finding) mapping and its
//! [`Fix`](crate::Fix)es, the claim that each verb leaves the whole *workspace*
//! `check`-clean, and the answers this crate gives to the host traits — the
//! recycle bin's items kept out of a capture set, the store's interior kept out
//! of the title index, the [`FixityCache`](crate::FixityCache) a capture reads
//! through. See `tests` for the entry condition stated as a rule.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::workspace::Workspace;
use prov_graph::error::Result;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

pub use prov_history::{
    BLOBS_DIR, Captured, Change, Conflict, DiffRow, Disposition, EVENTS_DIR, Event, FORGOTTEN_STEM,
    FileEntry, Forgotten, HISTORY_DIR, HistoryIssue, HistoryStore, Latest, ManifestDiff, Presence,
    Pruned, RestoreOp, RestorePlan, Retention, Retrieved, Scope, StoreLocation, Subject, Summary,
    TRIGGER_MANUAL, Version, blob_path, comparable, event_path, manifest_diff, shard_of, store_dir,
    under,
};

/// The compatibility surface: one forward per verb, through
/// [`history_store`](Workspace::history_store) for the reads and plans and
/// [`history_store_mut`](Workspace::history_store_mut) for the four that write.
///
/// These carry no logic of their own — deliberately. Every one of them is a
/// method that predates the extraction and is called from `prov-cli`, from
/// [`crate::remedy`], or from a workspace-level pass like
/// [`check`](Workspace::check); keeping them means the boundary moved without a
/// single call site having to.
impl<FS: Storage, IdP, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// The **capture set**: the live graph, minus prov's two byte-parking stores
    /// and its one derived page. See `prov_history::HistoryStore::capture_set`.
    pub async fn history_capture_set(&self, root_doc: &Path) -> Result<Vec<PathBuf>> {
        self.history_store().capture_set(root_doc).await
    }

    /// What the store holds, without reading the history in it — the cheap
    /// answer to "is a capture due?". See `prov_history::HistoryStore::summary`.
    pub async fn history_summary(&self, root_doc: &Path) -> Result<Summary> {
        self.history_store().summary(root_doc).await
    }

    /// What the store occupies on disk, in bytes. See
    /// `prov_history::HistoryStore::store_bytes`.
    pub async fn history_store_bytes(&self, root_doc: &Path) -> Result<u64> {
        self.history_store().store_bytes(root_doc).await
    }

    /// Every event in the store, oldest first. See
    /// `prov_history::HistoryStore::list`.
    pub async fn history_list(&self, root_doc: &Path) -> Result<Vec<Event>> {
        self.history_store().list(root_doc).await
    }

    /// One event by id, resolved through the pure id → path function. See
    /// `prov_history::HistoryStore::event`.
    pub async fn history_event(&self, root_doc: &Path, id: &str) -> Result<Option<Event>> {
        self.history_store().event(root_doc, id).await
    }

    /// The captured paths in `event` whose pre-image bytes are not parked in the
    /// store. See `prov_history::HistoryStore::missing_blobs`.
    pub async fn history_missing_blobs(
        &self,
        root_doc: &Path,
        event: &Event,
    ) -> Result<BTreeSet<PathBuf>> {
        self.history_store().missing_blobs(root_doc, event).await
    }

    /// The bytes one captured file held at `event`. See
    /// `prov_history::HistoryStore::cat`.
    pub async fn history_cat(
        &self,
        root_doc: &Path,
        event: &Event,
        subject: &Subject,
    ) -> Result<Retrieved> {
        self.history_store().cat(root_doc, event, subject).await
    }

    /// One document's lineage across every capture. See
    /// `prov_history::HistoryStore::log`.
    pub async fn history_log(&self, root_doc: &Path, subject: &Subject) -> Result<Vec<Version>> {
        self.history_store().log(root_doc, subject).await
    }

    /// The hashes this store has deliberately destroyed. See
    /// `prov_history::HistoryStore::forgotten`.
    pub async fn history_forgotten(&self, root_doc: &Path) -> Result<BTreeSet<String>> {
        self.history_store().forgotten(root_doc).await
    }

    /// Validate the history store: every index document against the directory it
    /// describes, plus the blob mark-and-sweep. See
    /// `prov_history::HistoryStore::findings`.
    pub async fn history_findings(&self, root_doc: &Path) -> Result<Vec<HistoryIssue>> {
        self.history_store().findings(root_doc).await
    }

    /// The text one history index document *should* hold, rebuilt from the
    /// directory it describes — the repair behind
    /// [`Fix::RebuildHistoryIndex`](crate::Fix::RebuildHistoryIndex). See
    /// `prov_history::HistoryStore::index_text`.
    pub async fn history_index_text(&self, index: &Path) -> Result<String> {
        self.history_store().index_text(index).await
    }

    /// The root document's text with its `history` pointer at the store index —
    /// the repair behind
    /// [`Fix::LinkHistoryStore`](crate::Fix::LinkHistoryStore). See
    /// `prov_history::HistoryStore::pointer_text`.
    pub(crate) async fn history_pointer_text(
        &self,
        root_doc: &Path,
        store_index: &Path,
    ) -> Result<String> {
        self.history_store()
            .pointer_text(root_doc, store_index)
            .await
    }

    /// What restoring `event` would do, computed **before a byte moves**. See
    /// `prov_history::HistoryStore::restore_plan`.
    pub async fn history_restore_plan(
        &self,
        root_doc: &Path,
        event: &Event,
        scope: &Scope,
        exact: bool,
    ) -> Result<RestorePlan> {
        self.history_store()
            .restore_plan(root_doc, event, scope, exact)
            .await
    }

    /// What pruning to `retention` would drop: the events, and the blobs no
    /// surviving manifest would name. See
    /// `prov_history::HistoryStore::prune_plan`.
    pub async fn history_prune_plan(
        &self,
        root_doc: &Path,
        retention: &Retention,
    ) -> Result<Pruned> {
        self.history_store().prune_plan(root_doc, retention).await
    }

    /// Capture the workspace: hash the capture set, park newly-seen blobs, and
    /// write one immutable event document into its `<YYYY>/<MM>` shard. See
    /// `prov_history::HistoryStore::capture`.
    pub async fn history_capture(
        &mut self,
        root_doc: &Path,
        now: &str,
        label: Option<&str>,
    ) -> Result<Captured> {
        self.history_store_mut().capture(root_doc, now, label).await
    }

    /// Execute a [`RestorePlan`]. See `prov_history::HistoryStore::restore`.
    pub async fn history_restore(
        &mut self,
        root_doc: &Path,
        plan: &RestorePlan,
        force: bool,
    ) -> Result<()> {
        self.history_store_mut()
            .restore(root_doc, plan, force)
            .await
    }

    /// Execute a [`Pruned`] plan. See `prov_history::HistoryStore::prune`.
    pub async fn history_prune(&mut self, root_doc: &Path, plan: &Pruned) -> Result<()> {
        self.history_store_mut().prune(root_doc, plan).await
    }

    /// Destroy the captured bytes of one document, and record that it was
    /// deliberate. See `prov_history::HistoryStore::forget`.
    pub async fn history_forget(
        &mut self,
        root_doc: &Path,
        subject: &Subject,
        now: &str,
        force: bool,
    ) -> Result<Forgotten> {
        self.history_store_mut()
            .forget(root_doc, subject, now, force)
            .await
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests;
