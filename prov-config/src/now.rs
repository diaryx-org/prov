//! A reading of the caller's clock, and what a stamped field is written from.
//!
//! prov has no clock (DESIGN §2): the program driving it reads the time and
//! hands it over, and prov writes it. What it handed over used to be one RFC
//! 3339 instant, written verbatim into whichever field is stamped. That is the
//! right value for a field prov reads back — the last-edit instant a
//! confirmation is judged against — and the wrong one for a field a person
//! reads and may correct: a journal's `created: 2026-09-28` is a calendar date
//! on the writer's wall, and the UTC instant of the same moment can be a
//! different day.
//!
//! A date is a fact about a wall clock, not about an instant, so the instant
//! alone cannot say it. [`Now`] is the instant together with the offset of the
//! clock it was read from, which is what a caller has and prov does not; and a
//! stamped field declared `type: date` is written as the date on that clock
//! ([`WorkspaceConfig::stamp_value`](crate::WorkspaceConfig::stamp_value)).
//! Every other stamped field is written the instant, as it always was.

use fig::ExtKind;
use fig_schema::FieldType;

use crate::config::FieldSpec;

/// The time a stamp is written from: an instant, and the UTC offset of the
/// wall clock it was read on.
///
/// The instant is RFC 3339 and is written as given, so a caller keeps the
/// spelling it has always written (prov's own CLI: UTC, six fractional digits,
/// fixed width). The offset is minutes east of UTC — `-360` for Mountain
/// Daylight Time — and is what a `type: date` stamp's day is read on. A caller
/// that cannot know its offset says [`utc`](Self::utc), and a date stamp is
/// then the UTC date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Now {
    instant: String,
    offset_minutes: i32,
}

/// The widest offset a wall clock keeps: RFC 3339's `±23:59`.
const MAX_OFFSET: i32 = 23 * 60 + 59;

impl Now {
    /// `instant`, read on a clock `offset_minutes` east of UTC. An offset
    /// beyond `±23:59` is clamped to it.
    pub fn new(instant: impl Into<String>, offset_minutes: i32) -> Self {
        Self {
            instant: instant.into(),
            offset_minutes: offset_minutes.clamp(-MAX_OFFSET, MAX_OFFSET),
        }
    }

    /// `instant`, on a clock that keeps UTC.
    pub fn utc(instant: impl Into<String>) -> Self {
        Self::new(instant, 0)
    }

    /// The instant, as the caller spelled it.
    pub fn instant(&self) -> &str {
        &self.instant
    }

    /// Minutes east of UTC of the clock the instant was read on.
    pub fn offset_minutes(&self) -> i32 {
        self.offset_minutes
    }

    /// The calendar date (`2026-09-28`) at this instant on this clock, or
    /// `None` when the instant is not RFC 3339.
    pub fn local_date(&self) -> Option<String> {
        let (seconds, _) = instant_seconds(&self.instant)?;
        let local = seconds + i64::from(self.offset_minutes) * 60;
        let (year, month, day) = civil_from_days(local.div_euclid(86_400));
        Some(format!("{year:04}-{month:02}-{day:02}"))
    }

    /// The value a stamped field declared by `spec` is written: the local
    /// date for a `type: date` field, the instant for any other. An instant
    /// that does not parse is written as given either way — prov writes what
    /// it is handed rather than inventing a day.
    pub fn stamp_for(&self, spec: Option<&FieldSpec>) -> String {
        let dated = spec.is_some_and(|s| s.ty == Some(FieldType::Extended(ExtKind::LocalDate)));
        match dated.then(|| self.local_date()).flatten() {
            Some(date) => date,
            None => self.instant.clone(),
        }
    }
}

/// An RFC 3339 date-time as (seconds since the Unix epoch, nanoseconds), its
/// offset applied — `2026-09-28T14:03:00Z`, `2026-09-28T16:03:00.5+02:00`.
/// `None` for anything else, a bare date included.
pub fn instant_seconds(text: &str) -> Option<(i64, u32)> {
    let text = text.trim();
    let b = text.as_bytes();
    let digits = |from: usize, len: usize| -> Option<i64> {
        let part = text.get(from..from + len)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    if b.len() < 20 || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    if b[13] != b':' || b[16] != b':' {
        return None;
    }
    let days = days_of(text)?;
    let (hour, minute, second) = (digits(11, 2)?, digits(14, 2)?, digits(17, 2)?);
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let mut at = 19;
    let mut nanos: u32 = 0;
    if b.get(at) == Some(&b'.') {
        at += 1;
        let start = at;
        while b.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        let frac = &text[start..at];
        if frac.is_empty() {
            return None;
        }
        let padded: String = frac.chars().chain(std::iter::repeat('0')).take(9).collect();
        nanos = padded.parse().ok()?;
    }
    let offset = match b.get(at)? {
        b'Z' | b'z' if at + 1 == b.len() => 0,
        sign @ (b'+' | b'-') if at + 6 == b.len() && b[at + 3] == b':' => {
            let minutes = digits(at + 1, 2)? * 60 + digits(at + 4, 2)?;
            if *sign == b'+' { minutes } else { -minutes }
        }
        _ => return None,
    };
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second - offset * 60;
    Some((seconds, nanos))
}

/// The date an RFC 3339 date-time or a bare calendar date (`2026-09-28`)
/// starts with, as days since 1970-01-01 — the date *as written*, in the
/// writer's own offset. `None` for anything else.
pub fn calendar_days(text: &str) -> Option<i64> {
    let text = text.trim();
    match text.len() {
        10 => days_of(text),
        _ => {
            instant_seconds(text)?;
            days_of(text)
        }
    }
}

/// Days since 1970-01-01 of the `YYYY-MM-DD` that opens `text`.
fn days_of(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let digits = |from: usize, len: usize| -> Option<i64> {
        let part = text.get(from..from + len)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let (year, month, day) = (digits(0, 4)?, digits(5, 2)?, digits(8, 2)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Days from the civil calendar to 1970-01-01 (Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// Days since 1970-01-01 → (year, month, day), the inverse of [`days_of`].
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { y + 1 } else { y }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dated() -> FieldSpec {
        FieldSpec {
            ty: Some(FieldType::Extended(ExtKind::LocalDate)),
            values: Default::default(),
            vocabulary: None,
            default: None,
            under: None,
            stamp: None,
        }
    }

    #[test]
    fn a_date_is_the_day_on_the_callers_wall() {
        // 03:30 UTC on the 29th is still the evening of the 28th in Denver.
        let now = Now::new("2026-09-29T03:30:00.000000Z", -360);
        assert_eq!(now.local_date().as_deref(), Some("2026-09-28"));
        assert_eq!(now.stamp_for(Some(&dated())), "2026-09-28");
        // …and the 29th on a UTC clock, or east of it.
        assert_eq!(
            Now::utc("2026-09-29T03:30:00Z").local_date().as_deref(),
            Some("2026-09-29")
        );
        assert_eq!(
            Now::new("2026-12-31T20:00:00Z", 9 * 60)
                .local_date()
                .as_deref(),
            Some("2027-01-01")
        );
    }

    #[test]
    fn an_undeclared_or_instant_stamp_is_the_instant_as_given() {
        let now = Now::new("2026-09-29T03:30:00.000000Z", -360);
        assert_eq!(now.stamp_for(None), "2026-09-29T03:30:00.000000Z");
        let mut instant = dated();
        instant.ty = Some(FieldType::Extended(ExtKind::OffsetDateTime));
        assert_eq!(now.stamp_for(Some(&instant)), now.instant());
        // Not RFC 3339: written as handed over, not turned into a guess.
        assert_eq!(Now::utc("yesterday").stamp_for(Some(&dated())), "yesterday");
    }

    #[test]
    fn instants_and_dates_read_as_days() {
        assert_eq!(calendar_days("1970-01-02"), Some(1));
        assert_eq!(calendar_days("1970-01-02T23:00:00-05:00"), Some(1));
        assert_eq!(calendar_days("1970-01-02T"), None);
        assert_eq!(instant_seconds("1970-01-02"), None);
        assert_eq!(
            instant_seconds("1970-01-01T01:00:00+01:00"),
            Some((0, 0)),
            "the offset is applied"
        );
        for days in [-1, 0, 20_000, 2_000_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_of(&format!("{y:04}-{m:02}-{d:02}")), Some(days));
        }
    }
}
