use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use prov_graph::error::Result;

use super::docs::{
    forgotten_hashes, index_entries, render_month_index, render_store_index, render_year_index,
};
use super::layout::blob_path;
use super::{BLOBS_DIR, EVENTS_DIR, HistoryIssue, HistoryReadHost, HistoryStore};

impl<H: HistoryReadHost> HistoryStore<H> {
    /// Validate the history store: every index document against the directory it
    /// describes, emitting one [`HistoryIssue::IndexStale`] per index that has
    /// drifted.
    ///
    /// The store's interior needs its own pass rather than riding `check`'s
    /// general walk, because descent is **spanning-only**: the root reaches the
    /// store index through the one-way `history` pointer, and the walk does not
    /// descend a non-spanning edge. That is the right default for every other
    /// pointer-reached store, and it means the shard directories are neither
    /// scanned for orphans nor validated — so history validates them here, from
    /// the directories themselves, which is also what makes the check immune to
    /// the very staleness it is looking for.
    ///
    /// The pass also reports a store the root has stopped declaring
    /// ([`HistoryIssue::StoreUnlinked`]) — the one failure that is otherwise
    /// completely silent, since an undiscovered store is a subtree the walk never
    /// enters and so never reports anything about, orphans included.
    pub async fn findings(&self, root_doc: &Path) -> Result<Vec<HistoryIssue>> {
        let (store_index, found) = self.store_index(root_doc).await?;
        if !found.exists() {
            return Ok(Vec::new());
        }
        let style = self.authoring(root_doc)?;
        let ext = style.ext.as_str();
        let events_root = super::layout::store_dir(&store_index).join(EVENTS_DIR);
        let mut findings = Vec::new();

        // Reported first: everything below is about the store's *contents*, and a
        // reader who is about to be told their indexes are stale needs to know
        // prov cannot see the store from the root at all.
        //
        // Gated on the axis, not on the store's existence: with `history: off` a
        // leftover directory is not a loss, and saying so would be prov objecting
        // to a directory the user is entitled to leave alone.
        if found == super::layout::StoreLocation::Conventional && self.host().history_captures() {
            findings.push(HistoryIssue::StoreUnlinked {
                root: root_doc.to_path_buf(),
                store: store_index.clone(),
            });
        }

        let years = self.event_years(&events_root, ext).await?;
        let forgotten = self.forgotten_link(&store_index).await?;
        self.compare_index(
            &mut findings,
            &store_index,
            &render_store_index(&years, forgotten.as_deref(), &style)?,
        )
        .await?;

        for year in &years {
            let months = self.event_months(&events_root.join(year), ext).await?;
            self.compare_index(
                &mut findings,
                &events_root.join(year).join(format!("index.{ext}")),
                &render_year_index(year, &months, &style)?,
            )
            .await?;
            for month in &months {
                let shard = events_root.join(year).join(month);
                let ids = self.shard_event_ids(&shard, ext).await?;
                self.compare_index(
                    &mut findings,
                    &shard.join(format!("index.{ext}")),
                    &render_month_index(year, month, &ids, &style)?,
                )
                .await?;
            }
        }
        findings.extend(self.blob_findings(&store_index, ext).await?);
        Ok(findings)
    }

    /// The two blob findings: what the manifests promise and the store cannot
    /// deliver, and what the store holds that no manifest promises.
    ///
    /// Both fall out of one **mark-and-sweep** — union every event's `files`
    /// hashes, compare against the blob listing — which is what full manifests
    /// buy. Under a delta log the same question would require folding ancestry,
    /// and could not be answered at all for an event whose ancestors had not
    /// arrived.
    ///
    /// The honest cost: this parses every event document in the store, on every
    /// `check`. That is the price of validating a store whose authority is
    /// distributed across immutable documents rather than concentrated in an
    /// index — the same price [`log`](HistoryStore::log) pays, and for the same
    /// reason. Bounded by event count × manifest size.
    ///
    /// An event-shaped file that fails to load or parse raises the store-format
    /// doc's promised [`HistoryIssue::Unreadable`] (§7) — a plain, unchanged reuse of
    /// the finding the general walk already raises for any other document it
    /// cannot read, since an unreadable event is the same kind of problem. It
    /// does **not** get folded into `referenced`, so its potential blob
    /// references are simply unknown for the rest of this sweep.
    async fn blob_findings(&self, store_index: &Path, ext: &str) -> Result<Vec<HistoryIssue>> {
        // hash → the captured paths that named it, across every event. A manifest
        // routinely names one blob from several paths, and one blob is one thing
        // to put back, so the report is keyed by hash rather than by event.
        let (events, unreadable) = self.events_in(store_index, ext).await?;
        let mut referenced: BTreeMap<String, BTreeSet<PathBuf>> = BTreeMap::new();
        for event in events {
            for file in event.files {
                referenced.entry(file.hash).or_default().insert(file.path);
            }
        }

        // A hash on the tombstone list is absent *by record*: the bytes were
        // destroyed deliberately and that act was written down. Reporting it would
        // mean `check` never returned to clean after a legitimate forget, which is
        // how a user learns to stop reading `check` — and the whole point of
        // keeping the list is to be able to tell this state from loss.
        let forgotten = match self.forgotten_link(store_index).await? {
            Some(path) => match self.host().graph().load(&path).await {
                Ok((_, doc)) => forgotten_hashes(&doc.meta),
                Err(_) => BTreeSet::new(),
            },
            None => BTreeSet::new(),
        };

        let mut findings: Vec<HistoryIssue> = unreadable
            .iter()
            .map(|(path, error)| HistoryIssue::Unreadable {
                doc: path.clone(),
                error: error.clone(),
            })
            .collect();
        let mut promised: BTreeSet<PathBuf> = BTreeSet::new();
        for (hash, paths) in referenced {
            let missing = HistoryIssue::BlobMissing {
                store: store_index.to_path_buf(),
                hash: hash.clone(),
                paths: paths.into_iter().collect(),
            };
            // A digest prov could never have parked (a foreign scheme, a mangled
            // string) names no blob that could be found, so it reports as missing
            // rather than failing the whole check — a foreign event stays legible,
            // the same call `missing_blobs` makes.
            let Ok(blob) = blob_path(store_index, &hash) else {
                if !forgotten.contains(&hash) {
                    findings.push(missing);
                }
                continue;
            };
            // Recorded whether or not the bytes are there: this is the set of
            // paths the manifests *claim*, and a blob is an orphan by not being
            // claimed, not by being absent.
            promised.insert(blob.clone());
            if !self.host().graph().exists(&blob).await? && !forgotten.contains(&hash) {
                findings.push(missing);
            }
        }

        // While any event in the store is unreadable, `promised` is known to be
        // incomplete — the unreadable event's own manifest might have promised
        // some of these bytes. Reporting them as orphaned would be wrong in the
        // one direction that matters: `HistoryBlobOrphaned`'s message points
        // straight at `history-prune`, so a false orphan here is a diagnostic
        // recommending the very command that would delete a blob still named by
        // a document `check` cannot currently read. Suppressed for the *whole*
        // store rather than scoped per shard: blobs are content-addressed and
        // shared across the store, not partitioned by the shard an event lives
        // in, so there is no shard-local subset of `blobs/` a given unreadable
        // event could not have referenced.
        if unreadable.is_empty() {
            let orphaned: Vec<PathBuf> = self
                .blob_files(store_index)
                .await?
                .into_iter()
                .filter(|blob| !promised.contains(blob))
                .collect();
            if !orphaned.is_empty() {
                findings.push(HistoryIssue::BlobOrphaned {
                    store: store_index.to_path_buf(),
                    blobs: orphaned,
                });
            }
        }
        Ok(findings)
    }

    /// Every file parked under `blobs/`, workspace-relative and sorted — the
    /// "sweep" half of the mark-and-sweep, shared by
    /// [`HistoryIssue::BlobOrphaned`] and by
    /// [`prune_plan`](HistoryStore::prune_plan)'s collector, so what `check`
    /// calls an orphan and what a prune collects are the same set by
    /// construction.
    ///
    /// The top level as well as each `<2 hex>` shard: a transport's conflict copy
    /// of a blob can land at either. **Anything non-hidden counts**, not only
    /// well-formed digests — that cruft would never match a hash, which is
    /// precisely why listing files rather than parsing names is the right sweep. A
    /// dotfile is the transport's own bookkeeping and is left alone.
    pub async fn blob_files(&self, store_index: &Path) -> Result<Vec<PathBuf>> {
        let blobs_root = super::layout::store_dir(store_index).join(BLOBS_DIR);
        let mut dirs = vec![blobs_root.clone()];
        dirs.extend(
            self.subdirs(&blobs_root)
                .await?
                .into_iter()
                .map(|prefix| blobs_root.join(prefix)),
        );
        let mut files = Vec::new();
        for dir in dirs {
            let Ok(entries) = self.host().graph().listing(&dir).await else {
                continue;
            };
            for entry in entries {
                let Some(name) = entry.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if entry.file_type().is_file() && !name.starts_with('.') {
                    files.push(dir.join(name));
                }
            }
        }
        files.sort();
        Ok(files)
    }

    /// The months under `year_dir` that actually hold an event.
    ///
    /// **A directory with no event in it is not a shard.** A change set removes
    /// files, not directories, so a prune leaves an empty one behind every time
    /// it drops a month's last event — and a transport that deletes files can
    /// leave one too. Filtering where the indexes are *rendered* means neither
    /// capture nor `check` has to special-case it: an empty directory is
    /// invisible rather than a permanent `Finding::HistoryIndexStale` naming an
    /// index that should not exist.
    pub async fn event_months(&self, year_dir: &Path, ext: &str) -> Result<BTreeSet<String>> {
        let mut months = BTreeSet::new();
        for month in self.subdirs(year_dir).await? {
            if !self
                .shard_event_ids(&year_dir.join(&month), ext)
                .await?
                .is_empty()
            {
                months.insert(month);
            }
        }
        Ok(months)
    }

    /// The years under the store's `events/` that hold at least one month that
    /// holds at least one event. See [`event_months`](Self::event_months).
    pub async fn event_years(&self, events_root: &Path, ext: &str) -> Result<BTreeSet<String>> {
        let mut years = BTreeSet::new();
        for year in self.subdirs(events_root).await? {
            if !self
                .event_months(&events_root.join(&year), ext)
                .await?
                .is_empty()
            {
                years.insert(year);
            }
        }
        Ok(years)
    }

    /// Compare one index document against what it *should* say, by the set of
    /// entries each declares. Compared on the resolved link set rather than the
    /// raw text, so hand-edited prose or a reordered block is not "stale" — only a
    /// genuinely missing or surplus entry is.
    async fn compare_index(
        &self,
        findings: &mut Vec<HistoryIssue>,
        index: &Path,
        expected_text: &str,
    ) -> Result<()> {
        let expected = match prov_graph::document::Document::parse(index, expected_text) {
            Ok(doc) => index_entries(index, &doc.meta),
            Err(_) => Vec::new(),
        };
        let actual = match self.host().graph().load(index).await {
            Ok((_, doc)) => index_entries(index, &doc.meta).into_iter().collect(),
            // No index where one is owed. Only a finding if the directory has
            // something to describe — an empty store is simply not there yet.
            Err(_) if expected.is_empty() => return Ok(()),
            Err(_) => BTreeSet::new(),
        };
        let expected: BTreeSet<PathBuf> = expected.into_iter().collect();
        let missing: Vec<PathBuf> = expected.difference(&actual).cloned().collect();
        let extra: Vec<PathBuf> = actual.difference(&expected).cloned().collect();
        if !missing.is_empty() || !extra.is_empty() {
            findings.push(HistoryIssue::IndexStale {
                index: index.to_path_buf(),
                missing,
                extra,
            });
        }
        Ok(())
    }
}
