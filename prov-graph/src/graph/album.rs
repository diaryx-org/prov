//! Reading albums off the graph: the album a node declares, and the manifest
//! it picks from. The model is [`crate::album`].

use std::path::{Path, PathBuf};

use super::Graph;
use super::resolve::Target;
use crate::album::{Album, PICKS_FROM_KEY};
use crate::error::{Error, Result};
use crate::fs::ReadStorage;
use crate::index::IdIndex;
use crate::link::Link;
use crate::manifest::Manifest;

/// The manifest an album picks from: its node, its manifest document and the
/// manifest itself, all workspace-relative.
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumSource {
    /// The manifest node `picks_from` links to.
    pub node: PathBuf,
    /// The manifest document that node declares.
    pub manifest_doc: PathBuf,
    /// The manifest, parsed.
    pub manifest: Manifest,
}

impl AlbumSource {
    /// The source's row for `hash` (`sha256:<hex>`), if it lists one.
    pub fn row(&self, hash: &str) -> Option<&crate::manifest::ManifestEntry> {
        self.manifest
            .files
            .iter()
            .find(|row| row.hash.as_deref() == Some(hash))
    }
}

/// Why an album's source could not be read — split so that `check` can leave
/// a link that resolves to nothing to the census, which already reports it.
#[derive(Debug)]
pub enum SourceError {
    /// `picks_from` names nothing in this workspace: a broken link, reported
    /// where links are.
    Unresolved,
    /// It names something that is not a manifest node with hashed rows, or a
    /// manifest that cannot be read: the album's own problem.
    Malformed(Error),
}

impl<FS: ReadStorage, Ix: IdIndex> Graph<FS, Ix> {
    /// The album `node` declares, parsed. `None` when it declares neither
    /// `picks_from` nor `picks`.
    pub async fn album_of(&self, node: &Path) -> Result<Option<Album>> {
        let read = self.read(node).await?;
        let album = Album::from_meta(&read.doc.meta)
            .map_err(|e| Error::Structure(format!("{}: {e}", node.display())))?;
        if album.is_some() && read.doc.manifest_attr().is_some() {
            return Err(Error::Structure(format!(
                "{}: declares both `manifest` and `{PICKS_FROM_KEY}`: a node covers a directory or picks from one, not both",
                node.display()
            )));
        }
        Ok(album)
    }

    /// The manifest the album at `node` picks from.
    ///
    /// The source must be a manifest node whose rows carry hashes, since a
    /// pick names a row by its digest. A held source is as good as one whose
    /// files are here: an album of a held archive is read on a device that has
    /// none of its files, which is the case it exists for.
    pub async fn album_source(
        &self,
        node: &Path,
        album: &Album,
    ) -> std::result::Result<AlbumSource, SourceError> {
        let link = Link::parse(&album.picks_from);
        let source = match self.resolve_link(node, &link) {
            Target::Path(path) => path,
            _ => return Err(SourceError::Unresolved),
        };
        if !self.exists(&source).await.unwrap_or(false) {
            return Err(SourceError::Unresolved);
        }
        let malformed = |why: String| SourceError::Malformed(Error::Structure(why));
        let (manifest_doc, manifest) = match self.manifest_of(&source).await {
            Ok(Some(found)) => found,
            Ok(None) => {
                return Err(malformed(format!(
                    "`{PICKS_FROM_KEY}` links to {}, which is not a manifest node",
                    source.display()
                )));
            }
            Err(e) => return Err(SourceError::Malformed(e)),
        };
        if !manifest.files.is_empty() && manifest.files.iter().all(|row| row.hash.is_none()) {
            return Err(malformed(format!(
                "{} lists no hashes, so nothing in it can be picked",
                manifest_doc.display()
            )));
        }
        Ok(AlbumSource {
            node: source,
            manifest_doc,
            manifest,
        })
    }
}
