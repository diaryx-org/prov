//! `where:` — the conditions a document must meet to be in a view.
//!
//! Scope ([`under`](crate::ViewSpec::under)) says *where* a view looks; this
//! says *which of what it finds* belongs. The two are separate because they
//! fail differently: an anchor that names nothing is a broken view, while a
//! condition that matches nothing is an ordinary empty answer.
//!
//! # A closed vocabulary, deliberately
//!
//! Two predicates — [`Has`](Condition::Has) and [`Equals`](Condition::Equals) —
//! and three combinators. That is enough to express the real filtering that
//! exists today (a publishing audience: *this document declares an `audience`,
//! and it is `public`*) and it is deliberately nowhere near an expression
//! language.
//!
//! Formulas are the point of no return for a view format: once views with
//! arbitrary expressions exist in the wild, the expression grammar is
//! load-bearing forever and every reader of the format has to implement it. A
//! closed set of named predicates can grow one member at a time, each with a
//! reason; a grammar cannot be taken back. So the rule for adding to this enum
//! is a concrete lens that cannot otherwise be said — not a shape that seems
//! likely to be wanted.
//!
//! # The spelling
//!
//! ```yaml
//! where:
//!   has: people                    # present, and not empty
//!   equals: { audience: public }   # carries this value
//! ```
//!
//! A mapping with several keys is an implicit **and** — every condition must
//! hold. So is a list given to `has:`, and so is a multi-key `equals:`. The
//! other two combinators are explicit:
//!
//! ```yaml
//! where:
//!   any-of:
//!     - equals: { audience: public }
//!     - equals: { audience: friends }
//!   not:
//!     has: draft
//! ```

use prov_graph::meta::{Mapping, Value};

use crate::spec::scalar_texts;

/// The keys valid inside a `where:` block.
pub const CONDITION_KEYS: &[&str] = &["has", "equals", "not", "any-of", "all-of"];

/// A condition a document's metadata must satisfy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Condition {
    /// The field is present and carries at least one non-empty value.
    ///
    /// "Present" means *usable*, not merely written: a `people:` with an empty
    /// string under it has nothing to group or display, and a view that
    /// included it would be showing a row it cannot say anything about.
    Has(String),
    /// The field carries this value — any element of it, for a sequence, so
    /// `equals: { people: Ada }` matches a document listing several people.
    Equals {
        /// The field to read.
        field: String,
        /// The value it must carry, compared as text.
        value: String,
    },
    /// The inverse of the condition it wraps.
    Not(Box<Condition>),
    /// Every condition must hold. An empty list holds vacuously, which is what
    /// makes it the identity a multi-key mapping folds into.
    AllOf(Vec<Condition>),
    /// At least one condition must hold. An empty list holds for nothing, so a
    /// `any-of: []` selects nothing rather than everything — the reading that
    /// cannot silently publish a whole workspace.
    AnyOf(Vec<Condition>),
}

impl Condition {
    /// Whether `meta` satisfies this condition.
    pub fn matches(&self, meta: &Value) -> bool {
        match self {
            Condition::Has(field) => meta.get(field).is_some_and(|v| !scalar_texts(v).is_empty()),
            Condition::Equals { field, value } => meta
                .get(field)
                .is_some_and(|v| scalar_texts(v).iter().any(|t| t == value)),
            Condition::Not(inner) => !inner.matches(meta),
            Condition::AllOf(all) => all.iter().all(|c| c.matches(meta)),
            Condition::AnyOf(any) => any.iter().any(|c| c.matches(meta)),
        }
    }

    /// Read a `where:` block.
    ///
    /// Returns `None` for a value that is not a mapping, or one that yields no
    /// condition at all — an empty `where:` is not a filter that excludes
    /// everything, it is a view that did not say anything, and treating it as
    /// the former would hide a whole workspace behind a typo.
    pub fn parse(value: &Value) -> Option<Self> {
        let map = value.as_mapping()?;
        let mut conditions = Vec::new();
        for (key, value) in map {
            match key.as_str() {
                "has" => conditions.extend(fields_of(value).into_iter().map(Condition::Has)),
                "equals" => conditions.extend(equalities_of(value)),
                "not" => {
                    conditions.extend(Condition::parse(value).map(|c| Condition::Not(c.into())))
                }
                "any-of" => conditions.extend(branch(value, Condition::AnyOf)),
                "all-of" => conditions.extend(branch(value, Condition::AllOf)),
                _ => {}
            }
        }
        match conditions.len() {
            0 => None,
            // A single condition is written back as itself rather than wrapped,
            // so a round trip does not accrete `all-of` layers.
            1 => conditions.pop(),
            _ => Some(Condition::AllOf(conditions)),
        }
    }

    /// The mapping this condition writes back as.
    pub fn to_value(&self) -> Value {
        let mut map = Mapping::new();
        match self {
            Condition::Has(field) => {
                map.insert("has".into(), Value::String(field.clone()));
            }
            Condition::Equals { field, value } => {
                let mut pairs = Mapping::new();
                pairs.insert(field.clone(), Value::String(value.clone()));
                map.insert("equals".into(), Value::Mapping(pairs));
            }
            Condition::Not(inner) => {
                map.insert("not".into(), inner.to_value());
            }
            Condition::AllOf(all) => {
                map.insert(
                    "all-of".into(),
                    Value::Sequence(all.iter().map(Condition::to_value).collect()),
                );
            }
            Condition::AnyOf(any) => {
                map.insert(
                    "any-of".into(),
                    Value::Sequence(any.iter().map(Condition::to_value).collect()),
                );
            }
        }
        Value::Mapping(map)
    }
}

/// A field name, or a list of them — the `has:` shapes.
fn fields_of(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) => non_empty(s).into_iter().collect(),
        Value::Sequence(items) => items
            .iter()
            .filter_map(Value::as_str)
            .filter_map(non_empty)
            .collect(),
        _ => Vec::new(),
    }
}

/// The `field: value` pairs of an `equals:` mapping.
fn equalities_of(value: &Value) -> Vec<Condition> {
    let Some(map) = value.as_mapping() else {
        return Vec::new();
    };
    map.iter()
        .filter_map(|(field, v)| {
            let field = non_empty(field)?;
            // Compared as text, the same way a group key is derived, so
            // `equals: { rating: 5 }` matches whether the frontmatter wrote
            // `5` or `"5"` — a view must not depend on which of those a format
            // happened to round-trip.
            let value = scalar_texts(v).into_iter().next()?;
            Some(Condition::Equals { field, value })
        })
        .collect()
}

/// A list of sub-conditions under `any-of`/`all-of`.
fn branch(value: &Value, build: fn(Vec<Condition>) -> Condition) -> Option<Condition> {
    let items = value.as_sequence()?;
    let parsed: Vec<Condition> = items.iter().filter_map(Condition::parse).collect();
    (!parsed.is_empty()).then(|| build(parsed))
}

fn non_empty(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(pairs: &[(&str, Value)]) -> Value {
        let mut map = Mapping::new();
        for (k, v) in pairs {
            map.insert((*k).into(), v.clone());
        }
        Value::Mapping(map)
    }

    fn text(s: &str) -> Value {
        Value::String(s.to_string())
    }

    fn seq(items: &[&str]) -> Value {
        Value::Sequence(items.iter().map(|s| text(s)).collect())
    }

    fn parse(yaml_ish: &Value) -> Condition {
        Condition::parse(yaml_ish).expect("a condition")
    }

    #[test]
    fn has_means_present_and_not_empty() {
        let c = parse(&doc(&[("has", text("people"))]));
        assert!(c.matches(&doc(&[("people", text("Ada"))])));
        assert!(c.matches(&doc(&[("people", seq(&["Ada"]))])));
        assert!(!c.matches(&doc(&[])));
        assert!(
            !c.matches(&doc(&[("people", text("  "))])),
            "written but unusable"
        );
        assert!(!c.matches(&doc(&[("people", Value::Sequence(vec![]))])));
    }

    #[test]
    fn equals_matches_any_element_of_a_sequence() {
        let c = parse(&doc(&[("equals", doc(&[("people", text("Grace"))]))]));
        assert!(c.matches(&doc(&[("people", seq(&["Ada", "Grace"]))])));
        assert!(!c.matches(&doc(&[("people", seq(&["Ada"]))])));
    }

    /// A view must not depend on whether a format round-tripped `5` as a number
    /// or a string.
    #[test]
    fn equals_compares_as_text_across_scalar_kinds() {
        let c = parse(&doc(&[("equals", doc(&[("rating", Value::Int(5))]))]));
        assert!(c.matches(&doc(&[("rating", Value::Int(5))])));
        assert!(c.matches(&doc(&[("rating", text("5"))])));
    }

    /// Several keys in one mapping are an implicit and — the shape a publishing
    /// audience actually takes.
    #[test]
    fn a_multi_key_block_is_an_implicit_and() {
        let c = parse(&doc(&[
            ("has", text("audience")),
            ("equals", doc(&[("audience", text("public"))])),
        ]));
        assert!(c.matches(&doc(&[("audience", text("public"))])));
        assert!(!c.matches(&doc(&[("audience", text("private"))])));
        assert!(!c.matches(&doc(&[])));
    }

    #[test]
    fn any_of_and_not_combine() {
        let c = parse(&doc(&[(
            "any-of",
            Value::Sequence(vec![
                doc(&[("equals", doc(&[("audience", text("public"))]))]),
                doc(&[("equals", doc(&[("audience", text("friends"))]))]),
            ]),
        )]));
        assert!(c.matches(&doc(&[("audience", text("friends"))])));
        assert!(!c.matches(&doc(&[("audience", text("private"))])));

        let c = parse(&doc(&[("not", doc(&[("has", text("draft"))]))]));
        assert!(c.matches(&doc(&[])));
        assert!(!c.matches(&doc(&[("draft", Value::Bool(true))])));
    }

    /// An empty `any-of` selects nothing. The other reading — that it holds
    /// vacuously — would publish a whole workspace on a typo.
    #[test]
    fn an_empty_where_is_not_a_filter_and_an_empty_any_of_selects_nothing() {
        assert!(Condition::parse(&doc(&[])).is_none());
        assert!(Condition::parse(&text("people")).is_none());
        assert!(
            Condition::parse(&doc(&[("any-of", Value::Sequence(vec![]))])).is_none(),
            "nothing to combine is not a condition; the linter reports the shape"
        );
        assert!(!Condition::AnyOf(Vec::new()).matches(&doc(&[])));
        assert!(Condition::AllOf(Vec::new()).matches(&doc(&[])));
    }

    #[test]
    fn conditions_round_trip() {
        for condition in [
            Condition::Has("people".into()),
            Condition::Equals {
                field: "audience".into(),
                value: "public".into(),
            },
            Condition::Not(Box::new(Condition::Has("draft".into()))),
            Condition::AllOf(vec![
                Condition::Has("audience".into()),
                Condition::Equals {
                    field: "audience".into(),
                    value: "public".into(),
                },
            ]),
            Condition::AnyOf(vec![
                Condition::Equals {
                    field: "audience".into(),
                    value: "public".into(),
                },
                Condition::Equals {
                    field: "audience".into(),
                    value: "friends".into(),
                },
            ]),
        ] {
            let back = Condition::parse(&condition.to_value()).expect("re-reads");
            assert_eq!(back, condition);
        }
    }
}
