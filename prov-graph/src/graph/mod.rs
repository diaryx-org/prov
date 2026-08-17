//! Plain text → walkable graph — the crate's read core.
//!
//! Underneath everything else sits the **census**
//! ([`census`](Graph::census)): one traversal that
//! yields every forward link reachable from a root — frontmatter relation
//! edges *and* body `[[…]]` wikilinks alike — each tagged with where it is
//! written ([`LinkSite`]) and how it resolves ([`Resolution`]), plus the
//! [`StructuralFact`]s the same pass raises from traversal state (a document
//! that would not load, a broken single-parent invariant, and so on). Because
//! it is read straight from the documents, the census is *ground truth*:
//! `prov`'s `validate`'s findings, the
//! [`backlinks`](Graph::backlinks) map, and
//! reachability ([`reachable_files`](Graph::reachable_files),
//! [`reachable_documents`](Graph::reachable_documents))
//! are all views over it, and any stored index heals *toward* the census,
//! never the reverse.
//!
//! Alongside it sits [`tree`]'s materialized [`Node`] walk — the same edges,
//! but a spanning-only DFS that renders a `contents`/`part_of` outline rather
//! than a flat link census. See `tree`'s module doc for why it stays a
//! second walker instead of a view over the census.
//!
//! This is the plain-text-workspace promise (crate root docs) made concrete:
//! follow the links declared in a document's own metadata and body, and the
//! structure unfolds without a side channel — no cache to trust instead of the
//! documents themselves. `validate`'s findings and `mutate`'s inbound-rename
//! maintenance are both built on what is censused here; nothing above this
//! module re-derives an edge from anywhere but a document's own bytes.
//!
//! Housed here: the read primitive ([`load`]) every pass shares, link
//! resolution ([`resolve`], [`Target`]) built on top of it, the census types
//! with the spanning-tree walker that fills them in, and the [`tree`] walker.
//! They stay `impl`ed on [`Graph`] rather
//! than a graph type of its own, but they no longer *require* it: every
//! function here is bounded on [`ReadStorage`](crate::fs::ReadStorage) and
//! [`IdIndex`](crate::index::IdIndex) — the read halves of the two ports — and
//! on nothing else. That is a compiler-checked statement, not a convention: the
//! read core cannot write a byte or change a registration, because the traits
//! it is generic over have no method that could.
//!
//! Those two splits exist for a consumer that does not exist yet: a language
//! server, a renderer, a browser viewer — anything that must traverse a
//! workspace without the authority to change it, and without linking the
//! machinery that would. Narrowing the bounds is the step that proves such a
//! consumer is *possible*; extracting a `prov-graph` crate is the step that
//! makes it *cheap*, and follows from here as a file move rather than a
//! redesign.
//!
//! `graph` is also the crate's sole *surface* onto
//! [`ReadStorage`](crate::fs::ReadStorage): every other module reaches the
//! filesystem for reads through a `Workspace` method housed here rather than
//! calling `self.fs()` directly. Most of that surface is [`load`] — clamped
//! against root escape and served from the read-scope memo — but a handful of
//! call sites (existence checks, a directory listing, a raw byte read for
//! something that is not a document) never wanted the clamp or the memo; those
//! go through [`probe`]'s raw primitives instead.
//!
//! **What this module does not depend on.** `graph` imports the mechanism
//! layers below it — [`crate::document`], [`crate::link`], [`crate::title`],
//! [`crate::identity`], and the generic [`crate::index::IdIndex`] — but
//! never a *policy* module (`crate::config`, `crate::validate`,
//! `crate::about`). The census walk raises [`StructuralFact`]s rather than
//! `prov`'s `Finding`s for exactly this reason: `Finding`
//! is `validate`'s vocabulary, and a walker that constructed one directly
//! would pull that policy layer's whole enum (and its config-, fixity-, and
//! vocabulary-flavored variants) down into the read core. `validate::check`
//! derives each `Finding` from a `StructuralFact` or a [`CensusEntry`]'s
//! [`Resolution`] one for one — the walk already knows exactly what
//! happened; `validate` only names it.
//! [`resolve_link_with`](Graph::resolve_link_with)'s
//! only reach beyond a bare path/id resolver is [`crate::title::TitleIndex`], which
//! is itself a derived cache with no policy of its own (DESIGN §5) — the same
//! dependency [`census`] already carries. That is a stable seam, not a design
//! gap the coupling papers over, so no `Resolve` trait was introduced here: it
//! would exist only to abstract a single already-generic parameter
//! (`Ix: IdIndex`) and a self-contained cache type, and would cost a layer
//! of indirection for no dependency this module does not already own.
//!
//! [`crate::peer`] is not that trait arriving late. It abstracts a dependency
//! this module genuinely does not have — a map from a workspace *name* to a
//! location, which only a host holds — and nothing here consumes it: no method
//! below takes a resolver and [`Graph`] grows no third parameter for one.
//! Following a foreign reference is a step a caller takes after resolution
//! returns [`Target::Foreign`], so a traversal that never follows one is
//! unchanged by the port existing.

pub mod census;
pub mod load;
pub mod manifest;
pub mod probe;
pub mod resolve;
pub mod scan;
pub mod shadow;
pub mod tree;

pub use census::{Backlink, CensusEntry, LinkSite, Resolution, StructuralFact, inbound, invert};
pub use census::{Walk, reachable_set};
pub use resolve::Target;
pub use shadow::sidecar_candidates;
pub use tree::{Node, NodeKind, TreeOptions};

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::identity::IdStorage;
use crate::memo::{ReadMemo, ReadScope};
use crate::relation::RelationSet;

/// The settings a *read* of a workspace depends on — the whole of what
/// traversal needs to be told about the workspace it is traversing.
///
/// Three fields out of the ten a full workspace is configured with, and the cut
/// is not arbitrary: these are the only ones that change what a link *resolves
/// to*. [`relations`](Self::relations) says which metadata fields are edges at
/// all; [`workspace_id`](Self::workspace_id) is what lets a foreign
/// `id:<ws>/<id>` reference be recognized as pointing back here rather than
/// away; [`id_storage`](Self::id_storage) says whether a document's own
/// frontmatter is a place an id can be found. The other seven — link style,
/// reference style, embed format and style, fixity, history — govern how prov
/// *writes*, and a reader that never writes has no use for any of them.
#[derive(Debug, Clone)]
pub struct ReadSettings {
    /// The relation vocabulary: which metadata fields are links, and which one
    /// (if any) is the spanning relation the tree walk follows.
    pub relations: RelationSet,
    /// What this workspace calls itself — the qualifier a cross-workspace
    /// reference names it by. Empty means anonymous, so no `id:<ws>/<id>`
    /// reference can ever be recognized as pointing back here.
    pub workspace_id: String,
    /// Where a document's stable id is persisted, and so where resolution may
    /// look for one.
    pub id_storage: IdStorage,
}

impl Default for ReadSettings {
    fn default() -> Self {
        Self {
            relations: RelationSet::diaryx(),
            workspace_id: String::new(),
            id_storage: IdStorage::default(),
        }
    }
}

/// A readable workspace: a root, a filesystem to read it through, an id index
/// to resolve `id:` references against, and the [`ReadSettings`] that say how
/// its links are spelled.
///
/// This is the whole of what traversal needs, and — because `FS` is only ever
/// bounded by [`ReadStorage`](crate::fs::ReadStorage) and `Ix` by
/// [`IdIndex`](crate::index::IdIndex) — the whole of what it *can* do. There is
/// no method here that changes a byte on disk or a registration in the index,
/// and no way to add one without changing a trait bound in this crate.
///
/// `prov`'s `Workspace` owns one of these and forwards every read to it, adding
/// the identity policy, the change/journal machinery, and the config layer on
/// top. A consumer that only needs to *see* the workspace — a language server,
/// a renderer, a viewer — can hold a `Graph` directly and link none of that.
#[derive(Debug)]
pub struct Graph<FS, Ix> {
    fs: FS,
    root: PathBuf,
    index: Ix,
    settings: ReadSettings,
    /// What the current operation has already read — empty unless a
    /// [`read_scope`](Graph::read_scope) is open. See [`crate::memo`].
    ///
    /// Interior mutability because the passes that benefit take `&self`: a
    /// census is a read-only operation, and making it `&mut` to let it remember
    /// what it read would be the tail wagging the dog.
    memo: Mutex<ReadMemo>,
}

impl<FS, Ix> Graph<FS, Ix> {
    /// A graph over `fs`, rooted at `root`, resolving ids through `index`.
    pub fn new(fs: FS, root: impl Into<PathBuf>, index: Ix, settings: ReadSettings) -> Self {
        Self {
            fs,
            root: root.into(),
            index,
            settings,
            memo: Mutex::new(ReadMemo::default()),
        }
    }

    /// The underlying filesystem.
    pub fn fs(&self) -> &FS {
        &self.fs
    }

    /// The workspace root every path here is relative to.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The absolute path of a workspace-relative one — `root` joined to `rel`.
    ///
    /// The two path forms are deliberately kept apart: everything this crate
    /// returns ([`Node::path`], [`Target::Path`], a [`CensusEntry`]'s source) is
    /// workspace-relative and root-independent, so a graph can be re-rooted to a
    /// different directory without touching a single stored path. `fs_path` is
    /// the one place that independence is given up, for the caller that actually
    /// needs to open the file.
    pub fn fs_path(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.root.join(rel)
    }

    /// The id index `id:` references resolve through.
    pub fn index(&self) -> &Ix {
        &self.index
    }

    /// The id index, mutably — for an owner that also writes to it.
    pub fn index_mut(&mut self) -> &mut Ix {
        &mut self.index
    }

    /// The settings this graph reads by.
    pub fn settings(&self) -> &ReadSettings {
        &self.settings
    }

    /// The relation vocabulary — which metadata fields are links.
    pub fn relations(&self) -> &RelationSet {
        &self.settings.relations
    }

    /// What this workspace calls itself; empty means anonymous.
    pub fn workspace_id(&self) -> &str {
        &self.settings.workspace_id
    }

    /// Where a document's stable id is persisted.
    pub fn id_storage(&self) -> IdStorage {
        self.settings.id_storage
    }

    /// Open a read scope: within it, a document read twice is parsed once. See
    /// [`crate::memo`].
    pub fn read_scope(&self) -> ReadScope<'_> {
        ReadScope::open(&self.memo)
    }

    /// The memo itself, locked — for an owner that must forget what it wrote.
    pub fn memo_lock(&self) -> std::sync::MutexGuard<'_, ReadMemo> {
        crate::memo::lock(&self.memo)
    }

    pub(crate) fn memo_hit(&self, path: &Path) -> Option<(String, crate::document::Document)> {
        self.memo.lock().unwrap().get(path)
    }

    pub(crate) fn memo_remember(&self, path: &Path, text: &str, doc: &crate::document::Document) {
        self.memo.lock().unwrap().remember(path, text, doc);
    }
}

impl<FS: Clone, Ix: Clone> Clone for Graph<FS, Ix> {
    fn clone(&self) -> Self {
        Self::new(
            self.fs.clone(),
            self.root.clone(),
            self.index.clone(),
            self.settings.clone(),
        )
    }
}
