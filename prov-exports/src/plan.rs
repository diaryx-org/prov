//! Planning what one export lets leave: the gate set, narrowed — only ever
//! narrowed — by the view.
//!
//! # The one-way valve
//!
//! > **An export's document set is a subset of its gate's admitted set.** A
//! > view may narrow that set. A view may never admit a document the gate
//! > holds back.
//!
//! [`compose`] enforces this structurally rather than by convention: it seeds
//! `entries` from the gate's admitted set and the only operation it applies
//! afterwards is `retain`. The view's selection is consulted as a set of
//! paths and never iterated to add. That matters because the whole value of
//! keeping gate and view apart is that *may this document leave* stays
//! answerable by reading one field on that one document — let a view widen
//! the set and the question becomes a proof about the pipeline.
//!
//! The export's `hold` is the other narrowing, and it goes through the same
//! valve: a document the gate admits that declares `true` under the hold
//! field is `retain`ed out of `entries` into [`ExportPlan::held`], and a
//! document the gate holds back is withheld whatever its hold field says.
//! The hold is applied before the view, so a draft that a view would also
//! have scoped out is reported as held — the document's own word about
//! itself outranks the workspace's arrangement of it.
//!
//! The valve also fails **closed**: a view that cannot be executed is an
//! error ([`Error::View`](crate::Error)), never a fall-back to the gate's
//! whole set, and an unreadable export declaration is not an export at all
//! ([`ExportSpec::parse`](crate::ExportSpec::parse)).
//!
//! # Two halves, like `select` and `group`
//!
//! [`plan`] is the half that touches the workspace: it walks the spanning
//! tree, reads each reachable document once, executes the named view, and
//! hands everything to [`compose`] — which is a pure function, so every
//! question about the valve is testable without a filesystem. The split is
//! `prov-views`' own (`select` does I/O, `group` is pure), reused because the
//! property it buys — the invariant lives in code with nothing to mock — is
//! worth more here, where the invariant is a disclosure bound rather than a
//! grouping.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use prov_graph::fs::ReadStorage;
use prov_graph::graph::{Graph, NodeKind, TreeOptions};
use prov_graph::index::IdIndex;
use prov_views::{Row, ViewSpec};

use crate::error::{Error, Result};
use crate::spec::ExportSpec;

/// One document an export lets leave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportDoc {
    /// Workspace-relative, normalized path.
    pub path: PathBuf,
    /// The document's `title`, when it declares one.
    pub title: Option<String>,
    /// Every value the document declares under the gate's field — the one
    /// field that answers *why is this document leaving*, carried so a
    /// preview can show its work.
    pub declared: Vec<String>,
}

/// A reachable document the gate holds back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Withheld {
    /// Workspace-relative, normalized path.
    pub path: PathBuf,
    /// The document's `title`, when it declares one.
    pub title: Option<String>,
    /// What the document declares under the gate's field: `None` when the
    /// field is absent (the default state — undeclared, private), `Some` when
    /// it is declared with other values. The difference is the difference
    /// between *nobody said anything* and *somebody said something else*, and
    /// a report that conflates them sends the author to the wrong file.
    pub declared: Option<Vec<String>>,
}

/// What one export lets leave: the documents, and what stayed behind on
/// which side of which boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportPlan {
    /// The export's name — its public handle.
    pub export: String,
    /// The documents this export lets leave, in path order. Guaranteed a
    /// subset of the gate's admitted set.
    pub entries: Vec<ExportDoc>,
    /// Documents the gate admits that the export's view scoped out. Not an
    /// error — it is the difference between an export and its gate, and a
    /// preview owes the user this list, since "I tagged it and it isn't in
    /// the export" is otherwise unexplainable from the file alone.
    pub outside_view: Vec<PathBuf>,
    /// Documents the gate admits that are held back by their own hold field
    /// (`draft: true` under an export with `hold: draft`), in path order.
    /// Carried as [`ExportDoc`]s, declared values and all, because these are
    /// the documents that *would* leave — a preview shows them as the
    /// export's pending set, not as strangers to it. Always empty for an
    /// export with no hold.
    pub held: Vec<ExportDoc>,
    /// Reachable documents the gate holds back, each with what it declared.
    /// The bulk of any workspace — closed by default means most documents
    /// are here — reported so *withheld* is an answer, never a silence.
    pub withheld: Vec<Withheld>,
}

/// Compose a gate's judgment over the reachable documents with a view's
/// selection — the pure heart of the export layer.
///
/// `rows` is every reachable document with its metadata, in path order.
/// `view_scope` is `None` for an export that names no view, `Some(paths)` for
/// the executed view's selection. Read the body: `entries` is seeded from
/// what the gate admits and only ever `retain`ed — see the module docs.
pub fn compose(
    spec: &ExportSpec,
    rows: &[Row],
    view_scope: Option<&HashSet<PathBuf>>,
) -> ExportPlan {
    let mut entries = Vec::new();
    let mut held = Vec::new();
    let mut withheld = Vec::new();
    for row in rows {
        let title = row.title().map(str::to_string);
        match spec.gate.declared_in(&row.meta) {
            Some(declared) if declared.iter().any(|v| v == spec.gate.value.trim()) => {
                let doc = ExportDoc {
                    path: row.path.clone(),
                    title,
                    declared,
                };
                // The hold is judged only here, on a document the gate has
                // already admitted: it can move a document from `entries` to
                // `held`, and nothing can move one the other way. A withheld
                // document's hold field is never read.
                if spec.holds(&row.meta) {
                    held.push(doc);
                } else {
                    entries.push(doc);
                }
            }
            declared => withheld.push(Withheld {
                path: row.path.clone(),
                title,
                declared,
            }),
        }
    }

    // The one-way valve: the scope is only ever *consulted*, never iterated
    // into `entries` — so no view can put back a document the gate held out.
    let mut outside_view = Vec::new();
    if let Some(scope) = view_scope {
        entries.retain(|doc| {
            if scope.contains(&doc.path) {
                true
            } else {
                outside_view.push(doc.path.clone());
                false
            }
        });
    }

    ExportPlan {
        export: spec.name.clone(),
        entries,
        outside_view,
        held,
        withheld,
    }
}

/// Plan what `spec` lets leave, walking from `root_doc`.
///
/// `views` is the workspace's declared views — the pool `spec.view` is
/// resolved against, which the caller already holds as `WorkspaceConfig::views`.
/// An export naming a view not in the pool is [`Error::ViewUnknown`], and a
/// named view that fails to execute is [`Error::View`]; neither falls back to
/// the gate's whole set, because a bound that was written down and cannot be
/// applied must fail closed.
///
/// The walk covers every document the spanning tree reaches, the workspace
/// root included — the root describes the workspace, and whether *it* leaves
/// is the gate's question like any other. A dead spanning link has nothing to
/// export and is skipped, exactly as a view's selection skips it.
pub async fn plan<FS: ReadStorage, Ix: IdIndex>(
    graph: &Graph<FS, Ix>,
    spec: &ExportSpec,
    views: &[ViewSpec],
    root_doc: impl AsRef<Path>,
) -> Result<ExportPlan> {
    let root_doc = root_doc.as_ref();
    // One scope for the whole plan: the reachability walk, the metadata pass,
    // and the view's own selection all read the same documents.
    let _scope = graph.read_scope();

    // Resolve the view *before* walking: an export declared against a view
    // nobody can find should fail before it reads a single document.
    let view = match &spec.view {
        Some(name) => {
            Some(
                views
                    .iter()
                    .find(|v| v.name == *name)
                    .ok_or_else(|| Error::ViewUnknown {
                        export: spec.name.clone(),
                        view: name.clone(),
                    })?,
            )
        }
        None => None,
    };

    let tree = graph
        .tree_with(
            root_doc,
            TreeOptions {
                ignore_missing: true,
            },
        )
        .await?;
    let mut reachable: Vec<PathBuf> = Vec::new();
    collect(&tree, &mut reachable);
    reachable.sort();
    reachable.dedup();

    let mut rows = Vec::with_capacity(reachable.len());
    for path in reachable {
        let doc = graph.document(&path).await?;
        rows.push(Row {
            path,
            meta: doc.meta,
        });
    }

    let view_scope = match view {
        Some(view) => {
            let selection = prov_views::select(graph, view, root_doc).await?;
            Some(selection.rows.into_iter().map(|r| r.path).collect())
        }
        None => None,
    };

    Ok(compose(spec, &rows, view_scope.as_ref()))
}

/// Flatten the readable documents of a spanning tree into `out`. Every other
/// [`NodeKind`] is skipped — a cycle marker, an unreadable file and an
/// unresolved id are all things `check` reports on, and an export must not
/// let leave what it cannot read.
fn collect(node: &prov_graph::graph::Node, out: &mut Vec<PathBuf>) {
    if matches!(node.kind, NodeKind::Doc) {
        out.push(node.path.clone());
    }
    for child in &node.children {
        collect(child, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::Gate;
    use prov_graph::meta::{Mapping, Value};

    fn spec(name: &str, value: &str, view: Option<&str>) -> ExportSpec {
        ExportSpec {
            name: name.to_string(),
            label: None,
            gate: Gate {
                field: "audience".into(),
                value: value.to_string(),
            },
            hold: None,
            view: view.map(str::to_string),
        }
    }

    fn row(path: &str, audience: Option<&[&str]>) -> Row {
        let mut meta = Mapping::new();
        if let Some(values) = audience {
            meta.insert(
                "audience".into(),
                Value::Sequence(values.iter().map(|v| Value::String((*v).into())).collect()),
            );
        }
        Row {
            path: PathBuf::from(path),
            meta: Value::Mapping(meta),
        }
    }

    fn scope(paths: &[&str]) -> HashSet<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    fn entry_paths(plan: &ExportPlan) -> Vec<&str> {
        plan.entries
            .iter()
            .map(|d| d.path.to_str().unwrap())
            .collect()
    }

    #[test]
    fn an_unarranged_export_is_the_gate_set_whole() {
        let rows = [
            row("index.md", Some(&["family"])),
            row("trip.md", Some(&["family", "friends"])),
            row("secret.md", None),
        ];
        let plan = compose(&spec("letters", "family", None), &rows, None);

        assert_eq!(entry_paths(&plan), ["index.md", "trip.md"]);
        assert!(plan.outside_view.is_empty());
        assert_eq!(plan.withheld.len(), 1);
        assert_eq!(plan.withheld[0].declared, None, "undeclared, not refused");
    }

    /// The load-bearing invariant. `daily/private.md` is inside the view's
    /// scope but declares nothing; the scope must not put it back.
    #[test]
    fn a_view_can_narrow_an_export_but_never_widen_it() {
        let rows = [
            row("daily/index.md", Some(&["family"])),
            row("daily/monday.md", Some(&["family"])),
            row("daily/private.md", None),
            row("drafts/note.md", Some(&["family"])),
        ];
        let in_scope = scope(&["daily/index.md", "daily/monday.md", "daily/private.md"]);
        let plan = compose(
            &spec("letters", "family", Some("daily")),
            &rows,
            Some(&in_scope),
        );

        assert_eq!(
            entry_paths(&plan),
            ["daily/index.md", "daily/monday.md"],
            "in scope AND admitted — the undeclared file stays out despite the scope"
        );
        assert_eq!(
            plan.outside_view,
            vec![PathBuf::from("drafts/note.md")],
            "admitted by the gate, scoped out by the view"
        );
    }

    /// The same invariant as a property: whatever the scope says — here, every
    /// document in the workspace — the result is a subset of what the gate
    /// admits.
    #[test]
    fn the_export_set_is_always_a_subset_of_the_gate_set() {
        let rows = [
            row("a.md", Some(&["family"])),
            row("b.md", Some(&["internal"])),
            row("c.md", None),
        ];
        let everything = scope(&["a.md", "b.md", "c.md"]);
        let plan = compose(
            &spec("letters", "family", Some("all")),
            &rows,
            Some(&everything),
        );

        assert_eq!(entry_paths(&plan), ["a.md"]);
        let admitted: HashSet<&Path> = rows
            .iter()
            .filter(|r| {
                Gate {
                    field: "audience".into(),
                    value: "family".into(),
                }
                .admits(&r.meta)
            })
            .map(|r| r.path.as_path())
            .collect();
        assert!(
            plan.entries
                .iter()
                .all(|d| admitted.contains(d.path.as_path()))
        );
    }

    /// Withheld keeps the why: an absent field and a field declaring other
    /// values are different findings, and the report sends the author to the
    /// right file only if it keeps them apart.
    #[test]
    fn withheld_distinguishes_undeclared_from_otherwise_declared() {
        let rows = [row("private.md", None), row("work.md", Some(&["internal"]))];
        let plan = compose(&spec("letters", "family", None), &rows, None);

        assert!(plan.entries.is_empty());
        assert_eq!(plan.withheld[0].declared, None);
        assert_eq!(
            plan.withheld[1].declared,
            Some(vec!["internal".to_string()])
        );
    }

    /// An empty scope empties the export — the narrow direction needs no
    /// special case, and the valve holds at the boundary.
    #[test]
    fn an_empty_view_scope_exports_nothing() {
        let rows = [row("a.md", Some(&["family"]))];
        let plan = compose(
            &spec("letters", "family", Some("none")),
            &rows,
            Some(&scope(&[])),
        );
        assert!(plan.entries.is_empty());
        assert_eq!(plan.outside_view, vec![PathBuf::from("a.md")]);
    }

    /// The entries carry what each document declared — the one field that
    /// answers "why is this leaving", kept so a preview can show its work.
    #[test]
    fn entries_carry_their_declared_values() {
        let rows = [row("trip.md", Some(&["family", "friends"]))];
        let plan = compose(&spec("letters", "family", None), &rows, None);
        assert_eq!(plan.entries[0].declared, ["family", "friends"]);
    }

    fn held_spec(view: Option<&str>) -> ExportSpec {
        ExportSpec {
            hold: Some("draft".into()),
            ..spec("letters", "family", view)
        }
    }

    /// A row that also declares something under the hold field.
    fn row_with(path: &str, audience: Option<&[&str]>, hold: Value) -> Row {
        let mut row = row(path, audience);
        let Value::Mapping(meta) = &mut row.meta else {
            unreachable!("row() builds a mapping");
        };
        meta.insert("draft".into(), hold);
        row
    }

    /// The hold's whole job: a document the gate admits, declaring `true`
    /// under the hold field, is not in the export — and is reported as held,
    /// with its declared values, rather than as a stranger the gate refused.
    #[test]
    fn a_held_document_is_admitted_but_does_not_leave() {
        let rows = [
            row("index.md", Some(&["family"])),
            row_with("draft.md", Some(&["family", "friends"]), Value::Bool(true)),
        ];
        let plan = compose(&held_spec(None), &rows, None);

        assert_eq!(entry_paths(&plan), ["index.md"]);
        assert_eq!(plan.held.len(), 1);
        assert_eq!(plan.held[0].path, PathBuf::from("draft.md"));
        assert_eq!(plan.held[0].declared, ["family", "friends"]);
        assert!(plan.withheld.is_empty(), "held is not withheld");
    }

    /// Only the literal `true` holds. `false` is an author un-drafting by
    /// editing rather than deleting, and any other value is not a hold —
    /// the field is a switch, not a vocabulary.
    #[test]
    fn only_true_holds() {
        for (value, leaves) in [
            (Value::Bool(true), false),
            (Value::String("true".into()), false),
            (Value::String(" true ".into()), false),
            (Value::Bool(false), true),
            (Value::String("yes".into()), true),
            (Value::String("draft".into()), true),
            (Value::Null, true),
            (Value::Sequence(vec![Value::Bool(true)]), false),
        ] {
            let rows = [row_with("a.md", Some(&["family"]), value.clone())];
            let plan = compose(&held_spec(None), &rows, None);
            assert_eq!(plan.entries.len() == 1, leaves, "for {value:?}");
            assert_eq!(plan.held.len() == 1, !leaves, "for {value:?}");
        }
    }

    /// The hold never widens: an export with no hold reads no hold field,
    /// and a document the gate holds back stays withheld whatever it says
    /// under the field — its hold is never consulted.
    #[test]
    fn a_hold_narrows_and_never_widens() {
        let rows = [
            row_with("draft.md", Some(&["family"]), Value::Bool(true)),
            row_with("private-draft.md", None, Value::Bool(true)),
            row_with("other-draft.md", Some(&["internal"]), Value::Bool(true)),
        ];

        let unheld = compose(&spec("letters", "family", None), &rows, None);
        assert_eq!(entry_paths(&unheld), ["draft.md"], "no hold, nothing held");
        assert!(unheld.held.is_empty());

        let held = compose(&held_spec(None), &rows, None);
        assert!(held.entries.is_empty());
        assert_eq!(held.held.len(), 1, "only the admitted draft is held");
        assert_eq!(
            held.withheld.len(),
            2,
            "the refused drafts are withheld, not held"
        );
        assert_eq!(held.withheld[0].declared, None);
        assert_eq!(
            held.withheld[1].declared,
            Some(vec!["internal".to_string()])
        );
    }

    /// A draft the view would also have scoped out is reported as held: the
    /// document's own word about itself comes before the workspace's
    /// arrangement, and one document is on exactly one side.
    #[test]
    fn a_held_document_outside_the_view_is_reported_as_held() {
        let rows = [
            row_with("drafts/note.md", Some(&["family"]), Value::Bool(true)),
            row("daily/monday.md", Some(&["family"])),
        ];
        let plan = compose(
            &held_spec(Some("daily")),
            &rows,
            Some(&scope(&["daily/monday.md"])),
        );

        assert_eq!(entry_paths(&plan), ["daily/monday.md"]);
        assert_eq!(plan.held[0].path, PathBuf::from("drafts/note.md"));
        assert!(plan.outside_view.is_empty(), "held once, not twice");
    }
}

// These tests read YAML frontmatter fixtures from a real directory, so they
// run under the `yaml` feature — the same arrangement as `prov-views`' select
// tests.
#[cfg(all(test, feature = "yaml"))]
mod fs_tests {
    use super::*;
    use crate::spec::Gate;
    use prov_graph::exec::block_on;
    use prov_graph::fs::StdFs;
    use prov_graph::graph::ReadSettings;
    use prov_graph::index::NoIndex;
    use prov_views::Grouping;

    use prov_testkit::write;
    fn tempdir(tag: &str) -> PathBuf {
        prov_testkit::scratch("exports", tag)
    }

    /// A journal with a `Daily/` subtree and a draft beside it. The gate field
    /// is `audience`; the interesting documents are `07-24.md` (gated in,
    /// inside the view) and `note.md` (gated in, outside the view), and
    /// `2026.md` (inside the view, undeclared — the one the valve must hold).
    fn journal(tag: &str) -> PathBuf {
        let dir = tempdir(tag);
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- daily.md\n- note.md\n---\n",
        );
        write(
            &dir,
            "note.md",
            "---\ntitle: Note\npart_of: index.md\naudience: family\n---\n",
        );
        write(
            &dir,
            "daily.md",
            "---\ntitle: Daily\npart_of: index.md\naudience: family\ncontents:\n- daily/2026.md\n---\n",
        );
        write(
            &dir,
            "daily/2026.md",
            "---\ntitle: '2026'\npart_of: ../daily.md\ncontents:\n- 07-24.md\n---\n",
        );
        write(
            &dir,
            "daily/07-24.md",
            "---\ntitle: July 24\npart_of: 2026.md\naudience:\n- family\n- friends\n---\n",
        );
        dir
    }

    fn graph(dir: &Path) -> Graph<StdFs, NoIndex> {
        Graph::new(StdFs, dir, NoIndex, ReadSettings::default())
    }

    fn export(view: Option<&str>) -> ExportSpec {
        ExportSpec {
            name: "letters".into(),
            label: None,
            gate: Gate {
                field: "audience".into(),
                value: "family".into(),
            },
            hold: None,
            view: view.map(str::to_string),
        }
    }

    fn daily_view() -> ViewSpec {
        ViewSpec {
            name: "daily".into(),
            label: None,
            icon: None,
            group: Grouping::field("title"),
            under: Some("daily.md".into()),
            filter: None,
            nest: None,
        }
    }

    fn entry_paths(plan: &ExportPlan) -> Vec<String> {
        plan.entries
            .iter()
            .map(|d| d.path.display().to_string())
            .collect()
    }

    /// The order is `Path`'s, which compares component-wise: `daily/07-24.md`
    /// precedes `daily.md` because the component `daily` sorts before
    /// `daily.md` — the same order a view's selection reads in.
    #[test]
    fn an_unarranged_export_walks_the_whole_workspace() {
        let dir = journal("whole");
        let plan = block_on(plan(&graph(&dir), &export(None), &[], "index.md")).expect("a plan");
        assert_eq!(
            entry_paths(&plan),
            ["daily/07-24.md", "daily.md", "note.md"]
        );
        // Home and 2026 are reachable, undeclared, and reported as such.
        assert_eq!(plan.withheld.len(), 2);
        assert!(plan.withheld.iter().all(|w| w.declared.is_none()));
    }

    /// End to end through a real view: the view narrows the export to the
    /// `Daily/` subtree — and `2026.md`, squarely inside that subtree,
    /// still does not leave, because the gate never admitted it.
    #[test]
    fn a_view_narrows_the_export_and_the_valve_holds() {
        let dir = journal("valve");
        let plan = block_on(plan(
            &graph(&dir),
            &export(Some("daily")),
            &[daily_view()],
            "index.md",
        ))
        .expect("a plan");

        assert_eq!(entry_paths(&plan), ["daily/07-24.md"]);
        assert_eq!(
            plan.outside_view,
            vec![PathBuf::from("daily.md"), PathBuf::from("note.md")],
            "admitted by the gate, outside the view's scope"
        );
        assert!(
            plan.withheld.iter().any(|w| w.path.ends_with("2026.md")),
            "in the view's scope, held back by the gate"
        );
    }

    /// A view that was written down and cannot be found must not fall back to
    /// the gate's whole set — the valve fails closed, as an error.
    #[test]
    fn an_unknown_view_is_an_error_not_an_unarranged_export() {
        let dir = journal("unknown-view");
        let err = block_on(plan(
            &graph(&dir),
            &export(Some("dialy")),
            &[daily_view()],
            "index.md",
        ))
        .unwrap_err();
        let Error::ViewUnknown { export, view } = &err else {
            panic!("got {err:?}");
        };
        assert_eq!(export, "letters");
        assert_eq!(view, "dialy");
    }

    /// A named view whose anchor resolves to nothing is equally an error —
    /// passed through from `prov-views`, never softened to "no view".
    #[test]
    fn a_broken_view_is_an_error_not_a_fallback() {
        let dir = journal("broken-view");
        let mut view = daily_view();
        view.under = Some("[Gone](nowhere.md)".into());
        let err = block_on(plan(
            &graph(&dir),
            &export(Some("daily")),
            &[view],
            "index.md",
        ))
        .unwrap_err();
        assert!(matches!(err, Error::View(_)), "got {err:?}");
    }

    /// End to end through a real file: a `draft: true` beside the gate value
    /// is read as a YAML boolean and holds the document, and the same file
    /// under an export with no hold leaves.
    #[test]
    fn a_draft_on_disk_is_held_by_an_export_that_holds() {
        let dir = journal("hold");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- daily.md\n- note.md\n- wip.md\n---\n",
        );
        write(
            &dir,
            "wip.md",
            "---\ntitle: Work in progress\npart_of: index.md\naudience: family\ndraft: true\n---\n",
        );
        let g = graph(&dir);

        let holding = ExportSpec {
            hold: Some("draft".into()),
            ..export(None)
        };
        let plan = block_on(plan(&g, &holding, &[], "index.md")).expect("a plan");
        assert_eq!(
            entry_paths(&plan),
            ["daily/07-24.md", "daily.md", "note.md"]
        );
        assert_eq!(plan.held.len(), 1);
        assert_eq!(plan.held[0].path, PathBuf::from("wip.md"));
        assert_eq!(plan.held[0].title.as_deref(), Some("Work in progress"));
        assert_eq!(
            plan.withheld.len(),
            2,
            "the hold moved nothing into withheld"
        );

        let unheld = block_on(super::plan(&g, &export(None), &[], "index.md")).expect("a plan");
        assert!(unheld.held.is_empty());
        assert!(entry_paths(&unheld).contains(&"wip.md".to_string()));
    }

    /// Planning twice over an unchanged workspace produces the identical
    /// plan — what lets a consumer diff a preview against the last one.
    #[test]
    fn a_plan_is_deterministic() {
        let dir = journal("stable");
        let g = graph(&dir);
        let views = [daily_view()];
        let spec = export(Some("daily"));
        let first = block_on(plan(&g, &spec, &views, "index.md")).unwrap();
        let second = block_on(plan(&g, &spec, &views, "index.md")).unwrap();
        assert_eq!(first, second);
    }
}
