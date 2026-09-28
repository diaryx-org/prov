//! What a `views:` block gets wrong, reported rather than dropped.
//!
//! [`ViewSpec::parse`](crate::ViewSpec::parse) is deliberately lossy: it
//! returns `None` for an entry it cannot read whole, so a broken declaration
//! cannot put a lens in a picker that shows the wrong thing. This module is the
//! other half — the same judgment, keeping the *reason*.
//!
//! The two must agree, and the test at the bottom of this file is what holds
//! them to it: every entry this reports as unusable is one `parse` drops, and
//! every entry `parse` accepts is one this reports nothing fatal about.
//!
//! Near-miss suggestions for a misspelled key are *not* computed here: the edit
//! distance lives in `prov-config` alongside every other config near-miss, and
//! [`VIEW_KEYS`] is what this crate exports so it can be computed there.

use prov_graph::meta::Value;

use crate::legacy;
use crate::spec::{RETIRED_VIEW_KEYS, VIEW_KEYS, expression, is_retired};

/// Something wrong with one `views.<name>` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewIssue {
    /// The view the entry declares.
    pub view: String,
    /// The key at fault, or the empty string when the entry as a whole is.
    pub key: String,
    /// What is wrong with it.
    pub kind: ViewIssueKind,
}

/// The kinds of thing a `views.<name>` entry gets wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewIssueKind {
    /// The entry is not a mapping — `daily: created` rather than `daily: {…}`.
    NotAMapping,
    /// No `key:`. The one key a view cannot do without.
    NoKey,
    /// A `where:` or `key:` that does not parse, or calls a function that
    /// does not exist. The message says which.
    BadExpression {
        /// Why, in a sentence.
        message: String,
    },
    /// The entry is written in the retired form — `group:`, `by:`, `under:`,
    /// `nest:`, or a `where:` mapping. Not read at all, rather than read
    /// half-way, and reported with what replaces it.
    Retired {
        /// The replacement, as YAML to paste, when the entry has enough to
        /// translate (see [`crate::legacy`]).
        replacement: Option<String>,
    },
    /// A key this format does not define. Reported so a `labl:` is caught;
    /// a near-miss suggestion is the caller's to add.
    UnknownKey,
}

impl ViewIssueKind {
    /// Whether this issue means the entry is not a view at all — the ones
    /// [`ViewSpec::parse`](crate::ViewSpec::parse) drops.
    pub fn is_fatal(&self) -> bool {
        !matches!(self, ViewIssueKind::UnknownKey)
    }

    /// The spellings a diagnostic should offer for this issue, if any.
    pub fn expected(&self) -> &'static [&'static str] {
        match self {
            ViewIssueKind::UnknownKey => VIEW_KEYS,
            _ => &[],
        }
    }
}

/// Diagnose one `views.<name>` entry.
pub fn diagnose_view(name: &str, value: &Value) -> Vec<ViewIssue> {
    let issue = |key: &str, kind| ViewIssue {
        view: name.to_string(),
        key: key.to_string(),
        kind,
    };
    let Some(map) = value.as_mapping() else {
        return vec![issue("", ViewIssueKind::NotAMapping)];
    };
    if is_retired(map) {
        let replacement = legacy::translate(value).map(|t| t.to_yaml(name));
        return vec![issue("", ViewIssueKind::Retired { replacement })];
    }
    let mut issues = Vec::new();
    match map.get("key") {
        None => issues.push(issue("key", ViewIssueKind::NoKey)),
        Some(value) => {
            if let Err(e) = expression(value) {
                issues.push(issue(
                    "key",
                    ViewIssueKind::BadExpression {
                        message: e.to_string(),
                    },
                ));
            }
        }
    }
    if let Some(value) = map.get("where")
        && let Err(e) = expression(value)
    {
        issues.push(issue(
            "where",
            ViewIssueKind::BadExpression {
                message: e.to_string(),
            },
        ));
    }
    for key in map.keys() {
        if !VIEW_KEYS.contains(&key.as_str()) && !RETIRED_VIEW_KEYS.contains(&key.as_str()) {
            issues.push(issue(key, ViewIssueKind::UnknownKey));
        }
    }
    issues
}

/// Diagnose every entry of a `views:` block, in declaration order.
pub fn diagnose_views(views: &Value) -> Vec<ViewIssue> {
    let Some(map) = views.as_mapping() else {
        return Vec::new();
    };
    map.iter()
        .flat_map(|(name, value)| diagnose_view(name, value))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::ViewSpec;
    use prov_graph::meta::Mapping;

    fn view(pairs: &[(&str, &str)]) -> Value {
        let mut map = Mapping::new();
        for (k, v) in pairs {
            map.insert((*k).into(), Value::String((*v).to_string()));
        }
        Value::Mapping(map)
    }

    #[test]
    fn a_clean_view_reports_nothing() {
        assert!(
            diagnose_view(
                "daily",
                &view(&[
                    ("label", "Daily"),
                    ("icon", "calendar"),
                    ("where", "!present(draft)"),
                    ("key", "month(first(date_of_document, created))"),
                ])
            )
            .is_empty()
        );
    }

    #[test]
    fn an_entry_that_is_not_a_mapping_is_reported_whole() {
        let issues = diagnose_view("daily", &Value::String("created".into()));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, ViewIssueKind::NotAMapping);
    }

    #[test]
    fn a_missing_key_is_reported() {
        let issues = diagnose_view("daily", &view(&[("label", "Daily")]));
        assert_eq!(issues[0].kind, ViewIssueKind::NoKey);
    }

    #[test]
    fn an_expression_that_does_not_parse_or_names_no_function_is_reported() {
        let issues = diagnose_view("daily", &view(&[("key", "dya(created)")]));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].key, "key");
        let ViewIssueKind::BadExpression { message } = &issues[0].kind else {
            panic!("{issues:?}");
        };
        assert!(message.contains("`dya`"), "{message}");

        let issues = diagnose_view("daily", &view(&[("key", "status"), ("where", "status ==")]));
        assert_eq!(issues[0].key, "where");
    }

    #[test]
    fn the_retired_form_is_reported_with_its_replacement() {
        let issues = diagnose_view(
            "daily",
            &view(&[
                ("group", "created"),
                ("by", "month"),
                ("under", "[[Daily]]"),
            ]),
        );
        assert_eq!(
            issues.len(),
            1,
            "one finding for the entry, not one per key"
        );
        let ViewIssueKind::Retired {
            replacement: Some(yaml),
        } = &issues[0].kind
        else {
            panic!("{issues:?}");
        };
        assert!(yaml.contains("key: \"month(created)\""), "{yaml}");
        assert!(
            yaml.contains("doc.ancestors.exists(a, a.title == 'Daily')"),
            "{yaml}"
        );
    }

    #[test]
    fn an_unknown_key_is_reported() {
        let issues = diagnose_view("daily", &view(&[("key", "created"), ("labl", "Daily")]));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].key, "labl");
        assert_eq!(issues[0].kind, ViewIssueKind::UnknownKey);
        assert_eq!(issues[0].kind.expected(), VIEW_KEYS);
    }

    /// The invariant that keeps the linter and the parser from drifting: an
    /// entry is dropped by `parse` if and only if the linter calls it fatal.
    #[test]
    fn fatal_issues_are_exactly_the_entries_parse_drops() {
        let cases = [
            Value::String("created".into()),
            Value::Sequence(vec![]),
            view(&[("label", "Nameless")]),
            view(&[("key", "  ")]),
            view(&[("key", "created")]),
            view(&[("key", "created"), ("where", "created ==")]),
            view(&[("key", "created"), ("labl", "x")]),
            view(&[("group", "created")]),
            view(&[("key", "created"), ("under", "[[Daily]]")]),
        ];
        for case in cases {
            let parsed = ViewSpec::parse("daily", &case).is_some();
            let fatal = diagnose_view("daily", &case)
                .iter()
                .any(|i| i.kind.is_fatal());
            assert_eq!(parsed, !fatal, "disagreed about {case:?}");
        }
    }
}
