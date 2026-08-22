//! JSON output for `check --json` — a value tree and a printer, hand-rolled.
//!
//! No serialization crate, for the reason the `similar` dependency note in
//! `Cargo.toml` gives and `now_rfc3339` follows: this is a *presentation*
//! concern of one CLI flag. A derive on [`prov::Finding`] would put serde on
//! every downstream consumer of the library — diaryx included — to serve one
//! flag on one command, and it would also make the wire shape a consequence of
//! the enum's field names rather than a thing decided on purpose.
//!
//! Deciding it on purpose is the point. [`finding`] is an explicit match, so a
//! new [`prov::Finding`] variant fails to compile here until someone says what
//! it looks like on the wire, and a field renamed inside the library does not
//! silently rename itself in a consumer's parser.

use std::fmt::Write as _;
use std::path::Path;

use prov::Finding;

/// A JSON value, only as far as findings need: no floats (nothing here has
/// one), and objects keep insertion order so the output is diffable.
pub enum J {
    Str(String),
    Bool(bool),
    Int(i64),
    Arr(Vec<J>),
    Obj(Vec<(&'static str, J)>),
}

impl J {
    fn write(&self, out: &mut String, indent: usize) {
        let pad = |out: &mut String, n: usize| {
            for _ in 0..n {
                out.push_str("  ");
            }
        };
        match self {
            J::Str(s) => {
                out.push('"');
                for c in s.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        // Control characters have no literal form in JSON, and
                        // a finding can carry one: a parser error quotes the
                        // offending bytes, and a document's title is whatever
                        // the author typed.
                        c if (c as u32) < 0x20 => {
                            let _ = write!(out, "\\u{:04x}", c as u32);
                        }
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            J::Int(n) => {
                let _ = write!(out, "{n}");
            }
            J::Arr(items) if items.is_empty() => out.push_str("[]"),
            J::Arr(items) => {
                out.push_str("[\n");
                for (i, item) in items.iter().enumerate() {
                    pad(out, indent + 1);
                    item.write(out, indent + 1);
                    if i + 1 < items.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                pad(out, indent);
                out.push(']');
            }
            J::Obj(fields) if fields.is_empty() => out.push_str("{}"),
            J::Obj(fields) => {
                out.push_str("{\n");
                for (i, (key, value)) in fields.iter().enumerate() {
                    pad(out, indent + 1);
                    J::Str((*key).to_string()).write(out, indent + 1);
                    out.push_str(": ");
                    value.write(out, indent + 1);
                    if i + 1 < fields.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                pad(out, indent);
                out.push('}');
            }
        }
    }

    /// Render as indented JSON with a trailing newline.
    pub fn render(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out.push('\n');
        out
    }
}

/// A path as a JSON string. Workspace-relative and `/`-separated everywhere
/// prov produces one; lossy only for a filename that is not UTF-8, which
/// nothing prov writes ever is.
fn p(path: &Path) -> J {
    J::Str(path.to_string_lossy().into_owned())
}

fn s(text: &str) -> J {
    J::Str(text.to_string())
}

fn paths(items: &[std::path::PathBuf]) -> J {
    J::Arr(items.iter().map(|i| p(i)).collect())
}

/// One finding as a JSON object.
///
/// Every object carries the same three keys first — `kind` to branch on,
/// `subject` (see [`Finding::subject`]) to group by, and `message`, the exact
/// line the human-readable output prints — followed by that variant's own
/// fields. A consumer that only understands the first three understands every
/// finding, including ones added after it was written.
pub fn finding(f: &Finding) -> J {
    let mut fields: Vec<(&'static str, J)> = vec![
        ("kind", s(f.kind())),
        ("subject", p(f.subject())),
        ("message", J::Str(f.to_string())),
    ];
    match f {
        Finding::BrokenLink { doc, site, target } | Finding::MalformedId { doc, site, target } => {
            fields.push(("doc", p(doc)));
            fields.push(("site", J::Str(site.to_string())));
            fields.push(("target", s(target)));
        }
        Finding::CaseMismatch {
            doc,
            site,
            target,
            actual,
        } => {
            fields.push(("doc", p(doc)));
            fields.push(("site", J::Str(site.to_string())));
            fields.push(("target", s(target)));
            fields.push(("actual", s(actual)));
        }
        Finding::DuplicateContainment { doc, target } => {
            fields.push(("doc", p(doc)));
            fields.push(("target", s(target)));
        }
        Finding::MissingInverse {
            doc,
            child,
            inverse,
        } => {
            fields.push(("parent", p(doc)));
            fields.push(("child", p(child)));
            fields.push(("inverse", s(inverse)));
        }
        Finding::Unreadable { doc, error } => {
            fields.push(("doc", p(doc)));
            fields.push(("error", s(error)));
        }
        Finding::DanglingId {
            doc,
            site,
            id,
            tombstoned,
        } => {
            fields.push(("doc", p(doc)));
            fields.push(("site", J::Str(site.to_string())));
            fields.push(("id", J::Str(id.to_string())));
            fields.push(("tombstoned", J::Bool(*tombstoned)));
        }
        Finding::AmbiguousAlias {
            doc,
            site,
            name,
            candidates,
        } => {
            fields.push(("doc", p(doc)));
            fields.push(("site", J::Str(site.to_string())));
            fields.push(("name", s(name)));
            fields.push(("candidates", paths(candidates)));
        }
        Finding::StaleLabel {
            doc,
            site,
            target,
            expected,
            actual,
        } => {
            fields.push(("doc", p(doc)));
            fields.push(("site", J::Str(site.to_string())));
            fields.push(("target", s(target)));
            fields.push(("expected", s(expected)));
            fields.push(("actual", s(actual)));
        }
        Finding::IdMismatch {
            doc,
            frontmatter,
            registry,
        } => {
            fields.push(("doc", p(doc)));
            fields.push(("frontmatter", J::Str(frontmatter.to_string())));
            fields.push((
                "registry",
                match registry {
                    Some(id) => J::Str(id.to_string()),
                    None => J::Str(String::new()),
                },
            ));
        }
        Finding::UnregisteredId { doc, frontmatter } => {
            fields.push(("doc", p(doc)));
            fields.push(("frontmatter", J::Str(frontmatter.to_string())));
        }
        Finding::UnstampedId { doc, registry } => {
            fields.push(("doc", p(doc)));
            fields.push(("registry", J::Str(registry.to_string())));
        }
        Finding::Orphan { doc, root } => {
            fields.push(("doc", p(doc)));
            fields.push(("root", p(root)));
        }
        Finding::MissingContainment { doc, parent } => {
            fields.push(("doc", p(doc)));
            fields.push(("parent", p(parent)));
        }
        Finding::FixityMismatch {
            doc,
            recorded,
            actual,
        } => {
            fields.push(("doc", p(doc)));
            fields.push(("recorded", s(recorded)));
            fields.push(("actual", s(actual)));
        }
        Finding::ConfigIssue { doc, issue } => {
            fields.push(("doc", p(doc)));
            fields.push(("key", s(&issue.key)));
            match &issue.kind {
                prov::ConfigIssueKind::UnknownKey { suggestion } => {
                    fields.push(("issue", s("unknown_key")));
                    fields.push(("suggestion", s(suggestion)));
                }
                prov::ConfigIssueKind::InvalidValue { value, expected } => {
                    fields.push(("issue", s("invalid_value")));
                    fields.push(("value", s(value)));
                    fields.push(("expected", J::Arr(expected.iter().map(|e| s(e)).collect())));
                }
                prov::ConfigIssueKind::SpanningNotSingleParent { inverse } => {
                    fields.push(("issue", s("spanning_not_single_parent")));
                    fields.push(("inverse", s(inverse)));
                }
                prov::ConfigIssueKind::NestNotSingleValued { field } => {
                    fields.push(("issue", s("nest_not_single_valued")));
                    fields.push(("field", s(field)));
                }
                prov::ConfigIssueKind::MalformedWorkspaceId { value } => {
                    fields.push(("issue", s("malformed_workspace_id")));
                    fields.push(("value", s(value)));
                }
            }
        }
        Finding::ConfigSpecAhead { doc, declared } => {
            fields.push(("doc", p(doc)));
            fields.push(("declared", J::Int(*declared)));
            fields.push(("understood", J::Int(prov::config::SPEC_VERSION)));
        }
        Finding::MalformedStore { doc, pointer } => {
            fields.push(("doc", p(doc)));
            fields.push(("pointer", s(pointer)));
        }
        Finding::UnknownTerm {
            doc,
            field,
            value,
            retired,
        } => {
            fields.push(("doc", p(doc)));
            fields.push(("field", s(field)));
            fields.push(("value", s(value)));
            fields.push(("retired", J::Bool(*retired)));
        }
        Finding::TermNearMiss {
            doc,
            field,
            value,
            suggestion,
        } => {
            fields.push(("doc", p(doc)));
            fields.push(("field", s(field)));
            fields.push(("value", s(value)));
            fields.push(("suggestion", s(suggestion)));
        }
        Finding::RecycledBytesMissing {
            index,
            from,
            missing,
        } => {
            fields.push(("index", p(index)));
            fields.push(("from", p(from)));
            fields.push(("missing", paths(missing)));
        }
        Finding::AboutStale { path, missing, .. } => {
            fields.push(("path", p(path)));
            fields.push(("missing", J::Bool(*missing)));
        }
        Finding::ManifestConflict { doc } => {
            fields.push(("doc", p(doc)));
        }
        Finding::ManifestMalformed { doc, error } => {
            fields.push(("doc", p(doc)));
            fields.push(("error", s(error)));
        }
        Finding::ManifestDrift {
            node,
            manifest,
            missing,
            extra,
        } => {
            fields.push(("node", p(node)));
            fields.push(("manifest", p(manifest)));
            fields.push(("missing", paths(missing)));
            fields.push(("extra", paths(extra)));
        }
        Finding::ManifestMismatch {
            node,
            manifest,
            path,
            recorded,
            actual,
        } => {
            fields.push(("node", p(node)));
            fields.push(("manifest", p(manifest)));
            fields.push(("path", p(path)));
            fields.push(("recorded", s(recorded)));
            fields.push(("actual", s(actual)));
        }
    }
    J::Obj(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_escape_quotes_backslashes_and_control_characters() {
        let rendered = J::Str("a\"b\\c\nd\te\u{1}f".into()).render();
        assert_eq!(rendered, "\"a\\\"b\\\\c\\nd\\te\\u0001f\"\n");
    }

    #[test]
    fn empty_containers_stay_on_one_line() {
        // `[]` is the whole point of the flag's "clean and silent are
        // distinguishable" promise, so it must not render as a blank block.
        assert_eq!(J::Arr(vec![]).render(), "[]\n");
        assert_eq!(J::Obj(vec![]).render(), "{}\n");
    }

    #[test]
    fn nested_values_indent_by_depth() {
        let v = J::Arr(vec![J::Obj(vec![
            ("kind", J::Str("orphan".into())),
            ("paths", J::Arr(vec![J::Str("a.md".into())])),
        ])]);
        assert_eq!(
            v.render(),
            "[\n  {\n    \"kind\": \"orphan\",\n    \"paths\": [\n      \"a.md\"\n    ]\n  }\n]\n"
        );
    }
}
