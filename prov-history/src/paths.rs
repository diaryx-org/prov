use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use prov_graph::identity::Id;

use super::model::*;

/// A path spelled with `/` separators regardless of host platform — what goes
/// into a manifest and into the canonical form, so an event minted on Windows and
/// one minted on Linux describe the same state identically.
pub fn slash_path(path: &Path) -> String {
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
pub fn path_sort_key(path: &Path) -> String {
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
pub fn manifest_of(files: &[FileEntry]) -> BTreeMap<&Path, (&Option<Id>, &str)> {
    files
        .iter()
        .map(|f| (f.path.as_path(), (&f.id, f.hash.as_str())))
        .collect()
}

/// Whether `path` sits inside `dir` (or *is* it) — the capture-set exclusion
/// test, applied to normalized workspace-relative paths.
pub fn under(path: &Path, dir: &Path) -> bool {
    dir.as_os_str().is_empty() || path == dir || path.starts_with(dir)
}

/// The first two paths in `paths` that fold to the same ASCII-lowercased key
/// without being equal — the shape a manifest captured on a case-sensitive
/// filesystem can legitimately hold (two real, distinct files) and that
/// self-clobbers on a case-insensitive one: writing the second row's bytes
/// lands on the file the first row's write just created, silently discarding
/// it. Order is the manifest's own (paths are captured pre-sorted), so the
/// pair reported is deterministic.
pub fn case_fold_collision<'a>(
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal manifest-row fixture — `support::entry` does not follow this
    /// module across the crate boundary, so each moved test module carries its
    /// own copy of this one-liner rather than reaching back into `prov`.
    fn entry(path: &str, hash_of: &[u8]) -> FileEntry {
        FileEntry {
            path: PathBuf::from(path),
            id: None,
            hash: prov_fixity::digest(hash_of),
        }
    }

    #[test]
    fn manifest_order_is_byte_wise_on_the_joined_string_not_path_component_order() {
        // The bug, stated as directly as possible: `.` (0x2E) sorts before `/`
        // (0x2F) in the joined string, so `notes.md` belongs before
        // `notes/x.md`. `Path::cmp` disagrees — it compares the bare `notes`
        // component (a prefix of `notes.md`) against the whole `notes.md`
        // component and calls `notes` smaller, putting `notes/x.md` first.
        let notes_file = Path::new("notes.md");
        let notes_dir_file = Path::new("notes/x.md");
        assert!(
            notes_dir_file < notes_file,
            "Path::cmp really does get this backwards"
        );
        assert!(
            path_sort_key(notes_file) < path_sort_key(notes_dir_file),
            "the manifest's own key must get it the other way round"
        );

        // The same collision one directory deeper, to be sure the key is not
        // secretly depth-limited.
        let mut paths = [
            Path::new("deep/notes/x.md"),
            Path::new("deep/notes.md"),
            Path::new("index.md"),
            Path::new("notes/x.md"),
            Path::new("notes.md"),
        ];
        paths.sort_by_key(|p| path_sort_key(p));
        assert_eq!(
            paths,
            [
                Path::new("deep/notes.md"),
                Path::new("deep/notes/x.md"),
                Path::new("index.md"),
                Path::new("notes.md"),
                Path::new("notes/x.md"),
            ]
        );
    }

    #[test]
    fn manifest_equality_for_the_unchanged_check_ignores_row_order() {
        // §6's "computed manifest is identical" is same-state, not
        // same-serialization: a manifest a pre-fix writer left in `Path`
        // component order must still compare equal to the correctly-ordered
        // one this fix computes for the same state, or a habitual `history-
        // capture` against an old store would start filling the log with
        // duplicates the moment it hit a collision like `notes.md` /
        // `notes/x.md`.
        let sorted = vec![entry("notes.md", b"n"), entry("notes/x.md", b"x")];
        let mut component_order = sorted.clone();
        component_order.reverse();
        assert_ne!(
            sorted, component_order,
            "the derived Vec equality this replaces really is row-order-sensitive"
        );
        assert_eq!(manifest_of(&sorted), manifest_of(&component_order));
    }

    #[test]
    fn the_capture_set_exclusion_is_by_directory_prefix() {
        let store = Path::new("history");
        assert!(under(Path::new("history"), store));
        assert!(under(Path::new("history/index.md"), store));
        assert!(under(Path::new("history/events/2026/07/x.md"), store));
        // A sibling that merely shares a prefix is not inside it.
        assert!(!under(Path::new("historybook.md"), store));
        assert!(!under(Path::new("notes/a.md"), store));
    }
}
