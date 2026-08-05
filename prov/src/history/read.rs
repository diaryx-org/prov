use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::document::MetaCarrier;
use crate::error::{Error, Result};
use crate::fs::Storage;
use crate::index::IndexStore;
use crate::workspace::Workspace;

use super::docs::*;
use super::event_id::*;
use super::layout::*;
use super::model::*;
use super::paths::*;
use super::{EVENTS_DIR, HISTORY_DIR};

impl<FS: Storage, IdP, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// The extension the store's documents are authored with — the root
    /// document's own content format, falling back to Markdown when the root is a
    /// whole-file metadata document (which has no prose body to inherit).
    pub(super) fn history_ext(&self, root_doc: &Path) -> &'static str {
        crate::ContentFormat::from_extension(root_doc)
            .unwrap_or(crate::ContentFormat::Markdown)
            .extension()
    }

    /// The fenced-frontmatter archetype the store's documents are authored in —
    /// the workspace's own metadata format, so a fig workspace's history reads
    /// like the rest of it.
    pub(super) fn history_embed(&self) -> Result<fig::EmbedType> {
        match crate::document::frontmatter_carrier(self.default_embed_format()) {
            MetaCarrier::Fenced(embed) => Ok(embed),
            // `frontmatter_carrier` only ever returns a fenced archetype.
            _ => Err(Error::Structure(
                "history events need a fenced frontmatter carrier".into(),
            )),
        }
    }

    /// The store index document: the one the root's `history` pointer names, or —
    /// when the root declares none yet — where the first capture will put it.
    /// The `bool` is whether the store already exists.
    pub(super) async fn history_store_index(&self, root_doc: &Path) -> Result<(PathBuf, bool)> {
        Ok(match self.history_path(root_doc).await? {
            Some(path) => (path, true),
            None => (
                PathBuf::from(HISTORY_DIR).join(format!("index.{}", self.history_ext(root_doc))),
                false,
            ),
        })
    }

    /// The **capture set**: the live graph, minus prov's two byte-parking stores.
    ///
    /// [`reachable_files`](crate::Workspace::reachable_files) — §8's bounded walk, the
    /// same population `check` validates — with two exclusions, each load-bearing:
    ///
    /// - **`history/` itself.** It is reachable off the root, so a naive "capture
    ///   everything reachable" would capture the store inside the store: no
    ///   capture could ever be empty, and an exact restore of an old event would
    ///   delete every event newer than it, destroying the recovery points
    ///   themselves. The store is the one subtree the mechanism is deliberately
    ///   blind to.
    /// - **`recyclebin/items/`.** Already unreached, and excluded even so, on
    ///   purpose: bytes the user has consigned to the bin should not be *newly*
    ///   retained by a routine capture.
    ///
    /// - **The generated `about.md`.** It is *derived* — a pure function of the
    ///   configuration, which this same manifest captures — so parking its bytes
    ///   stores nothing that cannot be reproduced, and a new blob would be parked
    ///   on every config change for no recovery value. Restoring an event
    ///   restores the config that determines the page, and `check` reports the
    ///   page as stale until `prov about` rewrites it from that config, which is
    ///   the same repair by a shorter route. Excluding it also removes an
    ///   ordering hazard: the first capture *bootstraps* the store, which changes
    ///   what the page says about this workspace, so a captured page would be one
    ///   the capture itself invalidated.
    ///
    /// Everything else structural stays in — the registry, the config document,
    /// and the recycle bin's *index*. Capturing the bin index keeps the common
    /// case correct: a document live at capture time comes back live, and the bin
    /// index reverts to a state that does not list it.
    ///
    /// Returned in **manifest order** — [`path_sort_key`], byte-wise ascending on
    /// the joined path string (§3.1) — not the component-wise order
    /// [`reachable_files`](crate::Workspace::reachable_files)'s `BTreeSet<PathBuf>`
    /// iterates in. The two agree almost everywhere and disagree exactly where a
    /// file and a same-named directory are siblings (`notes.md` next to
    /// `notes/`), which is precisely the case a real workspace produces and a
    /// `Path`-ordered manifest would get wrong.
    pub async fn history_capture_set(&self, root_doc: &Path) -> Result<Vec<PathBuf>> {
        let (store_index, _) = self.history_store_index(root_doc).await?;
        let store = store_dir(&store_index);
        let binned = self
            .recycle_bin_path(root_doc)
            .await?
            .map(|index| store_dir(&index).join("items"));
        let about = self.about_path(root_doc).await?;
        let mut files: Vec<PathBuf> = self
            .reachable_files(root_doc)
            .await?
            .into_iter()
            .filter(|p| !under(p, &store))
            .filter(|p| binned.as_ref().is_none_or(|items| !under(p, items)))
            .filter(|p| about.as_ref().is_none_or(|about| p != about))
            .collect();
        files.sort_by_key(|p| path_sort_key(p));
        Ok(files)
    }

    /// Every event in the store, oldest first (by `created`, then id).
    ///
    /// Read by **scanning the shard directories**, not by following the index
    /// documents — the indexes are a rebuildable cache, so a mangled one must not
    /// be able to hide an event that is sitting right there. A document that does
    /// not parse, or that carries no manifest, is skipped rather than fatal.
    pub async fn history_list(&self, root_doc: &Path) -> Result<Vec<Event>> {
        let (store_index, exists) = self.history_store_index(root_doc).await?;
        if !exists {
            return Ok(Vec::new());
        }
        let (events, _) = self
            .history_events_in(&store_index, self.history_ext(root_doc))
            .await?;
        Ok(events)
    }

    /// [`history_list`](Self::history_list) against a store index already in hand —
    /// so a pass that has resolved the store once does not resolve it again
    /// through the root.
    ///
    /// Returns the events that loaded and parsed, oldest first, **alongside every
    /// event-shaped file that did not** — its path and why. [`shard_event_ids`]
    /// finds a file by name alone (§4's id shape plus the extension), so a
    /// document a transport tore in transit — half-written, or a conflict marker
    /// landing inside its frontmatter — is still counted as an event *slot* even
    /// though nothing in it can be trusted.
    ///
    /// The read-only callers ([`history_list`](Self::history_list),
    /// `history_show`, `history_log`) drop the second list on the floor: a
    /// degraded read is exactly what those verbs are for, and the store-format
    /// doc says so (§7, §10). The callers that *destroy* — `history_prune_plan`
    /// and `history_forget` — and the `check` sweep must not: a blob set built
    /// only from the survivors is a bound with an unknown hole in it, and a prune
    /// or forget that trusts it can free bytes a torn event was the only record
    /// of naming.
    pub(super) async fn history_events_in(
        &self,
        store_index: &Path,
        ext: &str,
    ) -> Result<(Vec<Event>, Vec<(PathBuf, String)>)> {
        let events_root = store_dir(store_index).join(EVENTS_DIR);
        let mut events = Vec::new();
        let mut unreadable = Vec::new();
        for year in self.subdirs(&events_root).await? {
            for month in self.subdirs(&events_root.join(&year)).await? {
                let shard = events_root.join(&year).join(&month);
                for id in self.shard_event_ids(&shard, ext).await? {
                    let path = shard.join(format!("{id}.{ext}"));
                    match self.load(&path).await {
                        Ok((_, doc)) => match parse_event(&path, &id, &doc.meta) {
                            Some(event) => events.push(event),
                            None => unreadable.push((
                                path,
                                "not a history event document (no `created` or `files`)"
                                    .to_string(),
                            )),
                        },
                        Err(e) => unreadable.push((path, e.to_string())),
                    }
                }
            }
        }
        // Normalized, not raw: a store mixes the precisions of every version that
        // ever wrote into it (see [`comparable`]). The id tiebreak survives for
        // the genuine tie — two devices landing on the same microsecond — where it
        // is arbitrary but deterministic, which is all an ordering owes a fork.
        events.sort_by(|a, b| {
            comparable(&a.created)
                .cmp(&comparable(&b.created))
                .then_with(|| a.id.cmp(&b.id))
        });
        unreadable.sort();
        Ok((events, unreadable))
    }

    /// Format the paths [`history_events_in`](Self::history_events_in) could not
    /// read, for a refusal message a destructive verb raises rather than acting
    /// on an incomplete reference set.
    pub(super) fn describe_unreadable(unreadable: &[(PathBuf, String)]) -> String {
        unreadable
            .iter()
            .map(|(path, error)| format!("{} ({error})", path.display()))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// One event by id, resolved through the **pure id → path function** rather
    /// than through any index — so an event answers for itself with every index
    /// document in the store destroyed.
    ///
    /// `Ok(None)` when the store holds no such event (including when there is no
    /// store yet). An error when `id` is not an event id at all, or when the
    /// document is sitting there but is not an event.
    pub async fn history_event(&self, root_doc: &Path, id: &str) -> Result<Option<Event>> {
        let (store_index, exists) = self.history_store_index(root_doc).await?;
        if !exists {
            return Ok(None);
        }
        let path = event_path(&store_index, id, self.history_ext(root_doc))?;
        if !self.fs().try_exists(&self.root().join(&path)).await? {
            return Ok(None);
        }
        let (_, doc) = self.load(&path).await?;
        parse_event(&path, id, &doc.meta)
            .map(Some)
            .ok_or_else(|| Error::Structure(format!("`{id}` is not a history event document")))
    }

    /// The captured paths in `event` whose pre-image bytes are **not** parked in
    /// the store — the "this event is half-synced" report.
    ///
    /// A manifest and the blobs it names travel over the transport
    /// independently, and a small event document routinely lands well before a
    /// hundred megabytes of bytes it points at. That is ordinary in-flight state
    /// rather than damage, which is exactly why it has to be legible under a
    /// *read* verb before anyone asks a restore to act on it — and why a restore
    /// reports this same set rather than computing its own.
    ///
    /// Presence is tested once per distinct hash, not once per row: a manifest
    /// routinely names one blob from several paths, and a workspace is captured
    /// whole. A row whose hash prov could not have parked in the first place
    /// (a foreign digest, a mangled string) names no blob that could be found, so
    /// it counts as missing rather than failing the whole read.
    pub async fn history_missing_blobs(
        &self,
        root_doc: &Path,
        event: &Event,
    ) -> Result<BTreeSet<PathBuf>> {
        let (store_index, _) = self.history_store_index(root_doc).await?;
        let mut seen: BTreeMap<&str, bool> = BTreeMap::new();
        let mut missing = BTreeSet::new();
        for file in &event.files {
            let present = match seen.get(file.hash.as_str()) {
                Some(present) => *present,
                None => {
                    let present = match blob_path(&store_index, &file.hash) {
                        Ok(blob) => self.fs().try_exists(&self.root().join(blob)).await?,
                        Err(_) => false,
                    };
                    seen.insert(&file.hash, present);
                    present
                }
            };
            if !present {
                missing.insert(file.path.clone());
            }
        }
        Ok(missing)
    }

    /// One document's lineage across every capture, oldest first: pull its row
    /// out of each manifest in turn, and keep only the events where that row
    /// *changed*.
    ///
    /// This is the payoff for the manifest's `id` column, and it is a **derived
    /// query, not a storage design** — nothing in the store is keyed by document,
    /// and nothing here writes. Following a [`Subject::Id`] makes the lineage
    /// rename-robust in a way no path-keyed store can be: a move shows as one
    /// document that changed path, where a path-keyed view shows two unrelated
    /// lineages that happen to abut.
    ///
    /// Consecutive events are deduped on the **whole manifest row** — path, id
    /// and hash — not on the hash alone. A rename leaves the bytes
    /// byte-identical, so a hash-only dedupe would swallow precisely the event
    /// that following an id exists to surface. Including the id means a document
    /// acquiring one is a point too, which is right: the row changed.
    ///
    /// An event that does not mention the subject records [`Presence::Gone`], but
    /// only once the document has been seen, so a lineage starts where its
    /// document does rather than with a run of absences. Events are walked in
    /// capture order (`created`, then id), so concurrent captures on two devices
    /// interleave rather than branching — this is a display, and `history-list`
    /// is where forks are named.
    ///
    /// Cost is one pass over every event document in the store. That is the
    /// honest price of storing by consistent cut and querying by document, and it
    /// is why this is a query rather than an index.
    pub async fn history_log(&self, root_doc: &Path, subject: &Subject) -> Result<Vec<Version>> {
        let mut log: Vec<Version> = Vec::new();
        for event in self.history_list(root_doc).await? {
            let row = event.files.iter().find(|file| match subject {
                Subject::Id(id) => file.id.as_ref() == Some(id),
                Subject::Path(path) => &file.path == path,
            });
            let state = match row {
                Some(file) => Presence::At {
                    path: file.path.clone(),
                    id: file.id.clone(),
                    hash: file.hash.clone(),
                },
                None => Presence::Gone,
            };
            match log.last() {
                // The document did not exist yet when this capture was taken.
                None if state == Presence::Gone => continue,
                Some(previous) if previous.state == state => continue,
                _ => {}
            }
            log.push(Version {
                event: event.id,
                created: event.created,
                label: event.label,
                state,
            });
        }
        Ok(log)
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::super::support::*;
    use super::*;
    use crate::exec::block_on;

    #[test]
    fn an_event_resolves_by_id_with_every_index_destroyed() {
        let dir = seed("show-resolve");
        let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", Some("pre-sync"))
        else {
            panic!("the first capture must write an event");
        };
        // The indexes are a cache. Burn all three; the id still resolves, because
        // its path is a pure function of it.
        for index in [
            "history/index.md",
            "history/events/2026/index.md",
            "history/events/2026/07/index.md",
        ] {
            std::fs::remove_file(dir.join(index)).unwrap();
        }
        let event = block_on(ws(&dir).history_event(Path::new("index.md"), &id))
            .unwrap()
            .expect("the event must resolve without any index");
        assert_eq!(event.id, id);
        assert_eq!(event.label.as_deref(), Some("pre-sync"));
        assert_eq!(event.files.len(), 4);

        // An id that names nothing is absence, not an error; a string that is not
        // an event id at all is an error.
        assert!(
            block_on(ws(&dir).history_event(Path::new("index.md"), "2026-07-31-0000-deadbeef"))
                .unwrap()
                .is_none()
        );
        assert!(block_on(ws(&dir).history_event(Path::new("index.md"), "yesterday")).is_err());
    }

    #[test]
    fn missing_blobs_name_the_paths_a_restore_could_not_recover() {
        let dir = seed("show-blobs");
        let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
            panic!("the first capture must write an event");
        };
        let event = block_on(ws(&dir).history_event(Path::new("index.md"), &id))
            .unwrap()
            .unwrap();
        assert!(
            block_on(ws(&dir).history_missing_blobs(Path::new("index.md"), &event))
                .unwrap()
                .is_empty(),
            "a capture parks every file's bytes"
        );

        // The half-synced case: the event document arrived, one blob did not.
        let payload = crate::fixity::digest(b"JPEGBYTES");
        let blob = blob_path(Path::new("history/index.md"), &payload).unwrap();
        std::fs::remove_file(dir.join(&blob)).unwrap();
        let missing =
            block_on(ws(&dir).history_missing_blobs(Path::new("index.md"), &event)).unwrap();
        assert_eq!(
            missing.into_iter().collect::<Vec<_>>(),
            vec![PathBuf::from("notes/photo.jpg")],
            "only the file whose bytes are gone should be reported"
        );

        // A row prov could never have parked reports as missing rather than
        // failing the read — a foreign event must stay legible.
        let foreign = Event {
            files: vec![FileEntry {
                path: PathBuf::from("notes/a.md"),
                id: None,
                hash: "blake3:beef".into(),
            }],
            ..event
        };
        assert_eq!(
            block_on(ws(&dir).history_missing_blobs(Path::new("index.md"), &foreign))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn read_only_verbs_keep_degrading_gracefully_around_an_unreadable_event() {
        // §7's flip side, restated as a test: the destructive verbs and `check`
        // must refuse or report, but `history-list` (and anything built on it)
        // has always been allowed to skip what it cannot read — that is
        // graceful degradation, not the destruction this fix guards against.
        let dir = seed("list-torn");
        let first = capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
        let second = capture_edited(&dir, "2026-07-31T10:00:00.000000Z", "two", "beta");
        let torn = event_path(Path::new("history/index.md"), &first, "md").unwrap();
        tear(&dir, torn.to_str().unwrap());

        let events = block_on(ws(&dir).history_list(Path::new("index.md"))).unwrap();
        assert_eq!(
            events.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec![second.as_str()],
            "a read still answers with whatever it could parse"
        );
    }

    /// Re-point the root at `contents`, so a rename is visible to the reachable
    /// walk the capture set is taken from.
    fn relink(dir: &Path, contents: &[&str]) {
        let list = contents
            .iter()
            .map(|c| format!("- {c}\n"))
            .collect::<String>();
        write(
            dir,
            "index.md",
            &format!("---\ntitle: Home\ncontents:\n{list}---\nroot\n"),
        );
    }

    #[test]
    fn a_lineage_follows_an_id_through_a_rename_no_path_key_could() {
        let dir = seed("log-rename");
        let mut w = ws(&dir);
        let id = Id("b7k2m".into());
        w.index_mut().register(&id, Path::new("notes/a.md"));
        let take = |w: &mut Workspace<StdFs, Minter, FileIndex>, now: &str| {
            block_on(w.history_capture(Path::new("index.md"), now, None)).unwrap()
        };
        take(&mut w, "2026-07-31T09:00:00Z");

        // The move: same bytes, new path. A path-keyed store shows two unrelated
        // lineages here; the id column shows one document that moved.
        std::fs::rename(dir.join("notes/a.md"), dir.join("notes/b.md")).unwrap();
        relink(&dir, &["notes/b.md", "notes/photo.jpg.yaml"]);
        w.index_mut().set_path(&id, Path::new("notes/b.md"));
        take(&mut w, "2026-07-31T10:00:00Z");

        // An edit at the new path.
        write(
            &dir,
            "notes/b.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\nrevised\n",
        );
        take(&mut w, "2026-07-31T11:00:00Z");

        // …and a capture that changes nothing about this document, which must not
        // add a point to its lineage.
        write(&dir, "notes/photo.jpg", "OTHERBYTES");
        take(&mut w, "2026-07-31T12:00:00Z");

        let log = block_on(w.history_log(Path::new("index.md"), &Subject::Id(id.clone()))).unwrap();
        let paths: Vec<&Path> = log
            .iter()
            .map(|v| match &v.state {
                Presence::At { path, .. } => path.as_path(),
                Presence::Gone => Path::new("(gone)"),
            })
            .collect();
        assert_eq!(
            paths,
            vec![
                Path::new("notes/a.md"),
                Path::new("notes/b.md"),
                Path::new("notes/b.md")
            ],
            "the move must be a point in the lineage, and the untouched capture must not"
        );
        // Deduping on the hash alone would have swallowed the move: the bytes did
        // not change when the path did.
        let (Presence::At { hash: first, .. }, Presence::At { hash: second, .. }) =
            (&log[0].state, &log[1].state)
        else {
            panic!("both points should be present states");
        };
        assert_eq!(first, second, "a rename leaves the bytes identical");

        // The same document asked for by its old *path*: the lineage fragments at
        // the move, which is the nature of a path key. But the row it does find
        // still remembers the id — which is what lets the weaker query hand the
        // caller the stronger one instead of quietly under-reporting.
        let by_path = block_on(w.history_log(
            Path::new("index.md"),
            &Subject::Path(PathBuf::from("notes/a.md")),
        ))
        .unwrap();
        assert!(matches!(
            &by_path[0].state,
            Presence::At { id: Some(found), .. } if *found == id
        ));
        assert_eq!(
            by_path.last().unwrap().state,
            Presence::Gone,
            "a path-keyed lineage sees the move as the document disappearing"
        );
    }

    #[test]
    fn a_lineage_records_a_deletion_and_a_return() {
        let dir = seed("log-gone");
        let mut w = ws(&dir);
        let id = Id("b7k2m".into());
        w.index_mut().register(&id, Path::new("notes/a.md"));
        let take = |w: &mut Workspace<StdFs, Minter, FileIndex>, now: &str| {
            block_on(w.history_capture(Path::new("index.md"), now, None)).unwrap()
        };
        take(&mut w, "2026-07-31T09:00:00Z");

        // Out of the reachable graph and off disk.
        std::fs::remove_file(dir.join("notes/a.md")).unwrap();
        relink(&dir, &["notes/photo.jpg.yaml"]);
        take(&mut w, "2026-07-31T10:00:00Z");

        // Back again — which is what a restore looks like from the lineage's side.
        write(
            &dir,
            "notes/a.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n",
        );
        relink(&dir, &["notes/a.md", "notes/photo.jpg.yaml"]);
        take(&mut w, "2026-07-31T11:00:00Z");

        let log = block_on(w.history_log(Path::new("index.md"), &Subject::Id(id))).unwrap();
        assert_eq!(log.len(), 3);
        assert!(matches!(log[0].state, Presence::At { .. }));
        // Omission *is* deletion: there is no removal list to have consulted.
        assert_eq!(log[1].state, Presence::Gone);
        assert!(matches!(log[2].state, Presence::At { .. }));
        assert_eq!(log[2].created, "2026-07-31T11:00:00Z");
    }

    #[test]
    fn an_id_less_document_still_has_a_lineage_by_path() {
        // The documents with no id — the config document, the registry, the bin
        // index, an attachment payload — are disproportionately what a sync
        // transport damages, so the weaker key has to work.
        let dir = seed("log-path");
        capture(&dir, "2026-07-31T09:00:00Z", None);
        write(&dir, "notes/photo.jpg", "OTHERBYTES");
        capture(&dir, "2026-07-31T10:00:00Z", None);

        let log = block_on(ws(&dir).history_log(
            Path::new("index.md"),
            &Subject::Path(PathBuf::from("notes/photo.jpg")),
        ))
        .unwrap();
        assert_eq!(log.len(), 2, "the payload's bytes changed once");
        let Presence::At { hash, .. } = &log[1].state else {
            panic!("the payload should be present in the second event");
        };
        assert_eq!(*hash, crate::fixity::digest(b"OTHERBYTES"));

        // A subject no event ever captured has an empty lineage, not an error.
        assert!(
            block_on(ws(&dir).history_log(
                Path::new("index.md"),
                &Subject::Path(PathBuf::from("notes/never.md")),
            ))
            .unwrap()
            .is_empty()
        );
    }
}
