//! The workspace's clock, and the two calendar renderings it is read through.
//!
//! [`now_rfc3339`] is the one source of every timestamp prov writes — an
//! `updated` stamp, a `created` field, a confirmation's instant, a deletion
//! record — so the decision about precision and format is made here once.
//! [`civil_from_days`] is the calendar arithmetic under it, exposed so the
//! ZIP writer's DOS timestamp ([`dos_datetime`]) comes from the same
//! arithmetic rather than a second copy.
//!
//! Hand-rolled from the system clock rather than pulling in a date crate, in
//! the spirit of the dependency-free SHA-256 and journal checksum.

/// The current time as an RFC 3339 UTC timestamp with microsecond precision
/// (`2026-07-16T14:30:00.123456Z`) — the machine-standard value prov stores for
/// provenance fields like `updated` and a history event's `created` (DESIGN §2:
/// prov-maintained ⟹ prov owns the format, and human-friendly rendering is a
/// viewer's job).
///
/// **This is the workspace's only clock.** Every timestamp prov writes comes from
/// here, which is what keeps one decision about precision from having to be
/// remembered at each site.
///
/// Hand-rolled from the system clock rather than pulling in a date crate, in the
/// spirit of the dependency-free SHA-256 and journal checksum. A pre-epoch clock
/// (only a badly-wrong system) formats as the epoch.
pub(crate) fn now_rfc3339() -> String {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    rfc3339(since.as_secs(), since.subsec_micros())
}

/// Format an instant since the Unix epoch as an RFC 3339 UTC timestamp. Split out
/// from [`now_rfc3339`] so the calendar arithmetic is testable without a clock.
///
/// The fraction is **always six digits, never trimmed**. Sub-second precision
/// exists so that two events in the same second can be *ordered*, and a variable
/// number of digits would defeat exactly that: `…10.1Z` against `…10.12Z`
/// compares `Z` (0x5A) with `2` (0x32) at the second fraction digit, so the
/// shorter one sorts later. Fixed width keeps a plain string comparison a correct
/// total order. (Timestamps written before this precision existed carry no
/// fraction at all; readers normalize — see `prov::history`.)
fn rfc3339(secs: u64, micros: u32) -> String {
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let (hour, min, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}.{micros:06}Z")
}

/// Days since 1970-01-01 → (year, month, day), by Howard Hinnant's civil-calendar
/// algorithm — exact for the whole proleptic Gregorian range, no leap-year
/// special-casing beyond the era arithmetic.
pub(crate) fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64; // day-of-era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day-of-year [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if month <= 2 { y + 1 } else { y }, month, day)
}

/// Seconds-since-Unix-epoch as ZIP's native (time, date) pair, each a packed
/// bitfield (PKWARE APPNOTE.TXT §4.4.6). Clamped at the 1980-01-01 floor DOS
/// timestamps cannot represent below. Here rather than beside the ZIP writer
/// because it is the same calendar arithmetic as [`rfc3339`], read out into a
/// different field layout.
pub(crate) fn dos_datetime(secs: u64) -> (u16, u16) {
    const DOS_FLOOR_SECS: u64 = 315_532_800; // 1980-01-01T00:00:00Z
    let secs = secs.max(DOS_FLOOR_SECS);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hour, min, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (year, month, day) = civil_from_days(days);
    let dos_year = (year - 1980).clamp(0, i64::from(u16::MAX >> 9)) as u16;
    let dos_date = (dos_year << 9) | ((month as u16) << 5) | (day as u16);
    let dos_time = ((hour as u16) << 11) | ((min as u16) << 5) | ((sec as u16) / 2);
    (dos_time, dos_date)
}

#[cfg(test)]
mod tests {
    use super::{dos_datetime, now_rfc3339, rfc3339};

    #[test]
    fn rfc3339_matches_known_instants() {
        // Cross-checked against `date -u -r <secs>` / any RFC 3339 reference.
        assert_eq!(rfc3339(0, 0), "1970-01-01T00:00:00.000000Z");
        assert_eq!(rfc3339(1_700_000_000, 0), "2023-11-14T22:13:20.000000Z");
        // A leap day, to exercise the calendar arithmetic.
        assert_eq!(rfc3339(1_582_934_400, 0), "2020-02-29T00:00:00.000000Z");
        // End-of-year boundary.
        assert_eq!(rfc3339(1_609_459_199, 0), "2020-12-31T23:59:59.000000Z");
    }

    #[test]
    fn the_fraction_is_six_digits_whatever_the_value() {
        // Fixed width is the whole point: sub-second precision exists so two
        // instants in the same second can be *ordered*, and a trimmed fraction
        // breaks that. A leading-zero microsecond count must not lose its zeros,
        // and a whole-number one must not lose its trailing ones.
        assert_eq!(rfc3339(0, 1), "1970-01-01T00:00:00.000001Z");
        assert_eq!(rfc3339(0, 100_000), "1970-01-01T00:00:00.100000Z");
        assert_eq!(rfc3339(0, 999_999), "1970-01-01T00:00:00.999999Z");

        // …and with that, a plain string comparison is a correct total order.
        let mut stamps = [rfc3339(0, 120_000), rfc3339(0, 100_000), rfc3339(0, 99_999)];
        stamps.sort();
        assert_eq!(
            stamps,
            [rfc3339(0, 99_999), rfc3339(0, 100_000), rfc3339(0, 120_000)]
        );
    }

    #[test]
    fn the_clock_is_microsecond_precise_and_never_goes_backwards() {
        // The one assertion about the real clock: the format it produces is the
        // one the store's ordering rests on.
        let now = now_rfc3339();
        assert_eq!(now.len(), "2026-07-16T14:30:00.123456Z".len(), "{now}");
        assert!(now.ends_with('Z') && now.as_bytes()[19] == b'.', "{now}");
        assert!(now_rfc3339() >= now, "the clock must not run backwards");
    }

    #[test]
    fn dos_datetime_floors_at_1980() {
        assert_eq!(dos_datetime(0), (0, 0x0021));
    }

    #[test]
    fn dos_datetime_matches_a_known_instant() {
        // 2020-02-29T13:07:36Z (a leap day, to exercise the calendar path).
        let (time, date) = dos_datetime(1_582_981_656);
        let year = 1980 + (date >> 9);
        let month = (date >> 5) & 0x0F;
        let day = date & 0x1F;
        assert_eq!((year, month, day), (2020, 2, 29));
        let hour = time >> 11;
        let min = (time >> 5) & 0x3F;
        let sec2 = time & 0x1F; // seconds / 2
        assert_eq!((hour, min, sec2 * 2), (13, 7, 36));
    }
}
