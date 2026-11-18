//! # prov-fhir
//!
//! Import a FHIR export into a prov workspace kept with the `medical` preset:
//! one patient's record, as Markdown documents a person can read with `cat`,
//! each saying where it came from.
//!
//! - [`fhir`] reads an export (Bulk Data ndjson, or a Bundle) and cuts one
//!   patient's share out of it.
//! - [`map`] decides what that share becomes: a pure mapping from resources
//!   to planned documents, with no workspace in sight.
//! - [`import`] writes the plan through the prov library, idempotently.
//! - [`vocab`] reads the closed vocabularies the import must stay inside.
//! - [`preset`] is the medical preset, built in, and a fresh workspace made
//!   with it.

pub mod fhir;
pub mod import;
pub mod map;
pub mod preset;
pub mod vocab;

/// Anything that goes wrong is a message for the person running the import.
pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// The current time as RFC 3339 UTC with microseconds
/// (`2026-11-18T23:20:58.123456Z`) — the spelling prov writes every instant
/// in. Hand-rolled from the system clock, as prov's own is.
pub fn now_rfc3339() -> String {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    rfc3339(since.as_secs(), since.subsec_micros())
}

fn rfc3339(secs: u64, micros: u32) -> String {
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let (hour, min, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Days since 1970-01-01 → civil date, by Howard Hinnant's algorithm.
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}.{micros:06}Z")
}

#[cfg(test)]
mod tests {
    use super::rfc3339;

    #[test]
    fn instants_are_spelled_as_prov_spells_them() {
        assert_eq!(rfc3339(0, 0), "1970-01-01T00:00:00.000000Z");
        assert_eq!(rfc3339(1_582_934_400, 7), "2020-02-29T00:00:00.000007Z");
        assert_eq!(rfc3339(1_609_459_199, 0), "2020-12-31T23:59:59.000000Z");
    }
}
