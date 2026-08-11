use std::collections::BTreeSet;
use std::path::Path;

use crate::change::ChangeSet;
use crate::workspace::Workspace;
use prov_graph::document::MetaCarrier;
use prov_graph::error::{Error, Result};
use prov_graph::fs::Storage;
use prov_graph::index::IndexStore;
use prov_graph::link;

impl<FS: Storage, IdP, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Stage an index write only when it would change the file — see
    /// [`history_prune`](Self::history_prune) on why a prune must not churn
    /// indexes it has no reason to touch.
    pub(super) async fn stage_index_text(
        &self,
        cs: &mut ChangeSet,
        index: &Path,
        text: String,
    ) -> Result<()> {
        let unchanged = matches!(self.load(index).await, Ok((current, _)) if current == text);
        if !unchanged {
            cs.write(index, text);
        }
        Ok(())
    }

    /// Stage the removal of an index whose directory no longer holds any event —
    /// but only if it is actually there.
    pub(super) async fn stage_index_removal(&self, cs: &mut ChangeSet, index: &Path) -> Result<()> {
        if self.exists(index).await? {
            cs.remove(index);
        }
        Ok(())
    }

    /// A captured root document's text, with its `history` pointer restored if the
    /// capture carried none — the one edit a restore makes to bytes it is putting
    /// back verbatim.
    ///
    /// Absence is the only case it corrects. A pointer naming some *other* store
    /// index is what the workspace looked like at that capture, and rewriting it
    /// would be the restore substituting its own opinion for the manifest's.
    pub(super) fn rooted_at_store(
        &self,
        root_doc: &Path,
        text: &str,
        store_index: &Path,
    ) -> Result<String> {
        let Some(relation) = self.relations().history_relation() else {
            return Ok(text.to_string());
        };
        let relation = relation.to_string();
        let doc = prov_graph::document::Document::parse(root_doc, text)?;
        if doc.meta.get(&relation).is_some() {
            return Ok(text.to_string());
        }
        self.with_history_pointer(root_doc, text, doc.carrier, store_index)
    }

    /// The root document's text with its `history` pointer at the store index —
    /// authored the first time only, as a plain relative path (the same shape
    /// `recycle` gives the bin pointer), comment- and format-preservingly.
    ///
    /// Computed rather than staged directly so the capture can hash *this* text
    /// into its own manifest, and so the pointer still lands in the same
    /// [`ChangeSet`](crate::change::ChangeSet) as the event — a store written without the pointer would be
    /// unreachable, and invisible to `check`.
    pub(crate) async fn history_pointer_text(
        &self,
        root_doc: &Path,
        store_index: &Path,
    ) -> Result<String> {
        let (text, doc) = self.load(root_doc).await?;
        self.with_history_pointer(root_doc, &text, doc.carrier, store_index)
    }

    /// `text` — a root document's — with its `history` pointer set to
    /// `store_index`. Text in, text out: the capture edits the root it is about to
    /// hash, and the restore edits a root it is about to write back out of a blob,
    /// neither of which is what is on disk.
    fn with_history_pointer(
        &self,
        root_doc: &Path,
        text: &str,
        carrier: Option<MetaCarrier>,
        store_index: &Path,
    ) -> Result<String> {
        let relation = self
            .relations()
            .history_relation()
            .ok_or_else(|| Error::Structure("no history relation configured".into()))?
            .to_string();
        let root_dir = root_doc.parent().unwrap_or(Path::new(""));
        let pointer = link::relative(root_dir, store_index);
        prov_graph::edit::set_in_text(
            text,
            carrier,
            &relation,
            prov_graph::edit::infer_scalar(&pointer),
        )
    }

    /// The event ids in one shard directory. See
    /// `prov_history::HistoryStore::shard_event_ids`.
    pub(super) async fn shard_event_ids(
        &self,
        shard: &Path,
        ext: &str,
    ) -> Result<BTreeSet<String>> {
        self.history_store().shard_event_ids(shard, ext).await
    }

    /// The immediate subdirectory names of `dir`, sorted. See
    /// `prov_history::HistoryStore::subdirs`.
    pub(super) async fn subdirs(&self, dir: &Path) -> Result<BTreeSet<String>> {
        self.history_store().subdirs(dir).await
    }

    /// The text one history index document *should* hold, rebuilt from the
    /// directory it describes. See `prov_history::HistoryStore::index_text`.
    pub async fn history_index_text(&self, index: &Path) -> Result<String> {
        self.history_store().index_text(index).await
    }
}
