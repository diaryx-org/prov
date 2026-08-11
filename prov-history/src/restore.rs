use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use prov_graph::error::{Error, Result};
use prov_store::fs::Storage;
use prov_graph::index::{Collision, IdIndex};
use prov_graph::link;
use prov_transaction::{discard_file, write_probe};

use super::layout::blob_path;
use super::model::{
    Conflict, Disposition, Event, FileEntry, RestoreOp, RestorePlan, Scope, Subject,
};
use super::paths::{case_fold_collision, under};
use super::{HistoryReadHost, HistoryStore, HistoryWriteHost};

/// Planning a restore takes the tree's own answer to two questions a read
/// cannot settle — which spelling of a path is actually on disk, and whether
/// this filesystem folds case — and the second of those writes a throwaway
/// probe. So these sit behind `Fs: Storage` even though nothing here is a
/// mutation of the workspace.
impl<H: HistoryReadHost> HistoryStore<H>
where
    H::Fs: Storage,
{
    /// What restoring `event` would do, computed **before a byte moves** — the
    /// dry run, the confirmation prompt's removal list, and the plan
    /// [`restore`](Self::restore) executes, all one value.
    ///
    /// Everything here falls out of comparing the manifest against disk: no graph
    /// walk, no projected tree. True pre-flight *validation* — checking what the
    /// restored graph would look like before writing it — is a general `--dry-run`
    /// capability for every mutation, not this one verb's private machinery; what
    /// stands in for it is that a restore runs `check` before and after and reports
    /// the difference.
    ///
    /// ## What the plan decides
    ///
    /// - **Per row: create, overwrite, unchanged, or no bytes.** A row whose blob
    ///   is absent is skipped rather than fatal — a manifest and its blobs sync
    ///   independently. A row whose bytes are *already* on disk is skipped too, so
    ///   restoring a capture the workspace already matches writes nothing at all.
    /// - **Under `exact`, what to remove**: the capture set (the store and the
    ///   recycle bin's items already excluded, by construction) minus the paths the
    ///   manifest holds. The honest "undo this merge entirely" tool — bad-merge
    ///   damage is characteristically *additive* (a `.sync-conflict` copy, a
    ///   rename-vs-rename landing both names), and none of it goes away by writing
    ///   captured bytes over the top. The same pass discards legitimate work done
    ///   since the capture, which is why it is opt-in and why the caller is
    ///   expected to show [`removals`](RestorePlan::removals) before running it.
    ///
    ///   **Reachable** is the operative word, and it bounds the promise: a file
    ///   nothing links is not in the capture set, so `exact` leaves it exactly
    ///   where it is and `check` reports it as an orphan. A restore puts a
    ///   captured graph back; deciding that some unreferenced file in a directory
    ///   is rubble is not a call it gets to make. Note the timing this implies —
    ///   the plan is taken against the tree as it stands, so a file the *restored*
    ///   root would stop linking is still reachable when the delete set is
    ///   computed, and is removed.
    /// - **Which registrations it would displace.** `id_storage` defaults to
    ///   `both`, so a restored document's frontmatter carries an id the live
    ///   registry may bind elsewhere — and the target path can be free while the id
    ///   is taken, or the other way round. Both directions, via the host's
    ///   [`registration_conflict`](HistoryReadHost::registration_conflict).
    ///
    ///   A collision the restore **itself resolves** is not reported: if the
    ///   document currently holding the id is one this restore overwrites or (under
    ///   `exact`) removes, nothing is displaced. That is what lets `--exact` undo a
    ///   move without `--force`, while an *additive* restore of the same event —
    ///   which would put the old path back and leave the new one there, two
    ///   documents spelling one id — still refuses.
    ///
    /// `exact` is rejected outright with a scope. It means "make the tree match
    /// this capture", which a slice of the capture cannot say.
    pub async fn restore_plan(
        &self,
        root_doc: &Path,
        event: &Event,
        scope: &Scope,
        exact: bool,
    ) -> Result<RestorePlan> {
        let root_doc = link::normalize(root_doc);
        let (store_index, _) = self.store_index(&root_doc).await?;

        if exact && *scope != Scope::Whole {
            return Err(Error::Structure(
                "`exact` removes every reachable file the capture does not contain, \
                 which is a statement about the whole tree — it cannot be scoped to \
                 part of one"
                    .into(),
            ));
        }

        // The rows this restore is about. `Whole` is the consistent cut; the other
        // two are content recovery, and each names something that has to be *in*
        // the manifest — a scope that selects nothing is a typo, not an empty
        // restore.
        let selected: Vec<&FileEntry> = match scope {
            Scope::Whole => event.files.iter().collect(),
            Scope::Paths(paths) => {
                let mut rows: Vec<&FileEntry> = Vec::new();
                for want in paths {
                    let want = link::normalize(want);
                    let matched = event.files.iter().filter(|f| under(&f.path, &want));
                    let before = rows.len();
                    rows.extend(matched);
                    if rows.len() == before {
                        return Err(Error::Structure(format!(
                            "{} is not in {} — `prov history-show {}` lists what it captured",
                            want.display(),
                            event.id,
                            event.id
                        )));
                    }
                }
                rows.sort_by(|a, b| a.path.cmp(&b.path));
                rows.dedup_by(|a, b| a.path == b.path);
                rows
            }
            Scope::Id(id) => {
                let rows: Vec<&FileEntry> = event
                    .files
                    .iter()
                    .filter(|f| f.id.as_ref() == Some(id))
                    .collect();
                if rows.is_empty() {
                    return Err(Error::Structure(format!(
                        "{} is not in {} — that capture recorded no document with that id",
                        id, event.id
                    )));
                }
                rows
            }
        };

        // A manifest whose captured tree held two paths differing only by case is
        // a state a case-sensitive filesystem can hold and a case-insensitive one
        // cannot: writing the second row's bytes after the first would land on
        // the file the first row's write just created, silently discarding it.
        // This can only arrive from a manifest captured *elsewhere* — a capture
        // taken on a case-insensitive filesystem could never observe both paths
        // reachable at once, since the same folding that would defeat the
        // restore already defeated the walk that would have built such a
        // manifest. Checked against the real filesystem rather than the running
        // OS (`filesystem_case_folds`'s own doc says why), and only for the rows
        // this restore actually selected — a scope that names just one of the
        // pair has nothing to self-clobber.
        if let Some((a, b)) = case_fold_collision(selected.iter().map(|f| f.path.as_path()))
            && self.filesystem_case_folds().await?
        {
            return Err(Error::Structure(format!(
                "{} captured both {} and {}, which differ only in case — this \
                 filesystem cannot hold both at once, so restoring them together \
                 here would let the second overwrite the first",
                event.id,
                a.display(),
                b.display()
            )));
        }

        let mut ops = Vec::new();
        // The on-disk path each row actually resolves to right now, gathered
        // alongside the dispositions below — built once so the `exact` removal
        // pass can ask the identical question the probe just answered, instead
        // of falling back to a byte-exact string compare that a case-insensitive
        // filesystem can disagree with. That disagreement was the bug: a row the
        // probe found `Unchanged` under a different case, and the removal pass
        // then deleted anyway, because "captured" and "on disk" were compared as
        // literal strings in one pass and through the filesystem's own folding
        // in the other.
        let mut occupied: BTreeSet<PathBuf> = BTreeSet::new();
        for file in selected {
            // Presence of the bytes first: a row prov cannot supply has no
            // disposition worth computing, and there is nothing to read.
            let parked = match blob_path(&store_index, &file.hash) {
                Ok(blob) => self.host().graph().exists(&blob).await?,
                // A hash prov could not have parked names no blob that could be
                // found — missing, rather than fatal to the whole plan.
                Err(_) => false,
            };
            // Computed regardless of `parked`: even a row with no bytes to
            // restore still names a path the manifest holds, and a file already
            // sitting there under a different case is the same file that row is
            // about — `exact` must not delete it either.
            let identity = self.on_disk_identity(&file.path).await?;
            if let Some(actual) = &identity {
                occupied.insert(actual.clone());
            }
            let (disposition, rename_from) = match (parked, identity) {
                (false, _) => (Disposition::NoBytes, None),
                (true, None) => (Disposition::Create, None),
                (true, Some(actual)) => {
                    let bytes = self.host().graph().read_bytes(&actual).await?;
                    let matches = prov_fixity::digest(&bytes) == file.hash;
                    let recased = actual != file.path;
                    match (matches, recased) {
                        (true, false) => (Disposition::Unchanged, None),
                        (true, true) => (Disposition::CaseOnly, Some(actual)),
                        (false, false) => (Disposition::Overwrite, None),
                        (false, true) => (Disposition::Overwrite, Some(actual)),
                    }
                }
            };
            ops.push(RestoreOp {
                path: file.path.clone(),
                disposition,
                hash: Some(file.hash.clone()),
                id: file.id.clone(),
                rename_from,
            });
        }

        if exact {
            for path in self.capture_set(&root_doc).await? {
                // The root document is never removed. A capture always holds it
                // (it is how the walk started), so this only fires for a manifest
                // that is not one — and a tree with no root is not a restored
                // workspace, it is rubble.
                //
                // `occupied` is keyed by the *actual* on-disk path each row
                // resolves to, not the manifest's own spelling — so a reachable
                // file a row claims only under a different case is spared here
                // exactly as it was left unwritten (or renamed rather than
                // recreated) above, rather than the two passes disagreeing about
                // whether it is "captured".
                if occupied.contains(&path) || path == root_doc {
                    continue;
                }
                ops.push(RestoreOp {
                    path,
                    disposition: Disposition::Remove,
                    hash: None,
                    id: None,
                    rename_from: None,
                });
            }
        }

        // A collision only counts if it survives the restore, so the two sets the
        // restore *resolves* are needed before judging any of them.
        let written: BTreeSet<&Path> = ops
            .iter()
            .filter(|op| matches!(op.disposition, Disposition::Create | Disposition::Overwrite))
            .map(|op| op.path.as_path())
            .collect();
        let removed: BTreeSet<&Path> = ops
            .iter()
            .filter(|op| op.disposition == Disposition::Remove)
            .map(|op| op.path.as_path())
            .collect();
        let mut conflicts = Vec::new();
        for op in &ops {
            // Only a row actually being written can displace anything: an
            // `Unchanged` path already holds these bytes, a `NoBytes` one is not
            // touched at all, and a `CaseOnly` rename moves the same bytes this
            // id was already registered against — nothing about which id claims
            // which content changes, only the on-disk spelling does.
            if !matches!(op.disposition, Disposition::Create | Disposition::Overwrite) {
                continue;
            }
            let Some(id) = &op.id else { continue };
            let Some(collision) = self.host().registration_conflict(id, &op.path) else {
                continue;
            };
            let resolved = match &collision {
                // The id is registered elsewhere — harmless if "elsewhere" is a
                // path this restore is about to overwrite with captured content or
                // remove outright.
                Collision::Id { held_by, .. } => {
                    written.contains(held_by.as_path()) || removed.contains(held_by.as_path())
                }
                // The path is registered to a *different* id: whatever document is
                // there now, this would write over it and leave that id resolving
                // to bytes that no longer spell it. Nothing in the restore fixes
                // that.
                Collision::Path { .. } => false,
            };
            if !resolved {
                conflicts.push(Conflict {
                    path: op.path.clone(),
                    collision,
                });
            }
        }

        ops.sort_by(|a, b| {
            a.disposition
                .rank()
                .cmp(&b.disposition.rank())
                .then_with(|| a.path.cmp(&b.path))
        });
        Ok(RestorePlan {
            event: event.id.clone(),
            ops,
            conflicts,
        })
    }

    /// The literal on-disk spelling `path` resolves to, or `None` if nothing
    /// does.
    ///
    /// `try_exists` alone cannot say *which* spelling: on a case-insensitive
    /// filesystem it resolves `notes/A.md` to whatever is actually stored as
    /// `notes/a.md` without saying so — which is exactly the ambiguity that let
    /// [`restore_plan`](Self::restore_plan)'s disposition probe and its `exact`
    /// removal set disagree about identity, plan the same file `Unchanged` and
    /// `Remove` in the same breath, and delete it.
    ///
    /// The parent directory is read only *after* `try_exists` has already said
    /// the path resolves, so a filesystem that does not fold case — where a
    /// similarly-spelled but different file sitting nearby is not a collision at
    /// all — takes exactly the `try_exists`-false-means-absent path it always
    /// did. Nothing here reads the target OS; the filesystem answers for itself.
    pub async fn on_disk_identity(&self, path: &Path) -> Result<Option<PathBuf>> {
        if !self.host().graph().exists(path).await? {
            return Ok(None);
        }
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return Ok(Some(path.to_path_buf()));
        };
        let Ok(entries) = self.host().graph().listing(parent).await else {
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
    /// [`restore_plan`](Self::restore_plan)'s "before a byte moves" promise holds
    /// for every restore but this one, already-doomed shape.
    pub async fn filesystem_case_folds(&self) -> Result<bool> {
        let (fs, root) = (self.host().graph().fs(), self.host().graph().root());
        let probe = Path::new(".prov-case-probe.tmp");
        write_probe(fs, root, probe, b"").await?;
        let collides = self
            .host()
            .graph()
            .exists(Path::new(".PROV-CASE-PROBE.tmp"))
            .await;
        let _ = discard_file(fs, root, probe).await;
        collides
    }

    /// The subject's live path, when the next capture would park its bytes again.
    ///
    /// Tested against the **capture set** rather than mere existence on disk,
    /// because that is exactly the population a capture parks — a file sitting
    /// unreachable in the tree would not come back, and refusing on its account
    /// would be refusing for a reason that is not true.
    pub(crate) async fn subject_live(
        &self,
        root_doc: &Path,
        subject: &Subject,
    ) -> Result<Option<PathBuf>> {
        let path = match subject {
            Subject::Path(path) => link::normalize(path),
            Subject::Id(id) => match self.host().graph().index().resolve(id) {
                Some(path) => link::normalize(path),
                None => return Ok(None),
            },
        };
        Ok(self
            .capture_set(root_doc)
            .await?
            .into_iter()
            .find(|captured| *captured == path))
    }
}

impl<H: HistoryWriteHost> HistoryStore<H> {
    /// Execute a [`RestorePlan`]: write the captured bytes back, and — under a
    /// plan built with `exact` — remove what the capture did not hold.
    ///
    /// Takes the plan rather than recomputing it, so what runs is exactly what the
    /// caller showed and the user agreed to.
    ///
    /// ## What it never touches
    ///
    /// - **The store itself.** No manifest row can name a path inside it — the
    ///   capture set is blind to it by construction — and the removal pass is
    ///   drawn from that same set, so neither half of a restore can reach in. An
    ///   `exact` restore of an old event deleting every event newer than it is the
    ///   failure this rules out.
    /// - **The root's `history` pointer.** A captured root predating the store (or
    ///   hand-edited since) must not strand it unreachable, so a restored root that
    ///   declares no pointer gets one before it is written. Present-but-different is
    ///   left alone: that is the capture's truth about where the store lived.
    /// - **The registry, as a data structure.** The registry *document* is an
    ///   ordinary captured file and comes back with the rest; nothing here edits
    ///   the in-memory index, which is why a caller must re-open the workspace
    ///   before reading it again.
    ///
    /// ## Why the bytes ride a `CopyFrom`
    ///
    /// The journal embeds file contents, so staging a whole restored workspace as
    /// [`ChangeSet::write`](prov_transaction::ChangeSet::write) would duplicate
    /// the entire tree into the journal at the commit point.
    /// [`FileOp::CopyFrom`](prov_transaction::FileOp::CopyFrom) journals the
    /// *source path* instead, and a history blob is exactly the immutable,
    /// content-addressed referent that makes replaying such a reference
    /// deterministic: the path is the digest of the contents, so the bytes found
    /// there are the bytes intended, or the file is gone and replay fails loudly.
    pub async fn restore(
        &mut self,
        root_doc: &Path,
        plan: &RestorePlan,
        force: bool,
    ) -> Result<()> {
        let root_doc = link::normalize(root_doc);
        let (store_index, _) = self.store_index(&root_doc).await?;
        if let Some(conflict) = plan.conflicts.first()
            && !force
        {
            return Err(conflict.collision.clone().into());
        }

        // Sorted by disposition, so writes are staged before removals — the order a
        // half-applied set should fail in, and the order the plan was read in.
        let mut cs = self.host_mut().change();
        for op in &plan.ops {
            // A row found only under a different case is moved to the manifest's
            // own spelling first — the rename-in-place that keeps this write (or,
            // for `CaseOnly`, this rename alone) and the `exact` pass that spared
            // the same on-disk file above in agreement about which path it now
            // lives at. Staged ahead of the write below, and a set applies its
            // ops in order, so a rename immediately followed by a write to its
            // own destination is exactly the sequencing `ChangeSet` promises.
            if let Some(from) = &op.rename_from {
                cs.rename(from, &op.path);
            }
            match op.disposition {
                Disposition::Create | Disposition::Overwrite => {
                    let hash = op.hash.as_deref().ok_or_else(|| {
                        Error::Structure(format!(
                            "{} has no captured digest to restore from",
                            op.path.display()
                        ))
                    })?;
                    let blob = blob_path(&store_index, hash)?;
                    match op.path == root_doc {
                        true => {
                            let bytes = self.host().graph().read_bytes(&blob).await?;
                            let text = String::from_utf8(bytes).map_err(|e| {
                                Error::Structure(format!(
                                    "the captured {} is not valid UTF-8: {e}",
                                    root_doc.display()
                                ))
                            })?;
                            cs.write(
                                &root_doc,
                                self.rooted_at_store(&root_doc, &text, &store_index)?,
                            );
                        }
                        false => {
                            cs.copy_from(&op.path, blob);
                        }
                    }
                }
                Disposition::Remove => {
                    cs.remove(&op.path);
                }
                // `CaseOnly` already got everything it needs from the rename
                // staged above — the bytes were already right, only the name
                // wasn't. `Unchanged` and `NoBytes` write nothing at all.
                Disposition::CaseOnly | Disposition::Unchanged | Disposition::NoBytes => {}
            }
        }
        self.host_mut().commit(cs).await
    }
}
