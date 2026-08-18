use std::path::Path;

use prov_graph::content::{ContentFormat, transcode};
use prov_graph::error::Result;
use prov_graph::index::IdIndex;
use prov_graph::link;
use prov_graph::meta::{Mapping, Value};
use prov_transaction::{ChangeSet, write_blob_atomic};

use super::docs::{
    Authoring, render_month_index, render_store_index, render_year_index, shard_title,
};
use super::event_id::mint_id;
use super::layout::{StoreLocation, blob_path, event_path, shard_of, shard_parts, store_dir};
use super::model::{CaptureNote, Captured, Event, FileEntry};
use super::paths::{manifest_of, slash_path};
use super::{EVENTS_DIR, HistoryStore, HistoryWriteHost, TRIGGER_MANUAL};

impl<H: HistoryWriteHost> HistoryStore<H> {
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
    /// journal embeds file contents, so a genesis capture riding the change set
    /// would write a second whole copy of the workspace into the journal. They go
    /// through [`write_blob_atomic`] instead, which is safe precisely because a
    /// content-addressed write is idempotent — replaying it can only write the
    /// same bytes to the same path.
    ///
    /// Blobs are parked *before* the change set lands, so the failure mode is an
    /// orphaned blob (reported by `check`, collected by `history-prune`) rather
    /// than an event whose bytes are missing.
    pub async fn capture(
        &mut self,
        root_doc: &Path,
        now: &str,
        note: CaptureNote<'_>,
    ) -> Result<Captured> {
        // The capture set comes from a full walk of the graph, and the manifest
        // loop then visits every file that walk found. One scope over both means
        // a document is read once rather than once per pass. Dropped explicitly
        // where the writing half begins — which is both what the borrow checker
        // needs and exactly the right boundary: nothing after that point reads a
        // document, and everything after it changes one.
        let scope = self.host().graph().read_scope();
        let root_doc = link::normalize(root_doc);
        let style = self.authoring(&root_doc)?;
        let ext = style.ext.as_str();
        let (store_index, found) = self.store_index(&root_doc).await?;
        let label = note.label.map(str::trim).filter(|l| !l.is_empty());
        let message = note.message.map(str::trim).filter(|m| !m.is_empty());

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
            _ => Some(self.pointer_text(&root_doc, &store_index).await?),
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
        for path in self.capture_set(&root_doc).await? {
            // The root's post-edit text is computed here, not on disk. It has no
            // stat to validate a remembered digest against, so it is never
            // served from the cache and never recorded into it.
            let staged = match &root_pointer {
                Some(text) if path == root_doc => Some(text.clone().into_bytes()),
                _ => None,
            };
            // One stat, and on a hit it is the *only* thing this file costs: no
            // read, and no pass over its bytes. A stat this pass has to be able
            // to afford, since the alternative it replaces is reading the file.
            let meta = match staged {
                Some(_) => None,
                None => self.host().graph().stat(&path).await.ok(),
            };
            let remembered = match &meta {
                Some(meta) => self.host().fixity_cached(&path, meta),
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
                        .host()
                        .graph()
                        .exists(&blob_path(&store_index, &hash)?)
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
                        None => self.host().graph().read_bytes(&path).await?,
                    };
                    let hash = prov_fixity::digest(&bytes);
                    // Content-addressed, so a hash already on disk *is* the same
                    // bytes — nothing to rewrite, and two devices parking the
                    // same content converge instead of conflicting.
                    let blob_rel = blob_path(&store_index, &hash)?;
                    if !self.host().graph().exists(&blob_rel).await? {
                        write_blob_atomic(
                            self.host().graph().fs(),
                            self.host().graph().root(),
                            &blob_rel,
                            &bytes,
                        )
                        .await?;
                        parked += 1;
                    }
                    if let Some(meta) = &meta {
                        self.host().fixity_remember(&path, meta, &hash);
                    }
                    hash
                }
            };
            let id = self.host().graph().index().id_for_path(&path);
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
        let existing = self.list(&root_doc).await?;
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
        // The message goes after the heading, never at the top of the body. A
        // body opening with a user's `---` line, in a delimited-frontmatter
        // workspace, is a second fence where a re-parse expects prose; a
        // heading in front of it means the message can say anything.
        let note = match message {
            Some(message) => format!("{message}\n\n"),
            None => String::new(),
        };
        let body = format!(
            "# History — {}\n\n{note}{summary}\n\nRoll the workspace back to this point with:\n\n    \
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
        let event_text = prov_store::edit::reformat_block(
            &transcode(&body, ContentFormat::Markdown, style.content)?,
            &map,
            style.embed,
        )?;

        drop(scope);
        let mut cs = self.host_mut().change();
        cs.write(&event_rel, event_text);
        self.stage_indexes(&mut cs, &store_index, &id, &style)
            .await?;
        if let Some(text) = root_pointer {
            cs.write(&root_doc, text);
        }
        self.host_mut().commit(cs).await?;

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
    /// cache" honest: capture and the rebuild autofix run the same code, so a
    /// repaired index is byte-identical to a freshly written one.
    async fn stage_indexes(
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
        let forgotten = self.forgotten_link(store_index).await?;
        cs.write(
            store_index,
            render_store_index(&years, forgotten.as_deref(), style)?,
        );
        Ok(())
    }
}
