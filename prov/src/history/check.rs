use std::collections::BTreeSet;
use std::path::Path;

use crate::workspace::Workspace;
use prov_graph::error::Result;
use prov_graph::fs::Storage;
use prov_graph::index::IndexStore;

use prov_history::*;

impl<FS: Storage, IdP, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Validate the history store: every index document against the directory it
    /// describes, emitting one [`HistoryIssue::IndexStale`] per index that has
    /// drifted. See `prov_history::HistoryStore::findings`.
    pub async fn history_findings(&self, root_doc: &Path) -> Result<Vec<HistoryIssue>> {
        self.history_store().findings(root_doc).await
    }

    /// The months under `year_dir` that actually hold an event. See
    /// `prov_history::HistoryStore::event_months`.
    pub(super) async fn event_months(
        &self,
        year_dir: &Path,
        ext: &str,
    ) -> Result<BTreeSet<String>> {
        self.history_store().event_months(year_dir, ext).await
    }

    /// The years under the store's `events/` that hold at least one month that
    /// holds at least one event. See `prov_history::HistoryStore::event_years`.
    pub(super) async fn event_years(
        &self,
        events_root: &Path,
        ext: &str,
    ) -> Result<BTreeSet<String>> {
        self.history_store().event_years(events_root, ext).await
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use std::path::PathBuf;

    use prov_history::Captured;

    use super::super::support::*;
    use super::*;
    use crate::validate::Finding;
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
