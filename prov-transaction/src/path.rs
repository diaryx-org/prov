//! Lexical path handling — normalization, and the guard that keeps a staged op
//! inside the root it was applied against.
//!
//! Purely lexical: nothing here touches the filesystem, so it holds for a path
//! naming something that does not exist yet, and it is the same answer on every
//! backend. Symlinks are consequently *not* resolved — a link inside the root
//! pointing out of it is not something this can see. A backend that must defend
//! against that has to refuse symlinks itself.

use std::path::{Component, Path, PathBuf};

/// Lexically normalize a relative path: drop `.` components and fold
/// `parent/..` pairs. Leading `..` components (escaping the root) are kept —
/// the caller decides whether that is an error, which is what
/// [`escapes_root`] is for.
pub fn normalize(path: impl AsRef<Path>) -> PathBuf {
    let mut out: Vec<Component> = Vec::new();
    for component in path.as_ref().components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                _ => out.push(component),
            },
            other => out.push(other),
        }
    }
    out.iter().collect()
}

/// Whether `path`, resolved against a root, would land *outside* it.
///
/// Two ways a root-relative path can escape the tree it is joined onto: an
/// **absolute** path (or a Windows drive prefix), which `root.join(path)` jumps
/// to wholesale, ignoring the root entirely; and one whose [`normalize`]d form
/// still leads with `..`, a climb above the root that the `parent/..` folding
/// could not cancel.
///
/// [`ChangeSet::apply`](crate::ChangeSet::apply) refuses either before it
/// writes or journals anything, so a set assembled from untrusted input — a
/// link target authored by whoever wrote the document, a path out of a config
/// file — can never name a file outside the tree it was pointed at.
///
/// A path that stays within the root (`notes/a.md`, or `../sibling/b.md` where
/// the leading climb is cancelled by what precedes it) returns `false`.
pub fn escapes_root(path: impl AsRef<Path>) -> bool {
    matches!(
        normalize(path).components().next(),
        Some(Component::ParentDir | Component::RootDir | Component::Prefix(_))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_dot_and_parent_components() {
        assert_eq!(normalize("a/./b"), PathBuf::from("a/b"));
        assert_eq!(normalize("a/b/../c"), PathBuf::from("a/c"));
        assert_eq!(normalize("a/../../b"), PathBuf::from("../b"));
    }

    #[test]
    fn escapes_only_when_the_climb_survives_folding() {
        assert!(!escapes_root("notes/a.md"));
        assert!(!escapes_root("notes/../a.md"));
        assert!(escapes_root("../a.md"));
        assert!(escapes_root("a/../../etc/passwd"));
        assert!(escapes_root("/etc/passwd"));
    }
}
