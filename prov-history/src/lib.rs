//! The history bounded context.
//!
//! This crate owns history's diagnostics and host boundary. The filesystem
//! verbs are being moved here in compatible slices; `prov` currently supplies
//! the forwarding surface while this crate stays independent of `prov`.

use std::path::PathBuf;

mod docs;
mod event_id;
mod layout;
mod model;
mod paths;

pub use docs::*;
pub use event_id::*;
pub use layout::*;
pub use model::*;
pub use paths::*;

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
#[derive(Debug, Clone, Copy)]
pub struct HistoryStore<'a, H: ?Sized> {
    host: &'a H,
}

impl<'a, H: ?Sized> HistoryStore<'a, H> {
    pub fn new(host: &'a H) -> Self {
        Self { host }
    }

    pub fn host(&self) -> &'a H {
        self.host
    }
}
