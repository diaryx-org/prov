//! # prov-filing
//!
//! Filing: where a new record goes — the `filing:` axis of a
//! [prov](https://docs.rs/prov) workspace.
//!
//! A view reads. A filing entry says where a frontend should *write* a new
//! record: under which index, by which field, how deep. The two used to be one
//! declaration, a view with a `nest:` key, and that was the mistake this crate
//! exists to correct. Filing writes into the spine, which is single-parent, so
//! it needs guarantees before anything runs — one value per record, a grain
//! whose levels chain — and a view carrying a `nest:` had to live inside those
//! guarantees even when it was only being read. Apart, a view is free to be a
//! query (`prov-views`) and a filing entry stays a small closed declaration
//! prov can check. The two share only the [`Grain`]s they cut values by, which
//! are `prov-grain`'s, so this crate does not depend on the view engine.
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
//! # Which records an entry files
//!
//! An entry may name the kinds of record it is for — `kind: [image, video]`,
//! or `kind: attachment` for every payload kind — in prov's
//! [`RecordKind`] words. A host with one way to add anything (a page, a
//! photograph, a recording) asks [`filing_for_kind`] which entry takes the
//! kind it is adding. Only an entry that *names* the kind answers: an entry
//! that names none files whatever it is asked to, and which of several such
//! entries a host uses — Daily, an inbox — stays the host's choice, as it was
//! before kinds. Two entries naming the same kind is an ambiguity
//! [`diagnose_kinds`] reports, since a photograph cannot be filed in two
//! places.
//!
//! # This crate describes; `prov` files
//!
//! [`FilingSpec::route`] returns index *titles* or a link, which is exactly
//! what prov's route addressing takes. Nothing here creates a file: this crate
//! cannot write. `prov`'s `Workspace::file` is where a route is acted on — the
//! anchor resolved, each index found by the period it carries or made, the
//! container returned — so a frontend asks it rather than walking the route
//! itself.
//!
//! [MoReq2010]: https://moreq.info/files/moreq2010_vol1_v1_1_en.pdf

use prov_graph::field::{FieldPath, values_at};
pub use prov_graph::kind::{ATTACHMENT_KINDS, RECORD_KINDS, RecordKind};
use prov_graph::meta::{Mapping, Value};

pub use prov_grain::Grain;
use prov_grain::scalar_texts;

/// The config block filing entries are declared in.
pub const FILING_KEY: &str = "filing";

/// The keys valid inside one `filing.<name>` entry.
pub const FILING_KEYS: &[&str] = &["label", "under", "field", "nest", "kind"];

/// The words a `kind:` value accepts: every [`RecordKind`], and
/// [`ATTACHMENT_KINDS`] for all the payload kinds at once.
pub const KIND_WORDS: &[&str] = &[
    "page",
    "image",
    "audio",
    "video",
    "file",
    "manifest",
    "attachment",
];

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
    /// The kinds of record this entry files, in declaration order, with
    /// `attachment` expanded. Empty files any kind — see the module docs.
    pub kind: Vec<RecordKind>,
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
        // An unknown kind drops the entry, as an unknown nest does: an entry
        // that would take records it was not written for files them somewhere
        // nobody chose.
        let kind = kind_list(map.get("kind"))?;
        Some(FilingSpec {
            name: name.to_string(),
            label: non_empty(map.get("label")),
            under: non_empty(map.get("under")),
            field,
            nest,
            kind,
        })
    }

    /// Whether this entry takes a record of `kind` — any kind, when it names
    /// none.
    pub fn files(&self, kind: RecordKind) -> bool {
        self.kind.is_empty() || self.kind.contains(&kind)
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
        if !self.kind.is_empty() {
            map.insert("kind".into(), kind_value(&self.kind));
        }
        map
    }

    /// What a person calls this entry.
    pub fn display_label(&self) -> String {
        match &self.label {
            Some(label) => label.clone(),
            None => humanize(&self.name),
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
    /// **multi-valued**. (`prov`'s `Workspace::file`, which has to put a real
    /// record somewhere, files the first two as deep as the value reaches —
    /// under the anchor when that is nowhere — and refuses the third.) That last is the constraint prov's spanning relation
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

/// The values of the first field in `chain` that carries any — how a filing
/// entry reads the value it files a record by, and so how a record is *dated*
/// (or initialled, or shelved) in that entry's terms.
///
/// Each key is a [field path](prov_graph::field::FieldPath) (`written.on`,
/// `sources[].date`), and each value it reaches is read as trimmed scalar text
/// ([`scalar_texts`]): a list gives each item, an empty string or null gives
/// nothing, so a blank `date_of_document:` falls through to `created`. The
/// result is empty when no key carries anything, and has more than one entry
/// when the first that does is multi-valued — which [`FilingSpec::route`]
/// declines to file.
///
/// Public so a frontend reading "the date this page files under" reads it the
/// way filing does rather than re-walking the chain.
pub fn chain_values(meta: &Value, chain: &[String]) -> Vec<String> {
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

/// The kinds a `kind:` value names — a bare word, or a list of them — with
/// `attachment` expanded and repeats dropped. `Some(vec![])` when the key is
/// absent or empty; `None` when any word is not a kind.
fn kind_list(value: Option<&Value>) -> Option<Vec<RecordKind>> {
    let words: Vec<&str> = match value {
        None | Some(Value::Null) => return Some(Vec::new()),
        Some(Value::String(s)) => vec![s.as_str()],
        Some(Value::Sequence(items)) => {
            let mut words = Vec::new();
            for item in items {
                words.push(item.as_str()?);
            }
            words
        }
        Some(_) => return None,
    };
    let mut kinds = Vec::new();
    for word in words.into_iter().filter(|w| !w.trim().is_empty()) {
        for kind in RecordKind::parse_list_item(word)? {
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
    }
    Some(kinds)
}

/// How a kind list writes back: one word for one kind, `attachment` for the
/// four payload kinds together, a list otherwise.
fn kind_value(kinds: &[RecordKind]) -> Value {
    let all_payloads = RecordKind::payload_kinds()
        .iter()
        .all(|k| kinds.contains(k));
    let mut words: Vec<&str> = Vec::new();
    for kind in kinds {
        let word = if all_payloads && kind.is_payload() {
            ATTACHMENT_KINDS
        } else {
            kind.as_str()
        };
        if !words.contains(&word) {
            words.push(word);
        }
    }
    match words.as_slice() {
        [one] => Value::String((*one).to_string()),
        many => Value::Sequence(
            many.iter()
                .map(|w| Value::String((*w).to_string()))
                .collect(),
        ),
    }
}

/// Which entry files a record of a given kind — see [`filing_for_kind`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KindFiling<'a> {
    /// No entry names the kind. A host files it however it filed records
    /// before kinds — under an entry it chose, or where the person is.
    Unclaimed,
    /// Exactly one entry names it.
    One(&'a FilingSpec),
    /// Several do — the ambiguity [`diagnose_kinds`] reports. None of them
    /// is the answer.
    Ambiguous(Vec<&'a FilingSpec>),
}

/// The entry among `specs` that files records of `kind`: the one that names
/// it. An entry naming no kinds is not an answer here, however many kinds it
/// would take — see the module docs.
pub fn filing_for_kind(specs: &[FilingSpec], kind: RecordKind) -> KindFiling<'_> {
    let claiming: Vec<&FilingSpec> = specs.iter().filter(|s| s.kind.contains(&kind)).collect();
    match claiming.as_slice() {
        [] => KindFiling::Unclaimed,
        [one] => KindFiling::One(one),
        _ => KindFiling::Ambiguous(claiming),
    }
}

/// Every kind two or more entries claim: one issue per later entry, against
/// its `kind` key, naming the entry that claimed the kind first.
pub fn diagnose_kinds(specs: &[FilingSpec]) -> Vec<FilingIssue> {
    let mut first: Vec<(RecordKind, &str)> = Vec::new();
    let mut issues = Vec::new();
    for spec in specs {
        for kind in &spec.kind {
            match first.iter().find(|(k, _)| k == kind) {
                Some((_, by)) => issues.push(FilingIssue {
                    filing: spec.name.clone(),
                    key: "kind".to_string(),
                    kind: FilingIssueKind::KindClaimedTwice {
                        kind: kind.as_str().to_string(),
                        by: (*by).to_string(),
                    },
                }),
                None => first.push((*kind, spec.name.as_str())),
            }
        }
    }
    issues
}

/// The field chain a `field:` value names — a bare string, or a list.
fn field_chain(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(s)) if !s.trim().is_empty() => vec![s.trim().to_string()],
        Some(Value::Sequence(items)) => items.iter().filter_map(|v| non_empty(Some(v))).collect(),
        _ => Vec::new(),
    }
}

/// `daily_entries` → `Daily entries`: a key is written for a file, a label for
/// a person. The same rule a view's label falls back to.
fn humanize(key: &str) -> String {
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
/// [`UnknownKey`](Self::UnknownKey) and
/// [`KindClaimedTwice`](Self::KindClaimedTwice) drops the entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilingIssueKind {
    /// The entry is not a mapping.
    NotAMapping,
    /// A `nest:` with no `field:` to nest by.
    NoField,
    /// A `nest:` that is neither a grain nor `ref`.
    BadNest,
    /// A `kind:` naming something that is not a kind of record.
    BadKind,
    /// A kind another entry, `by`, already names. Reported against the
    /// later entry; the entry itself is still read.
    KindClaimedTwice {
        /// The kind both name.
        kind: String,
        /// The entry that named it first.
        by: String,
    },
    /// A key this format does not define.
    UnknownKey,
}

impl FilingIssueKind {
    /// The spellings a diagnostic should offer for this issue, if any.
    pub fn expected(&self) -> &'static [&'static str] {
        match self {
            FilingIssueKind::UnknownKey => FILING_KEYS,
            FilingIssueKind::BadNest => NESTS,
            FilingIssueKind::BadKind => KIND_WORDS,
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
    if kind_list(map.get("kind")).is_none() {
        issues.push(issue("kind", FilingIssueKind::BadKind));
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
    fn the_chain_reads_the_first_field_that_says_anything() {
        let chain = ["date_of_document".to_string(), "created".to_string()];
        let meta = mapping(&[
            ("date_of_document", text("  ")),
            ("created", text("2026-07-24")),
        ]);
        assert_eq!(chain_values(&meta, &chain), ["2026-07-24"]);
        assert!(chain_values(&mapping(&[]), &chain).is_empty());
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
    fn an_entry_names_the_kinds_it_files() {
        let spec = FilingSpec::parse(
            "photos",
            &mapping(&[("kind", Value::Sequence(vec![text("image"), text("Video")]))]),
        )
        .expect("an entry");
        assert_eq!(spec.kind, [RecordKind::Image, RecordKind::Video]);
        assert!(spec.files(RecordKind::Image));
        assert!(!spec.files(RecordKind::Page));

        let any = FilingSpec::parse("inbox", &mapping(&[])).expect("an entry");
        assert!(any.files(RecordKind::Audio), "no kind: files anything");

        let attachments =
            FilingSpec::parse("scans", &mapping(&[("kind", text("attachment"))])).unwrap();
        assert_eq!(attachments.kind, RecordKind::payload_kinds());
        assert_eq!(
            attachments.to_mapping().get("kind"),
            Some(&text("attachment")),
            "the four payload kinds write back as the word that named them"
        );
    }

    #[test]
    fn an_unknown_kind_drops_the_entry() {
        let bad = mapping(&[("kind", Value::Sequence(vec![text("image"), text("photo")]))]);
        assert_eq!(FilingSpec::parse("x", &bad), None);
        assert_eq!(diagnose_filing("x", &bad)[0].kind, FilingIssueKind::BadKind);
    }

    #[test]
    fn only_an_entry_that_names_a_kind_answers_for_it() {
        let parse = |name: &str, kind: Option<&str>| {
            let pairs: Vec<(&str, Value)> = kind.map(|k| ("kind", text(k))).into_iter().collect();
            FilingSpec::parse(name, &mapping(&pairs)).unwrap()
        };
        let specs = vec![
            parse("daily", None),
            parse("photos", Some("image")),
            parse("scans", Some("attachment")),
        ];
        assert_eq!(
            filing_for_kind(&specs, RecordKind::File),
            KindFiling::One(&specs[2])
        );
        assert_eq!(
            filing_for_kind(&specs, RecordKind::Page),
            KindFiling::Unclaimed
        );
        assert_eq!(
            filing_for_kind(&specs, RecordKind::Image),
            KindFiling::Ambiguous(vec![&specs[1], &specs[2]])
        );
        assert_eq!(
            diagnose_kinds(&specs),
            vec![FilingIssue {
                filing: "scans".into(),
                key: "kind".into(),
                kind: FilingIssueKind::KindClaimedTwice {
                    kind: "image".into(),
                    by: "photos".into()
                },
            }]
        );
    }

    #[test]
    fn an_entry_round_trips() {
        let spec = entry(&["date_of_document", "created"], "year");
        let back = FilingSpec::parse("daily", &Value::Mapping(spec.to_mapping()));
        assert_eq!(back, Some(spec));
    }
}
