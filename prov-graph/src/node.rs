//! What a workspace node looks like, so a walk can tell where another
//! workspace begins.
//!
//! The node is `prov`'s idea — finding one, reading it, and choosing a root by
//! it all live there (`prov::node`). This crate needs only the shape, because
//! its scans are the ones that would otherwise walk straight into a workspace
//! nested inside the one they serve and read its ids and titles as their own.
//! The boundary proposal made such a directory a workspace of its own; a scan
//! that meets one stops at its edge, as the census already stops at a foreign
//! reference.

use std::path::Path;

use crate::document;
use crate::fs::{DirEntry, ReadStorage};

/// The stem of a workspace node, in every location.
pub const NODE_STEM: &str = "prov";

/// Where a node may live under a workspace's root, in precedence order: the
/// top level, then a `config` directory, then a hidden one.
pub const NODE_DIRS: [&str; 3] = ["", "config", ".config"];

/// Whether `path` is shaped like a workspace node: stem `prov`, in a metadata
/// format this build can parse.
///
/// A format whose feature is off is not a node, which is the honest answer —
/// prov cannot read it, so it cannot be the policy a workspace runs under.
pub fn is_node_file(path: &Path) -> bool {
    let stem_matches = path
        .file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case(NODE_STEM));
    stem_matches && document::whole_file_format(path).is_some()
}

/// Whether a directory, whose listing is `entries`, holds a workspace node in
/// any of the [`NODE_DIRS`] — which is what makes it a workspace's root.
///
/// The top level costs nothing, the listing being in hand; a `config` or
/// `.config` directory costs one more listing, and only where the listing
/// says one is there.
pub async fn holds_node<FS: ReadStorage>(fs: &FS, dir: &Path, entries: &[DirEntry]) -> bool {
    let file_named_node = |entry: &DirEntry| {
        !entry.file_type().is_dir()
            && entry
                .file_name()
                .is_some_and(|n| is_node_file(Path::new(n)))
    };
    if entries.iter().any(file_named_node) {
        return true;
    }
    for sub in &NODE_DIRS[1..] {
        let present = entries.iter().any(|entry| {
            entry.file_type().is_dir() && entry.file_name().and_then(|n| n.to_str()) == Some(sub)
        });
        if !present {
            continue;
        }
        if let Ok(inner) = fs.read_dir(&dir.join(sub)).await
            && inner.iter().any(file_named_node)
        {
            return true;
        }
    }
    false
}
