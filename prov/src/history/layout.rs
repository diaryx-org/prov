use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

use super::{BLOBS_DIR, EVENTS_DIR, HISTORY_DIR};

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
