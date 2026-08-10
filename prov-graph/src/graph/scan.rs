//! Flat scans — the passes that read the tree as a *directory*, not as a graph.
//!
//! Three of the four things resolution needs cannot themselves be found by
//! following links, because they are what makes following links possible. The
//! [`TitleIndex`] is the clearest case: nominal references (`[[My File]]`)
//! resolve through it, and a nominal reference may itself be *spanning*
//! (`contents: alias`), so building the index by walking the tree would need the
//! index to walk the tree. It is a flat filesystem scan for exactly that reason
//! — a derived cache (DESIGN §5), rebuilt on demand and never persisted.
//!
//! [`scan_ids`](Graph::scan_ids) and [`content_documents`](Graph::content_documents)
//! are the same shape: enumerate what is on disk, decide nothing about it.
//! [`direct_child_files`](Graph::direct_child_files) is the bounded variant the
//! census uses — the directories the graph already reaches, and no others, so a
//! vendored subtree or a nested workspace is never swept in (DESIGN §8).

use std::collections::BTreeSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use super::{Graph, Target};
use crate::content::ContentFormat;
use crate::document::is_opaque_payload;
use crate::error::Result;
use crate::fs::ReadStorage;
use crate::index::IdIndex;
use crate::link::{self, Link};
use crate::meta::Value;
use crate::title::{self, TitleIndex};

impl<FS: ReadStorage, Ix: IdIndex> Graph<FS, Ix> {
    /// Build the workspace's [`TitleIndex`] by scanning every document under the
    /// root and registering it under its `title` and its file stem. This is a
    /// **derived cache** (DESIGN §5): rebuilt on demand, never persisted. It is
    /// what makes nominal (`[[My File]]`) references resolvable — a flat
    /// filesystem scan, deliberately independent of link resolution so that
    /// alias links can themselves be *spanning* (`contents: alias`) without a
    /// chicken-and-egg between "walk the tree" and "resolve the walk's links."
    pub async fn title_index(&self) -> Result<TitleIndex> {
        let mut index = TitleIndex::new();
        self.scan_titles(PathBuf::new(), &[], &mut index).await?;
        Ok(index)
    }

    /// The title index bounded to the directories the workspace reaches from
    /// `start` (DESIGN §8) — the reachability-scoped counterpart to
    /// [`title_index`](Self::title_index). Only documents in a directory some
    /// link path/id-reaches are indexed, so a `[[alias]]` resolves within the
    /// workspace without scanning `target/`, a vendored tree, or a nested
    /// workspace at the repo root.
    ///
    /// Falls back to the full [`title_index`](Self::title_index) when the
    /// **spanning** relation is addressed by alias: descending the tree then needs
    /// every title up front, so the scan cannot be bounded (the chicken-and-egg
    /// the flat scan was written to avoid). An overlay alias to an *orphan* (a doc
    /// no path/id link reaches) likewise falls outside the scope and reads as
    /// broken — which it effectively is.
    /// `parked` names the directories whose *interiors* are prov's own
    /// bookkeeping — a history store's events and blobs, the recycle bin's items.
    /// They are reached like anything else (the root points at each store's index
    /// document) but a title found inside one is not a place a reader can go, so
    /// indexing it would let `[[Some Note]]` resolve to a deleted copy or an old
    /// version — silently, since neither is anywhere the reader can see. The
    /// caller supplies them because *which* directories those are is a question
    /// about prov's storage layout, and this crate has no opinion about it.
    pub async fn title_index_scoped(&self, start: &Path, parked: &[PathBuf]) -> Result<TitleIndex> {
        let (dirs, needs_full) = self.title_scope(start, parked).await?;
        if needs_full {
            // The unbounded fallback still owes the same exclusion: falling back
            // is about not being able to *bound* the scan, not about suddenly
            // being willing to name prov's bookkeeping.
            let mut index = TitleIndex::new();
            self.scan_titles(PathBuf::new(), parked, &mut index).await?;
            return Ok(index);
        }
        let mut index = TitleIndex::new();
        let files = self.direct_child_files(&dirs).await?;
        let listing: BTreeSet<PathBuf> = files.iter().cloned().collect();
        for rel in files {
            if !is_document_path(&rel) || self.is_shadowed_payload(&rel, &listing).await {
                continue;
            }
            if let Some(stem) = rel.file_stem().and_then(|s| s.to_str()) {
                index.insert(stem, rel.clone());
            }
            if let Ok((_, doc)) = self.load(&rel).await
                && let Some(title) = doc.meta.get("title").and_then(Value::as_str)
            {
                index.insert(title, rel.clone());
            }
        }
        Ok(index)
    }

    /// The directories the workspace occupies, reached from `start` by following
    /// path/id links — spanning links drive descent, and every relation's (and
    /// body wikilink's) path/id target contributes its directory, so an alias can
    /// resolve to anything the tree links. The scope [`title_index_scoped`] indexes.
    ///
    /// The returned flag is `true` when a **spanning** link is alias-shaped: it
    /// cannot be followed without the title index, so the scope would be
    /// incomplete and the caller must scan in full instead. That answer is
    /// final the moment it is reached, and the only caller throws `dirs` away
    /// when it comes back set — so the walk **stops there** rather than
    /// finishing a traversal whose result is already known to be discarded.
    /// The abandoned half is not cheap: every remaining document would be read
    /// and its prose body parsed (`scan_body_links`) purely to contribute
    /// directories to a set nobody reads.
    async fn title_scope(
        &self,
        start: &Path,
        parked: &[PathBuf],
    ) -> Result<(BTreeSet<PathBuf>, bool)> {
        let spanning = self.relations().spanning_relation().map(str::to_owned);
        let dir_of = |p: &Path| p.parent().unwrap_or(Path::new("")).to_path_buf();
        let is_parked = |dir: &Path| parked.iter().any(|p| dir.starts_with(p));
        let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
        let mut visited: BTreeSet<PathBuf> = BTreeSet::new();
        let mut queue = vec![link::normalize(start)];
        while let Some(path) = queue.pop() {
            if !visited.insert(path.clone()) {
                continue;
            }
            let dir = dir_of(&path);
            if is_parked(&dir) {
                continue;
            }
            dirs.insert(dir);
            let Ok((_, doc)) = self.load(&path).await else {
                continue;
            };
            for edge in self.relations().edges(&doc.meta) {
                let link = Link::parse(&edge.target);
                let is_spanning = Some(edge.relation.as_str()) == spanning.as_deref();
                if link.is_external() {
                    continue;
                }
                if title::is_alias_shaped(&link.target) {
                    // Can't resolve without the index; a spanning alias defeats
                    // bounding, and nothing later can un-defeat it.
                    if is_spanning {
                        return Ok((BTreeSet::new(), true));
                    }
                    continue;
                }
                if let Target::Path(target) = self.resolve_link(&path, &link) {
                    let dir = dir_of(&target);
                    if is_parked(&dir) {
                        continue;
                    }
                    dirs.insert(dir);
                    if is_spanning {
                        queue.push(target);
                    }
                }
            }
            for body_link in link::scan_body_links(&path, &doc.body) {
                let link = body_link.link;
                if link.is_external() || title::is_alias_shaped(&link.target) {
                    continue;
                }
                if let Target::Path(target) = self.resolve_link(&path, &link) {
                    let dir = dir_of(&target);
                    if !is_parked(&dir) {
                        dirs.insert(dir);
                    }
                }
            }
        }
        // Reaching here means no spanning link was alias-shaped — every early
        // return above is the only way `true` comes back.
        Ok((dirs, false))
    }

    /// Scan every document under the root for a self-stored `id` frontmatter
    /// field, returning the `(id, path)` pairs — the rebuildable id→path map for
    /// the frontmatter-only identity storage mode ([`IdStorage::FrontmatterOnly`]).
    /// Like [`title_index`](Self::title_index) this is a flat filesystem scan,
    /// deliberately independent of link resolution (so it can bootstrap the very
    /// index that id links resolve through, with no chicken-and-egg).
    ///
    /// [`IdStorage::FrontmatterOnly`]: crate::identity::IdStorage::FrontmatterOnly
    pub async fn scan_ids(&self) -> Result<Vec<(crate::identity::Id, PathBuf)>> {
        let mut ids = Vec::new();
        self.scan_ids_dir(PathBuf::new(), &mut ids).await?;
        Ok(ids)
    }

    /// Every content document (Markdown/Djot/HTML) under the root, as sorted
    /// workspace-relative paths — the on-disk population the orphan check diffs
    /// against what the spanning tree reaches (DESIGN §8). Deliberately restricted
    /// to *content* documents: whole-file metadata sidecars (a config or registry
    /// document, a stray `.yaml`) are not prose a user orphans, so they are not
    /// candidates. A flat filesystem scan (hidden entries skipped), independent of
    /// link resolution, like the title/id scans beside it.
    pub async fn content_documents(&self) -> Result<Vec<PathBuf>> {
        let mut docs = Vec::new();
        self.scan_content_dir(PathBuf::new(), &mut docs).await?;
        docs.sort();
        Ok(docs)
    }

    /// The workspace-relative direct-child files of each directory in `dirs`
    /// (non-recursive), skipping hidden entries and unreadable directories.
    ///
    /// The bounded-scan primitive behind reachability-scoped discovery (DESIGN
    /// §8): it opens only the directories it is handed and never descends into
    /// subdirectories, so an *unreached* directory — a vendored tree, a nested
    /// prov workspace — is neither read nor reported. Callers filter the
    /// result for the file kind they care about (content documents for the orphan
    /// check, opaque payloads for `attach --all`).
    pub async fn direct_child_files(&self, dirs: &BTreeSet<PathBuf>) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        for dir in dirs {
            let Ok(entries) = self.listing(dir).await else {
                continue;
            };
            for entry in entries {
                let Some(name) = entry
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_owned)
                else {
                    continue;
                };
                if name.starts_with('.') || !entry.file_type().is_file() {
                    continue;
                }
                files.push(if dir.as_os_str().is_empty() {
                    PathBuf::from(&name)
                } else {
                    dir.join(&name)
                });
            }
        }
        Ok(files)
    }

    /// The directories the reachable set `reachable` occupies — each reached
    /// document's own directory (the workspace root's directory always among
    /// them, since the root document is reachable). The scope
    /// [`direct_child_files`](Self::direct_child_files) is bounded to: a directory
    /// is "known" precisely when a linked document lives directly in it.
    pub fn reached_dirs(reachable: &BTreeSet<PathBuf>) -> BTreeSet<PathBuf> {
        reachable
            .iter()
            .map(|p| p.parent().unwrap_or(Path::new("")).to_path_buf())
            .collect()
    }

    /// Recursively collect content-document paths under `rel_dir`. Same walk as
    /// [`scan_ids_dir`](Self::scan_ids_dir); unreadable/hidden entries are skipped.
    fn scan_content_dir<'a>(
        &'a self,
        rel_dir: PathBuf,
        docs: &'a mut Vec<PathBuf>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
            let Ok(entries) = self.listing(&rel_dir).await else {
                return Ok(());
            };
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
                let rel = if rel_dir.as_os_str().is_empty() {
                    PathBuf::from(&name)
                } else {
                    rel_dir.join(&name)
                };
                if entry.file_type().is_dir() {
                    self.scan_content_dir(rel, docs).await?;
                } else if entry.file_type().is_file()
                    && ContentFormat::from_extension(&rel).is_some()
                {
                    docs.push(rel);
                }
            }
            Ok(())
        })
    }

    /// Recursively collect self-stored `id` fields under `rel_dir`. Same walk as
    /// [`scan_titles`](Self::scan_titles); unreadable/hidden entries are skipped.
    fn scan_ids_dir<'a>(
        &'a self,
        rel_dir: PathBuf,
        ids: &'a mut Vec<(crate::identity::Id, PathBuf)>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
            let Ok(entries) = self.listing(&rel_dir).await else {
                return Ok(());
            };
            let listing = file_listing(&rel_dir, &entries);
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
                let rel = if rel_dir.as_os_str().is_empty() {
                    PathBuf::from(&name)
                } else {
                    rel_dir.join(&name)
                };
                if entry.file_type().is_dir() {
                    self.scan_ids_dir(rel, ids).await?;
                } else if entry.file_type().is_file()
                    && is_document_path(&rel)
                    // An `id:` inside a shadowed payload is an example, not a
                    // claim on the registry (see `attach_opaque`).
                    && !self.is_shadowed_payload(&rel, &listing).await
                    && let Ok((_, doc)) = self.load(&rel).await
                    && let Some(id) = doc.meta.get("id").and_then(Value::as_str)
                    && !id.trim().is_empty()
                {
                    ids.push((crate::identity::Id(id.trim().to_string()), rel));
                }
            }
            Ok(())
        })
    }

    /// Recursively index the documents under the workspace-relative `rel_dir`,
    /// never descending into a directory under `parked`. Unreadable directories
    /// and files are skipped (a title index is a best-effort cache, not a
    /// validation pass); hidden entries (`.`-prefixed) are ignored.
    ///
    /// `parked` is [`parked_dirs`](Self::parked_dirs) — prov's byte-parking
    /// stores. Excluded by *not descending* rather than by filtering afterwards,
    /// so a workspace with a thousand history events does not read a thousand
    /// event documents in order to throw their titles away.
    fn scan_titles<'a>(
        &'a self,
        rel_dir: PathBuf,
        parked: &'a [PathBuf],
        index: &'a mut TitleIndex,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
            if parked.iter().any(|p| rel_dir.starts_with(p)) {
                return Ok(());
            }
            let Ok(entries) = self.listing(&rel_dir).await else {
                return Ok(());
            };
            let listing = file_listing(&rel_dir, &entries);
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
                let rel = if rel_dir.as_os_str().is_empty() {
                    PathBuf::from(&name)
                } else {
                    rel_dir.join(&name)
                };
                if entry.file_type().is_dir() {
                    self.scan_titles(rel, parked, index).await?;
                } else if entry.file_type().is_file()
                    && is_document_path(&rel)
                    // A shadowed payload is bytes prov agreed not to read: its
                    // title is a specimen's, and must not answer `[[alias]]`.
                    && !self.is_shadowed_payload(&rel, &listing).await
                {
                    // Always index by stem (name-based resolution, Obsidian-style)…
                    if let Some(stem) = rel.file_stem().and_then(|s| s.to_str()) {
                        index.insert(stem, rel.clone());
                    }
                    // …and by the declared `title` when the document parses.
                    if let Ok((_, doc)) = self.load(&rel).await
                        && let Some(title) = doc.meta.get("title").and_then(Value::as_str)
                    {
                        index.insert(title, rel.clone());
                    }
                }
            }
            Ok(())
        })
    }
}

/// Whether `path` names a document the title scan should read — one whose
/// extension is a recognized body format (Markdown/Djot/HTML) or a whole-file
/// metadata format (YAML/JSON/…). Non-document files (images, binaries) are
/// skipped so the scan neither reads nor mis-indexes them.
fn is_document_path(path: &Path) -> bool {
    !is_opaque_payload(path)
}

/// The workspace-relative paths of the *files* among a directory's `entries`,
/// the listing a shadow check probes
/// ([`is_shadowed_payload`](Graph::is_shadowed_payload)). Hidden entries are
/// skipped, matching the scans that build this.
fn file_listing(rel_dir: &Path, entries: &[crate::fs::DirEntry]) -> BTreeSet<PathBuf> {
    entries
        .iter()
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.file_name().and_then(|n| n.to_str()).map(str::to_owned))
        .filter(|name| !name.starts_with('.'))
        .map(|name| {
            if rel_dir.as_os_str().is_empty() {
                PathBuf::from(name)
            } else {
                rel_dir.join(name)
            }
        })
        .collect()
}
