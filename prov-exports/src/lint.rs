//! What an `exports:` block gets wrong, reported rather than dropped.
//!
//! [`ExportSpec::parse`](crate::ExportSpec::parse) is deliberately lossy: it
//! returns `None` for an entry it cannot make an export of, so a malformed
//! declaration cannot put an exportable set behind a gate nobody wrote. That
//! is the fail-closed direction, and it is also completely silent — this
//! module is the other half, the same judgment keeping the *reason*.
//!
//! The stakes are higher here than for a view. A view that goes unread is a
//! lens missing from a picker; an export that goes unread is a publish step
//! that quietly publishes nothing — which an author notices — or a gate typo
//! that holds back documents someone meant to share, which they may not. The
//! parity test at the bottom holds parser and linter to one judgment, exactly
//! as `prov-views` does.
//!
//! Near-miss suggestions for a misspelled key are *not* computed here: the
//! edit distance lives in `prov-config` alongside every other config
//! near-miss, and [`EXPORT_KEYS`]/[`GATE_KEYS`] are what this crate exports so
//! it can be computed there.

use prov_graph::meta::Value;

use crate::spec::{EXPORT_KEYS, ExportSpec, GATE_KEYS};

/// Something wrong with one `exports.<name>` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportIssue {
    /// The export the entry declares.
    pub export: String,
    /// The key at fault, or the empty string when the entry as a whole is.
    pub key: String,
    /// What is wrong with it.
    pub kind: ExportIssueKind,
}

/// The kinds of thing an `exports.<name>` entry gets wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportIssueKind {
    /// The entry is not a mapping — `letters: family` rather than
    /// `letters: {…}`.
    NotAMapping,
    /// No readable gate: `gate:` is absent, not a mapping, or missing a
    /// non-empty `field` or `value`. The one key an export cannot do without,
    /// and there is no shorthand form — a gate that does not say both halves
    /// would have to guess one, and either guess exports a set nobody chose.
    NoGate,
    /// A key this format does not define, at the entry level.
    UnknownKey,
    /// A key inside `gate:` this format does not define. Reported separately
    /// from [`UnknownKey`](Self::UnknownKey) because the accepted spellings
    /// differ, and a stray key inside a gate is the likeliest place for a
    /// meaningful word (`audience:`, `values:`) to land and silently do
    /// nothing.
    GateUnknownKey,
}

impl ExportIssueKind {
    /// Whether this issue means the entry is not an export at all — the ones
    /// [`ExportSpec::parse`](crate::ExportSpec::parse) drops.
    pub fn is_fatal(&self) -> bool {
        matches!(self, ExportIssueKind::NotAMapping | ExportIssueKind::NoGate)
    }

    /// The spellings a diagnostic should offer for this issue, if any.
    pub fn expected(&self) -> &'static [&'static str] {
        match self {
            ExportIssueKind::UnknownKey => EXPORT_KEYS,
            ExportIssueKind::GateUnknownKey | ExportIssueKind::NoGate => GATE_KEYS,
            ExportIssueKind::NotAMapping => &[],
        }
    }
}

/// Diagnose one `exports.<name>` entry.
pub fn diagnose_export(name: &str, value: &Value) -> Vec<ExportIssue> {
    let issue = |key: &str, kind| ExportIssue {
        export: name.to_string(),
        key: key.to_string(),
        kind,
    };
    let Some(map) = value.as_mapping() else {
        return vec![issue("", ExportIssueKind::NotAMapping)];
    };
    let mut issues = Vec::new();
    if ExportSpec::parse(name, value).is_none() {
        issues.push(issue("gate", ExportIssueKind::NoGate));
    }
    for (key, value) in map {
        match key.as_str() {
            "label" | "view" => {}
            "gate" => {
                let Some(gate) = value.as_mapping() else {
                    // Already reported as `NoGate` above; the shape says the
                    // rest.
                    continue;
                };
                for (gate_key, _) in gate {
                    if !GATE_KEYS.contains(&gate_key.as_str()) {
                        issues.push(issue(gate_key, ExportIssueKind::GateUnknownKey));
                    }
                }
            }
            _ => issues.push(issue(key, ExportIssueKind::UnknownKey)),
        }
    }
    issues
}

/// Diagnose every entry of an `exports:` block, in declaration order.
pub fn diagnose_exports(exports: &Value) -> Vec<ExportIssue> {
    let Some(map) = exports.as_mapping() else {
        return Vec::new();
    };
    map.iter()
        .flat_map(|(name, value)| diagnose_export(name, value))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use prov_graph::meta::Mapping;

    fn entry(pairs: &[(&str, Value)]) -> Value {
        let mut map = Mapping::new();
        for (k, v) in pairs {
            map.insert((*k).into(), v.clone());
        }
        Value::Mapping(map)
    }

    fn gate(pairs: &[(&str, &str)]) -> Value {
        let mut map = Mapping::new();
        for (k, v) in pairs {
            map.insert((*k).into(), Value::String((*v).to_string()));
        }
        Value::Mapping(map)
    }

    fn good_gate() -> Value {
        gate(&[("field", "audience"), ("value", "family")])
    }

    #[test]
    fn a_clean_export_reports_nothing() {
        assert!(
            diagnose_export(
                "letters",
                &entry(&[
                    ("label", Value::String("Letters home".into())),
                    ("gate", good_gate()),
                    ("view", Value::String("daily".into())),
                ])
            )
            .is_empty()
        );
    }

    #[test]
    fn an_entry_that_is_not_a_mapping_is_reported_whole() {
        let issues = diagnose_export("letters", &Value::String("family".into()));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, ExportIssueKind::NotAMapping);
        assert_eq!(issues[0].key, "");
    }

    /// The silent failure this linter exists for: every one of these exports
    /// nothing, and without the report nothing would ever say why.
    #[test]
    fn a_missing_or_unreadable_gate_is_reported() {
        for broken in [
            entry(&[("view", Value::String("daily".into()))]),
            entry(&[("gate", Value::String("family".into()))]),
            entry(&[("gate", gate(&[("field", "audience")]))]),
            entry(&[("gate", gate(&[("value", "family")]))]),
            entry(&[("gate", gate(&[("field", "audience"), ("value", "  ")]))]),
        ] {
            let issues = diagnose_export("letters", &broken);
            assert!(
                issues.iter().any(|i| i.kind == ExportIssueKind::NoGate),
                "for {broken:?}"
            );
            assert_eq!(issues[0].kind.expected(), GATE_KEYS);
        }
    }

    #[test]
    fn an_unknown_key_is_reported_at_both_levels() {
        let issues = diagnose_export(
            "letters",
            &entry(&[("gate", good_gate()), ("veiw", Value::String("daily".into()))]),
        );
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].key, "veiw");
        assert_eq!(issues[0].kind, ExportIssueKind::UnknownKey);
        assert_eq!(issues[0].kind.expected(), EXPORT_KEYS);

        let issues = diagnose_export(
            "letters",
            &entry(&[(
                "gate",
                gate(&[("field", "audience"), ("value", "family"), ("audience", "x")]),
            )]),
        );
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].key, "audience");
        assert_eq!(issues[0].kind, ExportIssueKind::GateUnknownKey);
        assert_eq!(issues[0].kind.expected(), GATE_KEYS);
    }

    /// The invariant that keeps the linter and the parser from drifting: an
    /// entry is dropped by `parse` if and only if the linter calls it fatal.
    #[test]
    fn fatal_issues_are_exactly_the_entries_parse_drops() {
        let cases = [
            Value::String("family".into()),
            Value::Sequence(vec![]),
            entry(&[("label", Value::String("Nameless".into()))]),
            entry(&[("gate", Value::String("family".into()))]),
            entry(&[("gate", gate(&[("field", "audience")]))]),
            entry(&[("gate", good_gate())]),
            entry(&[("gate", good_gate()), ("veiw", Value::String("daily".into()))]),
            entry(&[(
                "gate",
                gate(&[("field", "audience"), ("value", "family"), ("extra", "x")]),
            )]),
        ];
        for case in cases {
            let parsed = ExportSpec::parse("letters", &case).is_some();
            let fatal = diagnose_export("letters", &case)
                .iter()
                .any(|i| i.kind.is_fatal());
            assert_eq!(parsed, !fatal, "disagreed about {case:?}");
        }
    }
}
