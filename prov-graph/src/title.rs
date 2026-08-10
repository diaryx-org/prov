//! Title index — the derived `name → document` map that resolves nominal
//! ("alias") references like `[[My File]]`.
//!
//! This is a **derived cache** in DESIGN §5's sense: rebuildable from a scan of
//! the workspace, never authoritative. A nominal link addresses a document by
//! its `title` (or, failing that, its file stem) rather than by a path or a
//! stable id — the readable-but-fallible option on the identity spectrum (see
//! `docs/reference-styles.md`). Because titles are neither unique nor stable, a
//! name can resolve to exactly one document, to several (ambiguous — a nominal
//! link cannot choose), or to none.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A `name → document(s)` index built by scanning a workspace. Names are the
/// documents' `title` fields and their file stems; a document is registered
/// under both so `[[My File]]` (by title) and `[[my-file]]` (by stem) both find
/// it.
#[derive(Debug, Clone, Default)]
pub struct TitleIndex {
    by_name: HashMap<String, Vec<PathBuf>>,
}

/// The outcome of resolving a name against a [`TitleIndex`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TitleMatch {
    /// Exactly one document claims the name.
    Unique(PathBuf),
    /// Several documents claim it — a nominal link cannot disambiguate. The
    /// paths are sorted for a stable, diffable report.
    Ambiguous(Vec<PathBuf>),
    /// No document claims it.
    Unknown,
}

impl TitleIndex {
    /// An empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Index `path` under `name` (a title or a file stem). A blank name is
    /// ignored; a duplicate `(name, path)` pair collapses, so registering a
    /// document under both a title and an identical stem does not make it look
    /// ambiguous.
    pub fn insert(&mut self, name: impl Into<String>, path: impl Into<PathBuf>) {
        let name = name.into();
        if name.trim().is_empty() {
            return;
        }
        let path = path.into();
        let paths = self.by_name.entry(name).or_default();
        if !paths.contains(&path) {
            paths.push(path);
        }
    }

    /// Resolve `name` to a document. `Unique` when exactly one claims it,
    /// `Ambiguous` when several do, `Unknown` when none.
    pub fn resolve(&self, name: &str) -> TitleMatch {
        match self.by_name.get(name).map(Vec::as_slice) {
            None | Some([]) => TitleMatch::Unknown,
            Some([one]) => TitleMatch::Unique(one.clone()),
            Some(many) => {
                let mut paths = many.to_vec();
                paths.sort();
                TitleMatch::Ambiguous(paths)
            }
        }
    }

    /// Whether the index knows no names.
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// The number of distinct names known.
    pub fn len(&self) -> usize {
        self.by_name.len()
    }
}

/// Whether `target` is shaped like a nominal reference — a single bare name,
/// with no path separator and no file extension (`My File`, `intro`), as
/// opposed to a path (`notes/a.md`, `README.md`) or a scheme'd id. Only such
/// targets are looked up in the title index; everything else resolves as a path.
pub fn is_alias_shaped(target: &str) -> bool {
    !target.is_empty()
        && !target.contains('/')
        && !target.contains('\\')
        && Path::new(target).extension().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_unique_ambiguous_and_unknown() {
        let mut ix = TitleIndex::new();
        ix.insert("My File", "notes/a.md");
        // Same document under its stem too — collapses, stays unique.
        ix.insert("a", "notes/a.md");
        ix.insert("Shared", "one.md");
        ix.insert("Shared", "two.md");

        assert_eq!(
            ix.resolve("My File"),
            TitleMatch::Unique(PathBuf::from("notes/a.md"))
        );
        assert_eq!(
            ix.resolve("a"),
            TitleMatch::Unique(PathBuf::from("notes/a.md"))
        );
        assert_eq!(
            ix.resolve("Shared"),
            TitleMatch::Ambiguous(vec![PathBuf::from("one.md"), PathBuf::from("two.md")])
        );
        assert_eq!(ix.resolve("nobody"), TitleMatch::Unknown);
    }

    #[test]
    fn blank_names_are_ignored() {
        let mut ix = TitleIndex::new();
        ix.insert("   ", "a.md");
        assert!(ix.is_empty());
    }

    #[test]
    fn alias_shape_excludes_paths_and_extensions() {
        assert!(is_alias_shaped("My File"));
        assert!(is_alias_shaped("intro"));
        assert!(!is_alias_shaped("notes/a.md"));
        assert!(!is_alias_shaped("README.md"));
        assert!(!is_alias_shaped(""));
    }
}
