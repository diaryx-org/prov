use std::collections::BTreeSet;
use std::path::Path;

use crate::change::ChangeSet;
use crate::document::MetaCarrier;
use crate::error::{Error, Result};
use crate::fs::Storage;
use crate::index::IndexStore;
use crate::link;
use crate::workspace::Workspace;

use super::EVENTS_DIR;
use super::docs::*;
use super::layout::*;

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
        if self.fs().try_exists(&self.root().join(index)).await? {
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
        let doc = crate::document::Document::parse(root_doc, text)?;
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
    pub(super) async fn history_pointer_text(
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
        crate::edit::set_in_text(
            text,
            carrier,
            &relation,
            crate::edit::infer_scalar(&pointer),
        )
    }

    /// The event ids in one shard directory: every `*.<ext>` file that is not the
    /// shard's own index. Directory-driven, so it sees exactly what is there.
    pub(super) async fn shard_event_ids(
        &self,
        shard: &Path,
        ext: &str,
    ) -> Result<BTreeSet<String>> {
        let suffix = format!(".{ext}");
        let index = format!("index.{ext}");
        let mut ids = BTreeSet::new();
        let Ok(entries) = self.fs().read_dir(&self.root().join(shard)).await else {
            return Ok(ids);
        };
        for entry in entries {
            let Some(name) = entry.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !entry.file_type().is_file() || name.starts_with('.') || name == index {
                continue;
            }
            if let Some(stem) = name.strip_suffix(&suffix)
                && is_event_id(stem)
            {
                ids.insert(stem.to_string());
            }
        }
        Ok(ids)
    }

    /// The immediate subdirectory names of `dir`, sorted. An unreadable or absent
    /// directory is empty, not an error — the store is grown lazily.
    pub(super) async fn subdirs(&self, dir: &Path) -> Result<BTreeSet<String>> {
        let mut names = BTreeSet::new();
        let Ok(entries) = self.fs().read_dir(&self.root().join(dir)).await else {
            return Ok(names);
        };
        for entry in entries {
            let Some(name) = entry.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if entry.file_type().is_dir() && !name.starts_with('.') {
                names.insert(name.to_string());
            }
        }
        Ok(names)
    }

    /// The text one history index document *should* hold, rebuilt from the
    /// directory it describes — the repair behind [`Fix::RebuildHistoryIndex`].
    ///
    /// Takes only the index's own path: which of the three index kinds it is
    /// falls out of where it sits relative to the store's `events/` directory, so
    /// the repair needs neither the root document nor the `history` pointer —
    /// which matters, because a workspace whose *store index* was mangled is
    /// exactly when you want to rebuild without depending on it.
    ///
    /// Per-shard by construction: a mangled `2026/07/index.<ext>` is rebuilt from
    /// that one directory's listing, touching no other month.
    ///
    /// [`Fix::RebuildHistoryIndex`]: crate::Fix::RebuildHistoryIndex
    pub async fn history_index_text(&self, index: &Path) -> Result<String> {
        let index = link::normalize(index);
        let ext = index
            .extension()
            .and_then(|e| e.to_str())
            .ok_or_else(|| Error::Structure(format!("{} has no extension", index.display())))?;
        let embed = self.history_embed()?;
        let dir = index.parent().unwrap_or(Path::new(""));

        // Locate the store's `events/` directory by name, walking up from the
        // index. Its absence means this *is* the store index.
        let depth_below_events = dir
            .components()
            .rev()
            .position(|c| c.as_os_str() == EVENTS_DIR);
        match depth_below_events {
            // `<store>/events/<year>/<month>/index.<ext>`
            Some(2) => {
                let (year, month) = shard_parts(
                    dir.parent()
                        .and_then(|p| p.parent())
                        .map(|events| dir.strip_prefix(events).unwrap_or(dir))
                        .unwrap_or(dir),
                )?;
                render_month_index(
                    &year,
                    &month,
                    &self.shard_event_ids(dir, ext).await?,
                    ext,
                    embed,
                )
            }
            // `<store>/events/<year>/index.<ext>`
            Some(1) => {
                let year = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                render_year_index(&year, &self.event_months(dir, ext).await?, ext, embed)
            }
            // `<store>/index.<ext>` — the store index itself.
            _ => render_store_index(
                &self.event_years(&dir.join(EVENTS_DIR), ext).await?,
                ext,
                self.history_forgotten_link(&index).await?.as_deref(),
                embed,
            ),
        }
    }
}
