//! An in-memory [`Storage`] backend.
//!
//! Available on every target this crate compiles for — including
//! `wasm32-unknown-unknown`, where it needs no browser API at all. Useful for
//! tests and sandboxes, and for clients (a WASM frontend with no direct disk
//! access) that load a workspace into memory up front and persist it
//! out-of-band (export/import, a network round-trip, OPFS as a bulk blob).

use std::collections::{HashMap, HashSet};
use std::io::{self, Error, ErrorKind};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock};

use super::{DirEntry, FileType, Metadata, ReadStorage};

use super::{Capabilities, Storage};

/// An in-memory, clone-shared [`Storage`] backend.
///
/// Content lives behind `Arc<RwLock<_>>`, so cloning an `InMemoryFs` is cheap
/// and every clone sees the same files — the same relationship an `Arc<StdFs>`
/// has to the one real filesystem it names, but without needing the `Arc`
/// wrapper, since it's built into the type. `std::sync::RwLock` (not a
/// runtime's async lock) is deliberate: every method here runs to completion
/// without ever awaiting *inside* the critical section, so there is nothing
/// for an async lock to buy, and a plain `std::sync` primitive is the one
/// that's guaranteed to exist — and to compile — on `wasm32-unknown-unknown`,
/// which has no threads and no async-runtime assumption to lean on.
///
/// Text and binary content are stored separately (a write picks one store
/// based on whether the bytes are valid UTF-8) so that a round-trip through
/// [`export_entries`](Self::export_entries) — text only — stays plain
/// strings, the shape a JS/WASM caller wants. Directories are tracked
/// explicitly (in a `HashSet`) rather than inferred from file paths, so an
/// empty directory `create_dir_all` created still shows up in
/// [`read_dir`](ReadStorage::read_dir).
///
/// Symlinks may be added with [`add_symlink`](Self::add_symlink): reading or
/// getting [`metadata`](ReadStorage::metadata) of the link resolves to the
/// target's content, matching [`ReadStorage::metadata`]'s documented
/// "follows symlinks" contract. Resolution is a single hop, not a followed
/// chain — a symlink to a symlink is not resolved further — which is all the
/// coherence a test double needs; a real filesystem's chain-following and
/// cycle detection isn't reproduced here.
#[derive(Debug, Clone, Default)]
pub struct InMemoryFs {
    /// Text files, stored as path -> content.
    files: Arc<RwLock<HashMap<PathBuf, String>>>,
    /// Binary (non-UTF-8) files, stored as path -> bytes.
    binary_files: Arc<RwLock<HashMap<PathBuf, Vec<u8>>>>,
    /// Directories known to exist — implicitly populated by every write's
    /// parent chain, and by an explicit `create_dir_all`.
    directories: Arc<RwLock<HashSet<PathBuf>>>,
    /// Symlinks: link path -> target path. Reading the link path resolves to
    /// the target's content; the parent's `read_dir` reports the link itself
    /// as [`FileType::SYMLINK`].
    symlinks: Arc<RwLock<HashMap<PathBuf, PathBuf>>>,
}

impl InMemoryFs {
    /// An empty in-memory filesystem.
    pub fn new() -> Self {
        Self::default()
    }

    /// A filesystem pre-populated with text files (and the directories that
    /// contain them).
    pub fn with_files(entries: Vec<(PathBuf, String)>) -> Self {
        let fs = Self::new();
        {
            let mut files = fs.files.write().unwrap();
            let mut dirs = fs.directories.write().unwrap();
            for (path, content) in entries {
                insert_ancestor_dirs(&mut dirs, &path);
                files.insert(path, content);
            }
        }
        fs
    }

    /// Load files from `(path_string, content)` pairs — convenience for a
    /// caller (JS/WASM interop) that only has strings, not `PathBuf`s.
    pub fn load_from_entries(entries: Vec<(String, String)>) -> Self {
        Self::with_files(
            entries
                .into_iter()
                .map(|(path, content)| (PathBuf::from(path), content))
                .collect(),
        )
    }

    /// Every text file, as `(path_string, content)` pairs — the counterpart to
    /// [`load_from_entries`](Self::load_from_entries), for persisting a
    /// session's edits back out.
    pub fn export_entries(&self) -> Vec<(String, String)> {
        self.files
            .read()
            .unwrap()
            .iter()
            .map(|(path, content)| (path.to_string_lossy().into_owned(), content.clone()))
            .collect()
    }

    /// Every binary file, as `(path_string, content_bytes)` pairs.
    pub fn export_binary_entries(&self) -> Vec<(String, Vec<u8>)> {
        self.binary_files
            .read()
            .unwrap()
            .iter()
            .map(|(path, content)| (path.to_string_lossy().into_owned(), content.clone()))
            .collect()
    }

    /// Load binary files from `(path_string, content_bytes)` pairs.
    pub fn load_binary_entries(&self, entries: Vec<(String, Vec<u8>)>) {
        let mut binary_files = self.binary_files.write().unwrap();
        let mut dirs = self.directories.write().unwrap();
        for (path_str, content) in entries {
            let path = PathBuf::from(path_str);
            insert_ancestor_dirs(&mut dirs, &path);
            binary_files.insert(path, content);
        }
    }

    /// Every text-file path currently stored.
    pub fn list_all_files(&self) -> Vec<PathBuf> {
        self.files.read().unwrap().keys().cloned().collect()
    }

    /// Remove every file, directory, and symlink — resetting the filesystem to
    /// empty without needing a fresh `InMemoryFs` (and its own, separately
    /// shared, clones).
    pub fn clear(&self) {
        self.files.write().unwrap().clear();
        self.binary_files.write().unwrap().clear();
        self.directories.write().unwrap().clear();
        self.symlinks.write().unwrap().clear();
    }

    /// Add a symlink from `link` to `target`. Reading `link` (or its
    /// [`metadata`](ReadStorage::metadata)) resolves to `target`'s content;
    /// `link`'s entry in its parent's [`read_dir`](ReadStorage::read_dir) reports
    /// [`FileType::SYMLINK`] — the un-followed type a caller needs in order to
    /// recognize and skip it, since `metadata` itself only ever reports the
    /// followed, resolved type.
    pub fn add_symlink(&self, link: &Path, target: &Path) {
        let link = normalize_path(link);
        let target = normalize_path(target);
        insert_ancestor_dirs(&mut self.directories.write().unwrap(), &link);
        self.symlinks.write().unwrap().insert(link, target);
    }

    /// The single-hop resolution [`ReadStorage::read`], [`ReadStorage::read_to_string`],
    /// and [`ReadStorage::metadata`] all use: a symlinked path resolves to its
    /// target; anything else resolves to itself.
    fn resolve(&self, normalized: &Path) -> PathBuf {
        self.symlinks
            .read()
            .unwrap()
            .get(normalized)
            .cloned()
            .unwrap_or_else(|| normalized.to_path_buf())
    }
}

/// Strip `.` and resolve `..` lexically — the backend has no real parent
/// directories to walk, so this is the closest available analog of
/// `std::fs`'s implicit path resolution, and it's what keeps
/// `"dir/file.md"` and `"dir/sub/../file.md"` naming the same entry.
fn normalize_path(path: &Path) -> PathBuf {
    let mut components: Vec<Component> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !matches!(components.last(), None | Some(Component::RootDir)) {
                    components.pop();
                }
            }
            c => components.push(c),
        }
    }
    components.iter().collect()
}

/// Register every non-empty ancestor of `path` as an existing directory —
/// the implicit parent-creation a real `write` to a nested path performs via
/// `create_dir_all`.
fn insert_ancestor_dirs(dirs: &mut HashSet<PathBuf>, path: &Path) {
    let mut current = path;
    while let Some(parent) = current.parent() {
        if parent.as_os_str().is_empty() {
            break;
        }
        dirs.insert(parent.to_path_buf());
        current = parent;
    }
}

fn not_found(path: &Path) -> Error {
    Error::new(
        ErrorKind::NotFound,
        format!("not found: {}", path.display()),
    )
}

impl ReadStorage for InMemoryFs {
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let normalized = normalize_path(path);
        let resolved = self.resolve(&normalized);
        if let Some(data) = self.binary_files.read().unwrap().get(&resolved) {
            return Ok(data.clone());
        }
        if let Some(text) = self.files.read().unwrap().get(&resolved) {
            return Ok(text.as_bytes().to_vec());
        }
        Err(not_found(path))
    }

    async fn read_to_string(&self, path: &Path) -> io::Result<String> {
        // Built on `read` rather than duplicating its lookup: this is the one
        // point of divergence from the crossfs reference, and it's a
        // correctness fix, not just a dedup — reusing `read` means a binary
        // file correctly reports `InvalidData` (mirroring
        // `std::fs::read_to_string`) instead of a misleading `NotFound`.
        let bytes = self.read(path).await?;
        String::from_utf8(bytes).map_err(|e| Error::new(ErrorKind::InvalidData, e))
    }

    async fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        let normalized = normalize_path(path);
        if !normalized.as_os_str().is_empty()
            && !self.directories.read().unwrap().contains(&normalized)
        {
            return Err(not_found(path));
        }

        let mut result = Vec::new();
        for entry in self.files.read().unwrap().keys() {
            if entry.parent() == Some(normalized.as_path()) {
                result.push(DirEntry::new(entry.clone(), FileType::FILE));
            }
        }
        for entry in self.binary_files.read().unwrap().keys() {
            if entry.parent() == Some(normalized.as_path()) {
                result.push(DirEntry::new(entry.clone(), FileType::FILE));
            }
        }
        // Listed by un-followed type — a caller that wants to skip symlinks
        // (rather than transparently read through them) needs exactly this,
        // since `metadata` itself only ever reports the resolved type.
        for entry in self.symlinks.read().unwrap().keys() {
            if entry.parent() == Some(normalized.as_path()) {
                result.push(DirEntry::new(entry.clone(), FileType::SYMLINK));
            }
        }
        for entry in self.directories.read().unwrap().iter() {
            if entry.parent() == Some(normalized.as_path()) && entry != &normalized {
                result.push(DirEntry::new(entry.clone(), FileType::DIR));
            }
        }
        Ok(result)
    }

    async fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        let normalized = normalize_path(path);
        let resolved = self.resolve(&normalized);

        if let Some(data) = self.binary_files.read().unwrap().get(&resolved) {
            return Ok(Metadata::new(FileType::FILE, data.len() as u64, None));
        }
        if let Some(text) = self.files.read().unwrap().get(&resolved) {
            return Ok(Metadata::new(FileType::FILE, text.len() as u64, None));
        }
        if self.directories.read().unwrap().contains(&resolved) {
            return Ok(Metadata::new(FileType::DIR, 0, None));
        }
        Err(not_found(path))
    }

    // No modification-time tracking: unlike a real filesystem there is no
    // clock backing these bytes, and a fabricated timestamp (e.g. "now" on
    // every write) would claim a precision this backend cannot honor across
    // a clone or an export/import round-trip. `Metadata::modified` reports
    // `Unsupported` accordingly — an honest "this backend doesn't know",
    // exactly as it would for a real backend that genuinely lacks the field.
}

impl Storage for InMemoryFs {
    async fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        let normalized = normalize_path(path);
        insert_ancestor_dirs(&mut self.directories.write().unwrap(), &normalized);

        // Store as text when the bytes are valid UTF-8, so `read_to_string`
        // and `export_entries` see a plain string — matching the diaryx
        // behavior this mirrors, where `write`/`read_to_string` round-tripped
        // through a text store. Non-UTF-8 content still round-trips through
        // `read`, just via the binary store instead.
        match std::str::from_utf8(contents) {
            Ok(s) => {
                self.files
                    .write()
                    .unwrap()
                    .insert(normalized.clone(), s.to_string());
                self.binary_files.write().unwrap().remove(&normalized);
            }
            Err(_) => {
                self.binary_files
                    .write()
                    .unwrap()
                    .insert(normalized.clone(), contents.to_vec());
                self.files.write().unwrap().remove(&normalized);
            }
        }
        Ok(())
    }

    async fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        let normalized = normalize_path(path);
        let mut dirs = self.directories.write().unwrap();
        if !normalized.as_os_str().is_empty() {
            dirs.insert(normalized.clone());
        }
        insert_ancestor_dirs(&mut dirs, &normalized);
        Ok(())
    }

    async fn remove_file(&self, path: &Path) -> io::Result<()> {
        let normalized = normalize_path(path);
        if self.files.write().unwrap().remove(&normalized).is_some() {
            return Ok(());
        }
        if self
            .binary_files
            .write()
            .unwrap()
            .remove(&normalized)
            .is_some()
        {
            return Ok(());
        }
        if self.symlinks.write().unwrap().remove(&normalized).is_some() {
            return Ok(());
        }
        Err(not_found(path))
    }

    async fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        let normalized = normalize_path(path);
        self.files
            .write()
            .unwrap()
            .retain(|p, _| !p.starts_with(&normalized));
        self.binary_files
            .write()
            .unwrap()
            .retain(|p, _| !p.starts_with(&normalized));
        self.symlinks
            .write()
            .unwrap()
            .retain(|p, _| !p.starts_with(&normalized));
        self.directories
            .write()
            .unwrap()
            .retain(|p| p != &normalized && !p.starts_with(&normalized));
        Ok(())
    }

    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let from_norm = normalize_path(from);
        let to_norm = normalize_path(to);
        if from_norm == to_norm {
            return Ok(());
        }

        let is_dir = self.directories.read().unwrap().contains(&from_norm);
        if is_dir {
            self.rename_dir(&from_norm, &to_norm, to)
        } else {
            self.rename_file(&from_norm, &to_norm, from, to).await
        }
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::IN_MEMORY
    }

    async fn write_atomic(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        // The default protocol stages through a temp sibling and a `rename`
        // because *that* is what makes a plain `write` atomic on a real
        // filesystem. Here, a single `write` already is the atomic step — it
        // takes the map's write lock for its entire duration, so no observer
        // ever sees a splice — so replaying the temp-then-rename dance would
        // only litter the map with a `.prov-tmp` entry no caller asked for.
        // This is exactly the "backend with a better native path" case the
        // default documents overriding wholesale.
        self.write(path, contents).await
    }
}

impl InMemoryFs {
    fn rename_dir(&self, from_norm: &Path, to_norm: &Path, to: &Path) -> io::Result<()> {
        {
            let files = self.files.read().unwrap();
            let bin = self.binary_files.read().unwrap();
            let dirs = self.directories.read().unwrap();
            if files.contains_key(to_norm) || bin.contains_key(to_norm) || dirs.contains(to_norm) {
                return Err(Error::new(
                    ErrorKind::AlreadyExists,
                    format!("destination already exists: {}", to.display()),
                ));
            }
        }

        let files_to_move: Vec<(PathBuf, String)> = self
            .files
            .read()
            .unwrap()
            .iter()
            .filter(|(p, _)| p.starts_with(from_norm))
            .map(|(p, c)| (p.clone(), c.clone()))
            .collect();
        let binaries_to_move: Vec<(PathBuf, Vec<u8>)> = self
            .binary_files
            .read()
            .unwrap()
            .iter()
            .filter(|(p, _)| p.starts_with(from_norm))
            .map(|(p, c)| (p.clone(), c.clone()))
            .collect();

        {
            let mut files = self.files.write().unwrap();
            for (old_path, content) in files_to_move {
                files.remove(&old_path);
                let relative = old_path.strip_prefix(from_norm).unwrap();
                files.insert(to_norm.join(relative), content);
            }
        }
        {
            let mut binary = self.binary_files.write().unwrap();
            for (old_path, content) in binaries_to_move {
                binary.remove(&old_path);
                let relative = old_path.strip_prefix(from_norm).unwrap();
                binary.insert(to_norm.join(relative), content);
            }
        }
        {
            let mut dirs = self.directories.write().unwrap();
            let old_dirs: Vec<PathBuf> = dirs
                .iter()
                .filter(|d| d.starts_with(from_norm))
                .cloned()
                .collect();
            for old_dir in old_dirs {
                dirs.remove(&old_dir);
                let relative = old_dir.strip_prefix(from_norm).unwrap();
                dirs.insert(to_norm.join(relative));
            }
            insert_ancestor_dirs(&mut dirs, to_norm);
        }

        Ok(())
    }

    async fn rename_file(
        &self,
        from_norm: &Path,
        to_norm: &Path,
        from: &Path,
        to: &Path,
    ) -> io::Result<()> {
        {
            let files = self.files.read().unwrap();
            let bin = self.binary_files.read().unwrap();
            if !files.contains_key(from_norm) && !bin.contains_key(from_norm) {
                return Err(not_found(from));
            }
            if files.contains_key(to_norm) || bin.contains_key(to_norm) {
                return Err(Error::new(
                    ErrorKind::AlreadyExists,
                    format!("destination already exists: {}", to.display()),
                ));
            }
        }

        if let Some(parent) = to_norm.parent() {
            self.create_dir_all(parent).await?;
        }

        // Each removal is its own statement, not an `if let`'s scrutinee: an
        // `if let SCRUTINEE { BODY }` extends the scrutinee's temporaries
        // across the whole body, so writing `if let Some(c) =
        // self.files.write().unwrap().remove(..) { self.files.write()... }`
        // would keep the first write guard alive while the body took a second
        // one on the same lock — a same-thread self-deadlock on
        // `std::sync::RwLock`, not a panic. Binding the removal to a plain
        // `let` first drops that guard before the body ever runs.
        let removed_text = self.files.write().unwrap().remove(from_norm);
        if let Some(content) = removed_text {
            self.files
                .write()
                .unwrap()
                .insert(to_norm.to_path_buf(), content);
            return Ok(());
        }
        let removed_binary = self.binary_files.write().unwrap().remove(from_norm);
        if let Some(content) = removed_binary {
            self.binary_files
                .write()
                .unwrap()
                .insert(to_norm.to_path_buf(), content);
            return Ok(());
        }
        Err(not_found(from))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::block_on;

    #[test]
    fn read_write_roundtrip() {
        let fs = InMemoryFs::new();
        block_on(fs.write(Path::new("test.md"), b"Hello, World!")).unwrap();
        assert_eq!(
            block_on(fs.read_to_string(Path::new("test.md"))).unwrap(),
            "Hello, World!"
        );
        assert!(block_on(fs.try_exists(Path::new("test.md"))).unwrap());
        block_on(fs.remove_file(Path::new("test.md"))).unwrap();
        assert!(!block_on(fs.try_exists(Path::new("test.md"))).unwrap());
    }

    #[test]
    fn binary_content_round_trips_through_read_but_not_read_to_string() {
        let fs = InMemoryFs::new();
        let invalid_utf8 = vec![0xff, 0xfe, 0xfd];
        block_on(fs.write(Path::new("bin.dat"), &invalid_utf8)).unwrap();
        assert_eq!(
            block_on(fs.read(Path::new("bin.dat"))).unwrap(),
            invalid_utf8
        );
        let err = block_on(fs.read_to_string(Path::new("bin.dat"))).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn create_dir_all_creates_parents_implicitly_via_write() {
        let fs = InMemoryFs::new();
        block_on(fs.write(Path::new("a/b/c/file.md"), b"Content")).unwrap();
        assert!(block_on(fs.metadata(Path::new("a"))).unwrap().is_dir());
        assert!(block_on(fs.metadata(Path::new("a/b"))).unwrap().is_dir());
        assert!(block_on(fs.metadata(Path::new("a/b/c"))).unwrap().is_dir());
        assert!(block_on(fs.try_exists(Path::new("a/b/c/file.md"))).unwrap());
    }

    #[test]
    fn read_dir_returns_immediate_children_only() {
        let fs = InMemoryFs::new();
        block_on(fs.write(Path::new("dir/file1.md"), b"1")).unwrap();
        block_on(fs.write(Path::new("dir/file2.md"), b"2")).unwrap();
        block_on(fs.write(Path::new("dir/subdir/file3.md"), b"3")).unwrap();

        let entries = block_on(fs.read_dir(Path::new("dir"))).unwrap();
        let paths: Vec<PathBuf> = entries.iter().map(|e| e.path().to_path_buf()).collect();
        assert!(paths.contains(&PathBuf::from("dir/file1.md")));
        assert!(paths.contains(&PathBuf::from("dir/file2.md")));
        assert!(paths.contains(&PathBuf::from("dir/subdir")));
        assert!(!paths.contains(&PathBuf::from("dir/subdir/file3.md")));
    }

    #[test]
    fn read_dir_of_an_untracked_directory_is_not_found() {
        // Fidelity to `std::fs::read_dir`'s contract: a path that was never
        // written to or `create_dir_all`'d is an error, not an empty listing.
        let fs = InMemoryFs::new();
        let err = block_on(fs.read_dir(Path::new("never/created"))).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn read_dir_of_the_root_never_errors() {
        // The root is never explicitly inserted into `directories` (it has no
        // non-empty parent to register it), so it needs its own carve-out
        // against the untracked-directory check above.
        let fs = InMemoryFs::new();
        assert!(block_on(fs.read_dir(Path::new(""))).unwrap().is_empty());
    }

    #[test]
    fn export_then_import_roundtrip() {
        let fs = InMemoryFs::new();
        block_on(fs.write(Path::new("file1.md"), b"Content 1")).unwrap();
        block_on(fs.write(Path::new("dir/file2.md"), b"Content 2")).unwrap();

        let entries = fs.export_entries();
        let fs2 = InMemoryFs::load_from_entries(entries);

        assert_eq!(
            block_on(fs2.read_to_string(Path::new("file1.md"))).unwrap(),
            "Content 1"
        );
        assert_eq!(
            block_on(fs2.read_to_string(Path::new("dir/file2.md"))).unwrap(),
            "Content 2"
        );
    }

    #[test]
    fn path_normalization() {
        let fs = InMemoryFs::new();
        block_on(fs.write(Path::new("dir/file.md"), b"Content")).unwrap();
        assert!(block_on(fs.try_exists(Path::new("dir/file.md"))).unwrap());
        assert!(block_on(fs.try_exists(Path::new("dir/./file.md"))).unwrap());
        assert!(block_on(fs.try_exists(Path::new("dir/subdir/../file.md"))).unwrap());
    }

    #[test]
    fn rename_moves_a_single_file() {
        let fs = InMemoryFs::new();
        block_on(fs.write(Path::new("old.md"), b"content")).unwrap();
        block_on(fs.rename(Path::new("old.md"), Path::new("new.md"))).unwrap();
        assert!(!block_on(fs.try_exists(Path::new("old.md"))).unwrap());
        assert_eq!(
            block_on(fs.read_to_string(Path::new("new.md"))).unwrap(),
            "content"
        );
    }

    #[test]
    fn rename_moves_a_directory_and_its_contents() {
        let fs = InMemoryFs::new();
        block_on(fs.write(Path::new("dir/a.md"), b"a")).unwrap();
        block_on(fs.write(Path::new("dir/sub/b.md"), b"b")).unwrap();

        block_on(fs.rename(Path::new("dir"), Path::new("moved"))).unwrap();

        assert!(!block_on(fs.try_exists(Path::new("dir/a.md"))).unwrap());
        assert_eq!(
            block_on(fs.read_to_string(Path::new("moved/a.md"))).unwrap(),
            "a"
        );
        assert_eq!(
            block_on(fs.read_to_string(Path::new("moved/sub/b.md"))).unwrap(),
            "b"
        );
        assert!(
            block_on(fs.metadata(Path::new("moved/sub")))
                .unwrap()
                .is_dir()
        );
    }

    #[test]
    fn rename_refuses_to_clobber_an_existing_destination() {
        let fs = InMemoryFs::new();
        block_on(fs.write(Path::new("a.md"), b"a")).unwrap();
        block_on(fs.write(Path::new("b.md"), b"b")).unwrap();
        let err = block_on(fs.rename(Path::new("a.md"), Path::new("b.md"))).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    }

    // ---- symlinks: coherence with `ReadStorage::metadata`'s "follows symlinks"
    // contract, and with `read_dir`'s un-followed listing — the two shapes
    // diaryx_core's validator actually exercises (skip a symlink named
    // directly, and skip one discovered by scanning a directory). ----

    #[test]
    fn metadata_and_read_follow_a_symlink_to_its_target() {
        let fs = InMemoryFs::new();
        block_on(fs.write(Path::new("real.md"), b"hello")).unwrap();
        fs.add_symlink(Path::new("link.md"), Path::new("real.md"));

        let m = block_on(fs.metadata(Path::new("link.md"))).unwrap();
        assert!(m.is_file());
        assert!(!m.is_dir());

        assert_eq!(
            block_on(fs.read_to_string(Path::new("link.md"))).unwrap(),
            "hello"
        );
    }

    #[test]
    fn read_dir_reports_a_symlink_by_its_own_unfollowed_type() {
        // This is what a directory scan (diaryx_core's orphan-file pass) uses
        // to recognize and skip a symlink without ever resolving it.
        let fs = InMemoryFs::new();
        block_on(fs.write(Path::new("real.md"), b"hello")).unwrap();
        fs.add_symlink(Path::new("link.md"), Path::new("real.md"));

        let entries = block_on(fs.read_dir(Path::new(""))).unwrap();
        let link_entry = entries
            .iter()
            .find(|e| e.path() == Path::new("link.md"))
            .expect("symlink should appear in its parent's listing");
        assert!(link_entry.file_type().is_symlink());

        let real_entry = entries
            .iter()
            .find(|e| e.path() == Path::new("real.md"))
            .expect("the real file should also be listed");
        assert!(!real_entry.file_type().is_symlink());
    }

    #[test]
    fn a_symlink_to_a_missing_target_is_not_found_by_metadata() {
        let fs = InMemoryFs::new();
        fs.add_symlink(Path::new("dangling.md"), Path::new("nowhere.md"));
        let err = block_on(fs.metadata(Path::new("dangling.md"))).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn removing_a_symlink_leaves_its_target_untouched() {
        let fs = InMemoryFs::new();
        block_on(fs.write(Path::new("real.md"), b"hello")).unwrap();
        fs.add_symlink(Path::new("link.md"), Path::new("real.md"));

        block_on(fs.remove_file(Path::new("link.md"))).unwrap();

        assert!(!block_on(fs.try_exists(Path::new("link.md"))).unwrap());
        assert_eq!(
            block_on(fs.read_to_string(Path::new("real.md"))).unwrap(),
            "hello"
        );
    }

    // ---- capabilities ----

    #[test]
    fn in_memory_declares_atomic_replace_but_no_durability_across_a_restart() {
        let fs = InMemoryFs::new();
        let caps = fs.capabilities();
        assert!(
            caps.atomic_replace,
            "a single locked write is already atomic"
        );
        assert_eq!(
            caps.sync_guarantee,
            super::super::SyncGuarantee::None,
            "nothing here survives the process exiting, so there is not even an \
             ordering worth promising against a crash"
        );
        assert!(
            !caps.native_transactions,
            "the lock covers one call, not a batch of several committed together"
        );
    }

    #[test]
    fn write_atomic_lands_the_new_contents_without_a_temp_sibling() {
        let fs = InMemoryFs::new();
        block_on(fs.write(Path::new("doc.md"), b"old")).unwrap();
        block_on(fs.write_atomic(Path::new("doc.md"), b"new")).unwrap();

        assert_eq!(
            block_on(fs.read_to_string(Path::new("doc.md"))).unwrap(),
            "new"
        );
        // No `.doc.md.prov-tmp` sibling should exist — `write_atomic` was
        // overridden to skip the default's staging dance.
        let entries = block_on(fs.read_dir(Path::new(""))).unwrap();
        assert_eq!(entries.len(), 1, "no stray temp-sibling entry: {entries:?}");
    }

    // ---- clone-shares-state ----

    #[test]
    fn clones_share_the_same_backing_store() {
        let fs = InMemoryFs::new();
        let clone = fs.clone();
        block_on(fs.write(Path::new("shared.md"), b"visible everywhere")).unwrap();
        assert_eq!(
            block_on(clone.read_to_string(Path::new("shared.md"))).unwrap(),
            "visible everywhere"
        );
    }

    #[test]
    fn clear_empties_every_store() {
        let fs = InMemoryFs::new();
        block_on(fs.write(Path::new("a.md"), b"a")).unwrap();
        fs.add_symlink(Path::new("link.md"), Path::new("a.md"));

        fs.clear();

        assert!(!block_on(fs.try_exists(Path::new("a.md"))).unwrap());
        assert!(block_on(fs.metadata(Path::new("link.md"))).is_err());
    }
}
