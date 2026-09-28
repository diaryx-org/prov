//! Grains: how finely a value is cut into groups.
//!
//! A grain is what a view's `year(…)`, `month(…)`, `day(…)` and `initial(…)`
//! functions apply (see [`crate::expr`]), and what a filing entry's `nest:`
//! names (see [`crate::filing`]). The two uses need different things from it,
//! and the type says which: reading needs only [`Grain::cuts`], filing also
//! needs [`Grain::chain`].

use prov_graph::meta::{Mapping, Value};

/// A **coarsening**: how finely a value is cut into groups.
///
/// Not a date vocabulary. A grain is any many-to-one function from a value to
/// group keys, and the calendar grains are one family of them — `year` is
/// "the year this date names", and [`Initial`](Self::Initial) is "the first
/// *n* characters" with no such condition. What makes something a grain is
/// the two properties below, not what it is about.
///
/// # Two properties, and what each one licenses
///
/// - [`cuts`](Self::cuts) — value → keys. This is all a view's `key:` needs
///   (through the `year`/`month`/`day`/`initial` functions, see
///   [`crate::expr`]), because grouping is a *reading* operation with no
///   invariant to keep. Usually one key; an interval (`1918/1922`) is under every year it
///   spans, which is what makes it *keys*.
/// - [`chain`](Self::chain) — the coarser grains this one refines, coarsest
///   first. This is what [`nest`](crate::FilingSpec::nest) needs, and it is a strictly
///   stronger requirement: nesting builds a hierarchy of index documents, so
///   each level's key must be determined by the finer level's
///   (`2026-07-24` → `2026-07` → `2026`, `Ada` → `Ad` → `A`). A coarsening
///   with no such chain can group but cannot nest.
///
/// The second constraint is prov's, not taste. `nest` files a record into the
/// **spanning relation**, which is single-parent, so a nest chain must also be
/// *single-valued* per document — see [`FilingSpec::route`](crate::FilingSpec::route), which returns
/// `None` rather than guessing which of a multi-valued field's values a
/// document should be filed under.
///
/// # Adding a grain
///
/// The rule is the one [`crate::expr`] uses for prov's functions: a **concrete lens
/// that cannot otherwise be said**, not a shape that seems likely to be wanted.
/// `initial` earns its place as the A–Z index every list of names and places
/// eventually wants. A numeric `bucket` (ratings by tens) is the obvious next
/// one and is deliberately *not* here: nobody has asked for it, and it would
/// arrive with a problem the calendar grains do not have — its keys sort
/// lexically as `0, 10, 100, 20`, so it needs group ordering to become
/// grain-aware, which is really the deferred `sort:` axis wearing a disguise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Grain {
    /// `2026` — the default, and what a lifetime of entries wants.
    #[default]
    Year,
    /// `2026-07`.
    Month,
    /// `2026-07-25`.
    Day,
    /// The first *n* characters, upper-cased — the A–Z index.
    ///
    /// Upper-casing is a deliberate normalization rather than a faithful cut:
    /// an alphabetical index that files `ada` apart from `Ada` is not an index.
    /// It is the same kind of choice a date cut makes when it reports `2026`
    /// for a value that says `2026-07-24`; a group key describes a bucket, not
    /// a value that appears in the data.
    Initial(usize),
}

/// The grain spellings that are a bare word — what a near-miss diagnostic
/// offers. [`Grain::Initial`] also takes a parameterized form
/// (`{ initial: 2 }`) that is not a spelling to suggest.
pub const GRAINS: &[&str] = &["year", "month", "day", "initial"];

impl Grain {
    /// The config spelling, when this grain has a bare-word one.
    ///
    /// `None` for a parameterized grain that is not at its default — write
    /// [`to_value`](Self::to_value) instead, which always round-trips.
    pub fn as_config_str(self) -> Option<&'static str> {
        Some(match self {
            Grain::Year => "year",
            Grain::Month => "month",
            Grain::Day => "day",
            Grain::Initial(1) => "initial",
            Grain::Initial(_) => return None,
        })
    }

    /// Parse a bare-word config spelling. Unknown text is **not** silently
    /// defaulted — a `nest: yearr` that quietly filed by year would look
    /// applied and be wrong, which is the failure a config linter exists to
    /// prevent.
    pub fn from_config_str(text: &str) -> Option<Self> {
        match text.trim() {
            "year" => Some(Grain::Year),
            "month" => Some(Grain::Month),
            "day" => Some(Grain::Day),
            // The bare word is the useful case; `{ initial: n }` says the rest.
            "initial" => Some(Grain::Initial(1)),
            _ => None,
        }
    }

    /// Read a `nest:` value: a bare word, or a one-key mapping naming a
    /// parameterized grain (`{ initial: 2 }`).
    ///
    /// A parameter of zero is rejected rather than clamped: `{ initial: 0 }`
    /// would put every document in one group called "", which is a view that
    /// has stopped being one.
    pub fn parse(value: &Value) -> Option<Self> {
        match value {
            Value::String(text) => Grain::from_config_str(text),
            Value::Mapping(map) => match map.iter().next() {
                Some((key, arg)) if map.len() == 1 && key == "initial" => {
                    let n = match arg {
                        Value::Int(n) => *n,
                        Value::String(s) => s.trim().parse().ok()?,
                        _ => return None,
                    };
                    (n > 0).then_some(Grain::Initial(n as usize))
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// The value this grain writes back as — a bare word where it has one, a
    /// one-key mapping otherwise.
    pub fn to_value(self) -> Value {
        match self.as_config_str() {
            Some(word) => Value::String(word.into()),
            None => {
                let Grain::Initial(n) = self else {
                    unreachable!("every non-parameterized grain has a bare spelling")
                };
                let mut map = Mapping::new();
                map.insert("initial".into(), Value::Int(n as i64));
                Value::Mapping(map)
            }
        }
    }

    /// How this grain reads in a listing (`month`, `initial 2`).
    pub fn display(self) -> String {
        match self {
            Grain::Initial(n) if n > 1 => format!("initial {n}"),
            other => other.as_config_str().unwrap_or("initial").to_string(),
        }
    }

    /// The grains to nest through to reach `self`, coarsest first.
    ///
    /// Filing at month grain means a year index and then a month index inside
    /// it: a month index that is not inside its year is not where anyone looks
    /// for it. The alphabetical case is the same shape — filing at `initial 2`
    /// means an `A` index holding an `Ad` index.
    ///
    /// Each step must be *determined* by the one after it, which is what makes
    /// the hierarchy well defined. That is why this is a property of the grain
    /// rather than something a caller can assemble: an arbitrary sequence of
    /// coarsenings is not a nest.
    pub fn chain(self) -> Vec<Grain> {
        match self {
            Grain::Year => vec![Grain::Year],
            Grain::Month => vec![Grain::Year, Grain::Month],
            Grain::Day => vec![Grain::Year, Grain::Month, Grain::Day],
            Grain::Initial(n) => (1..=n).map(Grain::Initial).collect(),
        }
    }

    /// Every group key `value` falls under at this grain — empty when the
    /// value does not reach it.
    ///
    /// The calendar grains *validate* rather than taking a blind prefix, which
    /// is what keeps `by:` usable on a view whose field is only usually a date:
    /// `banana` cut to a year would otherwise group under `bana`, a group key
    /// that looks like data. A value this rejects falls to the ungrouped
    /// bucket, where it is visible as something that did not sort.
    ///
    /// What they validate *as* is EDTF, so `1913~` is the group `1913`, `192X`
    /// is a group of its own, `1918/1922` is five groups, and `XXXX` is none
    /// — the rules are in [`crate::date`]. An RFC 3339 instant
    /// (`2026-07-24T07:32:00Z` — what a machine-maintained `updated` field
    /// carries) cuts exactly like the plain date it starts with.
    ///
    /// The group keys are spelled so that an ISO date's lexical order is its
    /// calendar order, so the group order falls out of the string with no
    /// calendar arithmetic and no time zone to get wrong. (Years before 0000
    /// sort backwards among themselves; nothing files there yet.)
    pub fn cuts(self, value: &str) -> Vec<String> {
        let text = value.trim();
        if let Grain::Initial(n) = self {
            // By *character*, not byte: a name may begin with any of them, and
            // slicing `Ålesund` at byte 1 is a panic. A value shorter than the
            // cut is taken whole rather than rejected — `Bo` under a two-letter
            // index belongs at `BO`, and there is no coarser truth to wait for.
            let cut: String = text.chars().take(n).flat_map(char::to_uppercase).collect();
            return if cut.is_empty() {
                Vec::new()
            } else {
                vec![cut]
            };
        }
        crate::date::keys(text, self)
    }

    /// The one group key `value` falls under at this grain, or `None` when it
    /// falls under none — or under several.
    ///
    /// The single-valued half of [`cuts`](Self::cuts), for the caller that
    /// needs one answer: filing. An interval has several homes at a grain it
    /// spans, and [`FilingSpec::route`](crate::FilingSpec::route) must not pick one, for the reason
    /// it does not pick between two people.
    pub fn cut(self, value: &str) -> Option<String> {
        let mut keys = self.cuts(value);
        (keys.len() == 1).then(|| keys.remove(0))
    }
}
