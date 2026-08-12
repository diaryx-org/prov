//! What a `views:` block gets wrong, reported rather than dropped.
//!
//! [`ViewSpec::parse`](crate::ViewSpec::parse) is deliberately lossy: it
//! returns `None` for an entry it cannot make a view of, so a malformed
//! declaration cannot put a lens in a picker that groups nothing. This module
//! is the other half — the same judgment, keeping the *reason*.
//!
//! The two must agree, and the test at the bottom of this file is what holds
//! them to it: every entry this reports as unusable is one `parse` drops, and
//! every entry `parse` accepts is one this reports nothing fatal about. A
//! linter that disagreed with the parser would report a clean config prov then
//! ignored, which is the exact failure the config-issue machinery exists to
//! prevent.
//!
//! Near-miss suggestions for a misspelled key are *not* computed here: the edit
//! distance lives in `prov-config` alongside every other config near-miss, and
//! [`VIEW_KEYS`] is what this crate exports so it can be
//! computed there. One copy of the rule, in the crate that already owns it.

use prov_graph::meta::Value;

use crate::spec::{GRAINS, Grain, VIEW_KEYS, ViewSpec};

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
    /// The entry is not a mapping — `daily: date` rather than `daily: {…}`.
    NotAMapping,
    /// No `group:`, or one that is empty or not a string/list of strings. The
    /// one key a view cannot do without.
    NoGrouping,
    /// A key this format does not define. Reported so a `labl:` is caught;
    /// a near-miss suggestion is the caller's to add.
    UnknownKey,
    /// A `by:` or `nest:` whose value is not a grain.
    BadGrain {
        /// The value as written.
        value: String,
    },
}

impl ViewIssueKind {
    /// Whether this issue means the entry is not a view at all — the ones
    /// [`ViewSpec::parse`](crate::ViewSpec::parse) drops.
    pub fn is_fatal(&self) -> bool {
        matches!(self, ViewIssueKind::NotAMapping | ViewIssueKind::NoGrouping)
    }

    /// The spellings a diagnostic should offer for this issue, if any.
    pub fn expected(&self) -> &'static [&'static str] {
        match self {
            ViewIssueKind::UnknownKey => VIEW_KEYS,
            ViewIssueKind::BadGrain { .. } => GRAINS,
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
    let mut issues = Vec::new();
    if ViewSpec::parse(name, value).is_none() {
        issues.push(issue("group", ViewIssueKind::NoGrouping));
    }
    for (key, value) in map {
        match key.as_str() {
            "label" | "icon" | "under" | "group" => {}
            "by" | "nest" => {
                // `ViewSpec::parse` reads an unparseable grain as *no grain* —
                // it will not invent a cut the config did not ask for, and the
                // view stays usable by grouping on the raw values. That is the
                // right fallback and it is also completely silent, so this is
                // the only place a `by: yearr` is ever heard from.
                let text = match value {
                    Value::String(s) => s.clone(),
                    other => format!("{other:?}"),
                };
                if value.as_str().and_then(Grain::from_config_str).is_none() {
                    issues.push(issue(key, ViewIssueKind::BadGrain { value: text }));
                }
            }
            _ => issues.push(issue(key, ViewIssueKind::UnknownKey)),
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
                    ("group", "created"),
                    ("by", "month"),
                    ("under", "[Daily](id:abc1234)"),
                    ("nest", "year"),
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
    fn a_missing_group_is_reported() {
        let issues = diagnose_view("daily", &view(&[("label", "Daily")]));
        assert!(issues.iter().any(|i| i.kind == ViewIssueKind::NoGrouping));
    }

    /// The failure the fallback would otherwise hide: `ViewSpec::parse` reads an
    /// unparseable grain as no grain, so the view still works and nothing else
    /// ever says the config was wrong.
    #[test]
    fn a_misspelled_grain_is_reported_for_both_axes() {
        for key in ["by", "nest"] {
            let issues = diagnose_view("daily", &view(&[("group", "created"), (key, "yearr")]));
            assert_eq!(
                issues,
                vec![ViewIssue {
                    view: "daily".into(),
                    key: key.into(),
                    kind: ViewIssueKind::BadGrain {
                        value: "yearr".into()
                    },
                }]
            );
        }
    }

    #[test]
    fn an_unknown_key_is_reported() {
        let issues = diagnose_view("daily", &view(&[("group", "created"), ("labl", "Daily")]));
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
            view(&[("group", "  ")]),
            view(&[("group", "created")]),
            view(&[("group", "created"), ("by", "yearr")]),
            view(&[("group", "created"), ("labl", "x")]),
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
