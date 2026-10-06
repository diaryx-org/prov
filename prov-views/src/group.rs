//! Turning a [`Selection`] into groups — a pure function, no I/O.
//!
//! A [`RowSet`] *borrows* its selection rather than copying it, which is the
//! type saying what it is: a projection of a set of documents, not a second
//! copy of them. One selection can be grouped several ways at once (the same
//! query behind several lenses), and none of it goes back to disk.
//!
//! # Ordering
//!
//! Groups sort ascending by label, then by key, and rows within a group sort by
//! path. All are lexical and all are total, so grouping the same selection
//! twice produces the identical row set. A group's label is its key except
//! under a reference (below), so for every other key this is ascending by key.
//!
//! Ascending is the honest default rather than the convenient one: it is right
//! for `people` and `tags`, and wrong for a date view, where a reader wants the
//! newest first. There is deliberately no `sort:` axis yet — ordering is a
//! place the format grows teeth, and a consumer that wants newest-first
//! reverses a `Vec` it already has.
//!
//! # References group by what they name
//!
//! A key that is, character for character, a `type: ref` value the document
//! carries and the census resolved ([`Row::references`]) is keyed by the
//! document it resolves to — its workspace path — and labelled by that
//! document's title. `[Ruth Harris](id:…)` and `[Grandma](id:…)` are one
//! person, and relabelling a link must not move a document between groups. A
//! reference that resolves to nothing groups by its text, as it always did.

use std::collections::BTreeMap;

use crate::expr::{Evaluator, Expression};
use crate::select::{Clause, Failure, Row, Selection};

/// One group of a view's result.
#[derive(Debug, Clone, PartialEq)]
pub struct Group<'a> {
    /// The group key — one of the values the view's `key:` gave, or, for a
    /// value that is a resolved reference, the path of the document it names.
    pub key: String,
    /// What a person calls the group: the key, or a referenced document's
    /// title (its file stem, when it has none).
    pub label: String,
    /// The documents under this key, ordered by path.
    pub rows: Vec<&'a Row>,
}

/// A selection projected into groups.
#[derive(Debug, Clone, PartialEq)]
pub struct RowSet<'a> {
    /// The name of the view that produced this.
    pub view: String,
    /// Groups, ascending by key.
    pub groups: Vec<Group<'a>>,
    /// Documents the view's `key:` gave no key for.
    ///
    /// Reported rather than dropped: a view whose entries have all quietly
    /// stopped grouping looks exactly like an empty archive, and the difference
    /// is the whole diagnosis. A frontend labels this bucket ("Undated",
    /// "Untagged"); which words to use is a presentation decision this crate
    /// does not make.
    pub ungrouped: Vec<&'a Row>,
    /// Documents the `key:` could not be evaluated on. They are in neither
    /// [`groups`](Self::groups) nor [`ungrouped`](Self::ungrouped): "no key"
    /// is a fact about the document, and "the key failed" is a fact about the
    /// view, and a reader needs to tell the two apart.
    pub failures: Vec<Failure>,
}

impl RowSet<'_> {
    /// How many **documents** this row set covers.
    ///
    /// Not the number of rows printed: a document under two of a multi-valued
    /// field's groups is one document in two places, and counting it twice is
    /// how a view comes to claim more entries than the workspace has. The
    /// placements are [`placements`](Self::placements).
    pub fn len(&self) -> usize {
        let mut paths: Vec<_> = self
            .groups
            .iter()
            .flat_map(|g| &g.rows)
            .chain(&self.ungrouped)
            .map(|r| &r.path)
            .collect();
        paths.sort();
        paths.dedup();
        paths.len()
    }

    /// Whether the view grouped nothing at all.
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty() && self.ungrouped.is_empty()
    }

    /// How many rows a renderer will draw — one per document *per group it
    /// falls into*, which is what makes it different from [`len`](Self::len).
    pub fn placements(&self) -> usize {
        self.groups.iter().map(|g| g.rows.len()).sum::<usize>() + self.ungrouped.len()
    }
}

/// Group `selection` by the `key` expression.
///
/// Pure: no I/O and no workspace. Every row of the selection lands in at least
/// one group, in [`ungrouped`](RowSet::ungrouped), or in
/// [`failures`](RowSet::failures), so nothing selected can go missing on the
/// way to being displayed.
pub fn group<'a>(selection: &'a Selection, key: &Expression) -> RowSet<'a> {
    let evaluator = Evaluator::new();
    let mut grouped: BTreeMap<String, (String, Vec<&'a Row>)> = BTreeMap::new();
    let mut ungrouped: Vec<&'a Row> = Vec::new();
    let mut failures = Vec::new();

    for row in &selection.rows {
        // Deduplicated by the evaluator: a field may repeat a value
        // (`people: [Ada, Ada]`), and one document belongs to a group once.
        // Two labels for one reference are deduplicated below, once resolved.
        let keys = match evaluator.keys(key, row) {
            Ok(keys) => keys,
            Err(message) => {
                failures.push(Failure {
                    path: row.path.clone(),
                    clause: Clause::Key,
                    message,
                });
                continue;
            }
        };
        if keys.is_empty() {
            ungrouped.push(row);
            continue;
        }
        for key in keys {
            let (key, label) = resolved(row, key);
            let (_, rows) = grouped.entry(key).or_insert_with(|| (label, Vec::new()));
            if !rows.last().is_some_and(|last| std::ptr::eq(*last, row)) {
                rows.push(row);
            }
        }
    }

    // The selection was in path order, so each bucket is too; the groups are
    // put in label order here, `BTreeMap` having ordered the keys that break a
    // tie between two labels.
    let mut groups: Vec<Group<'a>> = grouped
        .into_iter()
        .map(|(key, (label, rows))| Group { key, label, rows })
        .collect();
    groups.sort_by(|a, b| a.label.cmp(&b.label));

    RowSet {
        view: selection.view.clone(),
        groups,
        ungrouped,
        failures,
    }
}

/// The key and label a key's text files `row` under: the referenced
/// document's path and title where the text is one of the row's resolved
/// references, else the text for both.
fn resolved(row: &Row, key: String) -> (String, String) {
    let Some(reference) = row.references.iter().find(|r| r.raw == key) else {
        return (key.clone(), key);
    };
    let label = reference.title.clone().unwrap_or_else(|| {
        reference
            .target
            .file_stem()
            .map_or_else(|| key.clone(), |s| s.to_string_lossy().into_owned())
    });
    (prov_graph::manifest::slash_path(&reference.target), label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::select::Ancestor;
    use prov_graph::meta::{Mapping, Value};
    use std::path::PathBuf;

    /// The payoff of the split: a selection is a plain value, so every grouping
    /// question is answered without a filesystem.
    fn selection(rows: &[(&str, &[(&str, Value)])]) -> Selection {
        Selection {
            view: "v".into(),
            rows: rows
                .iter()
                .map(|(path, fields)| {
                    let mut meta = Mapping::new();
                    for (k, v) in *fields {
                        meta.insert((*k).into(), v.clone());
                    }
                    Row {
                        path: PathBuf::from(path),
                        id: None,
                        ancestors: Vec::<Ancestor>::new(),
                        meta: Value::Mapping(meta),
                        references: Vec::new(),
                    }
                })
                .collect(),
            failures: Vec::new(),
        }
    }

    fn text(s: &str) -> Value {
        Value::String(s.to_string())
    }

    fn key(src: &str) -> Expression {
        Expression::parse(src).expect(src)
    }

    fn seq(items: &[&str]) -> Value {
        Value::Sequence(items.iter().map(|s| text(s)).collect())
    }

    #[test]
    fn groups_are_ascending_and_rows_stay_in_path_order() {
        let sel = selection(&[
            ("b.md", &[("created", text("2026-08-01"))]),
            ("a.md", &[("created", text("2026-07-24"))]),
            ("c.md", &[("created", text("2026-07-30"))]),
        ]);
        let rows = group(&sel, &key("month(created)"));
        assert_eq!(
            rows.groups
                .iter()
                .map(|g| g.key.as_str())
                .collect::<Vec<_>>(),
            ["2026-07", "2026-08"]
        );
        assert_eq!(
            rows.groups[0]
                .rows
                .iter()
                .map(|r| r.path.to_str().unwrap())
                .collect::<Vec<_>>(),
            ["a.md", "c.md"]
        );
    }

    /// The wart the split was for: a document under two groups is *one*
    /// document, and `len` says so while `placements` counts the rows drawn.
    #[test]
    fn len_counts_documents_and_placements_counts_rows() {
        let sel = selection(&[
            ("letter.md", &[("people", seq(&["Ada", "Grace"]))]),
            ("note.md", &[("people", seq(&["Ada"]))]),
            ("bare.md", &[]),
        ]);
        let rows = group(&sel, &key("people"));

        assert_eq!(rows.len(), 3, "three documents");
        assert_eq!(
            rows.placements(),
            4,
            "Ada twice, Grace once, ungrouped once"
        );
        assert_eq!(rows.len(), sel.len(), "nothing selected went missing");
    }

    /// A repeated value is one membership, not two.
    #[test]
    fn a_repeated_value_does_not_double_a_row_within_its_group() {
        let sel = selection(&[("letter.md", &[("people", seq(&["Ada", "Ada"]))])]);
        let rows = group(&sel, &key("people"));
        assert_eq!(rows.groups.len(), 1);
        assert_eq!(rows.groups[0].rows.len(), 1);
    }

    /// Grouping is total: every selected row is reachable afterwards.
    #[test]
    fn every_selected_row_lands_somewhere() {
        let sel = selection(&[
            ("a.md", &[("created", text("2026-07-24"))]),
            ("b.md", &[("created", text("banana"))]),
            ("c.md", &[]),
        ]);
        let rows = group(&sel, &key("year(created)"));
        assert_eq!(rows.len(), 3);
        assert_eq!(rows.ungrouped.len(), 2, "the unparseable and the absent");
    }

    /// An interval is one document in several groups, like a letter about two
    /// people — and, like that letter, one document when counted.
    #[test]
    fn an_interval_is_under_every_year_it_spans() {
        let sel = selection(&[
            ("birth.md", &[("date_of_document", text("1918/1920"))]),
            ("photo.md", &[("date_of_document", text("1919~"))]),
            ("scan.md", &[("date_of_document", text("XXXX"))]),
        ]);
        let rows = group(&sel, &key("year(date_of_document)"));
        assert_eq!(
            rows.groups
                .iter()
                .map(|g| (g.key.as_str(), g.rows.len()))
                .collect::<Vec<_>>(),
            [("1918", 1), ("1919", 2), ("1920", 1)]
        );
        assert_eq!(rows.ungrouped.len(), 1, "undated, on purpose");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows.placements(), 5);
    }

    /// One selection, several lenses — what borrowing rather than copying is
    /// for, and what a frontend offering a view switcher actually does.
    #[test]
    fn one_selection_groups_several_ways_at_once() {
        let sel = selection(&[(
            "letter.md",
            &[("people", seq(&["Ada"])), ("created", text("2026-07-24"))],
        )]);
        let by_people = group(&sel, &key("people"));
        let by_year = group(&sel, &key("year(created)"));
        assert_eq!(by_people.groups[0].key, "Ada");
        assert_eq!(by_year.groups[0].key, "2026");
        assert_eq!(
            by_people.groups[0].rows[0].path,
            by_year.groups[0].rows[0].path
        );
    }

    /// The union the `group:` chain could not say: a document under every day
    /// it has a date for, and once under a day both dates fall on.
    #[test]
    fn a_union_puts_a_document_under_each_of_its_keys() {
        let sel = selection(&[
            (
                "a.md",
                &[
                    ("created", text("2026-09-01")),
                    ("updated", text("2026-09-20T08:00:00Z")),
                ],
            ),
            (
                "b.md",
                &[
                    ("created", text("2026-09-01")),
                    ("updated", text("2026-09-01")),
                ],
            ),
        ]);
        let rows = group(&sel, &key("[day(created), day(updated)]"));
        assert_eq!(
            rows.groups
                .iter()
                .map(|g| (g.key.as_str(), g.rows.len()))
                .collect::<Vec<_>>(),
            [("2026-09-01", 2), ("2026-09-20", 1)]
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows.placements(), 3);
    }

    /// A key that fails is reported, and the document is neither grouped nor
    /// called ungrouped.
    #[test]
    fn a_failing_key_is_a_failure_not_ungrouped() {
        let sel = selection(&[("a.md", &[("people", seq(&["Ada"]))])]);
        let rows = group(&sel, &key("doc.meta"));
        assert!(rows.groups.is_empty());
        assert!(rows.ungrouped.is_empty());
        assert_eq!(rows.failures.len(), 1);
        assert_eq!(rows.failures[0].clause, Clause::Key);
    }

    /// Two labels for one record are one group, keyed by the record and
    /// titled by it, and a document carrying both is in it once. A reference
    /// the census could not resolve keeps its text, and sorts among the
    /// titles by it.
    #[test]
    fn a_resolved_reference_groups_by_its_target_and_is_titled_by_it() {
        let ruth = |raw: &str| crate::select::Reference {
            raw: raw.into(),
            target: PathBuf::from("people/ruth.md"),
            title: Some("Ruth Harris".into()),
        };
        let mut sel = selection(&[
            ("a.md", &[("people", seq(&["[Ruth Harris](id:rth0001)"]))]),
            (
                "b.md",
                &[(
                    "people",
                    seq(&["[Grandma](id:rth0001)", "[Nan](people/ruth.md)", "Ada"]),
                )],
            ),
        ]);
        sel.rows[0].references = vec![ruth("[Ruth Harris](id:rth0001)")];
        sel.rows[1].references = vec![ruth("[Grandma](id:rth0001)"), ruth("[Nan](people/ruth.md)")];

        let rows = group(&sel, &key("people"));
        assert_eq!(
            rows.groups
                .iter()
                .map(|g| (g.key.as_str(), g.label.as_str(), g.rows.len()))
                .collect::<Vec<_>>(),
            [("Ada", "Ada", 1), ("people/ruth.md", "Ruth Harris", 2)]
        );
        assert_eq!(rows.placements(), 3);
    }
}
