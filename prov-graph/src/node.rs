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

use crate::document::{self, Document};
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
/// any of the [`NODE_DIRS`] — one half of what makes it a workspace's root.
/// The other half is a root document; see [`holds_workspace`].
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

/// Whether a directory, whose listing is `entries`, is a workspace's root: it
/// holds a node ([`holds_node`]) **and** a root document — the one its node
/// names (`root:`), or a document there shaped like one.
///
/// A node alone is not a workspace. A preset is a directory laid out like the
/// root of the workspace it will be merged into — a node carrying only the
/// axes it declares, and the stores beside it — and it has no root document
/// and no spine (`docs/proposals/presets/proposal-presets-v1.md`, §5): it is
/// configuration to be applied, and a link to its node is an ordinary local
/// link.
///
/// The root half is discovery's test (`prov::discovery`), stated over the
/// shape alone: a named root that is there with metadata, or a content
/// document — or a whole-file one stemmed `index`/`readme` — with metadata, no
/// `part_of`, and no prov byline. It does not break ties: two candidates are a
/// root prov will not pick, but they are still a root's worth of evidence that
/// the directory is a workspace, and the boundary errs toward holding.
pub async fn holds_workspace<FS: ReadStorage>(fs: &FS, dir: &Path, entries: &[DirEntry]) -> bool {
    if !holds_node(fs, dir, entries).await {
        return false;
    }
    if names_present_root(fs, dir, entries).await {
        return true;
    }
    for entry in entries {
        if entry.file_type().is_dir() {
            continue;
        }
        let Some(name) = entry.file_name() else {
            continue;
        };
        let name = Path::new(name);
        if !can_be_root(name) {
            continue;
        }
        if let Some(doc) = parse(fs, &dir.join(name), name).await
            && is_root_candidate(&doc)
        {
            return true;
        }
    }
    false
}

/// Whether the directory's node (the first by [`NODE_DIRS`] precedence) names
/// a `root` that is there and has metadata — discovery's named-root test,
/// which trusts the name without asking whether the document has a parent.
async fn names_present_root<FS: ReadStorage>(fs: &FS, dir: &Path, entries: &[DirEntry]) -> bool {
    for sub in NODE_DIRS {
        let at = dir.join(sub);
        let listing;
        let listed: &[DirEntry] = if sub.is_empty() {
            entries
        } else {
            let present = entries.iter().any(|entry| {
                entry.file_type().is_dir()
                    && entry.file_name().and_then(|n| n.to_str()) == Some(sub)
            });
            if !present {
                continue;
            }
            match fs.read_dir(&at).await {
                Ok(inner) => {
                    listing = inner;
                    &listing
                }
                Err(_) => continue,
            }
        };
        let Some(node) = listed.iter().find_map(|entry| {
            (!entry.file_type().is_dir())
                .then(|| entry.file_name())
                .flatten()
                .filter(|n| is_node_file(Path::new(n)))
                .map(|n| Path::new(n).to_path_buf())
        }) else {
            continue;
        };
        let Some(doc) = parse(fs, &at.join(&node), &node).await else {
            return false;
        };
        let Some(root) = doc.meta.get("root").and_then(|v| v.as_str()) else {
            return false;
        };
        let root = Path::new(root);
        if !root
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
        {
            return false;
        }
        return parse(fs, &dir.join(root), root)
            .await
            .is_some_and(|doc| doc.has_meta());
    }
    false
}

/// Read and parse one document, or nothing when either fails.
async fn parse<FS: ReadStorage>(fs: &FS, at: &Path, name: &Path) -> Option<Document> {
    let text = fs.read_to_string(at).await.ok()?;
    Document::parse(name, &text).ok()
}

/// Shaped like a root document: a content document, or a whole-file one under
/// the conventional `index`/`readme` stem — a stray `.yaml` beside a node is a
/// store, not a root.
fn can_be_root(name: &Path) -> bool {
    if crate::content::ContentFormat::from_extension(name).is_some() {
        return true;
    }
    document::whole_file_format(name).is_some()
        && name
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.eq_ignore_ascii_case("index") || s.eq_ignore_ascii_case("readme"))
}

/// Metadata, no `part_of`, and not a page prov generated (its `generated_by`
/// byline names `prov`).
fn is_root_candidate(doc: &Document) -> bool {
    let generated = doc
        .meta
        .get("generated_by")
        .and_then(|v| v.as_str())
        .and_then(|by| by.split_whitespace().next())
        == Some("prov");
    doc.has_meta() && doc.meta.get("part_of").is_none() && !generated
}
