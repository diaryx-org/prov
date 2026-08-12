//! Executing a [`ViewSpec`] against a workspace: scope, then group, then order.
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
//!
//! # Ordering
//!
//! Groups sort ascending by key, and rows within a group sort by path. Both are
//! lexical and both are total, so a view executed twice over an unchanged
//! workspace produces the identical row set.
//!
//! Ascending is the honest default rather than the convenient one: it is right
//! for `people` and `tags`, and wrong for a date view, where a reader wants the
//! newest first. There is deliberately no `sort:` axis yet — ordering, like
//! formulas, is a place the format grows teeth, and a consumer that wants
//! newest-first reverses a `Vec` it already has.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use prov_graph::fs::ReadStorage;
use prov_graph::graph::{Graph, NodeKind, Target, TreeOptions};
use prov_graph::index::IdIndex;
use prov_graph::link::Link;

use crate::error::{Error, Result};
use crate::spec::ViewSpec;

/// One document in a view's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Workspace-relative, normalized path — join it onto the root with
    /// [`Graph::fs_path`] before reading.
    pub path: PathBuf,
    /// The document's `title`, when it declares one.
    pub title: Option<String>,
}

/// One group of a view's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// The group key — a field value, or a value cut at the view's grain.
    pub key: String,
    /// The documents under this key, ordered by path.
    pub rows: Vec<Row>,
}

/// A view's result: what the workspace says when the view is asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowSet {
    /// The name of the view that produced this.
    pub view: String,
    /// Groups, ascending by key.
    pub groups: Vec<Group>,
    /// Documents in scope that no field in the grouping chain gave a usable
    /// value for.
    ///
    /// Reported rather than dropped: a view whose entries have all quietly
    /// stopped grouping looks exactly like an empty archive, and the difference
    /// is the whole diagnosis. A frontend labels this bucket ("Undated",
    /// "Untagged"); which words to use is a presentation decision this crate
    /// does not make.
    pub ungrouped: Vec<Row>,
}

impl RowSet {
    /// Every row, grouped or not — the view's scope as it was actually read.
    pub fn len(&self) -> usize {
        self.groups.iter().map(|g| g.rows.len()).sum::<usize>() + self.ungrouped.len()
    }

    /// Whether the view selected nothing at all.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Execute `spec` against `graph`, walking from `root_doc`.
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
pub async fn execute<FS: ReadStorage, Ix: IdIndex>(
    graph: &Graph<FS, Ix>,
    spec: &ViewSpec,
    root_doc: impl AsRef<Path>,
) -> Result<RowSet> {
    let root_doc = root_doc.as_ref();
    // One scope for the whole execution: the spanning walk reads every document
    // in scope, and so does the grouping pass immediately after. Without this
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

    let mut grouped: BTreeMap<String, Vec<Row>> = BTreeMap::new();
    let mut ungrouped: Vec<Row> = Vec::new();
    for path in scope {
        let doc = graph.document(&path).await?;
        let row = Row {
            title: doc
                .meta
                .get("title")
                .and_then(prov_graph::meta::Value::as_str)
                .map(str::to_string),
            path,
        };
        let keys = spec.group.keys_of(&doc.meta);
        if keys.is_empty() {
            ungrouped.push(row);
            continue;
        }
        for key in keys {
            grouped.entry(key).or_default().push(row.clone());
        }
    }

    let mut groups: Vec<Group> = grouped
        .into_iter()
        .map(|(key, mut rows)| {
            rows.sort_by(|a, b| a.path.cmp(&b.path));
            Group { key, rows }
        })
        .collect();
    // `BTreeMap` already ordered the keys; sorting the rows is what is left.
    groups.iter_mut().for_each(|g| g.rows.dedup());
    ungrouped.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(RowSet {
        view: spec.name.clone(),
        groups,
        ungrouped,
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
    use crate::spec::{Grain, Grouping};
    use prov_graph::exec::block_on;
    use prov_graph::fs::StdFs;
    use prov_graph::graph::ReadSettings;
    use prov_graph::index::NoIndex;

    fn write(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prov-views-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A journal: a `Daily/` index with two years of entries under it, plus a
    /// README beside them that carries a `created` stamp and is *not* a daily
    /// entry. The README is the reason a view needs scope at all.
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
            "---\ntitle: July 24\npart_of: 2026.md\ndate_of_document: 2026-07-24\n---\n",
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

    fn spec(under: Option<&str>, keys: &[&str], by: Option<Grain>) -> ViewSpec {
        ViewSpec {
            name: "daily".into(),
            label: None,
            icon: None,
            group: Grouping {
                keys: keys.iter().map(|k| (*k).to_string()).collect(),
                by,
            },
            under: under.map(str::to_string),
            nest: None,
        }
    }

    /// The whole point of `under:`: the README carries a `created` date and is
    /// still not in the Daily view, because it is not under `Daily`.
    #[test]
    fn an_anchor_scopes_the_view_to_its_subtree() {
        let dir = journal("scope");
        let spec = spec(
            Some("daily.md"),
            &["date_of_document", "created"],
            Some(Grain::Month),
        );
        let rows = block_on(execute(&graph(&dir), &spec, "index.md")).unwrap();

        assert_eq!(
            rows.groups
                .iter()
                .map(|g| g.key.as_str())
                .collect::<Vec<_>>(),
            ["2026-07", "2026-08"]
        );
        assert_eq!(rows.groups[0].rows[0].path, PathBuf::from("daily/07-24.md"));
        assert_eq!(rows.groups[0].rows[0].title.as_deref(), Some("July 24"));
        assert!(
            !rows.ungrouped.iter().any(|r| r.path.ends_with("readme.md")),
            "the README is out of scope, not merely ungrouped"
        );
    }

    /// The anchor is what the records hang under, not one of them — and the
    /// year index between them has no date, so it is the ungrouped bucket's.
    #[test]
    fn the_anchor_is_excluded_and_undated_indexes_are_ungrouped() {
        let dir = journal("anchor");
        let spec = spec(Some("daily.md"), &["date_of_document", "created"], None);
        let rows = block_on(execute(&graph(&dir), &spec, "index.md")).unwrap();

        let paths: Vec<_> = rows.ungrouped.iter().map(|r| &r.path).collect();
        assert_eq!(paths, [&PathBuf::from("daily/2026.md")]);
        assert_eq!(rows.len(), 3);
    }

    /// Without an anchor the view is the whole workspace, so the README joins —
    /// the difference the previous test isolated, in the other direction.
    #[test]
    fn an_unscoped_view_covers_the_whole_workspace() {
        let dir = journal("unscoped");
        let spec = spec(None, &["date_of_document", "created"], Some(Grain::Year));
        let rows = block_on(execute(&graph(&dir), &spec, "index.md")).unwrap();

        assert_eq!(rows.groups.len(), 1, "one year");
        let paths: Vec<_> = rows.groups[0].rows.iter().map(|r| &r.path).collect();
        assert_eq!(
            paths,
            [
                &PathBuf::from("daily/07-24.md"),
                &PathBuf::from("daily/08-01.md"),
                &PathBuf::from("readme.md"),
            ]
        );
    }

    /// Scope follows the spanning links, so moving the whole subtree to a new
    /// directory changes nothing. A `path starts-with "Daily/"` filter would
    /// have returned an empty view here.
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

        let spec = spec(
            Some("daily.md"),
            &["date_of_document", "created"],
            Some(Grain::Month),
        );
        let rows = block_on(execute(&graph(&dir), &spec, "index.md")).unwrap();
        assert_eq!(
            rows.groups
                .iter()
                .map(|g| g.key.as_str())
                .collect::<Vec<_>>(),
            ["2026-07", "2026-08"]
        );
        assert_eq!(
            rows.groups[0].rows[0].path,
            PathBuf::from("archive/07-24.md")
        );
    }

    /// One document, two groups — and the row appears whole in each, so a
    /// consumer never has to join anything back together.
    #[test]
    fn a_multi_valued_field_files_one_document_under_every_value() {
        let dir = tempdir("people");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- letter.md\n---\n",
        );
        write(
            &dir,
            "letter.md",
            "---\ntitle: A letter\npart_of: index.md\npeople:\n- Ada\n- Grace\n---\n",
        );

        let spec = spec(None, &["people"], None);
        let rows = block_on(execute(&graph(&dir), &spec, "index.md")).unwrap();
        assert_eq!(
            rows.groups
                .iter()
                .map(|g| g.key.as_str())
                .collect::<Vec<_>>(),
            ["Ada", "Grace"]
        );
        for group in &rows.groups {
            assert_eq!(group.rows[0].path, PathBuf::from("letter.md"));
        }
    }

    /// An anchor that names nothing is an error, not an empty view. The two
    /// look identical to a reader and mean opposite things.
    #[test]
    fn an_unresolvable_anchor_is_an_error_not_an_empty_result() {
        let dir = journal("dead-anchor");
        // A path anchor always *resolves* — a path is a path — so this one is
        // only caught by the walk failing to arrive.
        let by_path = spec(Some("[Gone](nowhere.md)"), &["created"], None);
        let err = block_on(execute(&graph(&dir), &by_path, "index.md")).unwrap_err();
        let Error::AnchorUnresolved { under, why, .. } = &err else {
            panic!("got {err:?}");
        };
        assert_eq!(under, "[Gone](nowhere.md)");
        assert_eq!(why, "no document exists there");

        let by_id = spec(Some("[Gone](id:abcd123)"), &["created"], None);
        let err = block_on(execute(&graph(&dir), &by_id, "index.md")).unwrap_err();
        let Error::AnchorUnresolved { view, under, .. } = &err else {
            panic!("got {err:?}");
        };
        assert_eq!(view, "daily");
        assert_eq!(under, "[Gone](id:abcd123)");
        assert!(err.to_string().contains("is registered under the id"));
    }

    /// Executing the same view twice over an unchanged workspace produces the
    /// identical row set — the property that lets a consumer diff two runs.
    #[test]
    fn execution_is_deterministic() {
        let dir = journal("stable");
        let spec = spec(
            Some("daily.md"),
            &["date_of_document", "created"],
            Some(Grain::Year),
        );
        let g = graph(&dir);
        let first = block_on(execute(&g, &spec, "index.md")).unwrap();
        let second = block_on(execute(&g, &spec, "index.md")).unwrap();
        assert_eq!(first, second);
    }
}
