//! A real filesystem that counts what it was asked to read, per path.
//!
//! The observable the fixity cache exists to move: a capture that does not read
//! a file is the whole point, and "did not read it" is not visible in any result
//! the capture returns. Delegates everything to [`StdFs`], so timestamps, atomic
//! writes and durability all behave like the real thing — a capture over this
//! backend takes the same code path a production one does, and merely leaves a
//! tally behind.
//!
//! `prov` keeps its own copy of this (`prov::fs_faults`) beside the fault
//! injectors that only make sense against a whole workspace. The duplication is
//! deliberate: sharing it would mean one of the two crates exporting a test
//! double from its public surface, which is a worse trade than sixty lines.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use prov_graph::fs::{Capabilities, DirEntry, Durability, Metadata, ReadStorage, StdFs, Storage};

#[derive(Clone, Default, Debug)]
pub(super) struct CountingFs {
    /// `read` — raw bytes. What the capture manifest loop spends.
    bytes: Arc<Mutex<BTreeMap<PathBuf, usize>>>,
    /// `read_to_string` — documents. What the traversal passes spend.
    docs: Arc<Mutex<BTreeMap<PathBuf, usize>>>,
}

impl CountingFs {
    /// How many times the file at workspace-relative `rel` had its bytes read.
    pub(super) fn byte_reads(&self, dir: &Path, rel: &str) -> usize {
        count(&self.bytes, &dir.join(rel))
    }

    /// Total byte reads — for asserting that a whole pass read nothing at all.
    pub(super) fn total_byte_reads(&self) -> usize {
        self.bytes.lock().unwrap().values().sum()
    }

    pub(super) fn reset(&self) {
        self.bytes.lock().unwrap().clear();
        self.docs.lock().unwrap().clear();
    }
}

fn count(counter: &Mutex<BTreeMap<PathBuf, usize>>, path: &Path) -> usize {
    counter.lock().unwrap().get(path).copied().unwrap_or(0)
}

fn tally(counter: &Mutex<BTreeMap<PathBuf, usize>>, path: &Path) {
    *counter
        .lock()
        .unwrap()
        .entry(path.to_path_buf())
        .or_insert(0) += 1;
}

impl ReadStorage for CountingFs {
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        tally(&self.bytes, path);
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
    fn capabilities(&self) -> Capabilities {
        StdFs.capabilities()
    }
    async fn sync(&self, path: &Path, need: Durability) -> io::Result<()> {
        StdFs.sync(path, need).await
    }
}
