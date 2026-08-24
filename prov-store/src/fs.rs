//! The write half of prov's filesystem port.
//!
//! [`prov_graph::fs`] declares [`ReadStorage`] — everything the traversal core
//! needs, and nothing that can change a byte. This module declares the other
//! half: [`Storage`], the durability vocabulary a backend answers with
//! ([`Capabilities`], [`Durability`], [`SyncGuarantee`]), and the
//! write-temp-then-rename protocol that makes a replacement crash-atomic.
//!
//! The method set mirrors [`std::fs`] names exactly, so an adapter is
//! mechanical to write. A backend implements the read surface on
//! [`ReadStorage`] over in `prov-graph` and the write/mutate/durability surface
//! here.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use prov_graph::fs::ReadStorage;

pub mod memory;

pub use memory::InMemoryFs;
pub use prov_graph::fs::StdFs;

/// An async filesystem backend prov can drive — [`ReadStorage`] plus everything
/// that changes bytes on disk.
///
/// Each method mirrors the [`std::fs`] function of the same name. Backends
/// implement the write/mutate/durability surface here and the read surface on
/// [`ReadStorage`].
pub trait Storage: ReadStorage {
    // ---- write ----

    /// Write a file, replacing it if it already exists. Mirrors
    /// [`std::fs::write`].
    fn write(&self, path: &Path, contents: &[u8]) -> impl Future<Output = io::Result<()>>;

    /// Create a directory and all missing parents. Mirrors
    /// [`std::fs::create_dir_all`].
    fn create_dir_all(&self, path: &Path) -> impl Future<Output = io::Result<()>>;

    // ---- mutate ----

    /// Remove a regular file. Mirrors [`std::fs::remove_file`].
    fn remove_file(&self, path: &Path) -> impl Future<Output = io::Result<()>>;

    /// Recursively remove a directory and its contents. Mirrors
    /// [`std::fs::remove_dir_all`].
    fn remove_dir_all(&self, path: &Path) -> impl Future<Output = io::Result<()>>;

    /// Rename or move a file or directory. Mirrors [`std::fs::rename`].
    fn rename(&self, from: &Path, to: &Path) -> impl Future<Output = io::Result<()>>;

    /// Give `to` the same access permissions `from` has. A `from` that does not
    /// exist is not an error — there is no prior state to carry over, so the
    /// call has nothing to do.
    ///
    /// This exists for [`write_atomic`](Storage::write_atomic), which publishes
    /// its bytes by renaming a freshly-created sibling over the target. A new
    /// file is born with the backend's default permissions, and a rename carries
    /// those onto the name it replaces — so without this step, replacing a
    /// document the user had deliberately restricted (`chmod 600` on a private
    /// journal entry) silently widens it to whatever the umask allows. A
    /// content replacement must not be a permission change.
    ///
    /// What this does *not* close is the window before it: the sibling holds the
    /// new contents under default permissions from the moment it is written
    /// until this call narrows it. Shutting that window means creating the file
    /// with the final mode already on it, which is not something
    /// [`write`](Storage::write) — a `std::fs::write` mirror — can express. The
    /// sibling lives in the target's own directory throughout, so whatever gates
    /// access to the document gates access to it too.
    ///
    /// The default is a no-op, which is the *correct* behavior for a backend
    /// with no permission model at all — [`InMemoryFs`], OPFS, IndexedDB. There
    /// is nothing there to preserve, and nothing is lost by not preserving it.
    fn copy_permissions(&self, from: &Path, to: &Path) -> impl Future<Output = io::Result<()>> {
        async move {
            let _ = (from, to);
            Ok(())
        }
    }

    // ---- durability ----
    //
    // prov spans backends with very different crash guarantees — `std::fs`
    // (atomic rename and fsync on every major OS), OPFS (a flush primitive but a
    // weak rename), IndexedDB (its own multi-object transactions). Rather than
    // assume the strongest of these and silently lie on the weakest, the crash-
    // safety machinery *asks* what a backend can promise and adapts. These three
    // members are defaulted to the pessimistic answer, so a backend gains a
    // guarantee only by explicitly claiming it.

    /// What durability guarantees this backend can make. Defaults to
    /// [`Capabilities::NONE`] — a backend promises a guarantee only by saying so,
    /// so an adapter that forgets to override this degrades to the most defensive
    /// path rather than to a false promise.
    fn capabilities(&self) -> Capabilities {
        Capabilities::NONE
    }

    /// Flush `path` — and nothing else — to the strength `need` asks for.
    ///
    /// `path` names *one* object, and only that object is flushed. To make a
    /// directory entry durable (the naming half of a create or a rename), sync
    /// the directory itself: on a POSIX filesystem a directory is a thing that
    /// can be opened and fsynced, and prov's own
    /// [`write_atomic`](Storage::write_atomic) does exactly that after its
    /// rename. Folding the parent into every call instead would flush twice as
    /// much as any single step needs, and would leave the caller unable to say
    /// which of the two it actually meant.
    ///
    /// `need` is the *weakest* guarantee that is still correct at the call site,
    /// not a wish. [`Durability::Ordered`] asks only that everything written to
    /// `path` before this call land before anything written after it — enough to
    /// stop a rename overtaking the bytes it publishes, and on some platforms far
    /// cheaper than the real thing. [`Durability::Durable`] asks that the bytes
    /// survive power loss. A backend may always answer with something stronger
    /// than it was asked for; it may never answer with something weaker.
    ///
    /// The default is a no-op, which is the *correct* behavior for any backend
    /// whose [`capabilities`](Storage::capabilities) report
    /// [`SyncGuarantee::None`]: it cannot make the promise, so it must not
    /// pretend to. A backend that can flush must both override this and report
    /// the strongest request it genuinely honors — the two always travel
    /// together, and [`SyncGuarantee::satisfies`] is how a caller asks.
    fn sync(&self, path: &Path, need: Durability) -> impl Future<Output = io::Result<()>> {
        async move {
            let _ = (path, need);
            Ok(())
        }
    }

    /// Replace `path`'s contents with `contents` atomically and durably: no
    /// observer — concurrent reader or post-crash survivor — ever sees a splice
    /// of old and new bytes, and once this returns the new contents outlive a
    /// power loss.
    ///
    /// The default composes the primitives into the standard protocol, whenever
    /// [`capabilities`](Storage::capabilities) report `atomic_replace`:
    ///
    /// 1. write the bytes to a temporary sibling;
    /// 2. [`sync`](Storage::sync) that sibling [`Ordered`](Durability::Ordered),
    ///    so the rename cannot be reordered ahead of the bytes it publishes;
    /// 3. [`copy_permissions`](Storage::copy_permissions) from the target onto
    ///    that sibling, so the replacement carries the target's access
    ///    permissions rather than a fresh file's defaults;
    /// 4. [`rename`](Storage::rename) it over the target — *this* is the atomic
    ///    instant;
    /// 5. `sync` the target's **parent directory** [`Durable`](Durability::Durable),
    ///    which is what carries the rename itself through a power cut.
    ///
    /// Steps 2 and 3 are in that order because a backend may implement `sync` by
    /// opening the path, and a mode faithfully copied from the target can be one
    /// that forbids opening it to read — `0o200` is replaceable but not readable.
    /// The cost is that the mode change lands after the flush and so is not
    /// itself durable: a crash in that window can leave the new contents under
    /// the *default* permissions. That is precisely the outcome every write had
    /// before step 3 existed, so the window is a smaller bad case, never a new
    /// one.
    ///
    /// Two flushes, and each one is load-bearing. Neither of the two this
    /// protocol conspicuously does *not* do would buy anything. The bytes are
    /// never flushed under their final name, because a rename does not move an
    /// inode: the file the target now names is the very one step 2 flushed, and
    /// nothing has been written to it since. The sibling's own directory entry is
    /// never flushed either, because nobody is owed a temporary that survives a
    /// crash — only the directory state *after* the rename is worth a barrier.
    ///
    /// A backend that cannot rename atomically falls back to a plain durable
    /// write, which is *not* crash-atomic; a caller that needs the guarantee
    /// consults `capabilities` and leans on the journal instead of pretending
    /// this call gave it. A backend with a better native path — a transactional
    /// store — overrides this method wholesale.
    ///
    /// The temporary is removed on any failure, so a torn attempt leaves the
    /// target exactly as it was and no litter behind. It is a dotted sibling in
    /// the target's own directory, so the follow-up rename stays within one
    /// filesystem (a cross-device rename is neither atomic nor, often, even
    /// permitted).
    fn write_atomic(&self, path: &Path, contents: &[u8]) -> impl Future<Output = io::Result<()>> {
        async move {
            if !self.capabilities().atomic_replace {
                // No atomic rename to lean on: the honest best effort is a plain
                // durable write. Not crash-atomic — and the caller was told so by
                // `capabilities`, so this is a documented degrade, not a lie.
                // Both the bytes and, if this call created the file, the entry
                // naming them have to be flushed; there is no rename here to fold
                // the second into.
                self.write(path, contents).await?;
                self.sync(path, Durability::Durable).await?;
                return match parent_dir(path) {
                    Some(dir) => self.sync(dir, Durability::Durable).await,
                    None => Ok(()),
                };
            }
            let tmp = temp_sibling(path);
            // Any failure past this point must not leave the staging file behind,
            // and must never have touched the target — hence the whole dance
            // happens on `tmp` and only the rename names `path`.
            let staged = async {
                self.write(&tmp, contents).await?;
                self.sync(&tmp, Durability::Ordered).await?;
                // The sibling was just created, so it carries default
                // permissions rather than the target's. Carry the target's over
                // before the rename publishes them — a replacement changes
                // contents, never who may read them. A target that does not
                // exist yet has nothing to carry, and this is a no-op.
                //
                // After the flush, not before: a backend may well implement
                // `sync` by opening the path (`StdFs` does), and a target whose
                // mode this faithfully copies can be one that forbids exactly
                // that — a write-only `0o200` document is replaceable but not
                // openable for reading. Narrowing the sibling first would make
                // its own flush fail.
                self.copy_permissions(path, &tmp).await?;
                self.rename(&tmp, path).await
            }
            .await;
            match staged {
                // The bytes are already flushed and the rename has happened, so
                // the directory entry is the last thing standing between this
                // write and a power cut.
                Ok(()) => match parent_dir(path) {
                    Some(dir) => self.sync(dir, Durability::Durable).await,
                    // A bare relative filename, whose directory is the process's
                    // current one — not a path prov holds, nor one it owns.
                    None => Ok(()),
                },
                Err(e) => {
                    // Best-effort cleanup: if even this fails the target is still
                    // untouched, so the atomicity promise holds regardless — the
                    // worst case is one stray dotfile, not a torn document.
                    let _ = self.remove_file(&tmp).await;
                    Err(e)
                }
            }
        }
    }
}

impl<S: Storage + ?Sized> Storage for &S {
    async fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        (**self).write(path, contents).await
    }

    async fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        (**self).create_dir_all(path).await
    }

    async fn remove_file(&self, path: &Path) -> io::Result<()> {
        (**self).remove_file(path).await
    }

    async fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        (**self).remove_dir_all(path).await
    }

    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        (**self).rename(from, to).await
    }

    async fn copy_permissions(&self, from: &Path, to: &Path) -> io::Result<()> {
        (**self).copy_permissions(from, to).await
    }

    fn capabilities(&self) -> Capabilities {
        (**self).capabilities()
    }

    async fn sync(&self, path: &Path, need: Durability) -> io::Result<()> {
        (**self).sync(path, need).await
    }

    async fn write_atomic(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        (**self).write_atomic(path, contents).await
    }
}

impl<S: Storage + ?Sized> Storage for Arc<S> {
    async fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        (**self).write(path, contents).await
    }

    async fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        (**self).create_dir_all(path).await
    }

    async fn remove_file(&self, path: &Path) -> io::Result<()> {
        (**self).remove_file(path).await
    }

    async fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        (**self).remove_dir_all(path).await
    }

    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        (**self).rename(from, to).await
    }

    async fn copy_permissions(&self, from: &Path, to: &Path) -> io::Result<()> {
        (**self).copy_permissions(from, to).await
    }

    fn capabilities(&self) -> Capabilities {
        (**self).capabilities()
    }

    async fn sync(&self, path: &Path, need: Durability) -> io::Result<()> {
        (**self).sync(path, need).await
    }

    async fn write_atomic(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        (**self).write_atomic(path, contents).await
    }
}

/// The durability guarantees a [`Storage`] backend can make — declared by the
/// backend through [`Storage::capabilities`], honored by prov's crash-safety
/// machinery.
///
/// The point of naming these explicitly is that prov must run correctly over
/// backends that keep very different promises. Rather than assume a guarantee and
/// corrupt data on the backend that cannot keep it, prov reads the
/// capabilities and picks the strongest *protocol the backend actually supports*:
/// a filesystem gets atomic-rename writes and a journal; a transactional store is
/// handed the whole change set to commit itself; a backend that can promise
/// neither still works, it simply cannot claim a write survives a crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// The backend can replace an existing file's contents in one indivisible
    /// step, so no crash exposes a half-written file — an observer sees the whole
    /// old contents or the whole new. On a filesystem this is realized by
    /// [`Storage::write_atomic`]'s write-temp-then-`rename`; a backend may
    /// instead be atomic by nature.
    pub atomic_replace: bool,

    /// How strong the backend's [`Storage::sync`] is: whether it can flush at
    /// all, and if so whether a flush merely orders writes or carries them
    /// through a power cut. `fsync` on `std::fs`, `FileSystemSyncAccessHandle
    /// .flush()` on OPFS, the implicit durability of a committed IndexedDB
    /// transaction.
    pub sync_guarantee: SyncGuarantee,

    /// The backend commits changes to *many* objects as one indivisible unit, so
    /// prov's own write-ahead journal would be redundant and it should defer
    /// to the backend instead. True for IndexedDB; false for a plain filesystem,
    /// where multi-file atomicity is prov's job to provide.
    pub native_transactions: bool,
}

impl Capabilities {
    /// Promises nothing — the safe assumption for an unknown backend, and the
    /// [`Storage::capabilities`] default. Every field is the pessimistic value,
    /// so code that checks a capability before relying on it takes the most
    /// defensive branch unless a backend has explicitly earned a lighter one.
    pub const NONE: Self = Self {
        atomic_replace: false,
        sync_guarantee: SyncGuarantee::None,
        native_transactions: false,
    };

    /// A conventional local filesystem: atomic replacement by rename and durable
    /// fsync, but no native multi-object transaction (that is the journal's job).
    /// What [`StdFs`] reports on every platform prov targets.
    pub const LOCAL_FS: Self = Self {
        atomic_replace: true,
        sync_guarantee: SyncGuarantee::Durable,
        native_transactions: false,
    };

    /// An in-process, memory-only store ([`InMemoryFs`]): every mutation takes
    /// the backend's single lock for its whole duration, so one write already
    /// swaps old bytes for new as one indivisible step — no separate
    /// temp-then-rename dance is needed for `atomic_replace` to be true. But
    /// nothing here is backed by anything other than process memory, so its
    /// `sync_guarantee` is [`SyncGuarantee::None`]: there is nothing to flush,
    /// and the entire store evaporates the instant the process exits — it cannot
    /// even promise ordering against a crash it will not survive.
    /// `native_transactions`
    /// is false too — the lock makes each *single* call atomic, not a batch of
    /// several calls committed together, so a multi-file change set still
    /// needs prov's own journal over this backend exactly as it would over a
    /// real filesystem.
    pub const IN_MEMORY: Self = Self {
        atomic_replace: true,
        sync_guarantee: SyncGuarantee::None,
        native_transactions: false,
    };
}

/// What a caller needs from one [`Storage::sync`] call — the *weakest* guarantee
/// that is still correct at that point, so that a backend able to serve it
/// cheaply is free to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Durability {
    /// Everything written to the path before this call must land before anything
    /// written after it. It says nothing about *when*: a crash may still lose
    /// the lot, only never a suffix without its prefix. This is all
    /// [`Storage::write_atomic`] needs from its staging flush — the rename must
    /// not be seen before the bytes it publishes — and on Apple platforms it is
    /// the difference between a barrier and draining the drive's write cache.
    Ordered,
    /// Once the call returns, the bytes survive power loss.
    Durable,
}

/// How strong a backend's [`Storage::sync`] actually is — the standing answer to
/// a [`Durability`] request, declared once in [`Capabilities`] rather than
/// discovered per call.
///
/// Deliberately three-valued rather than the "can this backend flush?" boolean
/// it replaces, because that question has a common and useful middle answer it
/// could not express: a backend that orders writes against each other without
/// paying for a device-wide cache drain. Offered only `true` and `false`, such a
/// backend has to either overstate — claiming a durability it does not deliver —
/// or understate, claiming it cannot flush at all when ordering is precisely
/// what [`Storage::write_atomic`] asks it for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SyncGuarantee {
    /// `sync` does nothing: an in-memory store, or a port with no flush
    /// primitive under it to call.
    None,
    /// `sync` orders writes against each other, but does not promise any of them
    /// outlives a power cut.
    Ordered,
    /// `sync` flushes through to durable storage.
    Durable,
}

impl SyncGuarantee {
    /// Whether a backend making this guarantee can honor `need`.
    pub const fn satisfies(self, need: Durability) -> bool {
        match need {
            Durability::Ordered => !matches!(self, SyncGuarantee::None),
            Durability::Durable => matches!(self, SyncGuarantee::Durable),
        }
    }
}

/// The temporary sibling [`Storage::write_atomic`]'s default protocol stages a
/// write through before renaming it into place. A dotted, suffixed name in the
/// target's own directory: dotted and suffixed so it reads as plainly prov's
/// and will not collide with a real document, and a *sibling* so the rename that
/// follows never crosses a filesystem boundary.
/// The directory holding `path`, when there is one to name. `Path::parent`
/// answers `Some("")` for a bare relative filename like `index.md` — the
/// process's current directory, which prov neither holds a path to nor owns —
/// and that empty path is not something a backend can open, so it is folded in
/// with "no parent" here rather than at each call site.
fn parent_dir(path: &Path) -> Option<&Path> {
    path.parent().filter(|p| !p.as_os_str().is_empty())
}

fn temp_sibling(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("document");
    path.with_file_name(format!(".{name}.prov-tmp"))
}

impl Storage for StdFs {
    async fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        std::fs::write(path, contents)
    }

    async fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        std::fs::create_dir_all(path)
    }

    async fn remove_file(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_file(path)
    }

    async fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_dir_all(path)
    }

    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }

    async fn copy_permissions(&self, from: &Path, to: &Path) -> io::Result<()> {
        let perms = match std::fs::metadata(from) {
            Ok(meta) => meta.permissions(),
            // Nothing to carry over: `write_atomic` is creating `from` rather
            // than replacing it, so the new file's default permissions are the
            // right ones and there is no prior state to lose.
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        // Deliberately best-effort. On a filesystem with no permission model —
        // exFAT or FAT32 on a USB stick, some FUSE mounts — `chmod` refuses
        // outright, but every file there already reports the same mount-wide
        // mode, so there was never a permission to preserve and failing the
        // whole document write over it would be absurd. Where modes *are* real,
        // this is a chmod on a file this process created moments ago and owns,
        // which does not fail for any reason a caller could act on.
        let _ = std::fs::set_permissions(to, perms);
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        // Every OS prov targets gives an atomic same-filesystem rename and an
        // fsync. `std::fs::rename` replaces the destination on all of them —
        // POSIX by definition, Windows via `MoveFileEx(MOVEFILE_REPLACE_EXISTING)`
        // — so the write-temp-then-rename protocol in the default `write_atomic`
        // is genuinely atomic here.
        Capabilities::LOCAL_FS
    }

    async fn sync(&self, path: &Path, need: Durability) -> io::Result<()> {
        // `fsync` is the only flush in the standard library, and it is the strong
        // one — so both requests are answered with it. Answering `Ordered` more
        // cheaply means a platform-specific primitive (`F_BARRIERFSYNC` on Apple,
        // `sync_file_range` on Linux) and the `libc` dependency that comes with
        // it; a port that wants the cheaper answer can wrap this one and say so
        // in its own `capabilities`, which is exactly what `SyncGuarantee` is for.
        let _ = need;
        sync_path(path)
    }
}

/// Flush exactly `path` — file or directory — so a preceding write or rename to
/// it is durable. The one place a real OS difference lives, quarantined behind
/// the port here rather than leaking up into the engine.
fn sync_path(path: &Path) -> io::Result<()> {
    // A fresh read handle is enough: fsync acts on the inode, not the descriptor,
    // so it flushes writes made through any handle. A path that does not exist (a
    // fallback write that failed before creating it) has nothing to flush and is
    // not an error.
    //
    // Opening a *directory* for reading and fsyncing it — how
    // [`Storage::write_atomic`] makes its rename durable — is a POSIX facility.
    // Windows has no equivalent (`MoveFileEx`'s durability is a separate story),
    // and rejects the open outright, so there the directory step is skipped
    // rather than faked.
    #[cfg(not(unix))]
    if path.is_dir() {
        return Ok(());
    }
    match std::fs::File::open(path) {
        Ok(file) => file.sync_all()?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use prov_graph::exec::block_on;

    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prov-fs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ---- capability declaration ----

    #[test]
    fn stdfs_declares_the_local_filesystem_guarantees() {
        // The native adapter promises atomic replacement and durable fsync, but
        // not native transactions — the journal's job, not the filesystem's.
        assert_eq!(StdFs.capabilities(), Capabilities::LOCAL_FS);
        assert!(StdFs.capabilities().atomic_replace);
        assert_eq!(StdFs.capabilities().sync_guarantee, SyncGuarantee::Durable);
        assert!(!StdFs.capabilities().native_transactions);
    }

    #[test]
    fn a_guarantee_answers_only_the_requests_it_can_keep() {
        // The whole point of the three-valued guarantee: the middle one can serve
        // `write_atomic`'s staging flush without being able to serve its final
        // one, which a boolean had no way to say.
        assert!(!SyncGuarantee::None.satisfies(Durability::Ordered));
        assert!(!SyncGuarantee::None.satisfies(Durability::Durable));
        assert!(SyncGuarantee::Ordered.satisfies(Durability::Ordered));
        assert!(!SyncGuarantee::Ordered.satisfies(Durability::Durable));
        assert!(SyncGuarantee::Durable.satisfies(Durability::Ordered));
        assert!(SyncGuarantee::Durable.satisfies(Durability::Durable));
    }

    // ---- the atomic-write protocol ----

    // ---- sync ----

    #[test]
    fn sync_of_a_missing_path_is_not_an_error() {
        // A fallback write that failed before creating the file leaves nothing to
        // flush; asking to sync it is a no-op, not a failure.
        let root = tmp("sync-missing");
        block_on(StdFs.sync(&root.join("never-created.md"), Durability::Durable)).unwrap();
    }

    #[test]
    fn sync_flushes_a_directory_as_readily_as_a_file() {
        // `write_atomic` makes its rename durable by syncing the directory, so a
        // directory has to be something `sync` accepts rather than something it
        // reaches only via a file's parent.
        let root = tmp("sync-dir");
        block_on(StdFs.sync(&root, Durability::Durable)).unwrap();
    }
}
