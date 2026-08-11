//! prov's filesystem port.
//!
//! prov is generic over *where* documents live. Rather than depend on any
//! one concrete backend — `std::fs`, `tokio::fs`, or a browser filesystem like
//! OPFS/IndexedDB — the library asks only for a small async trait that mirrors
//! the slice of [`std::fs`] its scan/traverse engine needs. Integrators
//! implement [`ReadStorage`] over whatever backend they have; the workspace
//! never learns which one.
//!
//! This is the classic *ports and adapters* seam. The trait uses native
//! `async fn` (no boxed futures) because [`Graph`](crate::graph::Graph) is
//! generic over its backend rather than erased to `dyn`, so callers keep the
//! backend's real future types and their `Send`-ness. A backend whose futures
//! are `Send` composes into multithreaded runtimes unchanged.
//!
//! The method set mirrors [`std::fs`] names exactly so an adapter is mechanical
//! to write.
//!
//! Only the read half is here. The write half — `Storage`, the durability
//! vocabulary, and the writable [`StdFs`]/in-memory adapters — is
//! `prov-store`'s `fs` module, so that depending on this crate cannot get you
//! the ability to change a workspace.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

/// The read half of an async filesystem backend: everything the traversal core
/// needs, and nothing that can change a byte on disk.
///
/// This is the trait [`crate::graph`] is generic over. The split is not
/// decoration — it is what lets the read core be depended on by a consumer that
/// must not, and cannot, write: a language server, a renderer, a browser
/// viewer. A backend that implements only this is a *provably* read-only
/// workspace, checked by the compiler rather than by review.
///
/// Each method mirrors the [`std::fs`] function of the same name.
/// [`try_exists`] has a default in terms of [`metadata`].
///
/// [`try_exists`]: ReadStorage::try_exists
/// [`metadata`]: ReadStorage::metadata
pub trait ReadStorage {
    /// Read the entire contents of a file as bytes. Mirrors [`std::fs::read`].
    fn read(&self, path: &Path) -> impl Future<Output = io::Result<Vec<u8>>>;

    /// Read the entire contents of a file as a string. Mirrors
    /// [`std::fs::read_to_string`].
    fn read_to_string(&self, path: &Path) -> impl Future<Output = io::Result<String>>;

    /// Return the entries in a directory (non-recursive). Mirrors
    /// [`std::fs::read_dir`], but yields a `Vec` since async iterators are not
    /// yet stable.
    fn read_dir(&self, path: &Path) -> impl Future<Output = io::Result<Vec<DirEntry>>>;

    /// Return metadata about the entry at `path`. Mirrors
    /// [`std::fs::metadata`]; follows symlinks.
    fn metadata(&self, path: &Path) -> impl Future<Output = io::Result<Metadata>>;

    /// Returns `Ok(true)` if the path exists, `Ok(false)` if it does not, and
    /// `Err(_)` if the check itself failed. Mirrors `std::fs::try_exists`.
    fn try_exists(&self, path: &Path) -> impl Future<Output = io::Result<bool>> {
        async move {
            match self.metadata(path).await {
                Ok(_) => Ok(true),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(e),
            }
        }
    }
}

/// A borrowed [`ReadStorage`] is itself a [`ReadStorage`] — so an owned backend
/// can be lent to something generic over `S: ReadStorage` (e.g. a temporary
/// [`Graph`](crate::graph::Graph)) without moving it or wrapping it in an
/// `Arc` the caller doesn't otherwise need.
///
/// Every member is forwarded explicitly. `prov-store`'s matching `Storage`
/// forwarding does the same for the durability members, where leaving any to
/// inherit the trait's defaults would silently downgrade a real backend's
/// guarantees the moment it was borrowed.
impl<S: ReadStorage + ?Sized> ReadStorage for &S {
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        (**self).read(path).await
    }

    async fn read_to_string(&self, path: &Path) -> io::Result<String> {
        (**self).read_to_string(path).await
    }

    async fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        (**self).read_dir(path).await
    }

    async fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        (**self).metadata(path).await
    }

    async fn try_exists(&self, path: &Path) -> io::Result<bool> {
        (**self).try_exists(path).await
    }
}

/// An `Arc<S>` is itself a [`ReadStorage`] on the same terms as `&S` above — so
/// a backend shared across several owners (several open `Workspace`s, a
/// multi-tab web client) still carries its real capabilities through the
/// `Arc`, rather than an adapter that forgot to unwrap it silently degrading
/// to the pessimistic defaults.
///
/// `Arc<S>` derefs to `S` exactly like `&S` does, so the same explicit,
/// every-member forwarding applies for the same reason: the trait's defaults
/// must never be reached by accident.
impl<S: ReadStorage + ?Sized> ReadStorage for Arc<S> {
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        (**self).read(path).await
    }

    async fn read_to_string(&self, path: &Path) -> io::Result<String> {
        (**self).read_to_string(path).await
    }

    async fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        (**self).read_dir(path).await
    }

    async fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        (**self).metadata(path).await
    }

    async fn try_exists(&self, path: &Path) -> io::Result<bool> {
        (**self).try_exists(path).await
    }
}

/// One entry returned by [`ReadStorage::read_dir`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    path: PathBuf,
    file_type: FileType,
}

impl DirEntry {
    /// Construct an entry from its path and type.
    pub fn new(path: impl Into<PathBuf>, file_type: FileType) -> Self {
        Self {
            path: path.into(),
            file_type,
        }
    }

    /// The full path to the entry.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The final component of the entry's path.
    pub fn file_name(&self) -> Option<&std::ffi::OsStr> {
        self.path.file_name()
    }

    /// The entry's type.
    pub fn file_type(&self) -> FileType {
        self.file_type
    }
}

/// Metadata about a filesystem entry — the subset prov needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metadata {
    file_type: FileType,
    len: u64,
    modified: Option<SystemTime>,
}

impl Metadata {
    /// Construct metadata from its parts.
    pub fn new(file_type: FileType, len: u64, modified: Option<SystemTime>) -> Self {
        Self {
            file_type,
            len,
            modified,
        }
    }

    /// The entry's type.
    pub fn file_type(&self) -> FileType {
        self.file_type
    }

    /// Whether the entry is a regular file.
    pub fn is_file(&self) -> bool {
        self.file_type.is_file()
    }

    /// Whether the entry is a directory.
    pub fn is_dir(&self) -> bool {
        self.file_type.is_dir()
    }

    /// Size in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the entry is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Last-modified time, if the backend reports one. Mirrors
    /// [`std::fs::Metadata::modified`], returning [`io::ErrorKind::Unsupported`]
    /// when unavailable.
    pub fn modified(&self) -> io::Result<SystemTime> {
        self.modified
            .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "modified time unavailable"))
    }
}

/// [`ReadStorage`] over the process filesystem (`std::fs`).
///
/// Reads only. The matching `Storage` implementation — everything that changes
/// a byte — is `prov-store`'s, so this adapter is writable exactly when that
/// crate is in the dependency graph.
///
/// The trait is async so that genuinely async backends (network, OPFS) fit;
/// this adapter's futures are immediately ready, so any executor — including
/// the dependency-free [`crate::exec::block_on`] — drives them to completion
/// in a single poll.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdFs;

impl ReadStorage for StdFs {
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }

    async fn read_to_string(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }

    async fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        std::fs::read_dir(path)?
            .map(|entry| {
                let entry = entry?;
                Ok(DirEntry::new(
                    entry.path(),
                    convert_file_type(entry.file_type()?),
                ))
            })
            .collect()
    }

    async fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        let md = std::fs::metadata(path)?;
        Ok(Metadata::new(
            convert_file_type(md.file_type()),
            md.len(),
            md.modified().ok(),
        ))
    }
}

fn convert_file_type(ft: std::fs::FileType) -> FileType {
    if ft.is_dir() {
        FileType::DIR
    } else if ft.is_file() {
        FileType::FILE
    } else {
        FileType::SYMLINK
    }
}

/// The type of a filesystem entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileType {
    is_dir: bool,
    is_file: bool,
    is_symlink: bool,
}

impl FileType {
    /// A regular file.
    pub const FILE: FileType = FileType {
        is_dir: false,
        is_file: true,
        is_symlink: false,
    };

    /// A directory.
    pub const DIR: FileType = FileType {
        is_dir: true,
        is_file: false,
        is_symlink: false,
    };

    /// A symbolic link.
    pub const SYMLINK: FileType = FileType {
        is_dir: false,
        is_file: false,
        is_symlink: true,
    };

    /// Whether this is a regular file.
    pub fn is_file(&self) -> bool {
        self.is_file
    }

    /// Whether this is a directory.
    pub fn is_dir(&self) -> bool {
        self.is_dir
    }

    /// Whether this is a symbolic link.
    pub fn is_symlink(&self) -> bool {
        self.is_symlink
    }
}
