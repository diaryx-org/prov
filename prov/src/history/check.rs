use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::validate::Finding;
use crate::workspace::Workspace;
use prov_graph::error::Result;
use prov_graph::fs::Storage;
use prov_graph::index::IndexStore;

use super::docs::*;
use super::layout::*;
use super::{BLOBS_DIR, EVENTS_DIR};

impl<FS: Storage, IdP, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Validate the history store: every index document against the directory it
    /// describes, emitting one [`Finding::HistoryIndexStale`] per index that has
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
    /// ([`Finding::HistoryStoreUnlinked`]) — the one failure that is otherwise
    /// completely silent, since an undiscovered store is a subtree the walk never
    /// enters and so never reports anything about, orphans included.
    pub async fn history_findings(&self, root_doc: &Path) -> Result<Vec<Finding>> {
        let (store_index, found) = self.history_store_index(root_doc).await?;
        if !found.exists() {
            return Ok(Vec::new());
        }
        let style = self.history_authoring(root_doc)?;
        let ext = style.ext.as_str();
        let events_root = store_dir(&store_index).join(EVENTS_DIR);
        let mut findings = Vec::new();

        // Reported first: everything below is about the store's *contents*, and a
        // reader who is about to be told their indexes are stale needs to know
        // prov cannot see the store from the root at all.
        //
        // Gated on the axis, not on the store's existence: with `history: off` a
        // leftover directory is not a loss, and saying so would be prov objecting
        // to a directory the user is entitled to leave alone.
        if found == StoreLocation::Conventional && self.history().captures() {
            findings.push(Finding::HistoryStoreUnlinked {
                root: root_doc.to_path_buf(),
                store: store_index.clone(),
            });
        }

        let years = self.event_years(&events_root, ext).await?;
        let forgotten = self.history_forgotten_link(&store_index).await?;
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
        findings.extend(self.history_blob_findings(&store_index, ext).await?);
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
    /// index — the same price [`history_log`](Self::history_log) pays, and for the
    /// same reason. Bounded by event count × manifest size.
    ///
    /// An event-shaped file that fails to load or parse raises the store-format
    /// doc's promised [`Finding::Unreadable`] (§7) — a plain, unchanged reuse of
    /// the finding the general walk already raises for any other document it
    /// cannot read, since an unreadable event is the same kind of problem. It
    /// does **not** get folded into `referenced`, so its potential blob
    /// references are simply unknown for the rest of this sweep.
    async fn history_blob_findings(&self, store_index: &Path, ext: &str) -> Result<Vec<Finding>> {
        // hash → the captured paths that named it, across every event. A manifest
        // routinely names one blob from several paths, and one blob is one thing
        // to put back, so the report is keyed by hash rather than by event.
        let (events, unreadable) = self.history_events_in(store_index, ext).await?;
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
        let forgotten = match self.history_forgotten_link(store_index).await? {
            Some(path) => match self.load(&path).await {
                Ok((_, doc)) => forgotten_hashes(&doc.meta),
                Err(_) => BTreeSet::new(),
            },
            None => BTreeSet::new(),
        };

        let mut findings: Vec<Finding> = unreadable
            .iter()
            .map(|(path, error)| Finding::Unreadable {
                doc: path.clone(),
                error: error.clone(),
            })
            .collect();
        let mut promised: BTreeSet<PathBuf> = BTreeSet::new();
        for (hash, paths) in referenced {
            let missing = Finding::HistoryBlobMissing {
                store: store_index.to_path_buf(),
                hash: hash.clone(),
                paths: paths.into_iter().collect(),
            };
            // A digest prov could never have parked (a foreign scheme, a mangled
            // string) names no blob that could be found, so it reports as missing
            // rather than failing the whole check — a foreign event stays legible,
            // the same call `history_missing_blobs` makes.
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
            if !self.exists(&blob).await? && !forgotten.contains(&hash) {
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
                .history_blob_files(store_index)
                .await?
                .into_iter()
                .filter(|blob| !promised.contains(blob))
                .collect();
            if !orphaned.is_empty() {
                findings.push(Finding::HistoryBlobOrphaned {
                    store: store_index.to_path_buf(),
                    blobs: orphaned,
                });
            }
        }
        Ok(findings)
    }

    /// Every file parked under `blobs/`, workspace-relative and sorted — the
    /// "sweep" half of the mark-and-sweep, shared by
    /// [`Finding::HistoryBlobOrphaned`](crate::validate::Finding::HistoryBlobOrphaned) and by
    /// [`history_prune`](Self::history_prune)'s collector, so what `check` calls
    /// an orphan and what `prune` collects are the same set by construction.
    ///
    /// The top level as well as each `<2 hex>` shard: a transport's conflict copy
    /// of a blob can land at either. **Anything non-hidden counts**, not only
    /// well-formed digests — that cruft would never match a hash, which is
    /// precisely why listing files rather than parsing names is the right sweep. A
    /// dotfile is the transport's own bookkeeping and is left alone.
    pub(super) async fn history_blob_files(&self, store_index: &Path) -> Result<Vec<PathBuf>> {
        let blobs_root = store_dir(store_index).join(BLOBS_DIR);
        let mut dirs = vec![blobs_root.clone()];
        dirs.extend(
            self.subdirs(&blobs_root)
                .await?
                .into_iter()
                .map(|prefix| blobs_root.join(prefix)),
        );
        let mut files = Vec::new();
        for dir in dirs {
            let Ok(entries) = self.listing(&dir).await else {
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
    /// files, not directories, so [`history_prune`](Self::history_prune) leaves an
    /// empty one behind every time it drops a month's last event — and a transport
    /// that deletes files can leave one too. Filtering where the indexes are
    /// *rendered* means neither capture nor `check` has to special-case it: an
    /// empty directory is invisible rather than a permanent
    /// [`Finding::HistoryIndexStale`] naming an index that should not exist.
    pub(super) async fn event_months(
        &self,
        year_dir: &Path,
        ext: &str,
    ) -> Result<BTreeSet<String>> {
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
    pub(super) async fn event_years(
        &self,
        events_root: &Path,
        ext: &str,
    ) -> Result<BTreeSet<String>> {
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
        findings: &mut Vec<Finding>,
        index: &Path,
        expected_text: &str,
    ) -> Result<()> {
        let expected = match prov_graph::document::Document::parse(index, expected_text) {
            Ok(doc) => index_entries(index, &doc.meta),
            Err(_) => Vec::new(),
        };
        let actual = match self.load(index).await {
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
            findings.push(Finding::HistoryIndexStale {
                index: index.to_path_buf(),
                missing,
                extra,
            });
        }
        Ok(())
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::super::model::Captured;
    use super::super::support::*;
    use super::*;
    use prov_graph::exec::block_on;

    /// A shard index is titled `"{Month} {Year}"`, which in a journal is an
    /// entirely ordinary thing for a person to have called a note. Before the
    /// stores were excluded, `[[January 2026]]` resolved `Unique` into
    /// `history/events/2026/01/index.md` — a document the reader cannot see in
    /// the tree and never meant to link to.
    #[test]
    fn a_shard_index_never_answers_to_a_name_the_author_might_use() {
        let dir = seed("titles-history");
        capture(&dir, "2026-01-15T09:15:22.000000Z", Some("first"));
        let w = ws(&dir);

        let titles = block_on(w.title_index_scoped(Path::new("index.md"))).unwrap();
        assert!(
            matches!(titles.resolve("January 2026"), crate::TitleMatch::Unknown),
            "a history shard answered to a month-and-year name"
        );
        // The store's own index is the deliberate exception, and the boundary is
        // worth pinning: the root points at it, `check` validates it, and a
        // reader can open it and learn what the store holds — so it is a
        // document of the workspace and keeps a name like any other. What is
        // excluded is its *interior*.
        assert!(
            matches!(titles.resolve("History"), crate::TitleMatch::Unique(_)),
            "the store index is part of the workspace and should still resolve"
        );

        // The author's *own* documents still resolve — the exclusion is about
        // prov's bookkeeping, not about narrowing the workspace.
        assert!(
            matches!(titles.resolve("A"), crate::TitleMatch::Unique(_)),
            "an ordinary note stopped resolving"
        );
    }

    /// The same exclusion, in the direction the bin makes vivid: a recycled
    /// document keeps the title it had, so indexing `items/` means `[[A]]` can
    /// resolve to the copy of a note the author deleted — while the live note is
    /// still sitting there under the same name.
    #[test]
    fn a_recycled_document_stops_answering_to_the_name_it_had() {
        let dir = seed("titles-bin");
        let mut w = ws(&dir);
        block_on(w.recycle(Path::new("notes/a.md"), false, Some("2026-01-15T09:15:22Z"))).unwrap();

        let titles = block_on(ws(&dir).title_index_scoped(Path::new("index.md"))).unwrap();
        assert!(
            matches!(titles.resolve("A"), crate::TitleMatch::Unknown),
            "a binned document still answered to its title"
        );
    }

    #[test]
    fn lost_bytes_are_reported_once_per_hash_however_many_events_named_them() {
        let dir = seed("blob-missing");
        capture(&dir, "2026-07-31T09:00:00.000000Z", None);
        // A second capture that changes one file: everything else keeps the blob
        // the first capture parked, so one blob is now named by two manifests.
        write(
            &dir,
            "notes/a.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\nrevised\n",
        );
        capture(&dir, "2026-07-31T10:00:00.000000Z", None);

        let payload = crate::fixity::digest(b"JPEGBYTES");
        std::fs::remove_file(dir.join(blob_path(Path::new("history/index.md"), &payload).unwrap()))
            .unwrap();

        let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
        assert_eq!(
            findings,
            vec![Finding::HistoryBlobMissing {
                store: PathBuf::from("history/index.md"),
                hash: payload.clone(),
                paths: vec![PathBuf::from("notes/photo.jpg")],
            }],
            "one lost blob is one thing to put back, not one report per event"
        );
        // Both causes have to be readable in the text — a store that syncs is in
        // this state routinely, and a finding that cries corruption at a
        // self-resolving state is one people learn to ignore.
        let text = findings[0].to_string();
        assert!(
            text.contains("has not arrived yet") && text.contains("gone"),
            "{text}"
        );
        assert!(text.contains("notes/photo.jpg"), "{text}");

        // Deleting the blob left nothing behind, so there is no orphan to pair
        // with it: the two findings answer opposite questions.
        assert!(
            !findings
                .iter()
                .any(|f| matches!(f, Finding::HistoryBlobOrphaned { .. }))
        );
    }

    #[test]
    fn a_manifest_row_prov_could_never_have_parked_reports_rather_than_failing() {
        // A foreign event has to stay legible: `check` reads what arrived from
        // another device, and a digest in a scheme this build does not know is a
        // report, not a parse error that takes the whole run down.
        let dir = seed("blob-foreign");
        let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:00:00.000000Z", None)
        else {
            panic!("the first capture must write an event");
        };
        let event = event_path(Path::new("history/index.md"), &id, "md").unwrap();
        let text = read(&dir, event.to_str().unwrap());
        write(
            &dir,
            event.to_str().unwrap(),
            &text.replace(
                &crate::fixity::digest(b"JPEGBYTES"),
                "blake3:beefbeefbeefbeef",
            ),
        );

        let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
        let missing: Vec<&Finding> = findings
            .iter()
            .filter(|f| matches!(f, Finding::HistoryBlobMissing { .. }))
            .collect();
        assert_eq!(missing.len(), 1, "{findings:?}");
        assert!(
            missing[0].to_string().contains("blake3:"),
            "{:?}",
            missing[0]
        );
        // …and the blob it no longer names is now unreferenced, which is the
        // other half of the same sweep.
        assert!(
            findings
                .iter()
                .any(|f| matches!(f, Finding::HistoryBlobOrphaned { .. })),
            "{findings:?}"
        );
    }

    #[test]
    fn bytes_no_manifest_claims_are_reported_as_orphaned() {
        let dir = seed("blob-orphan");
        capture(&dir, "2026-07-31T09:00:00.000000Z", None);
        assert!(
            block_on(ws(&dir).check(Path::new("index.md")))
                .unwrap()
                .is_empty(),
            "a fresh capture claims every blob it parked"
        );

        // Cruft of the two shapes a transport actually leaves: a conflict copy
        // beside a real blob, and a stray at the top of the store. Neither could
        // ever match a hash, which is the point — this is not a digest check.
        write(&dir, "history/blobs/ab/sync-conflict-20260731", "junk");
        write(&dir, "history/blobs/stray.txt", "junk");
        // A hidden file is transport bookkeeping, not cruft prov should name.
        write(&dir, "history/blobs/.DS_Store", "junk");

        let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
        assert_eq!(
            findings,
            vec![Finding::HistoryBlobOrphaned {
                store: PathBuf::from("history/index.md"),
                blobs: vec![
                    PathBuf::from("history/blobs/ab/sync-conflict-20260731"),
                    PathBuf::from("history/blobs/stray.txt"),
                ],
            }],
            "one sweep, one finding, sorted — and the dotfile left alone"
        );
        assert!(
            findings[0].to_string().contains("history-prune"),
            "the report names the verb that collects them: {}",
            findings[0]
        );
    }

    #[test]
    fn check_reports_an_unreadable_event_and_never_recommends_pruning_its_blobs() {
        // The promise docs/history-format.md §7 makes and the codebase did not
        // keep: an event document that fails to parse is a plain `Unreadable`.
        // And the other half of the bug: while it is unreadable, its blobs must
        // not be reported `HistoryBlobOrphaned` — that finding's own message
        // points straight at `history-prune`, so a false orphan here is a
        // diagnostic recommending the destructive verb the two tests above
        // refuse to run.
        let dir = seed("check-torn");
        let first = capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
        let torn = event_path(Path::new("history/index.md"), &first, "md").unwrap();
        tear(&dir, torn.to_str().unwrap());

        let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
        assert!(
            findings
                .iter()
                .any(|f| matches!(f, Finding::Unreadable { doc, .. } if doc == &torn)),
            "missing the promised finding for {}: {findings:?}",
            torn.display()
        );
        assert!(
            !findings
                .iter()
                .any(|f| matches!(f, Finding::HistoryBlobOrphaned { .. })),
            "a torn event's blobs must not be reported as orphans: {findings:?}"
        );

        // Reading `check` must not be what destroys the bytes: the blob only
        // this (now unreadable) event named is still exactly where it was.
        assert!(
            dir.join(blob_of(
                b"---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n"
            ))
            .exists()
        );
    }

    /// Strip the root's `history` line and everything else about the store goes
    /// quiet: descent into it is through that pointer, so the walk never enters
    /// the subtree and reports nothing about it — not even an orphan. This is the
    /// finding that exists because the silence is total.
    fn unlink_the_store(dir: &Path) {
        let root = read(dir, "index.md");
        write(
            dir,
            "index.md",
            &root
                .lines()
                .filter(|l| !l.starts_with("history:"))
                .map(|l| format!("{l}\n"))
                .collect::<String>(),
        );
    }

    #[test]
    fn a_store_the_root_stopped_declaring_is_reported_and_relinked() {
        let dir = seed("check-unlinked");
        capture(&dir, "2026-07-31T09:00:00.000000Z", None);
        unlink_the_store(&dir);

        let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
        let unlinked = Finding::HistoryStoreUnlinked {
            root: PathBuf::from("index.md"),
            store: PathBuf::from("history/index.md"),
        };
        assert!(
            findings.contains(&unlinked),
            "a store nothing declares must not be silent: {findings:?}"
        );
        // Reported *first*: everything else about the store is about its contents,
        // and a reader has to know prov cannot see it from the root at all.
        assert_eq!(
            findings.iter().position(|f| f == &unlinked),
            Some(0),
            "{findings:?}"
        );
        let text = unlinked.to_string();
        assert!(
            text.contains("history/index.md") && text.contains("index.md"),
            "{text}"
        );

        // Metadata-only, and the pointer comes back spelled the way a bootstrap
        // capture would have spelled it.
        let fix = block_on(ws(&dir).suggest_fix(&unlinked)).unwrap().unwrap();
        assert_eq!(
            fix,
            crate::Fix::LinkHistoryStore {
                root: PathBuf::from("index.md"),
                store: PathBuf::from("history/index.md"),
            }
        );
        block_on(ws(&dir).apply_fix(&fix)).unwrap();
        assert!(read(&dir, "index.md").contains("history: history/index.md"));
        assert!(
            !block_on(ws(&dir).check(Path::new("index.md")))
                .unwrap()
                .iter()
                .any(|f| matches!(f, Finding::HistoryStoreUnlinked { .. })),
            "the fix has to actually retire the finding"
        );
    }

    /// With the axis off, a leftover `history/` is not a loss — the workspace said
    /// it wants no store, and a finding would be prov objecting to a directory the
    /// user is entitled to leave lying around. Declaring `manual` is what makes a
    /// missing pointer a defect rather than a preference.
    #[test]
    fn an_undeclared_store_is_not_a_finding_when_history_is_off() {
        let dir = seed("check-unlinked-off");
        capture(&dir, "2026-07-31T09:00:00.000000Z", None);
        unlink_the_store(&dir);

        assert!(
            !block_on(ws_history_off(&dir).check(Path::new("index.md")))
                .unwrap()
                .iter()
                .any(|f| matches!(f, Finding::HistoryStoreUnlinked { .. }))
        );
    }
}
