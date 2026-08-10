//! Embedded-metadata values — a dynamic, order-preserving value tree over `fig`.
//!
//! This is prov's *common currency*: link fields are configurable, so the
//! metadata is accessed dynamically rather than through a fixed struct. The
//! parse/serialize paths are serde-free — they walk `fig`'s native value tree —
//! mirroring the proven approach in `diaryx_core`'s `yaml` module.
//!
//! The functions here are format-parametric: the caller passes the
//! [`fig::Format`] the block is written in, resolved from the detected embed
//! archetype via [`fig::EmbedType::inner_format`] (see `document`). Which
//! formats are compiled in is governed by prov's forwarded `fig` feature
//! gates (`yaml`, `json`, `fig`).

use indexmap::IndexMap;

use crate::error::Result;

/// A dynamic metadata value. Integers and floats are kept distinct, and
/// mappings preserve key order (frontmatter is order-significant to humans).
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Value {
    /// Null (`~`, `null`), and the [`Default`].
    #[default]
    Null,
    /// Boolean.
    Bool(bool),
    /// Integer.
    Int(i64),
    /// Float.
    Float(f64),
    /// String.
    String(String),
    /// Sequence (`- item`).
    Sequence(Vec<Value>),
    /// Mapping (`key: value`), key order preserved.
    Mapping(Mapping),
}

/// An order-preserving metadata mapping — the shape of a frontmatter block.
pub type Mapping = IndexMap<String, Value>;

impl Value {
    /// The string, if this is a [`Value::String`].
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// The boolean, if this is a [`Value::Bool`].
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// The sequence, if this is a [`Value::Sequence`].
    pub fn as_sequence(&self) -> Option<&[Value]> {
        match self {
            Value::Sequence(v) => Some(v),
            _ => None,
        }
    }

    /// The mapping, if this is a [`Value::Mapping`].
    pub fn as_mapping(&self) -> Option<&Mapping> {
        match self {
            Value::Mapping(m) => Some(m),
            _ => None,
        }
    }

    /// `true` if this is [`Value::Null`].
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Look up a key, if this is a mapping.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_mapping().and_then(|m| m.get(key))
    }

    /// Interpret this value as a list of link strings: a bare string yields one
    /// element, a sequence yields its string-shaped elements, anything else
    /// yields nothing. This is how a relation field (single or multi) is read.
    pub fn link_strings(&self) -> Vec<String> {
        match self {
            Value::String(s) => vec![s.clone()],
            Value::Sequence(seq) => seq
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// Parse a metadata document in `format` into a [`Value`], serde-free.
///
/// An empty document is [`Value::Null`].
pub fn parse_value(s: &str, format: fig::Format) -> Result<Value> {
    let doc = fig::Document::parse(s.as_bytes(), format)?;
    Ok(Value::from(doc.to_value()?))
}

/// Parse a metadata mapping (the shape of frontmatter) in `format`. An empty
/// document is an empty mapping; a non-mapping top level is an error.
pub fn parse_mapping(s: &str, format: fig::Format) -> Result<Mapping> {
    match parse_value(s, format)? {
        Value::Mapping(m) => Ok(m),
        Value::Null => Ok(Mapping::new()),
        _ => Err(crate::error::Error::Structure(
            "frontmatter must be a mapping".into(),
        )),
    }
}

/// Serialize a metadata mapping back to a string in `format` — the same format
/// it was parsed from, so a ```` ```fig ```` block is never rewritten as YAML.
///
/// Forces block layout (one list item per line) rather than fig 2.0's default
/// flow style for short sequences, matching the diffs humans expect from
/// frontmatter.
pub fn serialize_mapping(map: &Mapping, format: fig::Format) -> Result<String> {
    let value = fig::Value::from(&Value::Mapping(map.clone()));
    Ok(value.serialize_with(format, fig::SerializeOptions::default().width(1))?)
}

/// Serialize any metadata value to a string in `format`. What `serialize_mapping`
/// is for whole frontmatter blocks, this is for a value plucked out of one
/// (the CLI's `get` on a compound field).
pub fn serialize_value(value: &Value, format: fig::Format) -> Result<String> {
    Ok(
        fig::Value::from(value)
            .serialize_with(format, fig::SerializeOptions::default().width(1))?,
    )
}

// ---------------------------------------------------------------------------
// Conversions to/from fig's native value tree (the serde-free bridge).
// ---------------------------------------------------------------------------

impl From<&Value> for fig::Value {
    fn from(value: &Value) -> Self {
        match value {
            Value::Null => fig::Value::Null,
            Value::Bool(b) => fig::Value::Bool(*b),
            Value::Int(i) => fig::Value::Int(*i),
            Value::Float(f) => fig::Value::Float(*f),
            Value::String(s) => fig::Value::Str(s.clone()),
            Value::Sequence(seq) => fig::Value::Seq(seq.iter().map(fig::Value::from).collect()),
            Value::Mapping(map) => fig::Value::Map(
                map.iter()
                    .map(|(k, v)| (fig::Value::Str(k.clone()), fig::Value::from(v)))
                    .collect(),
            ),
        }
    }
}

impl From<fig::Value> for Value {
    fn from(value: fig::Value) -> Self {
        match value {
            fig::Value::Null => Value::Null,
            fig::Value::Bool(b) => Value::Bool(b),
            fig::Value::Int(i) => Value::Int(i),
            fig::Value::Uint(u) => {
                if u <= i64::MAX as u64 {
                    Value::Int(u as i64)
                } else {
                    Value::Float(u as f64)
                }
            }
            fig::Value::Float(f) => Value::Float(f),
            fig::Value::Str(s) => Value::String(s),
            // Format-specific scalars (TOML datetimes, ZON literals) surface as
            // their verbatim text, matching fig's serde path.
            fig::Value::Extended { text, .. } => Value::String(text),
            fig::Value::Seq(items) => Value::Sequence(items.into_iter().map(Value::from).collect()),
            fig::Value::Map(entries) => {
                let mut map = IndexMap::with_capacity(entries.len());
                for (k, v) in entries {
                    map.insert(fig_key_to_string(k), Value::from(v));
                }
                Value::Mapping(map)
            }
        }
    }
}

/// Stringify a `fig` mapping key. Frontmatter keys are virtually always strings;
/// other scalars render to text, and non-scalar keys collapse to empty.
fn fig_key_to_string(key: fig::Value) -> String {
    match key {
        fig::Value::Str(s) => s,
        fig::Value::Bool(b) => b.to_string(),
        fig::Value::Int(i) => i.to_string(),
        fig::Value::Uint(u) => u.to_string(),
        fig::Value::Null => "null".to_string(),
        fig::Value::Extended { text, .. } => text,
        fig::Value::Float(_) | fig::Value::Seq(_) | fig::Value::Map(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "yaml")]
    #[test]
    fn parses_frontmatter_mapping() {
        let m = parse_mapping(
            "title: Hello\ncount: 42\ntags:\n- a\n- b\n",
            fig::Format::Yaml,
        )
        .unwrap();
        assert_eq!(m.get("title").and_then(Value::as_str), Some("Hello"));
        assert_eq!(m.get("count"), Some(&Value::Int(42)));
        assert_eq!(
            m.get("tags").map(Value::link_strings),
            Some(vec!["a".to_string(), "b".to_string()])
        );
    }

    #[cfg(feature = "fig-lang")]
    #[test]
    fn parses_fig_dialect_mapping() {
        let m = parse_mapping("title = Hello\ntags = [a, b]\n", fig::Format::Fig).unwrap();
        assert_eq!(m.get("title").and_then(Value::as_str), Some("Hello"));
        assert_eq!(
            m.get("tags").map(Value::link_strings),
            Some(vec!["a".to_string(), "b".to_string()])
        );
    }

    #[test]
    fn link_strings_handles_scalar_and_sequence() {
        assert_eq!(Value::String("x".into()).link_strings(), vec!["x"]);
        let seq = Value::Sequence(vec![
            Value::String("a".into()),
            Value::Int(3),
            Value::String("b".into()),
        ]);
        assert_eq!(seq.link_strings(), vec!["a".to_string(), "b".to_string()]);
        assert!(Value::Null.link_strings().is_empty());
    }

    #[cfg(all(feature = "yaml", feature = "fig-lang"))]
    #[test]
    fn round_trips_through_fig() {
        for format in [fig::Format::Yaml, fig::Format::Fig] {
            let m = parse_mapping(
                "title: Root\ncontents:\n- a.md\n- b.md\n",
                fig::Format::Yaml,
            )
            .unwrap();
            let out = serialize_mapping(&m, format).unwrap();
            let reparsed = parse_mapping(&out, format).unwrap();
            assert_eq!(m, reparsed, "round-trip through {format:?}");
        }
    }
}
