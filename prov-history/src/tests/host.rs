//! The host the skiplist is computed against, built to the trait and nothing
//! more.
//!
//! [`SkipHost`] hands over four facts, and every one of them is the
//! *workspace's* knowledge — so the honest test host states them as fixtures
//! rather than deriving them. Deriving reachability here would mean carrying a
//! graph, a parser and a format into a crate that is defined not to know any
//! of them; a fixture set is the trait taken at its word.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use prov_graph::error::Result;
use prov_graph::fs::{DirEntry, ReadStorage, StdFs};

use crate::SkipHost;

pub(super) struct TestHost {
    root: PathBuf,
    reachable: BTreeSet<PathBuf>,
    bookkeeping: Vec<PathBuf>,
    claimed: BTreeSet<PathBuf>,
}

impl TestHost {
    pub(super) fn new(root: &Path) -> Self {
        Self {
            root: root.to_owned(),
            reachable: BTreeSet::new(),
            bookkeeping: Vec::new(),
            claimed: BTreeSet::new(),
        }
    }

    /// Declare what the graph reaches.
    pub(super) fn reaches(mut self, paths: &[&str]) -> Self {
        self.reachable = paths.iter().map(PathBuf::from).collect();
        self
    }

    /// Declare a bookkeeping prefix — the recycle bin's items, a derived page.
    pub(super) fn parks(mut self, prefixes: &[&str]) -> Self {
        self.bookkeeping = prefixes.iter().map(PathBuf::from).collect();
        self
    }

    /// Declare a manifest-claimed archive directory.
    pub(super) fn claims(mut self, dirs: &[&str]) -> Self {
        self.claimed = dirs.iter().map(PathBuf::from).collect();
        self
    }
}

impl SkipHost for TestHost {
    async fn listing(&self, rel_dir: &Path) -> Result<Vec<DirEntry>> {
        Ok(StdFs.read_dir(&self.root.join(rel_dir)).await?)
    }

    async fn reachable_files(&self, _root_doc: &Path) -> Result<BTreeSet<PathBuf>> {
        Ok(self.reachable.clone())
    }

    async fn bookkeeping(&self, _root_doc: &Path) -> Result<Vec<PathBuf>> {
        Ok(self.bookkeeping.clone())
    }

    async fn claimed(&self, rel_dir: &Path) -> Result<bool> {
        Ok(self.claimed.contains(rel_dir))
    }
}
