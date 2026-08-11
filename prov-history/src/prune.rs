use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use prov_graph::error::{Error, Result};
use prov_graph::link;

use super::event_id::{check_cutoff, comparable};
use super::layout::blob_path;
use super::model::{Pruned, Retention};
use super::{HistoryReadHost, HistoryStore};

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
