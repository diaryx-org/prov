//! The host history's verbs run against, built to the traits and nothing more.
//!
//! [`HistoryReadHost`] and [`HistoryWriteHost`] are the whole of what this
//! crate may assume about the workspace hosting it, so the tests are owed a
//! host supplying exactly that and no more. `prov`'s `Workspace` is a much
//! larger object — an identity policy, a config layer, `check`, the recycle
//! bin, the generated about page — and running these tests through it would let
//! them quietly depend on things history is defined not to know about. A test
//! that can only be phrased in those terms is testing `prov`, and stays there.
//!
//! So this is a *deliberately* thin host: a [`Graph`] over the real filesystem,
//! the two authoring facts a store's own documents are written from, the
//! `history` axis, and a change-set boundary. Every method below is the
//! narrowest honest implementation of its trait method — except
//! [`commit`](TestHost::commit), which has to mirror a real host's ordering
//! rather than merely apply, and says why inline.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use prov_fixity::FixityCache;
use prov_graph::document::EmbedStyle;
use prov_graph::error::Result;
use prov_graph::fs::{Metadata, ReadStorage, StdFs};
use prov_graph::graph::{Graph, ReadSettings, Target};
use prov_graph::identity::Id;
use prov_graph::index::{Collision, IdIndex};
use prov_graph::link::{self, Link, LinkStyle};
use prov_store::fs::Storage;
use prov_store::index::{FileIndex, IndexStore};
use prov_transaction::{ChangeSet, FileOp};

use crate::{BLOBS_DIR, EVENTS_DIR, HistoryReadHost, HistoryWriteHost, store_dir};

/// A workspace as history sees one.
///
/// Generic over its filesystem so the read-counting tests can watch what a
/// capture actually reads; everything else uses [`StdFs`], because the store's
/// whole subject is bytes that are really on disk.
pub(super) struct TestHost<FS = StdFs> {
    graph: Graph<FS, FileIndex>,
    embed_style: EmbedStyle,
    default_embed_format: fig::Format,
    /// Whether this workspace's `history` axis is on — the one axis the store's
    /// own verbs never consult and [`findings`](crate::HistoryStore::findings)
    /// does. Every fixture here has a store on purpose, so `true` is the honest
    /// default and [`history_off`](Self::history_off) is the exception.
    captures: bool,
    /// Interior mutability because [`HistoryWriteHost::fixity_remember`] takes
    /// `&self`: a capture that learns a digest is not a mutation of the
    /// workspace, and the trait says so.
    fixity_cache: RefCell<Option<FixityCache>>,
    /// The path style the `history` pointer is authored in — `prov`'s own
    /// default (root-absolute), unless a test opts into relative with
    /// [`link_style`](Self::link_style).
    link_style: LinkStyle,
}

impl<FS> TestHost<FS> {
    /// A host over `fs` rooted at `root`, authoring in the conventional
    /// `---`-delimited YAML.
    pub(super) fn new(fs: FS, root: &Path) -> Self {
        Self::authoring(fs, root, EmbedStyle::Delimited, fig::Format::Yaml)
    }

    /// A host with a declared metadata embedding. The store's *content* grammar
    /// comes from the root document's own extension, so a caller wanting an
    /// HTML store writes an `index.html` root rather than naming one here.
    pub(super) fn authoring(
        fs: FS,
        root: &Path,
        embed_style: EmbedStyle,
        default_embed_format: fig::Format,
    ) -> Self {
        Self {
            graph: Graph::new(
                fs,
                root,
                FileIndex::new(default_embed_format),
                ReadSettings::default(),
            ),
            embed_style,
            default_embed_format,
            captures: true,
            fixity_cache: RefCell::new(None),
            link_style: LinkStyle::default(),
        }
    }

    /// The `history` axis off — for the one distinction that turns on it: a
    /// leftover store nobody declared is only a defect in a workspace that says
    /// it wants one.
    pub(super) fn history_off(mut self) -> Self {
        self.captures = false;
        self
    }

    /// Author the `history` pointer in a given path style — the axis a real
    /// workspace's config drives, exercised here without pulling in `prov`'s
    /// `Workspace`.
    pub(super) fn link_style(mut self, style: LinkStyle) -> Self {
        self.link_style = style;
        self
    }

    /// Register ids by hand — the tests that need one are testing history's
    /// *use* of the id column, and minting policy is the host's business.
    pub(super) fn index_mut(&mut self) -> &mut FileIndex {
        self.graph.index_mut()
    }

    pub(super) fn set_fixity_cache(&mut self, cache: Option<FixityCache>) {
        *self.fixity_cache.borrow_mut() = cache;
    }

    pub(super) fn take_fixity_cache(&mut self) -> Option<FixityCache> {
        self.fixity_cache.borrow_mut().take()
    }
}

impl<FS: ReadStorage> HistoryReadHost for TestHost<FS> {
    type Fs = FS;
    type Ix = FileIndex;

    fn graph(&self) -> &Graph<Self::Fs, Self::Ix> {
        &self.graph
    }

    fn embed_style(&self) -> EmbedStyle {
        self.embed_style
    }

    fn default_embed_format(&self) -> fig::Format {
        self.default_embed_format
    }

    fn history_captures(&self) -> bool {
        self.captures
    }

    fn history_relation(&self) -> Option<&str> {
        self.graph.relations().history_relation()
    }

    fn history_link_style(&self) -> LinkStyle {
        self.link_style
    }

    /// The first target of the `history` relation on the root, resolved — the
    /// same mechanic every structural pointer in a workspace uses.
    async fn history_path(&self, root_doc: &Path) -> Result<Option<PathBuf>> {
        let Some(relation) = self.history_relation().map(str::to_string) else {
            return Ok(None);
        };
        let root_doc = link::normalize(root_doc);
        let (_, doc) = self.graph.load(&root_doc).await?;
        let meta = fig::Value::from(&doc.meta);
        let Some(raw) = meta
            .get(relation.as_str())
            .map(prov_graph::meta::link_strings)
            .and_then(|targets| targets.into_iter().next())
        else {
            return Ok(None);
        };
        match self.graph.resolve_link(&root_doc, &Link::parse(&raw)) {
            Target::Path(path) => Ok(Some(path)),
            _ => Ok(None),
        }
    }

    /// The reachable walk with the store's interior parked, which is what makes
    /// `the_store_is_never_captured_into_itself` a property of the *walk*
    /// rather than of a filter applied afterwards.
    async fn reachable_files(&self, root_doc: &Path) -> Result<BTreeSet<PathBuf>> {
        let mut parked = Vec::new();
        if let Some(index) = self.history_path(root_doc).await? {
            parked.push(store_dir(&index).join(EVENTS_DIR));
            parked.push(store_dir(&index).join(BLOBS_DIR));
        }
        self.graph.reachable_files_within(root_doc, &parked).await
    }

    /// Nothing. A real host excludes its recycle bin's items and its generated
    /// about page here; both are knowledge history does not have, so the test
    /// that they are excluded is not history's to run.
    async fn history_exclusions(&self, _root_doc: &Path) -> Result<Vec<PathBuf>> {
        Ok(Vec::new())
    }

    /// Both directions, with the already-registered pair discounted — the
    /// policy a restore defers to rather than re-deriving from the index.
    fn registration_conflict(&self, id: &Id, path: &Path) -> Option<Collision> {
        if let Some(held_by) = self.graph.index().resolve(id)
            && held_by != path
        {
            return Some(Collision::Id {
                id: id.clone(),
                held_by,
            });
        }
        if let Some(held) = self.graph.index().id_for_path(path)
            && held != *id
        {
            return Some(Collision::Path {
                path: path.to_path_buf(),
                held,
            });
        }
        None
    }
}

impl<FS: Storage> HistoryWriteHost for TestHost<FS> {
    fn change(&mut self) -> ChangeSet {
        self.graph.index_mut().rollback();
        self.graph.index_mut().checkpoint();
        ChangeSet::new()
    }

    /// Land a change set the way a real host does, which is the part history's
    /// verbs are entitled to assume and `ChangeSet::apply` alone does not give
    /// them: the index is rebased onto the set (so a set rewriting the index's
    /// own home does not get clobbered by the index's write), its pending write
    /// is staged *last*, everything the set touches is dropped from the read
    /// memo and the fixity cache before it lands, and a failure rolls the index
    /// back rather than leaving it claiming a move that never happened.
    ///
    /// What is deliberately absent is pending-id stamping: history mints no
    /// ids, so a host that stamped them here would be lending these tests a
    /// guarantee no history verb ever asks for.
    async fn commit(&mut self, mut cs: ChangeSet) -> Result<()> {
        if let Err(e) = self.graph.index_mut().rebase(&cs) {
            self.graph.index_mut().rollback();
            return Err(e);
        }
        let staged_index = match self.graph.index_mut().pending_write() {
            Ok(Some((path, text))) => {
                cs.write(path, text);
                true
            }
            Ok(None) => false,
            Err(e) => {
                self.graph.index_mut().rollback();
                return Err(e);
            }
        };
        self.forget_written(&cs);
        match cs.apply(self.graph.fs(), self.graph.root()).await {
            Ok(()) => {
                self.graph.index_mut().committed(staged_index);
                Ok(())
            }
            Err(e) => {
                self.graph.index_mut().rollback();
                Err(e)
            }
        }
    }

    fn fixity_cached(&self, path: &Path, meta: &Metadata) -> Option<String> {
        self.fixity_cache
            .borrow()
            .as_ref()
            .and_then(|cache| cache.get(path, meta).map(str::to_string))
    }

    fn fixity_remember(&self, path: &Path, meta: &Metadata, hash: &str) {
        if let Some(cache) = self.fixity_cache.borrow_mut().as_mut() {
            cache.put(path, meta, hash);
        }
    }
}

impl<FS> TestHost<FS> {
    /// Everything a set touches stops being something the host remembers —
    /// *before* it lands, so a set that fails halfway leaves nothing behind
    /// claiming to know what is on disk.
    fn forget_written(&self, cs: &ChangeSet) {
        let mut memo = self.graph.memo_lock();
        let mut cache = self.fixity_cache.borrow_mut();
        let mut forget = |path: &Path| {
            memo.forget(path);
            if let Some(cache) = cache.as_mut() {
                cache.forget(path);
            }
        };
        for op in cs.ops() {
            match op {
                FileOp::Write { path, .. }
                | FileOp::Remove { path }
                | FileOp::CopyFrom { path, .. } => forget(path),
                FileOp::Rename { from, to } => {
                    forget(from);
                    forget(to);
                }
            }
        }
    }
}
