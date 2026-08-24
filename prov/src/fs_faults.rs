//! Fault-injecting and observing [`Storage`] backends, for tests.
//!
//! Each one is a real local filesystem in every respect but the single fault it
//! injects (or the single thing it counts), so an operation over it takes the
//! *same* code path a production `StdFs` workspace takes right up to the moment
//! the fault lands. That is what makes them worth having: a change set unwinding
//! under `FailAtWrite` exercises the real staging/rename protocol, not a mock of
//! it.
//!
//! These live in `prov` rather than beside [`StdFs`] in `prov-graph` because
//! every one of them fakes a *write*, and `prov-graph`'s reason to exist is that
//! traversal needs no such thing. The read-only crate ships no backend that can
//! fail a write because it ships no way to attempt one.

#![cfg(test)]

use std::io;
use std::path::{Path, PathBuf};

use prov_graph::fs::{DirEntry, Metadata, ReadStorage, StdFs};
use prov_store::fs::{Capabilities, Durability, Storage, SyncGuarantee};

/// [`Storage`] over `std::fs` that fails the *n*th write, for testing that a
/// [`ChangeSet`](crate::change::ChangeSet) unwinds.
///
/// Every other method delegates to [`StdFs`], so a workspace over this backend
/// behaves exactly like a real one until the chosen write, then reports the kind
/// of failure a full disk or a revoked permission would.
#[derive(Debug)]
pub(crate) struct FailAtWrite {
    writes: std::cell::Cell<usize>,
    fail_at: usize,
}

impl FailAtWrite {
    /// Fail the `fail_at`th write (0-indexed); let every other one through.
    pub(crate) fn nth(fail_at: usize) -> Self {
        Self {
            writes: std::cell::Cell::new(0),
            fail_at,
        }
    }

    /// Never fail — a counting [`StdFs`]. Pair with
    /// [`attempted`](Self::attempted) to learn how many writes an operation
    /// makes, so a test can then fail each of them in turn.
    pub(crate) fn never() -> Self {
        Self::nth(usize::MAX)
    }

    /// How many writes have been attempted.
    ///
    /// Only meaningful after a *successful* run: once a write fails, the
    /// rollback's own writes go through this same backend and are counted too.
    pub(crate) fn attempted(&self) -> usize {
        self.writes.get()
    }
}

/// The read half of a test backend that fakes only writes: forwarded verbatim to
/// [`StdFs`]. Three of the backends below differ from a real local filesystem
/// exclusively in *what they refuse to write*, so spelling their reads out four
/// times would be four copies of the same nine lines with nothing to say.
macro_rules! reads_like_stdfs {
    ($ty:ty) => {
        impl ReadStorage for $ty {
            async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
                StdFs.read(path).await
            }
            async fn read_to_string(&self, path: &Path) -> io::Result<String> {
                StdFs.read_to_string(path).await
            }
            async fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
                StdFs.read_dir(path).await
            }
            async fn metadata(&self, path: &Path) -> io::Result<Metadata> {
                StdFs.metadata(path).await
            }
        }
    };
}

reads_like_stdfs!(FailAtWrite);

impl Storage for FailAtWrite {
    async fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        // The write-ahead journal's own writes are infrastructure, not document
        // writes: leaving them uncounted keeps `nth` addressing the *document*
        // write a test means to fail, the way a real full disk fills mid-content
        // rather than mid-journal. A journal-write failure has its own test.
        if crate::journal::is_journal_path(path) {
            return StdFs.write(path, contents).await;
        }
        let n = self.writes.get();
        self.writes.set(n + 1);
        if n == self.fail_at {
            return Err(io::Error::other("disk full (test)"));
        }
        StdFs.write(path, contents).await
    }
    async fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        StdFs.create_dir_all(path).await
    }
    async fn remove_file(&self, path: &Path) -> io::Result<()> {
        StdFs.remove_file(path).await
    }
    async fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        StdFs.remove_dir_all(path).await
    }
    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        StdFs.rename(from, to).await
    }
    async fn copy_permissions(&self, from: &Path, to: &Path) -> io::Result<()> {
        StdFs.copy_permissions(from, to).await
    }
    // A faithful local filesystem in every respect but the chosen failing write,
    // so a change set applied over it exercises the *real* atomic-write protocol
    // (temp, sync, rename) — the injected failure lands on the staging write and
    // the target is never touched, exactly as a full disk mid-write would behave.
    fn capabilities(&self) -> Capabilities {
        Capabilities::LOCAL_FS
    }
    async fn sync(&self, path: &Path, need: Durability) -> io::Result<()> {
        StdFs.sync(path, need).await
    }
}

/// A [`Storage`] over `std::fs` that records the ordered sequence of mutating
/// operations it performs, for asserting a *protocol* — the one durability
/// guarantee a unit test cannot check by actually crashing. It reports whatever
/// [`Capabilities`] it is built with, and never overrides
/// [`write_atomic`](Storage::write_atomic), so a test observes the default
/// protocol's own internal ordering (write temp → sync temp → rename → sync the
/// target's directory) rather than a substitute.
#[derive(Debug)]
pub(crate) struct RecordingFs {
    log: std::cell::RefCell<Vec<FsEvent>>,
    caps: Capabilities,
}

/// A real filesystem that counts the documents it was asked to read, per path.
///
/// The observable the read memo exists to move: a pass that does not read a
/// file is the whole point, and "did not read it" is not visible in any result
/// the pass returns. Delegates everything to [`StdFs`], so timestamps, atomic
/// writes and durability all behave like the real thing.
///
/// Documents only: nothing here tallies raw byte reads, because no pass of
/// prov's spends them wholesale any more.
#[derive(Clone, Default, Debug)]
pub(crate) struct CountingFs {
    /// `read_to_string` — documents. What the traversal passes spend.
    docs: std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<PathBuf, usize>>>,
}

impl CountingFs {
    /// How many times the document at workspace-relative `rel` was read.
    pub(crate) fn doc_reads(&self, dir: &Path, rel: &str) -> usize {
        count(&self.docs, &dir.join(rel))
    }
}

fn count(
    counter: &std::sync::Mutex<std::collections::BTreeMap<PathBuf, usize>>,
    path: &Path,
) -> usize {
    counter.lock().unwrap().get(path).copied().unwrap_or(0)
}

fn tally(counter: &std::sync::Mutex<std::collections::BTreeMap<PathBuf, usize>>, path: &Path) {
    *counter
        .lock()
        .unwrap()
        .entry(path.to_path_buf())
        .or_insert(0) += 1;
}

impl ReadStorage for CountingFs {
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        StdFs.read(path).await
    }
    async fn read_to_string(&self, path: &Path) -> io::Result<String> {
        tally(&self.docs, path);
        StdFs.read_to_string(path).await
    }
    async fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        StdFs.read_dir(path).await
    }
    async fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        StdFs.metadata(path).await
    }
}

impl Storage for CountingFs {
    async fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        StdFs.write(path, contents).await
    }
    async fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        StdFs.create_dir_all(path).await
    }
    async fn remove_file(&self, path: &Path) -> io::Result<()> {
        StdFs.remove_file(path).await
    }
    async fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        StdFs.remove_dir_all(path).await
    }
    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        StdFs.rename(from, to).await
    }
    async fn copy_permissions(&self, from: &Path, to: &Path) -> io::Result<()> {
        StdFs.copy_permissions(from, to).await
    }
    fn capabilities(&self) -> Capabilities {
        StdFs.capabilities()
    }
    async fn sync(&self, path: &Path, need: Durability) -> io::Result<()> {
        StdFs.sync(path, need).await
    }
}

/// One mutating operation [`RecordingFs`] observed, in order. Reads are not
/// recorded — the protocol under test is about the sequence of *durability*
/// steps, and a read changes nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FsEvent {
    Write(PathBuf),
    /// Carries the strength asked for, not just the path: which of the protocol's
    /// two flushes merely orders and which is meant to outlive a power cut is
    /// itself part of the protocol under test.
    Sync(PathBuf, Durability),
    Rename(PathBuf, PathBuf),
    Remove(PathBuf),
}

impl RecordingFs {
    /// A recorder that reports the local-filesystem guarantees, so `write_atomic`
    /// runs its full atomic protocol.
    pub(crate) fn local() -> Self {
        Self {
            log: std::cell::RefCell::new(Vec::new()),
            caps: Capabilities::LOCAL_FS,
        }
    }

    /// A recorder that reports the given capabilities — used to observe the
    /// `atomic_replace: false` fallback taking the plain-write path.
    pub(crate) fn with_caps(caps: Capabilities) -> Self {
        Self {
            log: std::cell::RefCell::new(Vec::new()),
            caps,
        }
    }

    /// The operations recorded so far, in the order they happened.
    pub(crate) fn events(&self) -> Vec<FsEvent> {
        self.log.borrow().clone()
    }
}

reads_like_stdfs!(RecordingFs);

impl Storage for RecordingFs {
    async fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        self.log
            .borrow_mut()
            .push(FsEvent::Write(path.to_path_buf()));
        StdFs.write(path, contents).await
    }
    async fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        StdFs.create_dir_all(path).await
    }
    async fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.log
            .borrow_mut()
            .push(FsEvent::Remove(path.to_path_buf()));
        StdFs.remove_file(path).await
    }
    async fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        StdFs.remove_dir_all(path).await
    }
    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.log
            .borrow_mut()
            .push(FsEvent::Rename(from.to_path_buf(), to.to_path_buf()));
        StdFs.rename(from, to).await
    }
    // Not logged: `FsEvent` records the *durability* steps whose ordering is the
    // protocol under test, and carrying a mode across is not one of them.
    // Delegated rather than defaulted so the staging file this recorder leaves
    // on the real disk has the permissions a real `write_atomic` would give it.
    async fn copy_permissions(&self, from: &Path, to: &Path) -> io::Result<()> {
        StdFs.copy_permissions(from, to).await
    }
    fn capabilities(&self) -> Capabilities {
        self.caps
    }
    async fn sync(&self, path: &Path, need: Durability) -> io::Result<()> {
        self.log
            .borrow_mut()
            .push(FsEvent::Sync(path.to_path_buf(), need));
        StdFs.sync(path, need).await
    }
}

/// A local filesystem whose `rename` always fails — the fault an atomic write
/// must survive without ever touching the target. Every other operation is real,
/// so the staging write genuinely happens and the test can prove it was cleaned
/// up and the target left untouched.
#[derive(Debug)]
pub(crate) struct FailingRename;

reads_like_stdfs!(FailingRename);

impl Storage for FailingRename {
    async fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        StdFs.write(path, contents).await
    }
    async fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        StdFs.create_dir_all(path).await
    }
    async fn remove_file(&self, path: &Path) -> io::Result<()> {
        StdFs.remove_file(path).await
    }
    async fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        StdFs.remove_dir_all(path).await
    }
    async fn rename(&self, _from: &Path, _to: &Path) -> io::Result<()> {
        Err(io::Error::other("rename failed (test)"))
    }
    async fn copy_permissions(&self, from: &Path, to: &Path) -> io::Result<()> {
        StdFs.copy_permissions(from, to).await
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::LOCAL_FS
    }
    async fn sync(&self, path: &Path, need: Durability) -> io::Result<()> {
        StdFs.sync(path, need).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prov_graph::exec::block_on;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prov-fsfault-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_torn_atomic_write_leaves_the_target_untouched_and_no_litter() {
        // The atomicity promise stated as a failure: a write that dies at the
        // rename — the moment nearest the atomic instant — must leave the target
        // exactly as it was, and clean up its staging file.
        let root = tmp("atomic-fail");
        std::fs::write(root.join("doc.md"), "old").unwrap();
        let target = root.join("doc.md");
        let temp = root.join(".doc.md.prov-tmp");

        let err = block_on(FailingRename.write_atomic(&target, b"new")).unwrap_err();

        assert!(err.to_string().contains("rename failed"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "old",
            "target was touched"
        );
        assert!(!temp.exists(), "the staging file was left behind");
    }

    #[test]
    fn the_default_capability_promises_nothing() {
        // A backend that does not override `capabilities` is assumed to guarantee
        // nothing, so nothing above it can rely on a promise it never made. Proven
        // through a backend (`RecordingFs::with_caps`) that reports the default.
        let bare = RecordingFs::with_caps(Capabilities::NONE);
        assert_eq!(bare.capabilities(), Capabilities::NONE);
        // NONE is every field at its pessimistic value — spelled out so a future
        // edit that quietly flips one has to change this line too.
        assert_eq!(
            Capabilities::NONE,
            Capabilities {
                atomic_replace: false,
                sync_guarantee: SyncGuarantee::None,
                native_transactions: false
            }
        );
    }

    #[test]
    fn write_atomic_follows_the_durable_replace_protocol() {
        // The one durability guarantee a unit test cannot check by crashing, so it
        // checks the *protocol* that makes a crash survivable instead: the new
        // bytes are written and flushed to a *temporary* file, and only then
        // renamed over the target — so a crash at any instant leaves the target
        // wholly old or wholly new, never spliced — and the *directory* is
        // flushed last, which is what makes the rename itself durable.
        //
        // The two flushes and their two strengths are the assertion. A staging
        // flush weaker than `Ordered` would let the rename be seen before the
        // bytes it publishes; a third flush, of the target under its final name,
        // would flush an inode nothing has written to since — the waste this
        // protocol was tightened to drop, and the reason the exact event list is
        // pinned rather than merely searched for the steps it ought to contain.
        let root = tmp("protocol");
        std::fs::write(root.join("doc.md"), "old").unwrap();
        let fs = RecordingFs::local();
        let target = root.join("doc.md");
        let temp = root.join(".doc.md.prov-tmp");

        block_on(fs.write_atomic(&target, b"new")).unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
        assert!(!temp.exists(), "the staging file must be gone");
        assert_eq!(
            fs.events(),
            vec![
                FsEvent::Write(temp.clone()),
                FsEvent::Sync(temp.clone(), Durability::Ordered),
                FsEvent::Rename(temp.clone(), target.clone()),
                FsEvent::Sync(root.clone(), Durability::Durable),
            ],
            "the atomic-replace protocol ran out of order"
        );
    }

    #[test]
    #[cfg(unix)]
    fn write_atomic_replaces_contents_without_widening_permissions() {
        use std::os::unix::fs::PermissionsExt;

        // Replacing a document's contents must not republish it to a wider
        // audience. The staging sibling is a file this process just created, so
        // it is born with the umask's default mode; without carrying the
        // target's mode across, the rename would publish *that* under the
        // target's name and quietly turn a `chmod 600` private entry into a
        // world-readable one. Nothing in the atomic protocol would notice, and
        // nothing the user does afterwards would restore it.
        let root = tmp("atomic-perms");
        let target = root.join("private.md");
        std::fs::write(&target, "old").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();

        let fs = RecordingFs::local();
        block_on(fs.write_atomic(&target, b"new")).unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600,
            "replacing the contents widened the document's permissions"
        );
    }

    #[test]
    fn a_backend_without_a_permission_model_is_unbothered_by_the_step() {
        // `copy_permissions` defaults to a no-op, so a backend with nothing to
        // preserve — `InMemoryFs`, OPFS — runs the same protocol untouched. The
        // step must not become a precondition for writing at all.
        let fs = prov_store::fs::InMemoryFs::default();
        let target = Path::new("/w/doc.md");
        block_on(fs.write_atomic(target, b"hello")).unwrap();
        assert_eq!(block_on(fs.read(target)).unwrap(), b"hello");
    }

    #[test]
    fn write_atomic_creates_a_new_file_without_disturbing_the_directory() {
        // The target need not already exist — replacing "nothing" with the whole
        // file is still atomic, and still routes through the staging sibling.
        let root = tmp("atomic-create");
        let fs = RecordingFs::local();
        let target = root.join("fresh.md");
        let temp = root.join(".fresh.md.prov-tmp");

        block_on(fs.write_atomic(&target, b"hello")).unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
        assert!(!temp.exists());
        assert_eq!(fs.events().first(), Some(&FsEvent::Write(temp)));
    }

    #[test]
    fn a_non_atomic_backend_writes_straight_through_without_claiming_atomicity() {
        // With `atomic_replace: false` there is no rename to lean on, so
        // `write_atomic` degrades to a plain durable write — the bytes still land,
        // it simply does not route through a staging sibling and makes no
        // crash-atomicity claim. Proven by the absence of a Rename in the log.
        let root = tmp("fallback");
        let fs = RecordingFs::with_caps(Capabilities::NONE);
        let target = root.join("doc.md");

        block_on(fs.write_atomic(&target, b"hello")).unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
        assert_eq!(
            fs.events(),
            vec![
                FsEvent::Write(target.clone()),
                FsEvent::Sync(target, Durability::Durable),
                // No rename to fold it into, so the entry naming a *newly created*
                // file needs a flush of its own — the one case where the caller,
                // not `sync`, has to ask for the directory.
                FsEvent::Sync(root.clone(), Durability::Durable),
            ],
            "the fallback must write the target directly, with no staging rename"
        );
        assert!(!root.join(".doc.md.prov-tmp").exists());
    }
}
