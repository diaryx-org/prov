use std::path::Path;

use crate::workspace::Workspace;
use prov_graph::error::Result;
use prov_graph::fs::Storage;
use prov_graph::index::IndexStore;

impl<FS: Storage, IdP, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// The root document's text with its `history` pointer at the store index.
    /// See `prov_history::HistoryStore::pointer_text`.
    pub(crate) async fn history_pointer_text(
        &self,
        root_doc: &Path,
        store_index: &Path,
    ) -> Result<String> {
        self.history_store()
            .pointer_text(root_doc, store_index)
            .await
    }

    /// The text one history index document *should* hold, rebuilt from the
    /// directory it describes. See `prov_history::HistoryStore::index_text`.
    pub async fn history_index_text(&self, index: &Path) -> Result<String> {
        self.history_store().index_text(index).await
    }
}
