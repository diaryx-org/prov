use std::path::Path;

use crate::workspace::Workspace;
use prov_graph::error::Result;
use prov_graph::fs::Storage;
use prov_graph::index::IndexStore;

use prov_history::{Event, RestorePlan, Scope};

impl<FS: Storage, IdP, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// What restoring `event` would do, computed **before a byte moves**. See
    /// `prov_history::HistoryStore::restore_plan`.
    pub async fn history_restore_plan(
        &self,
        root_doc: &Path,
        event: &Event,
        scope: &Scope,
        exact: bool,
    ) -> Result<RestorePlan> {
        self.history_store()
            .restore_plan(root_doc, event, scope, exact)
            .await
    }

    /// Execute a [`RestorePlan`]. See `prov_history::HistoryStore::restore`.
    pub async fn history_restore(
        &mut self,
        root_doc: &Path,
        plan: &RestorePlan,
        force: bool,
    ) -> Result<()> {
        self.history_store_mut()
            .restore(root_doc, plan, force)
            .await
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use std::path::PathBuf;

    use super::super::support::*;
    use super::*;
    use prov_graph::error::Error;
    use prov_graph::exec::block_on;
    use prov_graph::index::Collision;
    use prov_history::*;

    #[test]
    fn a_captured_workspace_goes_back_from_its_blobs_without_a_journal_its_size() {
        // What restore will rest on, proved against what Phase 0 actually writes:
        // a manifest plus `blob_path` is enough to stage the whole capture set as
        // copies, and the journal that makes that set crash-atomic is bounded by
        // the file *count*, not by the size of the workspace. Staged as `write`s,
        // this same set would put every byte below into `.prov-journal` first.
        let dir = seed("restore-primitive");
        let payload = "J".repeat(256 * 1024);
        write(&dir, "notes/photo.jpg", &payload);
        let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
            panic!("the first capture must write an event");
        };

        // Damage of the shape a bad merge leaves: bytes clobbered at several paths
        // at once, which is why an event is a consistent cut rather than a file.
        write(&dir, "notes/a.md", "clobbered by a sync conflict");
        write(&dir, "notes/photo.jpg", "truncated");

        let mut w = ws(&dir);
        let event = block_on(w.history_event(Path::new("index.md"), &id))
            .unwrap()
            .unwrap();
        let store_index = Path::new("history/index.md");
        let mut cs = w.change();
        for file in &event.files {
            cs.copy_from(&file.path, blob_path(store_index, &file.hash).unwrap());
        }
        let journal = crate::journal::encode(cs.ops()).unwrap();
        assert!(
            journal.len() < 2048,
            "the journal for a {payload_len}-byte workspace should be paths only, \
             got {journal_len} bytes",
            payload_len = payload.len(),
            journal_len = journal.len()
        );
        block_on(w.commit(cs)).unwrap();

        // Byte-exact at every captured path — checked against the manifest's own
        // hashes, which is the only claim a restore actually owes.
        for file in &event.files {
            let bytes = std::fs::read(dir.join(&file.path)).unwrap();
            assert_eq!(
                crate::fixity::digest(&bytes),
                file.hash,
                "{} did not come back byte-exact",
                file.path.display()
            );
        }
        assert_eq!(read(&dir, "notes/photo.jpg").len(), payload.len());
    }

    /// Plan and run a restore in one go, on a workspace of the caller's choosing —
    /// the sequence the CLI performs, so a test exercises the shipped path rather
    /// than a convenient shortcut past it.
    fn restore(
        w: &mut Workspace<StdFs, Minter, FileIndex>,
        id: &str,
        scope: &Scope,
        exact: bool,
        force: bool,
    ) -> Result<RestorePlan> {
        let root = Path::new("index.md");
        let event = block_on(w.history_event(root, id))?.expect("the event should be in the store");
        let plan = block_on(w.history_restore_plan(root, &event, scope, exact))?;
        block_on(w.history_restore(root, &plan, force))?;
        Ok(plan)
    }

    fn dispositions(plan: &RestorePlan, want: Disposition) -> Vec<&Path> {
        plan.ops
            .iter()
            .filter(|op| op.disposition == want)
            .map(|op| op.path.as_path())
            .collect()
    }

    #[test]
    fn a_restore_puts_the_whole_consistent_cut_back_byte_exact() {
        let dir = seed("restore-cut");
        let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
            panic!("the first capture must write an event");
        };

        // Damage of the shape a bad merge leaves: several files at once, which is
        // why an event is a consistent cut rather than a file. One of them is the
        // parent's child list — the structural half a per-file undo would miss.
        write(&dir, "notes/a.md", "clobbered by a sync conflict");
        write(&dir, "notes/photo.jpg", "truncated");
        relink_live(&dir, &["notes/photo.jpg.yaml"]);

        let mut w = ws(&dir);
        let plan = restore(&mut w, &id, &Scope::Whole, false, false).unwrap();
        assert_eq!(
            dispositions(&plan, Disposition::Overwrite),
            vec![
                Path::new("index.md"),
                Path::new("notes/a.md"),
                Path::new("notes/photo.jpg")
            ]
        );
        // The sidecar was never touched, so the restore has nothing to say about
        // it — and says so, rather than rewriting bytes that already match.
        assert_eq!(
            dispositions(&plan, Disposition::Unchanged),
            vec![Path::new("notes/photo.jpg.yaml")]
        );

        let event = block_on(w.history_event(Path::new("index.md"), &id))
            .unwrap()
            .unwrap();
        for file in &event.files {
            let bytes = std::fs::read(dir.join(&file.path)).unwrap();
            assert_eq!(
                crate::fixity::digest(&bytes),
                file.hash,
                "{} did not come back byte-exact",
                file.path.display()
            );
        }
        assert!(read(&dir, "index.md").contains("notes/a.md"));
        assert!(
            block_on(ws(&dir).check(Path::new("index.md")))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_default_restore_deletes_nothing_and_exact_makes_the_tree_match() {
        let dir = seed("restore-exact");
        let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
            panic!("the first capture must write an event");
        };

        // What a sync transport actually does: leaves a second file behind, linked
        // into the graph. Writing captured bytes over the top does not remove it —
        // which is the gap `--exact` exists to close, and why the default leaving
        // it is a deliberate decision rather than an oversight.
        write(
            &dir,
            "notes/a.sync-conflict-20260731.md",
            "---\ntitle: A (conflicted copy)\npart_of: '../index.md'\n---\nalpha\n",
        );
        relink_live(
            &dir,
            &[
                "notes/a.md",
                "notes/a.sync-conflict-20260731.md",
                "notes/photo.jpg.yaml",
            ],
        );

        // Both plans off the same damaged tree, so what differs between them is the
        // flag and nothing else. Taken before either runs, because the delete set is
        // drawn from the *reachable* files: the restored root stops linking the
        // conflict copy, and a plan computed afterwards would no longer see it.
        let mut w = ws(&dir);
        let root = Path::new("index.md");
        let event = block_on(w.history_event(root, &id)).unwrap().unwrap();
        let additive =
            block_on(w.history_restore_plan(root, &event, &Scope::Whole, false)).unwrap();
        let exact = block_on(w.history_restore_plan(root, &event, &Scope::Whole, true)).unwrap();

        assert_eq!(additive.count(Disposition::Remove), 0);
        block_on(w.history_restore(root, &additive, false)).unwrap();
        assert!(
            dir.join("notes/a.sync-conflict-20260731.md").exists(),
            "the default restore must delete nothing"
        );

        assert_eq!(
            exact.removals().collect::<Vec<_>>(),
            vec![Path::new("notes/a.sync-conflict-20260731.md")]
        );
        block_on(w.history_restore(root, &exact, false)).unwrap();
        assert!(!dir.join("notes/a.sync-conflict-20260731.md").exists());

        // The one subtree the mechanism is blind to survives its own exact
        // restore: an event that deleted every event newer than it would destroy
        // the recovery points themselves.
        let event = event_path(Path::new("history/index.md"), &id, "md").unwrap();
        assert!(dir.join(event).exists(), "the store must survive --exact");
        assert!(dir.join("history/blobs").exists());
    }

    #[test]
    fn restoring_the_state_the_workspace_already_holds_writes_nothing() {
        let dir = seed("restore-noop");
        let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
            panic!("the first capture must write an event");
        };
        let before = std::fs::metadata(dir.join("notes/a.md"))
            .unwrap()
            .modified()
            .unwrap();

        let mut w = ws(&dir);
        let plan = restore(&mut w, &id, &Scope::Whole, false, false).unwrap();
        assert!(plan.is_noop(), "every row already matches the capture");
        assert_eq!(plan.count(Disposition::Unchanged), plan.ops.len());
        assert_eq!(
            std::fs::metadata(dir.join("notes/a.md"))
                .unwrap()
                .modified()
                .unwrap(),
            before,
            "an unchanged row must not be rewritten"
        );
    }

    #[test]
    fn a_row_whose_blob_never_arrived_is_skipped_by_name_not_fatal() {
        // A manifest and the blobs it names travel over a transport separately, so
        // a half-synced event is ordinary rather than broken. The rows prov *can*
        // supply still come back; the one it cannot is reported.
        let dir = seed("restore-halfsynced");
        let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
            panic!("the first capture must write an event");
        };
        let payload = crate::fixity::digest(b"JPEGBYTES");
        std::fs::remove_file(dir.join(blob_path(Path::new("history/index.md"), &payload).unwrap()))
            .unwrap();
        write(&dir, "notes/a.md", "clobbered");
        write(&dir, "notes/photo.jpg", "truncated");

        let mut w = ws(&dir);
        let plan = restore(&mut w, &id, &Scope::Whole, false, false).unwrap();
        assert_eq!(
            dispositions(&plan, Disposition::NoBytes),
            vec![Path::new("notes/photo.jpg")]
        );
        assert_eq!(
            read(&dir, "notes/a.md"),
            "---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n"
        );
        assert_eq!(
            read(&dir, "notes/photo.jpg"),
            "truncated",
            "a row with no bytes is left alone, not emptied"
        );

        // Under `--exact` the same event is refused nothing: the delete pass is
        // drawn from the manifest's paths, and a row it cannot supply is still a
        // path the manifest holds — so nothing is removed on the strength of bytes
        // that merely have not arrived.
        let mut w = ws(&dir);
        let exact = restore(&mut w, &id, &Scope::Whole, true, false).unwrap();
        assert_eq!(exact.count(Disposition::Remove), 0);
        assert!(dir.join("notes/photo.jpg").exists());
    }

    #[test]
    fn a_restore_refuses_to_displace_a_registration_unless_it_resolves_it_itself() {
        let dir = seed("restore-collision");
        let mut w = ws(&dir);
        let id = Id("b7k2m".into());
        w.index_mut().register(&id, Path::new("notes/a.md"));
        let Captured::Written { id: event, .. } =
            block_on(w.history_capture(Path::new("index.md"), "2026-07-31T09:15:22Z", None))
                .unwrap()
        else {
            panic!("the first capture must write an event");
        };

        // The document moved after the capture. Restoring additively would put the
        // old path back and leave the new one there — two documents spelling one
        // id, which only their author can arbitrate.
        std::fs::rename(dir.join("notes/a.md"), dir.join("notes/b.md")).unwrap();
        relink_live(&dir, &["notes/b.md", "notes/photo.jpg.yaml"]);
        w.index_mut().set_path(&id, Path::new("notes/b.md"));

        let ev = block_on(w.history_event(Path::new("index.md"), &event))
            .unwrap()
            .unwrap();
        let plan =
            block_on(w.history_restore_plan(Path::new("index.md"), &ev, &Scope::Whole, false))
                .unwrap();
        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(plan.conflicts[0].path, Path::new("notes/a.md"));
        assert!(matches!(
            plan.conflicts[0].collision,
            Collision::Id { ref held_by, .. } if held_by == Path::new("notes/b.md")
        ));
        let err = block_on(w.history_restore(Path::new("index.md"), &plan, false)).unwrap_err();
        assert!(matches!(err, Error::Collision(Collision::Id { .. })));
        assert!(
            !dir.join("notes/a.md").exists(),
            "a refused restore must move nothing"
        );

        // `--exact` removes the document currently holding the id, so nothing is
        // displaced and the same restore is no longer a collision at all. This is
        // the difference between "put these bytes back too" and "make the tree
        // match this capture".
        let exact =
            block_on(w.history_restore_plan(Path::new("index.md"), &ev, &Scope::Whole, true))
                .unwrap();
        assert!(
            exact.conflicts.is_empty(),
            "a collision the restore itself resolves is not a collision: {:?}",
            exact.conflicts
        );
        assert_eq!(
            exact.removals().collect::<Vec<_>>(),
            vec![Path::new("notes/b.md")]
        );
        block_on(w.history_restore(Path::new("index.md"), &exact, false)).unwrap();
        assert!(dir.join("notes/a.md").exists());
        assert!(!dir.join("notes/b.md").exists());
    }

    #[test]
    fn a_scope_restores_a_slice_and_refuses_what_the_capture_never_held() {
        let dir = seed("restore-scope");
        let mut w = ws(&dir);
        let id = Id("b7k2m".into());
        w.index_mut().register(&id, Path::new("notes/a.md"));
        let Captured::Written { id: event, .. } =
            block_on(w.history_capture(Path::new("index.md"), "2026-07-31T09:15:22Z", None))
                .unwrap()
        else {
            panic!("the first capture must write an event");
        };
        write(&dir, "notes/a.md", "clobbered");
        write(&dir, "notes/photo.jpg", "truncated");

        // A directory scope takes everything the capture held beneath it; the root
        // above it is left alone.
        let ev = block_on(w.history_event(Path::new("index.md"), &event))
            .unwrap()
            .unwrap();
        let plan = block_on(w.history_restore_plan(
            Path::new("index.md"),
            &ev,
            &Scope::Paths(vec![PathBuf::from("notes")]),
            false,
        ))
        .unwrap();
        assert_eq!(plan.ops.len(), 3, "the three files under notes/");
        assert!(!plan.ops.iter().any(|op| op.path == Path::new("index.md")));

        // An id scope reaches the one document, wherever the capture found it.
        let by_id = block_on(w.history_restore_plan(
            Path::new("index.md"),
            &ev,
            &Scope::Id(id.clone()),
            false,
        ))
        .unwrap();
        assert_eq!(
            by_id
                .ops
                .iter()
                .map(|op| op.path.as_path())
                .collect::<Vec<_>>(),
            vec![Path::new("notes/a.md")]
        );
        block_on(w.history_restore(Path::new("index.md"), &by_id, false)).unwrap();
        assert!(read(&dir, "notes/a.md").contains("alpha"));
        assert_eq!(
            read(&dir, "notes/photo.jpg"),
            "truncated",
            "a scope restores only what it names"
        );

        // A scope that selects nothing is a typo, not an empty restore.
        for scope in [
            Scope::Paths(vec![PathBuf::from("notes/never.md")]),
            Scope::Id(Id("nosuch".into())),
        ] {
            assert!(
                block_on(w.history_restore_plan(Path::new("index.md"), &ev, &scope, false))
                    .is_err()
            );
        }

        // And `exact` is a statement about the whole tree, which a slice of the
        // capture cannot make.
        assert!(
            block_on(w.history_restore_plan(
                Path::new("index.md"),
                &ev,
                &Scope::Paths(vec![PathBuf::from("notes")]),
                true,
            ))
            .is_err()
        );
    }

    #[test]
    fn a_restored_root_never_strands_the_store_unreachable() {
        // A capture always records a root that already declares the store, so this
        // is the hand-edited (or foreign) case: a manifest whose root predates the
        // pointer. Restoring it verbatim would leave `history/` unreachable —
        // invisible to `check`, and unfindable by the next restore.
        let dir = seed("restore-pointer");
        let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
            panic!("the first capture must write an event");
        };
        let rootless =
            "---\ntitle: Home\ncontents:\n- notes/a.md\n- notes/photo.jpg.yaml\n---\nroot\n";
        let hash = crate::fixity::digest(rootless.as_bytes());
        let blob = blob_path(Path::new("history/index.md"), &hash).unwrap();
        std::fs::create_dir_all(dir.join(&blob).parent().unwrap()).unwrap();
        std::fs::write(dir.join(&blob), rootless).unwrap();

        let mut w = ws(&dir);
        let mut event = block_on(w.history_event(Path::new("index.md"), &id))
            .unwrap()
            .unwrap();
        for file in &mut event.files {
            if file.path == Path::new("index.md") {
                file.hash = hash.clone();
            }
        }
        let plan =
            block_on(w.history_restore_plan(Path::new("index.md"), &event, &Scope::Whole, false))
                .unwrap();
        block_on(w.history_restore(Path::new("index.md"), &plan, false)).unwrap();

        let root = read(&dir, "index.md");
        assert!(
            root.contains("history:"),
            "a restored root must still declare the store: {root}"
        );
        assert!(
            block_on(ws(&dir).history_path(Path::new("index.md")))
                .unwrap()
                .is_some()
        );
    }

    // Case-fold identity: the probe and the `exact` removal set agreeing.

    /// Whether `dir` sits on a filesystem that folds ASCII case for path
    /// lookups — probed empirically (this suite runs on APFS in development
    /// and ext4 in CI, and the two disagree) rather than assumed from
    /// `cfg(target_os)`, mirroring the production probe this exercises
    /// ([`Workspace::filesystem_case_folds`]). Every test below that depends on
    /// case-folding actually happening skips its case-insensitive-only
    /// assertions when this is `false`, so the suite stays green on Linux CI.
    fn case_insensitive_fs(dir: &Path) -> bool {
        let probe = dir.join(".case-probe.tmp");
        std::fs::write(&probe, b"x").unwrap();
        let collides = dir.join(".CASE-PROBE.tmp").exists();
        let _ = std::fs::remove_file(&probe);
        collides
    }

    /// The literal on-disk spelling of `rel`'s final component, read straight
    /// from its parent directory's listing — the same thing a restore's own
    /// [`Workspace::on_disk_identity`] reads, so a test can assert *which*
    /// casing survived rather than merely that *a* casing did.
    fn literal_name(dir: &Path, rel: &str) -> String {
        let rel = Path::new(rel);
        let entries = std::fs::read_dir(dir.join(rel.parent().unwrap())).unwrap();
        let want = rel.file_name().unwrap().to_string_lossy();
        entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .find(|name| name.eq_ignore_ascii_case(&want))
            .unwrap_or_else(|| panic!("no entry named {want} (any case) in {}", rel.display()))
    }

    #[test]
    fn an_exact_restore_spares_and_recases_a_row_that_only_differs_from_disk_by_case() {
        let dir = seed("restore-case-exact");
        let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
            panic!("the first capture must write an event");
        };
        if !case_insensitive_fs(&dir) {
            return;
        }

        // A sync client — or a user in Finder — renamed the file to a
        // different case after the capture. The manifest still spells it
        // `notes/a.md`. Restoring that old event with `--exact` is the exact
        // shape of the data-loss bug: the disposition probe used to find this
        // row `Unchanged` through the filesystem's own folding, while the
        // removal pass compared paths as literal strings and queued the very
        // same file for `Remove` — so the run deleted it and neither spelling
        // survived.
        std::fs::rename(dir.join("notes/a.md"), dir.join("notes/A.md")).unwrap();

        let mut w = ws(&dir);
        let plan = restore(&mut w, &id, &Scope::Whole, true, false).unwrap();

        assert_eq!(
            plan.removals().collect::<Vec<_>>(),
            Vec::<&Path>::new(),
            "a case-only rename must never be planned for removal under --exact"
        );
        assert_eq!(
            dispositions(&plan, Disposition::CaseOnly),
            vec![Path::new("notes/a.md")],
            "the bytes already matched; only the on-disk name's case did not"
        );
        assert_eq!(
            literal_name(&dir, "notes/a.md"),
            "a.md",
            "restore renames the file to the manifest's own spelling"
        );
        assert_eq!(
            read(&dir, "notes/a.md"),
            "---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n"
        );
    }

    #[test]
    fn an_additive_restore_recases_the_old_spelling_instead_of_silently_doing_nothing() {
        let dir = seed("restore-case-additive");
        let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
            panic!("the first capture must write an event");
        };
        if !case_insensitive_fs(&dir) {
            return;
        }
        std::fs::rename(dir.join("notes/a.md"), dir.join("notes/A.md")).unwrap();

        // The other edge the same bug left behind: without `--exact`, the old
        // probe found this row `Unchanged` and wrote nothing at all, so the
        // manifest's own spelling never came back — a restore that silently
        // no-ops on a row it was actually asked to restore.
        let mut w = ws(&dir);
        let plan = restore(&mut w, &id, &Scope::Whole, false, false).unwrap();

        assert!(
            !plan.is_noop(),
            "a case-only rename is a real change the plan must report, not silence"
        );
        assert_eq!(
            dispositions(&plan, Disposition::CaseOnly),
            vec![Path::new("notes/a.md")]
        );
        assert_eq!(literal_name(&dir, "notes/a.md"), "a.md");
    }

    #[test]
    fn an_overwrite_recases_too_when_the_on_disk_content_also_changed() {
        let dir = seed("restore-case-overwrite");
        let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
            panic!("the first capture must write an event");
        };
        if !case_insensitive_fs(&dir) {
            return;
        }
        std::fs::rename(dir.join("notes/a.md"), dir.join("notes/A.md")).unwrap();
        write(&dir, "notes/A.md", "clobbered by a sync conflict");

        let mut w = ws(&dir);
        let plan = restore(&mut w, &id, &Scope::Whole, false, false).unwrap();

        assert_eq!(
            dispositions(&plan, Disposition::Overwrite),
            vec![Path::new("notes/a.md")]
        );
        assert_eq!(
            literal_name(&dir, "notes/a.md"),
            "a.md",
            "an overwrite must fix the casing too, not just the content"
        );
        assert_eq!(
            read(&dir, "notes/a.md"),
            "---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n"
        );
    }

    #[test]
    fn a_foreign_event_naming_two_paths_that_differ_only_by_case_is_refused_exactly_where_the_filesystem_would_self_clobber()
     {
        // A manifest naming both spellings is a state only a case-sensitive
        // filesystem can capture — a normal capture here could never observe
        // both paths reachable at once. Simulated directly on the event rather
        // than on disk, since this filesystem could not produce it either.
        let dir = seed("restore-case-foreign");
        let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
            panic!("the first capture must write an event");
        };
        let w = ws(&dir);
        let mut event = block_on(w.history_event(Path::new("index.md"), &id))
            .unwrap()
            .unwrap();
        event.files.push(entry("notes/A.md", b"a different alpha"));

        let result =
            block_on(w.history_restore_plan(Path::new("index.md"), &event, &Scope::Whole, false));
        if case_insensitive_fs(&dir) {
            assert!(
                result.is_err(),
                "a case-colliding manifest must be refused on a filesystem that \
                 folds case — writing the second row would silently clobber the first"
            );
        } else {
            // The whole point: this fix must change nothing on a filesystem
            // that does not fold case, where the two paths are simply two
            // ordinary, unrelated files.
            assert!(
                result.is_ok(),
                "must not refuse on a filesystem that does not fold case: {result:?}"
            );
        }
    }
}
