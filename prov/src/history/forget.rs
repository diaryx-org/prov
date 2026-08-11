use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::workspace::Workspace;
use prov_graph::error::{Error, Result};
use prov_graph::fs::Storage;
use prov_graph::index::IndexStore;
use prov_graph::link;

use prov_history::*;

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
        if let Ok(entries) = self.listing(&dir).await {
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
        let ext = prov_graph::document::whole_file_extension(self.default_embed_format());
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
        let (store_index, found) = self.history_store_index(root_doc).await?;
        if !found.exists() {
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
        let (store_index, found) = self.history_store_index(&root_doc).await?;
        if !found.exists() {
            return Ok(Forgotten::default());
        }
        let style = self.history_authoring(&root_doc)?;
        let ext = style.ext.as_str();

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
            if self.exists(&blob).await? {
                bytes += match self.stat(&blob).await {
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
            render_store_index(&years, Some(&forgotten_path), &style)?,
        )
        .await?;
        self.commit(cs).await?;

        for blob in &blobs {
            if self.exists(blob).await? {
                crate::change::discard_file(self.fs(), self.root(), blob).await?;
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

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::super::support::*;
    use super::*;
    use crate::validate::Finding;
    use prov_graph::exec::block_on;

    fn forget(dir: &Path, subject: &Subject, now: &str, force: bool) -> Result<Forgotten> {
        block_on(ws(dir).history_forget(Path::new("index.md"), subject, now, force))
    }

    #[test]
    fn a_forget_destroys_only_the_bytes_nothing_else_names() {
        let dir = seed("forget-basic");
        // Two documents with byte-identical content, so one hash is shared — the
        // case content addressing makes possible and a naive "delete every hash
        // this path ever had" would get catastrophically wrong.
        let shared = "---\ntitle: Same\npart_of: '../index.md'\n---\ntwin\n";
        write(&dir, "notes/twin.md", shared);
        write(&dir, "notes/other.md", shared);
        relink_live(
            &dir,
            &[
                "notes/a.md",
                "notes/twin.md",
                "notes/other.md",
                "notes/photo.jpg.yaml",
            ],
        );
        capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
        // A second version of the doomed document, so forget has to reach every
        // hash it ever had rather than only the newest.
        write(
            &dir,
            "notes/twin.md",
            "---\ntitle: Same\npart_of: '../index.md'\n---\nrevised\n",
        );
        capture_edited(&dir, "2026-07-31T10:00:00.000000Z", "two", "beta");

        // Out of the workspace first: forget refuses a live document, and the
        // point here is what it destroys, not that guard.
        std::fs::remove_file(dir.join("notes/twin.md")).unwrap();
        relink_live(
            &dir,
            &["notes/a.md", "notes/other.md", "notes/photo.jpg.yaml"],
        );

        let revised = blob_of(b"---\ntitle: Same\npart_of: '../index.md'\n---\nrevised\n");
        assert!(dir.join(&revised).exists());
        let out = forget(
            &dir,
            &Subject::Path(PathBuf::from("notes/twin.md")),
            "2026-08-01T12:00:00.000000Z",
            false,
        )
        .unwrap();

        assert_eq!(out.blobs, vec![revised.clone()]);
        assert!(!dir.join(&revised).exists(), "the unique version must go");
        assert_eq!(
            out.shared.len(),
            1,
            "the version it shares with notes/other.md survives, and is reported"
        );
        assert!(
            dir.join(blob_of(shared.as_bytes())).exists(),
            "forgetting one document must not reach into another's history"
        );
        assert!(out.bytes > 0);

        // The record of *what was captured* survives the destruction of the bytes.
        // That is the bargain, and it has to be visible in the store.
        let events = block_on(ws(&dir).history_list(Path::new("index.md"))).unwrap();
        assert!(
            events
                .iter()
                .any(|e| e.files.iter().any(|f| f.path == Path::new("notes/twin.md"))),
            "events are immutable: the manifest still names it"
        );

        // Tombstoned, reachable, and clean — the record must not itself be an
        // orphan, and a deliberate destruction must not leave `check` failing.
        let tombstone = read(&dir, "history/forgotten.yaml");
        assert!(tombstone.contains("notes/twin.md") && tombstone.contains("2026-08-01T12:00:00"));
        assert!(read(&dir, "history/index.md").contains("forgotten.yaml"));
        assert_eq!(
            block_on(ws(&dir).check(Path::new("index.md"))).unwrap(),
            vec![]
        );
    }

    #[test]
    fn a_tombstoned_hash_is_accounted_for_where_a_lost_one_is_not() {
        let dir = seed("forget-findings");
        capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
        std::fs::remove_file(dir.join("notes/a.md")).unwrap();
        relink_live(&dir, &["notes/photo.jpg.yaml"]);

        forget(
            &dir,
            &Subject::Path(PathBuf::from("notes/a.md")),
            "2026-08-01T12:00:00.000000Z",
            false,
        )
        .unwrap();
        assert!(
            block_on(ws(&dir).check(Path::new("index.md")))
                .unwrap()
                .is_empty(),
            "a recorded destruction is not a finding — a `check` that never came \
             back to clean would teach the user to stop reading it"
        );
        assert_eq!(
            block_on(ws(&dir).history_forgotten(Path::new("index.md")))
                .unwrap()
                .len(),
            1
        );

        // …and the suppression is precise, not blanket: bytes that went missing
        // without a record still say so.
        std::fs::remove_file(dir.join(blob_of(b"JPEGBYTES"))).unwrap();
        let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
        assert!(
            matches!(findings.as_slice(), [Finding::HistoryBlobMissing { paths, .. }]
                if paths == &[PathBuf::from("notes/photo.jpg")]),
            "{findings:?}"
        );
    }

    #[test]
    fn a_forget_refuses_a_document_the_next_capture_would_park_again() {
        let dir = seed("forget-live");
        capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");

        let subject = Subject::Path(PathBuf::from("notes/a.md"));
        let err = forget(&dir, &subject, "2026-08-01T12:00:00.000000Z", false).unwrap_err();
        assert!(
            err.to_string().contains("notes/a.md")
                && err.to_string().contains("still in the workspace"),
            "the refusal has to name the document and say why: {err}"
        );
        assert!(
            dir.join(blob_of(
                b"---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n"
            ))
            .exists(),
            "a refused forget destroys nothing"
        );

        // Forced, for the deliberate "purge the history, keep the file" case.
        let out = forget(&dir, &subject, "2026-08-01T12:00:00.000000Z", true).unwrap();
        assert!(!out.is_empty());
    }

    #[test]
    fn forgetting_by_id_reaches_the_versions_a_path_key_would_miss() {
        let dir = seed("forget-id");
        let mut w = ws(&dir);
        let id = Id("b7k2m".into());
        w.index_mut().register(&id, Path::new("notes/a.md"));
        block_on(w.history_capture(Path::new("index.md"), "2026-07-31T09:00:00.000000Z", None))
            .unwrap();

        // The move: the same document, a second path, and a hash a path-keyed
        // forget would leave behind.
        std::fs::rename(dir.join("notes/a.md"), dir.join("notes/b.md")).unwrap();
        write(
            &dir,
            "notes/b.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\nrevised\n",
        );
        relink_live(&dir, &["notes/b.md", "notes/photo.jpg.yaml"]);
        w.index_mut().set_path(&id, Path::new("notes/b.md"));
        block_on(w.history_capture(Path::new("index.md"), "2026-07-31T10:00:00.000000Z", None))
            .unwrap();

        // Out of the workspace, so the guard is not what is under test.
        std::fs::remove_file(dir.join("notes/b.md")).unwrap();
        relink_live(&dir, &["notes/photo.jpg.yaml"]);
        w.index_mut().unregister(&id);

        let out = block_on(w.history_forget(
            Path::new("index.md"),
            &Subject::Id(id),
            "2026-08-01T12:00:00.000000Z",
            false,
        ))
        .unwrap();
        assert_eq!(out.hashes.len(), 2, "both versions, across the rename");
        assert!(
            !dir.join(blob_of(
                b"---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n"
            ))
            .exists()
        );
        assert!(
            !dir.join(blob_of(
                b"---\ntitle: A\npart_of: '../index.md'\n---\nrevised\n"
            ))
            .exists()
        );
    }

    #[test]
    fn a_forget_refuses_while_any_event_is_unreadable() {
        // Same bug, `history-forget`'s side: `others` built only from the events
        // that parsed can miss a hash the torn event shared with the subject,
        // so a hash that should have survived (named elsewhere) reads as
        // belonging only to the subject and gets destroyed.
        let dir = seed("forget-torn");
        let first = capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
        capture_edited(&dir, "2026-07-31T10:00:00.000000Z", "two", "beta");
        let torn = event_path(Path::new("history/index.md"), &first, "md").unwrap();
        tear(&dir, torn.to_str().unwrap());

        std::fs::remove_file(dir.join("notes/a.md")).unwrap();
        relink_live(&dir, &["notes/photo.jpg.yaml"]);

        let err = forget(
            &dir,
            &Subject::Path(PathBuf::from("notes/a.md")),
            "2026-08-01T12:00:00.000000Z",
            false,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains(torn.to_str().unwrap()),
            "the refusal has to name the file that could not be read: {err}"
        );
        assert!(
            dir.join(blob_of(
                b"---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n"
            ))
            .exists(),
            "a refused forget destroys nothing"
        );
    }
}
