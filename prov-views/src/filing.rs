//! Filing: where a new record goes — the `filing:` axis.
//!
//! A view reads. A filing entry says where a frontend should *write* a new
//! record: under which index, by which field, how deep. The two used to be one
//! declaration, a view with a `nest:` key, and that was the mistake this module
//! exists to correct. Filing writes into the spine, which is single-parent, so
//! it needs guarantees before anything runs — one value per record, a grain
//! whose levels chain — and a view carrying a `nest:` had to live inside those
//! guarantees even when it was only being read. Apart, a view is free to be a
//! query ([`crate::expr`]) and a filing entry stays a small closed declaration
//! prov can check.
//!
//! ```yaml
//! filing:
//!   daily:
//!     under: '[Daily](id:abc1234)'
//!     field: [date_of_document, created]
//!     nest: year
//!   journal:
//!     under: '[Calendar](/Calendar/index.md)'
//!     field: written.on
//!     nest: ref
//! ```
//!
//! # Classification is not aggregation
//!
//! The split is [MoReq2010]'s. ISO 15489 calls *classification* the
//! identification of a record by the context that produced it; MoReq2010
//! §1.4.5 separates that from *aggregation*, "the activity of assembling
//! related records together", and warns what happens when the two are
//! conjoined: schemes hybridize, and naturally occurring aggregations get split
//! apart to fit the classification. A view is classification — how records
//! become groups, for reading. A filing entry is aggregation — the index a
//! record actually hangs under. Keeping them apart is what keeps a change to
//! how a view reads from moving where tomorrow's entry lands.
//!
//! # Filing by grain, and by reference
//!
//! `nest:` takes a [`Grain`], which computes the shelf from the value:
//! `2026-07-24` becomes the index titled `2026` and the one titled `2026-07`
//! inside it. Or it takes `ref` ([`Nest::Ref`]), which files the record under
//! the document its value *links to*, whose own place in the spine is the rest
//! of the chain — so the chain condition a grain has to prove holds by
//! construction, and nothing here knows the shelf is a day. The field should
//! be declared `type: ref`, so that a move of the shelf rewrites every record
//! that files under it (`prov-config` reports it when it is not).
//!
//! # prov describes; the frontend files
//!
//! [`FilingSpec::route`] returns index *titles* or a link, which is exactly
//! what prov's route addressing takes. Nothing here creates a file: this crate
//! cannot write, and a filing entry has no invariant of its own until a
//! frontend acts on it.
//!
//! [MoReq2010]: https://moreq.info/files/moreq2010_vol1_v1_1_en.pdf

use prov_graph::field::{FieldPath, values_at};
use prov_graph::meta::{Mapping, Value};

use crate::grain::Grain;
use crate::scalar::scalar_texts;

/// The config block filing entries are declared in.
pub const FILING_KEY: &str = "filing";

/// The keys valid inside one `filing.<name>` entry.
pub const FILING_KEYS: &[&str] = &["label", "under", "field", "nest"];

/// The `nest:` spellings that are a bare word: every grain's, and `ref`.
pub const NESTS: &[&str] = &["year", "month", "day", "initial", "ref"];

/// How a record is filed — the `nest:` value.
///
/// Two shapes, and the difference is who knows where the shelf is. A
/// [`Grain`] computes it from the value, so the crate must know what a year
/// or an initial is. [`Ref`](Self::Ref) reads it off the record: the value is
/// a link, and the record files under the document it links to, whose place
/// in the spine is the whole chain. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nest {
    /// File under an index at this grain, the coarser indexes above it —
    /// `["2026", "2026-07"]` for a month.
    Grain(Grain),
    /// File under the document the field's value links to.
    Ref,
}

impl Nest {
    /// Read a `nest:` value: a grain's spelling, or the word `ref`.
    pub fn parse(value: &Value) -> Option<Self> {
        if let Some(text) = value.as_str()
            && text.trim() == "ref"
        {
            return Some(Nest::Ref);
        }
        Grain::parse(value).map(Nest::Grain)
    }

    /// The value this writes back as.
    pub fn to_value(self) -> Value {
        match self {
            Nest::Grain(grain) => grain.to_value(),
            Nest::Ref => Value::String("ref".into()),
        }
    }

    /// How this reads in a listing (`month`, `initial 2`, `ref`).
    pub fn display(self) -> String {
        match self {
            Nest::Grain(grain) => grain.display(),
            Nest::Ref => "ref".to_string(),
        }
    }

    /// The grain, when this nest is one.
    pub fn grain(self) -> Option<Grain> {
        match self {
            Nest::Grain(grain) => Some(grain),
            Nest::Ref => None,
        }
    }
}

/// Where a record files — what [`FilingSpec::route`] answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NestRoute {
    /// The index *titles* to file under, coarsest first, below the entry's
    /// [`under`](FilingSpec::under) — `["2026", "2026-07"]` — which is exactly
    /// what prov's route addressing takes, so a frontend hands them to
    /// `plan_route` and never assembles a path. An index that does not exist
    /// yet is the frontend's to create. Empty for an entry that files flat.
    Titles(Vec<String>),
    /// The link the record's own field carries, as written. The record files
    /// under whatever it resolves to — by path, by `id:`, or by title — and
    /// the frontend resolves it from where the record will live, since a
    /// relative link is relative to its document. Nothing is created: a link
    /// to no document is a broken link, not a shelf.
    Link(String),
}

/// One filing entry a workspace declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilingSpec {
    /// The key under `filing`.
    pub name: String,
    /// What a person calls it. Absent falls back to the name, humanized.
    pub label: Option<String>,
    /// The index new records go below, as a link. `None` files below the
    /// workspace root.
    pub under: Option<String>,
    /// The field paths a record is filed by, tried in order — the first that
    /// is filled in wins. Empty only for an entry that files flat.
    pub field: Vec<String>,
    /// How deep, or by reference. `None` files flat, directly under
    /// [`under`](Self::under).
    pub nest: Option<Nest>,
}

impl FilingSpec {
    /// Read one `filing.<name>` entry.
    ///
    /// `None` when the entry is not a mapping, names a `nest:` that is neither
    /// a grain nor `ref`, or nests without saying by which `field:`. A filing
    /// entry that would file somewhere other than where it says is worse than
    /// none, so an unreadable one is dropped and [`diagnose_filing`] says why.
    pub fn parse(name: &str, value: &Value) -> Option<Self> {
        let map = value.as_mapping()?;
        let nest = match map.get("nest") {
            Some(value) => Some(Nest::parse(value)?),
            None => None,
        };
        let field = field_chain(map.get("field"));
        if nest.is_some() && field.is_empty() {
            return None;
        }
        Some(FilingSpec {
            name: name.to_string(),
            label: non_empty(map.get("label")),
            under: non_empty(map.get("under")),
            field,
            nest,
        })
    }

    /// The mapping this entry writes back as.
    pub fn to_mapping(&self) -> Mapping {
        let mut map = Mapping::new();
        if let Some(label) = &self.label {
            map.insert("label".into(), Value::String(label.clone()));
        }
        if let Some(under) = &self.under {
            map.insert("under".into(), Value::String(under.clone()));
        }
        match self.field.as_slice() {
            [] => {}
            [one] => {
                map.insert("field".into(), Value::String(one.clone()));
            }
            many => {
                map.insert(
                    "field".into(),
                    Value::Sequence(many.iter().cloned().map(Value::String).collect()),
                );
            }
        }
        if let Some(nest) = self.nest {
            map.insert("nest".into(), nest.to_value());
        }
        map
    }

    /// What a person calls this entry.
    pub fn display_label(&self) -> String {
        match &self.label {
            Some(label) => label.clone(),
            None => crate::spec::humanize(&self.name),
        }
    }

    /// Where a new record with metadata `meta` files — or `None` when it
    /// cannot be filed.
    ///
    /// For an entry at month grain this is the titles `["2026", "2026-07"]`;
    /// for one at `initial 2`, `["A", "AD"]`; for one that files flat, no
    /// titles at all. For one that files by reference it is the
    /// [link](NestRoute::Link) the record carries.
    ///
    /// `None` when no field in the chain carries a usable value, when the
    /// grain cannot cut it all the way down, or when the value is
    /// **multi-valued**. That last is the constraint prov's spanning relation
    /// imposes: a document with two people cannot hang under two parents,
    /// and picking one would be inventing an answer the workspace did not
    /// give. A record linking to two shelves is the same case.
    pub fn route(&self, meta: &Value) -> Option<NestRoute> {
        let Some(nest) = self.nest else {
            return Some(NestRoute::Titles(Vec::new()));
        };
        let values = chain_values(meta, &self.field);
        let [value] = values.as_slice() else {
            return None;
        };
        let grain = match nest {
            Nest::Ref => return Some(NestRoute::Link(value.clone())),
            Nest::Grain(grain) => grain,
        };
        let route: Vec<String> = grain
            .chain()
            .into_iter()
            .filter_map(|step| step.cut(value))
            .collect();
        // A partial chain would file a July entry under `2026` and call it
        // done, which is a different place from the one the entry describes.
        (route.len() == grain.chain().len()).then_some(NestRoute::Titles(route))
    }

    /// The link a record files under when this entry files by reference —
    /// the single-valued half of [`route`](Self::route) for a caller that
    /// only files that way.
    pub fn link(&self, meta: &Value) -> Option<String> {
        match self.route(meta)? {
            NestRoute::Link(link) => Some(link),
            NestRoute::Titles(_) => None,
        }
    }
}

/// The values of the first field in `chain` that carries any.
fn chain_values(meta: &Value, chain: &[String]) -> Vec<String> {
    for key in chain {
        let raw: Vec<String> = values_at(meta, &FieldPath::parse(key))
            .into_iter()
            .flat_map(|(_, value)| scalar_texts(value))
            .collect();
        if !raw.is_empty() {
            return raw;
        }
    }
    Vec::new()
}

/// The field chain a `field:` value names — a bare string, or a list.
fn field_chain(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(s)) if !s.trim().is_empty() => vec![s.trim().to_string()],
        Some(Value::Sequence(items)) => items.iter().filter_map(|v| non_empty(Some(v))).collect(),
        _ => Vec::new(),
    }
}

fn non_empty(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Read every `filing.<name>` entry out of a config surface, in declaration
/// order.
pub fn filing_from(config: &Mapping) -> Vec<FilingSpec> {
    let Some(entries) = config.get(FILING_KEY).and_then(Value::as_mapping) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|(name, value)| FilingSpec::parse(name, value))
        .collect()
}

/// Something wrong with one `filing.<name>` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilingIssue {
    /// The entry.
    pub filing: String,
    /// The key at fault, or the empty string when the entry as a whole is.
    pub key: String,
    /// What is wrong with it.
    pub kind: FilingIssueKind,
}

/// The kinds of thing a `filing.<name>` entry gets wrong. Every one but
/// [`UnknownKey`](Self::UnknownKey) drops the entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilingIssueKind {
    /// The entry is not a mapping.
    NotAMapping,
    /// A `nest:` with no `field:` to nest by.
    NoField,
    /// A `nest:` that is neither a grain nor `ref`.
    BadNest,
    /// A key this format does not define.
    UnknownKey,
}

impl FilingIssueKind {
    /// The spellings a diagnostic should offer for this issue, if any.
    pub fn expected(&self) -> &'static [&'static str] {
        match self {
            FilingIssueKind::UnknownKey => FILING_KEYS,
            FilingIssueKind::BadNest => NESTS,
            _ => &[],
        }
    }
}

/// Diagnose one `filing.<name>` entry.
pub fn diagnose_filing(name: &str, value: &Value) -> Vec<FilingIssue> {
    let issue = |key: &str, kind| FilingIssue {
        filing: name.to_string(),
        key: key.to_string(),
        kind,
    };
    let Some(map) = value.as_mapping() else {
        return vec![issue("", FilingIssueKind::NotAMapping)];
    };
    let mut issues = Vec::new();
    if let Some(nest) = map.get("nest") {
        if Nest::parse(nest).is_none() {
            issues.push(issue("nest", FilingIssueKind::BadNest));
        } else if field_chain(map.get("field")).is_empty() {
            issues.push(issue("field", FilingIssueKind::NoField));
        }
    }
    for key in map.keys() {
        if !FILING_KEYS.contains(&key.as_str()) {
            issues.push(issue(key, FilingIssueKind::UnknownKey));
        }
    }
    issues
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

    fn entry(field: &[&str], nest: &str) -> FilingSpec {
        FilingSpec::parse(
            "daily",
            &mapping(&[
                (
                    "field",
                    Value::Sequence(field.iter().map(|f| text(f)).collect()),
                ),
                ("nest", text(nest)),
            ]),
        )
        .expect("an entry")
    }

    #[test]
    fn a_grain_routes_through_its_chain() {
        let spec = entry(&["date_of_document", "created"], "month");
        let meta = mapping(&[("created", text("2026-07-24"))]);
        assert_eq!(
            spec.route(&meta),
            Some(NestRoute::Titles(vec!["2026".into(), "2026-07".into()]))
        );
    }

    #[test]
    fn a_multi_valued_or_uncuttable_value_has_no_route() {
        let spec = entry(&["people"], "initial");
        let two = mapping(&[("people", Value::Sequence(vec![text("Ada"), text("Grace")]))]);
        assert_eq!(spec.route(&two), None);
        let spec = entry(&["created"], "day");
        assert_eq!(spec.route(&mapping(&[("created", text("2026"))])), None);
        assert_eq!(spec.route(&mapping(&[])), None);
    }

    #[test]
    fn ref_routes_to_the_link_the_record_carries() {
        let spec = entry(&["written.on"], "ref");
        let meta = mapping(&[(
            "written",
            mapping(&[("on", text("/Calendar/2026/09/17.md"))]),
        )]);
        assert_eq!(spec.link(&meta).as_deref(), Some("/Calendar/2026/09/17.md"));
    }

    #[test]
    fn an_entry_without_nest_files_flat() {
        let spec = FilingSpec::parse("tasks", &mapping(&[("under", text("[[Tasks]]"))]))
            .expect("an entry");
        assert_eq!(spec.route(&mapping(&[])), Some(NestRoute::Titles(vec![])));
    }

    #[test]
    fn an_unreadable_entry_is_dropped_and_diagnosed() {
        let bad = mapping(&[("field", text("created")), ("nest", text("yearr"))]);
        assert_eq!(FilingSpec::parse("x", &bad), None);
        assert_eq!(diagnose_filing("x", &bad)[0].kind, FilingIssueKind::BadNest);

        let no_field = mapping(&[("nest", text("year"))]);
        assert_eq!(FilingSpec::parse("x", &no_field), None);
        assert_eq!(
            diagnose_filing("x", &no_field)[0].kind,
            FilingIssueKind::NoField
        );

        let typo = mapping(&[("feild", text("created"))]);
        assert!(FilingSpec::parse("x", &typo).is_some());
        assert_eq!(
            diagnose_filing("x", &typo)[0].kind,
            FilingIssueKind::UnknownKey
        );
    }

    #[test]
    fn an_entry_round_trips() {
        let spec = entry(&["date_of_document", "created"], "year");
        let back = FilingSpec::parse("daily", &Value::Mapping(spec.to_mapping()));
        assert_eq!(back, Some(spec));
    }
}
