use std::path::{Path, PathBuf};

use crate::change::ChangeSet;
use crate::content::transcode;
use crate::error::Result;
use crate::fs::Storage;
use crate::index::IndexStore;
use crate::link;
use crate::meta::{Mapping, Value};
use crate::workspace::Workspace;

use super::docs::*;
use super::event_id::*;
use super::layout::*;
use super::model::*;
use super::paths::*;
use super::{EVENTS_DIR, TRIGGER_MANUAL};

impl<FS: Storage, IdP, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Capture the workspace: hash the capture set, park newly-seen blobs, and
    /// write one immutable event document into its `<YYYY>/<MM>` shard.
    ///
    /// `now` is the caller-supplied RFC 3339 UTC timestamp (the CLI passes the
    /// current time). The library takes it as an argument rather than reading a
    /// clock, so the op stays deterministic — the same convention `recycle` uses.
    ///
    /// **Adds files only**, except the current month's rebuildable index (and, on
    /// a new month or year, the shard index above it — itself pure addition). If
    /// the computed manifest equals the newest existing event's, nothing is
    /// written and [`Captured::Unchanged`] names the event that already describes
    /// this state.
    ///
    /// ## Why blobs do not ride the change set
    ///
    /// The event document and the shard indexes are staged in one journaled
    /// [`ChangeSet`], because they must land together. **Blobs are not**: the
    /// journal embeds file contents ([`crate::journal::encode`]), so a genesis
    /// capture riding the change set would write a second whole copy of the
    /// workspace into `.prov-journal`. They go through
    /// [`Storage::write_atomic`] directly instead, which is safe precisely
    /// because a content-addressed write is idempotent — replaying it can only
    /// write the same bytes to the same path.
    ///
    /// Blobs are parked *before* the change set lands, so the failure mode is an
    /// orphaned blob (reported by `check`, collected by `history-prune`) rather
    /// than an event whose bytes are missing.
    pub async fn history_capture(
        &mut self,
        root_doc: &Path,
        now: &str,
        label: Option<&str>,
    ) -> Result<Captured> {
        // The capture set comes from a full walk of the graph, and the manifest
        // loop then visits every file that walk found. One scope over both means
        // a document is read once rather than once per pass. Dropped explicitly
        // where the writing half begins — which is both what the borrow checker
        // needs and exactly the right boundary: nothing after that point reads a
        // document, and everything after it changes one.
        let scope = self.read_scope();
        let root_doc = link::normalize(root_doc);
        let style = self.history_authoring(&root_doc)?;
        let ext = style.ext.as_str();
        let (store_index, found) = self.history_store_index(&root_doc).await?;
        let label = label.map(str::trim).filter(|l| !l.is_empty());

        // Bootstrapping the store *edits the root* (it gains the `history`
        // pointer), so that edit is computed up front and the manifest hashes the
        // post-edit bytes. Otherwise the very first event would record a root
        // predating its own store — and restoring it exactly would strand the
        // store unreachable, which is the one thing a restore must never do.
        //
        // A store found at the conventional path but *not* declared gets the same
        // edit, which adopts it rather than bootstrapping a second one over the
        // top: the pointer is what a lost root line took away, and capturing is
        // exactly when to put it back.
        let root_pointer = match found {
            StoreLocation::Declared => None,
            _ => Some(self.history_pointer_text(&root_doc, &store_index).await?),
        };

        // The manifest: one row per captured file, in manifest order — the
        // capture set is already sorted that way (§3.1), so the manifest
        // inherits it rather than sorting again here.
        //
        // Each file's bytes are hashed and parked in the same pass, then dropped —
        // a workspace is captured whole, so accumulating every file's contents to
        // park them afterwards would hold the entire workspace in memory. Parking
        // *before* the change set lands also fixes the failure mode the right way
        // round: an interrupted capture leaves an orphaned blob (reported by
        // `check`, collected by `history-prune`) rather than an event whose bytes
        // are missing.
        let mut files = Vec::new();
        let mut parked = 0usize;
        for path in self.history_capture_set(&root_doc).await? {
            // The root's post-edit text is computed here, not on disk. It has no
            // stat to validate a remembered digest against, so it is never
            // served from the cache and never recorded into it.
            let staged = match &root_pointer {
                Some(text) if path == root_doc => Some(text.clone().into_bytes()),
                _ => None,
            };
            let full = self.root().join(&path);
            // One stat, and on a hit it is the *only* thing this file costs: no
            // read, and no pass over its bytes. A stat this pass has to be able
            // to afford, since the alternative it replaces is reading the file.
            let meta = match staged {
                Some(_) => None,
                None => self.fs().metadata(&full).await.ok(),
            };
            let remembered = match &meta {
                Some(meta) => self.fixity_cached(&path, meta),
                None => None,
            };

            let hash = match remembered {
                // A remembered digest is trusted only when the blob it names is
                // already parked. That is what keeps a stale entry survivable:
                // the bytes at that address are on disk and were hashed from a
                // real file when they got there, so the worst a wrong answer can
                // do is record an event that misdescribes this instant — never
                // park bytes under an address that is not their digest.
                Some(hash)
                    if self
                        .fs()
                        .try_exists(&self.root().join(blob_path(&store_index, &hash)?))
                        .await? =>
                {
                    hash
                }
                // Everything else reads the file — and hashes the bytes it read,
                // so a digest prov writes down is always a digest of bytes prov
                // has actually seen.
                _ => {
                    let bytes = match staged {
                        Some(bytes) => bytes,
                        None => self.fs().read(&full).await?,
                    };
                    let hash = crate::fixity::digest(&bytes);
                    // Content-addressed, so a hash already on disk *is* the same
                    // bytes — nothing to rewrite, and two devices parking the
                    // same content converge instead of conflicting.
                    let blob = self.root().join(blob_path(&store_index, &hash)?);
                    if !self.fs().try_exists(&blob).await? {
                        if let Some(dir) = blob.parent() {
                            self.fs().create_dir_all(dir).await?;
                        }
                        self.fs().write_atomic(&blob, &bytes).await?;
                        parked += 1;
                    }
                    if let Some(meta) = &meta {
                        self.fixity_remember(&path, meta, &hash);
                    }
                    hash
                }
            };
            let id = self.index().id_for_path(&path);
            files.push(FileEntry { path, id, hash });
        }

        // The newest local event: what a new capture compares against, and what
        // it records as `parent` (display metadata — nothing computes through it).
        //
        // Compared by `manifest_of`, not `==` on the `Vec`: `previous` may be an
        // event a pre-fix writer (or a store synced in from elsewhere) wrote with
        // its rows in `Path`'s component order rather than §3.1's, and "the
        // computed manifest is identical" (§6) means the same paths, ids and
        // hashes — not the same row order.
        let existing = self.history_list(&root_doc).await?;
        let newest = existing.last();
        if let Some(previous) = newest
            && manifest_of(&previous.files) == manifest_of(&files)
        {
            return Ok(Captured::Unchanged {
                id: previous.id.clone(),
            });
        }
        let parent = newest.map(|e| e.id.as_str());
        let id = mint_id(now, TRIGGER_MANUAL, label, parent, &files)?;
        let event_rel = event_path(&store_index, &id, ext)?;

        let diff = newest.map(|previous| {
            let event = Event {
                id: id.clone(),
                path: event_rel.clone(),
                created: now.to_string(),
                trigger: TRIGGER_MANUAL.to_string(),
                label: label.map(str::to_owned),
                parent: parent.map(str::to_owned),
                files: files.clone(),
            };
            event.diff(previous)
        });

        // The event document. `part_of` points at its own shard index; the event
        // carries no `id` field — minting registry ids for events would make every
        // capture write `registry.<ext>`, the conflict-prone shape this store
        // exists to avoid.
        let mut map = Mapping::new();
        map.insert(
            "part_of".into(),
            Value::String(format!("[{}](index.{ext})", shard_title(&id))),
        );
        map.insert("created".into(), Value::String(now.to_string()));
        map.insert("trigger".into(), Value::String(TRIGGER_MANUAL.to_string()));
        if let Some(label) = label {
            map.insert("label".into(), Value::String(label.to_string()));
        }
        if let Some(parent) = parent {
            map.insert("parent".into(), Value::String(parent.to_string()));
        }
        map.insert(
            "files".into(),
            Value::Sequence(
                files
                    .iter()
                    .map(|f| {
                        let mut row = Mapping::new();
                        row.insert("path".into(), Value::String(slash_path(&f.path)));
                        if let Some(id) = &f.id {
                            row.insert("id".into(), Value::String(id.0.clone()));
                        }
                        row.insert("hash".into(), Value::String(f.hash.clone()));
                        Value::Mapping(row)
                    })
                    .collect(),
            ),
        );
        let summary = match diff {
            Some((changed, removed)) => format!(
                "Captured {} file(s) — {changed} changed, {removed} removed since \
                 the previous event.",
                files.len()
            ),
            None => format!(
                "Captured {} file(s). This is the first event in the store.",
                files.len()
            ),
        };
        let body = format!(
            "# History — {}\n\n{summary}\n\nRoll the workspace back to this point with:\n\n    \
             prov history-restore {id}\n",
            Event {
                id: id.clone(),
                path: event_rel.clone(),
                created: now.to_string(),
                trigger: TRIGGER_MANUAL.to_string(),
                label: label.map(str::to_owned),
                parent: None,
                files: Vec::new(),
            }
            .describe()
        );
        let event_text =
            crate::edit::reformat_block(&transcode(&body, style.content)?, &map, style.embed)?;

        drop(scope);
        let mut cs = self.change();
        cs.write(&event_rel, event_text);
        self.stage_history_indexes(&mut cs, &store_index, &id, &style)
            .await?;
        if let Some(text) = root_pointer {
            cs.write(&root_doc, text);
        }
        self.commit(cs).await?;

        Ok(Captured::Written {
            id,
            files: files.len(),
            blobs: parked,
            diff,
        })
    }

    /// Stage a rebuild of every index document on the path from the store root
    /// down to `id`'s month shard, each rendered from its own directory listing
    /// (plus the event this capture is adding, which is not on disk yet).
    ///
    /// Rebuilding rather than surgically appending is what keeps "the index is a
    /// cache" honest: capture and the [`Fix::RebuildHistoryIndex`] autofix run the
    /// same code, so a repaired index is byte-identical to a freshly written one.
    ///
    /// [`Fix::RebuildHistoryIndex`]: crate::Fix::RebuildHistoryIndex
    async fn stage_history_indexes(
        &self,
        cs: &mut ChangeSet,
        store_index: &Path,
        id: &str,
        style: &Authoring,
    ) -> Result<()> {
        let ext = style.ext.as_str();
        let shard = shard_of(id)?;
        let (year, month) = shard_parts(&shard)?;
        let events_root = store_dir(store_index).join(EVENTS_DIR);
        let shard_dir = events_root.join(&shard);

        let mut ids = self.shard_event_ids(&shard_dir, ext).await?;
        ids.insert(id.to_string());
        cs.write(
            shard_dir.join(format!("index.{ext}")),
            render_month_index(&year, &month, &ids, style)?,
        );

        let mut months = self.event_months(&events_root.join(&year), ext).await?;
        months.insert(month.clone());
        cs.write(
            events_root.join(&year).join(format!("index.{ext}")),
            render_year_index(&year, &months, style)?,
        );

        let mut years = self.event_years(&events_root, ext).await?;
        years.insert(year.clone());
        let forgotten = self.history_forgotten_link(store_index).await?;
        cs.write(
            store_index,
            render_store_index(&years, forgotten.as_deref(), style)?,
        );
        Ok(())
    }

    /// The literal on-disk spelling `path` resolves to, or `None` if nothing
    /// does.
    ///
    /// `try_exists` alone cannot say *which* spelling: on a case-insensitive
    /// filesystem it resolves `notes/A.md` to whatever is actually stored as
    /// `notes/a.md` without saying so — which is exactly the ambiguity that let
    /// [`history_restore_plan`](crate::Workspace::history_restore_plan)'s disposition
    /// probe and its `exact` removal set disagree about identity, plan the same
    /// file `Unchanged` and `Remove` in the same breath, and delete it.
    ///
    /// The parent directory is read only *after* `try_exists` has already said
    /// the path resolves, so a filesystem that does not fold case — where a
    /// similarly-spelled but different file sitting nearby is not a collision at
    /// all — takes exactly the `try_exists`-false-means-absent path it always
    /// did. Nothing here reads the target OS; the filesystem answers for itself.
    pub(super) async fn on_disk_identity(&self, path: &Path) -> Result<Option<PathBuf>> {
        let full = self.root().join(path);
        if !self.fs().try_exists(&full).await? {
            return Ok(None);
        }
        let (Some(parent), Some(name)) = (full.parent(), full.file_name()) else {
            return Ok(Some(path.to_path_buf()));
        };
        let Ok(entries) = self.fs().read_dir(parent).await else {
            // `try_exists` already said yes; a listing that cannot then confirm
            // it (a permission fault, a race) is not grounds to guess a
            // different spelling than the one asked for.
            return Ok(Some(path.to_path_buf()));
        };
        let mut folded = None;
        for entry in entries {
            let Some(entry_name) = entry.file_name() else {
                continue;
            };
            if entry_name == name {
                return Ok(Some(path.to_path_buf()));
            }
            if entry_name.eq_ignore_ascii_case(name) {
                folded = Some(path.with_file_name(entry_name.to_string_lossy().into_owned()));
            }
        }
        Ok(folded.or_else(|| Some(path.to_path_buf())))
    }

    /// Whether this workspace's filesystem folds ASCII case for path lookups —
    /// probed against the workspace root with a throwaway file rather than
    /// assumed from the platform prov is running on, because the two can
    /// disagree (a case-sensitive volume mounted on macOS, a case-insensitive
    /// share mounted on Linux), and getting this wrong in either direction is
    /// exactly the hazard the case-identity fix exists to close.
    ///
    /// Only called once [`case_fold_collision`] has already found two manifest
    /// rows that need the answer — the overwhelming majority of plans never hit
    /// this and never write a byte to get one, so
    /// [`history_restore_plan`](crate::Workspace::history_restore_plan)'s "before a
    /// byte moves" promise holds for every restore but this one, already-doomed
    /// shape.
    pub(super) async fn filesystem_case_folds(&self) -> Result<bool> {
        let probe = self.root().join(".prov-case-probe.tmp");
        let folded = self.root().join(".PROV-CASE-PROBE.tmp");
        self.fs().write(&probe, b"").await?;
        let collides = self.fs().try_exists(&folded).await;
        let _ = self.fs().remove_file(&probe).await;
        Ok(collides?)
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::super::support::*;
    use super::*;
    use crate::exec::block_on;
    use crate::validate::Finding;

    /// The event document's **bytes**, pinned whole.
    ///
    /// Event documents are immutable, so the format is a compatibility contract
    /// that cannot be retrofitted — and every other test in this module reads a
    /// capture back through prov's own parser, which means all of them would keep
    /// passing if the writer and `docs/history-format.md` drifted apart together.
    /// This is the one that reads the file as a *stranger* does: as text, compared
    /// against what §3 says it should say.
    ///
    /// What it holds still, all from §3: the key order (`part_of`, `created`,
    /// `trigger`, `label`, `parent`, `files`), six fractional digits on `created`
    /// never trimmed (§3.2), rows sorted byte-wise by path (§3.1), `id` omitted
    /// entirely rather than left empty when a document has none, `hash` spelled
    /// `sha256:<64 hex>`, and **no `id` field on the event itself** (§3 — minting
    /// one would make every capture rewrite the registry).
    ///
    /// If it fails, the question is which side is wrong. A deliberate format
    /// change means updating §3 *and* accepting that stores in the field hold
    /// documents written the old way.
    #[test]
    fn the_event_document_matches_the_format_spec_byte_for_byte() {
        let dir = tempdir("capture-golden");
        // One registered document and one unregistered payload, so the manifest
        // exercises both row shapes; ids are minted from a seeded `Minter`, so
        // `b0` below is deterministic rather than incidental.
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- notes/a.md\n---\nroot\n",
        );
        write(
            &dir,
            "notes/a.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n",
        );

        let Captured::Written { id, .. } =
            capture(&dir, "2026-07-31T09:15:22.481903Z", Some("pre-sync"))
        else {
            panic!("the first capture must write an event");
        };
        let text = read(&dir, &format!("history/events/2026/07/{id}.md"));

        // The root gained its `history` pointer in this same capture, and the
        // manifest hashes the *post-edit* bytes — so these two digests are what a
        // reader recomputing them off disk will get.
        let root_hash = crate::fixity::digest(read(&dir, "index.md").as_bytes());
        let note_hash = crate::fixity::digest(read(&dir, "notes/a.md").as_bytes());

        let expected = format!(
            "---\n\
             part_of: '[July 2026](index.md)'\n\
             created: 2026-07-31T09:15:22.481903Z\n\
             trigger: manual\n\
             label: pre-sync\n\
             files:\n\
             - path: index.md\n  \
             hash: {root_hash}\n\
             - path: notes/a.md\n  \
             hash: {note_hash}\n\
             ---\n\
             # History — 2026-07-31 09:15 (pre-sync)\n\
             \n\
             Captured 2 file(s). This is the first event in the store.\n\
             \n\
             Roll the workspace back to this point with:\n\
             \n    \
             prov history-restore {id}\n"
        );
        assert_eq!(
            text, expected,
            "the event document drifted from docs/history-format.md §3"
        );

        // No `parent` line: this is the first event, and §3 omits an absent
        // optional field rather than writing it empty. Same rule as `label`.
        assert!(
            !text.contains("parent:"),
            "a first event has no parent to name"
        );
        // And the event carries no id of its own (§3), which is what keeps a
        // capture from having to write `registry.md`.
        assert!(
            !text.contains("\nid:"),
            "event documents carry no `id` field"
        );
    }

    /// A capture in an HTML workspace has to write HTML — all three axes, not the
    /// extension alone. The bug this pins was silent precisely because prov's own
    /// parsers are lenient: a `.html` file holding `;;;`-delimited JSON and a
    /// literal `# History` round-tripped through capture, `check` and `history-list`
    /// without complaint, and only a browser (or any other tool) could tell.
    #[test]
    fn the_store_is_authored_in_the_workspaces_own_grammar_not_just_its_extension() {
        let dir = tempdir("capture-html");
        write(
            &dir,
            "index.html",
            "<script type=\"application/json\">\n{\"title\": \"Home\"}\n</script>\n\n<h1>Home</h1>\n",
        );
        let mut w = ws_authoring(
            &dir,
            crate::document::EmbedStyle::HtmlScript,
            fig::Format::Json,
        );
        let Captured::Written { id, .. } = block_on(w.history_capture(
            Path::new("index.html"),
            "2026-07-31T09:15:22.000000Z",
            None,
        ))
        .unwrap() else {
            panic!("the first capture must write an event");
        };

        for rel in [
            "history/index.html".to_string(),
            "history/events/2026/index.html".to_string(),
            "history/events/2026/07/index.html".to_string(),
            format!("history/events/2026/07/{id}.html"),
        ] {
            let text = read(&dir, &rel);
            assert!(
                text.starts_with("<script type=\"application/json\">"),
                "{rel} does not carry the workspace's own embedding: {text}"
            );
            assert!(
                !text.contains(";;;"),
                "{rel} fell back to a fence this workspace never uses: {text}"
            );
            assert!(
                text.contains("<h1>"),
                "{rel} has no HTML heading — the body is still Markdown: {text}"
            );
            assert!(
                !text.contains("\n# "),
                "{rel} holds a literal Markdown heading: {text}"
            );
        }

        // Still readable *by prov*, which the broken version also was — so this is
        // the weaker half of the assertion, kept because a legibility fix that
        // broke the round trip would be a worse bug than the one it fixed.
        assert_eq!(
            block_on(w.history_list(Path::new("index.html")))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn a_capture_bootstraps_the_store_and_captures_attachment_payloads() {
        let dir = seed("capture-basic");
        let Captured::Written { id, files, .. } =
            capture(&dir, "2026-07-31T09:15:22Z", Some("pre-sync"))
        else {
            panic!("the first capture must write an event");
        };

        // The root now points at the store, so it is reachable — the whole
        // anti-`.obsidian/` move.
        assert!(
            read(&dir, "index.md").contains("history:"),
            "the root must declare the store: {}",
            read(&dir, "index.md")
        );
        // The id resolves to its path with no index consulted.
        let event = event_path(Path::new("history/index.md"), &id, "md").unwrap();
        assert!(dir.join(&event).exists(), "{} missing", event.display());

        // The capture set is the reachable file set: root, note, sidecar, and —
        // the one that is easy to get wrong — the attachment *payload*, which is
        // reached through the sidecar's `content` pointer rather than a relation.
        let manifest = read(&dir, event.to_str().unwrap());
        for expected in [
            "index.md",
            "notes/a.md",
            "notes/photo.jpg",
            "notes/photo.jpg.yaml",
        ] {
            assert!(
                manifest.contains(expected),
                "{expected} should be captured:\n{manifest}"
            );
        }
        assert_eq!(files, 4);

        // Every captured file's bytes are parked, addressed by content, with no
        // colon anywhere in the path.
        let payload_hash = crate::fixity::digest(b"JPEGBYTES");
        let blob = blob_path(Path::new("history/index.md"), &payload_hash).unwrap();
        assert_eq!(read(&dir, blob.to_str().unwrap()), "JPEGBYTES");
    }

    #[test]
    fn capture_sorts_the_manifest_byte_wise_not_by_path_components() {
        // `notes.md` beside `notes/x.md` — a file and a same-stem directory as
        // siblings — plus the identical collision one directory deeper, so a
        // depth-limited fix would still fail this. `docs/history-format.md`
        // §3.1 requires byte-wise ascending order on the joined path string;
        // `BTreeSet<PathBuf>`/`Path::cmp` order component-wise and get exactly
        // this shape backwards (see `path_sort_key`).
        let dir = tempdir("capture-manifest-order");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- notes.md\n- notes/x.md\n- deep/notes.md\n\
             - deep/notes/x.md\n---\nroot\n",
        );
        write(
            &dir,
            "notes.md",
            "---\ntitle: Notes\npart_of: 'index.md'\n---\nnotes\n",
        );
        write(
            &dir,
            "notes/x.md",
            "---\ntitle: X\npart_of: '../index.md'\n---\nx\n",
        );
        write(
            &dir,
            "deep/notes.md",
            "---\ntitle: Deep notes\npart_of: '../index.md'\n---\ndeep notes\n",
        );
        write(
            &dir,
            "deep/notes/x.md",
            "---\ntitle: Deep X\npart_of: '../../index.md'\n---\ndeep x\n",
        );

        let Captured::Written { id, files, .. } = capture(&dir, "2026-07-31T09:15:22Z", None)
        else {
            panic!("the first capture must write an event");
        };
        assert_eq!(files, 5, "the root plus the four collision files");

        // Read the `path:` rows back off the document itself, in the order
        // they were written — the manifest is what two implementations have
        // to agree on, not `Event.files`' in-memory order.
        let event_rel = event_path(Path::new("history/index.md"), &id, "md").unwrap();
        let manifest_text = read(&dir, event_rel.to_str().unwrap());
        let order: Vec<&str> = manifest_text
            .lines()
            .filter_map(|line| line.trim_start().strip_prefix("- path: "))
            .collect();
        assert_eq!(
            order,
            vec![
                "deep/notes.md",
                "deep/notes/x.md",
                "index.md",
                "notes.md",
                "notes/x.md",
            ],
            "byte-wise ascending — `.` (0x2E) sorts before `/` (0x2F):\n{manifest_text}"
        );

        // And the id: read the event back and independently recompute the
        // digest suffix from its own recorded fields via `canonical_bytes`,
        // the same function `mint_id` used to mint it — proof the id names
        // exactly the manifest that landed on disk, in the order it landed.
        let event = block_on(ws(&dir).history_event(Path::new("index.md"), &id))
            .unwrap()
            .expect("the just-written event must read back");
        let digest = crate::fixity::digest(&canonical_bytes(
            &event.created,
            &event.trigger,
            event.label.as_deref(),
            event.parent.as_deref(),
            &event.files,
        ));
        assert_eq!(
            &id[id.len() - 8..],
            &digest["sha256:".len().."sha256:".len() + 8]
        );
    }

    #[test]
    fn the_store_is_never_captured_into_itself() {
        // The recursion the whole design turns on: capturing the store inside the
        // store would mean no capture could ever be empty, and an exact restore
        // would delete the recovery points themselves.
        let dir = seed("capture-recursion");
        capture(&dir, "2026-07-31T09:15:22Z", None);
        let set = block_on(ws(&dir).history_capture_set(Path::new("index.md"))).unwrap();
        assert!(
            set.iter().all(|p| !p.starts_with("history")),
            "the store must be invisible to the mechanism: {set:?}"
        );
        // And that is exactly what makes the no-op capture reachable.
        let second = capture(&dir, "2026-07-31T10:00:00Z", None);
        assert!(
            matches!(second, Captured::Unchanged { .. }),
            "an unchanged workspace must write nothing, got {second:?}"
        );
    }

    #[test]
    fn an_unchanged_workspace_writes_no_second_event() {
        let dir = seed("capture-empty");
        let first = capture(&dir, "2026-07-31T09:15:22Z", None);
        let Captured::Written { id, .. } = first else {
            panic!("expected a first event")
        };
        // A different clock and a different label — still the same *state*, so
        // still nothing to record. Otherwise a git hook fills the log.
        let again = capture(&dir, "2026-07-31T11:00:00Z", Some("nightly"));
        assert_eq!(again, Captured::Unchanged { id: id.clone() });
        assert_eq!(event_ids(&dir), vec![id.clone()]);

        // Change one byte and it captures again.
        write(
            &dir,
            "notes/a.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\nalpha edited\n",
        );
        let third = capture(&dir, "2026-07-31T12:00:00Z", None);
        let Captured::Written {
            diff: Some((changed, removed)),
            blobs,
            ..
        } = third
        else {
            panic!("a changed workspace must capture")
        };
        assert_eq!((changed, removed), (1, 0));
        // Only the changed file's bytes are new — the rest deduplicate for free.
        assert_eq!(blobs, 1);
        assert_eq!(event_ids(&dir).len(), 2);
    }

    #[test]
    fn the_first_event_records_the_root_that_already_declares_the_store() {
        // The bootstrap capture edits the root (it gains the `history` pointer),
        // so the manifest must hash the *post-edit* bytes. Otherwise event #1
        // describes a root predating its own store, and restoring it exactly
        // would strand the store unreachable — the one thing a restore must never
        // do. It is also what lets the very next capture be a no-op.
        let dir = seed("capture-pointer");
        let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
            panic!("expected an event")
        };
        let events = block_on(ws(&dir).history_list(Path::new("index.md"))).unwrap();
        let root_row = events[0]
            .files
            .iter()
            .find(|f| f.path == Path::new("index.md"))
            .expect("the root is in the capture set");
        let on_disk = crate::fixity::digest(read(&dir, "index.md").as_bytes());
        assert_eq!(
            root_row.hash, on_disk,
            "event {id} must record the root as the capture left it"
        );
        // And the parked blob is those same bytes, so a restore is byte-exact.
        let blob = blob_path(Path::new("history/index.md"), &root_row.hash).unwrap();
        assert_eq!(read(&dir, blob.to_str().unwrap()), read(&dir, "index.md"));
    }

    #[test]
    fn same_second_captures_chain_in_the_order_they_happened() {
        // The bug microsecond precision exists to close: with `created` pinned to
        // the second, two captures in one second tied, the sort fell through to
        // the id — whose *middle* is the label slug — and every later event
        // recorded the alphabetically-last label as its `parent`, so
        // `history-list` reported forks that never happened.
        let dir = seed("ordering");
        let stamps = [
            ("2026-07-31T09:15:10.000000Z", "zulu"),
            ("2026-07-31T09:15:10.200000Z", "alpha"),
            ("2026-07-31T09:15:10.900000Z", "mike"),
        ];
        for (i, (now, label)) in stamps.iter().enumerate() {
            // Each capture must change something, or the second one writes nothing.
            write(
                &dir,
                "notes/a.md",
                &format!("---\ntitle: A\npart_of: '../index.md'\n---\nrevision {i}\n"),
            );
            capture(&dir, now, Some(label));
        }

        let events = block_on(ws(&dir).history_list(Path::new("index.md"))).unwrap();
        assert_eq!(
            events
                .iter()
                .map(|e| e.label.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("zulu"), Some("alpha"), Some("mike")],
            "capture order, not alphabetical order by label"
        );
        // A chain, not a fan: each event's parent is the one actually before it,
        // which is what makes a real fork mean something in `history-list`.
        assert_eq!(events[0].parent, None);
        assert_eq!(events[1].parent.as_deref(), Some(events[0].id.as_str()));
        assert_eq!(events[2].parent.as_deref(), Some(events[1].id.as_str()));
    }

    #[test]
    fn an_event_written_before_sub_second_precision_keeps_its_place() {
        // The mixed store, end to end: an event carrying a second-granularity
        // `created` (every event written before this precision existed) against
        // ones that carry a fraction. Compared raw, the old event would sort last
        // in its second and the newest-event lookup would pick it — so a later
        // capture would record a *superseded* event as its parent.
        let dir = seed("ordering-mixed");
        write(
            &dir,
            "notes/a.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\nfirst\n",
        );
        capture(&dir, "2026-07-31T09:15:10Z", Some("legacy"));
        write(
            &dir,
            "notes/a.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\nsecond\n",
        );
        capture(&dir, "2026-07-31T09:15:10.500000Z", Some("current"));

        let events = block_on(ws(&dir).history_list(Path::new("index.md"))).unwrap();
        assert_eq!(
            events
                .iter()
                .map(|e| e.label.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("legacy"), Some("current")]
        );
        assert_eq!(events[1].parent.as_deref(), Some(events[0].id.as_str()));
    }

    #[test]
    fn a_transport_conflict_copy_is_not_mistaken_for_an_event() {
        // Litter beside the store must not become a phantom event — an index
        // rebuilt to *include* a conflict copy would enshrine the damage.
        assert!(is_event_id("2026-07-31-0915-pre-sync-4f2a9c1e"));
        assert!(is_event_id("2026-07-31-0915-4f2a9c1e"));
        assert!(!is_event_id(
            "2026-07-31-0915-one-1d1beacc.sync-conflict-20260731-091600"
        ));
        assert!(!is_event_id("index.sync-conflict-20260731-091600"));
        assert!(!is_event_id("index"));
        assert!(!is_event_id("notes"));
    }

    #[test]
    fn a_capture_leaves_check_clean() {
        let dir = seed("capture-check");
        capture(&dir, "2026-07-31T09:15:22Z", Some("pre-sync"));
        let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
        assert!(
            findings.is_empty(),
            "a capture must leave the workspace valid: {findings:?}"
        );
    }

    #[test]
    fn a_new_month_grows_the_shard_tree_without_rewriting_old_shards() {
        let dir = seed("capture-shard");
        capture(&dir, "2026-07-31T09:15:22Z", None);
        let july = read(&dir, "history/events/2026/07/index.md");

        write(&dir, "notes/b.md", "---\ntitle: B\n---\nbeta\n");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- notes/a.md\n- notes/b.md\n- notes/photo.jpg.yaml\n\
             history: history/index.md\n---\nroot\n",
        );
        write(
            &dir,
            "notes/b.md",
            "---\ntitle: B\npart_of: '../index.md'\n---\nbeta\n",
        );
        capture(&dir, "2026-08-01T09:00:00Z", None);

        // The new month is its own shard, linked from the year index; July's
        // shard index is untouched — the mutable surface is "this month", not
        // "forever".
        assert!(dir.join("history/events/2026/08/index.md").exists());
        assert_eq!(read(&dir, "history/events/2026/07/index.md"), july);
        assert!(read(&dir, "history/events/2026/index.md").contains("08/index.md"));
        assert!(
            block_on(ws(&dir).check(Path::new("index.md")))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn binned_bytes_are_not_newly_retained_by_a_routine_capture() {
        // The exclusion is narrow and worth pinning: a capture must not park
        // bytes the user has consigned to the bin. (It emphatically does *not*
        // make a purge final for content captured while it was live — that is
        // documented, not tested here, because it is a non-guarantee.)
        let dir = seed("capture-bin");
        write(
            &dir,
            "recyclebin/index.yaml",
            "title: Recycle Bin\ndeleted: []\n",
        );
        write(&dir, "recyclebin/items/notes/old.md", "binned bytes\n");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- notes/a.md\n- notes/photo.jpg.yaml\n\
             recycle_bin: recyclebin/index.yaml\n---\nroot\n",
        );
        let set = block_on(ws(&dir).history_capture_set(Path::new("index.md"))).unwrap();
        assert!(
            set.iter().all(|p| !p.starts_with("recyclebin/items")),
            "binned bytes must not be captured: {set:?}"
        );
        // The bin *index* is captured, though — that is what makes a restore put
        // a live document back as live.
        assert!(
            set.contains(&PathBuf::from("recyclebin/index.yaml")),
            "the bin index is ordinary structural state: {set:?}"
        );
    }

    // The feature's entire claim is surviving an external sync transport, so the
    // tests below simulate one: two workspace copies, concurrent captures, and a
    // directory merge that unions added files, drops in a `.sync-conflict-…` file,
    // and clobbers a shard index.

    /// Copy every file under `from` into `to`, adding what is missing and leaving
    /// what is already there — the union-of-added-files merge that git, Dropbox,
    /// Syncthing and iCloud all perform without conflict.
    fn merge_into(from: &Path, to: &Path) {
        fn walk(dir: &Path, base: &Path, to: &Path) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let rel = path.strip_prefix(base).unwrap().to_path_buf();
                if path.is_dir() {
                    walk(&path, base, to);
                } else if !to.join(&rel).exists() {
                    let dest = to.join(&rel);
                    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
                    std::fs::copy(&path, &dest).unwrap();
                }
            }
        }
        walk(from, from, to);
    }

    #[test]
    fn concurrent_captures_on_two_devices_merge_without_conflict() {
        // Two devices, same starting state, each captures locally. Because a
        // capture only *adds* files, the transport's union merge produces both
        // events side by side — the whole point of the append-only design.
        let one = seed("transport-one");
        let two = tempdir("transport-two");
        merge_into(&one, &two);

        // Device one edits and captures.
        write(
            &one,
            "notes/a.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\nfrom device one\n",
        );
        let Captured::Written { id: id_one, .. } =
            capture(&one, "2026-07-31T09:15:22Z", Some("one"))
        else {
            panic!("device one must capture")
        };
        // Device two edits differently and captures — same minute, no coordination.
        write(
            &two,
            "notes/a.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\nfrom device two\n",
        );
        let Captured::Written { id: id_two, .. } =
            capture(&two, "2026-07-31T09:15:22Z", Some("two"))
        else {
            panic!("device two must capture")
        };
        assert_ne!(id_one, id_two, "different content must mint different ids");

        // The transport reconciles: every added file lands in device one's copy.
        merge_into(&two, &one);

        // Both events survive, and both devices' pre-images are present.
        let ids = event_ids(&one);
        assert!(
            ids.contains(&id_one) && ids.contains(&id_two),
            "a merge must not lose either device's event: {ids:?}"
        );
        for bytes in [b"from device one".as_slice(), b"from device two".as_slice()] {
            let hash = crate::fixity::digest(
                format!(
                    "---\ntitle: A\npart_of: '../index.md'\n---\n{}\n",
                    String::from_utf8_lossy(bytes)
                )
                .as_bytes(),
            );
            let blob = blob_path(Path::new("history/index.md"), &hash).unwrap();
            assert!(
                one.join(&blob).exists(),
                "both devices' pre-images must survive the merge: {}",
                blob.display()
            );
        }
    }

    #[test]
    fn a_merged_shard_index_is_reported_stale_and_rebuilt_from_its_directory() {
        // The one mutable file in the store is the shard index, so it is the one
        // a transport can mangle. That must be a finding with a mechanical fix,
        // never data loss — which is exactly what "the index is a cache" buys.
        let one = seed("transport-index");
        let two = tempdir("transport-index-two");
        merge_into(&one, &two);

        write(
            &one,
            "notes/a.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\none\n",
        );
        capture(&one, "2026-07-31T09:15:22Z", Some("one"));
        write(
            &two,
            "notes/a.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\ntwo\n",
        );
        capture(&two, "2026-07-31T09:16:00Z", Some("two"));

        // Merge device two's *event* across but let the transport clobber the
        // shard index with device two's copy — which knows nothing of device
        // one's event. This is the realistic damage: last-writer-wins on the
        // only file both devices rewrote.
        merge_into(&two, &one);
        std::fs::copy(
            two.join("history/events/2026/07/index.md"),
            one.join("history/events/2026/07/index.md"),
        )
        .unwrap();
        // …and drop in the conflict copy such a transport leaves behind.
        write(
            &one,
            "history/events/2026/07/index.sync-conflict-20260731-091600.md",
            "---\ntitle: July 2026\n---\nconflicted copy\n",
        );

        // Both events are still listed: `history-list` reads the directories, so
        // a mangled index cannot hide an event that is sitting right there.
        assert_eq!(
            event_ids(&one).len(),
            2,
            "the events are the authority, not the index"
        );

        // `check` names it, and the fix rebuilds that one shard.
        let findings = block_on(ws(&one).check(Path::new("index.md"))).unwrap();
        let stale: Vec<_> = findings
            .iter()
            .filter(|f| matches!(f, Finding::HistoryIndexStale { .. }))
            .collect();
        assert_eq!(stale.len(), 1, "expected one stale shard: {findings:?}");

        let mut w = ws(&one);
        let fix = block_on(w.suggest_fix(stale[0])).unwrap().expect("a fix");
        block_on(w.apply_fix(&fix)).unwrap();

        let after = block_on(ws(&one).check(Path::new("index.md"))).unwrap();
        assert!(
            !after
                .iter()
                .any(|f| matches!(f, Finding::HistoryIndexStale { .. })),
            "the rebuild should have settled the index: {after:?}"
        );
        let rebuilt = read(&one, "history/events/2026/07/index.md");
        for id in event_ids(&one) {
            assert!(
                rebuilt.contains(&id),
                "the rebuilt index must list every event in its directory: {rebuilt}"
            );
        }
    }

    #[test]
    fn a_capture_after_a_merge_records_the_merged_state() {
        // The end-to-end claim: after a transport has done its worst, a capture
        // still runs and still records a consistent cut.
        let one = seed("transport-after");
        let two = tempdir("transport-after-two");
        merge_into(&one, &two);
        capture(&one, "2026-07-31T09:00:00Z", None);
        capture(&two, "2026-07-31T09:00:00Z", None);
        merge_into(&two, &one);

        write(
            &one,
            "notes/a.md",
            "---\ntitle: A\npart_of: '../index.md'\n---\npost-merge\n",
        );
        let outcome = capture(&one, "2026-07-31T10:00:00Z", Some("post-merge"));
        let Captured::Written { id, .. } = outcome else {
            panic!("a post-merge capture must write: {outcome:?}")
        };
        // Its parent is the newest event that existed locally — display metadata,
        // but it should still be recorded.
        let events = block_on(ws(&one).history_list(Path::new("index.md"))).unwrap();
        let latest = events.iter().find(|e| e.id == id).unwrap();
        assert!(latest.parent.is_some(), "a parent should be recorded");
        assert!(
            latest
                .files
                .iter()
                .any(|f| f.path == Path::new("notes/a.md")),
            "the merged state must be in the manifest"
        );
    }

    // ---- the fixity cache ----
    //
    // The unit tests in `crate::fixity::cache` pin the validator; these pin the
    // wiring — that a warm cache actually removes the reads, that it cannot hide
    // a real edit, and that the one thing it is trusted for stays narrow.

    /// Push a file's modification time forward, so a test's two writes are
    /// distinguishable no matter how coarse the filesystem's own clock is.
    fn touch(dir: &Path, rel: &str, secs_ahead: u64) {
        let path = dir.join(rel);
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        let now = std::fs::metadata(&path).unwrap().modified().unwrap();
        file.set_modified(now + std::time::Duration::from_secs(secs_ahead))
            .unwrap();
    }

    /// The payoff, stated as the thing a user actually feels: a capture over a
    /// workspace where nothing changed reads nothing at all.
    #[test]
    fn a_warm_cache_captures_without_reading_a_single_file() {
        let dir = seed("cache-warm");
        let (mut ws, fs) = ws_counting(&dir);
        ws.set_fixity_cache(Some(crate::FixityCache::new(&dir)));
        let first =
            block_on(ws.history_capture(Path::new("index.md"), "2026-01-01T00:00:00Z", None))
                .unwrap();
        assert!(matches!(first, Captured::Written { .. }));
        assert!(
            fs.total_byte_reads() > 0,
            "the first capture has to read everything — there is nothing to remember yet"
        );
        let cache = ws.take_fixity_cache().unwrap();
        assert!(cache.is_dirty(), "the first capture learned nothing");

        // A second capture, with what the first learned. One read remains, and it
        // is the root: the bootstrap capture hashed the root's *computed*
        // post-edit text, which has no stat to record an entry against, so this
        // is the first capture in a position to remember it.
        fs.reset();
        ws.set_fixity_cache(Some(cache));
        let second =
            block_on(ws.history_capture(Path::new("index.md"), "2026-01-02T00:00:00Z", None))
                .unwrap();
        assert_eq!(fs.byte_reads(&dir, "index.md"), 1);
        assert_eq!(
            fs.total_byte_reads(),
            1,
            "a capture with a warm cache read a file it had already hashed"
        );
        assert!(
            matches!(second, Captured::Unchanged { .. }),
            "the manifest built from the cache must equal the one built from the disk: {second:?}"
        );

        // The steady state: nothing changed, nothing left to learn, nothing read.
        fs.reset();
        let third =
            block_on(ws.history_capture(Path::new("index.md"), "2026-01-03T00:00:00Z", None))
                .unwrap();
        assert_eq!(
            fs.total_byte_reads(),
            0,
            "a capture over an unchanged workspace read a file"
        );
        assert!(matches!(third, Captured::Unchanged { .. }), "{third:?}");
    }

    /// The manifest a warm cache produces has to be the same manifest the disk
    /// produces — byte for byte, row for row. If the two ever disagreed, a
    /// capture would silently record a workspace that never existed.
    #[test]
    fn a_cached_manifest_is_the_manifest_the_disk_would_have_given() {
        let cold = seed("cache-cold-manifest");
        let warm = seed("cache-warm-manifest");
        for dir in [&cold, &warm] {
            block_on(ws(dir).history_capture(Path::new("index.md"), "2026-01-01T00:00:00Z", None))
                .unwrap();
        }
        // `warm` gets a cache populated by that first capture; `cold` never does.
        let mut warm_ws = ws(&warm);
        warm_ws.set_fixity_cache(Some(crate::FixityCache::new(&warm)));
        block_on(warm_ws.history_capture(Path::new("index.md"), "2026-01-02T00:00:00Z", None))
            .unwrap();

        for dir in [&cold, &warm] {
            write(
                dir,
                "notes/a.md",
                "---\ntitle: A\npart_of: '../index.md'\n---\nsecond\n",
            );
            touch(dir, "notes/a.md", 5);
        }
        block_on(ws(&cold).history_capture(Path::new("index.md"), "2026-01-03T00:00:00Z", None))
            .unwrap();
        block_on(warm_ws.history_capture(Path::new("index.md"), "2026-01-03T00:00:00Z", None))
            .unwrap();

        let manifest = |dir: &Path| {
            let events = block_on(ws(dir).history_list(Path::new("index.md"))).unwrap();
            events
                .last()
                .unwrap()
                .files
                .iter()
                .map(|f| (f.path.clone(), f.hash.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            manifest(&cold),
            manifest(&warm),
            "a cached capture and an uncached one disagreed about the workspace"
        );
    }

    /// A cache that could hide an edit would be worse than no cache. Both halves
    /// of the validator are exercised: a file whose length changed, and one that
    /// was rewritten at exactly the same length.
    #[test]
    fn an_edited_file_is_still_captured_with_a_warm_cache() {
        let dir = seed("cache-edit");
        let mut ws = ws(&dir);
        ws.set_fixity_cache(Some(crate::FixityCache::new(&dir)));
        block_on(ws.history_capture(Path::new("index.md"), "2026-01-01T00:00:00Z", None)).unwrap();

        // Same length, different bytes — only the timestamp can catch this one.
        let before = read(&dir, "notes/a.md");
        let after = before.replace("alpha", "ALPHA");
        assert_eq!(before.len(), after.len(), "the test's own premise");
        write(&dir, "notes/a.md", &after);
        touch(&dir, "notes/a.md", 5);

        let outcome =
            block_on(ws.history_capture(Path::new("index.md"), "2026-01-02T00:00:00Z", None))
                .unwrap();
        let Captured::Written { .. } = outcome else {
            panic!("a warm cache hid an edit: {outcome:?}")
        };
        let events =
            block_on(crate::history::support::ws(&dir).history_list(Path::new("index.md")))
                .unwrap();
        let row = events
            .last()
            .unwrap()
            .files
            .iter()
            .find(|f| f.path == Path::new("notes/a.md"))
            .unwrap();
        assert_eq!(
            row.hash,
            crate::fixity::digest(after.as_bytes()),
            "the manifest recorded a digest of bytes that are no longer there"
        );
    }

    /// The containment argument, made executable: a remembered digest is trusted
    /// *only* when the blob it names is already parked. Take the blob away and
    /// the capture must go back to the file, so the bytes it stores are always
    /// bytes it has read.
    #[test]
    fn a_remembered_digest_is_ignored_when_its_blob_is_gone() {
        let dir = seed("cache-blobless");
        let (mut ws, fs) = ws_counting(&dir);
        ws.set_fixity_cache(Some(crate::FixityCache::new(&dir)));
        block_on(ws.history_capture(Path::new("index.md"), "2026-01-01T00:00:00Z", None)).unwrap();

        // The blob for `notes/a.md` goes missing — the loss `check` reports as
        // `HistoryBlobMissing`, and a state a warm cache must not paper over.
        let body = read(&dir, "notes/a.md");
        let blob = dir.join(blob_of(body.as_bytes()));
        std::fs::remove_file(&blob).unwrap();

        fs.reset();
        block_on(ws.history_capture(Path::new("index.md"), "2026-01-02T00:00:00Z", None)).unwrap();
        assert_eq!(
            fs.byte_reads(&dir, "notes/a.md"),
            1,
            "a digest was trusted for a blob that is not on disk"
        );
        assert!(blob.exists(), "the missing blob was not re-parked");
    }

    /// A cache written for another workspace is not a cache. This is the failure
    /// that would be silent and wrong rather than loud and wrong, so it is worth
    /// its own test even though `decode` is unit-tested.
    #[test]
    fn a_cache_from_another_workspace_is_refused() {
        let one = seed("cache-foreign-one");
        let two = seed("cache-foreign-two");
        let mut first = ws(&one);
        first.set_fixity_cache(Some(crate::FixityCache::new(&one)));
        block_on(first.history_capture(Path::new("index.md"), "2026-01-01T00:00:00Z", None))
            .unwrap();
        let bytes = first.take_fixity_cache().unwrap().encode();

        assert!(
            crate::FixityCache::decode(&bytes, &two).is_none(),
            "one workspace's digests were offered to another"
        );
        assert!(crate::FixityCache::decode(&bytes, &one).is_some());
    }
}
