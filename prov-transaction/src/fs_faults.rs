use std::io;
use std::path::{Path, PathBuf};

use prov_graph::fs::{Capabilities, DirEntry, Durability, Metadata, ReadStorage, StdFs, Storage};

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

#[derive(Debug)]
pub struct FailAtWrite {
    writes: std::cell::Cell<usize>,
    fail_at: usize,
}

impl FailAtWrite {
    pub fn nth(fail_at: usize) -> Self {
        Self {
            writes: std::cell::Cell::new(0),
            fail_at,
        }
    }
}

reads_like_stdfs!(FailAtWrite);

impl Storage for FailAtWrite {
    async fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        if super::journal::is_journal_path(path) {
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

    fn capabilities(&self) -> Capabilities {
        Capabilities::LOCAL_FS
    }

    async fn sync(&self, path: &Path, need: Durability) -> io::Result<()> {
        StdFs.sync(path, need).await
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsEvent {
    Write(PathBuf),
    Sync(PathBuf, Durability),
    Rename(PathBuf, PathBuf),
    Remove(PathBuf),
}

#[derive(Debug)]
pub struct RecordingFs {
    log: std::cell::RefCell<Vec<FsEvent>>,
    caps: Capabilities,
}

impl RecordingFs {
    pub fn local() -> Self {
        Self {
            log: std::cell::RefCell::new(Vec::new()),
            caps: Capabilities::LOCAL_FS,
        }
    }

    pub fn events(&self) -> Vec<FsEvent> {
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
