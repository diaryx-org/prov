//! The inverse of the link graph, kept across verbs.
//!
//! `retitle` and `rename` share one question — *which documents link here?* —
//! and the census answers it by reading every document reachable from the
//! spanning root, every call, whatever the answer turns out to be. On a
//! 2,000-document workspace that is seconds of coordinated reads for an edit
//! that writes two files, spent under whatever lock the consumer holds around
//! the verb. This module is the memo the census leaves behind: every reachable
//! document's resolved link targets, so that the next ask is a lookup.
//!
//! ## What it is not
//!
//! Not ground truth. The census is read from the documents and is always right
//! ([`Graph::census`](prov_graph::graph::Graph::census) says so); this heals
//! toward it, never the reverse, and [`backlinks`](super::Workspace::backlinks)
//! keeps reading the census rather than this. Not persisted either: the
//! `IndexStore`'s derived section (DESIGN §5) is where a stored inverse would
//! go, but a file every mutation rewrites is the contention hotspot §5 warns
//! about, and the CLI builds a fresh workspace per command. This lives on the
//! [`Workspace`] and dies with it.
//!
//! ## What invalidates it
//!
//! A memo with no end is a cache, and a cache has to be invalidated
//! ([`prov_graph::memo`]). Two writers can change what this remembers, and
//! each is handled where it is visible:
//!
//! - **prov's own writes** all land through
//!   [`apply_set`](super::Workspace::apply_set). Before a set lands, each
//!   document it rewrites is re-read *from the staged bytes* — no I/O — and
//!   its resolved targets replaced. A set that changes a document's
//!   **spanning** entries, writes a document this does not know, moves or
//!   removes anything, or carries a registry write, drops the whole index
//!   instead: any of those can change which documents are reachable or what
//!   an `id:` link resolves to, and that is the census's question to answer
//!   again. A set that fails drops it too, since forgetting is never wrong.
//! - **Writes prov did not make** — a consumer editing through its own
//!   filesystem handle, a sync bringing another device's edits in — are found
//!   by *stat*, not by being told. The index remembers each document's
//!   modification time and length as it was read; every ask stats every
//!   document it knows (and confirms every spanning target it knew to be
//!   missing is still missing) before trusting itself. A stat is not a read:
//!   on a coordinated filesystem nothing is opened and nothing downloads, and
//!   locally two thousand of them cost milliseconds where the reads cost
//!   seconds. Any change, and the index is dropped and the census runs.
//!
//! The one write this cannot see is a rewrite that lands in the same
//! modification-time tick as the read it followed, leaving the file the same
//! length — the resolution of the tick being the backend's (nanoseconds on
//! APFS, milliseconds through diaryx's coordinated storage). What it would
//! miss is one relabel or one retarget, which `check` reports as a stale
//! label or a broken link and `fix` repairs; the change set's expectations
//! still refuse to overwrite the racing edit itself. A backend that reports
//! no modification time at all gets no index, and pays the census each time
//! as before.
//!
//! ## Why the census builds it
//!
//! The first ask still runs the census, and the index is derived from *that*
//! walk rather than from a walk of its own — which documents are reachable is
//! one definition (§8), and a second traversal here would be the drift
//! [`Workspace`] is written to prevent. Each reachable document's targets are
//! then resolved through [`resolve_link`](super::Workspace::resolve_link),
//! the resolver the verbs' per-document rewrites already filter on, so the
//! set of sources the index names is exactly the set those rewrites would
//! have touched; the census's own resolution is not reused because it
//! differs in ways the rewrites ignore (alias links through the title index).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use prov_graph::document::Document;
use prov_graph::error::Result;
use prov_graph::fs::ReadStorage;
use prov_graph::graph::{CensusEntry, Target};
use prov_graph::index::IdIndex;
use prov_graph::link::{self, Link};
use prov_graph::memo::lock;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

use super::Workspace;
use crate::change::{ChangeSet, FileOp};
use crate::mutate::maintain::Moves;

/// Which inbound links an ask is after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Form {
    /// Path-form links only — what a move rewrites; `id:` links are left to
    /// the registry.
    Path,
    /// Path- and id-form alike — what a retitle relabels, the label being the
    /// same human title either way.
    Any,
}

/// How one document reaches one target: by a path, by an id, or both.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Forms {
    by_path: bool,
    by_id: bool,
}

impl Forms {
    fn add(&mut self, link: &Link) {
        if link.is_path_target() {
            self.by_path = true;
        } else if link.id_ref().is_some() {
            self.by_id = true;
        }
    }

    fn matches(self, form: Form) -> bool {
        match form {
            Form::Path => self.by_path,
            Form::Any => self.by_path || self.by_id,
        }
    }
}

/// What a document was when it was read: enough to notice, without reading
/// it again, that it is not that any more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    modified: SystemTime,
    len: u64,
}

/// One reachable document's resolved links.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DocEdges {
    /// Where its spanning entries point — the structure test: a write that
    /// changes these may change what is reachable, and drops the index.
    spanning: BTreeSet<PathBuf>,
    /// Every path any of its links resolves to, and by which forms.
    targets: BTreeMap<PathBuf, Forms>,
}

#[derive(Debug, Clone)]
struct IndexedDoc {
    /// `None` when the backend could not say when the document changed — an
    /// index holding one of these answers this ask and is not kept.
    stamp: Option<Stamp>,
    edges: DocEdges,
}

/// The inverse link graph of everything reachable from one root, with what
/// it would take to notice it is stale.
#[derive(Debug, Clone)]
pub(crate) struct InboundIndex {
    /// Every document the census read, keyed by normalized workspace path.
    docs: BTreeMap<PathBuf, IndexedDoc>,
    /// Spanning targets that resolved to nothing on disk when the census ran.
    /// If one appears, a subtree may have come with it — so these are checked
    /// alongside the stamps.
    absent: BTreeSet<PathBuf>,
}

impl InboundIndex {
    /// For every document with a link in `form` to a path `moves` relocates:
    /// the source, and the moved paths it reaches. A document's reference to
    /// itself is left out — a move never maintains those.
    fn sources_of(&self, moves: &Moves, form: Form) -> BTreeMap<PathBuf, BTreeSet<PathBuf>> {
        let mut out: BTreeMap<PathBuf, BTreeSet<PathBuf>> = BTreeMap::new();
        for (source, doc) in &self.docs {
            let reached: BTreeSet<PathBuf> = doc
                .edges
                .targets
                .iter()
                .filter(|(target, forms)| {
                    target != &source && forms.matches(form) && moves.landed(target).is_some()
                })
                .map(|(target, _)| target.clone())
                .collect();
            if !reached.is_empty() {
                out.insert(source.clone(), reached);
            }
        }
        out
    }
}

/// What [`apply_set`](Workspace::apply_set) will do to the index once the
/// set lands, decided from the staged bytes before it does.
pub(crate) enum InboundPlan {
    /// No index to keep, or nothing in the set concerns it.
    Nothing,
    /// The set only rewrote documents the index knows, without changing their
    /// spanning entries: these are their new edges, to install with a fresh
    /// stamp once they are on disk.
    Update(Vec<(PathBuf, DocEdges)>),
    /// The set may have changed what is reachable or how an id resolves.
    Drop,
}

/// The read-side half — resolving one document's links — needs no more than
/// the graph, so it sits outside the mutation bounds.
impl<FS: ReadStorage, Id, Ix: IdIndex> Workspace<FS, Id, Ix> {
    /// The links one document declares, resolved the way the verbs'
    /// per-document rewrites resolve them: path and id targets through
    /// [`resolve_link`](Self::resolve_link), aliases left unresolved — the
    /// population the census scans, filtered by the same resolver the rewrites
    /// filter on, **plus images**. The census leaves an image out because it
    /// names a payload rather than a document, so it is no edge of the graph;
    /// but a payload moves — beside its sidecar, or inside a directory — and
    /// the page embedding it is then exactly a source the move must rewrite.
    /// This is the inverse the rewrites consult, so it counts what they carry.
    fn edges_of(&self, path: &Path, doc: &Document) -> DocEdges {
        let spanning = self.relations().spanning_relation();
        let meta = fig::Value::from(&doc.meta);
        let mut edges = DocEdges::default();
        for edge in self.relations().edges(&meta) {
            let link = Link::parse(&edge.target);
            let Target::Path(target) = self.resolve_link(path, &link) else {
                continue;
            };
            if Some(edge.relation.as_str()) == spanning {
                edges.spanning.insert(target.clone());
            }
            edges.targets.entry(target).or_default().add(&link);
        }
        for body in link::scan_body_links(path, &doc.body) {
            if let Target::Path(target) = self.resolve_link(path, &body.link) {
                edges.targets.entry(target).or_default().add(&body.link);
            }
        }
        edges
    }

    /// The stamp of the document at workspace-relative `path`, or `None` when
    /// the backend cannot say when it changed — in which case nothing about it
    /// can be remembered safely.
    async fn stamp(&self, path: &Path) -> Result<Option<Stamp>> {
        let meta = self.fs().metadata(&self.fs_path(path)).await?;
        Ok(meta.modified().ok().map(|modified| Stamp {
            modified,
            len: meta.len(),
        }))
    }

    /// Whether every document the index remembers is as it was remembered,
    /// and every spanning target it found missing is still missing. Stats
    /// only; nothing is read.
    async fn still_fresh(&self, index: &InboundIndex) -> Result<bool> {
        for (path, doc) in &index.docs {
            if doc.stamp.is_none() || self.stamp(path).await.ok().flatten() != doc.stamp {
                return Ok(false);
            }
        }
        for path in &index.absent {
            if self.exists(path).await? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

impl<FS: Storage, IdP, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Every reachable document with a link to `target` in `form`, `target`
    /// itself excluded — the sources a retitle relabels.
    ///
    /// [`inbound_sources_of`](Self::inbound_sources_of) for one target, which
    /// is also the anchor: a retitle's subject is a document in the tree.
    pub(crate) async fn inbound_sources(
        &self,
        target: &Path,
        form: Form,
    ) -> Result<BTreeSet<PathBuf>> {
        let target = link::normalize(target);
        let by_source = self
            .inbound_sources_of(&target, &Moves::one(&target, &target), form)
            .await?;
        Ok(by_source.into_keys().collect())
    }

    /// Every reachable document with a link in `form` to a path `moves`
    /// relocates, with the moved paths it reaches — the sources a move
    /// retargets, each source's own path excluded from what it reaches.
    ///
    /// `anchor` is a document in the spanning tree the answer must cover.
    /// Answered from the index when it knows `anchor` and a stat sweep finds
    /// it fresh; otherwise from a census of the tree `anchor` sits in, whose
    /// inverse is then kept for the next ask. Either way the answer is the
    /// census's. The anchor is asked for rather than derived from the moves
    /// because a moved path need not be a document at all — a payload is
    /// reached by an image and lives in no tree — and because a directory
    /// move names no single document.
    pub(crate) async fn inbound_sources_of(
        &self,
        anchor: &Path,
        moves: &Moves,
        form: Form,
    ) -> Result<BTreeMap<PathBuf, BTreeSet<PathBuf>>> {
        let anchor = link::normalize(anchor);
        if let Some(sources) = self.inbound_from_index(&anchor, moves, form).await? {
            return Ok(sources);
        }
        let _scope = self.read_scope();
        let (_spanning, inverse) = self.spanning_pair()?;
        let root = self.spanning_root(&anchor, &inverse).await?;
        let census = self.census(&root).await?;
        let (index, stamped) = self.index_census(&root, &census).await?;
        let sources = index.sources_of(moves, form);
        // A backend with no modification times leaves nothing to validate
        // against, so nothing is kept: the next ask is a census again.
        *lock(&self.inbound) = stamped.then_some(index);
        Ok(sources)
    }

    /// The index's answer for `moves`, if it covers `anchor`'s tree and can
    /// still vouch for itself.
    async fn inbound_from_index(
        &self,
        anchor: &Path,
        moves: &Moves,
        form: Form,
    ) -> Result<Option<BTreeMap<PathBuf, BTreeSet<PathBuf>>>> {
        // Cloned out rather than held: the sweep awaits, and no lock is ever
        // held across an await.
        let Some(index) = lock(&self.inbound).clone() else {
            return Ok(None);
        };
        if !index.docs.contains_key(anchor) {
            return Ok(None);
        }
        if !self.still_fresh(&index).await? {
            *lock(&self.inbound) = None;
            return Ok(None);
        }
        Ok(Some(index.sources_of(moves, form)))
    }

    /// The inverse of a census just taken from `root`, stamped. Reads nothing
    /// the census did not: each reachable document is loaded through the scope
    /// the caller holds, so it comes back from the memo.
    ///
    /// The second value is whether every document could be stamped; when it
    /// could not, the index is still a correct answer for this ask but must
    /// not be kept.
    async fn index_census(
        &self,
        root: &Path,
        census: &[CensusEntry],
    ) -> Result<(InboundIndex, bool)> {
        // What the walk visited: the root, and every spanning target that
        // resolved to a document. A document with no links of its own is not
        // a census source, but it is a document that could gain one.
        let spanning = self.relations().spanning_relation();
        let mut visited: BTreeSet<PathBuf> = BTreeSet::new();
        visited.insert(link::normalize(root));
        for entry in census {
            if entry.site.relation().is_some()
                && entry.site.relation() == spanning
                && let Some(target) = entry.resolution.resolved_path()
            {
                visited.insert(target.clone());
            }
        }

        let mut stamped = true;
        let mut docs = BTreeMap::new();
        let mut absent = BTreeSet::new();
        for path in visited {
            // A document the walk could not read has no links to remember,
            // and a stamp that will change when it is repaired.
            let edges = match self.load(&path).await {
                Ok((_, doc)) => self.edges_of(&path, &doc),
                Err(_) => DocEdges::default(),
            };
            let stamp = self.stamp(&path).await.ok().flatten();
            stamped &= stamp.is_some();
            absent.extend(edges.spanning.iter().cloned());
            docs.insert(path, IndexedDoc { stamp, edges });
        }
        absent.retain(|path| !docs.contains_key(path));
        Ok((InboundIndex { docs, absent }, stamped))
    }

    /// Decide what landing `cs` will do to the index, from the staged bytes.
    /// Called before the apply, so the decision is made against the index the
    /// set was computed over.
    pub(crate) fn plan_inbound(&self, cs: &ChangeSet) -> InboundPlan {
        let guard = lock(&self.inbound);
        let Some(index) = guard.as_ref() else {
            return InboundPlan::Nothing;
        };
        let mut updates = Vec::new();
        for op in cs.ops() {
            match op {
                FileOp::Write { path, bytes } => {
                    let path = link::normalize(path);
                    let Some(known) = index.docs.get(&path) else {
                        return InboundPlan::Drop;
                    };
                    let parsed = std::str::from_utf8(bytes)
                        .ok()
                        .and_then(|text| Document::parse(&path, text).ok());
                    let Some(doc) = parsed else {
                        return InboundPlan::Drop;
                    };
                    let edges = self.edges_of(&path, &doc);
                    if edges.spanning != known.edges.spanning {
                        return InboundPlan::Drop;
                    }
                    updates.push((path, edges));
                }
                // A mode bit changes no link.
                FileOp::SetExecutable { .. } => {}
                _ => return InboundPlan::Drop,
            }
        }
        if updates.is_empty() {
            InboundPlan::Nothing
        } else {
            InboundPlan::Update(updates)
        }
    }

    /// Carry out a [`plan_inbound`](Self::plan_inbound) decision now that its
    /// set has landed: install the rewritten documents' edges under the stamps
    /// they now carry on disk, or drop the index.
    pub(crate) async fn settle_inbound(&self, plan: InboundPlan) {
        let updates = match plan {
            InboundPlan::Nothing => return,
            InboundPlan::Drop => {
                self.forget_inbound();
                return;
            }
            InboundPlan::Update(updates) => updates,
        };
        let mut stamps = Vec::with_capacity(updates.len());
        for (path, edges) in updates {
            match self.stamp(&path).await.ok().flatten() {
                Some(stamp) => stamps.push((
                    path,
                    IndexedDoc {
                        stamp: Some(stamp),
                        edges,
                    },
                )),
                None => {
                    self.forget_inbound();
                    return;
                }
            }
        }
        if let Some(index) = lock(&self.inbound).as_mut() {
            for (path, doc) in stamps {
                index.docs.insert(path, doc);
            }
        }
    }

    /// Drop the index. Never the wrong answer: the next ask is a census.
    pub(crate) fn forget_inbound(&self) {
        *lock(&self.inbound) = None;
    }
}

/// The field's starting state, for the builder and for a clone — which
/// inherits nothing remembered, exactly as it inherits an empty read memo.
pub(crate) fn empty() -> Mutex<Option<InboundIndex>> {
    Mutex::new(None)
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::*;
    use crate::fs_faults::CountingFs;
    use crate::identity::Minter;
    use prov_graph::exec::block_on;
    use prov_store::index::FileIndex;
    use prov_testkit::{read, scratch, write};

    /// A root with three children, `c.md` linked by its parent (a labeled
    /// `contents` entry) and by `a.md` (a labeled `links` entry); `b.md`
    /// links to nothing but its parent.
    fn tree(tag: &str) -> PathBuf {
        let dir = scratch("inbound", tag);
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- '[A](a.md)'\n- '[B](b.md)'\n- '[C](c.md)'\n---\n",
        );
        write(
            &dir,
            "a.md",
            "---\ntitle: A\npart_of: '[Root](index.md)'\nlinks:\n- '[C](c.md)'\n---\n",
        );
        write(
            &dir,
            "b.md",
            "---\ntitle: B\npart_of: '[Root](index.md)'\n---\n",
        );
        write(
            &dir,
            "c.md",
            "---\ntitle: C\npart_of: '[Root](index.md)'\n---\n",
        );
        dir
    }

    fn ws(dir: &Path, fs: CountingFs) -> Workspace<CountingFs, Minter, FileIndex> {
        Workspace::builder(fs)
            .root(dir)
            .identity(Minter::lazy(3))
            .index(FileIndex::new(fig::Format::Yaml))
            .build()
    }

    /// The task's done state: a second retitle on an unchanged workspace
    /// reads no document it does not write.
    #[test]
    fn a_second_retitle_reads_only_what_it_writes() {
        let dir = tree("second-retitle");
        let fs = CountingFs::default();
        let mut w = ws(&dir, fs.clone());

        // The first retitle censuses: every document read once.
        assert_eq!(block_on(w.retitle(Path::new("c.md"), "C two")).unwrap(), 2);
        let after_first: Vec<usize> = ["index.md", "a.md", "b.md", "c.md"]
            .iter()
            .map(|p| fs.doc_reads(&dir, p))
            .collect();
        assert_eq!(after_first, vec![1, 1, 1, 1], "the first ask is a census");

        // The second writes c.md, index.md and a.md — and reads exactly those.
        assert_eq!(
            block_on(w.retitle(Path::new("c.md"), "C three")).unwrap(),
            2
        );
        assert_eq!(fs.doc_reads(&dir, "c.md"), 2, "the document retitled");
        assert_eq!(fs.doc_reads(&dir, "index.md"), 2, "a relabeled source");
        assert_eq!(fs.doc_reads(&dir, "a.md"), 2, "a relabeled source");
        assert_eq!(
            fs.doc_reads(&dir, "b.md"),
            1,
            "a document that links nowhere near c.md was read again"
        );
        assert!(read(&dir, "index.md").contains("[C three](c.md)"));
        assert!(read(&dir, "a.md").contains("[C three](c.md)"));
    }

    /// `rename` asks the same question, so a rename after a retitle finds
    /// its inbound set without a census of its own.
    #[test]
    fn rename_shares_the_index() {
        let dir = tree("rename-shares");
        let fs = CountingFs::default();
        let mut w = ws(&dir, fs.clone());

        assert_eq!(block_on(w.retitle(Path::new("c.md"), "C two")).unwrap(), 2);
        block_on(w.rename(Path::new("c.md"), Path::new("d.md"))).unwrap();
        assert_eq!(
            fs.doc_reads(&dir, "b.md"),
            1,
            "not an inbound source; not re-read"
        );
        assert!(read(&dir, "index.md").contains("[C two](/d.md)"));
        assert!(read(&dir, "a.md").contains("[C two](/d.md)"));

        // A move changes the structure, so the index is gone; the next ask
        // censuses again and reads b.md.
        assert_eq!(block_on(w.retitle(Path::new("d.md"), "D")).unwrap(), 2);
        assert_eq!(fs.doc_reads(&dir, "b.md"), 2, "a rename drops the index");
    }

    /// A write prov did not make is noticed by stat: a document edited behind
    /// prov's back to link at the target is relabeled all the same.
    #[test]
    fn an_out_of_band_edit_is_seen() {
        let dir = tree("out-of-band");
        let fs = CountingFs::default();
        let mut w = ws(&dir, fs.clone());
        assert_eq!(block_on(w.retitle(Path::new("c.md"), "C two")).unwrap(), 2);

        // Not through the workspace: b.md gains a labeled link to c.md.
        write(
            &dir,
            "b.md",
            "---\ntitle: B\npart_of: '[Root](index.md)'\nlinks:\n- '[C](c.md)'\n---\n",
        );
        assert_eq!(
            block_on(w.retitle(Path::new("c.md"), "C three")).unwrap(),
            3,
            "the edit behind prov's back was not seen"
        );
        assert!(read(&dir, "b.md").contains("[C three](c.md)"));
    }

    /// A child the parent already lists but which does not exist yet is
    /// watched too: when it appears — out of band — its subtree is censused.
    #[test]
    fn a_missing_child_appearing_is_seen() {
        let dir = scratch("inbound", "absent-child");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- '[C](c.md)'\n- '[Late](late.md)'\n---\n",
        );
        write(
            &dir,
            "c.md",
            "---\ntitle: C\npart_of: '[Root](index.md)'\n---\n",
        );
        let fs = CountingFs::default();
        let mut w = ws(&dir, fs.clone());
        assert_eq!(block_on(w.retitle(Path::new("c.md"), "C two")).unwrap(), 1);

        write(
            &dir,
            "late.md",
            "---\ntitle: Late\npart_of: '[Root](index.md)'\nlinks:\n- '[C](c.md)'\n---\n",
        );
        assert_eq!(
            block_on(w.retitle(Path::new("c.md"), "C three")).unwrap(),
            2,
            "a spanning child that came into being was not censused"
        );
        assert!(read(&dir, "late.md").contains("[C three](c.md)"));
    }

    /// A structural change through prov — a new child — drops the index, and
    /// the next ask censuses it in; a content save through prov updates the
    /// saved document's edges in place.
    #[test]
    fn writes_through_prov_are_seen() {
        let dir = tree("create");
        let fs = CountingFs::default();
        let mut w = ws(&dir, fs.clone());
        assert_eq!(block_on(w.retitle(Path::new("c.md"), "C two")).unwrap(), 2);

        block_on(w.create_with_title(Path::new("d.md"), Path::new("index.md"), "D")).unwrap();
        assert_eq!(
            block_on(w.retitle(Path::new("c.md"), "C three")).unwrap(),
            2,
            "the new child links nowhere yet"
        );

        // A save through prov of a document the index knows: its edges are
        // replaced in place, without a census.
        let text = read(&dir, "d.md");
        let linked = text.replacen("title: D\n", "title: D\nlinks:\n- '[C](c.md)'\n", 1);
        assert_ne!(linked, text);
        block_on(w.save_document("d.md", &linked, None)).unwrap();
        let before = fs.doc_reads(&dir, "b.md");
        assert_eq!(
            block_on(w.retitle(Path::new("c.md"), "C four")).unwrap(),
            3,
            "a link saved through prov was not seen"
        );
        assert_eq!(fs.doc_reads(&dir, "b.md"), before, "a save is not a census");
        assert!(read(&dir, "d.md").contains("[C four](c.md)"));
    }
}
