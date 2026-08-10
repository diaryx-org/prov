use std::path::{Path, PathBuf};

use prov_graph::error::{Error, Result};

use super::{BLOBS_DIR, EVENTS_DIR, HISTORY_DIR};

/// How the store was located — the difference between "the root says where it
/// is", "it is sitting at the conventional path and the root has stopped saying
/// so", and "there is no store".
///
/// The middle case is the one this distinction exists for. Discovery is through
/// the root's `history` pointer, so a transport that mangles one line of the root
/// document takes the entire safety net out of prov's view: `history-list` goes
/// blank, `check` says nothing, and the store is sitting right there. Recovery
/// must never be gated behind repairing the thing that broke, so the read verbs
/// take [`Conventional`](StoreLocation::Conventional) as a found store — and
/// `check` reports the missing pointer
/// ([`Finding::HistoryStoreUnlinked`](crate::validate::Finding::HistoryStoreUnlinked)) so it is
/// re-declared rather than silently depended upon.
///
/// **Only the conventional path is probed**, never a search. A store the root
/// declared somewhere unusual and then stopped declaring is not recoverable by
/// guessing, and a filesystem sweep for anything shaped like a store is how a
/// backup copy gets adopted as the live one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreLocation {
    /// The root's `history` pointer names it. The ordinary case.
    Declared,
    /// Nothing declares it, but a store index is on disk at [`HISTORY_DIR`].
    Conventional,
    /// There is no store: nothing declared, nothing at the conventional path.
    Absent,
}

impl StoreLocation {
    /// Whether a store is actually there to read.
    pub fn exists(self) -> bool {
        !matches!(self, StoreLocation::Absent)
    }
}

/// The shard directory an event id belongs in, relative to the store's `events/`
/// directory: `<YYYY>/<MM>`, parsed straight out of the id's own leading
/// `YYYY-MM-`.
///
/// This is what makes "the index is only a cache" true rather than aspirational:
/// an id resolves to a path with every index file destroyed.
pub fn shard_of(id: &str) -> Result<PathBuf> {
    let bad = || Error::Structure(format!("`{id}` is not a history event id"));
    let (year, rest) = id.split_once('-').ok_or_else(bad)?;
    let (month, _) = rest.split_once('-').ok_or_else(bad)?;
    if year.len() != 4
        || month.len() != 2
        || !year.bytes().all(|b| b.is_ascii_digit())
        || !month.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(bad());
    }
    Ok(PathBuf::from(year).join(month))
}

/// Where the event document for `id` lives, given the store's index document.
/// A pure function of the id — the whole point of repeating the date in it.
pub fn event_path(store_index: &Path, id: &str, ext: &str) -> Result<PathBuf> {
    Ok(store_dir(store_index)
        .join(EVENTS_DIR)
        .join(shard_of(id)?)
        .join(format!("{id}.{ext}")))
}

/// Where the blob for `hash` lives: `blobs/<first-2-hex>/<rest>`.
///
/// **Bare hex, never the `sha256:` scheme prefix an event spells** — a colon in a
/// filename is hostile to Windows and to more than one sync client.
pub fn blob_path(store_index: &Path, hash: &str) -> Result<PathBuf> {
    let hex = hash.strip_prefix("sha256:").ok_or_else(|| {
        Error::Structure(format!("`{hash}` is not a sha256 digest prov can park"))
    })?;
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Structure(format!("`{hash}` is not a sha256 digest")));
    }
    let (prefix, rest) = hex.split_at(2);
    Ok(store_dir(store_index)
        .join(BLOBS_DIR)
        .join(prefix)
        .join(rest))
}

/// The store's directory — the index document's own parent.
pub fn store_dir(store_index: &Path) -> PathBuf {
    store_index
        .parent()
        .unwrap_or(Path::new(HISTORY_DIR))
        .to_path_buf()
}

/// The `<year>`/`<month>` pair of a shard path, or an error when it is not one.
pub(super) fn shard_parts(shard: &Path) -> Result<(String, String)> {
    let parts: Vec<String> = shard
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    match parts.as_slice() {
        [year, month] => Ok((year.clone(), month.clone())),
        _ => Err(Error::Structure(format!(
            "{} is not a history shard directory",
            shard.display()
        ))),
    }
}

/// Whether `stem` is a well-formed event id: `YYYY-MM-DD-HHMM[-slug]-<8 hex>`.
///
/// The gate that keeps a transport's leavings out of the store. A
/// `.sync-conflict-20260731-091600` copy of an event or an index ends in six
/// digits rather than eight hex characters, so it is litter beside the store
/// rather than a phantom event — which matters, because an index rebuilt to
/// *include* the conflict copy would enshrine the damage it is repairing.
pub(super) fn is_event_id(stem: &str) -> bool {
    let parts: Vec<&str> = stem.split('-').collect();
    let [year, month, day, time, rest @ ..] = parts.as_slice() else {
        return false;
    };
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    let Some(digest) = rest.last() else {
        return false;
    };
    digits(year, 4)
        && digits(month, 2)
        && digits(day, 2)
        && digits(time, 4)
        && digest.len() == 8
        && digest.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The minute an event id stamps, as `YYYY-MM-DD-HHMM`.
///
/// The whole of what a filename can say about *when* — `created` is written to
/// [`FRACTION_DIGITS`](super::event_id::FRACTION_DIGITS) places, and the id's
/// trailing digest is content-derived rather than ordered. So this narrows a
/// search for the newest event; it does not settle one. See
/// [`history_summary`](crate::Workspace::history_summary).
pub(super) fn id_stamp_of(stem: &str) -> Option<String> {
    if !is_event_id(stem) {
        return None;
    }
    // `is_event_id` has already established four leading fields and a digest.
    let parts: Vec<&str> = stem.split('-').collect();
    Some(parts[..4].join("-"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stamp_is_the_minute_and_nothing_after_it() {
        assert_eq!(
            id_stamp_of("2026-07-31-0915-pre-sync-4f2a9c1e").as_deref(),
            Some("2026-07-31-0915")
        );
        // A labelled and an unlabelled capture in the same minute share a stamp —
        // which is the point: the bucket is what gets read, not the winner.
        assert_eq!(
            id_stamp_of("2026-07-31-0915-4f2a9c1e"),
            id_stamp_of("2026-07-31-0915-pre-sync-9b3e0d77")
        );
        assert_eq!(id_stamp_of("not-an-event-id"), None);
        // A transport's conflict copy is not an event, so it stamps nothing.
        assert_eq!(
            id_stamp_of("2026-07-31-0915-4f2a9c1e.sync-conflict-091600"),
            None
        );
    }

    #[test]
    fn an_event_id_is_reversible_to_its_shard_path() {
        let id = "2026-07-31-0915-pre-sync-4f2a9c1e";
        assert_eq!(shard_of(id).unwrap(), Path::new("2026").join("07"));
        assert_eq!(
            event_path(Path::new("history/index.md"), id, "md").unwrap(),
            Path::new("history/events/2026/07/2026-07-31-0915-pre-sync-4f2a9c1e.md")
        );
        // The point of repeating the date in the id: it resolves with every
        // index file destroyed.
        assert!(shard_of("not-an-event-id").is_err());
    }

    #[test]
    fn a_blob_path_is_bare_hex_never_the_scheme_prefix() {
        let hash = crate::fixity::digest(b"hello");
        let path = blob_path(Path::new("history/index.md"), &hash).unwrap();
        let spelled = path.to_string_lossy();
        assert!(
            !spelled.contains(':'),
            "a colon in a blob filename is hostile to Windows and to sync clients: {spelled}"
        );
        let hex = hash.strip_prefix("sha256:").unwrap();
        assert_eq!(
            path,
            Path::new("history/blobs").join(&hex[..2]).join(&hex[2..])
        );
        assert!(blob_path(Path::new("history/index.md"), "blake3:beef").is_err());
    }
}
