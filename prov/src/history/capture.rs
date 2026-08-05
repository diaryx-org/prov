use std::path::{Path, PathBuf};

use crate::change::ChangeSet;
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
        let root_doc = link::normalize(root_doc);
        let ext = self.history_ext(&root_doc);
        let embed = self.history_embed()?;
        let (store_index, store_exists) = self.history_store_index(&root_doc).await?;
        let label = label.map(str::trim).filter(|l| !l.is_empty());

        // Bootstrapping the store *edits the root* (it gains the `history`
        // pointer), so that edit is computed up front and the manifest hashes the
        // post-edit bytes. Otherwise the very first event would record a root
        // predating its own store — and restoring it exactly would strand the
        // store unreachable, which is the one thing a restore must never do.
        let root_pointer = match store_exists {
            true => None,
            false => Some(self.history_pointer_text(&root_doc, &store_index).await?),
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
            let bytes = match &root_pointer {
                Some(text) if path == root_doc => text.clone().into_bytes(),
                _ => self.fs().read(&self.root().join(&path)).await?,
            };
            let hash = crate::fixity::digest(&bytes);
            // Content-addressed, so a hash already on disk *is* the same bytes —
            // nothing to rewrite, and two devices parking the same content
            // converge instead of conflicting.
            let blob = self.root().join(blob_path(&store_index, &hash)?);
            if !self.fs().try_exists(&blob).await? {
                if let Some(dir) = blob.parent() {
                    self.fs().create_dir_all(dir).await?;
                }
                self.fs().write_atomic(&blob, &bytes).await?;
                parked += 1;
            }
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
        let event_text = crate::edit::reformat_block(&body, &map, embed)?;

        let mut cs = self.change();
        cs.write(&event_rel, event_text);
        self.stage_history_indexes(&mut cs, &store_index, &id, ext, embed)
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
        ext: &str,
        embed: fig::EmbedType,
    ) -> Result<()> {
        let shard = shard_of(id)?;
        let (year, month) = shard_parts(&shard)?;
        let events_root = store_dir(store_index).join(EVENTS_DIR);
        let shard_dir = events_root.join(&shard);

        let mut ids = self.shard_event_ids(&shard_dir, ext).await?;
        ids.insert(id.to_string());
        cs.write(
            shard_dir.join(format!("index.{ext}")),
            render_month_index(&year, &month, &ids, ext, embed)?,
        );

        let mut months = self.event_months(&events_root.join(&year), ext).await?;
        months.insert(month.clone());
        cs.write(
            events_root.join(&year).join(format!("index.{ext}")),
            render_year_index(&year, &months, ext, embed)?,
        );

        let mut years = self.event_years(&events_root, ext).await?;
        years.insert(year.clone());
        let forgotten = self.history_forgotten_link(store_index).await?;
        cs.write(
            store_index,
            render_store_index(&years, ext, forgotten.as_deref(), embed)?,
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
