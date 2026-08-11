//! The history bounded context.
//!
//! This crate owns history's diagnostics and host boundary. The filesystem
//! verbs are being moved here in compatible slices; `prov` currently supplies
//! the forwarding surface while this crate stays independent of `prov`.

use std::path::{Path, PathBuf};

use prov_graph::document::EmbedStyle;
use prov_graph::error::Result;
use prov_graph::fs::ReadStorage;
use prov_graph::graph::Graph;
use prov_graph::index::IndexStore;

mod check;
mod docs;
mod event_id;
mod forget;
mod layout;
mod model;
mod paths;
mod prune;
mod read;
mod store;

pub use docs::*;
pub use event_id::*;
pub use layout::*;
pub use model::*;
pub use paths::*;
pub use read::describe_unreadable;

/// Read capabilities a host must supply for `HistoryStore`'s read-side verbs.
///
/// Deliberately narrow: "capabilities, not mirrored methods". Everything a
/// moved verb needs beyond these four is reached through
/// [`graph`](Self::graph) — `load`, `exists`, `read_bytes`, `listing`, `stat` —
/// rather than being re-declared here one by one.
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

    /// The history-store index document this root declares via its pointer
    /// relation, if any. `None` when there is no relation configured or the
    /// root declares none.
    fn history_path(&self, root_doc: &Path) -> impl Future<Output = Result<Option<PathBuf>>>;
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
    async fn history_path(&self, root_doc: &Path) -> Result<Option<PathBuf>> {
        (**self).history_path(root_doc).await
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
/// its own capability: `HistoryStore::new(&workspace)` for reads and
/// planning, `HistoryStore::new(&mut workspace)` once a verb needs to write.
/// The verbs that will live behind this (capture, restore, prune, forget) all
/// need to mutate their host, which a fixed `&'a H` cannot express.
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
}
