//! History — what prov contributes to a workspace recorded by historica.
//!
//! prov workspaces are plaintext, and versioning plaintext is
//! [historica](https://crates.io/crates/historica)'s whole subject: revisions,
//! merge, amendment, forgetting, and a store a person can read. prov used to
//! carry a versioned safety net of its own — an event store of
//! whole-workspace manifests, with capture, restore, prune and forget verbs —
//! and that machinery is retired rather than maintained in parallel: a second
//! implementation of recording inside prov could only fall behind the real
//! one.
//!
//! What prov still knows, and historica deliberately does not, is **which
//! files are the workspace**: the graph's bounded reachable walk, where the
//! recycle bin parks consigned bytes, which page is derived rather than
//! authored, and which directories a manifest claims in bulk. That knowledge
//! has exactly one place to land in historica's model — `history/skipped.txt`,
//! the file that says what recording does not take — and [`prov_history`]
//! computes it: a generated region of skip rules, regenerated whole as the
//! graph changes, scoping the store to the graph. Recording itself stays with
//! the `historica` command.
//!
//! ## Where the code lives
//!
//! **The computation is [`prov_history`]** — the walk, the subtraction, the
//! region convention, and the store I/O through the historica library. It
//! reaches this workspace through one trait ([`SkipHost`]), implemented
//! below; nothing in that crate names `prov`.
//!
//! What is left here is the integration:
//!
//! - the [`SkipHost`] answers — reachability, the bookkeeping prefixes, the
//!   manifest claim — which are the workspace's policies, not history's;
//! - [`Workspace::skiplist`], the one forwarding method the CLI drives;
//! - the store's parking: a `history/` directory holding a historica marker
//!   is a byte-parking store to every workspace walk
//!   ([`parked_dirs`](Workspace::parked_dirs)), so the title index, the
//!   orphan sweep and reachability stay blind to its interior.
//!
//! ## Audience honesty
//!
//! When the transport is **git**, none of this is needed: git already stores
//! every pre-image and reconciles concurrent histories. A historica store
//! earns its keep where the transport keeps no history — Dropbox, Syncthing,
//! iCloud, a synced network share — which is why the `history` axis
//! ([`History`](crate::config::History)) defaults off and the skiplist is
//! only maintained where it says `manual`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::workspace::Workspace;
use prov_graph::error::Result;
use prov_graph::fs::DirEntry;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

pub use prov_history::{
    REGION_BEGIN, REGION_END, Reason, Rule, Skip, SkipHost, Skiplist, Standing, StandingError,
    apply,
};

/// A byte-parking store's directory — the parent of the index document that
/// names it. The recycle bin's `items/` hangs off its index this way, and the
/// retired history store's interior did too.
pub(crate) fn store_dir(store_index: &Path) -> PathBuf {
    store_index.parent().unwrap_or(Path::new("")).to_path_buf()
}

impl<FS: Storage, Id, Ix: IndexStore> SkipHost for Workspace<FS, Id, Ix> {
    async fn listing(&self, rel_dir: &Path) -> Result<Vec<DirEntry>> {
        self.listing(rel_dir).await
    }

    async fn reachable_files(&self, root_doc: &Path) -> Result<BTreeSet<PathBuf>> {
        self.reachable_files(root_doc).await
    }

    async fn bookkeeping(&self, root_doc: &Path) -> Result<Vec<PathBuf>> {
        // The knowledge the skiplist cannot work out for itself: where the
        // bin parks the bytes a person has already consigned, and which page
        // prov derives rather than the author writing. The about page is
        // *reachable* — its pointer is what keeps it from lying loose — and
        // excluded even so.
        let mut prefixes = Vec::new();
        if let Some(index) = self.recycle_bin_path(root_doc).await? {
            prefixes.push(store_dir(&index).join("items"));
        }
        if let Some(about) = self.about_path(root_doc).await? {
            prefixes.push(about);
        }
        Ok(prefixes)
    }

    async fn claimed(&self, rel_dir: &Path) -> Result<bool> {
        Ok(self.manifest_node_for(rel_dir).await?.is_some())
    }
}

impl<FS: Storage, Id, Ix: IndexStore> Workspace<FS, Id, Ix> {
    /// The skiplist this workspace's graph implies, against what the store
    /// already says.
    ///
    /// [`Standing`] is passed in rather than read here because the store is a
    /// real directory on a real disk, and this workspace may not be — the
    /// caller that has both (the CLI) reads it with [`Standing::read`] and
    /// applies the result with [`apply`].
    pub async fn skiplist(&self, root_doc: &Path, standing: &Standing) -> Result<Skiplist> {
        prov_history::skiplist(self, root_doc, standing).await
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests;
