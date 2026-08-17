//! Reading manifests off the graph — the lookups every pass over a covered
//! directory shares.
//!
//! The model and its serialization are [`crate::manifest`]; here is what needs a
//! filesystem: loading the manifest a node declares, the reverse lookup from a
//! directory to the node covering it, and the directory walk a manifest is
//! compared against.
//!
//! The reverse lookup mirrors the attachment one exactly (`shadow.rs`): the
//! `<dir>.<ext>` convention is the fast path, and the `manifest` → `root`
//! chain is authoritative. A node under a non-conventional name still covers
//! its directory; it just is not found by probing.

use std::collections::BTreeSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use super::Graph;
use crate::document::{is_opaque_payload, require_whole_file};
use crate::error::{Error, Result};
use crate::fs::ReadStorage;
use crate::index::IdIndex;
use crate::link;
use crate::manifest::{Manifest, manifest_node_candidates};

impl<FS: ReadStorage, Ix: IdIndex> Graph<FS, Ix> {
    /// The manifest document `node` declares, loaded and parsed, with its
    /// workspace-relative path. `None` when `node` declares no `manifest`.
    ///
    /// A manifest is a **record store** (spec §5): prov re-lays-out its rows, so
    /// a markdown carrier has no stable home for them and is refused here, at
    /// the one choke point every reader passes through.
    pub async fn manifest_of(&self, node: &Path) -> Result<Option<(PathBuf, Manifest)>> {
        let (_, doc) = self.load(node).await?;
        let Some(raw) = doc.manifest_attr() else {
            return Ok(None);
        };
        let path = link::resolve(node, raw);
        let manifest = self.read_manifest(&path).await?;
        Ok(Some((path, manifest)))
    }

    /// Read and parse the manifest document at `path` itself.
    pub async fn read_manifest(&self, path: &Path) -> Result<Manifest> {
        let (_, doc) = self.load(path).await?;
        let carrier = doc
            .carrier
            .ok_or_else(|| Error::Structure(format!("{} carries no metadata", path.display())))?;
        require_whole_file(path, carrier)?;
        let manifest = Manifest::from_meta(&doc.meta)
            .map_err(|e| Error::Structure(format!("{}: {e}", path.display())))?;
        // Judged here, where the manifest's own location is known — see
        // `Manifest::checked_root`.
        manifest
            .checked_root(path)
            .map_err(|e| Error::Structure(format!("{}: {e}", path.display())))?;
        Ok(manifest)
    }

    /// Whether the document at `candidate` is a manifest node whose manifest
    /// covers the directory `dir` — the authoritative half of the reverse
    /// lookup below.
    ///
    /// Unreadable, unparsable and non-manifest candidates simply do not claim:
    /// this runs inside best-effort scans, where the question is "is this
    /// directory already accounted for", and a damaged manifest is a finding
    /// `check` raises rather than a reason to abort a walk.
    pub async fn manifest_claims(&self, candidate: &Path, dir: &Path) -> bool {
        match self.manifest_of(candidate).await {
            Ok(Some((manifest_doc, manifest))) => {
                manifest.covered_root(&manifest_doc) == link::normalize(dir)
            }
            _ => false,
        }
    }

    /// The node covering the directory `dir`, or `None` when nothing does —
    /// the counterpart of `attachment_for` for a whole directory. Probes the
    /// `<dir>.<ext>` convention and confirms each hit through the node's own
    /// `manifest` pointer.
    pub async fn manifest_node_for(&self, dir: &Path) -> Result<Option<PathBuf>> {
        let dir = link::normalize(dir);
        for candidate in manifest_node_candidates(&dir) {
            if self.exists(&candidate).await? && self.manifest_claims(&candidate, &dir).await {
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    }

    /// Whether any directory on `path`'s way down from the workspace root is
    /// covered by a manifest — the guard the loose-attachment sweeps use so a
    /// covered directory is never offered up for ten thousand sidecars.
    ///
    /// Walks the ancestors rather than only the immediate parent, because a
    /// manifest claims its root *recursively*: `photos/2019/a.jpg` is covered by
    /// the node beside `photos/`.
    pub async fn under_manifest(&self, path: &Path) -> Result<bool> {
        let path = link::normalize(path);
        let mut dir = path.parent().map(Path::to_path_buf);
        while let Some(current) = dir {
            if current.as_os_str().is_empty() {
                break;
            }
            if self.manifest_node_for(&current).await?.is_some() {
                return Ok(true);
            }
            dir = current.parent().map(Path::to_path_buf);
        }
        Ok(false)
    }

    /// The opaque payloads under the covered directory `root`, as paths relative
    /// to it, sorted — what a manifest is built from and compared against.
    ///
    /// Three exclusions, each deliberate. **Hidden entries** are skipped, as in
    /// every other prov walk. **Files prov can read** (a `.md` note, a `.yaml`
    /// store) are not payloads and stay ordinary documents — a manifest covers
    /// bytes, never shadows a document. And a **nested manifest's** directory is
    /// left to its own node, so two manifests never claim the same file.
    pub async fn scan_covered(&self, root: &Path) -> Result<Vec<PathBuf>> {
        let mut found = Vec::new();
        self.scan_covered_into(root, PathBuf::new(), &mut found)
            .await?;
        found.sort_by(|a, b| {
            crate::manifest::path_sort_key(a).cmp(&crate::manifest::path_sort_key(b))
        });
        Ok(found)
    }

    fn scan_covered_into<'a>(
        &'a self,
        root: &'a Path,
        rel: PathBuf,
        out: &'a mut Vec<PathBuf>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
            let dir = link::normalize(root.join(&rel));
            let Ok(entries) = self.listing(&dir).await else {
                return Ok(());
            };
            let mut names: Vec<(String, bool)> = Vec::new();
            for entry in entries {
                let Some(name) = entry
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_owned)
                else {
                    continue;
                };
                if name.starts_with('.') {
                    continue;
                }
                names.push((name, entry.file_type().is_dir()));
            }
            for (name, is_dir) in names {
                let child = if rel.as_os_str().is_empty() {
                    PathBuf::from(&name)
                } else {
                    rel.join(&name)
                };
                if is_dir {
                    // A nested manifest owns its own subtree.
                    if self
                        .manifest_node_for(&link::normalize(root.join(&child)))
                        .await?
                        .is_some()
                    {
                        continue;
                    }
                    self.scan_covered_into(root, child, out).await?;
                } else if is_opaque_payload(&child) {
                    out.push(child);
                }
            }
            Ok(())
        })
    }

    /// The covered roots of every manifest reachable in `walk_docs` — the set a
    /// scan consults to know which directories are already accounted for.
    /// Damaged manifests contribute nothing (their damage is `check`'s to
    /// report).
    pub async fn manifest_roots(&self, walk_docs: &BTreeSet<PathBuf>) -> BTreeSet<PathBuf> {
        let mut roots = BTreeSet::new();
        for doc in walk_docs {
            if let Ok(Some((manifest_doc, manifest))) = self.manifest_of(doc).await {
                roots.insert(manifest.covered_root(&manifest_doc));
            }
        }
        roots
    }
}
