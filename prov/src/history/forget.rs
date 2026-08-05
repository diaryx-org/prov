use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::fs::Storage;
use crate::index::IndexStore;
use crate::link;
use crate::workspace::Workspace;

use super::docs::*;
use super::layout::*;
use super::model::*;
use super::paths::*;
use super::{EVENTS_DIR, FORGOTTEN_STEM};

impl<FS: Storage, IdP, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Where the store's tombstone list lives, and whether it is there.
    ///
    /// Located by **stem**, not by the workspace's current metadata format: a
    /// workspace that switched formats after a forget must not lose track of what
    /// it destroyed, and a record of destruction is the last thing that should go
    /// quiet because a setting changed.
    async fn history_forgotten_path(&self, store_index: &Path) -> Result<(PathBuf, bool)> {
        let dir = store_dir(store_index);
        if let Ok(entries) = self.fs().read_dir(&self.root().join(&dir)).await {
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
        let ext = crate::document::whole_file_extension(self.default_embed_format());
        Ok((dir.join(format!("{FORGOTTEN_STEM}.{ext}")), false))
    }

    /// The tombstone list's path when the store has one — what a store index has
    /// to link so the record of what was destroyed is not itself an orphan.
    pub(super) async fn history_forgotten_link(
        &self,
        store_index: &Path,
    ) -> Result<Option<PathBuf>> {
        let (path, present) = self.history_forgotten_path(store_index).await?;
        Ok(present.then_some(path))
    }

    /// The hashes this store has deliberately destroyed.
    ///
    /// The tombstone is what turns "these bytes are missing" into "these bytes are
    /// accounted for": [`Finding::HistoryBlobMissing`](crate::validate::Finding::HistoryBlobMissing) skips a hash on this list,
    /// and the read verbs label its rows *forgotten* rather than lost. Events stay
    /// immutable — nothing rewrites a manifest — so the record of **what was
    /// captured** survives the destruction of the bytes, which is the honest
    /// bargain and has to be stated as one.
    ///
    /// Empty when there is no store, or nothing has been forgotten.
    pub async fn history_forgotten(&self, root_doc: &Path) -> Result<BTreeSet<String>> {
        let (store_index, exists) = self.history_store_index(root_doc).await?;
        if !exists {
            return Ok(BTreeSet::new());
        }
        let (path, present) = self.history_forgotten_path(&store_index).await?;
        if !present {
            return Ok(BTreeSet::new());
        }
        let Ok((_, doc)) = self.load(&path).await else {
            return Ok(BTreeSet::new());
        };
        Ok(forgotten_hashes(&doc.meta))
    }

    /// Destroy the captured bytes of one document, and record that it was
    /// deliberate.
    ///
    /// The counterpart to the retention this store creates. A document's bytes
    /// normally end at `empty_bin` or `rm --purge`; with history on, any event
    /// that captured it while it was live still holds them, and `history-restore`
    /// brings them back. This is the tool that makes that reversible act
    /// irreversible on purpose, and the full-manifest design is what makes it
    /// tractable: every hash a document ever had is a column lookup, not a fold.
    ///
    /// ## What it destroys, and what it cannot
    ///
    /// - **Only bytes nothing else names.** A hash the subject shares with another
    ///   captured path survives, and is reported in
    ///   [`shared`](Forgotten::shared). Content addressing means forgetting one
    ///   document cannot reach into another's history — which is a safety property
    ///   and a limit in the same breath.
    /// - **Bytes, not the record.** Event documents are immutable, so every
    ///   manifest still names the path, the id and the hash. If what must
    ///   disappear is the *name*, this is not that tool, and no amount of wording
    ///   should let a user believe otherwise.
    ///
    /// ## Why it refuses a live document
    ///
    /// Forgetting the captured bytes of a document still in the workspace is very
    /// nearly a no-op: the next capture parks them again. `force` proceeds anyway,
    /// for the deliberate "purge the history, keep the file" case.
    ///
    /// ## Ordering
    ///
    /// The tombstone is written and committed **before** the bytes are freed —
    /// write-ahead, like every other mutation here. `now` is the caller's
    /// timestamp, since the library keeps no clock.
    ///
    /// Blobs are deleted outside the change set for
    /// [`history_prune`](Self::history_prune)'s reason: a staged removal buffers
    /// the bytes it deletes in order to be able to put them back, which is the one
    /// thing a destruction verb must not do.
    ///
    /// A crash between the two leaves a hash tombstoned whose blob is still
    /// present. Re-running the same forget finishes the job. It is the one residue
    /// this ordering can leave, and it is the quiet one — which is the tradeoff
    /// write-ahead always makes, and worth knowing rather than worth reversing:
    /// destroying bytes before recording the intent would be the alternative.
    pub async fn history_forget(
        &mut self,
        root_doc: &Path,
        subject: &Subject,
        now: &str,
        force: bool,
    ) -> Result<Forgotten> {
        let root_doc = link::normalize(root_doc);
        let (store_index, exists) = self.history_store_index(&root_doc).await?;
        if !exists {
            return Ok(Forgotten::default());
        }
        let ext = self.history_ext(&root_doc);
        let embed = self.history_embed()?;

        // The next capture would park them again, so this would be theatre. Named
        // rather than merely refused: the user has to know *which* document, and
        // what to do about it.
        if !force && let Some(live) = self.history_subject_live(&root_doc, subject).await? {
            return Err(Error::Structure(format!(
                "{} is still in the workspace — the next capture would park its bytes \
                 again. Remove it first (`prov rm --purge`), or force this to forget \
                 the captured copies only",
                live.display()
            )));
        }

        // Every hash the subject ever had, and every hash anything *else* ever
        // had. The difference is what can go — a set subtraction, where a delta
        // log would need the ancestry folded per event to answer the same
        // question.
        //
        // **Refuses if any event document fails to load or parse.** `others` is
        // built only from the events that parsed, so an unreadable event's
        // manifest is invisible to it — a hash that event alone shared with the
        // subject would read as belonging only to the subject, and forget would
        // destroy bytes a different, unreadable document's history still names.
        // The safe default is to stop and name the file, not to guess.
        let (events, unreadable) = self.history_events_in(&store_index, ext).await?;
        if !unreadable.is_empty() {
            let named = match subject {
                Subject::Id(id) => format!("id:{id}"),
                Subject::Path(path) => slash_path(path),
            };
            return Err(Error::Structure(format!(
                "history-forget refuses: {} event document(s) could not be read, so which \
                 other documents share {named}'s bytes cannot be determined: {}. Repair or \
                 restore them (or let the transport finish syncing) before forgetting.",
                unreadable.len(),
                Self::describe_unreadable(&unreadable)
            )));
        }
        let (mut mine, mut others) = (BTreeSet::new(), BTreeSet::new());
        for event in events {
            for file in event.files {
                match subject_matches(subject, &file) {
                    true => mine.insert(file.hash),
                    false => others.insert(file.hash),
                };
            }
        }
        let shared: Vec<String> = mine.intersection(&others).cloned().collect();
        mine.retain(|hash| !others.contains(hash));
        if mine.is_empty() {
            return Ok(Forgotten {
                shared,
                ..Forgotten::default()
            });
        }
        others.clear();

        let mut blobs = Vec::new();
        let mut bytes = 0u64;
        for hash in &mine {
            let Ok(blob) = blob_path(&store_index, hash) else {
                continue;
            };
            if self.fs().try_exists(&self.root().join(&blob)).await? {
                bytes += match self.fs().metadata(&self.root().join(&blob)).await {
                    Ok(meta) => meta.len(),
                    Err(_) => 0,
                };
                blobs.push(blob);
            }
        }

        // The tombstone, re-rendered whole — a machine file, and the one mutable
        // document in the store besides the indexes. It can conflict under sync,
        // which is acceptable for an explicitly invoked, rare act of destruction.
        let (forgotten_path, present) = self.history_forgotten_path(&store_index).await?;
        let existing = match present {
            true => self
                .load(&forgotten_path)
                .await
                .ok()
                .map(|(_, doc)| doc.meta),
            false => None,
        };
        let text = render_forgotten(
            existing.as_ref(),
            &mine,
            subject,
            now,
            self.default_embed_format(),
        )?;

        let mut cs = self.change();
        cs.write(&forgotten_path, text);
        // The list has to be reachable, or `check` reports the record of what was
        // destroyed as an orphan. The store index is the only thing above it.
        let years = self
            .event_years(&store_dir(&store_index).join(EVENTS_DIR), ext)
            .await?;
        self.stage_index_text(
            &mut cs,
            &store_index,
            render_store_index(&years, ext, Some(&forgotten_path), embed)?,
        )
        .await?;
        self.commit(cs).await?;

        for blob in &blobs {
            let full = self.root().join(blob);
            if self.fs().try_exists(&full).await? {
                self.fs().remove_file(&full).await?;
            }
        }
        Ok(Forgotten {
            hashes: mine.into_iter().collect(),
            blobs,
            bytes,
            shared,
        })
    }

    /// The subject's live path, when the next capture would park its bytes again.
    ///
    /// Tested against the **capture set** rather than mere existence on disk,
    /// because that is exactly the population a capture parks — a file sitting
    /// unreachable in the tree would not come back, and refusing on its account
    /// would be refusing for a reason that is not true.
    async fn history_subject_live(
        &self,
        root_doc: &Path,
        subject: &Subject,
    ) -> Result<Option<PathBuf>> {
        let path = match subject {
            Subject::Path(path) => link::normalize(path),
            Subject::Id(id) => match self.index().resolve(id) {
                Some(path) => link::normalize(path),
                None => return Ok(None),
            },
        };
        Ok(self
            .history_capture_set(root_doc)
            .await?
            .into_iter()
            .find(|captured| *captured == path))
    }
}
