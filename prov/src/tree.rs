//! Traversal — materialize the spanning containment tree from a root document.
//!
//! This is the discovery walk the whole crate exists for: start at a document,
//! follow the spanning relation's links declared *in* each document, and the
//! workspace structure unfolds. The walk is resilient by design — a missing or
//! unparseable target becomes a marked node, not an error — because a
//! traversal that dies on the first broken link cannot power `tree`, `check`,
//! or any editor view of an imperfect (i.e. real) workspace.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use crate::document::Document;
use crate::error::Result;
use crate::fs::Storage;
use crate::index::IndexStore;
use crate::link::{self, Link};
use crate::meta::Value;
use crate::workspace::{Target, Workspace};

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
    /// [`Target::Foreign`](crate::workspace::Target::Foreign)). Shown rather
    /// than dropped, because the link is really declared and a reader deserves
    /// to see the structure leave the building.
    Foreign {
        workspace: String,
        id: crate::identity::Id,
    },
}

/// Options controlling how [`Workspace::tree_with`] materializes a spanning
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
    /// Workspace-relative, normalized path — relative to [`Workspace::root`],
    /// *not* fs-readable as-is. Join it onto the root with
    /// [`Workspace::fs_path`] before handing it to a [`Storage`](crate::fs::Storage)
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

impl<FS: Storage, Id, Ix: IndexStore> Workspace<FS, Id, Ix> {
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
        let start = link::normalize(start);
        // The title index is built lazily — only if a nominal (`[[alias]]`) link
        // is actually encountered. A path/id workspace never needs it, so it never
        // pays for a full-workspace scan (which, at the root of a larger repo,
        // would read every file under `target/`, vendored trees, and the rest).
        let mut titles: Option<crate::title::TitleIndex> = None;
        let mut trail: Vec<PathBuf> = Vec::new();
        let root = start.clone();
        self.tree_node(start, None, &root, &mut titles, &mut trail, options)
            .await
    }

    /// Read and parse the workspace-relative document at `path`, returning the
    /// raw text alongside. The building block traversal, validation, and
    /// mutation share.
    pub(crate) async fn load(&self, path: &Path) -> Result<(String, Document)> {
        // Clamp reads to the workspace root: `path` may originate in a document's
        // own metadata (a `contents`/`part_of` target), so a hostile or careless
        // `../../../etc/passwd` must be refused here rather than opened. The
        // traversal turns this error into an `Unreadable` node; a direct caller
        // sees the `Escape` error itself.
        if link::escapes_root(path) {
            return Err(crate::error::Error::Escape(path.to_path_buf()));
        }
        // Inside a `read_scope`, a document already read this operation is
        // answered from memory — the escape check above still runs first, so a
        // memo can never be the thing that lets a hostile path through.
        if let Some(hit) = self.memo_hit(path) {
            return Ok(hit);
        }
        let text = self.fs().read_to_string(&self.root().join(path)).await?;
        let doc = Document::parse(path, &text)?;
        self.memo_remember(path, &text, &doc);
        Ok((text, doc))
    }

    /// Read and parse the workspace-relative document at `path`, returning its
    /// full [`Document`] — the public counterpart to [`load`](Self::load), for
    /// a caller walking a [`Node`] tree who needs more than [`Node::title`]
    /// (the rest of the frontmatter, the body, the carrier) without re-reading
    /// and re-parsing the file by hand.
    ///
    /// Unlike the traversal, which degrades a bad target to a
    /// [`NodeKind::Unreadable`] node, this surfaces the [`Error`](crate::error::Error)
    /// directly — a caller who names a path expects to know why it failed, not
    /// to receive a placeholder.
    pub async fn document(&self, path: impl AsRef<Path>) -> Result<Document> {
        let path = link::normalize(path);
        self.load(&path).await.map(|(_, doc)| doc)
    }

    fn tree_node<'a>(
        &'a self,
        path: PathBuf,
        label: Option<String>,
        root: &'a Path,
        titles: &'a mut Option<crate::title::TitleIndex>,
        trail: &'a mut Vec<PathBuf>,
        options: TreeOptions,
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
            match self.fs().try_exists(&self.root().join(&path)).await {
                Ok(true) => {}
                Ok(false) => {
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
            }
            let doc = match self.load(&path).await {
                Ok((_, doc)) => doc,
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
            let title = doc
                .meta
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_owned);

            trail.push(path.clone());
            let mut children = Vec::new();
            for raw in self.relations().children(&doc.meta) {
                let child = Link::parse(&raw);
                // Build the title index on first sight of a nominal link, never
                // before — this is the only place the tree walk can need it.
                if titles.is_none() && crate::title::is_alias_shaped(&child.target) {
                    *titles = Some(self.title_index_scoped(root).await?);
                }
                let child_path = match self.resolve_link_with(&path, &child, titles.as_ref()) {
                    Target::External => continue,
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
                    .tree_node(child_path, child.label, root, titles, trail, options)
                    .await?;
                // `ignore_missing` only ever removes what the default would have
                // included: a `Missing` child is dropped here rather than pushed,
                // so a caller who asked for it sees no trace of the dead link at
                // all, matching diaryx's traversal. Every other kind (including a
                // deeper `Missing` several levels down, which surfaced as `Doc`
                // with that descendant already filtered) is unaffected.
                if !(options.ignore_missing && child_node.kind == NodeKind::Missing) {
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

    fn write(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prov-tree-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
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

        let ws = Workspace::builder(StdFs).root(&dir).build();
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

        let ws = Workspace::builder(StdFs).root(&dir).build();
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

    #[test]
    fn cycles_are_marked_not_followed() {
        let dir = tempdir("cycle");
        write(&dir, "a.md", "---\ncontents:\n- b.md\n---\n");
        write(&dir, "b.md", "---\ncontents:\n- a.md\n---\n");

        let ws = Workspace::builder(StdFs).root(&dir).build();
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

        let ws = Workspace::builder(StdFs).root(&dir).build();
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

        let ws = Workspace::builder(StdFs).root(&dir).build();
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

        let ws = Workspace::builder(StdFs).root(&dir).build();
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
    fn document_reads_full_metadata_for_a_workspace_relative_path() {
        let dir = tempdir("document");
        write(
            &dir,
            "notes/a.md",
            "---\ntitle: A\nauthor: Ada\n---\nbody text\n",
        );

        let ws = Workspace::builder(StdFs).root(&dir).build();
        let doc = block_on(ws.document("notes/a.md")).unwrap();
        assert_eq!(doc.meta.get("title").and_then(Value::as_str), Some("A"));
        assert_eq!(doc.meta.get("author").and_then(Value::as_str), Some("Ada"));
        assert_eq!(doc.body, "body text\n");
    }

    #[test]
    fn document_surfaces_the_error_for_an_unreadable_path() {
        let dir = tempdir("document-missing");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        assert!(block_on(ws.document("nope.md")).is_err());
    }

    #[test]
    fn fs_path_joins_a_node_path_onto_the_workspace_root() {
        let dir = tempdir("fs-path");
        write(&dir, "notes/a.md", "---\ntitle: A\n---\n");

        let ws = Workspace::builder(StdFs).root(&dir).build();
        let node = block_on(ws.tree("notes/a.md")).unwrap();
        assert_eq!(ws.fs_path(&node.path), dir.join("notes/a.md"));
    }
}
