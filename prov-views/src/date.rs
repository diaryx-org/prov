//! What a calendar grain reads a value through: EDTF.
//!
//! An archive is mostly approximate dates. A photograph is "about 1913", a
//! letter is "May 1943" because the day is torn off, a deed is "before the
//! sale in 1921", a birth is "between 1918 and 1922" from two censuses that
//! disagree, and a shoebox of scans is undated. A calendar grain that only
//! reads `YYYY-MM-DD` files every one of those as nothing, which is not what
//! any of them says.
//!
//! [EDTF] — ISO 8601-2, the Library of Congress Extended Date/Time Format —
//! is the standard spelling of exactly these statements, and it is a strict
//! superset of the ISO date a grain already read:
//!
//! | written      | means                                   |
//! | ------------ | --------------------------------------- |
//! | `1943-05`    | May 1943, day unknown                   |
//! | `1913~`      | approximately 1913                      |
//! | `1913?`      | 1913, uncertain                         |
//! | `192X`       | some year in the 1920s                  |
//! | `1918/1922`  | sometime between 1918 and 1922          |
//! | `../1920`    | before 1920                             |
//! | `1918/..`    | after 1918                              |
//! | `XXXX`       | a date, not known                       |
//!
//! Parsing is [`edtf_core`]'s. This module decides what a **grain** makes of
//! the parse, which is the one question a view has to answer and the
//! standard does not.
//!
//! # What a cut keeps
//!
//! A cut keeps the year, or the year and month, or all three, and drops the
//! qualifier: `1913~` at year grain is the group `1913`, beside the `1913`
//! that was sure of itself. A group key describes a bucket, not a value that
//! appears in the data, and the bucket "1913" is where a reader looks for
//! either.
//!
//! An unspecified digit is kept as written when it is in the **year**, so
//! the 1920s file together under `192X` — a decade is a shelf people use.
//! It is refused below the year: `1943-XX` cuts to `1943` at year grain and
//! to nothing at month, because "some month of 1943" is not a month. A year
//! with no specified digit at all (`XXXX`, `XXXX-05`) cuts to nothing at
//! any grain: it is the ungrouped bucket said on purpose, and a frontend
//! already labels that bucket "Undated". (`192X` sorts between `1929` and
//! `1930`, which is where a reader expects the decade.)
//!
//! An **interval** files under every group it reaches, the way a letter about
//! two people appears under both: `1918/1922` at year grain is five groups.
//! An end that is open (`..`) or unknown (empty) contributes nothing, so
//! `../1920` is under `1920` alone — under-claiming, never wrong. Both ends
//! must reach the grain: `1943/1944` at month grain is ungrouped, exactly as
//! a lone `1943` is. There is no cap on the span; a document that says
//! `1800/2000` is under two hundred years because it said so.
//!
//! A **set** (`{1667,1668}`, `[1667,1668]`) is EDTF level 2 and cuts to
//! nothing until someone writes one. A season (`1943-21`) cuts to its year
//! and no further. A `Y`-prefixed year beyond ±9999 has no shelf in a
//! calendar view.
//!
//! # Instants
//!
//! An RFC 3339 instant (`2026-07-24T07:32:00Z`, what a machine-maintained
//! `updated` field carries) is not EDTF beyond its date, and may carry
//! fractional seconds EDTF's time syntax does not. It cuts exactly like the
//! plain date it starts with, as it always has: the time is dropped before
//! the parse.
//!
//! [EDTF]: https://www.loc.gov/standards/datetime/

use edtf_core::{Date, Edtf, Interval, IntervalEndpoint, ParseError, YearKind};

use crate::spec::Grain;

/// Parse a `type: date` value: an EDTF expression, or an RFC 3339 instant
/// read as the date it starts with.
///
/// The error names the byte where the text stopped being a date, which is
/// what a finding reports.
pub fn parse(text: &str) -> Result<Edtf, ParseError> {
    Edtf::parse(date_part(text.trim()))
}

/// Whether `text` is something a `type: date` field accepts.
pub fn is_valid(text: &str) -> bool {
    parse(text).is_ok()
}

/// The date an instant starts with, or `text` itself when it is not one.
///
/// An instant is a full calendar day followed by `T` (or the space RFC 3339
/// permits in its place) and a time: digits, `:`, `.`, and a `Z` or a
/// `±hh:mm` shift. The time is checked only for shape — the date is what a
/// grain cuts, and EDTF's own time grammar (no fractional seconds) is
/// stricter than what real `updated:` fields carry.
fn date_part(text: &str) -> &str {
    let bytes = text.as_bytes();
    if bytes.len() <= 10 || !matches!(bytes[10], b'T' | b' ') {
        return text;
    }
    let day_shaped = bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit);
    let time_shaped = bytes.len() > 11
        && bytes[11..]
            .iter()
            .all(|b| b.is_ascii_digit() || matches!(b, b':' | b'.' | b'+' | b'-' | b'Z'));
    if day_shaped && time_shaped {
        &text[..10]
    } else {
        text
    }
}

/// The group keys a calendar grain files `text` under — see the module docs
/// for what a cut keeps. Empty when the text is not a date, or is a date the
/// grain cannot reach.
pub(crate) fn keys(text: &str, grain: Grain) -> Vec<String> {
    let Ok(edtf) = parse(text) else {
        return Vec::new();
    };
    match edtf {
        Edtf::Date(date) => key_of(&date, grain).into_iter().collect(),
        Edtf::DateTime(instant) => key_of(&instant.date, grain).into_iter().collect(),
        Edtf::Interval(interval) => interval_keys(&interval, grain),
        Edtf::Set(_) => Vec::new(),
    }
}

/// One date's key at `grain`, or `None` when the date does not reach it.
fn key_of(date: &Date, grain: Grain) -> Option<String> {
    let year = year_key(date)?;
    if grain == Grain::Year {
        return Some(year);
    }
    // Below the year every kept component must be fully specified, and the
    // month must be one: EDTF puts its season codes (21–41) in the month
    // slot, and a season is not a month.
    let month = date.month?.value().filter(|m| (1..=12).contains(m))?;
    if grain == Grain::Month {
        return Some(format!("{year}-{month:02}"));
    }
    let day = date.day?.value()?;
    Some(format!("{year}-{month:02}-{day:02}"))
}

/// The year as written — digits and `X`s, with its sign — or `None` when no
/// digit is specified or the year is not a four-digit one.
fn year_key(date: &Date) -> Option<String> {
    let YearKind::Standard { negative, digits } = date.year.kind else {
        return None;
    };
    if digits.iter().all(Option::is_none) {
        return None;
    }
    let mut key = String::with_capacity(5);
    if negative {
        key.push('-');
    }
    for digit in digits {
        key.push(match digit {
            Some(d) => char::from(b'0' + d),
            None => 'X',
        });
    }
    Some(key)
}

/// Every key from the interval's start to its end at `grain`, both
/// inclusive; one end's key when the other is open or unknown; nothing when
/// either dated end fails to reach the grain, or carries an unspecified digit
/// there is no stepping through.
fn interval_keys(interval: &Interval, grain: Grain) -> Vec<String> {
    let dated = |end: &IntervalEndpoint| match end {
        IntervalEndpoint::Date(d)
        | IntervalEndpoint::OnOrBefore(d)
        | IntervalEndpoint::OnOrAfter(d) => Some(*d),
        IntervalEndpoint::Open | IntervalEndpoint::Unknown => None,
    };
    let (start, end) = match (dated(&interval.start), dated(&interval.end)) {
        (Some(start), Some(end)) => (start, end),
        (Some(only), None) | (None, Some(only)) => {
            return key_of(&only, grain).into_iter().collect();
        }
        (None, None) => return Vec::new(),
    };
    if key_of(&start, grain).is_none() || key_of(&end, grain).is_none() {
        return Vec::new();
    }
    let (Some(from), Some(to)) = (calendar(&start), calendar(&end)) else {
        return Vec::new();
    };
    let mut keys = Vec::new();
    let mut at = from;
    while at <= to {
        keys.push(match grain {
            Grain::Year => format_year(at.0),
            Grain::Month => format!("{}-{:02}", format_year(at.0), at.1),
            _ => format!("{}-{:02}-{:02}", format_year(at.0), at.1, at.2),
        });
        at = match grain {
            Grain::Year => (at.0 + 1, 1, 1),
            Grain::Month if at.1 == 12 => (at.0 + 1, 1, 1),
            Grain::Month => (at.0, at.1 + 1, 1),
            _ if at.2 < last_day(at.0, at.1) => (at.0, at.1, at.2 + 1),
            _ if at.1 == 12 => (at.0 + 1, 1, 1),
            _ => (at.0, at.1 + 1, 1),
        };
    }
    keys
}

/// A fully specified date as numbers, with an absent month or day at its
/// first — the position to start stepping from. `None` when any written
/// component has an unspecified digit.
fn calendar(date: &Date) -> Option<(i64, u8, u8)> {
    let year = date.year.value()?;
    let month = match date.month {
        Some(m) => m.value()?,
        None => 1,
    };
    let day = match date.day {
        Some(d) => d.value()?,
        None => 1,
    };
    Some((year, month, day))
}

/// Four digits, signed — the spelling a cut year key has.
fn format_year(year: i64) -> String {
    if year < 0 {
        format!("-{:04}", -year)
    } else {
        format!("{year:04}")
    }
}

/// The last day of `month` in `year`, proleptic Gregorian.
fn last_day(year: i64, month: u8) -> u8 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 31,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(grain: Grain, text: &str) -> Vec<String> {
        keys(text, grain)
    }

    #[test]
    fn a_calendar_date_cuts_as_it_always_did() {
        assert_eq!(at(Grain::Year, "2026-07-24"), ["2026"]);
        assert_eq!(at(Grain::Month, "2026-07-24"), ["2026-07"]);
        assert_eq!(at(Grain::Day, "2026-07-24"), ["2026-07-24"]);
        assert_eq!(at(Grain::Year, "1943"), ["1943"]);
        assert!(
            at(Grain::Month, "1943").is_empty(),
            "a year does not reach month"
        );
    }

    #[test]
    fn an_instant_cuts_like_the_date_it_starts_with() {
        assert_eq!(at(Grain::Month, "2026-07-24T07:32:00Z"), ["2026-07"]);
        assert_eq!(
            at(Grain::Day, "2026-07-24T07:32:00.000000Z"),
            ["2026-07-24"]
        );
        assert_eq!(at(Grain::Day, "2026-07-24T07:32:00+02:00"), ["2026-07-24"]);
        assert_eq!(at(Grain::Day, "2026-07-24 07:32:00"), ["2026-07-24"]);
        assert!(is_valid("2026-07-24T07:32:00.000000Z"));
        assert!(!is_valid("2026-07-24Tbanana"));
    }

    /// The qualifier is dropped: the group is the year, not the confidence.
    #[test]
    fn a_qualified_date_groups_beside_the_sure_one() {
        assert_eq!(at(Grain::Year, "1913~"), ["1913"]);
        assert_eq!(at(Grain::Year, "1913?"), ["1913"]);
        assert_eq!(at(Grain::Year, "1913%"), ["1913"]);
        assert_eq!(at(Grain::Month, "1943-05~"), ["1943-05"]);
        assert!(at(Grain::Month, "1913~").is_empty());
    }

    #[test]
    fn an_unspecified_year_digit_is_a_shelf_and_a_lower_one_is_not() {
        assert_eq!(at(Grain::Year, "192X"), ["192X"]);
        assert_eq!(at(Grain::Year, "19XX"), ["19XX"]);
        assert_eq!(at(Grain::Year, "1943-XX"), ["1943"]);
        assert!(at(Grain::Month, "1943-XX").is_empty());
        assert!(at(Grain::Day, "1943-05-XX").is_empty());
        assert_eq!(at(Grain::Month, "192X-05"), ["192X-05"]);
        // `192X` sits where the decade belongs in a lexical list.
        let mut shelf = ["1930", "192X", "1929"];
        shelf.sort();
        assert_eq!(shelf, ["1929", "192X", "1930"]);
    }

    /// `XXXX` is the ungrouped bucket said on purpose.
    #[test]
    fn a_wholly_unknown_date_is_undated_at_every_grain() {
        for grain in [Grain::Year, Grain::Month, Grain::Day] {
            assert!(at(grain, "XXXX").is_empty());
            assert!(at(grain, "XXXX-05").is_empty());
            assert!(at(grain, "XXXX~").is_empty());
        }
        assert!(is_valid("XXXX"), "it is still a date the type accepts");
    }

    #[test]
    fn an_interval_is_under_every_group_it_reaches() {
        assert_eq!(
            at(Grain::Year, "1918/1922"),
            ["1918", "1919", "1920", "1921", "1922"]
        );
        assert_eq!(
            at(Grain::Month, "1943-11/1944-02"),
            ["1943-11", "1943-12", "1944-01", "1944-02"]
        );
        assert_eq!(
            at(Grain::Day, "1944-02-27/1944-03-02"),
            [
                "1944-02-27",
                "1944-02-28",
                "1944-02-29",
                "1944-03-01",
                "1944-03-02"
            ]
        );
        assert_eq!(at(Grain::Year, "1943-05-12/1943-07-03"), ["1943"]);
        assert_eq!(
            at(Grain::Month, "1943-05-12/1943-07-03"),
            ["1943-05", "1943-06", "1943-07"]
        );
    }

    #[test]
    fn an_interval_reaches_a_grain_only_when_both_ends_do() {
        assert!(at(Grain::Month, "1943/1944").is_empty());
        assert!(at(Grain::Month, "1943-05/1944").is_empty());
        assert!(
            at(Grain::Year, "192X/1935").is_empty(),
            "no stepping through a decade"
        );
    }

    #[test]
    fn an_open_or_unknown_end_contributes_nothing() {
        assert_eq!(at(Grain::Year, "../1920"), ["1920"]);
        assert_eq!(at(Grain::Year, "1918/.."), ["1918"]);
        assert_eq!(at(Grain::Year, "1918/"), ["1918"]);
        assert_eq!(at(Grain::Month, "../1920-03"), ["1920-03"]);
        assert!(at(Grain::Month, "../1920").is_empty());
    }

    #[test]
    fn a_negative_year_keeps_its_sign() {
        assert_eq!(at(Grain::Year, "-0044"), ["-0044"]);
        assert_eq!(at(Grain::Year, "-0044/-0042"), ["-0044", "-0043", "-0042"]);
    }

    #[test]
    fn what_is_not_a_date_cuts_to_nothing() {
        for text in [
            "banana",
            "20264",
            "2026/07",
            "",
            "May 1943",
            "2026-13-01",
            "1943-5",
            "unknown",
        ] {
            assert!(at(Grain::Year, text).is_empty(), "{text:?}");
            assert!(!is_valid(text), "{text:?}");
        }
        assert!(at(Grain::Day, "2026-07").is_empty());
    }

    #[test]
    fn a_season_and_a_set_stop_where_they_do() {
        assert_eq!(at(Grain::Year, "1943-21"), ["1943"]);
        assert!(at(Grain::Month, "1943-21").is_empty());
        assert!(at(Grain::Year, "{1667,1668}").is_empty());
        assert!(is_valid("{1667,1668}"));
    }
}
