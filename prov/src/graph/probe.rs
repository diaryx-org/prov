//! Raw filesystem probes — root-join and delegate, nothing else.
//!
//! Distinct from [`load`](super::load): these do not clamp against root
//! escape, do not consult or populate the read-scope memo, and (for
//! [`read_bytes`](Workspace::read_bytes)/[`read_text`](Workspace::read_text))
//! do not parse. They exist so that every module outside `graph` reaches the
//! filesystem through `Workspace` rather than holding a [`Storage`] handle of
//! its own — see the module doc at [`crate::graph`]. A caller that needs the
//! clamp (any path that can originate in a document's own metadata) wants
//! [`load`](super::load) instead.

use std::path::Path;

use crate::error::Result;
use crate::fs::{DirEntry, Metadata, Storage};
use crate::workspace::Workspace;

impl<FS: Storage, Id, Ix> Workspace<FS, Id, Ix> {
    /// Whether the workspace-relative `path` exists. Mirrors
    /// [`Storage::try_exists`], joined to the workspace root.
    pub(crate) async fn exists(&self, path: &Path) -> Result<bool> {
        Ok(self.fs().try_exists(&self.root().join(path)).await?)
    }

    /// Read the entire contents of the workspace-relative `path` as bytes.
    /// Mirrors [`Storage::read`], joined to the workspace root.
    pub(crate) async fn read_bytes(&self, path: &Path) -> Result<Vec<u8>> {
        Ok(self.fs().read(&self.root().join(path)).await?)
    }

    /// Read the entire contents of the workspace-relative `path` as a
    /// string. Mirrors [`Storage::read_to_string`], joined to the workspace
    /// root.
    pub(crate) async fn read_text(&self, path: &Path) -> Result<String> {
        Ok(self.fs().read_to_string(&self.root().join(path)).await?)
    }

    /// List the entries of the workspace-relative directory `path`. Mirrors
    /// [`Storage::read_dir`], joined to the workspace root.
    pub(crate) async fn listing(&self, path: &Path) -> Result<Vec<DirEntry>> {
        Ok(self.fs().read_dir(&self.root().join(path)).await?)
    }

    /// Metadata about the entry at the workspace-relative `path`. Mirrors
    /// [`Storage::metadata`], joined to the workspace root.
    pub(crate) async fn stat(&self, path: &Path) -> Result<Metadata> {
        Ok(self.fs().metadata(&self.root().join(path)).await?)
    }
}
