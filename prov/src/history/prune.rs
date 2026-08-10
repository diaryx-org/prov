use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::fs::Storage;
use crate::index::IndexStore;
use crate::link;
use crate::workspace::Workspace;

use super::EVENTS_DIR;
use super::docs::*;
use super::event_id::*;
use super::layout::*;
use super::model::*;

impl<FS: Storage, IdP, Ix: IndexStore> Workspace<FS, IdP, Ix> {
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
    /// The blob sweep is [`Finding::HistoryBlobOrphaned`](crate::validate::Finding::HistoryBlobOrphaned)'s, taken against the
    /// survivors rather than against every event — so what `check` calls an orphan
    /// and what a prune collects are the same set by construction, and a prune
    /// sweeps up the orphans that were already there.
    ///
    /// **Refuses if any event document in the store fails to load or parse.**
    /// The `referenced` set below is a bound computed *only* over the events that
    /// parsed; if some other event is unreadable, its manifest — and every blob
    /// it might name — is invisible to that bound, and the blobs the unreadable
    /// event alone referenced would be collected as orphans and deleted. That is
    /// permanent loss from a prune whose bound silently dropped nothing. A
    /// deliberate destruction must not proceed on an incomplete reference set, so
    /// this names the unreadable file(s) and stops before planning anything.
    pub async fn history_prune_plan(
        &self,
        root_doc: &Path,
        retention: &Retention,
    ) -> Result<Pruned> {
        let root_doc = link::normalize(root_doc);
        let (store_index, found) = self.history_store_index(&root_doc).await?;
        if !found.exists() {
            return Ok(Pruned::default());
        }
        let (events, unreadable) = self
            .history_events_in(&store_index, self.history_ext(&root_doc))
            .await?;
        if !unreadable.is_empty() {
            return Err(Error::Structure(format!(
                "history-prune refuses: {} event document(s) could not be read, so the \
                 blobs they might reference cannot be told apart from orphans: {}. Repair \
                 or restore them (or let the transport finish syncing) before pruning.",
                unreadable.len(),
                Self::describe_unreadable(&unreadable)
            )));
        }

        // Events arrive oldest first, so both axes cut a prefix — but `Before`
        // states its own predicate rather than trusting that, since a store that
        // mixes timestamp precisions is exactly where an assumed sort order goes
        // wrong quietly.
        let (dropped, kept): (Vec<&Event>, Vec<&Event>) = match retention {
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
        for blob in self.history_blob_files(&store_index).await? {
            if referenced.contains(&blob) {
                continue;
            }
            // A size that cannot be read is not worth failing a prune over; the
            // total is a report, not a decision.
            bytes += match self.stat(&blob).await {
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

    /// Execute a [`Pruned`] plan: drop the events, rebuild the indexes the drop
    /// changed, then collect the blobs.
    ///
    /// **In that order, and the order is the safety argument.** Events first means
    /// a crash mid-prune leaves blobs no manifest references — a
    /// [`Finding::HistoryBlobOrphaned`](crate::validate::Finding::HistoryBlobOrphaned), which the next prune collects. Blobs
    /// first would leave surviving manifests naming bytes that are gone, which is
    /// real loss. The benign residue is the one prov already tolerates from
    /// capture, in the opposite direction.
    ///
    /// **Blobs do not ride the change set**, mirroring capture. There the reason
    /// is that the journal embeds contents; here it is that
    /// [`ChangeSet::remove`](crate::change::ChangeSet::remove) buffers the bytes it deletes so it can put them
    /// back, and a GC that frees a gigabyte would hold a gigabyte in memory to do
    /// it. Deleting content-addressed bytes directly is safe for the same reason
    /// writing them is: the operation is idempotent, and a half-finished one is an
    /// orphan rather than a corruption.
    ///
    /// A surviving index is rewritten only when its content would actually change.
    /// Every index this touches is a file some transport has to carry, and a prune
    /// that rewrote five years of untouched shards would be five years of
    /// needless merge surface.
    pub async fn history_prune(&mut self, root_doc: &Path, plan: &Pruned) -> Result<()> {
        let root_doc = link::normalize(root_doc);
        let (store_index, found) = self.history_store_index(&root_doc).await?;
        if !found.exists() || plan.is_empty() {
            return Ok(());
        }
        let style = self.history_authoring(&root_doc)?;
        let ext = style.ext.as_str();
        let dropped: BTreeSet<&str> = plan.events.iter().map(String::as_str).collect();
        let events_root = store_dir(&store_index).join(EVENTS_DIR);

        let mut cs = self.change();
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
        let forgotten = self.history_forgotten_link(&store_index).await?;
        self.stage_index_text(
            &mut cs,
            &store_index,
            render_store_index(&surviving_years, forgotten.as_deref(), &style)?,
        )
        .await?;
        self.commit(cs).await?;

        for blob in &plan.blobs {
            // Tolerant of an already-absent blob: this runs after the commit, so a
            // re-run of an interrupted prune must be able to finish rather than
            // fail on the bytes the first run already freed.
            if self.exists(blob).await? {
                crate::change::discard_file(self.fs(), self.root(), blob).await?;
            }
        }
        Ok(())
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::super::support::*;
    use super::*;
    use crate::exec::block_on;
    use crate::validate::Finding;

    /// Plan and run a prune, the sequence the CLI performs.
    fn prune(dir: &Path, retention: &Retention) -> Pruned {
        let mut w = ws(dir);
        let root = Path::new("index.md");
        let plan = block_on(w.history_prune_plan(root, retention)).unwrap();
        block_on(w.history_prune(root, &plan)).unwrap();
        plan
    }

    #[test]
    fn a_prune_drops_the_oldest_and_collects_only_what_nothing_still_references() {
        let dir = seed("prune-basic");
        let first = capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
        let second = capture_edited(&dir, "2026-07-31T10:00:00.000000Z", "two", "beta");
        let third = capture_edited(&dir, "2026-07-31T11:00:00.000000Z", "three", "gamma");

        // The blob only the dropped events name, and one every event names — the
        // whole correctness question a GC has to get right.
        let dropped_bytes = blob_path(
            Path::new("history/index.md"),
            &crate::fixity::digest(b"---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n"),
        )
        .unwrap();
        let shared_bytes = blob_path(
            Path::new("history/index.md"),
            &crate::fixity::digest(b"JPEGBYTES"),
        )
        .unwrap();
        assert!(dir.join(&dropped_bytes).exists() && dir.join(&shared_bytes).exists());

        let plan = prune(&dir, &Retention::Keep(1));
        assert_eq!(plan.events, vec![first, second]);
        assert_eq!(plan.keeping, 1);
        assert!(plan.bytes > 0, "the report has to name what it freed");

        assert!(
            !dir.join(&dropped_bytes).exists(),
            "bytes only the dropped events named must go"
        );
        assert!(
            dir.join(&shared_bytes).exists(),
            "bytes a surviving manifest still names must not"
        );

        // The store is valid, and the surviving event is still a complete
        // recovery point — which is the property that makes prune safe at all.
        assert_eq!(
            block_on(ws(&dir).check(Path::new("index.md"))).unwrap(),
            vec![]
        );
        let events = block_on(ws(&dir).history_list(Path::new("index.md"))).unwrap();
        assert_eq!(
            events.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec![third.as_str()]
        );
        let survivor = &events[0];
        assert!(
            block_on(ws(&dir).history_missing_blobs(Path::new("index.md"), survivor))
                .unwrap()
                .is_empty(),
            "every row of a surviving event must still have its bytes"
        );
    }

    #[test]
    fn a_prune_also_collects_the_orphans_that_were_already_there() {
        // `HistoryBlobOrphaned` points at this verb, so the two have to agree on
        // what an orphan is. They share the sweep, and this is the assertion that
        // says so.
        let dir = seed("prune-orphans");
        capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
        write(&dir, "history/blobs/ab/sync-conflict-20260731", "junk");

        let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
        assert!(matches!(
            findings.as_slice(),
            [Finding::HistoryBlobOrphaned { blobs, .. }]
                if blobs == &[PathBuf::from("history/blobs/ab/sync-conflict-20260731")]
        ));

        // Keeping every event still collects it: the sweep is "what nothing
        // references", not "what this drop orphaned".
        let plan = prune(&dir, &Retention::Keep(10));
        assert!(plan.events.is_empty());
        assert_eq!(
            plan.blobs,
            vec![PathBuf::from("history/blobs/ab/sync-conflict-20260731")]
        );
        assert_eq!(
            block_on(ws(&dir).check(Path::new("index.md"))).unwrap(),
            vec![]
        );
    }

    #[test]
    fn an_emptied_shard_leaves_no_index_and_no_finding() {
        let dir = seed("prune-shards");
        capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "july", "alpha");
        capture_edited(&dir, "2026-08-01T09:00:00.000000Z", "august", "beta");
        assert!(dir.join("history/events/2026/07/index.md").exists());

        // Drop July: its shard index goes with it, but the year survives because
        // August is still there.
        prune(&dir, &Retention::Before("2026-08-01".into()));
        assert!(!dir.join("history/events/2026/07/index.md").exists());
        assert!(dir.join("history/events/2026/index.md").exists());
        assert!(dir.join("history/events/2026/08/index.md").exists());
        assert_eq!(
            block_on(ws(&dir).check(Path::new("index.md"))).unwrap(),
            vec![]
        );

        // Now the year, too. A change set removes files rather than directories,
        // so `2026/07/` is still sitting there — and must be invisible, not a
        // permanent finding about an index that should not exist.
        prune(&dir, &Retention::Keep(0));
        assert!(!dir.join("history/events/2026/index.md").exists());
        assert!(
            dir.join("history/events/2026/07").is_dir(),
            "the empty directory is expected to linger"
        );
        assert_eq!(
            block_on(ws(&dir).check(Path::new("index.md"))).unwrap(),
            vec![],
            "an event-less directory is not a shard"
        );

        // …and the store still works: a later capture rebuilds the tree around it.
        capture_edited(&dir, "2026-09-01T09:00:00.000000Z", "after", "delta");
        assert_eq!(
            block_on(ws(&dir).check(Path::new("index.md"))).unwrap(),
            vec![]
        );
    }

    #[test]
    fn a_date_cutoff_keeps_the_day_it_names_and_a_typo_drops_nothing() {
        let dir = seed("prune-before");
        capture_edited(&dir, "2026-07-31T23:59:59.999999Z", "eve", "alpha");
        let boundary = capture_edited(&dir, "2026-08-01T00:00:00.000000Z", "dawn", "beta");
        let later = capture_edited(&dir, "2026-08-02T09:00:00.000000Z", "later", "gamma");

        // "before 2026-08-01" means before that day *started*: a bare date is a
        // prefix of every timestamp in its day, which is what makes the boundary
        // read the way a person means it without parsing a calendar.
        let w = ws(&dir);
        let plan = block_on(w.history_prune_plan(
            Path::new("index.md"),
            &Retention::Before("2026-08-01".into()),
        ))
        .unwrap();
        assert_eq!(plan.keeping, 2);
        assert!(!plan.events.contains(&boundary) && !plan.events.contains(&later));

        // A cutoff that is not a date deletes nothing rather than everything.
        let typo = block_on(w.history_prune_plan(
            Path::new("index.md"),
            &Retention::Before("yesterday".into()),
        ));
        assert!(typo.is_err(), "a typo must not be a silent full sweep");
    }

    #[test]
    fn a_prune_with_nothing_to_drop_touches_no_file() {
        let dir = seed("prune-noop");
        capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
        let index = read(&dir, "history/events/2026/07/index.md");
        let before = std::fs::metadata(dir.join("history/index.md"))
            .unwrap()
            .modified()
            .unwrap();

        let plan = prune(&dir, &Retention::Keep(5));
        assert!(plan.is_empty());
        // Every index a prune touches is a file some transport has to carry, so
        // one with nothing to do must not churn them.
        assert_eq!(read(&dir, "history/events/2026/07/index.md"), index);
        assert_eq!(
            std::fs::metadata(dir.join("history/index.md"))
                .unwrap()
                .modified()
                .unwrap(),
            before
        );
    }

    #[test]
    fn a_prune_refuses_while_any_event_is_unreadable() {
        // The bug this guards: a `referenced` set built only from the events
        // that parsed treats the torn event's blobs as unclaimed, and a prune
        // would collect and delete them — permanent loss from a bound that
        // silently dropped a whole event's worth of references.
        let dir = seed("prune-torn");
        let first = capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
        capture_edited(&dir, "2026-07-31T10:00:00.000000Z", "two", "beta");
        let torn = event_path(Path::new("history/index.md"), &first, "md").unwrap();
        tear(&dir, torn.to_str().unwrap());

        let w = ws(&dir);
        let err =
            block_on(w.history_prune_plan(Path::new("index.md"), &Retention::Keep(1))).unwrap_err();
        assert!(
            err.to_string().contains(torn.to_str().unwrap()),
            "the refusal has to name the file that could not be read: {err}"
        );

        // Refused before a plan even exists — nothing on disk moved.
        assert!(dir.join(&torn).exists());
        assert!(
            dir.join(blob_of(
                b"---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n"
            ))
            .exists()
        );
    }
}
