use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use prov_graph::error::Result;

use super::docs::forgotten_hashes;
use super::layout::store_dir;
use super::{FORGOTTEN_STEM, HistoryReadHost, HistoryStore};

impl<H: HistoryReadHost> HistoryStore<H> {
    /// Where the store's tombstone list lives, and whether it is there.
    ///
    /// Located by **stem**, not by the workspace's current metadata format: a
    /// workspace that switched formats after a forget must not lose track of what
    /// it destroyed, and a record of destruction is the last thing that should go
    /// quiet because a setting changed.
    pub async fn forgotten_path(&self, store_index: &Path) -> Result<(PathBuf, bool)> {
        let dir = store_dir(store_index);
        if let Ok(entries) = self.host().graph().listing(&dir).await {
            for entry in entries {
                let Some(name) = entry.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if entry.file_type().is_file()
                    && Path::new(name).file_stem().and_then(|s| s.to_str()) == Some(FORGOTTEN_STEM)
                {
                    return Ok((dir.join(name), true));
                }
            }
        }
        let ext = prov_graph::document::whole_file_extension(self.host().default_embed_format());
        Ok((dir.join(format!("{FORGOTTEN_STEM}.{ext}")), false))
    }

    /// The tombstone list's path when the store has one — what a store index has
    /// to link so the record of what was destroyed is not itself an orphan.
    pub async fn forgotten_link(&self, store_index: &Path) -> Result<Option<PathBuf>> {
        let (path, present) = self.forgotten_path(store_index).await?;
        Ok(present.then_some(path))
    }

    /// The hashes this store has deliberately destroyed.
    ///
    /// The tombstone is what turns "these bytes are missing" into "these bytes
    /// are accounted for": `Finding::HistoryBlobMissing` skips a hash on this
    /// list, and the read verbs label its rows *forgotten* rather than lost.
    /// Events stay immutable — nothing rewrites a manifest — so the record of
    /// **what was captured** survives the destruction of the bytes, which is the
    /// honest bargain and has to be stated as one.
    ///
    /// Empty when there is no store, or nothing has been forgotten.
    pub async fn forgotten(&self, root_doc: &Path) -> Result<BTreeSet<String>> {
        let (store_index, found) = self.store_index(root_doc).await?;
        if !found.exists() {
            return Ok(BTreeSet::new());
        }
        let (path, present) = self.forgotten_path(&store_index).await?;
        if !present {
            return Ok(BTreeSet::new());
        }
        let Ok((_, doc)) = self.host().graph().load(&path).await else {
            return Ok(BTreeSet::new());
        };
        Ok(forgotten_hashes(&doc.meta))
    }
}
