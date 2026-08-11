use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use prov_graph::error::{Error, Result};
use prov_graph::link;
use prov_transaction::discard_file;

use super::docs::{render_month_index, render_store_index, render_year_index};
use super::event_id::{check_cutoff, comparable};
use super::layout::{blob_path, event_path, store_dir};
use super::model::{Pruned, Retention};
use super::{EVENTS_DIR, HistoryReadHost, HistoryStore, HistoryWriteHost};

impl<H: HistoryReadHost> HistoryStore<H> {
    /// What pruning to `retention` would drop: the events, and the blobs no
    /// surviving manifest would name.
    ///
    /// Read-only. With full manifests this is delete + GC and nothing else — no
    /// folding, no re-anchoring, no rewriting of surviving events, which under the
    /// delta design was the hardest problem in the store (a dropped event's
    /// entries could be load-bearing for later events' effective state, so pruning
    /// had to rewrite an "immutable" event, the one operation that conflicts under
    /// exactly the sync this store exists to survive).
    ///
    /// The blob sweep is `Finding::HistoryBlobOrphaned`'s, taken against the
    /// survivors rather than against every event — so what `check` calls an
    /// orphan and what a prune collects are the same set by construction, and a
    /// prune sweeps up the orphans that were already there.
    ///
    /// **Refuses if any event document in the store fails to load or parse.**
    /// The `referenced` set below is a bound computed *only* over the events that
    /// parsed; if some other event is unreadable, its manifest — and every blob
    /// it might name — is invisible to that bound, and the blobs the unreadable
    /// event alone referenced would be collected as orphans and deleted. That is
    /// permanent loss from a prune whose bound silently dropped nothing. A
    /// deliberate destruction must not proceed on an incomplete reference set, so
    /// this names the unreadable file(s) and stops before planning anything.
    pub async fn prune_plan(&self, root_doc: &Path, retention: &Retention) -> Result<Pruned> {
        let root_doc = link::normalize(root_doc);
        let (store_index, found) = self.store_index(&root_doc).await?;
        if !found.exists() {
            return Ok(Pruned::default());
        }
        let (events, unreadable) = self.events_in(&store_index, self.ext(&root_doc)).await?;
        if !unreadable.is_empty() {
            return Err(Error::Structure(format!(
                "history-prune refuses: {} event document(s) could not be read, so the \
                 blobs they might reference cannot be told apart from orphans: {}. Repair \
                 or restore them (or let the transport finish syncing) before pruning.",
                unreadable.len(),
                super::read::describe_unreadable(&unreadable)
            )));
        }

        // Events arrive oldest first, so both axes cut a prefix — but `Before`
        // states its own predicate rather than trusting that, since a store that
        // mixes timestamp precisions is exactly where an assumed sort order goes
        // wrong quietly.
        let (dropped, kept): (Vec<&super::model::Event>, Vec<&super::model::Event>) =
            match retention {
                Retention::Keep(n) => {
                    let cut = events.len().saturating_sub(*n);
                    (
                        events[..cut].iter().collect(),
                        events[cut..].iter().collect(),
                    )
                }
                Retention::Before(cutoff) => {
                    check_cutoff(cutoff)?;
                    events
                        .iter()
                        .partition(|event| comparable(&event.created) < comparable(cutoff))
                }
            };

        let referenced: BTreeSet<PathBuf> = kept
            .iter()
            .flat_map(|event| event.files.iter())
            .filter_map(|file| blob_path(&store_index, &file.hash).ok())
            .collect();
        let mut blobs = Vec::new();
        let mut bytes = 0u64;
        for blob in self.blob_files(&store_index).await? {
            if referenced.contains(&blob) {
                continue;
            }
            // A size that cannot be read is not worth failing a prune over; the
            // total is a report, not a decision.
            bytes += match self.host().graph().stat(&blob).await {
                Ok(meta) => meta.len(),
                Err(_) => 0,
            };
            blobs.push(blob);
        }

        Ok(Pruned {
            events: dropped.iter().map(|event| event.id.clone()).collect(),
            blobs,
            bytes,
            keeping: kept.len(),
        })
    }
}

impl<H: HistoryWriteHost> HistoryStore<H> {
    /// Execute a [`Pruned`] plan: drop the events, rebuild the indexes the drop
    /// changed, then collect the blobs.
    ///
    /// **In that order, and the order is the safety argument.** Events first means
    /// a crash mid-prune leaves blobs no manifest references — a
    /// `Finding::HistoryBlobOrphaned`, which the next prune collects. Blobs first
    /// would leave surviving manifests naming bytes that are gone, which is real
    /// loss. The benign residue is the one prov already tolerates from capture, in
    /// the opposite direction.
    ///
    /// **Blobs do not ride the change set**, mirroring capture. There the reason
    /// is that the journal embeds contents; here it is that
    /// [`ChangeSet::remove`](prov_transaction::ChangeSet::remove) buffers the
    /// bytes it deletes so it can put them back, and a GC that frees a gigabyte
    /// would hold a gigabyte in memory to do it. Deleting content-addressed bytes
    /// directly is safe for the same reason writing them is: the operation is
    /// idempotent, and a half-finished one is an orphan rather than a corruption.
    ///
    /// A surviving index is rewritten only when its content would actually change.
    /// Every index this touches is a file some transport has to carry, and a prune
    /// that rewrote five years of untouched shards would be five years of
    /// needless merge surface.
    pub async fn prune(&mut self, root_doc: &Path, plan: &Pruned) -> Result<()> {
        let root_doc = link::normalize(root_doc);
        let (store_index, found) = self.store_index(&root_doc).await?;
        if !found.exists() || plan.is_empty() {
            return Ok(());
        }
        let style = self.authoring(&root_doc)?;
        let ext = style.ext.as_str();
        let dropped: BTreeSet<&str> = plan.events.iter().map(String::as_str).collect();
        let events_root = store_dir(&store_index).join(EVENTS_DIR);

        let mut cs = self.host_mut().change();
        for id in &plan.events {
            cs.remove(event_path(&store_index, id, ext)?);
        }

        // Rebuilt from the directory listing minus what this prune drops — the
        // same "an index is a pure function of its directory" rule capture and the
        // autofix follow, evaluated against the tree the prune is about to leave.
        let mut surviving_years = BTreeSet::new();
        for year in self.subdirs(&events_root).await? {
            let year_dir = events_root.join(&year);
            let mut surviving_months = BTreeSet::new();
            for month in self.subdirs(&year_dir).await? {
                let shard = year_dir.join(&month);
                let ids: BTreeSet<String> = self
                    .shard_event_ids(&shard, ext)
                    .await?
                    .into_iter()
                    .filter(|id| !dropped.contains(id.as_str()))
                    .collect();
                let index = shard.join(format!("index.{ext}"));
                if ids.is_empty() {
                    self.stage_index_removal(&mut cs, &index).await?;
                    continue;
                }
                surviving_months.insert(month.clone());
                self.stage_index_text(
                    &mut cs,
                    &index,
                    render_month_index(&year, &month, &ids, &style)?,
                )
                .await?;
            }
            let index = year_dir.join(format!("index.{ext}"));
            if surviving_months.is_empty() {
                self.stage_index_removal(&mut cs, &index).await?;
                continue;
            }
            surviving_years.insert(year.clone());
            self.stage_index_text(
                &mut cs,
                &index,
                render_year_index(&year, &surviving_months, &style)?,
            )
            .await?;
        }
        // The store index always survives: it is the root's pointer target, and a
        // store pruned to nothing is still a store — and it keeps linking the
        // tombstone list, which a prune never touches: those bytes are already
        // gone, and the record of that is not garbage.
        let forgotten = self.forgotten_link(&store_index).await?;
        self.stage_index_text(
            &mut cs,
            &store_index,
            render_store_index(&surviving_years, forgotten.as_deref(), &style)?,
        )
        .await?;
        self.host_mut().commit(cs).await?;

        for blob in &plan.blobs {
            // Tolerant of an already-absent blob: this runs after the commit, so a
            // re-run of an interrupted prune must be able to finish rather than
            // fail on the bytes the first run already freed.
            if self.host().graph().exists(blob).await? {
                discard_file(self.host().graph().fs(), self.host().graph().root(), blob).await?;
            }
        }
        Ok(())
    }
}
