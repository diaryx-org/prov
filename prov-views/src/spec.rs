//! The view format: what a workspace declares under `views.<name>`.
//!
//! A view is two expressions — which documents, and which group each goes
//! under — with a name a person can call it by:
//!
//! ```yaml
//! views:
//!   daily:
//!     label: Daily
//!     icon: calendar
//!     where: "under('Daily') && !present(draft)"
//!     key: month(first(date_of_document, created))
//! ```
//!
//! Both are [CEL](crate::expr), over the document's fields and the document
//! itself. `where:` is optional — without it a view covers every document the
//! workspace reaches — and `key:` is not, because a view is a way of grouping.
//!
//! # Why a view is not a field declaration
//!
//! A declared field (`fields.<name>`) already makes a lens: the workspace says
//! it files things by `people`, so a frontend groups by `people`. That covers a
//! lens whose groups *are* one field's values, over the whole corpus. A view
//! is what a real archive needs beyond that: a narrower set of documents, a
//! value cut to a year or a first letter, the first of several fields that is
//! filled in, or a union of several.
//!
//! # There is no `date` grouping
//!
//! An earlier form of this format spelled a date view `group: date`, a token
//! that meant "the date chain" — and the chain itself (`date_of_document` →
//! `created` → `updated`) was hardcoded in whichever program was reading.
//! Here the chain is `first(date_of_document, created, updated)`, a
//! declaration a workspace writes, and nothing in this crate knows the word
//! "date". The date functions cut a value, not a declared type.
//!
//! # A view does not know the spine
//!
//! A view used to carry `under:`, a link whose subtree it walked. That made the
//! view the one reader that knew the workspace has a shape. Now prov walks the
//! spine once, for the census ([`crate::documents`]), and hands each document
//! its ancestors as data — `doc.ancestors`, read by `under('Daily')` — so
//! scope is a condition like any other and survives a move or a rename because
//! the ancestry is recomputed on every run.
//!
//! # A view does not file
//!
//! Where a *new* record goes is `prov-filing`'s, a declaration of its own.
//! Filing writes into the single-parent spine and needs guarantees before
//! anything runs; reading has no invariant, and keeping the two apart is what
//! lets a view be a query.

use prov_graph::meta::{Mapping, Value};

use crate::expr::{Expression, ExpressionError};

/// The config block views are declared in — a top-level axis, so every prov
/// tool reads the same views rather than each app namespacing its own.
pub const VIEWS_KEY: &str = "views";

/// The keys valid inside one `views.<name>` entry.
pub const VIEW_KEYS: &[&str] = &["label", "icon", "where", "key"];

/// The keys an earlier form of the format used, which a view no longer
/// reads. An entry carrying any is diagnosed with its replacement (see
/// [`crate::legacy`]) rather than read half-way.
pub const RETIRED_VIEW_KEYS: &[&str] = &["group", "by", "under", "nest"];

/// One view a workspace declares for itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewSpec {
    /// The key under `views` — also the token that names this view to a
    /// frontend, and the id it is addressed by.
    pub name: String,
    /// What a person calls it. Absent falls back to the name, humanized.
    pub label: Option<String>,
    /// A glyph hint for a frontend's lens picker. Uninterpreted here: what a
    /// `calendar` looks like is the frontend's business.
    pub icon: Option<String>,
    /// The `where:` condition a document must meet, or `None` for every
    /// document the workspace reaches.
    ///
    /// Named `filter` because `where` is a Rust keyword; the config spelling is
    /// `where`, which is what a reader of the format sees.
    pub filter: Option<Expression>,
    /// The `key:` expression — the group or groups each document goes under.
    pub key: Expression,
}

impl ViewSpec {
    /// A view grouping every document by `key`, with no condition.
    pub fn new(name: impl Into<String>, key: Expression) -> Self {
        ViewSpec {
            name: name.into(),
            label: None,
            icon: None,
            filter: None,
            key,
        }
    }

    /// Read one `views.<name>` entry.
    ///
    /// `None` when the entry is not a mapping, has no `key:`, carries an
    /// expression that does not parse, or is written in the retired form. A
    /// view with a broken `where:` must not become a view of *everything*, and
    /// one with a broken `key:` has nothing to group by, so an entry that
    /// cannot be read whole is not read at all — and [`crate::diagnose_view`]
    /// is the half that says why.
    pub fn parse(name: &str, value: &Value) -> Option<Self> {
        let map = value.as_mapping()?;
        if is_retired(map) {
            return None;
        }
        let key = expression(map.get("key")?).ok()?;
        let filter = match map.get("where") {
            Some(value) => Some(expression(value).ok()?),
            None => None,
        };
        Some(ViewSpec {
            name: name.to_string(),
            label: non_empty(map.get("label")),
            icon: non_empty(map.get("icon")),
            filter,
            key,
        })
    }

    /// The mapping this view writes back as. Absent options are omitted rather
    /// than written empty, so a view declared from an app reads as the small
    /// thing it is.
    pub fn to_mapping(&self) -> Mapping {
        let mut map = Mapping::new();
        if let Some(label) = &self.label {
            map.insert("label".into(), Value::String(label.clone()));
        }
        if let Some(icon) = &self.icon {
            map.insert("icon".into(), Value::String(icon.clone()));
        }
        if let Some(filter) = &self.filter {
            map.insert("where".into(), Value::String(filter.source().to_string()));
        }
        map.insert("key".into(), Value::String(self.key.source().to_string()));
        map
    }

    /// What a person calls this view: its label, else its name humanized
    /// (`daily_entries` → `Daily entries`).
    pub fn display_label(&self) -> String {
        match &self.label {
            Some(label) => label.clone(),
            None => humanize(&self.name),
        }
    }
}

/// An expression from a config value, which must be text.
pub(crate) fn expression(value: &Value) -> Result<Expression, ExpressionError> {
    match value {
        Value::String(source) => Expression::parse(source),
        // A bare `key: 5` or `where: true` is YAML being helpful; the
        // expression it spells is the same text.
        Value::Int(i) => Expression::parse(&i.to_string()),
        Value::Bool(b) => Expression::parse(&b.to_string()),
        _ => Err(ExpressionError::Syntax(
            "an expression is written as text".to_string(),
        )),
    }
}

/// Whether an entry is written in the retired form: any of its keys, or a
/// `where:` that is a mapping of predicates rather than an expression.
pub(crate) fn is_retired(map: &Mapping) -> bool {
    RETIRED_VIEW_KEYS.iter().any(|k| map.contains_key(*k))
        || matches!(map.get("where"), Some(Value::Mapping(_)))
}

/// A trimmed non-empty string from a config value, or `None`.
fn non_empty(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// `daily_entries` → `Daily entries`: a key is written for a file, a label for
/// a person.
pub fn humanize(key: &str) -> String {
    let mut words = key.split(['_', '-']).filter(|w| !w.is_empty());
    let Some(first) = words.next() else {
        return key.to_string();
    };
    let mut out = first.to_string();
    if let Some(c) = out.get_mut(0..1) {
        c.make_ascii_uppercase();
    }
    for word in words {
        out.push(' ');
        out.push_str(&word.to_lowercase());
    }
    out
}

/// Read every `views.<name>` entry out of a config surface's `views:` block,
/// in declaration order.
pub fn views_from(config: &Mapping) -> Vec<ViewSpec> {
    let Some(views) = config.get(VIEWS_KEY).and_then(Value::as_mapping) else {
        return Vec::new();
    };
    views
        .iter()
        .filter_map(|(name, value)| ViewSpec::parse(name, value))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping(pairs: &[(&str, Value)]) -> Value {
        let mut map = Mapping::new();
        for (k, v) in pairs {
            map.insert((*k).into(), v.clone());
        }
        Value::Mapping(map)
    }

    fn text(s: &str) -> Value {
        Value::String(s.to_string())
    }

    #[test]
    fn a_view_is_a_key_and_an_optional_condition() {
        let spec = ViewSpec::parse(
            "open",
            &mapping(&[
                ("label", text("Open tasks")),
                ("where", text("present(status) && status != 'done'")),
                ("key", text("status")),
            ]),
        )
        .expect("a view");
        assert_eq!(spec.display_label(), "Open tasks");
        assert_eq!(spec.key.source(), "status");
        assert_eq!(
            spec.filter.as_ref().map(Expression::source),
            Some("present(status) && status != 'done'")
        );
    }

    #[test]
    fn an_entry_that_cannot_be_read_whole_is_not_read() {
        // No key: nothing to group by.
        assert!(ViewSpec::parse("v", &mapping(&[("where", text("true"))])).is_none());
        // A broken condition must not become a view of everything.
        assert!(
            ViewSpec::parse(
                "v",
                &mapping(&[("key", text("status")), ("where", text("status =="))])
            )
            .is_none()
        );
        // The retired form is diagnosed with its replacement, never read half-way.
        assert!(ViewSpec::parse("v", &mapping(&[("group", text("status"))])).is_none());
        assert!(
            ViewSpec::parse(
                "v",
                &mapping(&[
                    ("key", text("status")),
                    ("where", mapping(&[("has", text("status"))]))
                ])
            )
            .is_none()
        );
    }

    #[test]
    fn a_view_round_trips_through_its_mapping() {
        let value = mapping(&[
            ("label", text("Daily")),
            ("icon", text("calendar")),
            ("where", text("!present(draft)")),
            ("key", text("month(first(date_of_document, created))")),
        ]);
        let spec = ViewSpec::parse("daily", &value).expect("a view");
        let back = ViewSpec::parse("daily", &Value::Mapping(spec.to_mapping()));
        assert_eq!(back, Some(spec));
    }

    #[test]
    fn humanize_turns_a_key_into_a_label() {
        assert_eq!(humanize("daily_entries"), "Daily entries");
        assert_eq!(humanize("open-tasks"), "Open tasks");
    }
}
