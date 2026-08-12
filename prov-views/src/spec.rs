//! The view format: what a workspace declares under `views.<name>`.
//!
//! # Why a view is not a field declaration
//!
//! A declared field (`fields.<name>`) already makes a lens: the workspace says
//! it files things by `people`, so a frontend groups by `people`. That covers a
//! lens whose groups *are* one field's values, over the whole corpus.
//!
//! It cannot express the four things a real archive needs. **Scope**: a lens
//! over every file in the workspace buries the entries among the notes, drafts
//! and READMEs that happen to carry the same field. **Grain**: "by year" is a
//! rule about how a value becomes a group, and a field declaration has nowhere
//! to put it. **Fallback**: the value worth grouping on is often the first of
//! several fields that is filled in. **Conditions**: not everything in scope
//! belongs in every lens (see [`crate::filter`]).
//!
//! So a view is its own declaration:
//!
//! ```yaml
//! views:
//!   daily:
//!     label: Daily
//!     icon: calendar
//!     group: [date_of_document, created, updated]
//!     by: month
//!     under: '[Daily](/Daily/daily_index.md)'
//!     where:
//!       not: { has: draft }
//!     nest: month
//! ```
//!
//! # There is no `date` grouping
//!
//! An earlier form of this format spelled the above `group: date`, a token that
//! meant "the date chain" — and the chain itself (`date_of_document` →
//! `created` → `updated`) was hardcoded in whichever program was reading. Three
//! field names no workspace had agreed to, blessed by the tool.
//!
//! Here [`Grouping`] is one shape: an ordered list of field keys, first
//! non-empty wins, optionally [cut](Grain) at a grain. A date view is that
//! shape with date fields in it, and nothing in this crate knows the word
//! "date" — the chain above is a *declaration a workspace writes*, which is
//! what makes it reviewable, diffable, and different for a workspace that files
//! by `taken_on` or `received`.
//!
//! The grain applies to any value, not to a declared type: it is a prefix cut
//! over ISO-8601 text (see [`Grain::cut`]), so it works on the `2026-07-24`
//! that YAML hands back as a string without this crate having to resolve the
//! workspace's `fields.<name>.type` declarations. A value that is not
//! ISO-shaped does not group at that grain rather than grouping wrongly.
//!
//! # Classification is not aggregation
//!
//! The remaining shape is [MoReq2010]'s, not an invention. ISO 15489 calls
//! *classification* the identification of a record by the context that produced
//! it; MoReq2010 §1.4.5 separates that from *aggregation*, "the activity of
//! assembling related records together", which "may be based on any
//! organisational requirement or criteria, not business context alone". It
//! permits conjoining the two into one hierarchy and warns what happens when
//! you do: schemes hybridize, and naturally occurring aggregations get split
//! apart to fit the classification.
//!
//! That maps onto this struct exactly:
//!
//! - [`Grouping`] is classification — how records become groups.
//! - [`ViewSpec::under`] is aggregation — the index the records actually hang
//!   under, resolved through the spanning relation rather than by matching a
//!   path or a title, so it survives a rename, a move and a retitle.
//! - [`ViewSpec::nest`] is the *deliberate* seam between them. It is not
//!   derived from [`Grouping::by`], because a lens must never become a reason
//!   to move a file: changing how a view groups is a reading decision, and it
//!   would be a poor bargain if a picker that reads like a display setting
//!   silently changed where tomorrow's entry lands.
//!
//! # Inheritance and override
//!
//! `under:` is inherited: a view covers the whole subtree below its anchor, not
//! just the anchor's direct children. This is MoReq2010 §201.2.3 — a class
//! applied at a root aggregation "is inherited as the default classification
//! for all descendants". §201.2.4 then allows a class applied directly to a
//! child to break that chain, which is what keeps aggregations from having to
//! be homogeneous. That override is a document-level concern and is not part of
//! this struct; the scope walk in [`crate::select`] is the inheritance half.
//!
//! [MoReq2010]: https://moreq.info/files/moreq2010_vol1_v1_1_en.pdf

use prov_graph::meta::{Mapping, Value};

use crate::filter::Condition;

/// The config block views are declared in — a top-level axis, so every prov
/// tool reads the same views rather than each app namespacing its own.
pub const VIEWS_KEY: &str = "views";

/// The keys valid inside one `views.<name>` entry.
pub const VIEW_KEYS: &[&str] = &["label", "icon", "group", "by", "under", "nest", "where"];

/// How finely a value is cut into groups.
///
/// Deliberately stops at the day. An hour is an instant, not a grain anyone
/// files by, and the moment a group holds one document each the view has
/// stopped grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Grain {
    /// `2026` — the default, and what a lifetime of entries wants.
    #[default]
    Year,
    /// `2026-07`.
    Month,
    /// `2026-07-25`.
    Day,
}

/// Every spelling, in coarsest-first order — the set a diagnostic offers and a
/// picker lists.
pub const GRAINS: &[&str] = &["year", "month", "day"];

impl Grain {
    /// The config spelling.
    pub fn as_config_str(self) -> &'static str {
        match self {
            Grain::Year => "year",
            Grain::Month => "month",
            Grain::Day => "day",
        }
    }

    /// Parse a config spelling. Unknown text is **not** silently defaulted — a
    /// `by: yearr` that quietly grouped by year would look applied and be
    /// wrong, which is the failure a config linter exists to prevent.
    pub fn from_config_str(text: &str) -> Option<Self> {
        match text.trim() {
            "year" => Some(Grain::Year),
            "month" => Some(Grain::Month),
            "day" => Some(Grain::Day),
            _ => None,
        }
    }

    /// How many characters of an ISO-8601 date this grain keeps: `2026-07-25`
    /// cut to 4, 7 or 10.
    ///
    /// The group key is a *prefix* because an ISO date sorts lexically, so the
    /// group order falls out of the string with no calendar arithmetic and no
    /// time zone to get wrong.
    pub fn prefix_len(self) -> usize {
        match self {
            Grain::Year => 4,
            Grain::Month => 7,
            Grain::Day => 10,
        }
    }

    /// The grains to nest through to reach `self`, coarsest first: filing at
    /// month grain means a year index and then a month index inside it. A month
    /// index that is not inside its year is not where anyone looks for it.
    pub fn chain(self) -> &'static [Grain] {
        match self {
            Grain::Year => &[Grain::Year],
            Grain::Month => &[Grain::Year, Grain::Month],
            Grain::Day => &[Grain::Year, Grain::Month, Grain::Day],
        }
    }

    /// Cut `value` to this grain, or `None` if it is not ISO-8601-shaped that
    /// far.
    ///
    /// Validating rather than taking a blind prefix is what keeps `by:` usable
    /// on a view whose field is *usually* a date: `banana` cut to a year would
    /// otherwise group under `bana`, a group key that looks like data. A value
    /// this rejects falls to the ungrouped bucket, where it is visible as
    /// something that did not sort.
    ///
    /// Anything after the cut is ignored, so an RFC 3339 instant
    /// (`2026-07-24T07:32:00Z` — what a machine-maintained `updated` field
    /// carries) cuts exactly like the plain date it starts with.
    pub fn cut(self, value: &str) -> Option<String> {
        let text = value.trim();
        let bytes = text.as_bytes();
        if bytes.len() < self.prefix_len() {
            return None;
        }
        // `YYYY`, then `-MM` and `-DD` as the grain demands. Checked by byte
        // because every character an ISO date is allowed to use is ASCII, so
        // the prefix is a character boundary by construction.
        let shape_ok = bytes[..4].iter().all(u8::is_ascii_digit)
            && match self {
                Grain::Year => true,
                Grain::Month => bytes[4] == b'-' && bytes[5..7].iter().all(u8::is_ascii_digit),
                Grain::Day => {
                    bytes[4] == b'-'
                        && bytes[5..7].iter().all(u8::is_ascii_digit)
                        && bytes[7] == b'-'
                        && bytes[8..10].iter().all(u8::is_ascii_digit)
                }
            };
        // A year cut must not swallow the head of a longer number: `20264` is
        // not the year 2026. Every other grain is already delimited by its `-`.
        let bounded = match bytes.get(self.prefix_len()) {
            Some(b) if self == Grain::Year => !b.is_ascii_digit(),
            _ => true,
        };
        (shape_ok && bounded).then(|| text[..self.prefix_len()].to_string())
    }
}

/// What a view sorts records by — MoReq2010's *classification*.
///
/// One shape, not a set of blessed kinds: an ordered chain of field keys, and
/// an optional grain to cut the chosen value at. See the module docs for why
/// there is no `date` variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grouping {
    /// The field keys to read, in order — the first that carries a value wins,
    /// and supplies *all* of that view's group keys for the document.
    /// Guaranteed non-empty by [`ViewSpec::parse`].
    pub keys: Vec<String>,
    /// The grain the chosen value is cut at, or `None` to group on the value
    /// itself.
    pub by: Option<Grain>,
}

impl Grouping {
    /// A view grouped on one field's raw values.
    pub fn field(key: impl Into<String>) -> Self {
        Grouping {
            keys: vec![key.into()],
            by: None,
        }
    }

    /// The group keys `meta` falls under — empty when no field in the chain
    /// carries a usable value, which is the ungrouped bucket.
    ///
    /// A sequence-valued field yields one key per element, so a letter about
    /// two people appears under both. That is the whole point of a view: the
    /// same document reached several ways, with retrieval decoupled from the
    /// single containment spine.
    ///
    /// The chain stops at the first key that is *present and non-empty*, and
    /// its values are used even if the grain rejects all of them. Falling
    /// through to `created` because `date_of_document` held something
    /// unparseable would silently file the document under a date it does not
    /// claim; leaving it ungrouped shows the bad value instead.
    pub fn keys_of(&self, meta: &Value) -> Vec<String> {
        for key in &self.keys {
            let Some(value) = meta.get(key) else { continue };
            let raw = scalar_texts(value);
            if raw.is_empty() {
                continue;
            }
            return match self.by {
                Some(grain) => raw.iter().filter_map(|t| grain.cut(t)).collect(),
                None => raw,
            };
        }
        Vec::new()
    }

    /// The `group:` value this writes back as: a bare string for a single key,
    /// a list for a chain, so a one-field view reads as the small thing it is.
    fn to_value(&self) -> Value {
        match self.keys.as_slice() {
            [only] => Value::String(only.clone()),
            many => Value::Sequence(many.iter().cloned().map(Value::String).collect()),
        }
    }
}

/// The trimmed, non-empty text of a scalar, or of every scalar in a sequence.
///
/// A view groups on what a value *says*, so the numeric and boolean cases are
/// rendered rather than skipped — a `rating: 5` groups under `5`. A mapping has
/// no single text and is not groupable; a nested sequence is not flattened,
/// because a list of lists is a shape no frontmatter field means to declare.
pub(crate) fn scalar_texts(value: &Value) -> Vec<String> {
    match value {
        Value::Sequence(items) => items.iter().filter_map(scalar_text).collect(),
        other => scalar_text(other).into_iter().collect(),
    }
}

/// One scalar's trimmed text, or `None` for a null, an empty string, or a
/// composite.
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
    /// Classification — how records become groups.
    pub group: Grouping,
    /// Aggregation — the index this view's records hang under, as a link
    /// (`'[Daily](id:abc1234)'`). `None` scopes the view to the whole
    /// workspace.
    pub under: Option<String>,
    /// The `where:` conditions a document in scope must also meet. `None`
    /// takes everything scope reaches.
    ///
    /// Named `filter` because `where` is a Rust keyword; the config spelling is
    /// `where`, which is what a reader of the format sees.
    ///
    /// Separate from [`under`](Self::under) because the two fail differently:
    /// an anchor that names nothing is a broken view, while a condition that
    /// matches nothing is an ordinary empty answer.
    pub filter: Option<Condition>,
    /// Materialization: when set, filing a new record through this view nests
    /// it under an index at this grain below [`under`](Self::under), creating
    /// the index if the calendar has turned. `None` files flat.
    ///
    /// Independent of [`Grouping::by`] on purpose — see the module docs.
    pub nest: Option<Grain>,
}

impl ViewSpec {
    /// Read one `views.<name>` entry.
    ///
    /// Returns `None` when the entry is not a mapping or names no groupable
    /// field — an entry that does not say what it groups by is not a view, and
    /// recording it as one would put a lens in the picker that groups nothing.
    /// [`crate::diagnose_view`] is the half that says *why*, so a malformed
    /// entry is reported rather than merely dropped.
    pub fn parse(name: &str, value: &Value) -> Option<Self> {
        let map = value.as_mapping()?;
        let keys = group_keys(map.get("group"))?;
        Some(ViewSpec {
            name: name.to_string(),
            label: non_empty(map.get("label")),
            icon: non_empty(map.get("icon")),
            group: Grouping {
                keys,
                by: map
                    .get("by")
                    .and_then(Value::as_str)
                    .and_then(Grain::from_config_str),
            },
            under: non_empty(map.get("under")),
            filter: map.get("where").and_then(Condition::parse),
            nest: map
                .get("nest")
                .and_then(Value::as_str)
                .and_then(Grain::from_config_str),
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
        map.insert("group".into(), self.group.to_value());
        if let Some(by) = self.group.by {
            map.insert("by".into(), Value::String(by.as_config_str().into()));
        }
        if let Some(under) = &self.under {
            map.insert("under".into(), Value::String(under.clone()));
        }
        if let Some(filter) = &self.filter {
            map.insert("where".into(), filter.to_value());
        }
        if let Some(nest) = self.nest {
            map.insert("nest".into(), Value::String(nest.as_config_str().into()));
        }
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

/// The field-key chain a `group:` value names — a bare string, or a list.
///
/// `None` when the value is absent, is neither of those shapes, or names no
/// non-empty key. Empty entries are dropped rather than carried, so
/// `group: [people, '']` is the one-key chain it plainly means.
fn group_keys(value: Option<&Value>) -> Option<Vec<String>> {
    let keys: Vec<String> = match value? {
        Value::String(s) => s
            .trim()
            .is_empty()
            .then(Vec::new)
            .unwrap_or_else(|| vec![s.trim().to_string()]),
        Value::Sequence(items) => items.iter().filter_map(|v| non_empty(Some(v))).collect(),
        _ => return None,
    };
    (!keys.is_empty()).then_some(keys)
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

    fn text(pairs: &[(&str, &str)]) -> Value {
        let owned: Vec<(&str, Value)> = pairs
            .iter()
            .map(|(k, v)| (*k, Value::String((*v).to_string())))
            .collect();
        mapping(&owned)
    }

    fn seq(items: &[&str]) -> Value {
        Value::Sequence(items.iter().map(|s| Value::String((*s).into())).collect())
    }

    /// The un-blessing, stated as a test: `date` is not a token. A view that
    /// says `group: date` groups on a *field called `date`* like any other, so
    /// nothing in this crate has to know the word.
    #[test]
    fn date_is_a_field_name_not_a_grouping_kind() {
        let spec = ViewSpec::parse("daily", &text(&[("group", "date")])).expect("a view");
        assert_eq!(spec.group, Grouping::field("date"));

        let mut doc = Mapping::new();
        doc.insert("date".into(), Value::String("2026-07-24".into()));
        assert_eq!(spec.group.keys_of(&Value::Mapping(doc)), ["2026-07-24"]);
    }

    #[test]
    fn a_chain_takes_the_first_field_that_carries_a_value() {
        let spec = ViewSpec::parse(
            "daily",
            &mapping(&[
                ("group", seq(&["date_of_document", "created", "updated"])),
                ("by", Value::String("month".into())),
            ]),
        )
        .expect("a view");

        let mut doc = Mapping::new();
        doc.insert("created".into(), Value::String("2026-07-24".into()));
        doc.insert("updated".into(), Value::String("2020-01-01".into()));
        assert_eq!(
            spec.group.keys_of(&Value::Mapping(doc)),
            ["2026-07"],
            "created wins over updated; the grain cuts it"
        );
    }

    /// A present-but-unparseable value does not fall through to the next field
    /// in the chain. Filing the document under `created` because
    /// `date_of_document` held junk would assert a date the document never
    /// claimed.
    #[test]
    fn a_bad_value_does_not_fall_through_to_the_next_key() {
        let spec = ViewSpec::parse(
            "daily",
            &mapping(&[
                ("group", seq(&["date_of_document", "created"])),
                ("by", Value::String("year".into())),
            ]),
        )
        .expect("a view");

        let mut doc = Mapping::new();
        doc.insert("date_of_document".into(), Value::String("banana".into()));
        doc.insert("created".into(), Value::String("2026-07-24".into()));
        assert!(spec.group.keys_of(&Value::Mapping(doc)).is_empty());
    }

    /// One document, several groups — the property that makes a view different
    /// from the spine.
    #[test]
    fn a_sequence_field_puts_one_document_in_several_groups() {
        let spec = ViewSpec::parse("who", &text(&[("group", "people")])).expect("a view");
        let mut doc = Mapping::new();
        doc.insert("people".into(), seq(&["Ada", "Grace"]));
        assert_eq!(spec.group.keys_of(&Value::Mapping(doc)), ["Ada", "Grace"]);
    }

    #[test]
    fn a_document_with_nothing_in_the_chain_is_ungrouped() {
        let spec = ViewSpec::parse("daily", &text(&[("group", "created")])).expect("a view");
        assert!(
            spec.group
                .keys_of(&Value::Mapping(Mapping::new()))
                .is_empty()
        );
        let mut blank = Mapping::new();
        blank.insert("created".into(), Value::String("   ".into()));
        assert!(spec.group.keys_of(&Value::Mapping(blank)).is_empty());
    }

    #[test]
    fn a_grain_cuts_an_iso_date_and_an_rfc3339_instant_alike() {
        assert_eq!(Grain::Year.cut("2026-07-24"), Some("2026".into()));
        assert_eq!(Grain::Month.cut("2026-07-24"), Some("2026-07".into()));
        assert_eq!(Grain::Day.cut("2026-07-24"), Some("2026-07-24".into()));
        assert_eq!(
            Grain::Month.cut("2026-07-24T07:32:00Z"),
            Some("2026-07".into())
        );
        assert_eq!(Grain::Year.cut("  2026-07-24  "), Some("2026".into()));
    }

    /// The reason the cut validates instead of slicing: `banana` must not
    /// become the group `bana`, and `20264` must not become the year `2026`.
    #[test]
    fn a_grain_rejects_what_is_not_a_date_at_that_grain() {
        assert_eq!(Grain::Year.cut("banana"), None);
        assert_eq!(Grain::Year.cut("20264"), None);
        assert_eq!(Grain::Day.cut("2026-07"), None);
        assert_eq!(Grain::Month.cut("2026/07"), None);
        assert_eq!(Grain::Month.cut(""), None);
    }

    /// The load-bearing separation: `by:` is classification, `nest:` is
    /// aggregation, and reading one does not set the other. A view that grouped
    /// by month would otherwise start filing next month's entry somewhere new.
    #[test]
    fn grain_does_not_imply_nesting() {
        let spec = ViewSpec::parse("daily", &text(&[("group", "created"), ("by", "month")]))
            .expect("a view");
        assert_eq!(spec.group.by, Some(Grain::Month));
        assert_eq!(spec.nest, None);

        let materialized = ViewSpec::parse(
            "daily",
            &text(&[("group", "created"), ("by", "month"), ("nest", "year")]),
        )
        .expect("a view");
        assert_eq!(
            materialized.nest,
            Some(Grain::Year),
            "a view may group finer than it files"
        );
    }

    #[test]
    fn an_entry_without_a_grouping_is_not_a_view() {
        assert!(ViewSpec::parse("x", &text(&[("label", "Nameless")])).is_none());
        assert!(ViewSpec::parse("x", &text(&[("group", "  ")])).is_none());
        assert!(ViewSpec::parse("x", &mapping(&[("group", seq(&[]))])).is_none());
        assert!(ViewSpec::parse("x", &Value::String("created".into())).is_none());
    }

    #[test]
    fn a_view_round_trips_through_its_mapping() {
        for group in [
            Grouping {
                keys: vec!["created".into()],
                by: Some(Grain::Month),
            },
            Grouping {
                keys: vec!["date_of_document".into(), "created".into()],
                by: Some(Grain::Day),
            },
            Grouping::field("people"),
        ] {
            let spec = ViewSpec {
                name: "daily".into(),
                label: Some("Daily".into()),
                icon: Some("calendar".into()),
                group,
                under: Some("[Daily](id:abc1234)".into()),
                filter: Some(Condition::Not(Box::new(Condition::Has("draft".into())))),
                nest: Some(Grain::Year),
            };
            let back =
                ViewSpec::parse("daily", &Value::Mapping(spec.to_mapping())).expect("a view");
            assert_eq!(back, spec);
        }
    }

    /// A one-key chain writes back as a bare string, not a one-element list.
    #[test]
    fn a_single_key_group_serializes_unwrapped() {
        let spec = ViewSpec {
            name: "who".into(),
            label: None,
            icon: None,
            group: Grouping::field("people"),
            under: None,
            filter: None,
            nest: None,
        };
        assert_eq!(
            spec.to_mapping().get("group"),
            Some(&Value::String("people".into()))
        );
    }

    #[test]
    fn views_read_in_declaration_order() {
        let mut views = Mapping::new();
        views.insert("daily".into(), text(&[("group", "created")]));
        views.insert("who".into(), text(&[("group", "people")]));
        let mut config = Mapping::new();
        config.insert(VIEWS_KEY.into(), Value::Mapping(views));

        let specs = views_from(&config);
        assert_eq!(
            specs.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(),
            ["daily", "who"]
        );
    }

    #[test]
    fn a_label_falls_back_to_the_humanized_name() {
        let spec = ViewSpec::parse("daily_entries", &text(&[("group", "created")])).expect("view");
        assert_eq!(spec.display_label(), "Daily entries");
    }

    #[test]
    fn a_non_string_scalar_groups_under_its_text() {
        let spec = ViewSpec::parse("stars", &text(&[("group", "rating")])).expect("a view");
        let mut doc = Mapping::new();
        doc.insert("rating".into(), Value::Int(5));
        assert_eq!(spec.group.keys_of(&Value::Mapping(doc)), ["5"]);
    }
}
