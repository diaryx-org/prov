//! Traversal — materialize the spanning containment tree from a root document.
//!
//! This is the discovery walk the whole crate exists for: start at a document,
//! follow the spanning relation's links declared *in* each document, and the
//! workspace structure unfolds. The walk is resilient by design — a missing or
//! unparseable target becomes a marked node, not an error — because a
//! traversal that dies on the first broken link cannot power `tree`, `check`,
//! or any editor view of an imperfect (i.e. real) workspace.
//!
//! **Why this is a second walker, not a view over [`census`](super::census).**
//! The census is a flat BFS over a global `visited` set: once a path is
//! reached it is never redescended, and a spanning edge back into it is a
//! *finding* (a second parent breaking the single-parent tree). This walk is
//! a DFS over a per-branch `trail`: revisiting a node from another branch is
//! fine (each branch materializes its own subtree — that is what makes `tree`
//! a tree rather than a DAG rendered flat), and only a back-edge to an
//! *ancestor on the current path* is a cycle. Forcing one skeleton to serve
//! both would mean threading two different revisit policies through a single
//! traversal, which is more machinery than two short, separately-readable
//! walks. They stay side by side in `graph` because they walk the same edges
//! from the same [`Graph`], not because they
//! share a shape.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use super::Graph;
use crate::error::Result;
use crate::fs::ReadStorage;
use crate::index::IdIndex;
use crate::link::{self, Link};

use super::Target;

/// Why a node appears in the tree the way it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    /// A document that was read and parsed.
    Doc,
    /// A spanning target that does not exist on disk.
    Missing,
    /// A target already on the path from the root — a containment cycle. Not
    /// descended into.
    Cycle,
    /// A file that exists but could not be read or parsed; the message says why.
    Unreadable(String),
    /// An `id:<id>` target the registry does not currently resolve
    /// (unknown, tombstoned, or no registry attached).
    UnresolvedId(crate::identity::Id),
    /// A nominal (alias) target whose name several documents claim — a
    /// containment link that cannot be resolved to one child.
    AmbiguousAlias(String),
    /// An `id:<workspace>/<id>` target naming a document in another workspace.
    ///
    /// A leaf, always: the tree is *this* workspace's spanning walk, and prov
    /// has no map from a workspace name to a location to follow (see
    /// [`Target::Foreign`]). Shown rather
    /// than dropped, because the link is really declared and a reader deserves
    /// to see the structure leave the building.
    Foreign {
        workspace: String,
        id: crate::identity::Id,
    },
}

/// Options controlling how [`Graph::tree_with`] materializes a spanning
/// target that does not resolve on disk.
///
/// The default (`tree()`'s behavior) materializes a [`NodeKind::Missing`]
/// node for every such target, so a caller can report *which* link is broken.
/// Some callers instead want the tree to look exactly as if the dead link were
/// never declared — an editor's outline view, say, which has nothing useful to
/// render for a node with no title, no children, and no file. `ignore_missing`
/// is the additive escape hatch for that: it only ever *removes* nodes the
/// default would have included, so a workspace with no broken links traverses
/// identically either way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TreeOptions {
    /// When `true`, a spanning target that does not exist on disk is omitted
    /// from its parent's `children` entirely, rather than becoming a
    /// [`NodeKind::Missing`] node. Default: `false`.
    pub ignore_missing: bool,
}

/// One node of the materialized spanning tree.
#[derive(Debug, Clone)]
pub struct Node {
    /// Workspace-relative, normalized path — relative to [`Graph::root`],
    /// *not* fs-readable as-is. Join it onto the root with
    /// [`Graph::fs_path`] before handing it to a [`crate::fs::ReadStorage`]
    /// read; the raw form here is what makes a [`Node`] stable across a
    /// workspace re-rooted to a different directory.
    pub path: PathBuf,
    /// The document's `title` field, when present.
    pub title: Option<String>,
    /// The label the *parent's* link carried (`[label](path)`), when any.
    pub label: Option<String>,
    /// How this node was resolved.
    pub kind: NodeKind,
    /// Spanning children, in declaration order.
    pub children: Vec<Node>,
}

/// Whether a failed [`load`](Workspace::load) means "the target is not there"
/// — the [`NodeKind::Missing`] case — rather than "the target is there and
/// something about it went wrong". Both spellings of absent count: a storage
/// backend's `io::ErrorKind::NotFound`, and prov's own typed
/// [`Error::NotFound`](crate::error::Error::NotFound), which a backend that
/// reports absence structurally raises instead.
fn is_missing(error: &crate::error::Error) -> bool {
    match error {
        crate::error::Error::NotFound(_) => true,
        crate::error::Error::Io(e) => e.kind() == std::io::ErrorKind::NotFound,
        _ => false,
    }
}

/// The context one [`tree`](Graph::tree) walk carries unchanged from its root
/// to every leaf.
struct Walk<'a> {
    root: &'a Path,
    options: TreeOptions,
    parked: &'a [PathBuf],
}

impl<FS: ReadStorage, Ix: IdIndex> Graph<FS, Ix> {
    /// Materialize the spanning tree rooted at `start` (a workspace-relative
    /// path). Missing, unreadable, cyclic, unresolved-ID, and ambiguous-alias
    /// targets become marked nodes. `id:<id>` targets resolve through the
    /// registry; nominal (`[[My File]]`) targets resolve through the title
    /// index, built once for the whole walk so spanning alias links (a
    /// `contents: alias` vocabulary) descend like any other.
    pub async fn tree(&self, start: impl AsRef<Path>) -> Result<Node> {
        self.tree_with(start, TreeOptions::default()).await
    }

    /// Materialize the spanning tree rooted at `start`, as [`tree`](Self::tree),
    /// with [`TreeOptions`] controlling how an unresolved spanning target is
    /// represented. `TreeOptions::default()` is exactly `tree()`'s behavior.
    pub async fn tree_with(&self, start: impl AsRef<Path>, options: TreeOptions) -> Result<Node> {
        self.tree_within(start, options, &[]).await
    }

    /// [`tree_with`](Self::tree_with), told which directories are parked — see
    /// [`title_index_scoped`](Self::title_index_scoped).
    pub async fn tree_within(
        &self,
        start: impl AsRef<Path>,
        options: TreeOptions,
        parked: &[PathBuf],
    ) -> Result<Node> {
        // Two passes over the same documents whenever the workspace uses
        // `[[alias]]` links: the descent reads each node, and the title index it
        // builds on meeting the first alias reads every document in the reached
        // directories — most of them the same ones. Scoped here for the same
        // reason [`walk`](Self::walk) is: a caller should not have to know that
        // materializing a tree is more than one read of each document.
        let _scope = self.read_scope();
        let start = link::normalize(start);
        // The title index is built lazily — only if a nominal (`[[alias]]`) link
        // is actually encountered. A path/id workspace never needs it, so it never
        // pays for a full-workspace scan (which, at the root of a larger repo,
        // would read every file under `target/`, vendored trees, and the rest).
        let mut titles: Option<crate::title::TitleIndex> = None;
        let mut trail: Vec<PathBuf> = Vec::new();
        let root = start.clone();
        let cx = Walk {
            root: &root,
            options,
            parked,
        };
        self.tree_node(start, None, &cx, &mut titles, &mut trail)
            .await
    }

    /// What stays the same for every node of one walk: the root the title index
    /// is scoped to, the option controlling how an unresolved spanning target is
    /// rendered, and the directories whose interiors must not be indexed. Bundled
    /// rather than passed one by one because the recursion threads all three
    /// unchanged through every level.
    fn tree_node<'a>(
        &'a self,
        path: PathBuf,
        label: Option<String>,
        cx: &'a Walk<'a>,
        titles: &'a mut Option<crate::title::TitleIndex>,
        trail: &'a mut Vec<PathBuf>,
    ) -> Pin<Box<dyn Future<Output = Result<Node>> + 'a>> {
        Box::pin(async move {
            if trail.contains(&path) {
                return Ok(Node {
                    path,
                    title: None,
                    label,
                    kind: NodeKind::Cycle,
                    children: Vec::new(),
                });
            }
            // One read, not a stat and then a read: the open `load` performs
            // already answers "does this exist", and its `NotFound` is exactly
            // the `Missing` node a separate `try_exists` was asking for. The
            // stat was pure overhead on every node of every walk — and on the
            // memoized path it was the *only* syscall left, so a second pass
            // inside a `read_scope` paid it for nothing. Checking existence
            // first also meant stat-ing an escaping target (`../../etc/passwd`)
            // before `load`'s root clamp got to refuse it; now the clamp is
            // first.
            let doc = match self.load(&path).await {
                Ok((_, doc)) => doc,
                Err(e) if is_missing(&e) => {
                    return Ok(Node {
                        path,
                        title: None,
                        label,
                        kind: NodeKind::Missing,
                        children: Vec::new(),
                    });
                }
                Err(e) => {
                    return Ok(Node {
                        path,
                        title: None,
                        label,
                        kind: NodeKind::Unreadable(e.to_string()),
                        children: Vec::new(),
                    });
                }
            };
            let meta = fig::Value::from(&doc.meta);
            let title = meta
                .get("title")
                .and_then(fig::Value::as_str)
                .map(str::to_owned);

            trail.push(path.clone());
            let mut children = Vec::new();
            for raw in self.relations().children(&meta) {
                let child = Link::parse(&raw);
                // Build the title index on first sight of a nominal link, never
                // before — this is the only place the tree walk can need it.
                if titles.is_none() && crate::title::is_alias_shaped(&child.target) {
                    *titles = Some(self.title_index_scoped(cx.root, cx.parked).await?);
                }
                let child_path = match self.resolve_link_with(&path, &child, titles.as_ref()) {
                    // Neither names a document in this workspace, so neither can
                    // be a child: a URL leaves the building, and a bare `#3`
                    // never left this one.
                    Target::External | Target::SameDocument => continue,
                    Target::UnresolvedId(id) => {
                        children.push(Node {
                            path: PathBuf::from(child.target.clone()),
                            title: None,
                            label: child.label,
                            kind: NodeKind::UnresolvedId(id),
                            children: Vec::new(),
                        });
                        continue;
                    }
                    Target::AmbiguousAlias(name) => {
                        children.push(Node {
                            path: PathBuf::from(name.clone()),
                            title: None,
                            label: child.label,
                            kind: NodeKind::AmbiguousAlias(name),
                            children: Vec::new(),
                        });
                        continue;
                    }
                    Target::Foreign { workspace, id } => {
                        children.push(Node {
                            path: PathBuf::from(child.target.clone()),
                            title: None,
                            label: child.label,
                            kind: NodeKind::Foreign { workspace, id },
                            children: Vec::new(),
                        });
                        continue;
                    }
                    Target::Path(p) => p,
                };
                let child_node = self
                    .tree_node(child_path, child.label, cx, titles, trail)
                    .await?;
                // `ignore_missing` only ever removes what the default would have
                // included: a `Missing` child is dropped here rather than pushed,
                // so a caller who asked for it sees no trace of the dead link at
                // all, matching diaryx's traversal. Every other kind (including a
                // deeper `Missing` several levels down, which surfaced as `Doc`
                // with that descendant already filtered) is unaffected.
                if !(cx.options.ignore_missing && child_node.kind == NodeKind::Missing) {
                    children.push(child_node);
                }
                // (titles carried by &mut, so a nominal link deeper in the tree
                // reuses the index built above rather than rescanning.)
            }
            trail.pop();

            Ok(Node {
                path,
                title,
                label,
                kind: NodeKind::Doc,
                children,
            })
        })
    }
}

// These tests use YAML frontmatter fixtures, so they run under the `yaml` feature.
#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::*;
    use crate::exec::block_on;
    use crate::fs::StdFs;
    use crate::graph::ReadSettings;
    use crate::index::NoIndex;

    use prov_testkit::write;
    fn tempdir(tag: &str) -> PathBuf {
        prov_testkit::scratch("tree", tag)
    }

    #[test]
    fn walks_the_spanning_tree_with_labels_and_titles() {
        let dir = tempdir("walk");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- '[A](notes/a.md)'\n- missing.md\n---\n",
        );
        write(
            &dir,
            "notes/a.md",
            "---\ntitle: A\npart_of: ../index.md\n---\n",
        );

        let ws = Graph::new(StdFs, &dir, NoIndex, ReadSettings::default());
        let root = block_on(ws.tree("index.md")).unwrap();
        assert_eq!(root.title.as_deref(), Some("Root"));
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[0].path, PathBuf::from("notes/a.md"));
        assert_eq!(root.children[0].label.as_deref(), Some("A"));
        assert_eq!(root.children[0].kind, NodeKind::Doc);
        assert_eq!(root.children[1].kind, NodeKind::Missing);
    }

    #[test]
    fn spanning_alias_links_resolve_through_the_title_index() {
        // A workspace whose containment links are nominal `[[Title]]` aliases:
        // the walk must resolve them through the title index and descend, and
        // flag a name several documents share as ambiguous.
        let dir = tempdir("alias");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- '[[Alpha]]'\n- '[[Dup]]'\n- '[[Ghost]]'\n---\n",
        );
        write(&dir, "notes/alpha.md", "---\ntitle: Alpha\n---\n");
        write(&dir, "one.md", "---\ntitle: Dup\n---\n");
        write(&dir, "two.md", "---\ntitle: Dup\n---\n");

        let ws = Graph::new(StdFs, &dir, NoIndex, ReadSettings::default());
        let root = block_on(ws.tree("index.md")).unwrap();
        assert_eq!(root.children.len(), 3);

        // `[[Alpha]]` → the unique document titled Alpha, descended into.
        assert_eq!(root.children[0].kind, NodeKind::Doc);
        assert_eq!(root.children[0].path, PathBuf::from("notes/alpha.md"));

        // `[[Dup]]` → two documents claim the title, so it cannot resolve.
        assert_eq!(
            root.children[1].kind,
            NodeKind::AmbiguousAlias("Dup".into())
        );

        // `[[Ghost]]` → no document claims it; falls through to a missing path.
        assert_eq!(root.children[2].kind, NodeKind::Missing);
    }

    /// The walk asks for a document and reads the answer's *kind* — so the line
    /// between "not there" (`Missing`) and "there and wrong" (`Unreadable`) now
    /// lives in that one error match rather than in a preceding stat. Both
    /// sides of it, pinned: a directory exists but is not a document, and a
    /// target climbing out of the root is refused before it is opened at all.
    #[test]
    fn a_target_that_exists_but_cannot_be_read_is_unreadable_not_missing() {
        let dir = tempdir("unreadable");
        write(
            &dir,
            "index.md",
            "---\ncontents:\n- sub\n- ../outside.md\n---\n",
        );
        std::fs::create_dir_all(dir.join("sub")).unwrap();

        let ws = Graph::new(StdFs, &dir, NoIndex, ReadSettings::default());
        let root = block_on(ws.tree("index.md")).unwrap();
        assert_eq!(root.children.len(), 2);
        assert!(
            matches!(root.children[0].kind, NodeKind::Unreadable(_)),
            "a directory is not a missing document: {:?}",
            root.children[0].kind
        );
        assert!(
            matches!(root.children[1].kind, NodeKind::Unreadable(_)),
            "an escaping target is refused, not reported absent: {:?}",
            root.children[1].kind
        );
    }

    #[test]
    fn cycles_are_marked_not_followed() {
        let dir = tempdir("cycle");
        write(&dir, "a.md", "---\ncontents:\n- b.md\n---\n");
        write(&dir, "b.md", "---\ncontents:\n- a.md\n---\n");

        let ws = Graph::new(StdFs, &dir, NoIndex, ReadSettings::default());
        let root = block_on(ws.tree("a.md")).unwrap();
        let b = &root.children[0];
        assert_eq!(b.kind, NodeKind::Doc);
        assert_eq!(b.children[0].kind, NodeKind::Cycle);
        assert_eq!(b.children[0].path, PathBuf::from("a.md"));
    }

    #[test]
    fn default_tree_materializes_a_missing_node_for_a_broken_contents_link() {
        // `tree()` and `tree_with(TreeOptions::default())` must agree exactly —
        // the same fixture as `ignore_missing_drops_the_broken_link_entirely`
        // below, pinned against the default (unchanged) behavior.
        let dir = tempdir("missing-default");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- '[A](notes/a.md)'\n- gone.md\n---\n",
        );
        write(&dir, "notes/a.md", "---\ntitle: A\n---\n");

        let ws = Graph::new(StdFs, &dir, NoIndex, ReadSettings::default());
        let root = block_on(ws.tree("index.md")).unwrap();
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[1].kind, NodeKind::Missing);

        let root = block_on(ws.tree_with("index.md", TreeOptions::default())).unwrap();
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[1].kind, NodeKind::Missing);
    }

    #[test]
    fn ignore_missing_drops_the_broken_link_entirely() {
        let dir = tempdir("missing-ignore");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- '[A](notes/a.md)'\n- gone.md\n---\n",
        );
        write(&dir, "notes/a.md", "---\ntitle: A\n---\n");

        let ws = Graph::new(StdFs, &dir, NoIndex, ReadSettings::default());
        let options = TreeOptions {
            ignore_missing: true,
        };
        let root = block_on(ws.tree_with("index.md", options)).unwrap();
        // No trace of `gone.md` at all — not a `Missing` node, just absent.
        assert_eq!(root.children.len(), 1);
        assert_eq!(root.children[0].path, PathBuf::from("notes/a.md"));
    }

    #[test]
    fn ignore_missing_only_filters_missing_not_other_marker_kinds() {
        // A cycle is a different failure mode from a target that never existed;
        // `ignore_missing` must leave it alone.
        let dir = tempdir("missing-ignore-cycle");
        write(&dir, "a.md", "---\ncontents:\n- b.md\n- gone.md\n---\n");
        write(&dir, "b.md", "---\ncontents:\n- a.md\n---\n");

        let ws = Graph::new(StdFs, &dir, NoIndex, ReadSettings::default());
        let options = TreeOptions {
            ignore_missing: true,
        };
        let root = block_on(ws.tree_with("a.md", options)).unwrap();
        assert_eq!(root.children.len(), 1);
        let b = &root.children[0];
        assert_eq!(b.kind, NodeKind::Doc);
        assert_eq!(b.children.len(), 1);
        assert_eq!(b.children[0].kind, NodeKind::Cycle);
    }

    #[test]
    fn fs_path_joins_a_node_path_onto_the_workspace_root() {
        let dir = tempdir("fs-path");
        write(&dir, "notes/a.md", "---\ntitle: A\n---\n");

        let ws = Graph::new(StdFs, &dir, NoIndex, ReadSettings::default());
        let node = block_on(ws.tree("notes/a.md")).unwrap();
        assert_eq!(ws.fs_path(&node.path), dir.join("notes/a.md"));
    }
}
