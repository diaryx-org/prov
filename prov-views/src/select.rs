//! Selecting the documents a view covers: scope, then conditions.
//!
//! This is the half that touches the workspace. It answers one question — *which
//! documents does this view cover?* — and answers it as a flat, deduplicated set
//! in path order. How those documents become groups is [`group`](fn@crate::group), which
//! is a pure function over what this returns.
//!
//! The split is what makes a [`Selection`] worth having as a value: one
//! selection can be grouped several ways, and every grouping question is
//! testable without a filesystem.
//!
//! # Scope is a traversal, not a path filter
//!
//! A view's [`under`](ViewSpec::under) is resolved by walking the **spanning
//! relation** below the anchor it names, never by matching a path prefix or a
//! title. That is the difference between a view and a saved search: `path
//! starts-with "Daily/"` breaks the moment someone renames the folder, and
//! matching an index *titled* `2026` finds the one under `Trips/` just as
//! happily as the one under `Daily/`. A traversal survives a rename, a move and
//! a retitle, because it follows the same declarations that make the workspace
//! a workspace.
//!
//! The scope is the whole subtree below the anchor, not its direct children —
//! see the inheritance note in [`crate::spec`].

use std::path::{Path, PathBuf};

use prov_graph::fs::ReadStorage;
use prov_graph::graph::{Graph, NodeKind, Target, TreeOptions};
use prov_graph::index::IdIndex;
use prov_graph::link::Link;
use prov_graph::meta::Value;

use crate::error::{Error, Result};
use crate::spec::ViewSpec;

/// One document a view covers.
///
/// Carries the document's whole metadata block, which is what lets grouping and
/// filtering be pure functions over a selection rather than passes that have to
/// go back to disk.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// Workspace-relative, normalized path — join it onto the root with
    /// [`Graph::fs_path`] before reading.
    pub path: PathBuf,
    /// The document's parsed metadata block.
    pub meta: Value,
}

impl Row {
    /// The document's `title`, when it declares one.
    pub fn title(&self) -> Option<&str> {
        self.meta.get("title").and_then(Value::as_str)
    }
}

/// The documents a view covers: in scope, past its conditions, deduplicated,
/// ordered by path.
///
/// Each document appears **once**, however many groups it will later fall into.
/// That is the difference between this and a [`RowSet`](crate::RowSet), and it
/// is why "how many documents does this view cover" is a question only this type
/// can answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    /// The name of the view that produced this.
    pub view: String,
    /// The documents, ordered by path.
    pub rows: Vec<Row>,
}

impl Selection {
    /// How many documents the view covers.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the view covers nothing.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// Select the documents `spec` covers, walking from `root_doc`.
///
/// `root_doc` is the workspace's root document: the spanning start for a view
/// that declares no anchor, and the document an `under:` link resolves relative
/// to. It is deliberately *not* the config surface the view was declared in — a
/// view is a property of the workspace, so moving the config document that
/// carries it must not change what it points at.
///
/// A view whose anchor names nothing is an [`Error::AnchorUnresolved`], not an
/// empty result. Those two states look identical to a reader and mean opposite
/// things: one is an archive with nothing in it yet, the other is a
/// misconfigured lens, and swallowing the second is how a broken view gets read
/// as an empty one for a year.
pub async fn select<FS: ReadStorage, Ix: IdIndex>(
    graph: &Graph<FS, Ix>,
    spec: &ViewSpec,
    root_doc: impl AsRef<Path>,
) -> Result<Selection> {
    let root_doc = root_doc.as_ref();
    // One scope for the whole selection: the spanning walk reads every document
    // in scope, and so does the metadata pass immediately after. Without this
    // they are two reads of every file for one view.
    let _scope = graph.read_scope();

    let anchor = match &spec.under {
        Some(under) => resolve_anchor(graph, spec, root_doc, under)?,
        None => root_doc.to_path_buf(),
    };

    // A dead spanning link has nothing to show in a view — no title, no
    // children, no file — so it is dropped rather than materialized as a
    // `Missing` node this pass would then have to filter out. `check` is where
    // a broken link is a finding; a view is not a validator.
    let tree = graph
        .tree_with(
            &anchor,
            TreeOptions {
                ignore_missing: true,
            },
        )
        .await?;

    // Resolving is not the same as arriving. A path anchor always *resolves* —
    // a path is a path — so `Daily/gone.md` gets this far and then walks to
    // nothing, which is the empty-vs-broken confusion again, one step later.
    // The walk's own verdict on the anchor node is what settles it.
    if spec.under.is_some()
        && let Some(why) = unreached(&tree.kind)
    {
        return Err(Error::AnchorUnresolved {
            view: spec.name.clone(),
            under: spec.under.clone().unwrap_or_default(),
            why,
        });
    }

    let mut scope: Vec<PathBuf> = Vec::new();
    collect(&tree, spec.under.is_some(), &mut scope);
    // A spanning tree reaches each document once, so this only matters for a
    // workspace that has already broken the single-parent invariant — where a
    // view listing a document twice would be a second, confusing symptom of a
    // fault `check` already reports properly.
    scope.sort();
    scope.dedup();

    let mut rows = Vec::with_capacity(scope.len());
    for path in scope {
        let doc = graph.document(&path).await?;
        let row = Row {
            path,
            meta: doc.meta,
        };
        if spec.filter.as_ref().is_none_or(|c| c.matches(&row.meta)) {
            rows.push(row);
        }
    }

    Ok(Selection {
        view: spec.name.clone(),
        rows,
    })
}

/// The path a view's `under:` link names, or why it does not name one.
fn resolve_anchor<FS, Ix: IdIndex>(
    graph: &Graph<FS, Ix>,
    spec: &ViewSpec,
    root_doc: &Path,
    under: &str,
) -> Result<PathBuf> {
    let unresolved = |why: &str| Error::AnchorUnresolved {
        view: spec.name.clone(),
        under: under.to_string(),
        why: why.to_string(),
    };
    match graph.resolve_link(root_doc, &Link::parse(under)) {
        Target::Path(path) => Ok(path),
        Target::UnresolvedId(id) => Err(unresolved(&format!(
            "no document is registered under the id `{}`",
            id.0
        ))),
        Target::AmbiguousAlias(name) => Err(unresolved(&format!(
            "several documents are titled `{name}`, so the anchor names no one of them"
        ))),
        Target::External => Err(unresolved(
            "an anchor must name a document in this workspace, and this is a URL",
        )),
        Target::SameDocument => Err(unresolved(
            "an anchor must name a document, and this names only a place inside one",
        )),
        Target::Foreign { workspace, .. } => Err(unresolved(&format!(
            "the anchor names a document in the workspace `{workspace}`, which prov cannot see from here"
        ))),
    }
}

/// Why a walk did not arrive at a readable document, or `None` when it did.
///
/// The remaining [`NodeKind`]s cannot occur at the root of a walk — a cycle
/// needs a trail behind it, and the id/alias/foreign kinds are how a *link*
/// failed, which [`resolve_anchor`] has already had its say about — but they
/// are spelled out rather than swept into a wildcard, so a new node kind
/// arrives here as a compile error instead of as a silently empty view.
fn unreached(kind: &NodeKind) -> Option<String> {
    match kind {
        NodeKind::Doc => None,
        NodeKind::Missing => Some("no document exists there".to_string()),
        NodeKind::Unreadable(why) => Some(format!("that document could not be read: {why}")),
        NodeKind::Cycle => Some("that document contains itself".to_string()),
        NodeKind::UnresolvedId(id) => Some(format!("no document is registered under `{}`", id.0)),
        NodeKind::AmbiguousAlias(name) => Some(format!("several documents are titled `{name}`")),
        NodeKind::Foreign { workspace, .. } => Some(format!(
            "it names a document in the workspace `{workspace}`, which prov cannot see from here"
        )),
    }
}

/// Flatten the readable documents of a spanning tree into `out`.
///
/// `skip_root` drops the anchor itself: an index is what a scoped view's
/// records hang *under*, not one of them. An unscoped view keeps its start,
/// because there the start is the workspace root and there is nothing it would
/// be an index *of*.
///
/// Every other [`NodeKind`] is skipped — a cycle marker, an unreadable file, an
/// unresolved id and a foreign leaf are all things `check` reports on and a
/// view has no row for.
fn collect(node: &prov_graph::graph::Node, skip_root: bool, out: &mut Vec<PathBuf>) {
    if !skip_root && matches!(node.kind, NodeKind::Doc) {
        out.push(node.path.clone());
    }
    for child in &node.children {
        collect(child, false, out);
    }
}

// These tests use YAML frontmatter fixtures, so they run under the `yaml`
// feature.
#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::*;
    use crate::filter::Condition;
    use crate::spec::Grouping;
    use prov_graph::exec::block_on;
    use prov_graph::fs::StdFs;
    use prov_graph::graph::ReadSettings;
    use prov_graph::index::NoIndex;

    use prov_testkit::write;
    fn tempdir(tag: &str) -> PathBuf {
        prov_testkit::scratch("select", tag)
    }

    /// A journal: a `Daily/` index with entries under it, plus a README beside
    /// them that carries a `created` stamp and is *not* a daily entry. The
    /// README is the reason a view needs scope at all.
    fn journal(tag: &str) -> PathBuf {
        let dir = tempdir(tag);
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- daily.md\n- readme.md\n---\n",
        );
        write(
            &dir,
            "readme.md",
            "---\ntitle: Readme\npart_of: index.md\ncreated: 2026-01-02\n---\n",
        );
        write(
            &dir,
            "daily.md",
            "---\ntitle: Daily\npart_of: index.md\ncontents:\n- daily/2026.md\n---\n",
        );
        write(
            &dir,
            "daily/2026.md",
            "---\ntitle: '2026'\npart_of: ../daily.md\ncontents:\n- 07-24.md\n- 08-01.md\n---\n",
        );
        write(
            &dir,
            "daily/07-24.md",
            "---\ntitle: July 24\npart_of: 2026.md\ndate_of_document: 2026-07-24\ndraft: true\n---\n",
        );
        write(
            &dir,
            "daily/08-01.md",
            "---\ntitle: August 1\npart_of: 2026.md\ncreated: 2026-08-01T09:00:00Z\n---\n",
        );
        dir
    }

    fn graph(dir: &Path) -> Graph<StdFs, NoIndex> {
        Graph::new(StdFs, dir, NoIndex, ReadSettings::default())
    }

    fn spec(under: Option<&str>, filter: Option<Condition>) -> ViewSpec {
        ViewSpec {
            name: "daily".into(),
            label: None,
            icon: None,
            group: Grouping {
                keys: vec!["date_of_document".into(), "created".into()],
                by: None,
            },
            under: under.map(str::to_string),
            filter,
            nest: None,
        }
    }

    fn paths(selection: &Selection) -> Vec<String> {
        selection
            .rows
            .iter()
            .map(|r| r.path.display().to_string())
            .collect()
    }

    /// The whole point of `under:`: the README carries a `created` date and is
    /// still not selected, because it is not under `Daily`. And the anchor
    /// itself is what the records hang under, not one of them.
    #[test]
    fn an_anchor_scopes_the_selection_to_its_subtree_and_excludes_itself() {
        let dir = journal("scope");
        let selection = block_on(select(
            &graph(&dir),
            &spec(Some("daily.md"), None),
            "index.md",
        ))
        .expect("a selection");
        assert_eq!(
            paths(&selection),
            ["daily/07-24.md", "daily/08-01.md", "daily/2026.md"]
        );
    }

    /// Without an anchor the view is the whole workspace — the difference the
    /// previous test isolated, in the other direction.
    ///
    /// The order is `Path`'s, which compares **component-wise**, not by bytes:
    /// the component `daily` sorts before `daily.md`, so the directory's
    /// contents precede the file beside it. Spelled out because it reads like a
    /// bug otherwise.
    #[test]
    fn an_unscoped_view_covers_the_whole_workspace() {
        let dir = journal("unscoped");
        let selection =
            block_on(select(&graph(&dir), &spec(None, None), "index.md")).expect("a selection");
        assert_eq!(
            paths(&selection),
            [
                "daily/07-24.md",
                "daily/08-01.md",
                "daily/2026.md",
                "daily.md",
                "index.md",
                "readme.md",
            ]
        );
    }

    /// Scope follows the spanning links, so moving the whole subtree to a new
    /// directory changes nothing. A `path starts-with "Daily/"` filter would
    /// have returned an empty selection here.
    #[test]
    fn scope_survives_moving_the_subtree() {
        let dir = journal("moved");
        std::fs::rename(dir.join("daily"), dir.join("archive")).unwrap();
        write(
            &dir,
            "daily.md",
            "---\ntitle: Daily\npart_of: index.md\ncontents:\n- archive/2026.md\n---\n",
        );
        write(
            &dir,
            "archive/2026.md",
            "---\ntitle: '2026'\npart_of: ../daily.md\ncontents:\n- 07-24.md\n- 08-01.md\n---\n",
        );

        let selection = block_on(select(
            &graph(&dir),
            &spec(Some("daily.md"), None),
            "index.md",
        ))
        .expect("a selection");
        assert_eq!(
            paths(&selection),
            ["archive/07-24.md", "archive/08-01.md", "archive/2026.md"]
        );
    }

    /// `where:` narrows what scope reached — and, unlike a broken anchor,
    /// matching nothing is an ordinary answer rather than an error.
    #[test]
    fn a_where_condition_narrows_the_selection() {
        let dir = journal("filter");
        let no_drafts = Condition::Not(Box::new(Condition::Has("draft".into())));
        let selection = block_on(select(
            &graph(&dir),
            &spec(Some("daily.md"), Some(no_drafts)),
            "index.md",
        ))
        .expect("a selection");
        assert_eq!(paths(&selection), ["daily/08-01.md", "daily/2026.md"]);

        let matches_nothing = Condition::Has("nonexistent".into());
        let empty = block_on(select(
            &graph(&dir),
            &spec(Some("daily.md"), Some(matches_nothing)),
            "index.md",
        ))
        .expect("an empty selection is not an error");
        assert!(empty.is_empty());
    }

    /// Rows carry their metadata, which is what lets grouping be a pure
    /// function rather than a second pass over the disk.
    #[test]
    fn rows_carry_metadata_so_grouping_needs_no_second_read() {
        let dir = journal("meta");
        let spec = spec(Some("daily.md"), None);
        let selection = block_on(select(&graph(&dir), &spec, "index.md")).expect("a selection");

        let entry = selection
            .rows
            .iter()
            .find(|r| r.path.ends_with("07-24.md"))
            .expect("the entry");
        assert_eq!(entry.title(), Some("July 24"));

        // No graph, no filesystem, no async.
        let rows = crate::group(&selection, &spec.group);
        assert_eq!(rows.len(), 3, "documents, not placements");
        assert_eq!(rows.groups.len(), 2);
    }

    /// An anchor that names nothing is an error, not an empty result. The two
    /// look identical to a reader and mean opposite things.
    #[test]
    fn an_unresolvable_anchor_is_an_error_not_an_empty_selection() {
        let dir = journal("dead-anchor");
        // A path anchor always *resolves* — a path is a path — so this one is
        // only caught by the walk failing to arrive.
        let by_path = spec(Some("[Gone](nowhere.md)"), None);
        let err = block_on(select(&graph(&dir), &by_path, "index.md")).unwrap_err();
        let Error::AnchorUnresolved { under, why, .. } = &err else {
            panic!("got {err:?}");
        };
        assert_eq!(under, "[Gone](nowhere.md)");
        assert_eq!(why, "no document exists there");

        let by_id = spec(Some("[Gone](id:abcd123)"), None);
        let err = block_on(select(&graph(&dir), &by_id, "index.md")).unwrap_err();
        let Error::AnchorUnresolved { view, under, .. } = &err else {
            panic!("got {err:?}");
        };
        assert_eq!(view, "daily");
        assert_eq!(under, "[Gone](id:abcd123)");
        assert!(err.to_string().contains("is registered under the id"));
    }

    /// Selecting twice over an unchanged workspace produces the identical set —
    /// the property that lets a consumer diff two runs.
    #[test]
    fn selection_is_deterministic() {
        let dir = journal("stable");
        let spec = spec(Some("daily.md"), None);
        let g = graph(&dir);
        let first = block_on(select(&g, &spec, "index.md")).unwrap();
        let second = block_on(select(&g, &spec, "index.md")).unwrap();
        assert_eq!(first, second);
    }
}
