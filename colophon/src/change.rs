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
//! This is **error** atomicity, not **crash** atomicity. A failed write, a full
//! disk, a permission error, a rejected edit — none of these can leave a
//! half-linked workspace. A `kill -9` or a power cut still can: unwinding is
//! driven from memory, and memory dies with the process. Closing *that* window
//! needs a journal on disk and an `fsync` seam on [`Storage`] to order it
//! against, which is a separate piece of work. The distinction is worth keeping
//! sharp, because the cheap half covers almost every failure a user actually
//! meets and needs neither.
//!
//! Two smaller honesties, both deliberate:
//!
//! - **Directories are not unwound.** Applying a set creates any parent
//!   directory its writes need; a rollback leaves an empty one behind. An empty
//!   directory is litter, not a torn workspace — colophon's graph lives in the
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
}

impl FileOp {
    /// The path this op ultimately affects — the destination for a write or a
    /// rename, the victim for a remove. What a dry run lists.
    pub fn path(&self) -> &Path {
        match self {
            FileOp::Write { path, .. } => path,
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
        self.ops.push(FileOp::Write { path: path.into(), bytes: contents.into() });
        self
    }

    /// Stage a move from `from` to `to` (both workspace-relative).
    pub fn rename(&mut self, from: impl Into<PathBuf>, to: impl Into<PathBuf>) -> &mut Self {
        self.ops.push(FileOp::Rename { from: from.into(), to: to.into() });
        self
    }

    /// Stage the removal of `path` (workspace-relative).
    pub fn remove(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.ops.push(FileOp::Remove { path: path.into() });
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
    /// removes it. This is deliberately a lookup, not a filesystem overlay: it
    /// resolves the one hazard staging introduces and nothing more.
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

    /// Execute every staged op against `fs`, rooted at `root`, as one unit.
    ///
    /// On the first failure every op already applied is unwound in reverse and
    /// the original error is returned — the workspace is left as it was found.
    /// Should the *rollback* also fail (a disk that has gone away mid-unwind),
    /// [`Error::Torn`] reports both, because that is the one case where colophon
    /// genuinely cannot promise what is on disk.
    ///
    /// Takes `fs`/`root` rather than a [`crate::workspace::Workspace`] so a
    /// caller with neither — a bootstrap that must write two files before a
    /// workspace exists to write them through — can still land them together.
    pub async fn apply<FS: Storage>(&self, fs: &FS, root: &Path) -> Result<()> {
        let mut undo: Vec<Undo> = Vec::new();
        for op in &self.ops {
            let Err(cause) = exec(fs, root, op, &mut undo).await else {
                continue;
            };
            return Err(match unwind(fs, undo).await {
                Ok(()) => cause,
                Err(rollback) => Error::Torn {
                    cause: cause.to_string(),
                    rollback: rollback.to_string(),
                },
            });
        }
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

async fn exec<FS: Storage>(
    fs: &FS,
    root: &Path,
    op: &FileOp,
    undo: &mut Vec<Undo>,
) -> Result<()> {
    match op {
        FileOp::Write { path, bytes } => {
            let full = root.join(path);
            // Record the undo *before* writing: a write that fails partway
            // (a full disk) leaves a truncated file, and restoring the old
            // bytes over it is exactly the repair.
            match fs.read(&full).await {
                Ok(old) => undo.push(Undo::Restore { path: full.clone(), bytes: old }),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    undo.push(Undo::Delete { path: full.clone() });
                }
                Err(e) => return Err(e.into()),
            }
            ensure_parent(fs, &full).await?;
            fs.write(&full, bytes).await?;
        }
        FileOp::Rename { from, to } => {
            let (from_full, to_full) = (root.join(from), root.join(to));
            ensure_parent(fs, &to_full).await?;
            fs.rename(&from_full, &to_full).await?;
            undo.push(Undo::Rename { from: to_full, to: from_full });
        }
        FileOp::Remove { path } => {
            let full = root.join(path);
            let old = fs.read(&full).await?;
            fs.remove_file(&full).await?;
            undo.push(Undo::Restore { path: full, bytes: old });
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

/// Create `full`'s parent directory if it is missing. Unconditional (rather than
/// staged as its own op) because a directory is not part of the document graph:
/// it is an artifact of *where* a write lands, so it belongs to the write.
async fn ensure_parent<FS: Storage>(fs: &FS, full: &Path) -> Result<()> {
    if let Some(dir) = full.parent() {
        fs.create_dir_all(dir).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::block_on;
    use crate::fs::{FailAtWrite, StdFs};

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("colophon-change-{name}"));
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
        // `Torn` means "colophon cannot say what is on disk" — it must be reserved
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
