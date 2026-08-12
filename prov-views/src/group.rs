//! Turning a [`Selection`] into groups — a pure function, no I/O.
//!
//! A [`RowSet`] *borrows* its selection rather than copying it, which is the
//! type saying what it is: a projection of a set of documents, not a second
//! copy of them. One selection can be grouped several ways at once (the same
//! query behind several lenses), and none of it goes back to disk.
//!
//! # Ordering
//!
//! Groups sort ascending by key, and rows within a group sort by path. Both are
//! lexical and both are total, so grouping the same selection twice produces
//! the identical row set.
//!
//! Ascending is the honest default rather than the convenient one: it is right
//! for `people` and `tags`, and wrong for a date view, where a reader wants the
//! newest first. There is deliberately no `sort:` axis yet — ordering, like
//! formulas, is a place the format grows teeth, and a consumer that wants
//! newest-first reverses a `Vec` it already has.

use std::collections::BTreeMap;

use crate::select::{Row, Selection};
use crate::spec::Grouping;

/// One group of a view's result.
#[derive(Debug, Clone, PartialEq)]
pub struct Group<'a> {
    /// The group key — a field value, or a value cut at the view's grain.
    pub key: String,
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
    /// Documents in scope that no field in the grouping chain gave a usable
    /// value for.
    ///
    /// Reported rather than dropped: a view whose entries have all quietly
    /// stopped grouping looks exactly like an empty archive, and the difference
    /// is the whole diagnosis. A frontend labels this bucket ("Undated",
    /// "Untagged"); which words to use is a presentation decision this crate
    /// does not make.
    pub ungrouped: Vec<&'a Row>,
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

/// Group `selection` by `grouping`.
///
/// Pure, and total: every row of the selection lands in at least one group or
/// in [`ungrouped`](RowSet::ungrouped), so nothing selected can go missing on
/// the way to being displayed.
pub fn group<'a>(selection: &'a Selection, grouping: &Grouping) -> RowSet<'a> {
    let mut grouped: BTreeMap<String, Vec<&'a Row>> = BTreeMap::new();
    let mut ungrouped: Vec<&'a Row> = Vec::new();

    for row in &selection.rows {
        let keys = grouping.keys_of(&row.meta);
        if keys.is_empty() {
            ungrouped.push(row);
            continue;
        }
        for key in keys {
            let bucket = grouped.entry(key).or_default();
            // A field may repeat a value (`people: [Ada, Ada]`); one document
            // belongs to a group once.
            if !bucket.iter().any(|r| r.path == row.path) {
                bucket.push(row);
            }
        }
    }

    // `BTreeMap` ordered the keys; the selection was already in path order, so
    // each bucket is too.
    let groups = grouped
        .into_iter()
        .map(|(key, rows)| Group { key, rows })
        .collect();

    RowSet {
        view: selection.view.clone(),
        groups,
        ungrouped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::Grain;
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
                        meta: Value::Mapping(meta),
                    }
                })
                .collect(),
        }
    }

    fn text(s: &str) -> Value {
        Value::String(s.to_string())
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
        let rows = group(
            &sel,
            &Grouping {
                keys: vec!["created".into()],
                by: Some(Grain::Month),
            },
        );
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
        let rows = group(&sel, &Grouping::field("people"));

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
        let rows = group(&sel, &Grouping::field("people"));
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
        let rows = group(
            &sel,
            &Grouping {
                keys: vec!["created".into()],
                by: Some(Grain::Year),
            },
        );
        assert_eq!(rows.len(), 3);
        assert_eq!(rows.ungrouped.len(), 2, "the unparseable and the absent");
    }

    /// One selection, several lenses — what borrowing rather than copying is
    /// for, and what a frontend offering a view switcher actually does.
    #[test]
    fn one_selection_groups_several_ways_at_once() {
        let sel = selection(&[(
            "letter.md",
            &[("people", seq(&["Ada"])), ("created", text("2026-07-24"))],
        )]);
        let by_people = group(&sel, &Grouping::field("people"));
        let by_year = group(
            &sel,
            &Grouping {
                keys: vec!["created".into()],
                by: Some(Grain::Year),
            },
        );
        assert_eq!(by_people.groups[0].key, "Ada");
        assert_eq!(by_year.groups[0].key, "2026");
        assert_eq!(
            by_people.groups[0].rows[0].path,
            by_year.groups[0].rows[0].path
        );
    }
}
