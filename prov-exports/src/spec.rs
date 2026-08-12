//! The export format: what a workspace declares under `exports.<name>`.
//!
//! # Why an export is not a view
//!
//! A view answers *how documents are arranged*; an export answers *which
//! documents may leave the workspace*. The temptation is to collapse the two —
//! both select documents by the value of a declared field. Three things stop
//! it, and they are the reasons this is a crate beside `prov-views` rather
//! than a `where:` idiom inside it.
//!
//! A wrong view is a wrong grouping you fix in the picker; a wrong export is a
//! file in hands it was never meant for. A view with no `under:` covers the
//! whole workspace, while a document that declares nothing is in **no**
//! export — open-by-default against closed-by-default, and one primitive
//! cannot hold both. And the gate value is written *in the document*, so it
//! travels with the file and still means what it meant, where view membership
//! is a property of the workspace and cannot be.
//!
//! So a gate is not a *kind* of filter. It is a *position*: the domain every
//! view runs over once the corpus leaves the workspace. Inside the workspace
//! the gate field is an ordinary field, and a view can group by it like any
//! other.
//!
//! ```yaml
//! exports:
//!   letters:
//!     label: Letters home
//!     gate: { field: audience, value: family }
//!     view: daily
//! ```
//!
//! # One field, one value
//!
//! A [`Gate`] is exactly a field name and a value: a document is in the export
//! iff its own metadata declares that value under that field. Not a list of
//! values, not a `where:` condition — deliberately. The property that makes an
//! export auditable is that *"does this document leave?"* is answerable by
//! reading one field on that one document; a condition makes it a proof about
//! the pipeline, and an any-of list makes it depend on a set in the config
//! rather than a value you can grep for. This is the same discipline that
//! keeps `where:` a closed predicate set and `sort:` unshipped: the shapes a
//! format admits are its point of no return.
//!
//! Matching is **exact after trimming**. `audience: Family` does not pass
//! `value: family` — admitting it would let the written config say less than
//! the gate does, which is the fail-open direction. Casing drift between
//! documents is precisely what a closed vocabulary on the gate field already
//! diagnoses (`fields.<name>.vocabulary`, checked by `check`), so the typo is
//! *reported* where a forgiving match would silently forgive it.
//!
//! # Why an export has no front page
//!
//! A diaryx site fronts its published set with an `index:` page. That key is
//! not here: which page greets a reader is a *rendering* decision, and an
//! export is a set — the OCFL/copy-out consumer has no front page, and a
//! publish layer that wants one declares it in its own block, where its render
//! exists. MoReq2010 keeps access control as its own service rather than a
//! property of the classification scheme for the same reason `prov-views`
//! keeps classification apart from aggregation: conjoined schemes hybridize.

use prov_graph::meta::{Mapping, Value};
use prov_views::humanize;

/// The config block exports are declared in — a top-level axis beside
/// `views:`, so every prov tool reads the same exports rather than each app
/// namespacing its own.
pub const EXPORTS_KEY: &str = "exports";

/// The keys valid inside one `exports.<name>` entry.
pub const EXPORT_KEYS: &[&str] = &["label", "gate", "view"];

/// The keys valid inside a `gate:` mapping.
pub const GATE_KEYS: &[&str] = &["field", "value"];

/// The membership test an export runs on every reachable document: *does this
/// document's `field` declare `value`?*
///
/// The whole gate is these two strings, and [`admits`](Self::admits) never
/// reads anything but the one field — that locality is the audit property the
/// format exists to keep (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gate {
    /// The metadata field the document declares its membership in.
    pub field: String,
    /// The value that admits a document, matched exactly after trimming.
    pub value: String,
}

impl Gate {
    /// Read a `gate:` value: a mapping with a non-empty `field` and `value`.
    ///
    /// Anything else is `None` — there is no shorthand form and no default.
    /// A gate that does not say both halves is not a gate, and guessing either
    /// would admit documents nobody chose.
    pub fn parse(value: &Value) -> Option<Self> {
        let map = value.as_mapping()?;
        Some(Gate {
            field: non_empty(map.get("field"))?,
            value: non_empty(map.get("value"))?,
        })
    }

    /// The mapping this gate writes back as.
    pub fn to_value(&self) -> Value {
        let mut map = Mapping::new();
        map.insert("field".into(), Value::String(self.field.clone()));
        map.insert("value".into(), Value::String(self.value.clone()));
        Value::Mapping(map)
    }

    /// The values `meta` declares under this gate's field, or `None` when the
    /// field is absent altogether.
    ///
    /// The distinction matters for reporting: `None` is *undeclared* (the
    /// default state of every document), where `Some(vec![])` is a field that
    /// is present but empty — declared, and in no export. Both are outside the
    /// gate; only the second is something the author wrote.
    ///
    /// Values are read the way a view groups them: the trimmed text of a
    /// scalar, or of every scalar in a sequence. A mapping has no single text
    /// and declares nothing; a nested sequence is not flattened.
    pub fn declared_in(&self, meta: &Value) -> Option<Vec<String>> {
        Some(scalar_texts(meta.get(&self.field)?))
    }

    /// Whether `meta` declares this gate's value — the membership test.
    ///
    /// Exact after trim, in both directions; see the module docs for why a
    /// forgiving match is the fail-open direction.
    pub fn admits(&self, meta: &Value) -> bool {
        self.declared_in(meta)
            .is_some_and(|declared| declared.iter().any(|v| v == self.value.trim()))
    }
}

/// One export a workspace declares for itself: a gate that bounds what leaves,
/// optionally arranged by a view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportSpec {
    /// The key under `exports` — the export's public handle (a path segment,
    /// an OCFL object prefix). Deliberately its own name rather than the gate
    /// value's: the honest name for a readership is routinely one its members
    /// should never read off a URL, and separating them lets two gates share
    /// an arrangement or one gate carry two exports.
    pub name: String,
    /// What a person calls it. Absent falls back to the name, humanized.
    pub label: Option<String>,
    /// The gate whose admitted set bounds this export. Required: an export
    /// that does not say what may leave is not an export.
    pub gate: Gate,
    /// The [`ViewSpec`](prov_views::ViewSpec) naming this export's
    /// arrangement, by its key under `views:`. `None` exports the gate's whole
    /// admitted set. A view may narrow the set; it can never widen it — see
    /// [`plan`](crate::plan()).
    pub view: Option<String>,
}

impl ExportSpec {
    /// Read one `exports.<name>` entry.
    ///
    /// Returns `None` when the entry is not a mapping or carries no readable
    /// [`Gate`]. Dropping such an entry is the fail-closed direction — an
    /// unreadable export declaration exports nothing, where defaulting its
    /// gate would export a set nobody chose. [`crate::diagnose_export`] is the
    /// half that says *why*, so a malformed entry is reported rather than
    /// merely dropped.
    pub fn parse(name: &str, value: &Value) -> Option<Self> {
        let map = value.as_mapping()?;
        let gate = Gate::parse(map.get("gate")?)?;
        Some(ExportSpec {
            name: name.to_string(),
            label: non_empty(map.get("label")),
            gate,
            view: non_empty(map.get("view")),
        })
    }

    /// The mapping this export writes back as. Absent options are omitted
    /// rather than written empty, so an export declared from an app reads as
    /// the small thing it is.
    pub fn to_mapping(&self) -> Mapping {
        let mut map = Mapping::new();
        if let Some(label) = &self.label {
            map.insert("label".into(), Value::String(label.clone()));
        }
        map.insert("gate".into(), self.gate.to_value());
        if let Some(view) = &self.view {
            map.insert("view".into(), Value::String(view.clone()));
        }
        map
    }

    /// What a person calls this export: its label, else its name humanized.
    pub fn display_label(&self) -> String {
        match &self.label {
            Some(label) => label.clone(),
            None => humanize(&self.name),
        }
    }
}

/// Read every `exports.<name>` entry out of a config surface's `exports:`
/// block, in declaration order.
pub fn exports_from(config: &Mapping) -> Vec<ExportSpec> {
    let Some(exports) = config.get(EXPORTS_KEY).and_then(Value::as_mapping) else {
        return Vec::new();
    };
    exports
        .iter()
        .filter_map(|(name, value)| ExportSpec::parse(name, value))
        .collect()
}

/// The trimmed, non-empty text of a scalar, or of every scalar in a sequence —
/// the same reading `prov-views` groups by, so a field means one thing to a
/// gate and to a view.
fn scalar_texts(value: &Value) -> Vec<String> {
    match value {
        Value::Sequence(items) => items.iter().filter_map(scalar_text).collect(),
        other => scalar_text(other).into_iter().collect(),
    }
}

/// One scalar's trimmed text, or `None` for a null, an empty string, or a
/// composite. Numbers and booleans are rendered rather than skipped, exactly
/// as a view groups them.
fn scalar_text(value: &Value) -> Option<String> {
    let text = match value {
        Value::String(s) => s.trim().to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null | Value::Sequence(_) | Value::Mapping(_) => return None,
    };
    (!text.is_empty()).then_some(text)
}

/// A trimmed non-empty string from a config value, or `None`.
pub(crate) fn non_empty(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?.trim();
    (!text.is_empty()).then(|| text.to_string())
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

    fn text(pairs: &[(&str, &str)]) -> Value {
        let owned: Vec<(&str, Value)> = pairs
            .iter()
            .map(|(k, v)| (*k, Value::String((*v).to_string())))
            .collect();
        mapping(&owned)
    }

    fn gate(field: &str, value: &str) -> Value {
        text(&[("field", field), ("value", value)])
    }

    fn meta(pairs: &[(&str, Value)]) -> Value {
        mapping(pairs)
    }

    fn seq(items: &[&str]) -> Value {
        Value::Sequence(items.iter().map(|s| Value::String((*s).into())).collect())
    }

    #[test]
    fn an_export_reads_its_gate_view_and_label() {
        let spec = ExportSpec::parse(
            "letters",
            &mapping(&[
                ("label", Value::String("Letters home".into())),
                ("gate", gate("audience", "family")),
                ("view", Value::String("daily".into())),
            ]),
        )
        .expect("an export");
        assert_eq!(spec.gate.field, "audience");
        assert_eq!(spec.gate.value, "family");
        assert_eq!(spec.view.as_deref(), Some("daily"));
        assert_eq!(spec.display_label(), "Letters home");
    }

    /// An entry that never says what may leave is not an export. Recording it
    /// would put an exportable set in the list with no gate behind it.
    #[test]
    fn an_entry_without_a_gate_is_not_an_export() {
        assert!(ExportSpec::parse("x", &text(&[("view", "daily")])).is_none());
        assert!(ExportSpec::parse("x", &mapping(&[("gate", gate("audience", "  "))])).is_none());
        assert!(ExportSpec::parse("x", &mapping(&[("gate", gate("  ", "family"))])).is_none());
        // There is no shorthand: a bare string names half a gate at best.
        assert!(
            ExportSpec::parse("x", &mapping(&[("gate", Value::String("family".into()))])).is_none()
        );
        assert!(ExportSpec::parse("x", &Value::String("family".into())).is_none());
    }

    #[test]
    fn an_export_round_trips_through_its_mapping() {
        let spec = ExportSpec {
            name: "letters".into(),
            label: Some("Letters home".into()),
            gate: Gate {
                field: "audience".into(),
                value: "family".into(),
            },
            view: Some("daily".into()),
        };
        let back =
            ExportSpec::parse("letters", &Value::Mapping(spec.to_mapping())).expect("an export");
        assert_eq!(back, spec);

        let minimal = ExportSpec {
            name: "letters".into(),
            label: None,
            gate: Gate {
                field: "audience".into(),
                value: "family".into(),
            },
            view: None,
        };
        let map = minimal.to_mapping();
        assert!(map.get("label").is_none(), "absent options are omitted");
        assert!(map.get("view").is_none());
        let back = ExportSpec::parse("letters", &Value::Mapping(map)).expect("an export");
        assert_eq!(back, minimal);
    }

    #[test]
    fn a_gate_admits_a_declared_value_scalar_or_sequence() {
        let g = Gate {
            field: "audience".into(),
            value: "family".into(),
        };
        assert!(g.admits(&meta(&[("audience", Value::String("family".into()))])));
        assert!(g.admits(&meta(&[("audience", seq(&["friends", "family"]))])));
        assert!(!g.admits(&meta(&[("audience", seq(&["friends"]))])));
    }

    /// Closed by default — the property the whole crate exists for. A document
    /// that declares nothing is in no export, and a declared-but-empty field
    /// is equally outside; the two differ only in what a report says.
    #[test]
    fn an_undeclared_document_is_admitted_nowhere() {
        let g = Gate {
            field: "audience".into(),
            value: "family".into(),
        };
        assert!(!g.admits(&meta(&[])));
        assert!(!g.admits(&meta(&[("audience", Value::Null)])));
        assert!(!g.admits(&meta(&[("audience", seq(&[]))])));

        assert_eq!(g.declared_in(&meta(&[])), None, "undeclared");
        assert_eq!(
            g.declared_in(&meta(&[("audience", seq(&[]))])),
            Some(vec![]),
            "declared but empty — written, and still in no export"
        );
    }

    /// Exact after trim. A forgiving match is the fail-open direction: the
    /// written config would say less than the gate does. Casing drift is the
    /// vocabulary lint's to report, not the gate's to forgive.
    #[test]
    fn matching_is_exact_after_trim() {
        let g = Gate {
            field: "audience".into(),
            value: "family".into(),
        };
        assert!(g.admits(&meta(&[("audience", Value::String("  family  ".into()))])));
        assert!(!g.admits(&meta(&[("audience", Value::String("Family".into()))])));
        assert!(!g.admits(&meta(&[("audience", Value::String("FAMILY".into()))])));
    }

    /// A composite value declares nothing: a mapping has no single text, and a
    /// nested sequence is a shape no frontmatter field means to write. Skipped
    /// rather than rendered, so a malformed value cannot spell a gate value by
    /// accident.
    #[test]
    fn a_composite_value_declares_nothing() {
        let g = Gate {
            field: "audience".into(),
            value: "family".into(),
        };
        assert!(!g.admits(&meta(&[(
            "audience",
            meta(&[("family", Value::Bool(true))])
        )])));
        assert!(!g.admits(&meta(&[(
            "audience",
            Value::Sequence(vec![seq(&["family"])])
        )])));
    }

    /// Values are read the way a view groups them, so a field means one thing
    /// to a gate and to a view — including the non-string scalars.
    #[test]
    fn a_non_string_scalar_is_matched_by_its_text() {
        let g = Gate {
            field: "tier".into(),
            value: "5".into(),
        };
        assert!(g.admits(&meta(&[("tier", Value::Int(5))])));
    }

    #[test]
    fn exports_read_in_declaration_order() {
        let mut exports = Mapping::new();
        exports.insert(
            "letters".into(),
            mapping(&[("gate", gate("audience", "family"))]),
        );
        exports.insert(
            "notes".into(),
            mapping(&[("gate", gate("audience", "public"))]),
        );
        let mut config = Mapping::new();
        config.insert(EXPORTS_KEY.into(), Value::Mapping(exports));

        let specs = exports_from(&config);
        assert_eq!(
            specs.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["letters", "notes"]
        );
    }

    #[test]
    fn a_label_falls_back_to_the_humanized_name() {
        let spec = ExportSpec::parse(
            "letters_home",
            &mapping(&[("gate", gate("audience", "family"))]),
        )
        .expect("an export");
        assert_eq!(spec.display_label(), "Letters home");
    }
}
