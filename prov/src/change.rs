//! Transactional writes — the unit every mutation lands through.
//!
//! Mutating a linked workspace is never a single-file operation: the spanning
//! relation and its inverse live in *other* documents, so `create` touches two
//! files, `reparent` three, and a `rename` in a well-linked tree as many as have
//! ever pointed at the moved document. [`crate::mutate`] has always computed
//! every one of those edits *before* touching the filesystem — the hard half —
//! but it then wrote them one at a time, so an I/O failure partway through the
//! burst left the workspace torn: links maintained in the documents already
//! written, dangling in the ones not reached.
//!
//! A [`ChangeSet`] closes that window. An operation stages its writes into one
//! instead of issuing them, and [`ChangeSet::apply`] executes the whole set as a
//! unit: each op records how to undo itself *at the moment it runs*, and the
//! first failure unwinds every op already applied, in reverse. Either the whole
//! set lands or the workspace is as it was.
//!
//! ## What this does and does not buy
//!
//! This is **error** atomicity, and — per *file* — **crash** atomicity too; what
//! it is not yet is crash atomicity across the whole *set*. A failed write, a
//! full disk, a permission error, a rejected edit — none of these can leave a
//! half-linked workspace, because unwinding puts back every op already applied.
//! And no single document can be caught half-written even by a power cut: every
//! [`FileOp::Write`] lands through [`Storage::write_atomic`], which stages the
//! new bytes in a temporary sibling, flushes them, and renames it over the
//! target, so an observer sees the whole old document or the whole new one, never
//! a splice. What a `kill -9` or a power cut *between* ops can still do is leave
//! some of the set's files on their new contents and some on their old — each one
//! individually intact, but the set torn at a document boundary. The in-memory
//! unwind cannot repair that, because memory dies with the process. Closing that
//! last window needs a write-ahead journal on disk, replayed on recovery; the
//! [`Storage::write_atomic`]/[`Storage::sync`] seam it will order itself against
//! now exists, but the journal itself is a separate piece of work. The
//! distinction is worth keeping sharp: the half in place already covers every
//! failure short of a crash mid-set, and a crash mid-set can only land on a
//! document boundary that [`crate::validate`] can name.
//!
//! Two smaller honesties, both deliberate:
//!
//! - **Directories are not unwound.** Applying a set creates any parent
//!   directory its writes need; a rollback leaves an empty one behind. An empty
//!   directory is litter, not a torn workspace — prov's graph lives in the
//!   documents, so nothing about it is wrong (DESIGN §1).
//! - **Undo is held in memory.** Overwriting or removing a file reads its old
//!   bytes first so the rollback can put them back, which means a removed opaque
//!   payload (an attached photo) is briefly held whole. Documents are small and
//!   the buffer lives only for the length of the apply.
//!
//! ## Staging is also a plan
//!
//! Because a set is a value that describes writes without performing them, it is
//! equally an answer to "what *would* this do?" — the shape `--dry-run` needs.
//! [`crate::route::RoutePlan`] and [`crate::intake::StructurePlan`] already model
//! the *semantic* plan (which documents should exist); a `ChangeSet` is the
//! *physical* one (which bytes reach which files), and the two compose: a
//! semantic plan is realized by the ops that build change sets.
//!
//! This module (with its recovery arm in [`crate::journal`]) is the crate's
//! sole [`Storage`] write surface — staged ops through [`ChangeSet::apply`],
//! and the handful of writes that genuinely are not document mutations
//! through the raw facade at the bottom of this file — so every write's
//! durability and ordering guarantee is reviewable in one place.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::fs::Storage;

/// One staged filesystem operation. Paths are **workspace-relative** — the root
/// is joined on at [`apply`](ChangeSet::apply) time, so a set is portable
/// between workspaces and prints readably in a dry run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOp {
    /// Write `bytes` to `path`, creating it (and any missing parent directory)
    /// or replacing it wholesale.
    Write {
        /// The file to write.
        path: PathBuf,
        /// Its full new contents.
        bytes: Vec<u8>,
    },
    /// Move `from` to `to`, creating any missing parent directory of `to`.
    Rename {
        /// The current path.
        from: PathBuf,
        /// The new path.
        to: PathBuf,
    },
    /// Remove the file at `path`. It must exist.
    Remove {
        /// The file to remove.
        path: PathBuf,
    },
    /// Copy the bytes already on disk at `source` to `path`, verbatim.
    ///
    /// [`Write`](FileOp::Write) with the payload left where it lies. The journal
    /// records the *source path* instead of the bytes, so a set that writes a
    /// large payload costs O(path) of journal rather than a second copy of every
    /// byte — which is what makes putting a whole captured workspace back
    /// tractable, where `Write` would duplicate the entire tree into
    /// `.prov-journal` at the commit point.
    ///
    /// **The source must be immutable for the lifetime of the change**, because
    /// that is the entire correctness argument. A `Write` journals the exact bytes
    /// it intends, so replay after a crash is deterministic by construction; a
    /// `CopyFrom` journals a reference, and replay is deterministic only if the
    /// referent cannot have changed underneath it. A content-addressed history
    /// blob satisfies this by definition — its path *is* the digest of its
    /// contents, so bytes found there are the bytes intended, or the file is gone
    /// and replay fails loudly. Do not point this at a mutable document, at a
    /// path some other op in the same set writes, or at anything outside the
    /// workspace.
    ///
    /// This bounds *journal* growth, not peak memory: rollback still buffers the
    /// bytes it overwrites, exactly as `Write` does.
    CopyFrom {
        /// The file to write.
        path: PathBuf,
        /// The workspace-relative file to copy from — immutable, and ideally
        /// content-addressed.
        source: PathBuf,
    },
}

impl FileOp {
    /// The path this op ultimately affects — the destination for a write or a
    /// rename, the victim for a remove. What a dry run lists.
    pub fn path(&self) -> &Path {
        match self {
            FileOp::Write { path, .. } | FileOp::CopyFrom { path, .. } => path,
            FileOp::Rename { to, .. } => to,
            FileOp::Remove { path } => path,
        }
    }
}

/// A set of writes staged as one unit, applied all-or-nothing by
/// [`apply`](ChangeSet::apply).
///
/// Built by the mutation ops as they compute their edits, and applied once at
/// the end. Ops execute in the order they were staged: a set is a *sequence*,
/// not a bag, because `rename`-then-write and remove-then-rewrite-the-parent
/// depend on it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeSet {
    ops: Vec<FileOp>,
}

impl ChangeSet {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stage a write of `contents` to `path` (workspace-relative).
    pub fn write(&mut self, path: impl Into<PathBuf>, contents: impl Into<Vec<u8>>) -> &mut Self {
        self.ops.push(FileOp::Write {
            path: path.into(),
            bytes: contents.into(),
        });
        self
    }

    /// Stage a move from `from` to `to` (both workspace-relative).
    pub fn rename(&mut self, from: impl Into<PathBuf>, to: impl Into<PathBuf>) -> &mut Self {
        self.ops.push(FileOp::Rename {
            from: from.into(),
            to: to.into(),
        });
        self
    }

    /// Stage the removal of `path` (workspace-relative).
    pub fn remove(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.ops.push(FileOp::Remove { path: path.into() });
        self
    }

    /// Stage a copy of the file at `source` to `path` (both workspace-relative),
    /// instead of carrying its bytes through the set.
    ///
    /// See [`FileOp::CopyFrom`] for the immutability the source has to satisfy —
    /// it is what keeps crash recovery deterministic.
    pub fn copy_from(&mut self, path: impl Into<PathBuf>, source: impl Into<PathBuf>) -> &mut Self {
        self.ops.push(FileOp::CopyFrom {
            path: path.into(),
            source: source.into(),
        });
        self
    }

    /// The staged ops, in execution order. The dry-run view.
    pub fn ops(&self) -> &[FileOp] {
        &self.ops
    }

    /// The bytes this set will leave at `path`, if it writes it — the *last*
    /// write staged, since a later one supersedes an earlier.
    ///
    /// This is what makes a set safe to read back mid-build. A document can be
    /// touched twice by one op (`reparent` repoints a child that is somehow its
    /// own old parent, and must then edit the text it just staged rather than the
    /// stale copy on disk), and before staging existed the second edit read the
    /// first one's *write* off the filesystem. Nothing hits the filesystem now
    /// until commit, so the set has to answer instead.
    ///
    /// `None` if the set does not write `path` — including when it renames or
    /// removes it, and including a [`FileOp::CopyFrom`], whose bytes are on disk
    /// at the source rather than held in the set. This is deliberately a lookup,
    /// not a filesystem overlay: it resolves the one hazard staging introduces and
    /// nothing more. A caller that must read back a path it staged a copy to has
    /// to read the source itself.
    pub fn staged(&self, path: &Path) -> Option<&[u8]> {
        self.ops.iter().rev().find_map(|op| match op {
            FileOp::Write { path: p, bytes } if p == path => Some(bytes.as_slice()),
            _ => None,
        })
    }

    /// Where this set moves `path` to, if it moves it — following a chain of
    /// renames to the final destination. `None` if the set leaves it where it is.
    ///
    /// The companion to [`staged`](Self::staged) for anything holding a path this
    /// set might move out from under it. The registry is exactly that: it knows
    /// which document it persists into, and a set that renames that document has
    /// to be followed, or its write lands at a path the set just emptied.
    pub fn renamed_to(&self, path: &Path) -> Option<PathBuf> {
        let mut current = path.to_path_buf();
        let mut moved = false;
        for op in &self.ops {
            if let FileOp::Rename { from, to } = op
                && *from == current
            {
                current = to.clone();
                moved = true;
            }
        }
        moved.then_some(current)
    }

    /// Whether nothing is staged — [`apply`](ChangeSet::apply) would be a no-op.
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// The number of staged ops.
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Append `other`'s ops after this set's, consuming it.
    pub fn extend(&mut self, other: ChangeSet) -> &mut Self {
        self.ops.extend(other.ops);
        self
    }

    /// Execute every staged op against `fs`, rooted at `root`, as one unit —
    /// crash-atomically, behind a write-ahead journal.
    ///
    /// The set's intent is journaled and flushed *before* any document is
    /// touched (see [`crate::journal`]); that flush is the commit point. From
    /// there the ops run in order, each recording how to undo itself:
    ///
    /// - **On success**, the journal is removed and the change is done.
    /// - **On an error** (a full disk, a permission fault), every op already
    ///   applied is unwound in reverse, the workspace is restored to what it was,
    ///   and the journal is cleared — the mutation aborts as if it never began.
    /// - **On a crash** (a `kill -9`, a power cut) there is no error to catch and
    ///   no chance to unwind, so the journal simply survives; the next
    ///   [`crate::journal::recover`] rolls the set forward to its fully-applied
    ///   state. An interrupted change set is therefore always resolved to a
    ///   consistent workspace — fully before it on a caught error, fully after it
    ///   on a crash.
    ///
    /// A set of **one** op skips the journal entirely: a single op is already
    /// indivisible on a backend claiming `atomic_replace`, so there is no
    /// multi-file window for a journal to close, and a crash leaves the op either
    /// wholly done or wholly not — the same two states a recovered set lands on.
    /// It is the ordinary shape of a save, and it costs one file operation rather
    /// than four.
    ///
    /// The rare exception is a rollback that *itself* fails ([`Error::Torn`]):
    /// prov could not restore the pre-change state, so — rather than leave an
    /// unknown one — it keeps the journal, and recovery will later roll the set
    /// forward to the consistent applied state. Either way the workspace lands on
    /// a state prov can name.
    ///
    /// Takes `fs`/`root` rather than a [`crate::workspace::Workspace`] so a
    /// caller with neither — a bootstrap that must write two files before a
    /// workspace exists to write them through — can still land them together.
    pub async fn apply<FS: Storage>(&self, fs: &FS, root: &Path) -> Result<()> {
        if self.ops.is_empty() {
            return Ok(());
        }
        // Clamp every staged path to the workspace root *before* anything is
        // written or journaled. A set is built from workspace-relative,
        // already-normalized paths, so an escaping op cannot arise from prov's
        // own mutations — but `apply` also lands sets a caller assembled directly,
        // and a link target that resolves to `../../../etc/passwd` must be refused
        // rather than have the workspace write outside the tree it was pointed at.
        for op in &self.ops {
            match op {
                FileOp::Write { path, .. } | FileOp::Remove { path } => {
                    guard_in_root(path)?;
                }
                FileOp::Rename { from, to } => {
                    guard_in_root(from)?;
                    guard_in_root(to)?;
                }
                // The source is clamped too: it is read, and a set assembled by a
                // caller must not be able to pull `../../../etc/passwd` into the
                // workspace any more than it may write out of one.
                FileOp::CopyFrom { path, source } => {
                    guard_in_root(path)?;
                    guard_in_root(source)?;
                }
            }
        }
        // Refuse to clobber a journal left by a *previous* interrupted change. Its
        // presence means an earlier mutation crashed mid-apply and has not been
        // recovered; overwriting it with this set's intent would strand the old
        // change half-applied with no record of how to finish it. Recovery
        // ([`crate::journal::recover`], which `prov check` runs) must complete
        // it first. A journal this same apply is about to write does not exist yet,
        // so this only ever fires on a genuinely stale one.
        let journal = root.join(crate::journal::JOURNAL_NAME);
        if fs.try_exists(&journal).await? {
            return Err(Error::StaleJournal(journal));
        }
        // A set of one needs no journal. The journal exists to make *several*
        // file operations land as one unit; a lone op is already indivisible on a
        // backend claiming `atomic_replace` — a `write_atomic` is all-or-nothing
        // by construction, and a lone `rename` or `unlink` is atomic by the
        // filesystem's own guarantee. Journaling it would write, flush, and then
        // delete a second file in order to restate a promise the op already
        // carries, roughly tripling what the commonest mutation there is — saving
        // one document — costs in writes and flushes alike.
        //
        // What this gives up is *liveness*, not safety. With a journal, a crash
        // mid-apply is rolled forward to the applied state by the next
        // [`crate::journal::recover`]; without one, a crash simply means the op
        // did not happen. For a set of one those are the only two states there
        // are — no caller can observe a half-applied set of one — so all that
        // changes is which side of the atomic instant a crash lands on, never
        // whether it lands on one at all.
        //
        // The stale-journal refusal above still applies: this path writes no
        // journal, but it must not slip a write past an *earlier* interrupted
        // change that recovery has yet to roll forward, or recovery would later
        // overwrite what was just written.
        if self.ops.len() == 1 && fs.capabilities().atomic_replace {
            // No undo to record, either. Nothing preceded this op that could need
            // unwinding, and every failure mode leaves the target untouched — so
            // the reflexive read of the very file about to be overwritten, whose
            // only purpose is to hold the old bytes for a rollback that cannot
            // happen here, goes with it.
            return exec(fs, root, &self.ops[0], None).await;
        }
        // The commit point: durably record the whole intent before touching a
        // single document. `write_atomic` flushes it, so a crash finds the
        // journal whole or not at all — never half-written.
        fs.write_atomic(&journal, &crate::journal::encode(&self.ops)?)
            .await?;

        let mut undo: Vec<Undo> = Vec::new();
        for op in &self.ops {
            let Err(cause) = exec(fs, root, op, Some(&mut undo)).await else {
                continue;
            };
            return Err(match unwind(fs, undo).await {
                // Reverted cleanly: the change aborted, so the journal must go —
                // otherwise recovery would later roll this very set *forward*,
                // undoing the abort. If even the delete fails, fall through to
                // `Torn` and let recovery complete the set instead.
                Ok(()) => match fs.remove_file(&journal).await {
                    Ok(()) => cause,
                    Err(cleanup) => Error::Torn {
                        cause: cause.to_string(),
                        rollback: cleanup.to_string(),
                    },
                },
                // Could not revert: keep the journal so recovery rolls the set
                // forward to the consistent applied state.
                Err(rollback) => Error::Torn {
                    cause: cause.to_string(),
                    rollback: rollback.to_string(),
                },
            });
        }
        // Applied cleanly. Drop the journal; if this delete fails, a later
        // recovery re-applies the set idempotently and clears it — harmless.
        fs.remove_file(&journal).await?;
        Ok(())
    }
}

/// How to reverse one applied op, recorded against the state that op found.
///
/// Recorded *per op at execution time*, not for the whole set up front, because
/// ops in a set are not independent: `rename` moves `a.md` to `sub/a.md` and
/// then rewrites `sub/a.md`'s re-relativized links, so the write's undo has to
/// restore the bytes the rename put there — a snapshot taken before the set ran
/// would say "`sub/a.md` did not exist; delete it", and the rename's undo would
/// then have nothing to move back. Paths here are already root-joined.
enum Undo {
    /// Put these bytes back (the file existed and was overwritten or removed).
    Restore { path: PathBuf, bytes: Vec<u8> },
    /// Delete the file (it did not exist before the write created it).
    ///
    /// Recorded *before* the write it reverses, because a write that fails
    /// partway still leaves a file behind — so this has to tolerate finding
    /// nothing there, which is the case where the write failed before creating
    /// anything at all. Undoing nothing is success, not a torn workspace.
    Delete { path: PathBuf },
    /// Move `from` back to `to`.
    Rename { from: PathBuf, to: PathBuf },
}

/// Apply one op, optionally recording how to reverse it.
///
/// `undo` is `None` only for a set of one, which has no rollback to feed: see
/// the fast path in [`ChangeSet::apply`]. Recording is not merely unused there,
/// it is worth skipping — for a write it costs a full read of the file about to
/// be replaced.
async fn exec<FS: Storage>(
    fs: &FS,
    root: &Path,
    op: &FileOp,
    undo: Option<&mut Vec<Undo>>,
) -> Result<()> {
    match op {
        FileOp::Write { path, bytes } => {
            let full = root.join(path);
            // Record the undo *before* writing: a write that fails partway
            // (a full disk) leaves a truncated file, and restoring the old
            // bytes over it is exactly the repair.
            if let Some(undo) = undo {
                match fs.read(&full).await {
                    Ok(old) => undo.push(Undo::Restore {
                        path: full.clone(),
                        bytes: old,
                    }),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        undo.push(Undo::Delete { path: full.clone() });
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            ensure_parent(fs, &full).await?;
            // Land the document through the atomic-replace protocol, so even a
            // crash mid-write cannot expose a half-written file: the write goes to
            // a staging sibling and is renamed into place. On a backend without
            // atomic rename this degrades to a plain durable write (see
            // [`Storage::write_atomic`]) — the per-file guarantee follows the
            // backend's declared capabilities.
            fs.write_atomic(&full, bytes).await?;
        }
        FileOp::Rename { from, to } => {
            let (from_full, to_full) = (root.join(from), root.join(to));
            ensure_parent(fs, &to_full).await?;
            fs.rename(&from_full, &to_full).await?;
            if let Some(undo) = undo {
                undo.push(Undo::Rename {
                    from: to_full,
                    to: from_full,
                });
            }
        }
        FileOp::Remove { path } => {
            let full = root.join(path);
            match undo {
                // The removed bytes are the undo, so they have to be read out
                // before the file goes.
                Some(undo) => {
                    let old = fs.read(&full).await?;
                    fs.remove_file(&full).await?;
                    undo.push(Undo::Restore {
                        path: full,
                        bytes: old,
                    });
                }
                None => fs.remove_file(&full).await?,
            }
        }
        // A `Write` whose bytes were left at the source. The read happens here, at
        // execution time, rather than when the op was staged — that is the whole
        // saving, and it is why the source has to be immutable.
        FileOp::CopyFrom { path, source } => {
            let (full, source_full) = (root.join(path), root.join(source));
            let bytes = fs.read(&source_full).await?;
            if let Some(undo) = undo {
                match fs.read(&full).await {
                    Ok(old) => undo.push(Undo::Restore {
                        path: full.clone(),
                        bytes: old,
                    }),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        undo.push(Undo::Delete { path: full.clone() });
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            ensure_parent(fs, &full).await?;
            fs.write_atomic(&full, &bytes).await?;
        }
    }
    Ok(())
}

/// Reverse every recorded op, last-applied first. Best-effort: a step that fails
/// does not abandon the rest — the more that is put back the better — and the
/// first failure is what gets reported.
async fn unwind<FS: Storage>(fs: &FS, undo: Vec<Undo>) -> Result<()> {
    let mut first_error = None;
    for step in undo.into_iter().rev() {
        let result = match step {
            Undo::Restore { path, bytes } => fs.write(&path, &bytes).await,
            // Already absent is already undone — see `Undo::Delete`. Reporting it
            // would raise `Error::Torn` over the single most ordinary rollback
            // there is: a write to a new file that failed before creating it.
            Undo::Delete { path } => match fs.remove_file(&path).await {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            },
            Undo::Rename { from, to } => fs.rename(&from, &to).await,
        };
        if let Err(e) = result
            && first_error.is_none()
        {
            first_error = Some(e);
        }
    }
    match first_error {
        Some(e) => Err(e.into()),
        None => Ok(()),
    }
}

/// Refuse a staged path that would resolve outside the workspace root. The
/// write-side counterpart to the read guard in [`crate::Workspace`]'s `load`;
/// both defer to [`crate::link::escapes_root`] so read and write clamp to the
/// exact same boundary.
fn guard_in_root(path: &Path) -> Result<()> {
    if crate::link::escapes_root(path) {
        return Err(Error::Escape(path.to_path_buf()));
    }
    Ok(())
}

/// Create `full`'s parent directory if it is missing. Unconditional (rather than
/// staged as its own op) because a directory is not part of the document graph:
/// it is an artifact of *where* a write lands, so it belongs to the write.
async fn ensure_parent<FS: Storage>(fs: &FS, full: &Path) -> Result<()> {
    if let Some(dir) = full.parent() {
        fs.create_dir_all(dir).await?;
    }
    Ok(())
}

// Raw writes that deliberately bypass `ChangeSet` staging.
//
// Everything above lands a *document mutation*: an edit with an undo, that
// must reach disk with its siblings or not at all. The three functions below
// are not that, and routing them through a set would misrepresent what they
// are — see each one's own doc for its reason. They live here rather than
// beside their callers in `history/` so that every `Storage` write in the
// crate stays reviewable in one file — this one owns the durability and
// ordering policy, even for the writes that opt out of it. Each is a thin,
// byte-identical delegation to the direct calls it replaces.

/// Park `bytes` at `path` (workspace-relative) as a content-addressed blob,
/// creating any missing parent directory.
///
/// Bypasses [`ChangeSet`] staging on purpose: a blob's address is the digest
/// of its bytes, so parking one is idempotent — replaying it after a crash
/// can only write the same bytes to the same path — and there is nothing for
/// an unwind to undo. Riding the change set would also cost a second whole
/// copy of the payload in the journal, which embeds file contents; see
/// [`Workspace::history_capture`](crate::workspace::Workspace) for the full
/// argument.
pub(crate) async fn write_blob_atomic<FS: Storage>(
    fs: &FS,
    root: &Path,
    path: &Path,
    bytes: &[u8],
) -> Result<()> {
    let full = root.join(path);
    ensure_parent(fs, &full).await?;
    fs.write_atomic(&full, bytes).await?;
    Ok(())
}

/// Remove the file at `path` (workspace-relative). It must exist.
///
/// Bypasses [`ChangeSet`] staging on purpose, for two different reasons at
/// its two call sites. Freeing a history blob
/// ([`Workspace::history_prune`](crate::workspace::Workspace),
/// [`Workspace::history_forget`](crate::workspace::Workspace)) happens
/// *after* the index rewrite that stops referencing it has already committed
/// as its own set, so the removal was never part of that transaction — an
/// interrupted GC pass just leaves the blob for the next run to find again.
/// Removing [`write_probe`]'s throwaway file is not a document mutation to
/// begin with.
pub(crate) async fn discard_file<FS: Storage>(fs: &FS, root: &Path, path: &Path) -> Result<()> {
    fs.remove_file(&root.join(path)).await?;
    Ok(())
}

/// Write `bytes` to `path` (workspace-relative) directly, without the
/// atomic-replace protocol a document write gets.
///
/// Bypasses [`ChangeSet`] staging on purpose: this is for a throwaway file
/// used once to answer a question — [`Workspace::filesystem_case_folds`](
/// crate::workspace::Workspace) probes whether the filesystem folds ASCII
/// case — and then removed with [`discard_file`]. It is not workspace data,
/// so there is nothing here to stage or undo.
pub(crate) async fn write_probe<FS: Storage>(
    fs: &FS,
    root: &Path,
    path: &Path,
    bytes: &[u8],
) -> Result<()> {
    fs.write(&root.join(path), bytes).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::block_on;
    use crate::fs::{FailAtWrite, FsEvent, RecordingFs, StdFs};
    use crate::journal::JOURNAL_NAME;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prov-change-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn read(root: &Path, rel: &str) -> Option<String> {
        std::fs::read_to_string(root.join(rel)).ok()
    }

    #[test]
    fn applies_every_op_in_order() {
        let root = tmp("apply");
        std::fs::write(root.join("parent.md"), "old parent").unwrap();
        let mut cs = ChangeSet::new();
        cs.write("child.md", "child");
        cs.write("parent.md", "new parent");
        block_on(cs.apply(&StdFs, &root)).unwrap();
        assert_eq!(read(&root, "child.md").as_deref(), Some("child"));
        assert_eq!(read(&root, "parent.md").as_deref(), Some("new parent"));
    }

    #[test]
    fn creates_missing_parent_directories() {
        let root = tmp("mkdir");
        let mut cs = ChangeSet::new();
        cs.write("deep/nested/child.md", "hi");
        block_on(cs.apply(&StdFs, &root)).unwrap();
        assert_eq!(read(&root, "deep/nested/child.md").as_deref(), Some("hi"));
    }

    #[test]
    fn a_copy_lands_the_source_bytes_and_leaves_the_source_alone() {
        let root = tmp("copy");
        std::fs::create_dir_all(root.join("history/blobs/9f")).unwrap();
        std::fs::write(root.join("history/blobs/9f/86d081"), "captured").unwrap();
        std::fs::write(root.join("notes.md"), "damaged").unwrap();

        let mut cs = ChangeSet::new();
        cs.copy_from("notes.md", "history/blobs/9f/86d081");
        // A path that does not exist yet gets its parent made, as a write does.
        cs.copy_from("deep/fresh.md", "history/blobs/9f/86d081");
        block_on(cs.apply(&StdFs, &root)).unwrap();

        assert_eq!(read(&root, "notes.md").as_deref(), Some("captured"));
        assert_eq!(read(&root, "deep/fresh.md").as_deref(), Some("captured"));
        // The blob is shared by every event naming it: read, never consumed.
        assert_eq!(
            read(&root, "history/blobs/9f/86d081").as_deref(),
            Some("captured")
        );
    }

    #[test]
    fn a_failed_copy_rolls_back_exactly_as_a_failed_write_does() {
        // A copy is a write whose payload was fetched late, so it must record the
        // same undo: restore what it overwrote, delete what it created.
        let root = tmp("rollback-copy");
        std::fs::create_dir_all(root.join("history/blobs/9f")).unwrap();
        std::fs::write(root.join("history/blobs/9f/86d081"), "captured").unwrap();
        std::fs::write(root.join("notes.md"), "damaged").unwrap();

        let mut cs = ChangeSet::new();
        cs.copy_from("notes.md", "history/blobs/9f/86d081");
        cs.copy_from("fresh.md", "history/blobs/9f/86d081");
        cs.write("doomed.md", "never lands");
        let err = block_on(cs.apply(&FailAtWrite::nth(2), &root)).unwrap_err();
        assert!(err.to_string().contains("disk full"), "{err}");

        assert_eq!(read(&root, "notes.md").as_deref(), Some("damaged"));
        assert_eq!(read(&root, "fresh.md"), None);
    }

    #[test]
    fn a_copy_from_a_missing_source_fails_before_the_target_is_touched() {
        // The half-synced event: the manifest names a blob the transport has not
        // delivered. Better to fail the set than to write a hole into the tree.
        let root = tmp("copy-missing-source");
        std::fs::write(root.join("notes.md"), "damaged").unwrap();
        let mut cs = ChangeSet::new();
        cs.copy_from("notes.md", "history/blobs/9f/86d081");
        assert!(block_on(cs.apply(&StdFs, &root)).is_err());
        assert_eq!(read(&root, "notes.md").as_deref(), Some("damaged"));
    }

    #[test]
    fn a_copy_cannot_read_from_outside_the_root() {
        // The source is read, so it is clamped like every written path: a set a
        // caller assembled must not be able to pull the host's files into the tree.
        let root = tmp("copy-escape");
        let mut cs = ChangeSet::new();
        cs.copy_from("stolen.md", "../../../etc/passwd");
        let err = block_on(cs.apply(&StdFs, &root)).unwrap_err();
        assert!(matches!(err, Error::Escape(_)), "{err:?}");
        assert_eq!(read(&root, "stolen.md"), None);
    }

    #[test]
    fn a_failed_write_restores_the_files_already_written() {
        let root = tmp("rollback-write");
        std::fs::write(root.join("parent.md"), "old parent").unwrap();
        std::fs::write(root.join("child.md"), "old child").unwrap();

        // Three writes staged; the third fails.
        let mut cs = ChangeSet::new();
        cs.write("child.md", "new child");
        cs.write("parent.md", "new parent");
        cs.write("third.md", "third");
        let err = block_on(cs.apply(&FailAtWrite::nth(2), &root)).unwrap_err();
        assert!(err.to_string().contains("disk full"), "{err}");

        // Everything is as it was found — no half-linked workspace.
        assert_eq!(read(&root, "child.md").as_deref(), Some("old child"));
        assert_eq!(read(&root, "parent.md").as_deref(), Some("old parent"));
    }

    #[test]
    fn a_failed_write_deletes_files_the_set_had_created() {
        let root = tmp("rollback-create");
        let mut cs = ChangeSet::new();
        cs.write("fresh.md", "fresh");
        cs.write("doomed.md", "doomed");
        let err = block_on(cs.apply(&FailAtWrite::nth(1), &root)).unwrap_err();
        assert!(err.to_string().contains("disk full"), "{err}");
        // The file the set created before failing is gone, not orphaned.
        assert_eq!(read(&root, "fresh.md"), None);
    }

    #[test]
    fn a_clean_rollback_reports_the_cause_not_a_tear() {
        // `Torn` means "prov cannot say what is on disk" — it must be reserved
        // for a rollback that genuinely failed. The commonest rollback of all is a
        // write to a *new* file that failed before creating it, whose undo then
        // finds nothing to delete; calling that a tear would cry wolf on every
        // ordinary full disk. Asserted on the variant, because `Torn`'s message
        // embeds the cause and so still matches a "disk full" substring check.
        let root = tmp("clean-rollback");
        std::fs::write(root.join("existing.md"), "before").unwrap();
        let mut cs = ChangeSet::new();
        cs.write("existing.md", "after");
        cs.write("brand-new.md", "never lands");
        let err = block_on(cs.apply(&FailAtWrite::nth(1), &root)).unwrap_err();

        assert!(
            matches!(err, Error::Io(_)),
            "a clean rollback should surface the cause itself, got: {err:?}"
        );
        assert_eq!(read(&root, "existing.md").as_deref(), Some("before"));
        assert_eq!(read(&root, "brand-new.md"), None);
    }

    #[test]
    fn a_failed_write_after_a_rename_moves_the_file_back() {
        // The ordering `mutate::rename` actually uses: move the file, then
        // rewrite it with its re-relativized links. The write's undo must
        // restore the *renamed* bytes so the rename's undo has something to
        // move back — the reason undo is recorded per-op, not up front.
        let root = tmp("rollback-rename");
        std::fs::write(root.join("a.md"), "original").unwrap();
        let mut cs = ChangeSet::new();
        cs.rename("a.md", "sub/a.md");
        cs.write("sub/a.md", "rewritten");
        cs.write("parent.md", "never gets here");
        let err = block_on(cs.apply(&FailAtWrite::nth(1), &root)).unwrap_err();
        assert!(err.to_string().contains("disk full"), "{err}");

        assert_eq!(read(&root, "a.md").as_deref(), Some("original"));
        assert_eq!(read(&root, "sub/a.md"), None);
    }

    #[test]
    fn a_failed_write_restores_a_removed_file() {
        let root = tmp("rollback-remove");
        std::fs::write(root.join("gone.md"), "precious").unwrap();
        let mut cs = ChangeSet::new();
        cs.remove("gone.md");
        cs.write("parent.md", "boom");
        let err = block_on(cs.apply(&FailAtWrite::nth(0), &root)).unwrap_err();
        assert!(err.to_string().contains("disk full"), "{err}");
        assert_eq!(read(&root, "gone.md").as_deref(), Some("precious"));
    }

    #[test]
    fn every_document_write_lands_atomically_and_leaves_no_temp_files() {
        // The payoff of routing `FileOp::Write` through `write_atomic`: applying a
        // set stages each document through a sibling and renames it into place, so
        // no reader ever catches one half-written, and a clean apply leaves not one
        // staging file behind.
        let root = tmp("apply-atomic");
        std::fs::write(root.join("parent.md"), "old parent").unwrap();
        let fs = RecordingFs::local();
        let mut cs = ChangeSet::new();
        cs.write("child.md", "child");
        cs.write("parent.md", "new parent");
        block_on(cs.apply(&fs, &root)).unwrap();

        assert_eq!(read(&root, "child.md").as_deref(), Some("child"));
        assert_eq!(read(&root, "parent.md").as_deref(), Some("new parent"));

        // Every write in the log is either a staging sibling or a rename target —
        // never a plain write straight to a document path.
        for event in fs.events() {
            if let FsEvent::Write(p) = event {
                let name = p.file_name().unwrap().to_string_lossy();
                assert!(
                    name.contains("prov-tmp"),
                    "wrote a document non-atomically: {name}"
                );
            }
        }
        // And nothing staging survives.
        let leftovers: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("prov-tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "staging files survived apply: {leftovers:?}"
        );
    }

    #[test]
    fn apply_journals_before_touching_documents_and_clears_it_after() {
        // The commit-point protocol: the journal is written and renamed into
        // place *before* the first document write, and removed *after* the last —
        // so a crash is always found with the journal either whole (roll forward)
        // or absent (nothing began).
        let root = tmp("journal-order");
        std::fs::write(root.join("parent.md"), "old parent").unwrap();
        let fs = RecordingFs::local();
        let mut cs = ChangeSet::new();
        cs.write("child.md", "child");
        cs.write("parent.md", "new parent");
        block_on(cs.apply(&fs, &root)).unwrap();

        let events = fs.events();
        let journal = root.join(JOURNAL_NAME);

        // The journal is renamed into place before any document write happens.
        let journal_committed = events
            .iter()
            .position(|e| matches!(e, FsEvent::Rename(_, to) if *to == journal))
            .expect("journal must be committed");
        let first_doc_write = events
            .iter()
            .position(|e| matches!(e, FsEvent::Write(p) if !crate::journal::is_journal_path(p)))
            .expect("a document must be written");
        assert!(
            journal_committed < first_doc_write,
            "the journal must be durable before any document is touched"
        );

        // And it is removed at the very end — nothing survives a clean apply.
        assert_eq!(events.last(), Some(&FsEvent::Remove(journal.clone())));
        assert!(!journal.exists());
    }

    #[test]
    fn a_set_of_one_lands_without_a_journal_at_all() {
        // The counterpart to the test above, and the reason it stages two ops: a
        // lone op is already indivisible, so the journal that makes *several* land
        // together has nothing left to guarantee and is skipped. The assertion is
        // the exact event list, because what is being claimed is an absence —
        // "contains no journal write" would still pass if the set quietly grew a
        // second file operation somewhere else.
        let root = tmp("journal-single");
        std::fs::write(root.join("doc.md"), "old").unwrap();
        let fs = RecordingFs::local();
        let mut cs = ChangeSet::new();
        cs.write("doc.md", "new");
        block_on(cs.apply(&fs, &root)).unwrap();

        let (target, temp) = (root.join("doc.md"), root.join(".doc.md.prov-tmp"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
        assert_eq!(
            fs.events(),
            vec![
                FsEvent::Write(temp.clone()),
                FsEvent::Sync(temp.clone(), crate::fs::Durability::Ordered),
                FsEvent::Rename(temp, target),
                FsEvent::Sync(root.clone(), crate::fs::Durability::Durable),
            ],
            "a set of one must cost exactly one atomic write and nothing else"
        );
        assert!(!root.join(JOURNAL_NAME).exists());
    }

    #[test]
    fn a_set_of_one_still_refuses_to_run_over_a_stale_journal() {
        // Skipping the journal must not also skip the *check* for one. An earlier
        // change crashed mid-apply and recovery has yet to roll it forward; a save
        // that slipped past would be silently overwritten when it finally does.
        let root = tmp("journal-single-stale");
        std::fs::write(root.join("doc.md"), "old").unwrap();
        std::fs::write(root.join(JOURNAL_NAME), "a previous change's intent").unwrap();

        let mut cs = ChangeSet::new();
        cs.write("doc.md", "new");
        let err = block_on(cs.apply(&StdFs, &root)).unwrap_err();

        assert!(matches!(err, Error::StaleJournal(_)), "got {err:?}");
        assert_eq!(
            std::fs::read_to_string(root.join("doc.md")).unwrap(),
            "old",
            "the refused write must not have happened"
        );
    }

    #[test]
    fn a_set_of_one_does_not_read_the_file_it_is_about_to_replace() {
        // The undo bookkeeping is what made a save read its own target back, and a
        // set of one has no rollback to feed it to. Proven the only way an absent
        // read can be: a target that cannot be read at all still writes fine.
        let root = tmp("journal-single-unreadable");
        let target = root.join("doc.md");
        std::fs::write(&target, "old").unwrap();
        let mut perms = std::fs::metadata(&target).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o200); // write-only: any read of it fails
        }
        std::fs::set_permissions(&target, perms).unwrap();

        let mut cs = ChangeSet::new();
        cs.write("doc.md", "new");
        block_on(cs.apply(&StdFs, &root)).expect("a write-only target is still replaceable");

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
    }

    #[test]
    fn a_caught_error_reverts_and_leaves_no_journal_behind() {
        // An error mid-apply unwinds to the pre-change state *and* clears the
        // journal — so a later recovery cannot roll the aborted set forward.
        let root = tmp("journal-abort");
        std::fs::write(root.join("existing.md"), "before").unwrap();
        let mut cs = ChangeSet::new();
        cs.write("existing.md", "after");
        cs.write("brand-new.md", "never lands");
        let err = block_on(cs.apply(&FailAtWrite::nth(1), &root)).unwrap_err();

        assert!(err.to_string().contains("disk full"), "{err}");
        assert_eq!(read(&root, "existing.md").as_deref(), Some("before"));
        assert_eq!(read(&root, "brand-new.md"), None);
        assert!(
            !root.join(JOURNAL_NAME).exists(),
            "a cleanly-reverted change must not leave a journal to roll forward"
        );
    }

    #[test]
    fn a_crash_mid_apply_is_recovered_forward_from_the_journal() {
        // The end-to-end crash story: apply writes the journal, a crash strikes
        // before the set finishes (modeled by leaving the journal and only the
        // first write on disk), and `recover` rolls the rest forward.
        let root = tmp("journal-crash");
        std::fs::write(root.join("parent.md"), "old parent").unwrap();
        let mut cs = ChangeSet::new();
        cs.write("child.md", "child");
        cs.write("parent.md", "new parent");

        // The journal the real apply would have committed at its commit point.
        std::fs::write(
            root.join(JOURNAL_NAME),
            crate::journal::encode(cs.ops()).unwrap(),
        )
        .unwrap();
        // A crash after the first document landed but before the second.
        std::fs::write(root.join("child.md"), "child").unwrap();

        let outcome = block_on(crate::journal::recover(&StdFs, &root)).unwrap();
        assert_eq!(outcome, crate::journal::Recovered::Applied(2));
        assert_eq!(read(&root, "child.md").as_deref(), Some("child"));
        assert_eq!(read(&root, "parent.md").as_deref(), Some("new parent"));
        assert!(!root.join(JOURNAL_NAME).exists());
    }

    #[test]
    fn apply_refuses_a_path_that_escapes_the_root() {
        // A staged op whose path climbs above the root (a hostile link target that
        // resolved to `../escape.md`) is refused before anything — journal or
        // document — is written.
        let root = tmp("escape-write");
        let mut cs = ChangeSet::new();
        cs.write("../escape.md", "should never land");
        let err = block_on(cs.apply(&StdFs, &root)).unwrap_err();
        assert!(
            matches!(err, Error::Escape(_)),
            "expected Escape, got {err:?}"
        );
        // Nothing was written, in or out of the root, and no journal remains.
        assert!(!root.parent().unwrap().join("escape.md").exists());
        assert!(!root.join(JOURNAL_NAME).exists());
    }

    #[test]
    fn apply_refuses_an_absolute_path() {
        // An absolute path would ignore the root under `root.join`; it escapes too.
        let root = tmp("escape-abs");
        let mut cs = ChangeSet::new();
        cs.write("/tmp/prov-abs-escape-should-not-exist.md", "nope");
        let err = block_on(cs.apply(&StdFs, &root)).unwrap_err();
        assert!(
            matches!(err, Error::Escape(_)),
            "expected Escape, got {err:?}"
        );
    }

    #[test]
    fn apply_refuses_to_clobber_a_stale_journal() {
        // A journal from a *previous* interrupted change is on disk. Applying a new
        // set must refuse rather than overwrite it — the old change would otherwise
        // be stranded with no record to recover from.
        let root = tmp("stale-journal");
        std::fs::write(root.join("doc.md"), "before").unwrap();
        // Pretend a prior change crashed mid-apply, leaving a valid journal.
        let prior = vec![FileOp::Write {
            path: "other.md".into(),
            bytes: b"prior".to_vec(),
        }];
        std::fs::write(
            root.join(JOURNAL_NAME),
            crate::journal::encode(&prior).unwrap(),
        )
        .unwrap();

        let mut cs = ChangeSet::new();
        cs.write("doc.md", "after");
        let err = block_on(cs.apply(&StdFs, &root)).unwrap_err();
        assert!(
            matches!(err, Error::StaleJournal(_)),
            "expected StaleJournal, got {err:?}"
        );
        // The new set did not land, and the old journal is untouched — recovery can
        // still complete the interrupted change.
        assert_eq!(read(&root, "doc.md").as_deref(), Some("before"));
        assert!(root.join(JOURNAL_NAME).exists());
    }

    #[test]
    fn apply_proceeds_once_the_stale_journal_is_recovered() {
        // After recovery clears the journal, the same set applies cleanly — the
        // refusal is about an *unrecovered* interruption, not a permanent lock.
        let root = tmp("stale-journal-cleared");
        std::fs::write(root.join("doc.md"), "before").unwrap();
        let prior = vec![FileOp::Write {
            path: "other.md".into(),
            bytes: b"prior".to_vec(),
        }];
        std::fs::write(
            root.join(JOURNAL_NAME),
            crate::journal::encode(&prior).unwrap(),
        )
        .unwrap();
        block_on(crate::journal::recover(&StdFs, &root)).unwrap();

        let mut cs = ChangeSet::new();
        cs.write("doc.md", "after");
        block_on(cs.apply(&StdFs, &root)).unwrap();
        assert_eq!(read(&root, "doc.md").as_deref(), Some("after"));
        assert_eq!(read(&root, "other.md").as_deref(), Some("prior"));
    }

    #[test]
    fn staged_ops_are_readable_without_applying() {
        // The dry-run view: a set describes writes without performing them.
        let root = tmp("dry-run");
        let mut cs = ChangeSet::new();
        cs.write("child.md", "child");
        cs.remove("old.md");
        assert_eq!(cs.len(), 2);
        assert_eq!(
            cs.ops().iter().map(FileOp::path).collect::<Vec<_>>(),
            [Path::new("child.md"), Path::new("old.md")]
        );
        assert_eq!(read(&root, "child.md"), None);
    }
}
