use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::identity::Id;

use super::model::*;

/// A path spelled with `/` separators regardless of host platform — what goes
/// into a manifest and into the canonical form, so an event minted on Windows and
/// one minted on Linux describe the same state identically.
pub(super) fn slash_path(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The manifest's sort key: `path`, byte-wise ascending on the joined UTF-8
/// string (`docs/history-format.md` §3.1) — **not** `Path::cmp`, which orders
/// component-wise and disagrees with it. `notes.md` and `notes/x.md` are the
/// minimal case: joined, `.` (0x2E) sorts before `/` (0x2F), so `notes.md`
/// comes first. Component-wise, the first path is one component (`notes.md`)
/// and the second's first component is the bare `notes` — a prefix of
/// `notes.md` and therefore "less" than it — so `Path::cmp` puts `notes/x.md`
/// first, backwards from the joined string it will end up serialized as. Row
/// order feeds `canonical_bytes` (§4.1), so this is the one sort in the store
/// two independent implementations have to agree on bit-for-bit.
pub(super) fn path_sort_key(path: &Path) -> String {
    slash_path(path)
}

/// A manifest as a path → (id, hash) map — the same rows a `Vec<FileEntry>`
/// holds, keyed so two manifests compare equal **by content, regardless of row
/// order**. §6's "computed manifest is identical" is a same-state test ("same
/// paths, same ids, same hashes"), not a same-bytes test, so it must not care
/// that a manifest written before this fix keeps whatever row order it was
/// written in (§4: an event's id is the one it was minted with, never
/// re-derived) while every manifest computed from here on is sorted per §3.1.
/// `Vec<FileEntry>`'s derived `PartialEq` is row-order-sensitive and is the
/// wrong tool for this comparison.
pub(super) fn manifest_of(files: &[FileEntry]) -> BTreeMap<&Path, (&Option<Id>, &str)> {
    files
        .iter()
        .map(|f| (f.path.as_path(), (&f.id, f.hash.as_str())))
        .collect()
}

/// Whether `path` sits inside `dir` (or *is* it) — the capture-set exclusion
/// test, applied to normalized workspace-relative paths.
pub(super) fn under(path: &Path, dir: &Path) -> bool {
    dir.as_os_str().is_empty() || path == dir || path.starts_with(dir)
}

/// The first two paths in `paths` that fold to the same ASCII-lowercased key
/// without being equal — the shape a manifest captured on a case-sensitive
/// filesystem can legitimately hold (two real, distinct files) and that
/// self-clobbers on a case-insensitive one: writing the second row's bytes
/// lands on the file the first row's write just created, silently discarding
/// it. Order is the manifest's own (paths are captured pre-sorted), so the
/// pair reported is deterministic.
pub(super) fn case_fold_collision<'a>(
    paths: impl Iterator<Item = &'a Path>,
) -> Option<(PathBuf, PathBuf)> {
    let mut seen: BTreeMap<String, &Path> = BTreeMap::new();
    for path in paths {
        let key = path.to_string_lossy().to_ascii_lowercase();
        match seen.get(&key) {
            Some(&other) if other != path => {
                return Some((other.to_path_buf(), path.to_path_buf()));
            }
            _ => {
                seen.insert(key, path);
            }
        }
    }
    None
}
