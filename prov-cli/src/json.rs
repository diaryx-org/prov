//! JSON output for `check --json`, `ignore --json`, `views --json` and
//! `docs --json` — a value tree and a printer, hand-rolled.
//!
//! No serialization crate, for the reason the `similar` dependency note in
//! `Cargo.toml` gives and `now_rfc3339` follows: this is a *presentation*
//! concern of a handful of CLI flags. A derive on [`prov::Finding`] would put serde on
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

use prov::meta::Value;
use prov::views::{Grain, Hit, Row, RowSet, Selection, Site, ViewSpec};
use prov::{Finding, LinkSite};

/// A JSON value. Objects keep insertion order so the output is diffable.
///
/// [`Obj`](J::Obj) and [`Map`](J::Map) render identically and differ only in
/// where their keys come from: an object prov decided the shape of has
/// `&'static str` keys, so a typo in one is a compile error, while a document's
/// metadata block has whatever keys its author wrote.
pub enum J {
    Null,
    Str(String),
    Bool(bool),
    Int(i64),
    Float(f64),
    Arr(Vec<J>),
    Obj(Vec<(&'static str, J)>),
    Map(Vec<(String, J)>),
}

fn pad(out: &mut String, n: usize) {
    for _ in 0..n {
        out.push_str("  ");
    }
}

/// The shared body of [`J::Obj`] and [`J::Map`], so the two cannot drift into
/// printing the same value two ways.
fn write_object<'a>(
    fields: impl ExactSizeIterator<Item = (&'a str, &'a J)>,
    out: &mut String,
    indent: usize,
) {
    let len = fields.len();
    if len == 0 {
        out.push_str("{}");
        return;
    }
    out.push_str("{\n");
    for (i, (key, value)) in fields.enumerate() {
        pad(out, indent + 1);
        J::Str(key.to_string()).write(out, indent + 1);
        out.push_str(": ");
        value.write(out, indent + 1);
        if i + 1 < len {
            out.push(',');
        }
        out.push('\n');
    }
    pad(out, indent);
    out.push('}');
}

impl J {
    fn write(&self, out: &mut String, indent: usize) {
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
            J::Null => out.push_str("null"),
            J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            J::Int(n) => {
                let _ = write!(out, "{n}");
            }
            // JSON has no spelling for an infinity or a NaN, and a metadata
            // block parsed from YAML can carry either. `null` is the one
            // reading that survives a round trip through every parser; the
            // alternatives (`Infinity`, a quoted string) are each valid to
            // exactly one consumer. Debug formatting is what round-trips a
            // finite float: it is the shortest form that reads back as the
            // same bits, and it keeps the exponent rather than expanding
            // `1e300` to three hundred digits.
            J::Float(f) if !f.is_finite() => out.push_str("null"),
            J::Float(f) => {
                let _ = write!(out, "{f:?}");
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
            J::Obj(fields) => {
                write_object(fields.iter().map(|(k, v)| (*k, v)), out, indent);
            }
            J::Map(fields) => {
                write_object(fields.iter().map(|(k, v)| (k.as_str(), v)), out, indent);
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

/// An optional string as a string or `null`, never an omitted key: a consumer
/// reading a fixed set of keys should not have to distinguish "absent" from
/// "declared nothing here", which are the same fact everywhere below.
fn opt(text: Option<String>) -> J {
    match text {
        Some(text) => J::Str(text),
        None => J::Null,
    }
}

/// One workspace's whole report, for `check --follow`.
///
/// A different shape from the flat array `--json` prints without the flag, and
/// deliberately so: eighteen workspaces' findings are not one list, because a
/// `subject` is a path in one workspace's terms and means nothing once it has
/// crossed a root. `root` is what those paths are relative to — an absolute
/// directory on this device, and the only key here that is not a fact about the
/// archive. `workspace` is the name the reference asked for and `declares` what
/// the workspace calls itself; they differ only for an anonymous peer followed
/// with `--unverified`, and both are empty for an anonymous origin.
pub fn workspace_report(workspace: &str, declares: &str, root: &Path, findings: &[Finding]) -> J {
    J::Obj(vec![
        ("workspace", s(workspace)),
        ("declares", s(declares)),
        ("root", p(root)),
        ("findings", J::Arr(findings.iter().map(finding).collect())),
    ])
}

/// A link site as two keys: `site`, the relation field's name or `body`, and
/// `index`, the item's position when the field is a list — an integer, or
/// `null` for a scalar field and for a body site. Two keys rather than the
/// `contents[2]` the human line prints, so a consumer reads a number rather
/// than parsing one out of a name.
///
/// A path-valued field's site is its concrete address whole —
/// `sources[2].resource` — with `index` null: the position is inside the
/// path, not beside it, because the path has as many as it has list steps
/// and one integer could not carry them.
fn site(site: &LinkSite, fields: &mut Vec<(&'static str, J)>) {
    let (name, index) = match site {
        LinkSite::Relation { field, index } => (field.clone(), *index),
        LinkSite::Field { path } => (path.clone(), None),
        LinkSite::Body(_) => ("body".to_string(), None),
    };
    fields.push(("site", J::Str(name)));
    fields.push((
        "index",
        match index {
            Some(i) => J::Int(i as i64),
            None => J::Null,
        },
    ));
}

/// One finding as a JSON object.
///
/// Every object carries the same four keys first — `kind` to branch on,
/// `severity` (see [`Finding::severity`]) to sort by, `subject` (see
/// [`Finding::subject`]) to group by, and `message`, the exact line the
/// human-readable output prints — followed by that variant's own fields. A
/// consumer that only understands the first four understands every finding,
/// including ones added after it was written.
pub fn finding(f: &Finding) -> J {
    let mut fields: Vec<(&'static str, J)> = vec![
        ("kind", s(f.kind())),
        ("severity", s(f.severity().as_str())),
        ("subject", p(f.subject())),
        ("message", J::Str(f.to_string())),
    ];
    match f {
        Finding::BrokenLink { doc, site, target } | Finding::MalformedId { doc, site, target } => {
            fields.push(("doc", p(doc)));
            self::site(site, &mut fields);
            fields.push(("target", s(target)));
        }
        Finding::CaseMismatch {
            doc,
            site,
            target,
            actual,
        } => {
            fields.push(("doc", p(doc)));
            self::site(site, &mut fields);
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
            self::site(site, &mut fields);
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
            self::site(site, &mut fields);
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
            self::site(site, &mut fields);
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
                prov::ConfigIssueKind::MalformedRoot { value } => {
                    fields.push(("issue", s("malformed_root")));
                    fields.push(("value", s(value)));
                }
                prov::ConfigIssueKind::ScopedReference { field } => {
                    fields.push(("issue", s("scoped_reference")));
                    fields.push(("field", s(field)));
                }
                prov::ConfigIssueKind::NestRefNotDeclared { field } => {
                    fields.push(("issue", s("nest_ref_not_declared")));
                    fields.push(("field", s(field)));
                }
            }
        }
        Finding::ConfigSpecAhead { doc, declared } => {
            fields.push(("doc", p(doc)));
            fields.push(("declared", J::Int(*declared)));
            fields.push(("understood", J::Int(prov::config::SPEC_VERSION)));
        }
        Finding::ShadowedWorkspaceNode { node, shadowed } => {
            fields.push(("node", p(node)));
            fields.push(("shadowed", p(shadowed)));
        }
        Finding::ConfigHomesDisagree { node, named } => {
            fields.push(("node", p(node)));
            fields.push(("named", p(named)));
        }
        Finding::NamedRootMissing { node, named } => {
            fields.push(("node", p(node)));
            fields.push(("named", s(named)));
        }
        Finding::NamedRootContained { node, named } => {
            fields.push(("node", p(node)));
            fields.push(("named", p(named)));
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
        Finding::MalformedDate {
            doc,
            field,
            value,
            why,
        } => {
            fields.push(("doc", p(doc)));
            fields.push(("field", s(field)));
            fields.push(("value", s(value)));
            fields.push(("why", s(why)));
        }
        Finding::LegacyBodyHash {
            root,
            count,
            example,
        } => {
            fields.push(("root", p(root)));
            fields.push(("count", J::Int(*count as i64)));
            fields.push(("example", p(example)));
        }
        Finding::LegacyDeletionsPointer {
            root,
            relation,
            log,
        } => {
            fields.push(("root", p(root)));
            fields.push(("relation", s(relation)));
            fields.push(("log", p(log)));
        }
        Finding::AboutStale { path, missing, .. } => {
            fields.push(("path", p(path)));
            fields.push(("missing", J::Bool(*missing)));
        }
        Finding::ConfirmationStale { doc, by, at } => {
            fields.push(("doc", p(doc)));
            fields.push(("by", s(by)));
            fields.push(("at", s(at)));
        }
        Finding::FieldScopeUnresolved {
            doc,
            field,
            under,
            why,
        } => {
            fields.push(("doc", p(doc)));
            fields.push(("field", s(field)));
            fields.push(("under", s(under)));
            fields.push(("why", s(why)));
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

/// One ignore rule as a JSON object.
///
/// The rendered `line` is included beside the parts it is made of: a consumer
/// writing an ignore file wants the line prov would write, and one deciding
/// what to do about an unreached document wants the path and the reason
/// without parsing gitignore's escaping back off.
pub fn ignore(rule: &prov::Ignore) -> J {
    J::Obj(vec![
        ("path", s(&rule.path)),
        ("directory", J::Bool(rule.whole_dir)),
        ("reason", s(crate::ignore::reason_word(rule.reason))),
        ("line", s(&rule.to_string())),
    ])
}

/// A metadata value as JSON, structure for structure.
///
/// The one lossy step is [`Value::Float`], and only for a value JSON cannot
/// spell (see the printer). Everything else survives, which is what makes this
/// worth carrying on a row: a consumer that was reading whole frontmatter
/// blocks through `prov meta` gets the same blocks without a second pass over
/// the files.
pub fn meta(value: &Value) -> J {
    match value {
        Value::Null => J::Null,
        Value::Bool(b) => J::Bool(*b),
        Value::Int(n) => J::Int(*n),
        Value::Float(f) => J::Float(*f),
        Value::String(text) => s(text),
        Value::Sequence(items) => J::Arr(items.iter().map(meta).collect()),
        Value::Mapping(map) => J::Map(map.iter().map(|(k, v)| (k.clone(), meta(v))).collect()),
    }
}

/// One declared view, for the `views --json` listing.
///
/// The same facts the text listing prints, each as its own key rather than
/// assembled into a sentence: `label` resolved through
/// [`ViewSpec::display_label`], the grouping chain as an array, and the grains
/// in their `by:`/`nest:` display spelling. `filtered` is a flag for the same
/// reason the listing line makes it one — a nested `where:` does not fit a
/// listing, and what a reader needs from a list is that this view does not show
/// everything it reaches.
pub fn view(spec: &ViewSpec) -> J {
    J::Obj(vec![
        ("name", s(&spec.name)),
        ("label", J::Str(spec.display_label())),
        (
            "group",
            J::Arr(spec.group.keys.iter().map(|k| s(k)).collect()),
        ),
        ("by", opt(spec.group.by.map(Grain::display))),
        ("under", opt(spec.under.clone())),
        ("filtered", J::Bool(spec.filter.is_some())),
        ("nest", opt(spec.nest.map(prov::views::Nest::display))),
    ])
}

/// An executed view: its groups, its ungrouped bucket, and both counts.
///
/// `documents` comes from the selection and `rows` from the placements, kept
/// apart for the reason the text output keeps them apart — a document under two
/// of a multi-valued field's groups is one document in two places, and a single
/// total would have the view claiming more entries than the workspace holds.
///
/// `ungrouped` is always present, empty or not: a view whose entries have all
/// stopped grouping looks exactly like an empty archive, and an omitted key
/// would hide the difference from a consumer as effectively as silence hides it
/// from a reader.
pub fn view_result(selection: &Selection, rows: &RowSet<'_>) -> J {
    let group = |g: &prov::views::Group<'_>| {
        J::Obj(vec![
            ("key", s(&g.key)),
            ("rows", J::Arr(g.rows.iter().map(|r| view_row(r)).collect())),
        ])
    };
    J::Obj(vec![
        ("view", s(&selection.view)),
        ("groups", J::Arr(rows.groups.iter().map(group).collect())),
        (
            "ungrouped",
            J::Arr(rows.ungrouped.iter().map(|r| view_row(r)).collect()),
        ),
        ("documents", J::Int(selection.len() as i64)),
        ("rows", J::Int(rows.placements() as i64)),
    ])
}

/// One row of an executed view.
///
/// `title` is `null` rather than absent for a document that declares none —
/// the text output drops the dash, but a parser reading a fixed set of keys
/// should not have to branch on which keys arrived.
fn view_row(row: &Row) -> J {
    J::Obj(vec![
        ("path", p(&row.path)),
        ("title", opt(row.title().map(str::to_owned))),
        ("meta", meta(&row.meta)),
    ])
}

/// One document of the `docs --json` census: a view row with the document's
/// id as a column.
///
/// The id is resolved by the caller, which has the index; this function only
/// decides where it goes on the wire. `null` for a document without one, for
/// the reason `title` is: a fixed set of keys, however the document is
/// stored.
///
/// `body` is three-valued on purpose. The outer `None` is "not asked for" —
/// the key is absent, so a consumer of the plain `docs --json` sees the row it
/// always saw. The inner `None` is "asked for, and this document has none",
/// written `null` like an absent id; `Some("")` is a body with nothing in it,
/// which is a different fact about the document and keeps its own spelling.
pub fn doc_row(row: &Row, id: Option<String>, body: Option<Option<String>>) -> J {
    let mut fields = vec![
        ("path", p(&row.path)),
        ("title", opt(row.title().map(str::to_owned))),
        ("id", opt(id)),
        ("meta", meta(&row.meta)),
    ];
    if let Some(body) = body {
        fields.push(("body", opt(body)));
    }
    J::Obj(fields)
}

/// One `search` hit. `site` is where the passage was cut from — `title`,
/// `field` or `body` — and `field` the key when it is a field, `null`
/// otherwise, so a consumer reads two fixed keys rather than a variant shape.
pub fn hit(hit: &Hit) -> J {
    let (site, field) = match &hit.site {
        Site::Title => ("title", None),
        Site::Field(key) => ("field", Some(key.clone())),
        Site::Body => ("body", None),
    };
    J::Obj(vec![
        ("path", p(&hit.path)),
        ("title", s(&hit.title)),
        ("site", s(site)),
        ("field", opt(field)),
        (
            "passage",
            J::Obj(vec![
                ("before", s(&hit.passage.before)),
                ("matched", s(&hit.passage.matched)),
                ("after", s(&hit.passage.after)),
            ]),
        ),
    ])
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
        assert_eq!(J::Map(vec![]).render(), "{}\n");
    }

    /// The two object variants exist only to keep static keys checkable; a
    /// reader of the output must not be able to tell which one produced it.
    #[test]
    fn a_dynamic_key_object_renders_as_a_static_one_does() {
        let statik = J::Obj(vec![("a", J::Int(1)), ("b", J::Int(2))]);
        let dynamic = J::Map(vec![("a".into(), J::Int(1)), ("b".into(), J::Int(2))]);
        assert_eq!(statik.render(), dynamic.render());
    }

    #[test]
    fn a_float_round_trips_and_a_non_finite_one_becomes_null() {
        assert_eq!(J::Float(1.0).render(), "1.0\n");
        assert_eq!(J::Float(1e300).render(), "1e300\n");
        assert_eq!(J::Float(f64::NAN).render(), "null\n");
        assert_eq!(J::Float(f64::INFINITY).render(), "null\n");
    }

    /// Metadata crosses whole: a nested mapping, a sequence, and every scalar
    /// kind a frontmatter block can hold.
    #[test]
    fn a_metadata_block_crosses_structure_for_structure() {
        let mut inner = prov::Mapping::new();
        inner.insert("draft".into(), Value::Bool(true));
        inner.insert("weight".into(), Value::Int(3));
        let mut outer = prov::Mapping::new();
        outer.insert("title".into(), Value::String("July 24".into()));
        outer.insert("flags".into(), Value::Mapping(inner));
        outer.insert(
            "people".into(),
            Value::Sequence(vec![Value::String("Ada".into()), Value::Null]),
        );

        assert_eq!(
            meta(&Value::Mapping(outer)).render(),
            "{\n  \"title\": \"July 24\",\n  \"flags\": {\n    \"draft\": true,\n    \
             \"weight\": 3\n  },\n  \"people\": [\n    \"Ada\",\n    null\n  ]\n}\n"
        );
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
