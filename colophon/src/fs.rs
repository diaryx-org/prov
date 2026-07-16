//! colophon's filesystem port.
//!
//! colophon is generic over *where* documents live. Rather than depend on any
//! one concrete backend — `std::fs`, `tokio::fs`, or a browser filesystem like
//! OPFS/IndexedDB — the library asks only for a small async trait that mirrors
//! the slice of [`std::fs`] its scan/traverse/mutate engine needs. Integrators
//! implement [`Storage`] over whatever backend they have; the workspace never
//! learns which one.
//!
//! This is the classic *ports and adapters* seam. The trait uses native
//! `async fn` (no boxed futures) because [`crate::workspace::Workspace`] is
//! generic over its backend rather than erased to `dyn`, so callers keep the
//! backend's real future types and their `Send`-ness. A backend whose futures
//! are `Send` composes into multithreaded runtimes unchanged.
//!
//! The method set mirrors [`std::fs`] names exactly so an adapter is mechanical
//! to write.

use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// An async filesystem backend colophon can drive.
///
/// Each method mirrors the [`std::fs`] function of the same name. Backends
/// implement the read/write/mutate/inspect surface; [`try_exists`] has a
/// default in terms of [`metadata`].
///
/// [`try_exists`]: Storage::try_exists
/// [`metadata`]: Storage::metadata
pub trait Storage {
    // ---- read ----

    /// Read the entire contents of a file as bytes. Mirrors [`std::fs::read`].
    fn read(&self, path: &Path) -> impl Future<Output = io::Result<Vec<u8>>>;

    /// Read the entire contents of a file as a string. Mirrors
    /// [`std::fs::read_to_string`].
    fn read_to_string(&self, path: &Path) -> impl Future<Output = io::Result<String>>;

    /// Return the entries in a directory (non-recursive). Mirrors
    /// [`std::fs::read_dir`], but yields a `Vec` since async iterators are not
    /// yet stable.
    fn read_dir(&self, path: &Path) -> impl Future<Output = io::Result<Vec<DirEntry>>>;

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

    // ---- inspect ----

    /// Return metadata about the entry at `path`. Mirrors
    /// [`std::fs::metadata`]; follows symlinks.
    fn metadata(&self, path: &Path) -> impl Future<Output = io::Result<Metadata>>;

    /// Returns `Ok(true)` if the path exists, `Ok(false)` if it does not, and
    /// `Err(_)` if the check itself failed. Mirrors [`std::fs::try_exists`].
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

/// One entry returned by [`Storage::read_dir`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    path: PathBuf,
    file_type: FileType,
}

impl DirEntry {
    /// Construct an entry from its path and type.
    pub fn new(path: impl Into<PathBuf>, file_type: FileType) -> Self {
        Self { path: path.into(), file_type }
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

/// Metadata about a filesystem entry — the subset colophon needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metadata {
    file_type: FileType,
    len: u64,
    modified: Option<SystemTime>,
}

impl Metadata {
    /// Construct metadata from its parts.
    pub fn new(file_type: FileType, len: u64, modified: Option<SystemTime>) -> Self {
        Self { file_type, len, modified }
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

/// [`Storage`] over the process filesystem (`std::fs`).
///
/// The trait is async so that genuinely async backends (network, OPFS) fit;
/// this adapter's futures are immediately ready, so any executor — including
/// the dependency-free [`crate::exec::block_on`] — drives them to completion
/// in a single poll.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdFs;

impl Storage for StdFs {
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
                Ok(DirEntry::new(entry.path(), convert_file_type(entry.file_type()?)))
            })
            .collect()
    }

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

    async fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        let md = std::fs::metadata(path)?;
        Ok(Metadata::new(convert_file_type(md.file_type()), md.len(), md.modified().ok()))
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

/// [`Storage`] over `std::fs` that fails the *n*th write, for testing that a
/// [`ChangeSet`](crate::change::ChangeSet) unwinds.
///
/// Every other method delegates to [`StdFs`], so a workspace over this backend
/// behaves exactly like a real one until the chosen write, then reports the kind
/// of failure a full disk or a revoked permission would.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct FailAtWrite {
    writes: std::cell::Cell<usize>,
    fail_at: usize,
}

#[cfg(test)]
impl FailAtWrite {
    /// Fail the `fail_at`th write (0-indexed); let every other one through.
    pub(crate) fn nth(fail_at: usize) -> Self {
        Self { writes: std::cell::Cell::new(0), fail_at }
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

#[cfg(test)]
impl Storage for FailAtWrite {
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        StdFs.read(path).await
    }
    async fn read_to_string(&self, path: &Path) -> io::Result<String> {
        StdFs.read_to_string(path).await
    }
    async fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        StdFs.read_dir(path).await
    }
    async fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
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
    async fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        StdFs.metadata(path).await
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
    pub const FILE: FileType = FileType { is_dir: false, is_file: true, is_symlink: false };

    /// A directory.
    pub const DIR: FileType = FileType { is_dir: true, is_file: false, is_symlink: false };

    /// A symbolic link.
    pub const SYMLINK: FileType = FileType { is_dir: false, is_file: false, is_symlink: true };

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
