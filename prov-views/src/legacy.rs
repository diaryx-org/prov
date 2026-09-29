//! The retired view format, read only to say what replaces it.
//!
//! Before views were queries, a view was a set of verbs: `group:` (a field or a
//! first-non-empty chain), `by:` (a grain), `under:` (an anchor whose subtree
//! it walked), `where:` (a mapping of `has`/`equals`/`not`/`any-of`/`all-of`),
//! and `nest:` (where a new record files). None of those is read any more. An
//! entry still written that way is a config finding, and [`translate`] is what
//! lets the finding print its replacement rather than only its fault — and
//! what lets a frontend that wrote such entries on a user's behalf rewrite
//! them.
//!
//! The translation is exact where the old format was:
//!
//! - `group: [a, b]` + `by: month` → `key: month(first(a, b))`
//! - `under: '[[Daily]]'` → `doc.ancestors.exists(a, a.title == 'Daily')`, by
//!   `a.id` for an `id:` link and by `a.path` for a path
//! - `has: x` → `present(x)`; `equals: { x: v }` → `'v' in field('x')`, since
//!   the old comparison was by text and matched any item of a list
//! - `nest:` → a `filing:` entry (`prov-filing`) of the same name, with the
//!   anchor and the chain it filed by

use prov_graph::link::Link;
use prov_graph::meta::{Mapping, Value};
use prov_graph::title;

use crate::spec::is_retired;
use prov_grain::{Grain, scalar_texts};

/// A retired view, rewritten.
#[derive(Debug, Clone, PartialEq)]
pub struct Translation {
    /// The `views.<name>` entry.
    pub view: Mapping,
    /// The `filing.<name>` entry, when the old view nested.
    pub filing: Option<Mapping>,
}

/// Rewrite a `views.<name>` entry written in the retired form, or `None` when
/// it is not one or has no `group:` to make a key of.
pub fn translate(value: &Value) -> Option<Translation> {
    let map = value.as_mapping()?;
    if !is_retired(map) {
        return None;
    }
    let chain = group_keys(map.get("group"))?;
    let grain = map.get("by").and_then(Grain::parse);

    let mut view = Mapping::new();
    for key in ["label", "icon"] {
        if let Some(v) = map.get(key) {
            view.insert(key.into(), v.clone());
        }
    }
    let mut conditions = Vec::new();
    if let Some(under) = map.get("under").and_then(Value::as_str) {
        conditions.push(Cel::atom(ancestry(under)));
    }
    if let Some(condition) = map.get("where").and_then(Condition::parse) {
        conditions.push(condition.to_cel());
    }
    match conditions.len() {
        0 => {}
        1 => {
            view.insert("where".into(), Value::String(conditions.remove(0).text));
        }
        _ => {
            view.insert("where".into(), Value::String(Cel::all(conditions).text));
        }
    }
    view.insert("key".into(), Value::String(key_expression(&chain, grain)));

    let filing = map.get("nest").map(|nest| {
        let mut filing = Mapping::new();
        if let Some(under) = map.get("under") {
            filing.insert("under".into(), under.clone());
        }
        filing.insert(
            "field".into(),
            match chain.as_slice() {
                [one] => Value::String(one.clone()),
                many => Value::Sequence(many.iter().cloned().map(Value::String).collect()),
            },
        );
        filing.insert("nest".into(), nest.clone());
        filing
    });

    Some(Translation { view, filing })
}

impl Translation {
    /// The replacement as the YAML a person pastes into their config: the
    /// view under `views:`, and the filing entry under `filing:` when there is
    /// one.
    pub fn to_yaml(&self, name: &str) -> String {
        let mut out = String::from("views:\n");
        push_entry(&mut out, name, &self.view);
        if let Some(filing) = &self.filing {
            out.push_str("filing:\n");
            push_entry(&mut out, name, filing);
        }
        out
    }
}

fn push_entry(out: &mut String, name: &str, entry: &Mapping) {
    out.push_str(&format!("  {}:\n", yaml_key(name)));
    for (key, value) in entry {
        out.push_str(&format!("    {key}: {}\n", flow(value)));
    }
}

/// A value on one line: a double-quoted string, a flow sequence or mapping.
fn flow(value: &Value) -> String {
    match value {
        Value::String(s) => quoted(s),
        Value::Sequence(items) => format!(
            "[{}]",
            items.iter().map(flow).collect::<Vec<_>>().join(", ")
        ),
        Value::Mapping(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("{}: {}", yaml_key(k), flow(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
    }
}

fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn yaml_key(key: &str) -> String {
    let plain = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if plain { key.to_string() } else { quoted(key) }
}

/// The `key:` a chain and grain come to.
fn key_expression(chain: &[String], grain: Option<Grain>) -> String {
    let base = match chain {
        [one] => field_ref(one),
        many => format!(
            "first({})",
            many.iter()
                .map(|k| field_ref(k))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    match grain {
        None => base,
        Some(Grain::Year) => format!("year({base})"),
        Some(Grain::Month) => format!("month({base})"),
        Some(Grain::Day) => format!("day({base})"),
        Some(Grain::Initial(1)) => format!("initial({base})"),
        Some(Grain::Initial(n)) => format!("initial({base}, {n})"),
    }
}

/// The condition an `under:` anchor comes to: the anchor among the
/// document's ancestors, matched the way the link named it.
fn ancestry(under: &str) -> String {
    let link = Link::parse(under);
    if let Some(id) = link.id_target() {
        return format!("doc.ancestors.exists(a, a.id == {})", cel_string(&id.0));
    }
    let target = link.addressed_target();
    if title::is_alias_shaped(target) {
        return format!("doc.ancestors.exists(a, a.title == {})", cel_string(target));
    }
    let path = target.trim_start_matches("./").trim_start_matches('/');
    format!("doc.ancestors.exists(a, a.path == {})", cel_string(path))
}

/// A field by name when CEL can say it as one, else through `field()`.
fn field_ref(key: &str) -> String {
    if is_identifier(key) {
        key.to_string()
    } else {
        format!("field({})", cel_string(key))
    }
}

const RESERVED: &[&str] = &[
    "false",
    "in",
    "null",
    "true",
    "as",
    "break",
    "const",
    "continue",
    "else",
    "for",
    "function",
    "if",
    "import",
    "let",
    "loop",
    "package",
    "namespace",
    "return",
    "var",
    "void",
    "while",
    "doc",
];

fn is_identifier(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !RESERVED.contains(&key)
}

fn cel_string(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn group_keys(value: Option<&Value>) -> Option<Vec<String>> {
    let keys: Vec<String> = match value? {
        Value::String(s) if !s.trim().is_empty() => vec![s.trim().to_string()],
        Value::Sequence(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        _ => return None,
    };
    (!keys.is_empty()).then_some(keys)
}

/// A CEL fragment and how tightly it binds, so a combination parenthesizes
/// only what it must.
struct Cel {
    text: String,
    level: Level,
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
enum Level {
    Or,
    And,
    Atom,
}

impl Cel {
    fn atom(text: String) -> Self {
        Cel {
            text,
            level: Level::Atom,
        }
    }

    fn at_least(self, level: Level) -> String {
        if self.level >= level {
            self.text
        } else {
            format!("({})", self.text)
        }
    }

    fn all(parts: Vec<Cel>) -> Self {
        if parts.is_empty() {
            return Cel::atom("true".into());
        }
        Cel {
            text: parts
                .into_iter()
                .map(|p| p.at_least(Level::And))
                .collect::<Vec<_>>()
                .join(" && "),
            level: Level::And,
        }
    }

    fn any(parts: Vec<Cel>) -> Self {
        if parts.is_empty() {
            return Cel::atom("false".into());
        }
        Cel {
            text: parts
                .into_iter()
                .map(|p| p.at_least(Level::Or))
                .collect::<Vec<_>>()
                .join(" || "),
            level: Level::Or,
        }
    }
}

/// The retired `where:` vocabulary.
enum Condition {
    Has(String),
    Equals { field: String, value: String },
    Not(Box<Condition>),
    AllOf(Vec<Condition>),
    AnyOf(Vec<Condition>),
}

impl Condition {
    fn parse(value: &Value) -> Option<Self> {
        let map = value.as_mapping()?;
        let mut conditions = Vec::new();
        for (key, value) in map {
            match key.as_str() {
                "has" => conditions.extend(fields_of(value).into_iter().map(Condition::Has)),
                "equals" => {
                    if let Some(pairs) = value.as_mapping() {
                        for (field, v) in pairs {
                            // The old reader took a list's first element
                            // alone; the translation says what it did.
                            if let Some(text) = scalar_texts(v).into_iter().next() {
                                conditions.push(Condition::Equals {
                                    field: field.clone(),
                                    value: text,
                                });
                            }
                        }
                    }
                }
                "not" => {
                    conditions.extend(Condition::parse(value).map(|c| Condition::Not(c.into())))
                }
                "any-of" | "all-of" => {
                    let items: Vec<Condition> = value
                        .as_sequence()
                        .map(|items| items.iter().filter_map(Condition::parse).collect())
                        .unwrap_or_default();
                    conditions.push(if key == "any-of" {
                        Condition::AnyOf(items)
                    } else {
                        Condition::AllOf(items)
                    });
                }
                _ => {}
            }
        }
        match conditions.len() {
            0 => None,
            1 => conditions.pop(),
            _ => Some(Condition::AllOf(conditions)),
        }
    }

    fn to_cel(&self) -> Cel {
        match self {
            Condition::Has(field) => Cel::atom(format!("present({})", field_ref(field))),
            Condition::Equals { field, value } => Cel::atom(format!(
                "{} in field({})",
                cel_string(value),
                cel_string(field)
            )),
            Condition::Not(inner) => {
                Cel::atom(format!("!{}", inner.to_cel().at_least(Level::Atom)))
            }
            Condition::AllOf(all) => Cel::all(all.iter().map(Condition::to_cel).collect()),
            Condition::AnyOf(any) => Cel::any(any.iter().map(Condition::to_cel).collect()),
        }
    }
}

fn fields_of(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) if !s.trim().is_empty() => vec![s.trim().to_string()],
        Value::Sequence(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::*;
    use crate::expr::Expression;
    use crate::spec::ViewSpec;
    use prov_filing::FilingSpec;

    fn yaml(src: &str) -> Value {
        prov_graph::meta::parse_value(src, prov_graph::Format::Yaml).expect("yaml")
    }

    fn translated(src: &str) -> Translation {
        translate(&yaml(src)).expect("a translation")
    }

    fn get<'m>(map: &'m Mapping, key: &str) -> &'m str {
        map.get(key).and_then(Value::as_str).unwrap_or_default()
    }

    #[test]
    fn the_tasks_preset_view_translates_to_a_view_that_parses() {
        let t = translated(
            "label: Open tasks\ngroup: status\nunder: '[[Tasks]]'\nwhere:\n  has: status\n  not:\n    any-of:\n      - equals: { status: done }\n      - equals: { status: dropped }\n",
        );
        assert_eq!(get(&t.view, "key"), "status");
        assert_eq!(
            get(&t.view, "where"),
            "doc.ancestors.exists(a, a.title == 'Tasks') && present(status) && \
             !('done' in field('status') || 'dropped' in field('status'))"
        );
        assert!(t.filing.is_none());
        assert!(ViewSpec::parse("open-tasks", &Value::Mapping(t.view)).is_some());
    }

    #[test]
    fn a_chain_and_grain_become_a_key_and_nest_becomes_filing() {
        let t = translated(
            "group: [date_of_document, created]\nby: month\nunder: '[Daily](id:abc1234)'\nnest: year\n",
        );
        assert_eq!(
            get(&t.view, "key"),
            "month(first(date_of_document, created))"
        );
        assert_eq!(
            get(&t.view, "where"),
            "doc.ancestors.exists(a, a.id == 'abc1234')"
        );
        let filing = t.filing.clone().expect("a filing entry");
        let spec = FilingSpec::parse("daily", &Value::Mapping(filing)).expect("parses");
        assert_eq!(spec.under.as_deref(), Some("[Daily](id:abc1234)"));
        assert_eq!(spec.field, ["date_of_document", "created"]);
        assert!(t.to_yaml("daily").contains("filing:\n  daily:\n"));
    }

    #[test]
    fn a_path_anchor_and_a_nested_field_translate() {
        let t =
            translated("group: written.on\nunder: '[Calendar](/Calendar/index.md)'\nnest: ref\n");
        assert_eq!(get(&t.view, "key"), "field('written.on')");
        assert_eq!(
            get(&t.view, "where"),
            "doc.ancestors.exists(a, a.path == 'Calendar/index.md')"
        );
        assert!(Expression::parse(get(&t.view, "key")).is_ok());
    }

    #[test]
    fn a_current_view_is_not_translated() {
        assert!(translate(&yaml("key: status\n")).is_none());
        assert!(
            translate(&yaml("where: { has: x }\n")).is_none(),
            "no group"
        );
    }

    #[test]
    fn the_yaml_is_something_a_person_can_paste() {
        let t = translated("label: Open\ngroup: status\nwhere: { has: status }\n");
        assert_eq!(
            t.to_yaml("open-tasks"),
            "views:\n  open-tasks:\n    label: \"Open\"\n    where: \"present(status)\"\n    key: \"status\"\n"
        );
        let back = yaml(&t.to_yaml("open-tasks"));
        let views = back.get("views").and_then(Value::as_mapping).unwrap();
        assert!(ViewSpec::parse("open-tasks", views.get("open-tasks").unwrap()).is_some());
    }
}
