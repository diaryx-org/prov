//! The history bounded context.
//!
//! This crate owns the whole feature: the vocabulary an event is spelled in,
//! the store's shape on disk, and every verb — `capture`, `restore`, `prune`,
//! `forget`, `check`, and the reads. It stays independent of `prov`, reaching
//! its host through two traits ([`HistoryReadHost`], [`HistoryWriteHost`])
//! rather than through a concrete workspace, and `prov` supplies a forwarding
//! surface so existing call sites are unchanged.
//!
//! The modules are split by *what a reader is after*, not by type:
//!
//! - `model` — an [`Event`] and its manifest rows, and the outcome of each
//!   verb.
//! - `layout` — the store's shape on disk: id ⇄ shard, hash → blob.
//! - `event_id` — the canonical form an event's id is a digest of, and the
//!   timestamp arithmetic that keeps ids orderable across precisions.
//! - `paths` — the path and manifest helpers the canonical form is built from.
//! - `docs` — the store's own documents: parsing an event's frontmatter, and
//!   rendering the rebuildable index and tombstone caches.
//! - `read`, `capture`, `restore`, `prune`, `forget`, `check` — one module per
//!   verb, each an `impl HistoryStore` block.
//! - `store` — the plumbing those verbs share: staging an index write, the
//!   root's `history` pointer, walking shards.
//!
//! Every one of them is flat-re-exported at the crate root, so a caller writes
//! `prov_history::Event` rather than naming the file it happens to live in.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use prov_graph::document::EmbedStyle;
use prov_graph::error::Result;
use prov_graph::fs::{Metadata, ReadStorage};
use prov_graph::graph::Graph;
use prov_graph::identity::Id;
use prov_graph::index::Collision;
use prov_graph::link::LinkStyle;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;
use prov_transaction::ChangeSet;

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

#[cfg(all(test, feature = "yaml"))]
mod tests;

pub use docs::*;
pub use event_id::*;
pub use layout::*;
pub use model::*;
pub use paths::*;
pub use read::describe_unreadable;

/// Read capabilities a host must supply for [`HistoryStore`]'s read-side verbs.
///
/// Deliberately narrow: "capabilities, not mirrored methods". Everything a verb
/// needs beyond these is reached through [`graph`](Self::graph) — `load`,
/// `exists`, `read_bytes`, `listing`, `stat`, the index — rather than being
/// re-declared here one by one. What is declared is what the *host* knows and
/// the store cannot work out for itself: how this workspace authors metadata,
/// where it parks bytes, and which of its own policies history has to defer to.
pub trait HistoryReadHost {
    /// The filesystem backend the host's [`Graph`] reads through.
    type Fs: ReadStorage;
    /// The index store the host's [`Graph`] resolves ids through.
    type Ix: IndexStore;

    /// The read core: document loads, raw existence/listing/stat probes.
    fn graph(&self) -> &Graph<Self::Fs, Self::Ix>;

    /// How this workspace embeds metadata (family) and which format it defaults to.
    fn embed_style(&self) -> EmbedStyle;
    /// The metadata format a new document gets when it inherits no parent block.
    fn default_embed_format(&self) -> fig::Format;

    /// Whether this workspace's `history` axis is on at all (gates `StoreUnlinked`).
    fn history_captures(&self) -> bool;

    /// The name of the `history` pointer relation in this workspace's
    /// vocabulary, if it configures one. `None` means a store cannot be
    /// declared here at all, which is why a capture reports it rather than
    /// silently writing an unreachable store.
    fn history_relation(&self) -> Option<&str>;

    /// The path style prov authors the `history` pointer relation in — root-
    /// absolute or `../`-relative, this workspace's own axis. The store defers
    /// to it rather than assuming a shape, so a pointer it authors resolves the
    /// same way every other structural link in the workspace does.
    fn history_link_style(&self) -> LinkStyle;

    /// The history-store index document this root declares via its pointer
    /// relation, if any. `None` when there is no relation configured or the
    /// root declares none.
    fn history_path(&self, root_doc: &Path) -> impl Future<Output = Result<Option<PathBuf>>>;

    /// Every file the workspace reaches from `root_doc` that is on disk — §8's
    /// bounded walk, the population `check` validates and the capture set is
    /// drawn from.
    ///
    /// Not `graph().reachable_files`, which is the *unparked* walk: the host's
    /// own version knows which directories are byte-parking store interiors and
    /// declines to descend them, so a capture does not read a thousand event
    /// documents in order to discard them.
    fn reachable_files(&self, root_doc: &Path) -> impl Future<Output = Result<BTreeSet<PathBuf>>>;

    /// What the capture set excludes **beyond the store itself**, as path
    /// prefixes (a file names only itself).
    ///
    /// The store's own exclusion is history's business and is applied here; this
    /// is the knowledge history does not have — where the recycle bin parks its
    /// items, and which page is derived rather than authored. See
    /// [`HistoryStore::capture_set`] for why each one is load-bearing.
    fn history_exclusions(&self, root_doc: &Path) -> impl Future<Output = Result<Vec<PathBuf>>>;

    /// Whether registering `id` at `path` would displace a registration the
    /// index already holds.
    ///
    /// A *policy* rather than an index probe — both directions, and the
    /// already-registered pair discounted — which the host shares with the
    /// recycle bin's own re-registration of a restored document's id. History
    /// defers to it instead of re-deriving it from `graph().index()`, so the two
    /// verbs that carry an id in from outside cannot drift apart.
    fn registration_conflict(&self, id: &Id, path: &Path) -> Option<Collision>;
}

/// The write capabilities on top of [`HistoryReadHost`] that history's four
/// mutating verbs need.
///
/// Only what a host can do that history cannot do for itself. Parking a blob,
/// discarding one, and writing a case probe are all plain
/// [`prov_transaction`] calls against `graph().fs()` and `graph().root()`, so
/// they are absent here; what is left is the transaction boundary — a host's
/// [`ChangeSet`] does not merely apply, it also persists the index, stamps
/// pending ids, invalidates memos and rolls back — and the fixity cache, which
/// belongs to the host because the host is what persists it.
///
/// Implementing this grants history the ability to open and land change sets on
/// the implementor, which is the whole point and is worth naming: a host offers
/// this trait deliberately, not incidentally.
pub trait HistoryWriteHost: HistoryReadHost<Fs: Storage> {
    /// Open a change set for a mutation — including whatever checkpoint the
    /// host's index needs in order to unwind if the op fails partway.
    fn change(&mut self) -> ChangeSet;

    /// Land a staged change set as one unit, with everything else the host
    /// attaches to a commit.
    ///
    /// **Never** `ChangeSet::apply` — that is the write half only, and a
    /// history verb that reached for it would skip index persistence, pending-id
    /// stamps, memo invalidation, rollback and fixity-cache invalidation.
    fn commit(&mut self, cs: ChangeSet) -> impl Future<Output = Result<()>>;

    /// The remembered digest for `path`, if the host's cache still describes the
    /// file `meta` stat'ed. `None` when there is no cache, which is ordinary.
    fn fixity_cached(&self, path: &Path, meta: &Metadata) -> Option<String>;

    /// Remember that `path` hashed to `hash` at the stat `meta` describes.
    fn fixity_remember(&self, path: &Path, meta: &Metadata, hash: &str);
}

impl<T: HistoryReadHost + ?Sized> HistoryReadHost for &T {
    type Fs = T::Fs;
    type Ix = T::Ix;

    fn graph(&self) -> &Graph<Self::Fs, Self::Ix> {
        (**self).graph()
    }
    fn embed_style(&self) -> EmbedStyle {
        (**self).embed_style()
    }
    fn default_embed_format(&self) -> fig::Format {
        (**self).default_embed_format()
    }
    fn history_captures(&self) -> bool {
        (**self).history_captures()
    }
    fn history_relation(&self) -> Option<&str> {
        (**self).history_relation()
    }
    fn history_link_style(&self) -> LinkStyle {
        (**self).history_link_style()
    }
    async fn history_path(&self, root_doc: &Path) -> Result<Option<PathBuf>> {
        (**self).history_path(root_doc).await
    }
    async fn reachable_files(&self, root_doc: &Path) -> Result<BTreeSet<PathBuf>> {
        (**self).reachable_files(root_doc).await
    }
    async fn history_exclusions(&self, root_doc: &Path) -> Result<Vec<PathBuf>> {
        (**self).history_exclusions(root_doc).await
    }
    fn registration_conflict(&self, id: &Id, path: &Path) -> Option<Collision> {
        (**self).registration_conflict(id, path)
    }
}

impl<T: HistoryReadHost + ?Sized> HistoryReadHost for &mut T {
    type Fs = T::Fs;
    type Ix = T::Ix;

    fn graph(&self) -> &Graph<Self::Fs, Self::Ix> {
        (**self).graph()
    }
    fn embed_style(&self) -> EmbedStyle {
        (**self).embed_style()
    }
    fn default_embed_format(&self) -> fig::Format {
        (**self).default_embed_format()
    }
    fn history_captures(&self) -> bool {
        (**self).history_captures()
    }
    fn history_relation(&self) -> Option<&str> {
        (**self).history_relation()
    }
    fn history_link_style(&self) -> LinkStyle {
        (**self).history_link_style()
    }
    async fn history_path(&self, root_doc: &Path) -> Result<Option<PathBuf>> {
        (**self).history_path(root_doc).await
    }
    async fn reachable_files(&self, root_doc: &Path) -> Result<BTreeSet<PathBuf>> {
        (**self).reachable_files(root_doc).await
    }
    async fn history_exclusions(&self, root_doc: &Path) -> Result<Vec<PathBuf>> {
        (**self).history_exclusions(root_doc).await
    }
    fn registration_conflict(&self, id: &Id, path: &Path) -> Option<Collision> {
        (**self).registration_conflict(id, path)
    }
}

impl<T: HistoryWriteHost + ?Sized> HistoryWriteHost for &mut T {
    fn change(&mut self) -> ChangeSet {
        (**self).change()
    }
    async fn commit(&mut self, cs: ChangeSet) -> Result<()> {
        (**self).commit(cs).await
    }
    fn fixity_cached(&self, path: &Path, meta: &Metadata) -> Option<String> {
        (**self).fixity_cached(path, meta)
    }
    fn fixity_remember(&self, path: &Path, meta: &Metadata, hash: &str) {
        (**self).fixity_remember(path, meta, hash)
    }
}

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

/// Diagnostics produced by the history store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryIssue {
    /// A rebuildable index disagrees with the directory it describes.
    IndexStale {
        index: PathBuf,
        missing: Vec<PathBuf>,
        extra: Vec<PathBuf>,
    },
    /// An event manifest names bytes that are not present in the blob store.
    BlobMissing {
        store: PathBuf,
        hash: String,
        paths: Vec<PathBuf>,
    },
    /// Bytes exist in the blob store but no event manifest names them.
    BlobOrphaned { store: PathBuf, blobs: Vec<PathBuf> },
    /// The conventional store exists but the root no longer points at it.
    StoreUnlinked { root: PathBuf, store: PathBuf },
    /// An event document exists but cannot be read or parsed.
    Unreadable { doc: PathBuf, error: String },
}

/// Service handle for history operations hosted by another crate.
///
/// Generic over the host *value*, not a fixed reference, so a caller chooses
/// its own capability: `HistoryStore::new(&workspace)` for reads and planning,
/// `HistoryStore::new(&mut workspace)` for the four verbs that write (capture,
/// restore, prune, forget) — each of which lands a change set on its host,
/// which a fixed `&'a H` cannot express.
#[derive(Debug, Clone, Copy)]
pub struct HistoryStore<H> {
    host: H,
}

impl<H> HistoryStore<H> {
    pub fn new(host: H) -> Self {
        Self { host }
    }

    pub fn host(&self) -> &H {
        &self.host
    }

    /// The host, mutably — how a write verb reaches
    /// [`change`](HistoryWriteHost::change) and
    /// [`commit`](HistoryWriteHost::commit).
    pub fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }
}
