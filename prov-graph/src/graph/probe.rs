//! Raw filesystem probes — root-join and delegate, nothing else.
//!
//! Distinct from [`load`](super::load): these do not clamp against root
//! escape, do not consult or populate the read-scope memo, and (for
//! [`read_bytes`](Graph::read_bytes)/[`read_text`](Graph::read_text))
//! do not parse. They exist so that every module outside `graph` reaches the
//! filesystem through `Workspace` rather than holding a [`ReadStorage`] handle of
//! its own — see the module doc at [`crate::graph`]. A caller that needs the
//! clamp (any path that can originate in a document's own metadata) wants
//! [`load`](super::load) instead.

use std::path::Path;

use super::Graph;
use crate::error::Result;
use crate::fs::{DirEntry, Metadata, ReadStorage};

impl<FS: ReadStorage, Ix> Graph<FS, Ix> {
    /// Whether the workspace-relative `path` exists. Mirrors
    /// [`ReadStorage::try_exists`], joined to the workspace root.
    pub async fn exists(&self, path: &Path) -> Result<bool> {
        Ok(self.fs().try_exists(&self.root().join(path)).await?)
    }

    /// Read the entire contents of the workspace-relative `path` as bytes.
    /// Mirrors [`ReadStorage::read`], joined to the workspace root.
    pub async fn read_bytes(&self, path: &Path) -> Result<Vec<u8>> {
        Ok(self.fs().read(&self.root().join(path)).await?)
    }

    /// The fixity digest of the workspace-relative `path`'s bytes, holding no
    /// more of the file at once than
    /// [`STREAM_CHUNK`](crate::fixity::STREAM_CHUNK) when the backend can read
    /// it in pieces ([`ReadStorage::read_chunks`]).
    pub async fn digest_file(&self, path: &Path) -> Result<String> {
        let mut hasher = crate::fixity::Hasher::new();
        self.fs()
            .read_chunks(
                &self.root().join(path),
                crate::fixity::STREAM_CHUNK,
                |piece| hasher.update(piece),
            )
            .await?;
        Ok(hasher.finish())
    }

    /// Queue the digest of the workspace-relative `path` on `digests`. A file
    /// up to [`STREAM_ABOVE`](crate::fixity::STREAM_ABOVE) is read whole and
    /// hashed on the pool while the caller reads on; a larger one is hashed a
    /// piece at a time here ([`digest_file`](Self::digest_file)), so a pass
    /// over an archive never holds a whole video in memory.
    pub async fn push_file_digest(
        &self,
        digests: &mut crate::fixity::Digests,
        path: &Path,
    ) -> Result<()> {
        if self.stat(path).await?.len() > crate::fixity::STREAM_ABOVE {
            digests.push_digest(self.digest_file(path).await?);
        } else {
            digests.push(self.read_bytes(path).await?);
        }
        Ok(())
    }

    /// The fixity digest of the workspace-relative `path`: one file's
    /// [`push_file_digest`](Self::push_file_digest), read whole when small and
    /// in pieces when not.
    pub async fn file_digest(&self, path: &Path) -> Result<String> {
        if self.stat(path).await?.len() > crate::fixity::STREAM_ABOVE {
            self.digest_file(path).await
        } else {
            Ok(crate::fixity::digest(&self.read_bytes(path).await?))
        }
    }

    /// Read the entire contents of the workspace-relative `path` as a
    /// string. Mirrors [`ReadStorage::read_to_string`], joined to the workspace
    /// root.
    pub async fn read_text(&self, path: &Path) -> Result<String> {
        Ok(self.fs().read_to_string(&self.root().join(path)).await?)
    }

    /// List the entries of the workspace-relative directory `path`. Mirrors
    /// [`ReadStorage::read_dir`], joined to the workspace root.
    pub async fn listing(&self, path: &Path) -> Result<Vec<DirEntry>> {
        Ok(self.fs().read_dir(&self.root().join(path)).await?)
    }

    /// Metadata about the entry at the workspace-relative `path`. Mirrors
    /// [`ReadStorage::metadata`], joined to the workspace root.
    pub async fn stat(&self, path: &Path) -> Result<Metadata> {
        Ok(self.fs().metadata(&self.root().join(path)).await?)
    }
}
