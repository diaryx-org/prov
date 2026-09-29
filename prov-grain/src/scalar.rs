//! The text a value *says* — what a group key, a grain and a filing route all
//! read.

use prov_graph::meta::Value;

/// The trimmed, non-empty text of a scalar, or of every scalar in a sequence.
///
/// A view groups on what a value *says*, so the numeric and boolean cases are
/// rendered rather than skipped — a `rating: 5` groups under `5`. A mapping has
/// no single text and is not groupable; a nested sequence is not flattened,
/// because a list of lists is a shape no frontmatter field means to declare.
pub fn scalar_texts(value: &Value) -> Vec<String> {
    match value {
        Value::Sequence(items) => items.iter().filter_map(scalar_text).collect(),
        other => scalar_text(other).into_iter().collect(),
    }
}

/// One scalar's trimmed text, or `None` for a null, an empty string, or a
/// composite.
pub fn scalar_text(value: &Value) -> Option<String> {
    let text = match value {
        Value::String(s) => s.trim().to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null | Value::Sequence(_) | Value::Mapping(_) => return None,
    };
    (!text.is_empty()).then_some(text)
}
