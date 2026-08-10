use prov_graph::error::{Error, Result};
use prov_graph::link;

use super::model::*;
use super::paths::*;

/// The bytes an event's id digest is taken over — its **canonical form**.
///
/// Deliberately independent of the metadata serialization format, so the same
/// workspace state yields the same id whether frontmatter is YAML, JSON or fig.
/// Tab-separated fields, one per line; see `docs/history-format.md` §4.1.
///
/// Hashes `files` in the order given — it trusts the caller for that, rather
/// than sorting here, so this stays a pure function of "the manifest, in the
/// order it will be serialized" for a reader reconstructing an id from an
/// event already on disk. The one caller that *mints* an id ([`mint_id`], from
/// [`Workspace::history_capture`](crate::Workspace::history_capture)) is what owes it §3.1 order — see
/// [`path_sort_key`].
pub(super) fn canonical_bytes(
    created: &str,
    trigger: &str,
    label: Option<&str>,
    parent: Option<&str>,
    files: &[FileEntry],
) -> Vec<u8> {
    let mut out = String::new();
    out.push_str(&format!("created\t{created}\n"));
    out.push_str(&format!("trigger\t{trigger}\n"));
    if let Some(label) = label {
        out.push_str(&format!("label\t{label}\n"));
    }
    if let Some(parent) = parent {
        out.push_str(&format!("parent\t{parent}\n"));
    }
    for file in files {
        let id = file.id.as_ref().map(|i| i.0.as_str()).unwrap_or("");
        out.push_str(&format!(
            "file\t{}\t{id}\t{}\n",
            slash_path(&file.path),
            file.hash
        ));
    }
    out.into_bytes()
}

/// Mint the event id: `<YYYY>-<MM>-<DD>-<HHMM>[-<label-slug>]-<8 hex>`.
///
/// The suffix is content-derived rather than random — prov has a dependency-free
/// SHA-256 and no RNG, and the library stays clockless and deterministic, taking
/// its timestamp as an argument exactly as `recycle` does. It also makes
/// collisions *benign*: two devices producing byte-identical events yield the
/// same filename holding the same content, which is convergence rather than
/// conflict.
pub(super) fn mint_id(
    created: &str,
    trigger: &str,
    label: Option<&str>,
    parent: Option<&str>,
    files: &[FileEntry],
) -> Result<String> {
    let stamp = id_stamp(created)?;
    let digest = crate::fixity::digest(&canonical_bytes(created, trigger, label, parent, files));
    let short = &digest["sha256:".len().."sha256:".len() + 8];
    Ok(match label.map(link::slug) {
        Some(slug) => format!("{stamp}-{slug}-{short}"),
        None => format!("{stamp}-{short}"),
    })
}

/// How many fractional digits a `created` written by this version carries. Fixed,
/// never trimmed — see [`comparable`].
pub(super) const FRACTION_DIGITS: usize = 6;

/// A `created` value in the form two events can be **ordered** by, whatever
/// precision each was written at.
///
/// A store outlives any one version of prov, and event documents are immutable,
/// so a store holds second-granularity timestamps (everything written before
/// microsecond precision existed) alongside sub-second ones — permanently, and
/// interleaved by sync rather than neatly separated by date. Comparing those as
/// raw strings is wrong in precisely the case that matters: `Z` (0x5A) sorts
/// after `.` (0x2E), so `…10Z` would order *after* `…10.500000Z` inside the same
/// second, inverting the two events a finer clock was introduced to tell apart.
///
/// Padding the fraction to a fixed width restores the total order without parsing
/// a calendar and without rewriting a single event — which matters, because
/// rewriting one is the operation this format does not have.
///
/// A stamp not in `…Z` form is returned untouched. prov only ever writes `Z`, and
/// an offset form is already outside the order a string comparison can give;
/// mangling it here would only hide that.
pub(super) fn comparable(created: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    let Some(rest) = created.strip_suffix('Z') else {
        return Cow::Borrowed(created);
    };
    let (whole, fraction) = match rest.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (rest, ""),
    };
    if fraction.len() == FRACTION_DIGITS {
        return Cow::Borrowed(created);
    }
    let mut padded = fraction.to_string();
    padded.truncate(FRACTION_DIGITS);
    while padded.len() < FRACTION_DIGITS {
        padded.push('0');
    }
    Cow::Owned(format!("{whole}.{padded}Z"))
}

/// Reject a [`Retention::Before`] cutoff that is not a date, so a typo deletes
/// nothing rather than everything.
///
/// Only the `YYYY-MM-DD` head is checked. Anything after it is compared as text
/// against a normalized `created` ([`comparable`]), where a bare date is a prefix
/// of every timestamp in its day — which is what makes "before 2026-06-01" mean
/// "before that day started" without parsing a calendar.
pub(super) fn check_cutoff(cutoff: &str) -> Result<()> {
    let ok = cutoff.len() >= 10
        && cutoff.as_bytes()[4] == b'-'
        && cutoff.as_bytes()[7] == b'-'
        && cutoff
            .bytes()
            .take(10)
            .enumerate()
            .all(|(i, b)| matches!(i, 4 | 7) || b.is_ascii_digit());
    match ok {
        true => Ok(()),
        false => Err(Error::Structure(format!(
            "`{cutoff}` is not a date — expected YYYY-MM-DD, or a full RFC 3339 timestamp"
        ))),
    }
}

/// `YYYY-MM-DD-HHMM` from an RFC 3339 UTC timestamp — the human-readable head of
/// an event id. Full precision stays in the document's `created`.
///
/// Reads only the calendar head, so a fractional-second suffix passes through
/// untouched: event ids stay minute-granular (§4 — they are for humans), and the
/// eight-hex content digest is what tells two captures in one minute apart.
pub(super) fn id_stamp(created: &str) -> Result<String> {
    let bad = || Error::Structure(format!("`{created}` is not an RFC 3339 UTC timestamp"));
    let bytes = created.as_bytes();
    if bytes.len() < 16 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[13] != b':' {
        return Err(bad());
    }
    let digits = |range: std::ops::Range<usize>| {
        created
            .get(range)
            .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))
            .ok_or_else(bad)
    };
    Ok(format!(
        "{}-{}-{}-{}{}",
        digits(0..4)?,
        digits(5..7)?,
        digits(8..10)?,
        digits(11..13)?,
        digits(14..16)?
    ))
}

/// `2026-07-31 09:15` read back out of an event id — the display form of the
/// stamp `id_stamp` encoded.
pub(super) fn display_stamp(id: &str) -> String {
    let parts: Vec<&str> = id.splitn(5, '-').collect();
    match parts.as_slice() {
        [y, m, d, hm, ..] if hm.len() == 4 => format!("{y}-{m}-{d} {}:{}", &hm[..2], &hm[2..]),
        _ => id.to_string(),
    }
}

/// The label slug an id carries, if any — everything between the time and the
/// digest suffix. Lets an index document label its entries without opening every
/// event, which matters because two captures in the same minute would otherwise
/// read identically.
pub(super) fn label_slug(id: &str) -> Option<String> {
    let parts: Vec<&str> = id.split('-').collect();
    let [_, _, _, _, rest @ ..] = parts.as_slice() else {
        return None;
    };
    // The last segment is the digest; anything before it is the slug.
    match rest.len() {
        0 | 1 => None,
        n => Some(rest[..n - 1].join("-")),
    }
}

/// How an index document names one event: its timestamp, plus the label slug
/// when it has one.
pub(super) fn display_entry(id: &str) -> String {
    match label_slug(id) {
        Some(slug) => format!("{} ({slug})", display_stamp(id)),
        None => display_stamp(id),
    }
}

#[cfg(test)]
mod tests {
    use super::super::TRIGGER_MANUAL;
    use super::super::layout::shard_of;
    use super::super::support::entry;
    use super::*;

    /// The canonical form (§4.1) is a **published contract**: a third party can
    /// mint a conforming event id, which is a stronger property than being able
    /// to read one, and it is the reason two devices converge on a filename
    /// rather than conflicting.
    ///
    /// Every other test here goes through [`canonical_bytes`], so all of them
    /// would keep passing if the spec and the implementation drifted apart. This
    /// one spells the bytes out **by hand**, exactly as §4.1 words them —
    /// tab-separated fields, `\n`-terminated lines, the empty `id` field for an
    /// unregistered file, the `label` line carrying raw text and not the slug —
    /// and hashes that. It is the only test in the crate that fails when the
    /// document changes and the code does not, or the reverse.
    ///
    /// If it fails, do not "fix" it by regenerating the expected value: either
    /// the code has drifted from the format documents, or the format has changed
    /// and every event ever written under the old rule now has an id nothing can
    /// re-derive.
    #[test]
    fn the_canonical_form_matches_the_spec_spelled_out_by_hand() {
        let files = vec![
            FileEntry {
                path: "notes/foo.md".into(),
                id: Some(prov_graph::identity::Id("b7k2m".into())),
                hash: "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
                    .into(),
            },
            FileEntry {
                path: "notes/photo.jpg".into(),
                id: None,
                hash: "sha256:2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae"
                    .into(),
            },
        ];

        // §4.1, transcribed. Note the `\t\t` on the second file line: an
        // unregistered file's id is the *empty string*, so the line keeps the
        // same four-field shape either way.
        let by_hand = concat!(
            "created\t2026-07-31T09:15:22.481903Z\n",
            "trigger\tmanual\n",
            "label\tpre-sync\n",
            "parent\t2026-07-30-1804-nightly-8c1d55aa\n",
            "file\tnotes/foo.md\tb7k2m\tsha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08\n",
            "file\tnotes/photo.jpg\t\tsha256:2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae\n",
        );

        assert_eq!(
            String::from_utf8(canonical_bytes(
                "2026-07-31T09:15:22.481903Z",
                TRIGGER_MANUAL,
                Some("pre-sync"),
                Some("2026-07-30-1804-nightly-8c1d55aa"),
                &files,
            ))
            .unwrap(),
            by_hand,
            "the canonical form drifted from docs/history-format.md §4.1"
        );

        // And the id those bytes produce, pinned. `shasum -a 256` on the same
        // bytes prints the same digest — the check a third party can run without
        // this crate:
        //
        //   printf 'created\t…\n…' | shasum -a 256 | cut -c1-8
        let digest = crate::fixity::digest(by_hand.as_bytes());
        assert_eq!(
            &digest["sha256:".len().."sha256:".len() + 8],
            "21ae2ca1",
            "the id digest changed — every event on disk keeps its old id"
        );
        assert_eq!(
            mint_id(
                "2026-07-31T09:15:22.481903Z",
                TRIGGER_MANUAL,
                Some("pre-sync"),
                Some("2026-07-30-1804-nightly-8c1d55aa"),
                &files,
            )
            .unwrap(),
            "2026-07-31-0915-pre-sync-21ae2ca1",
            "the id's shape is `<date>-<HHMM>[-<slug>]-<8 hex>` (§4)"
        );
    }

    #[test]
    fn the_id_stamp_reads_the_timestamp_and_survives_a_round_trip() {
        assert_eq!(id_stamp("2026-07-31T09:15:22Z").unwrap(), "2026-07-31-0915");
        assert!(id_stamp("yesterday").is_err());
        assert_eq!(
            display_stamp("2026-07-31-0915-pre-sync-4f2a9c1e"),
            "2026-07-31 09:15"
        );
        // Two captures in the same minute must not read identically in an index,
        // so the entry carries the label slug the id already encodes.
        assert_eq!(
            display_entry("2026-07-31-0915-pre-sync-4f2a9c1e"),
            "2026-07-31 09:15 (pre-sync)"
        );
        assert_eq!(
            label_slug("2026-07-31-0915-pre-sync-4f2a9c1e"),
            Some("pre-sync".into())
        );
        assert_eq!(label_slug("2026-07-31-0915-4f2a9c1e"), None);
        assert_eq!(
            display_entry("2026-07-31-0915-4f2a9c1e"),
            "2026-07-31 09:15"
        );
    }

    #[test]
    fn the_canonical_form_ignores_the_serialization_format() {
        // Two devices, same state, same timestamp — the id must converge, which
        // is what makes a collision benign rather than a conflict.
        let files = vec![entry("a.md", b"a"), entry("b.md", b"b")];
        let one = mint_id("2026-07-31T09:15:22Z", TRIGGER_MANUAL, None, None, &files).unwrap();
        let two = mint_id("2026-07-31T09:15:22Z", TRIGGER_MANUAL, None, None, &files).unwrap();
        assert_eq!(one, two);

        // A different capture set is a different event.
        let changed = vec![entry("a.md", b"a"), entry("b.md", b"CHANGED")];
        let three = mint_id("2026-07-31T09:15:22Z", TRIGGER_MANUAL, None, None, &changed).unwrap();
        assert_ne!(one, three);
        // …and so is a different parent, so two devices forking from different
        // points do not collide.
        let forked = mint_id(
            "2026-07-31T09:15:22Z",
            TRIGGER_MANUAL,
            None,
            Some("2026-07-30-1804-nightly-8c1d55aa"),
            &files,
        )
        .unwrap();
        assert_ne!(one, forked);
    }

    #[test]
    fn a_label_is_slugged_into_the_id_and_omitted_when_absent() {
        let files = vec![entry("a.md", b"a")];
        let labeled = mint_id(
            "2026-07-31T09:15:22Z",
            TRIGGER_MANUAL,
            Some("Pre Sync!"),
            None,
            &files,
        )
        .unwrap();
        assert!(
            labeled.starts_with("2026-07-31-0915-pre-sync-"),
            "{labeled}"
        );
        let bare = mint_id("2026-07-31T09:15:22Z", TRIGGER_MANUAL, None, None, &files).unwrap();
        assert!(bare.starts_with("2026-07-31-0915-"), "{bare}");
        // Both still parse back to the same shard.
        assert_eq!(shard_of(&labeled).unwrap(), shard_of(&bare).unwrap());
    }

    #[test]
    fn timestamps_of_two_precisions_still_order_against_each_other() {
        // The migration hazard, stated as an assertion. A store keeps every
        // precision it was ever written at, because events are immutable and sync
        // interleaves devices — so the comparison, not the clock, is what has to
        // make them one order.
        let coarse = "2026-07-31T09:15:10Z";
        let fine = "2026-07-31T09:15:10.500000Z";
        assert!(
            coarse > fine,
            "the raw strings really are backwards — `Z` sorts after `.`"
        );
        assert!(
            comparable(coarse) < comparable(fine),
            "normalized, 09:15:10.000000 precedes 09:15:10.500000"
        );

        // Padding is to a fixed width, from either side, so a stamp written by
        // some other tool at millisecond or nanosecond precision still lands in
        // the right place.
        assert_eq!(
            comparable("2026-07-31T09:15:10Z"),
            "2026-07-31T09:15:10.000000Z"
        );
        assert_eq!(
            comparable("2026-07-31T09:15:10.5Z"),
            "2026-07-31T09:15:10.500000Z"
        );
        assert_eq!(
            comparable("2026-07-31T09:15:10.123456789Z"),
            "2026-07-31T09:15:10.123456Z"
        );
        // Already canonical: borrowed, not rebuilt.
        assert!(matches!(
            comparable("2026-07-31T09:15:10.123456Z"),
            std::borrow::Cow::Borrowed(_)
        ));
        // Not a `Z` stamp: left exactly as found rather than quietly mangled.
        assert_eq!(
            comparable("2026-07-31T09:15:10+01:00"),
            "2026-07-31T09:15:10+01:00"
        );

        // And the id is unaffected: it reads the calendar head only, so the
        // fraction changes nothing about where an event lives or what it is called.
        assert_eq!(id_stamp(coarse).unwrap(), id_stamp(fine).unwrap());
    }
}
