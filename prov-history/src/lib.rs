//! The history bounded context.
//!
//! This crate owns history's diagnostics and host boundary. The filesystem
//! verbs are being moved here in compatible slices; `prov` currently supplies
//! the forwarding surface while this crate stays independent of `prov`.

use std::path::PathBuf;

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
