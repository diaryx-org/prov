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
use crate::manifest::{Manifest, excluded_by, manifest_node_candidates};

/// How many symbolic links to directories a covered walk follows along one
/// path before it takes them for a loop and refuses.
///
/// Well under the kernel's own bound (40 links in resolving one path, on
/// Linux), on purpose: past that bound every listing and stat fails, and a
/// walk that ran into it would end quietly with each file already listed
/// once per lap of the loop. Eight is more links in one path than an archive
/// linked together on purpose has a reason for.
pub const LINKED_DIRECTORIES: usize = 8;

/// What one covered walk is asked: where, whether documents are claimed, and
/// what is left out.
struct Walk<'a> {
    root: &'a Path,
    verbatim: bool,
    exclude: &'a [String],
}

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
    ///
    /// **Probe-only, and bounded on purpose.** A node renamed away from its
    /// directory leaves no local evidence beside that directory (moving the
    /// archive to keep the convention is the thing `rename` deliberately does
    /// not do), so this can answer "no" where a census would answer "yes". The
    /// caller is `attach`, which runs per file inside `--all`; making each one
    /// authoritative would cost a census per file. The residue is a covered file
    /// that also gains a sidecar — duplicated bookkeeping, not a contradiction,
    /// since both records are derived from the same bytes. The operation where a
    /// wrong "no" *would* matter — minting a second manifest over a whole
    /// archive — asks `manifest_node_covering` instead and pays for the census.
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
        self.scan_claimed(root, false).await
    }

    /// [`scan_covered`](Self::scan_covered) for a manifest that may be
    /// [`verbatim`](crate::manifest::Manifest::verbatim): when `verbatim`, the
    /// files prov can read are listed too, since such a manifest claims them.
    /// Hidden entries and nested manifests' directories are still left out.
    pub async fn scan_claimed(&self, root: &Path, verbatim: bool) -> Result<Vec<PathBuf>> {
        self.scan_excluding(root, verbatim, &[]).await
    }

    /// What `manifest`, declared by the manifest document at `manifest_doc`,
    /// claims under its root as the directory stands: [`scan_claimed`]
    /// narrowed by the manifest's [`exclude`] patterns. An excluded directory
    /// is not walked.
    ///
    /// [`scan_claimed`]: Self::scan_claimed
    /// [`exclude`]: crate::manifest::Manifest::exclude
    pub async fn scan_manifest(
        &self,
        manifest_doc: &Path,
        manifest: &Manifest,
    ) -> Result<Vec<PathBuf>> {
        let root = manifest.checked_root(manifest_doc)?;
        self.scan_excluding(&root, manifest.verbatim, &manifest.exclude)
            .await
    }

    /// [`scan_claimed`](Self::scan_claimed), leaving out what `exclude` names.
    ///
    /// A **symbolic link** is followed: one to a file is listed as that file,
    /// whose bytes a hash then reads, and one to a directory is walked as that
    /// directory. That is what lets a covered directory, or one below it, live
    /// somewhere the workspace does not — a share on the machine the archive
    /// is kept on, linked in under the name the manifest's `root` gives. A
    /// link to nothing is not a file and is not listed; a chain of more than
    /// [`LINKED_DIRECTORIES`] linked directories on one path is taken for a
    /// loop and refused.
    pub async fn scan_excluding(
        &self,
        root: &Path,
        verbatim: bool,
        exclude: &[String],
    ) -> Result<Vec<PathBuf>> {
        let mut found = Vec::new();
        let walk = Walk {
            root,
            verbatim,
            exclude,
        };
        self.scan_covered_into(&walk, PathBuf::new(), 0, &mut found)
            .await?;
        found.sort_by(|a, b| {
            crate::manifest::path_sort_key(a).cmp(&crate::manifest::path_sort_key(b))
        });
        Ok(found)
    }

    fn scan_covered_into<'a>(
        &'a self,
        walk: &'a Walk<'a>,
        rel: PathBuf,
        links: usize,
        out: &'a mut Vec<PathBuf>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
            let root = walk.root;
            let dir = link::normalize(root.join(&rel));
            let Ok(entries) = self.listing(&dir).await else {
                return Ok(());
            };
            let mut names: Vec<(String, bool, bool)> = Vec::new();
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
                let file_type = entry.file_type();
                let (is_dir, linked) = if file_type.is_symlink() {
                    // What the link leads to decides; a link to nothing is
                    // not a file of the archive.
                    match self.stat(&dir.join(&name)).await {
                        Ok(target) if target.is_dir() => (true, true),
                        Ok(target) if target.is_file() => (false, true),
                        _ => continue,
                    }
                } else {
                    (file_type.is_dir(), false)
                };
                names.push((name, is_dir, linked));
            }
            for (name, is_dir, linked) in names {
                let child = if rel.as_os_str().is_empty() {
                    PathBuf::from(&name)
                } else {
                    rel.join(&name)
                };
                if excluded_by(walk.exclude, &child, is_dir) {
                    continue;
                }
                if is_dir {
                    // A nested manifest owns its own subtree.
                    if self
                        .manifest_node_for(&link::normalize(root.join(&child)))
                        .await?
                        .is_some()
                    {
                        continue;
                    }
                    let links = links + usize::from(linked);
                    if links > LINKED_DIRECTORIES {
                        return Err(Error::Structure(format!(
                            "{}: more than {LINKED_DIRECTORIES} linked directories deep — \
                             a symbolic link that leads back to itself?",
                            link::normalize(root.join(&child)).display()
                        )));
                    }
                    self.scan_covered_into(walk, child, links, out).await?;
                } else if walk.verbatim || is_opaque_payload(&child) {
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
        self.manifest_roots_claiming(walk_docs)
            .await
            .into_iter()
            .map(|(root, _)| root)
            .collect()
    }

    /// [`manifest_roots`](Self::manifest_roots), each with whether its manifest
    /// is [`verbatim`](crate::manifest::Manifest::verbatim) — whether the
    /// documents under it are claimed too.
    pub async fn manifest_roots_claiming(
        &self,
        walk_docs: &BTreeSet<PathBuf>,
    ) -> Vec<(PathBuf, bool)> {
        let mut roots = Vec::new();
        for doc in walk_docs {
            if let Ok(Some((manifest_doc, manifest))) = self.manifest_of(doc).await {
                roots.push((manifest.covered_root(&manifest_doc), manifest.verbatim));
            }
        }
        roots.sort();
        roots.dedup();
        roots
    }

    /// The roots of the [`verbatim`](crate::manifest::Manifest::verbatim)
    /// manifests whose nodes `reachable` holds beside a directory some
    /// reachable path lies in — the directories a reachability-bounded pass
    /// must not read documents out of.
    ///
    /// Found by the `<dir>.<ext>` convention against `reachable` alone, so a
    /// workspace with no manifest node beside a reached directory reads
    /// nothing; the bound is [`under_manifest`](Self::under_manifest)'s.
    pub async fn verbatim_roots_reached(&self, reachable: &BTreeSet<PathBuf>) -> Vec<PathBuf> {
        // The convention read backwards, as `ShadowProbe` reads it: a reached
        // `photos.yaml` could be the node of `photos/`. Only metadata files
        // can be nodes, so most reached paths cost one extension check.
        let mut by_dir: std::collections::HashMap<PathBuf, Vec<&PathBuf>> =
            std::collections::HashMap::new();
        for path in reachable {
            if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| super::shadow::SIDECAR_EXTENSIONS.contains(&ext))
            {
                by_dir
                    .entry(path.with_extension(""))
                    .or_default()
                    .push(path);
            }
        }
        let mut nodes: BTreeSet<PathBuf> = BTreeSet::new();
        if !by_dir.is_empty() {
            for dir in Self::reached_dirs(reachable) {
                for ancestor in dir.ancestors() {
                    if ancestor.as_os_str().is_empty() {
                        break;
                    }
                    if let Some(found) = by_dir.get(ancestor) {
                        nodes.extend(found.iter().map(|p| (*p).clone()));
                    }
                }
            }
        }
        self.manifest_roots_claiming(&nodes)
            .await
            .into_iter()
            .filter_map(|(root, verbatim)| verbatim.then_some(root))
            .collect()
    }

    /// Whether the directory `dir` is the root of a
    /// [`verbatim`](crate::manifest::Manifest::verbatim) manifest, judged from
    /// `siblings`, the names of the files in `dir`'s parent: the
    /// `<dir>.<ext>` convention is probed against that listing, so a directory
    /// with no such sibling costs no read at all.
    ///
    /// What the flat title and id scans ask before descending: a verbatim
    /// manifest claims the documents under it, so their titles and ids are
    /// not the workspace's. Probe-only, with the same bound
    /// [`under_manifest`](Self::under_manifest) states.
    pub async fn is_verbatim_root(&self, dir: &Path, siblings: &BTreeSet<String>) -> bool {
        let dir = link::normalize(dir);
        for candidate in manifest_node_candidates(&dir) {
            let Some(name) = candidate.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !siblings.contains(name) {
                continue;
            }
            if let Ok(Some((manifest_doc, manifest))) = self.manifest_of(&candidate).await
                && manifest.covered_root(&manifest_doc) == dir
            {
                return manifest.verbatim;
            }
        }
        false
    }
}
