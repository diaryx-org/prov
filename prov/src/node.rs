//! The **workspace node** — the config document, found without the root.
//!
//! A workspace node is a document whose subject is the workspace itself rather
//! than a member of it: it carries `workspace_id`, `relations`, `spanning`,
//! `exports` and the identity policy, and it is not in the spanning tree, not
//! censused, and reached by no walk. That is what the *config document* has
//! always been. What this module adds is finding it **by convention**, so that
//! policy is readable before the root is known rather than only after — the
//! config document proper is reached through the root's `config` pointer
//! ([`Workspace::config_path`](crate::workspace::Workspace::config_path)), which
//! makes spec §1 rule 3's "policy has two homes" true from only one side.
//!
//! The circle that breaks matters for exactly one thing: a directory holding two
//! root candidates with neither conventional stem cannot be read *at all* today,
//! including the policy that would say which of them is the root. A node found
//! without the root can carry [`root`](prov_config::WorkspaceConfig::root) and
//! settle it.
//!
//! The stem is `prov`, after the format, because every comparable specification
//! names its top-level file that way and none names it for a generic concept —
//! OCFL's `0=ocfl_object_1.1`, BagIt's `bagit.txt`, `ro-crate-metadata.json`,
//! `datapackage.json`. Role names like `inventory.json` appear only *inside* a
//! directory whose kind is already known, because at the top of an arbitrary one
//! only a format name resolves.

use std::path::{Path, PathBuf};

use prov_graph::document;
use prov_graph::fs::ReadStorage;

/// The stem of a workspace node, in every location.
pub const NODE_STEM: &str = "prov";

/// Where a node may live, in precedence order: the top level, then a `config`
/// directory, then a hidden one.
///
/// The empty string is the root directory itself. Top level is first because it
/// is what a workspace that has never thought about this already writes; the
/// other two exist for a workspace that wants its listing clean, which is a
/// presentation preference and so loses to the plain answer.
pub const NODE_DIRS: [&str; 3] = ["", "config", ".config"];

/// Extension precedence within one directory.
///
/// Fixed rather than left to directory order, which is not stable across
/// filesystems: two nodes in one directory is a [`check`](crate::validate)
/// finding, and a finding that reported a different winner on each machine
/// would be worse than the mistake it describes.
const NODE_EXTS: [&str; 6] = ["yaml", "yml", "json", "toml", "fig", "figl"];

/// What a directory's conventional locations hold.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Located {
    /// The workspace node — the first in precedence order, if any.
    pub node: Option<PathBuf>,
    /// Every other node found, in precedence order after the winner.
    ///
    /// Non-empty is a mistake worth reporting and not worth refusing over: a
    /// stale `config/prov.yaml` beside a live `prov.yaml` should be a finding,
    /// where the root tie is an outright refusal, because a workspace that
    /// cannot be opened cannot be repaired either.
    pub shadowed: Vec<PathBuf>,
}

impl Located {
    /// Every node found, winner first — the winner and its shadows in one list.
    pub fn all(&self) -> impl Iterator<Item = &PathBuf> {
        self.node.iter().chain(&self.shadowed)
    }
}

/// Whether `path` is shaped like a workspace node: stem `prov`, in a metadata
/// format this build can parse.
///
/// A format whose feature is off is not a node, which is the honest answer —
/// prov cannot read it, so it cannot be the policy this workspace runs under.
fn is_node_file(path: &Path) -> bool {
    let stem_matches = path
        .file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case(NODE_STEM));
    stem_matches && document::whole_file_format(path).is_some()
}

/// Rank a node file by [`NODE_EXTS`], so a directory holding two resolves the
/// same way everywhere. An extension outside the list sorts last.
fn ext_rank(path: &Path) -> usize {
    path.extension()
        .and_then(|e| e.to_str())
        .and_then(|e| {
            let lower = e.to_ascii_lowercase();
            NODE_EXTS.iter().position(|known| *known == lower)
        })
        .unwrap_or(NODE_EXTS.len())
}

/// The node files directly in `dir`, ranked by extension, named relative to
/// `prefix`.
async fn nodes_in<FS: ReadStorage>(fs: &FS, dir: &Path, prefix: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs.read_dir(dir).await else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .into_iter()
        .filter(|entry| !entry.file_type().is_dir())
        .filter_map(|entry| {
            let name = entry.file_name()?;
            is_node_file(Path::new(name)).then(|| prefix.join(name))
        })
        .collect();
    found.sort_by_key(|path| (ext_rank(path), path.clone()));
    found
}

/// Locate the workspace node under `root_dir`, in [`NODE_DIRS`] precedence.
///
/// `entries` is `root_dir`'s own listing, which the caller has already read —
/// [`discover`](crate::discovery::discover) reads one per ancestor either way,
/// so matching a stem in it is free. It is also what makes the subdirectories
/// cheap: an entry says whether `config`/`.config` exist, so a workspace with
/// neither pays no further syscall and one that opted in pays exactly one.
pub async fn locate_in<FS: ReadStorage>(
    fs: &FS,
    root_dir: &Path,
    entries: &[prov_graph::fs::DirEntry],
) -> Located {
    let mut found = Vec::new();
    for dir in NODE_DIRS {
        if dir.is_empty() {
            let mut top: Vec<PathBuf> = entries
                .iter()
                .filter(|entry| !entry.file_type().is_dir())
                .filter_map(|entry| {
                    let name = entry.file_name()?;
                    is_node_file(Path::new(name)).then(|| PathBuf::from(name))
                })
                .collect();
            top.sort_by_key(|path| (ext_rank(path), path.clone()));
            found.append(&mut top);
            continue;
        }
        // Only descend where the listing already says the directory is there.
        let present = entries.iter().any(|entry| {
            entry.file_type().is_dir() && entry.file_name().and_then(|n| n.to_str()) == Some(dir)
        });
        if !present {
            continue;
        }
        found.append(&mut nodes_in(fs, &root_dir.join(dir), Path::new(dir)).await);
    }
    let mut found = found.into_iter();
    Located {
        node: found.next(),
        shadowed: found.collect(),
    }
}

/// [`locate_in`], reading `root_dir`'s listing itself.
///
/// For a caller that does not already hold one — [`locate_in`] is the shape
/// discovery wants, since it reads the listing for its own reasons first.
pub async fn locate<FS: ReadStorage>(fs: &FS, root_dir: &Path) -> Located {
    let Ok(entries) = fs.read_dir(root_dir).await else {
        return Located::default();
    };
    locate_in(fs, root_dir, &entries).await
}

impl<FS: ReadStorage, Id, Ix: prov_graph::index::IdIndex> crate::workspace::Workspace<FS, Id, Ix> {
    /// This workspace's node, asked of a workspace already located rather than
    /// of a directory being searched for one — the counterpart to
    /// [`root_document`](crate::workspace::Workspace::root_document).
    ///
    /// Goes to the storage port rather than through
    /// [`listing`](crate::workspace::Workspace::listing) deliberately: a
    /// workspace that declared `.config` `out_of_scope` would have the node
    /// filtered out of its own listings, and policy is not content — it is the
    /// document that says what the content *is*, so it is read whatever the
    /// scope rules say about the directory it sits in.
    pub async fn workspace_node(&self) -> Located {
        locate(self.fs(), self.root()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prov_graph::exec::block_on;
    use prov_graph::fs::StdFs;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prov-node-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn found(dir: &Path) -> Located {
        block_on(locate(&StdFs, dir))
    }

    #[test]
    fn a_top_level_node_is_the_node() {
        let dir = tmp("top");
        std::fs::write(dir.join("prov.yaml"), "workspace_id: notes\n").unwrap();
        let located = found(&dir);
        assert_eq!(located.node, Some(PathBuf::from("prov.yaml")));
        assert!(located.shadowed.is_empty());
    }

    #[test]
    fn a_workspace_with_no_node_locates_nothing() {
        let dir = tmp("none");
        std::fs::write(dir.join("README.md"), "---\ntitle: Home\n---\n").unwrap();
        assert_eq!(found(&dir), Located::default());
    }

    #[test]
    fn config_and_hidden_config_are_alternatives() {
        for sub in ["config", ".config"] {
            let dir = tmp(&format!("sub-{}", sub.trim_start_matches('.')));
            std::fs::create_dir_all(dir.join(sub)).unwrap();
            std::fs::write(dir.join(sub).join("prov.yaml"), "workspace_id: notes\n").unwrap();
            assert_eq!(
                found(&dir).node,
                Some(PathBuf::from(sub).join("prov.yaml")),
                "{sub} should hold the node"
            );
        }
    }

    #[test]
    fn top_level_wins_and_the_rest_are_shadowed_in_order() {
        let dir = tmp("precedence");
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::create_dir_all(dir.join(".config")).unwrap();
        std::fs::write(dir.join("prov.yaml"), "workspace_id: top\n").unwrap();
        std::fs::write(dir.join("config/prov.yaml"), "workspace_id: mid\n").unwrap();
        std::fs::write(dir.join(".config/prov.yaml"), "workspace_id: low\n").unwrap();
        let located = found(&dir);
        assert_eq!(located.node, Some(PathBuf::from("prov.yaml")));
        assert_eq!(
            located.shadowed,
            vec![
                PathBuf::from("config/prov.yaml"),
                PathBuf::from(".config/prov.yaml")
            ]
        );
    }

    #[test]
    fn two_formats_in_one_directory_resolve_by_a_fixed_order() {
        // Not by directory order, which is not stable across filesystems.
        // Both spellings here belong to the `yaml` feature, which is the default
        // build, so the ordering is exercised whatever else is switched on.
        let dir = tmp("formats");
        std::fs::write(dir.join("prov.yml"), "workspace_id: notes\n").unwrap();
        std::fs::write(dir.join("prov.yaml"), "workspace_id: notes\n").unwrap();
        let located = found(&dir);
        assert_eq!(located.node, Some(PathBuf::from("prov.yaml")));
        assert_eq!(located.shadowed, vec![PathBuf::from("prov.yml")]);
    }

    #[test]
    fn a_format_this_build_cannot_parse_is_not_a_node() {
        // The honest answer: prov cannot read it, so it cannot be the policy
        // this workspace runs under. Under `--features toml` it would be one.
        let dir = tmp("unbuilt-format");
        std::fs::write(dir.join("prov.ini"), "workspace_id = notes\n").unwrap();
        assert_eq!(found(&dir).node, None);
    }

    #[test]
    fn a_directory_named_prov_is_not_a_node() {
        let dir = tmp("dir-named-prov");
        std::fs::create_dir_all(dir.join("prov.yaml")).unwrap();
        assert_eq!(found(&dir).node, None);
    }

    #[test]
    fn a_content_document_stemmed_prov_is_not_a_node() {
        // `prov.md` is prose about prov, which any workspace may hold. Only a
        // whole-file metadata document is policy.
        let dir = tmp("prose");
        std::fs::write(dir.join("prov.md"), "---\ntitle: prov\n---\n# prov\n").unwrap();
        assert_eq!(found(&dir).node, None);
    }
}
