//! The skiplist a prov workspace hands to historica.
//!
//! prov used to own a versioned safety net of its own — an event store of
//! whole-workspace manifests, with capture, restore, prune and forget verbs.
//! That machinery is retired: recording history is
//! [historica](https://crates.io/crates/historica)'s whole subject, and a
//! second implementation of it inside prov could only fall behind. What prov
//! still knows, and historica deliberately does not, is **which files are the
//! workspace** — the graph's bounded reachable walk — and that knowledge has
//! exactly one place to land in historica's model: `history/skipped.txt`, the
//! file that says what recording does not take.
//!
//! So this crate computes that file. It walks the folder the way historica's
//! own recording will, subtracts the reachable graph, and produces skip rules
//! for everything recording would otherwise sweep in: bookkeeping stores,
//! manifest-claimed archives, hidden directories, and files nothing links.
//! Recording itself stays with the `historica` command; nothing here writes a
//! revision.
//!
//! ## The region
//!
//! The graph changes with every edit, so the rules it implies cannot be
//! append-only the way hand-written rules are. The two are kept apart in the
//! file itself: this crate owns one region of `skipped.txt`, fenced by marker
//! comments, and regenerates it whole — the same contract a changelog's
//! generated region has. Everything outside the markers belongs to the person
//! (historica's own defaults included) and is preserved byte for byte.
//!
//! Two refusals keep the store sound:
//!
//! - **A rule never covers a tracked path.** historica refuses to record while
//!   a skip rule covers a file the tree holds, so a rule that would brick
//!   recording is withheld and reported instead — the person decides whether
//!   the file should be dropped from history or linked back into the graph.
//! - **A hand rule covering a reachable file is reported, not repaired.** The
//!   region is this crate's to rewrite; the rest of the file is not.
//!
//! ## The host
//!
//! Reachability, the location of byte-parking stores, and manifest claims are
//! the workspace's knowledge. This crate asks for them through [`SkipHost`]
//! rather than growing a capability to work them out; `prov`'s `Workspace`
//! implements the trait, and the tests implement it with fixtures.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use prov_graph::error::Result;
use prov_graph::fs::DirEntry;

mod plan;
mod store;

#[cfg(test)]
mod tests;

// The whole library, re-exported: this crate is its host's historica
// boundary, and a host that needs more of the store than the skiplist uses —
// a test initialising one, a tool reading one — should reach it through the
// same boundary rather than growing a second dependency to drift.
pub use historica;
pub use historica::store::{HEADER_FILE, STORE_DIR};
pub use historica::working::Rule;
pub use plan::{Reason, Skip, Skiplist, skiplist};
pub use store::{REGION_BEGIN, REGION_END, Standing, StandingError, apply};

/// What the workspace knows and the skiplist cannot work out for itself.
///
/// Deliberately narrow: four facts, no graph handle. The old history store
/// reached everything else through its host's `Graph`; the skiplist's whole
/// computation is a folder walk against a reachable set, and a trait that
/// hands over only that is a trait a test can implement with fixtures.
pub trait SkipHost {
    /// The entries of the workspace-relative directory `rel_dir` — the same
    /// listing recording itself will walk.
    fn listing(&self, rel_dir: &Path) -> impl Future<Output = Result<Vec<DirEntry>>>;

    /// Every file the workspace reaches from `root_doc` that is on disk — §8's
    /// bounded walk, the population recording should take.
    fn reachable_files(&self, root_doc: &Path) -> impl Future<Output = Result<BTreeSet<PathBuf>>>;

    /// The path prefixes that are prov's own bookkeeping rather than content —
    /// a byte-parking store's interior, a derived page. Excluded by decision,
    /// so the rules they produce say [`Reason::Bookkeeping`] rather than
    /// looking like an oversight.
    fn bookkeeping(&self, root_doc: &Path) -> impl Future<Output = Result<Vec<PathBuf>>>;

    /// Whether `rel_dir` is an archive a manifest document claims in bulk. Its
    /// rows are pinned by hash in a document the graph does reach, so the
    /// directory is one fact — skipped whole, never walked file by file.
    fn claimed(&self, rel_dir: &Path) -> impl Future<Output = Result<bool>>;
}

impl<T: SkipHost + ?Sized> SkipHost for &T {
    fn listing(&self, rel_dir: &Path) -> impl Future<Output = Result<Vec<DirEntry>>> {
        (**self).listing(rel_dir)
    }
    fn reachable_files(&self, root_doc: &Path) -> impl Future<Output = Result<BTreeSet<PathBuf>>> {
        (**self).reachable_files(root_doc)
    }
    fn bookkeeping(&self, root_doc: &Path) -> impl Future<Output = Result<Vec<PathBuf>>> {
        (**self).bookkeeping(root_doc)
    }
    fn claimed(&self, rel_dir: &Path) -> impl Future<Output = Result<bool>> {
        (**self).claimed(rel_dir)
    }
}
