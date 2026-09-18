//! Field paths — how a `fields` declaration names what it governs, and how a
//! finding or a repair names the one value it means.
//!
//! A declaration names a top-level key (`tags`), a key inside a mapping
//! (`generated.how`), or a key inside **every item of a list**
//! (`sources[].resource`, `confirmed[].by`). The three steps compose, and a
//! [`FieldPath`] is the parsed form: one [`Step`] per key or `[]`. Walking a
//! path over a document's metadata ([`values_at`]) yields every value it
//! reaches, each with the concrete [`Address`] it was found at — the same path
//! with every `[]` filled in (`sources[2].resource`) — which is what a
//! finding names and what an editor edits. The path grammar is its own
//! address grammar: a numeric step is a list index, so an address parses as
//! a path with no `[]` left in it, and displays as one.
//!
//! A dot is always a separator, as it is for `prov get`. A bracket group is
//! `[]` (every item) or `[n]` (one item); anything else in brackets is part
//! of the key, so a field whose name happens to carry brackets is still
//! addressable.

use std::fmt;

/// One step of a [`FieldPath`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Step {
    /// A mapping key.
    Key(String),
    /// Every item of a list — the `[]` a declaration writes.
    Each,
    /// One item of a list — the `[n]` a concrete address writes.
    At(usize),
}

/// A parsed field path: `tags`, `generated.how`, `sources[].resource`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FieldPath {
    steps: Vec<Step>,
}

impl FieldPath {
    /// Parse a path as a declaration or a finding writes it. Never fails: a
    /// malformed bracket group is read as part of the key it follows.
    pub fn parse(path: &str) -> Self {
        let mut steps = Vec::new();
        for segment in path.split('.') {
            let (key, brackets) = split_brackets(segment);
            steps.push(Step::Key(key.to_string()));
            for group in brackets {
                steps.push(group);
            }
        }
        Self { steps }
    }

    /// The steps, in order.
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// Whether every list step names one item — the form an [`Address`] takes.
    pub fn is_concrete(&self) -> bool {
        !self.steps.iter().any(|s| matches!(s, Step::Each))
    }

    /// Whether the path reaches into a list at all. A path that does not is
    /// the shape a starting value can be written at.
    pub fn enters_list(&self) -> bool {
        self.steps
            .iter()
            .any(|s| matches!(s, Step::Each | Step::At(_)))
    }

    /// The top-level key the path starts at.
    pub fn head(&self) -> &str {
        match &self.steps[0] {
            Step::Key(k) => k,
            // `parse` always begins with a key.
            _ => unreachable!("a field path starts with a key"),
        }
    }
}

impl fmt::Display for FieldPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_steps(f, &self.steps)
    }
}

/// A concrete address into a document's metadata — a [`FieldPath`] with every
/// `[]` filled in. `sources[2].resource`; `contents[3]`; `title`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Address {
    steps: Vec<Step>,
}

impl Address {
    /// Parse a concrete address. `None` when the text still has a `[]` in it.
    pub fn parse(text: &str) -> Option<Self> {
        let path = FieldPath::parse(text);
        path.is_concrete().then_some(Self { steps: path.steps })
    }

    /// The address of a top-level key.
    pub fn key(name: &str) -> Self {
        Self {
            steps: vec![Step::Key(name.to_string())],
        }
    }

    /// This address with a list index appended.
    pub fn item(mut self, index: usize) -> Self {
        self.steps.push(Step::At(index));
        self
    }

    /// The steps, in order.
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// The top-level key the address starts at.
    pub fn head(&self) -> &str {
        match &self.steps[0] {
            Step::Key(k) => k,
            _ => unreachable!("an address starts with a key"),
        }
    }

    /// The address as an editor path — one [`fig::Segment`] per step.
    pub fn segments(&self) -> Vec<fig::Segment<'_>> {
        self.steps
            .iter()
            .map(|s| match s {
                Step::Key(k) => fig::Segment::Key(k),
                Step::At(i) => fig::Segment::Index(*i),
                Step::Each => unreachable!("an address has no `[]` step"),
            })
            .collect()
    }

    /// The address of the list this one is an item of, and the item's
    /// position — `Some` only when the last step is an index. What a removal
    /// needs: an item is taken out of its list, not deleted at its own path.
    pub fn as_item(&self) -> Option<(Address, usize)> {
        match self.steps.last() {
            Some(Step::At(i)) => Some((
                Address {
                    steps: self.steps[..self.steps.len() - 1].to_vec(),
                },
                *i,
            )),
            _ => None,
        }
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_steps(f, &self.steps)
    }
}

fn write_steps(f: &mut fmt::Formatter<'_>, steps: &[Step]) -> fmt::Result {
    for (i, step) in steps.iter().enumerate() {
        match step {
            Step::Key(k) => {
                if i > 0 {
                    f.write_str(".")?;
                }
                f.write_str(k)?;
            }
            Step::Each => f.write_str("[]")?,
            Step::At(n) => write!(f, "[{n}]")?,
        }
    }
    Ok(())
}

/// Split a dotted segment into its key and the bracket groups that follow it.
/// The groups must run to the end of the segment and each must be `[]` or
/// `[digits]`; otherwise the whole segment is the key.
fn split_brackets(segment: &str) -> (&str, Vec<Step>) {
    let Some(open) = segment.find('[') else {
        return (segment, Vec::new());
    };
    let (key, rest) = segment.split_at(open);
    let mut groups = Vec::new();
    let mut rest = rest;
    while !rest.is_empty() {
        let Some(inner) = rest.strip_prefix('[') else {
            return (segment, Vec::new());
        };
        let Some(close) = inner.find(']') else {
            return (segment, Vec::new());
        };
        let (body, after) = inner.split_at(close);
        let step = if body.is_empty() {
            Step::Each
        } else if let Ok(n) = body.parse::<usize>() {
            Step::At(n)
        } else {
            return (segment, Vec::new());
        };
        groups.push(step);
        rest = &after[1..];
    }
    if key.is_empty() {
        return (segment, Vec::new());
    }
    (key, groups)
}

/// A metadata tree a [`FieldPath`] can be walked over. Both value trees prov
/// reads — its own [`Value`](crate::meta::Value) and `fig`'s — are one.
pub trait Navigate: Sized {
    /// The value under `key`, if this is a mapping holding it.
    fn child(&self, key: &str) -> Option<&Self>;
    /// The items, if this is a list.
    fn items(&self) -> Option<&[Self]>;
    /// The text, if this is a string.
    fn text(&self) -> Option<&str>;
}

impl Navigate for crate::meta::Value {
    fn child(&self, key: &str) -> Option<&Self> {
        self.get(key)
    }
    fn items(&self) -> Option<&[Self]> {
        self.as_sequence()
    }
    fn text(&self) -> Option<&str> {
        self.as_str()
    }
}

impl Navigate for fig::Value {
    fn child(&self, key: &str) -> Option<&Self> {
        self.get(key)
    }
    fn items(&self) -> Option<&[Self]> {
        self.as_seq()
    }
    fn text(&self) -> Option<&str> {
        self.as_str()
    }
}

/// Every value `path` reaches in `root`, each with the concrete address it
/// was found at. A `[]` step fans out over the list's items; a key step on
/// something that is not a mapping, or an index past the end, reaches
/// nothing. Document order.
pub fn values_at<'v, V: Navigate>(root: &'v V, path: &FieldPath) -> Vec<(Address, &'v V)> {
    let mut found = vec![(Address { steps: Vec::new() }, root)];
    for step in path.steps() {
        let mut next = Vec::new();
        for (address, value) in found {
            match step {
                Step::Key(key) => {
                    if let Some(child) = value.child(key) {
                        let mut address = address;
                        address.steps.push(step.clone());
                        next.push((address, child));
                    }
                }
                Step::Each => {
                    if let Some(items) = value.items() {
                        for (i, item) in items.iter().enumerate() {
                            let mut address = address.clone();
                            address.steps.push(Step::At(i));
                            next.push((address, item));
                        }
                    }
                }
                Step::At(i) => {
                    if let Some(item) = value.items().and_then(|items| items.get(*i)) {
                        let mut address = address;
                        address.steps.push(step.clone());
                        next.push((address, item));
                    }
                }
            }
        }
        found = next;
    }
    found
}

/// Every string `path` governs in `root`, with its address: a value that is a
/// string is one, addressed where the path landed; a value that is a list is
/// each of its string items, addressed by position, with the items that are
/// not strings holding their place in the count. This is how a relation
/// field has always been read (`tags: [a, b]` is two values), applied at the
/// end of whatever path reached it.
pub fn strings_at<V: Navigate>(root: &V, path: &FieldPath) -> Vec<(Address, String)> {
    let mut out = Vec::new();
    for (address, value) in values_at(root, path) {
        if let Some(s) = value.text() {
            out.push((address, s.to_string()));
        } else if let Some(items) = value.items() {
            for (i, item) in items.iter().enumerate() {
                if let Some(s) = item.text() {
                    out.push((address.clone().item(i), s.to_string()));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::{Mapping, Value};

    fn doc() -> Value {
        let mut a = Mapping::new();
        a.insert("resource".into(), Value::String("a.md".into()));
        a.insert("title".into(), Value::String("A".into()));
        let mut b = Mapping::new();
        b.insert("resource".into(), Value::String("b.md".into()));
        let mut generated = Mapping::new();
        generated.insert("how".into(), Value::String("drafted".into()));
        let mut root = Mapping::new();
        root.insert("title".into(), Value::String("x".into()));
        root.insert(
            "tags".into(),
            Value::Sequence(vec![
                Value::String("t1".into()),
                Value::Int(3),
                Value::String("t2".into()),
            ]),
        );
        root.insert("generated".into(), Value::Mapping(generated));
        root.insert(
            "sources".into(),
            Value::Sequence(vec![Value::Mapping(a), Value::Mapping(b), Value::Null]),
        );
        Value::Mapping(root)
    }

    #[test]
    fn a_path_round_trips_through_display() {
        for text in [
            "tags",
            "generated.how",
            "sources[].resource",
            "sources[2].resource[1]",
        ] {
            assert_eq!(FieldPath::parse(text).to_string(), text);
        }
        assert!(!FieldPath::parse("sources[].resource").is_concrete());
        assert!(FieldPath::parse("sources[2].resource").is_concrete());
        assert!(!FieldPath::parse("generated.how").enters_list());
        assert!(FieldPath::parse("sources[].resource").enters_list());
    }

    #[test]
    fn a_malformed_bracket_group_is_part_of_the_key() {
        let path = FieldPath::parse("odd[x].y");
        assert_eq!(
            path.steps(),
            &[Step::Key("odd[x]".into()), Step::Key("y".into())]
        );
        assert_eq!(
            FieldPath::parse("open[").steps(),
            &[Step::Key("open[".into())]
        );
        assert_eq!(FieldPath::parse("[]").steps(), &[Step::Key("[]".into())]);
    }

    #[test]
    fn a_key_path_reaches_one_value_and_a_list_path_reaches_each_item() {
        let doc = doc();
        let hits = values_at(&doc, &FieldPath::parse("generated.how"));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.to_string(), "generated.how");
        assert_eq!(hits[0].1.as_str(), Some("drafted"));

        let hits = strings_at(&doc, &FieldPath::parse("sources[].resource"));
        assert_eq!(
            hits.iter()
                .map(|(a, s)| (a.to_string(), s.clone()))
                .collect::<Vec<_>>(),
            vec![
                ("sources[0].resource".to_string(), "a.md".to_string()),
                ("sources[1].resource".to_string(), "b.md".to_string()),
            ]
        );
        // The null third item holds its place in the count and yields nothing.
        let hits = strings_at(&doc, &FieldPath::parse("sources[2].resource"));
        assert!(hits.is_empty());
    }

    #[test]
    fn a_list_leaf_yields_each_string_item_by_position() {
        let doc = doc();
        let hits = strings_at(&doc, &FieldPath::parse("tags"));
        assert_eq!(
            hits.iter()
                .map(|(a, s)| (a.to_string(), s.clone()))
                .collect::<Vec<_>>(),
            vec![
                ("tags[0]".to_string(), "t1".to_string()),
                ("tags[2]".to_string(), "t2".to_string()),
            ]
        );
        let hits = strings_at(&doc, &FieldPath::parse("title"));
        assert_eq!(hits[0].0.to_string(), "title");
    }

    #[test]
    fn a_path_through_the_wrong_shape_reaches_nothing() {
        let doc = doc();
        assert!(values_at(&doc, &FieldPath::parse("title.how")).is_empty());
        assert!(values_at(&doc, &FieldPath::parse("title[]")).is_empty());
        assert!(values_at(&doc, &FieldPath::parse("sources[9].resource")).is_empty());
        assert!(values_at(&doc, &FieldPath::parse("missing")).is_empty());
    }

    #[test]
    fn an_address_knows_its_list_and_its_editor_segments() {
        let address = Address::parse("sources[2].resource").unwrap();
        assert_eq!(address.head(), "sources");
        assert!(address.as_item().is_none());
        let (list, i) = Address::parse("contents[3]").unwrap().as_item().unwrap();
        assert_eq!(list.to_string(), "contents");
        assert_eq!(i, 3);
        assert!(Address::parse("sources[].resource").is_none());
        let segments = address.segments();
        assert!(matches!(segments[0], fig::Segment::Key("sources")));
        assert!(matches!(segments[1], fig::Segment::Index(2)));
        assert!(matches!(segments[2], fig::Segment::Key("resource")));
    }
}
