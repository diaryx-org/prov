//! Selecting the documents a view covers: the census, then its condition.
//!
//! This is the half that touches the workspace. It answers one question —
//! *which documents does this view cover?* — and answers it as a flat,
//! deduplicated set in path order. How those documents become groups is
//! [`group`](fn@crate::group), which never goes back to disk.
//!
//! # The census carries the spine as data
//!
//! [`documents`] walks the spanning relation from the root once and records,
//! on every row, the document's registry id and its **ancestors** — every
//! document above it, root first. A view's `where:` reads them like any other
//! field (`doc.ancestors.exists(a, a.title == 'Daily')`), which is how a view
//! is scoped to a subtree without knowing the workspace has a shape. It
//! survives a rename, a move and a retitle-by-id for the reason the old
//! `under:` did: the ancestry is recomputed from the spine on every run, never
//! matched against a path prefix.
//!
//! # The census resolves references
//!
//! A field declared `type: ref` holds links, and the same person linked as
//! `[Ruth Harris](id:…)` in one document and `[Grandma](id:…)` in another is
//! one person. Grouping is pure and cannot resolve a link, so the census does
//! it once, here, and hands each row its [`references`](Row::references) —
//! which is how [`group`](fn@crate::group) files both documents under the
//! record they point at rather than under two spellings of it.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use prov_graph::fs::ReadStorage;
use prov_graph::graph::{Graph, NodeKind, Target, TreeOptions};
use prov_graph::index::IdIndex;
use prov_graph::link::Link;
use prov_graph::meta::Value;
use prov_graph::title::TitleIndex;

use crate::error::Result;
use crate::expr::Evaluator;
use crate::spec::ViewSpec;

/// One document of the census.
///
/// Carries the document's whole metadata block and its place in the spine,
/// which is what lets conditions and grouping be pure functions over a
/// selection rather than passes that go back to disk.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// Workspace-relative, normalized path — join it onto the root with
    /// [`Graph::fs_path`] before reading.
    pub path: PathBuf,
    /// The document's id: its own `id` field where it carries one, the
    /// registry's answer otherwise, so the column reads the same under every
    /// `id_storage`.
    pub id: Option<String>,
    /// Every document above this one in the spine, from the root down to its
    /// parent. Empty for the root.
    pub ancestors: Vec<Ancestor>,
    /// The document's parsed metadata block.
    pub meta: Value,
    /// The values of the document's `type: ref` fields that resolve to a
    /// document, each once. A value that resolves to nothing is not here, and
    /// groups by its text as any other value does.
    pub references: Vec<Reference>,
}

impl Row {
    /// The document's `title`, when it declares one — as text, so a
    /// hand-written `title: 2026` is the title "2026" rather than none
    /// ([`title_text`](prov_graph::title::title_text)).
    pub fn title(&self) -> Option<String> {
        self.meta
            .get("title")
            .and_then(prov_graph::title::title_text)
    }
}

/// A `type: ref` value a row carries, and the document it resolves to.
///
/// Recorded by the census so that grouping, which never goes back to disk, can
/// key a reference by what it names instead of how it was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// The value exactly as written — `[Grandma](id:abc1234)` — which is what
    /// an expression over the field sees, and so what a key comes out as.
    pub raw: String,
    /// The workspace-relative path of the document it resolves to.
    pub target: PathBuf,
    /// That document's `title`, when it declares one.
    pub title: Option<String>,
}

/// A document above a row in the spine — `doc.ancestors` in an expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ancestor {
    /// Workspace-relative path.
    pub path: PathBuf,
    /// Its `title`, when it declares one.
    pub title: Option<String>,
    /// Its id, by the same rule as [`Row::id`].
    pub id: Option<String>,
}

/// Which of a view's expressions failed on a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clause {
    /// The `where:` condition.
    Where,
    /// The `key:`.
    Key,
}

impl fmt::Display for Clause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Clause::Where => "where",
            Clause::Key => "key",
        })
    }
}

/// A document an expression could not be evaluated on.
///
/// Reported beside the result rather than guessed at: a document whose
/// condition failed is neither shown (which could show what the condition
/// meant to hide) nor silently dropped (which is how a broken view gets read
/// as an empty one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// The document.
    pub path: PathBuf,
    /// Which expression failed.
    pub clause: Clause,
    /// Why, in a sentence.
    pub message: String,
}

/// The documents a view covers: past its condition, deduplicated, ordered by
/// path.
///
/// Each document appears **once**, however many groups it will later fall
/// into. That is the difference between this and a [`RowSet`](crate::RowSet),
/// and it is why "how many documents does this view cover" is a question only
/// this type can answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    /// The name of the view that produced this.
    pub view: String,
    /// The documents, ordered by path.
    pub rows: Vec<Row>,
    /// The documents the condition could not be evaluated on, in path order.
    pub failures: Vec<Failure>,
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

/// Select the documents `spec` covers: the census from `root_doc`, narrowed by
/// its `where:`.
///
/// `root_doc` is the workspace's root document. It is deliberately *not* the
/// config surface the view was declared in — a view is a property of the
/// workspace, so moving the config document that carries it must not change
/// what it covers.
pub async fn select<FS: ReadStorage, Ix: IdIndex>(
    graph: &Graph<FS, Ix>,
    spec: &ViewSpec,
    root_doc: impl AsRef<Path>,
) -> Result<Selection> {
    let rows = documents(graph, root_doc).await?;
    Ok(narrow(&spec.name, spec, rows))
}

/// [`select`] over rows already read — for a caller that holds the census and
/// runs several views over it.
pub fn narrow(view: &str, spec: &ViewSpec, rows: Vec<Row>) -> Selection {
    let mut selection = Selection {
        view: view.to_string(),
        rows: Vec::with_capacity(rows.len()),
        failures: Vec::new(),
    };
    let Some(condition) = &spec.filter else {
        selection.rows = rows;
        return selection;
    };
    let evaluator = Evaluator::new();
    for row in rows {
        match evaluator.test(condition, &row) {
            Ok(true) => selection.rows.push(row),
            Ok(false) => {}
            Err(message) => selection.failures.push(Failure {
                path: row.path,
                clause: Clause::Where,
                message,
            }),
        }
    }
    selection
}

/// Every document the workspace reaches from `root_doc` — the root included —
/// each once, with its metadata, its id and its ancestors, in path order.
///
/// This is the census every view narrows, offered without a [`ViewSpec`]
/// because the question needs none. A consumer building its own index over a
/// workspace — a query engine, a search table, a shell pipeline — wants the
/// whole reached set with the metadata attached.
///
/// Reached, not present: a file in a directory nothing links into is not a
/// row, for the same reason `check` does not report it. The spine decides what
/// the workspace contains; this lists it.
pub async fn documents<FS: ReadStorage, Ix: IdIndex>(
    graph: &Graph<FS, Ix>,
    root_doc: impl AsRef<Path>,
) -> Result<Vec<Row>> {
    let _scope = graph.read_scope();
    // A dead spanning link has nothing to show — no title, no children, no
    // file — so it is dropped rather than materialized as a `Missing` node
    // this pass would then have to filter out. `check` is where a broken link
    // is a finding; the census is not a validator.
    let tree = graph
        .tree_with(
            root_doc.as_ref(),
            TreeOptions {
                ignore_missing: true,
            },
        )
        .await?;

    let mut reached: Vec<(PathBuf, Vec<PathBuf>)> = Vec::new();
    collect(&tree, &mut Vec::new(), &mut reached);
    // A spanning tree reaches each document once, so this only matters for a
    // workspace that has already broken the single-parent invariant — where a
    // census listing a document twice would be a second, confusing symptom of
    // a fault `check` already reports properly. The first place it was
    // reached is the one it keeps.
    reached.sort_by(|a, b| a.0.cmp(&b.0));
    reached.dedup_by(|a, b| a.0 == b.0);

    let mut metas: HashMap<PathBuf, Value> = HashMap::with_capacity(reached.len());
    for (path, _) in &reached {
        let doc = graph.document(path).await?;
        metas.insert(path.clone(), doc.meta);
    }
    let id_of = |path: &Path, meta: &Value| {
        meta.get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| graph.index().id_for_path(path).map(|id| id.0))
    };
    let ancestor = |path: &PathBuf| {
        let meta = metas.get(path);
        Ancestor {
            path: path.clone(),
            title: meta
                .and_then(|m| m.get("title"))
                .and_then(prov_graph::title::title_text),
            id: meta.and_then(|m| id_of(path, m)),
        }
    };

    let mut rows = Vec::with_capacity(reached.len());
    for (path, above) in &reached {
        let meta = metas.get(path).cloned().unwrap_or(Value::Null);
        rows.push(Row {
            path: path.clone(),
            id: id_of(path, &meta),
            ancestors: above.iter().map(ancestor).collect(),
            meta,
            references: Vec::new(),
        });
    }
    if !graph.references().is_empty() {
        resolve_references(graph, &mut rows).await;
    }
    Ok(rows)
}

/// Fill in each row's [`references`](Row::references).
///
/// A `[[Name]]` resolves against the titles and file stems of the documents
/// the census reached — the documents a view can show — so a name two of them
/// claim is ambiguous and stays text. A target outside the census is read for
/// its title; one that cannot be read resolves to nothing, as a dead link does.
async fn resolve_references<FS: ReadStorage, Ix: IdIndex>(graph: &Graph<FS, Ix>, rows: &mut [Row]) {
    let mut titles = TitleIndex::new();
    let mut title_of: HashMap<PathBuf, Option<String>> = HashMap::with_capacity(rows.len());
    for row in rows.iter() {
        if let Some(stem) = row.path.file_stem().and_then(|s| s.to_str()) {
            titles.insert(stem, row.path.clone());
        }
        if let Some(title) = row.title() {
            titles.insert(title, row.path.clone());
        }
        title_of.insert(row.path.clone(), row.title());
    }

    for row in rows.iter_mut() {
        let mut references: Vec<Reference> = Vec::new();
        for field in graph.references() {
            for (_, raw) in prov_graph::field::strings_at(&row.meta, field) {
                if references.iter().any(|r| r.raw == raw) {
                    continue;
                }
                let link = Link::parse(raw.trim());
                let Target::Path(target) = graph.resolve_link_with(&row.path, &link, Some(&titles))
                else {
                    continue;
                };
                let title = match title_of.get(&target) {
                    Some(title) => title.clone(),
                    None => match graph.document(&target).await {
                        Ok(doc) => doc
                            .meta
                            .get("title")
                            .and_then(prov_graph::title::title_text),
                        Err(_) => continue,
                    },
                };
                references.push(Reference { raw, target, title });
            }
        }
        row.references = references;
    }
}

/// Flatten the readable documents of a spanning tree into `out`, each with
/// the paths of the documents above it.
///
/// Every other [`NodeKind`] is skipped — a cycle marker, an unreadable file,
/// an unresolved id and a foreign leaf are all things `check` reports on and a
/// census has no row for. Their children are not walked either: a node that
/// did not arrive has none.
fn collect(
    node: &prov_graph::graph::Node,
    above: &mut Vec<PathBuf>,
    out: &mut Vec<(PathBuf, Vec<PathBuf>)>,
) {
    if !matches!(node.kind, NodeKind::Doc) {
        return;
    }
    out.push((node.path.clone(), above.clone()));
    above.push(node.path.clone());
    for child in &node.children {
        collect(child, above, out);
    }
    above.pop();
}

// These tests use YAML frontmatter fixtures, so they run under the `yaml`
// feature.
#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::*;
    use crate::expr::Expression;
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
            "---\ntitle: Daily\nid: dly0001\npart_of: index.md\ncontents:\n- daily/2026.md\n---\n",
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

    fn spec(filter: Option<&str>) -> ViewSpec {
        ViewSpec {
            filter: filter.map(|f| Expression::parse(f).expect(f)),
            ..ViewSpec::new(
                "daily",
                Expression::parse("month(first(date_of_document, created))").unwrap(),
            )
        }
    }

    fn paths(selection: &Selection) -> Vec<String> {
        selection
            .rows
            .iter()
            .map(|r| r.path.display().to_string())
            .collect()
    }

    /// The census is the whole workspace, root included, in `Path` order —
    /// which compares **component-wise**, not by bytes: the component `daily`
    /// sorts before `daily.md`, so the directory's contents precede the file
    /// beside it. Spelled out because it reads like a bug otherwise.
    #[test]
    fn a_view_without_a_condition_covers_the_whole_workspace() {
        let dir = journal("unscoped");
        let selection = block_on(select(&graph(&dir), &spec(None), "index.md")).unwrap();
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

    /// Each row knows what is above it, root first, with titles and ids.
    #[test]
    fn rows_carry_their_ancestors_and_ids() {
        let dir = journal("ancestors");
        let rows = block_on(documents(&graph(&dir), "index.md")).unwrap();
        let entry = rows
            .iter()
            .find(|r| r.path.ends_with("07-24.md"))
            .expect("the entry");
        let titles: Vec<_> = entry
            .ancestors
            .iter()
            .map(|a| a.title.clone().unwrap_or_default())
            .collect();
        assert_eq!(titles, ["Home", "Daily", "2026"]);
        assert_eq!(entry.ancestors[1].id.as_deref(), Some("dly0001"));
        let daily = rows.iter().find(|r| r.path.ends_with("daily.md")).unwrap();
        assert_eq!(daily.id.as_deref(), Some("dly0001"));
        let root = rows.iter().find(|r| r.path.ends_with("index.md")).unwrap();
        assert!(root.ancestors.is_empty());
    }

    /// Scope is a condition on ancestry: the README carries a `created` date
    /// and is still not selected, because it is not under `Daily`. The index
    /// itself is not its own ancestor, so it is not one of its records.
    #[test]
    fn ancestry_scopes_a_view_to_a_subtree() {
        let dir = journal("scope");
        let by_title = spec(Some("doc.ancestors.exists(a, a.title == 'Daily')"));
        let selection = block_on(select(&graph(&dir), &by_title, "index.md")).unwrap();
        assert_eq!(
            paths(&selection),
            ["daily/07-24.md", "daily/08-01.md", "daily/2026.md"]
        );
        let by_id = spec(Some("doc.ancestors.exists(a, a.id == 'dly0001')"));
        assert_eq!(
            block_on(select(&graph(&dir), &by_id, "index.md")).unwrap(),
            Selection {
                view: "daily".into(),
                ..selection
            }
        );
    }

    /// The ancestry follows the spanning links, so moving the whole subtree to
    /// a new directory changes nothing. A `path starts-with "daily/"` filter
    /// would have returned an empty selection here.
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
            &spec(Some("doc.ancestors.exists(a, a.title == 'Daily')")),
            "index.md",
        ))
        .unwrap();
        assert_eq!(
            paths(&selection),
            ["archive/07-24.md", "archive/08-01.md", "archive/2026.md"]
        );
    }

    /// A condition narrows the census, and matching nothing is an ordinary
    /// answer rather than an error.
    #[test]
    fn a_condition_narrows_the_selection() {
        let dir = journal("filter");
        let selection = block_on(select(
            &graph(&dir),
            &spec(Some(
                "doc.ancestors.exists(a, a.title == 'Daily') && !present(draft)",
            )),
            "index.md",
        ))
        .unwrap();
        assert_eq!(paths(&selection), ["daily/08-01.md", "daily/2026.md"]);

        let empty = block_on(select(
            &graph(&dir),
            &spec(Some("present(nonexistent)")),
            "index.md",
        ))
        .unwrap();
        assert!(empty.is_empty());
        assert!(empty.failures.is_empty());
    }

    /// A condition that cannot be evaluated on a document names it, and the
    /// document is neither shown nor silently dropped.
    #[test]
    fn a_condition_that_fails_is_reported_per_document() {
        let dir = journal("failure");
        let selection = block_on(select(
            &graph(&dir),
            &spec(Some("size(created) > 4")),
            "index.md",
        ))
        .unwrap();
        assert_eq!(paths(&selection), ["daily/08-01.md", "readme.md"]);
        assert_eq!(selection.failures.len(), 4);
        assert!(
            selection
                .failures
                .iter()
                .all(|f| f.clause == Clause::Where && f.message.contains("present()"))
        );
    }

    /// Rows carry their metadata, which is what lets grouping be a pure
    /// function rather than a second pass over the disk.
    #[test]
    fn rows_carry_metadata_so_grouping_needs_no_second_read() {
        let dir = journal("meta");
        let spec = spec(Some("doc.ancestors.exists(a, a.title == 'Daily')"));
        let selection = block_on(select(&graph(&dir), &spec, "index.md")).unwrap();
        let entry = selection
            .rows
            .iter()
            .find(|r| r.path.ends_with("07-24.md"))
            .expect("the entry");
        assert_eq!(entry.title().as_deref(), Some("July 24"));

        // No graph, no filesystem, no async.
        let rows = crate::group(&selection, &spec.key);
        assert_eq!(rows.len(), 3, "documents, not placements");
        assert_eq!(rows.groups.len(), 2);
        assert_eq!(rows.ungrouped.len(), 1, "the year index carries no date");
    }

    /// The People lens over references: one person, linked by id under two
    /// labels and once by name, is one group titled by the person's record. A
    /// reference to nothing still groups, by its text.
    #[test]
    fn a_reference_field_groups_by_the_document_it_names() {
        let dir = tempdir("refs");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- people/ruth.md\n- a.md\n- b.md\n- c.md\n---\n",
        );
        write(
            &dir,
            "people/ruth.md",
            "---\ntitle: Ruth Harris\nid: rth0001\npart_of: ../index.md\n---\n",
        );
        write(
            &dir,
            "a.md",
            "---\ntitle: Letter\npart_of: index.md\npeople:\n- '[Ruth Harris](id:rth0001)'\n---\n",
        );
        write(
            &dir,
            "b.md",
            "---\ntitle: Recipe\npart_of: index.md\npeople:\n- '[Grandma](id:rth0001)'\n- '[Nan](people/ruth.md)'\n---\n",
        );
        write(
            &dir,
            "c.md",
            "---\ntitle: Photo\npart_of: index.md\npeople:\n- '[[Ruth Harris]]'\n- '[Walter](id:wlt0001)'\n---\n",
        );
        // The registry, as far as this workspace needs one: the record's id.
        struct Ruth;
        impl IdIndex for Ruth {
            fn resolve(&self, id: &prov_graph::identity::Id) -> Option<PathBuf> {
                (id.0 == "rth0001").then(|| PathBuf::from("people/ruth.md"))
            }
            fn id_for_path(&self, path: &Path) -> Option<prov_graph::identity::Id> {
                (path == Path::new("people/ruth.md"))
                    .then(|| prov_graph::identity::Id("rth0001".into()))
            }
        }
        let graph = Graph::new(
            StdFs,
            &dir,
            Ruth,
            ReadSettings {
                references: vec![prov_graph::field::FieldPath::parse("people")],
                ..ReadSettings::default()
            },
        );
        let spec = ViewSpec::new("people", Expression::parse("people").unwrap());
        let selection = block_on(select(&graph, &spec, "index.md")).unwrap();
        let rows = crate::group(&selection, &spec.key);
        let groups: Vec<_> = rows
            .groups
            .iter()
            .map(|g| {
                let paths: Vec<_> = g
                    .rows
                    .iter()
                    .map(|r| r.path.display().to_string())
                    .collect();
                (g.key.as_str(), g.label.as_str(), paths)
            })
            .collect();
        assert_eq!(
            groups,
            [
                (
                    "people/ruth.md",
                    "Ruth Harris",
                    vec!["a.md".to_string(), "b.md".into(), "c.md".into()]
                ),
                (
                    "[Walter](id:wlt0001)",
                    "[Walter](id:wlt0001)",
                    vec!["c.md".to_string()]
                ),
            ],
            "one group per person, the recipe in it once"
        );
    }

    /// Selecting twice over an unchanged workspace produces the identical set —
    /// the property that lets a consumer diff two runs.
    #[test]
    fn selection_is_deterministic() {
        let dir = journal("stable");
        let spec = spec(Some("doc.ancestors.exists(a, a.title == 'Daily')"));
        let g = graph(&dir);
        let first = block_on(select(&g, &spec, "index.md")).unwrap();
        let second = block_on(select(&g, &spec, "index.md")).unwrap();
        assert_eq!(first, second);
    }
}
