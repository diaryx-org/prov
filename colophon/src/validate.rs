//! Validation — integrity findings over the workspace graph, from a root.
//!
//! The sleeper feature (DESIGN §8): walk the spanning tree and report every
//! violated invariant as a [`Finding`] — data, not a panic.
//!
//! Underneath sits the **census** ([`Workspace::census`]): one traversal that
//! yields every forward link reachable from the root — frontmatter relation
//! edges *and* body `[[…]]` wikilinks alike — each tagged with where it is
//! written ([`LinkSite`]) and how it resolves ([`Resolution`]). Because it is
//! read straight from the documents, the census is *ground truth*; the backlink
//! map, these findings, and (in `mutate`) inbound-rename maintenance are all
//! views over it, and any stored index heals toward it. [`Workspace::check`] is
//! the findings view. The checks:
//!
//! - **broken link** — a path target (in a relation or a wikilink) that
//!   resolves to nothing on disk;
//! - **case mismatch** — a target that only resolves because the filesystem is
//!   case-insensitive (`docs/design.md` vs `docs/DESIGN.md`): works on macOS,
//!   breaks on Linux. Caught by comparing exact directory listings;
//! - **cycle / duplicate containment** — a spanning target already visited
//!   (the spanning relation must be a single-parent tree);
//! - **missing inverse** — a spanning child whose inverse field (`part_of`)
//!   does not point back at its parent;
//! - **malformed / dangling ID** — a `colophon:<id>` reference (in a relation
//!   or a wikilink) that fails its check character, or that no live registry
//!   entry resolves;
//! - **unreadable** — a document that exists but cannot be read or parsed.
//!
//! External targets (URLs, `mailto:`) are never checked. Autofix comes with
//! the mutation layer's growth; findings first.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::fs::Storage;
use crate::identity::{self, Id, IdentityPolicy};
use crate::index::IndexStore;
use crate::link::{self, Link};
use crate::meta::Value;
use crate::title::{self, TitleIndex, TitleMatch};
use crate::workspace::{Target, Workspace};

/// Where in a document a forward link is written — a frontmatter relation field
/// or a body wikilink. Carried by every link-resolution [`Finding`] and every
/// [`CensusEntry`] so a report can point at the exact site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkSite {
    /// A frontmatter relation field, by name (e.g. `contents`, `links`).
    Relation(String),
    /// A `[[…]]` wikilink in the body, at this byte span.
    Body(Range<usize>),
}

impl fmt::Display for LinkSite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinkSite::Relation(name) => f.write_str(name),
            LinkSite::Body(_) => f.write_str("body"),
        }
    }
}

/// How a forward link resolves against the workspace. Path and id forms stay
/// distinct on purpose: the registry owns id resolution (location-independent,
/// stable across moves), while a path is checked against the on-disk name — so
/// a caller can tell which links a rename must rewrite (paths) from which it
/// must leave alone (ids).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// A path target that resolves to an existing file (exact name).
    Path(PathBuf),
    /// A path target that only matches case-insensitively; `got` is the target
    /// as resolved, `actual` the exact on-disk name.
    CaseMismatch { got: PathBuf, actual: String },
    /// A path target with nothing on disk.
    Broken,
    /// A `colophon:<id>` target the registry resolves to the live path `to`.
    Id { id: Id, to: PathBuf },
    /// A well-formed `colophon:<id>` target with no live registry entry;
    /// `tombstoned` separates "deleted" from "never issued here" (§4 hazard).
    DanglingId { id: Id, tombstoned: bool },
    /// A `colophon:<id>` target failing its check character — a typo.
    MalformedId,
    /// A nominal (alias) target several documents claim — unresolvable.
    /// `candidates` are the sharers, sorted.
    AmbiguousAlias { name: String, candidates: Vec<PathBuf> },
    /// A URL / mail address — off-workspace, never resolved or rewritten.
    External,
}

impl Resolution {
    /// The workspace path this link reaches, if it resolves to one (by path or
    /// through the registry) — what the spanning walk descends into and what a
    /// backlink map keys on. `None` for broken, dangling, malformed, external.
    pub fn resolved_path(&self) -> Option<&PathBuf> {
        match self {
            Resolution::Path(p)
            | Resolution::CaseMismatch { got: p, .. }
            | Resolution::Id { to: p, .. } => Some(p),
            _ => None,
        }
    }
}

/// One forward link as found in a document: where it is written and how it
/// resolves. The unit of the [`census`](Workspace::census).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CensusEntry {
    /// The document that declares the link (workspace-relative).
    pub source: PathBuf,
    /// Where in `source` the link is written.
    pub site: LinkSite,
    /// The target exactly as written.
    pub target_text: String,
    /// How the target resolves.
    pub resolution: Resolution,
}

impl CensusEntry {
    /// The integrity finding this entry represents when its target failed to
    /// resolve cleanly — `None` for a link that resolves.
    fn finding(&self) -> Option<Finding> {
        let doc = self.source.clone();
        let site = self.site.clone();
        let target = self.target_text.clone();
        match &self.resolution {
            Resolution::CaseMismatch { actual, .. } => {
                Some(Finding::CaseMismatch { doc, site, target, actual: actual.clone() })
            }
            Resolution::Broken => Some(Finding::BrokenLink { doc, site, target }),
            Resolution::MalformedId => Some(Finding::MalformedId { doc, site, target }),
            Resolution::DanglingId { id, tombstoned } => {
                Some(Finding::DanglingId { doc, site, id: id.clone(), tombstoned: *tombstoned })
            }
            Resolution::AmbiguousAlias { name, candidates } => Some(Finding::AmbiguousAlias {
                doc,
                site,
                name: name.clone(),
                candidates: candidates.clone(),
            }),
            Resolution::Path(_) | Resolution::Id { .. } | Resolution::External => None,
        }
    }
}

/// An inbound reference to a document, as discovered by the census: which
/// document links here ([`source`](Backlink::source)), where in it
/// ([`site`](Backlink::site)), and whether the link is by stable id (survives
/// moves) or by path (rewritten on a move). The inverse of a forward
/// [`CensusEntry`] — the marquee payoff of the identity layer (DESIGN §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backlink {
    /// The document that links to the target.
    pub source: PathBuf,
    /// Where in `source` the link is written.
    pub site: LinkSite,
    /// `true` when the link is a `colophon:<id>` reference (location-independent),
    /// `false` when it is a path.
    pub by_id: bool,
}

/// One integrity finding. `doc` is always the document that *declares* the
/// problem (workspace-relative); `site` is where in it the offending link sits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    /// `target` (written at `site`) resolves to nothing on disk.
    BrokenLink { doc: PathBuf, site: LinkSite, target: String },
    /// `target` only resolves case-insensitively; the exact on-disk name is
    /// `actual`. Portable workspaces need the exact name.
    CaseMismatch { doc: PathBuf, site: LinkSite, target: String, actual: String },
    /// A spanning target that was already reached — a containment cycle or a
    /// second parent, either of which breaks the single-parent spanning tree.
    DuplicateContainment { doc: PathBuf, target: String },
    /// A spanning child whose inverse field does not link back to `doc`.
    MissingInverse { doc: PathBuf, child: PathBuf, inverse: String },
    /// A document that exists but could not be read or parsed.
    Unreadable { doc: PathBuf, error: String },
    /// A `colophon:<id>` reference whose ID fails the shape/check-character
    /// test — almost certainly a typo, caught before it dangles silently.
    MalformedId { doc: PathBuf, site: LinkSite, target: String },
    /// A well-formed `id:<id>` reference with no live registry entry.
    /// `tombstoned` distinguishes "that document was deleted" from "this ID
    /// was never issued here" (an out-of-band reference the registry has not
    /// reconciled — DESIGN §4's known hazard).
    DanglingId { doc: PathBuf, site: LinkSite, id: Id, tombstoned: bool },
    /// A nominal (alias) reference whose name several documents claim, so it
    /// cannot resolve to one — the fallible edge of title-based linking.
    /// `candidates` are the documents that share the name, sorted.
    AmbiguousAlias { doc: PathBuf, site: LinkSite, name: String, candidates: Vec<PathBuf> },
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Finding::BrokenLink { doc, site, target } => {
                write!(f, "{}: broken {site} link: {target}", doc.display())
            }
            Finding::CaseMismatch { doc, site, target, actual } => write!(
                f,
                "{}: case mismatch in {site} link: {target} is {actual} on disk",
                doc.display()
            ),
            Finding::DuplicateContainment { doc, target } => write!(
                f,
                "{}: {target} is already contained elsewhere (cycle or second parent)",
                doc.display()
            ),
            Finding::MissingInverse { doc, child, inverse } => write!(
                f,
                "{}: child {} does not declare {inverse} back to it",
                doc.display(),
                child.display()
            ),
            Finding::Unreadable { doc, error } => {
                write!(f, "{}: unreadable: {error}", doc.display())
            }
            Finding::MalformedId { doc, site, target } => write!(
                f,
                "{}: malformed ID in {site} link: {target} (bad shape or check character)",
                doc.display()
            ),
            Finding::DanglingId { doc, site, id, tombstoned } => write!(
                f,
                "{}: dangling {site} ID: id:{id} ({})",
                doc.display(),
                if *tombstoned { "document was deleted" } else { "never issued in this registry" }
            ),
            Finding::AmbiguousAlias { doc, site, name, candidates } => write!(
                f,
                "{}: ambiguous {site} alias: [[{name}]] matches {} documents ({})",
                doc.display(),
                candidates.len(),
                candidates.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
            ),
        }
    }
}

/// A concrete repair for a finding — **metadata only**. Autofix never edits body
/// prose: a `[[…]]` that is really code (`[[None] * width]`) must not be
/// "repaired", and structure-aware body editing belongs to a later layer. So the
/// fixable findings are the frontmatter ones; body-link findings are diagnosis
/// only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fix {
    /// Repair a [`Finding::MissingInverse`]: declare `relation` in `doc` pointing
    /// back at `parent`. The concrete target — a path in the workspace's link
    /// style, or a `colophon:<id>` when the workspace authors id links — is
    /// produced when the fix is applied (which may register `parent`), so the
    /// repair matches how the workspace authors every other link.
    AddInverse { doc: PathBuf, relation: String, parent: PathBuf, title: String },
}

impl fmt::Display for Fix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fix::AddInverse { doc, relation, parent, .. } => {
                write!(f, "declare {relation} → {} in {}", parent.display(), doc.display())
            }
        }
    }
}

impl<FS: Storage, IdP, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Suggest a safe, metadata-only [`Fix`] for `finding`, or `None` when it is
    /// not safely auto-fixable — a body-link finding (left for the
    /// structure-aware layer), or a contested containment (a human must pick the
    /// real parent).
    ///
    /// Currently fixes [`Finding::MissingInverse`]: when the child declares *no*
    /// competing parent, add the back-link — mirroring the style (absolute vs
    /// relative) the parent used to reference the child, so the repair reads
    /// native to the workspace. A child that already claims a different parent is
    /// a contested containment and is left alone.
    pub async fn suggest_fix(&self, finding: &Finding) -> Result<Option<Fix>> {
        let Finding::MissingInverse { doc: parent, child, inverse } = finding else {
            return Ok(None);
        };
        // Safe only when the child makes no other (cardinality-one) parent claim.
        let (_, child_doc) = self.load(child).await?;
        if child_doc.meta.get(inverse).is_some() {
            return Ok(None);
        }
        // Title the back-link with the parent's own title (else the path), so a
        // markdown-style repair reads well; the target itself is produced at
        // apply time, in the workspace's link style (or by id).
        let (_, parent_doc) = self.load(parent).await?;
        let title = parent_doc
            .meta
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| link::path_to_title(parent));
        Ok(Some(Fix::AddInverse {
            doc: child.clone(),
            relation: inverse.clone(),
            parent: parent.clone(),
            title,
        }))
    }

    /// Check the workspace reachable from `start`, returning every finding.
    /// An empty result means the reachable graph holds its invariants. This is
    /// the findings view over [`census`](Workspace::census): each forward link
    /// that fails to resolve becomes a finding, joined with the structural
    /// findings (unreadable document, duplicate containment, missing inverse)
    /// the walk raises from traversal state.
    pub async fn check(&self, start: impl AsRef<Path>) -> Result<Vec<Finding>> {
        let (census, mut findings) = self.walk(start.as_ref()).await?;
        for entry in &census {
            findings.extend(entry.finding());
        }
        Ok(findings)
    }

    /// Take a census of every forward link reachable from `start`: one
    /// [`CensusEntry`] per frontmatter relation edge *and* per body `[[…]]`
    /// wikilink, each carrying its [`LinkSite`] and [`Resolution`].
    ///
    /// This is the one traversal the backlink map, the integrity findings, and
    /// (via `mutate`) inbound-rename maintenance are all views over. Because it
    /// is read from the documents, it is ground truth: a stored backlink index
    /// heals *toward* the census, never the reverse.
    pub async fn census(&self, start: impl AsRef<Path>) -> Result<Vec<CensusEntry>> {
        Ok(self.walk(start.as_ref()).await?.0)
    }

    /// The backlink map for the workspace reachable from `start`: every resolved
    /// target to the inbound references ([`Backlink`]s) that reach it, path- and
    /// id-form alike. This is the census inverted — recomputed from the
    /// documents, so it is always fresh (the Route-N "reconcile-on-load": no
    /// stored index to drift). Each target's backlinks are sorted by source.
    pub async fn backlinks(&self, start: impl AsRef<Path>) -> Result<BTreeMap<PathBuf, Vec<Backlink>>> {
        let mut map: BTreeMap<PathBuf, Vec<Backlink>> = BTreeMap::new();
        for entry in self.census(start).await? {
            let by_id = matches!(entry.resolution, Resolution::Id { .. });
            let Some(target) = entry.resolution.resolved_path().cloned() else {
                continue;
            };
            map.entry(target).or_default().push(Backlink {
                source: entry.source,
                site: entry.site,
                by_id,
            });
        }
        for links in map.values_mut() {
            links.sort_by(|a, b| a.source.cmp(&b.source).then(a.by_id.cmp(&b.by_id)));
        }
        Ok(map)
    }

    /// The inbound references to a single `target` (workspace-relative) reachable
    /// from `start`, sorted by source. The focused form of
    /// [`backlinks`](Workspace::backlinks) for "who links here?".
    pub async fn backlinks_to(
        &self,
        start: impl AsRef<Path>,
        target: impl AsRef<Path>,
    ) -> Result<Vec<Backlink>> {
        let target = link::normalize(target);
        let mut links: Vec<Backlink> = self
            .census(start)
            .await?
            .into_iter()
            .filter(|entry| entry.resolution.resolved_path() == Some(&target))
            .map(|entry| {
                let by_id = matches!(entry.resolution, Resolution::Id { .. });
                Backlink { source: entry.source, site: entry.site, by_id }
            })
            .collect();
        links.sort_by(|a, b| a.source.cmp(&b.source).then(a.by_id.cmp(&b.by_id)));
        Ok(links)
    }

    /// The shared spanning-tree walk: gathers the forward-link census and the
    /// structural findings (which depend on traversal state, not on a single
    /// link's resolution) in one pass. Frontmatter edges may be spanning and so
    /// drive descent, the single-parent check, and the inverse check; body
    /// wikilinks are always overlay references — censused, never spanning.
    async fn walk(&self, start: &Path) -> Result<(Vec<CensusEntry>, Vec<Finding>)> {
        let mut census = Vec::new();
        let mut structural = Vec::new();
        let mut visited = BTreeSet::new();
        let mut queue = vec![link::normalize(start)];

        // The nominal-resolution index, built once for the whole walk (a flat
        // scan, independent of the link resolution it powers).
        let titles = self.title_index().await?;

        let spanning = self.relations().spanning_relation().map(str::to_owned);
        let inverse = spanning.as_deref().and_then(|s| {
            self.relations()
                .relations()
                .iter()
                .find(|r| r.name == s)
                .and_then(|r| r.inverse.clone())
        });

        while let Some(path) = queue.pop() {
            if !visited.insert(path.clone()) {
                continue;
            }
            let doc = match self.load(&path).await {
                Ok((_, doc)) => doc,
                Err(e) => {
                    structural.push(Finding::Unreadable { doc: path, error: e.to_string() });
                    continue;
                }
            };

            // Frontmatter relation edges — the only links that can be spanning.
            for edge in self.relations().edges(&doc.meta) {
                // Parse once: `link.target` is the bare target (any `[label](…)`
                // stripped), which is what both the census and findings record.
                let link = Link::parse(&edge.target);
                let resolution = self.resolve_forward(&path, &link, &titles).await;

                if Some(edge.relation.as_str()) == spanning.as_deref()
                    && let Some(resolved) = resolution.resolved_path().cloned()
                {
                    // Single-parent check, inverse check, descent.
                    if visited.contains(&resolved) || queue.contains(&resolved) {
                        structural.push(Finding::DuplicateContainment {
                            doc: path.clone(),
                            target: link.target.clone(),
                        });
                    } else {
                        if let Some(inverse) = inverse.as_deref()
                            && let Ok((_, child_doc)) = self.load(&resolved).await
                            && child_doc.has_meta()
                        {
                            let points_back = child_doc
                                .meta
                                .get(inverse)
                                .map(Value::link_strings)
                                .unwrap_or_default()
                                .iter()
                                .any(|t| {
                                    self.resolve_link_with(&resolved, &Link::parse(t), Some(&titles))
                                        == Target::Path(path.clone())
                                });
                            if !points_back {
                                structural.push(Finding::MissingInverse {
                                    doc: path.clone(),
                                    child: resolved.clone(),
                                    inverse: inverse.to_string(),
                                });
                            }
                        }
                        queue.push(resolved);
                    }
                }

                census.push(CensusEntry {
                    source: path.clone(),
                    site: LinkSite::Relation(edge.relation),
                    target_text: link.target,
                    resolution,
                });
            }

            // Body wikilinks — overlay references, censused but never spanning.
            for wikilink in link::scan_wikilinks(&path, &doc.body) {
                let resolution =
                    self.resolve_forward(&path, &Link::parse(&wikilink.target), &titles).await;
                census.push(CensusEntry {
                    source: path.clone(),
                    site: LinkSite::Body(wikilink.span),
                    target_text: wikilink.target,
                    resolution,
                });
            }

            // A separated document's `content` must resolve to an existing body
            // file. Validated here (not a graph edge, so kept out of the census).
            if let Some(content) = doc.content_attr() {
                let target = link::resolve(&path, content);
                let site = LinkSite::Relation("content".to_string());
                match self.exact_name(&target).await {
                    NameMatch::Exact => {}
                    NameMatch::CaseOnly(actual) => structural.push(Finding::CaseMismatch {
                        doc: path.clone(),
                        site,
                        target: content.to_string(),
                        actual,
                    }),
                    NameMatch::None => structural.push(Finding::BrokenLink {
                        doc: path.clone(),
                        site,
                        target: content.to_string(),
                    }),
                }
            }
        }
        Ok((census, structural))
    }

    /// Resolve one forward link (declared in the document at `source`) into a
    /// [`Resolution`]. A path target is checked against the on-disk name; an
    /// `id:<id>` target resolves through the registry and stays an id-form
    /// resolution; a nominal (`[[My File]]`) target resolves through `titles` —
    /// `Unique` to the on-disk path, `Ambiguous` to
    /// [`Resolution::AmbiguousAlias`], `Unknown` falling through to a path (so a
    /// nominal link to nothing reports as `Broken`, like any dead link).
    async fn resolve_forward(&self, source: &Path, link: &Link, titles: &TitleIndex) -> Resolution {
        if link.is_external() {
            return Resolution::External;
        }
        if let Some(id) = link.id_target() {
            if !identity::verify(id.as_str()) {
                return Resolution::MalformedId;
            }
            return match self.index().resolve(&id) {
                Some(path) => Resolution::Id { id, to: link::normalize(path) },
                None => Resolution::DanglingId { tombstoned: self.index().is_known(&id), id },
            };
        }
        if title::is_alias_shaped(&link.target) {
            match titles.resolve(&link.target) {
                TitleMatch::Unique(path) => {
                    return match self.exact_name(&path).await {
                        NameMatch::Exact => Resolution::Path(path),
                        NameMatch::CaseOnly(actual) => Resolution::CaseMismatch { got: path, actual },
                        NameMatch::None => Resolution::Broken,
                    };
                }
                TitleMatch::Ambiguous(candidates) => {
                    return Resolution::AmbiguousAlias { name: link.target.clone(), candidates };
                }
                TitleMatch::Unknown => {}
            }
        }
        let resolved = link::resolve(source, &link.target);
        match self.exact_name(&resolved).await {
            NameMatch::Exact => Resolution::Path(resolved),
            NameMatch::CaseOnly(actual) => Resolution::CaseMismatch { got: resolved, actual },
            NameMatch::None => Resolution::Broken,
        }
    }

    /// How `path`'s final component matches its parent directory's listing:
    /// exactly, only case-insensitively (the portability hazard), or not at all.
    async fn exact_name(&self, path: &Path) -> NameMatch {
        let full = self.root().join(path);
        let (Some(parent), Some(name)) = (full.parent(), full.file_name()) else {
            return NameMatch::None;
        };
        let Ok(entries) = self.fs().read_dir(parent).await else {
            return NameMatch::None;
        };
        let mut case_only = None;
        for entry in entries {
            let Some(entry_name) = entry.file_name() else { continue };
            if entry_name == name {
                return NameMatch::Exact;
            }
            if entry_name.eq_ignore_ascii_case(name) {
                case_only = Some(entry_name.to_string_lossy().into_owned());
            }
        }
        match case_only {
            Some(actual) => NameMatch::CaseOnly(actual),
            None => NameMatch::None,
        }
    }
}

impl<FS: Storage, IdP: IdentityPolicy, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Apply a [`Fix`], editing the target document's metadata comment- and
    /// format-preservingly (through the same editor `set` uses). The back-link is
    /// authored through the workspace's link seam in the fixed relation's
    /// reference style — a path, an `id:<id>` link (registering the parent), or an
    /// alias — so a repair matches how it authors every other link.
    pub async fn apply_fix(&mut self, fix: &Fix) -> Result<()> {
        match fix {
            Fix::AddInverse { doc, relation, parent, title } => {
                // The parent exists (this repair points a child back at it), so an
                // id link registers it by path. Authored in `relation`'s style.
                let target = self.authored_target(relation, doc, parent, title, true).await?;
                let (text, parsed) = self.load(doc).await?;
                let updated =
                    crate::edit::set_in_text(&text, parsed.carrier, relation, fig::Value::Str(target))?;
                self.fs().write(&self.root().join(doc), updated.as_bytes()).await?;
            }
        }
        Ok(())
    }
}

enum NameMatch {
    Exact,
    CaseOnly(String),
    None,
}

// These tests use YAML frontmatter fixtures, so they run under the `yaml` feature.
#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::*;
    use crate::exec::block_on;
    use crate::fs::StdFs;
    use crate::identity::Minter;
    use crate::index::FileIndex;
    use crate::link::LinkStyle;

    fn write(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("colophon-check-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_clean_workspace_has_no_findings() {
        let dir = tempdir("clean");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n---\n");
        write(&dir, "a.md", "---\npart_of: index.md\n---\n");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        assert_eq!(block_on(ws.check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn broken_case_mismatched_and_uninversed_links_are_found() {
        let dir = tempdir("dirty");
        write(
            &dir,
            "index.md",
            "---\ncontents:\n- gone.md\n- '[D](docs/design.md)'\n- b.md\n---\n",
        );
        write(&dir, "docs/DESIGN.md", "---\npart_of: ../index.md\n---\n");
        write(&dir, "b.md", "---\ntitle: no part_of here\n---\n");

        let ws = Workspace::builder(StdFs).root(&dir).build();
        let findings = block_on(ws.check("index.md")).unwrap();
        assert!(
            findings.iter().any(|f| matches!(f, Finding::BrokenLink { target, .. } if target == "gone.md")),
            "{findings:?}"
        );
        assert!(
            findings.iter().any(|f| matches!(
                f,
                Finding::CaseMismatch { target, actual, .. } if target == "docs/design.md" && actual == "DESIGN.md"
            )),
            "{findings:?}"
        );
        assert!(
            findings.iter().any(|f| matches!(
                f,
                Finding::MissingInverse { child, .. } if child == &PathBuf::from("b.md")
            )),
            "{findings:?}"
        );
    }

    #[test]
    fn census_covers_frontmatter_edges_and_body_wikilinks() {
        let dir = tempdir("census");
        write(
            &dir,
            "index.md",
            "---\ncontents:\n- a.md\n---\nBody links [[a.md]] and [[gone.md]].\n",
        );
        write(&dir, "a.md", "---\npart_of: index.md\n---\n");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let census = block_on(ws.census("index.md")).unwrap();

        // The frontmatter `contents` edge, resolving to the existing file.
        assert!(
            census.iter().any(|e| matches!(&e.site, LinkSite::Relation(r) if r == "contents")
                && matches!(&e.resolution, Resolution::Path(p) if p == &PathBuf::from("a.md"))),
            "{census:?}"
        );
        // The body wikilink to the same file — sited in the body, resolving.
        assert!(
            census.iter().any(|e| matches!(e.site, LinkSite::Body(_))
                && e.target_text == "a.md"
                && matches!(&e.resolution, Resolution::Path(_))),
            "{census:?}"
        );
        // The body wikilink to a missing file — a Broken resolution.
        assert!(
            census.iter().any(|e| e.target_text == "gone.md"
                && matches!(e.resolution, Resolution::Broken)),
            "{census:?}"
        );
    }

    #[test]
    fn backlinks_invert_the_census_across_relations_and_body() {
        let dir = tempdir("backlinks");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n- b.md\n---\n");
        write(&dir, "a.md", "---\npart_of: index.md\n---\n");
        write(
            &dir,
            "b.md",
            "---\npart_of: index.md\nlinks:\n- a.md\n---\nSee [[a.md]] again.\n",
        );
        let ws = Workspace::builder(StdFs).root(&dir).build();

        // Who links to a.md? index.md (contents), b.md (links), b.md (body).
        let to_a = block_on(ws.backlinks_to("index.md", "a.md")).unwrap();
        assert_eq!(to_a.len(), 3, "{to_a:?}");
        assert!(
            to_a.iter().any(|bl| bl.source == Path::new("index.md")
                && matches!(&bl.site, LinkSite::Relation(r) if r == "contents")),
            "{to_a:?}"
        );
        assert!(
            to_a.iter().any(|bl| bl.source == Path::new("b.md")
                && matches!(&bl.site, LinkSite::Relation(r) if r == "links")),
            "{to_a:?}"
        );
        assert!(
            to_a.iter().any(|bl| bl.source == Path::new("b.md")
                && matches!(bl.site, LinkSite::Body(_))),
            "{to_a:?}"
        );
        // All path-form (this workspace has no registry / id links).
        assert!(to_a.iter().all(|bl| !bl.by_id), "{to_a:?}");

        // The full map keys targets by path; a.md is one of them.
        let map = block_on(ws.backlinks("index.md")).unwrap();
        assert_eq!(map[&PathBuf::from("a.md")].len(), 3);
    }

    #[test]
    fn check_flags_a_broken_body_wikilink() {
        let dir = tempdir("body-broken");
        write(&dir, "index.md", "---\ntitle: Root\n---\nSee [[gone.md]] for more.\n");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let findings = block_on(ws.check("index.md")).unwrap();
        assert!(
            findings.iter().any(|f| matches!(
                f,
                Finding::BrokenLink { site: LinkSite::Body(_), target, .. } if target == "gone.md"
            )),
            "{findings:?}"
        );
    }

    #[test]
    fn check_resolves_a_unique_alias_and_flags_an_ambiguous_one() {
        let dir = tempdir("alias-check");
        // Body aliases: `[[Alpha]]` is unique (clean), `[[Dup]]` is claimed by
        // two documents (ambiguous → a finding).
        write(&dir, "index.md", "---\ntitle: Root\n---\nSee [[Alpha]] and [[Dup]].\n");
        write(&dir, "alpha.md", "---\ntitle: Alpha\n---\n");
        write(&dir, "one.md", "---\ntitle: Dup\n---\n");
        write(&dir, "two.md", "---\ntitle: Dup\n---\n");
        let ws = Workspace::builder(StdFs).root(&dir).build();

        let findings = block_on(ws.check("index.md")).unwrap();
        // The unique alias produced no finding; the ambiguous one did.
        assert!(
            !findings.iter().any(|f| matches!(f, Finding::AmbiguousAlias { name, .. } if name == "Alpha")),
            "unique alias must resolve cleanly: {findings:?}"
        );
        assert!(
            findings.iter().any(|f| matches!(
                f,
                Finding::AmbiguousAlias { name, candidates, .. }
                    if name == "Dup" && candidates.len() == 2
            )),
            "ambiguous alias must be flagged: {findings:?}"
        );
    }

    // Real-world regression: a fenced code block containing Python list
    // comprehensions (`[[float('inf')] * width ...]`) must never be mistaken
    // for a `[[…]]` wikilink — DESIGN §8's motivating example, life-sized.
    #[test]
    fn check_does_not_flag_python_list_comprehensions_in_a_code_block_as_broken_links() {
        let dir = tempdir("code-brackets");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\n---\n\n\
             ```python\n\
             dp_matrix = [[float('inf')] * width for _ in range(m + 1)]\n\
             ptr_matrix = [[None] * width for _ in range(m + 1)]\n\
             ```\n\n\
             See [[gone.md]] for the real broken link.\n",
        );
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let findings = block_on(ws.check("index.md")).unwrap();

        let broken: Vec<_> =
            findings.iter().filter(|f| matches!(f, Finding::BrokenLink { .. })).collect();
        assert_eq!(broken.len(), 1, "{findings:?}");
        assert!(matches!(broken[0], Finding::BrokenLink { target, .. } if target == "gone.md"));
    }

    #[test]
    fn a_resolving_body_wikilink_is_not_a_finding() {
        let dir = tempdir("body-clean");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n---\nSee [[a.md]].\n");
        write(&dir, "a.md", "---\npart_of: index.md\n---\n");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        assert_eq!(block_on(ws.check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn angle_bracketed_and_absolute_links_resolve_in_the_graph() {
        // The Adam's-Archive shape: the root links a spaced child by an
        // angle-bracketed, workspace-absolute path, and the child points back
        // by an absolute path. Everything must resolve — no missing/broken.
        let dir = tempdir("archive-links");
        write(&dir, "index.md", "---\ncontents:\n- '[Notes](</My Notes/notes.md>)'\n---\n");
        write(&dir, "My Notes/notes.md", "---\npart_of: /index.md\n---\n");
        let ws = Workspace::builder(StdFs).root(&dir).build();

        // The child resolves (the tree would show it, not "(missing)").
        let census = block_on(ws.census("index.md")).unwrap();
        assert!(
            census.iter().any(|e| matches!(&e.resolution,
                Resolution::Path(p) if p == &PathBuf::from("My Notes/notes.md"))),
            "{census:?}"
        );
        // And the whole graph validates: absolute inverse links back cleanly.
        assert_eq!(block_on(ws.check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn suggests_and_applies_a_missing_inverse_fix() {
        let dir = tempdir("autofix");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n---\n");
        write(&dir, "a.md", "---\ntitle: A\n---\n"); // no part_of → MissingInverse
        // Plain-canonical style keeps the assertion about the fix simple.
        let mut ws = Workspace::builder(StdFs).root(&dir).link_style(LinkStyle::PlainCanonical).build();

        let findings = block_on(ws.check("index.md")).unwrap();
        let mi = findings.iter().find(|f| matches!(f, Finding::MissingInverse { .. })).unwrap();
        let fix = block_on(ws.suggest_fix(mi)).unwrap().expect("safely fixable");
        assert!(
            matches!(&fix, Fix::AddInverse { doc, relation, parent, .. }
                if doc == &PathBuf::from("a.md") && relation == "part_of"
                    && parent == &PathBuf::from("index.md")),
            "{fix:?}"
        );

        block_on(ws.apply_fix(&fix)).unwrap();
        // a.md now declares the back-link (plain-canonical), and it validates.
        assert!(std::fs::read_to_string(dir.join("a.md")).unwrap().contains("part_of: index.md"));
        assert_eq!(block_on(ws.check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn autofix_matches_the_workspace_link_style() {
        // The Adam's-Archive concern: the repair must be written in the
        // workspace's declared style (markdown-root, titled with the parent's
        // own title) — never a bare fifth style colophon invented.
        let dir = tempdir("autofix-style");
        write(&dir, "index.md", "---\ntitle: Home\ncontents:\n- '[A](/a.md)'\n---\n");
        write(&dir, "a.md", "---\ntitle: A\n---\n");
        let mut ws = Workspace::builder(StdFs).root(&dir).link_style(LinkStyle::MarkdownRoot).build();

        let findings = block_on(ws.check("index.md")).unwrap();
        let mi = findings
            .iter()
            .find(|f| matches!(f, Finding::MissingInverse { .. }))
            .unwrap()
            .clone();
        let fix = block_on(ws.suggest_fix(&mi)).unwrap().unwrap();
        block_on(ws.apply_fix(&fix)).unwrap();
        // Applied in the workspace's markdown-root style, titled with the
        // parent's own title.
        assert!(
            std::fs::read_to_string(dir.join("a.md")).unwrap().contains("[Home](/index.md)"),
            "{:?}",
            std::fs::read_to_string(dir.join("a.md"))
        );
    }

    #[test]
    fn autofix_authors_an_id_link_when_configured() {
        // Obsidian-style: the repair is authored by id (registering the parent),
        // so it survives a later move untouched.
        let dir = tempdir("autofix-id");
        write(&dir, "index.md", "---\ntitle: Home\ncontents:\n- a.md\n---\n");
        write(&dir, "a.md", "---\ntitle: A\n---\n");
        let mut ws = Workspace::builder(StdFs)
            .root(&dir)
            .identity(Minter::lazy(9))
            .index(FileIndex::new(fig::Format::Yaml))
            .id_links(true)
            .build();

        let findings = block_on(ws.check("index.md")).unwrap();
        let mi = findings
            .iter()
            .find(|f| matches!(f, Finding::MissingInverse { .. }))
            .unwrap()
            .clone();
        let fix = block_on(ws.suggest_fix(&mi)).unwrap().unwrap();
        block_on(ws.apply_fix(&fix)).unwrap();

        let parent_id = ws.index().id_for_path(Path::new("index.md")).expect("parent registered");
        assert!(
            std::fs::read_to_string(dir.join("a.md"))
                .unwrap()
                .contains(&format!("part_of: id:{parent_id}"))
        );
    }

    #[test]
    fn a_contested_parent_is_not_auto_fixed() {
        // index claims a.md, but a.md already claims a *different* parent — a
        // contested containment, not a mechanical missing-inverse. Left to a
        // human (suggest_fix declines), so autofix never overwrites intent.
        let dir = tempdir("autofix-contested");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n---\n");
        write(&dir, "other.md", "---\ntitle: Other\n---\n");
        write(&dir, "a.md", "---\npart_of: other.md\n---\n");
        let ws = Workspace::builder(StdFs).root(&dir).build();

        let findings = block_on(ws.check("index.md")).unwrap();
        let mi = findings.iter().find(|f| matches!(f, Finding::MissingInverse { .. })).unwrap();
        assert!(block_on(ws.suggest_fix(mi)).unwrap().is_none(), "contested → not auto-fixed");
    }

    #[test]
    fn body_link_findings_are_never_auto_fixed() {
        // The code-block-false-positive guard: a broken *body* wikilink is
        // diagnosis only — autofix must not offer to edit prose.
        let dir = tempdir("autofix-body");
        // A nested list comprehension: `[[…]]` that is code, not a wikilink.
        write(&dir, "index.md", "---\ntitle: Root\n---\ndp = [[inf] * n for _ in range(m)]]\n");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let findings = block_on(ws.check("index.md")).unwrap();
        let broken = findings
            .iter()
            .find(|f| matches!(f, Finding::BrokenLink { site: LinkSite::Body(_), .. }))
            .expect("the code fragment scanned as a broken body link");
        assert!(block_on(ws.suggest_fix(broken)).unwrap().is_none());
    }

    #[test]
    fn duplicate_containment_is_found() {
        let dir = tempdir("dup");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n- b.md\n---\n");
        write(&dir, "a.md", "---\npart_of: index.md\ncontents:\n- b.md\n---\n");
        write(&dir, "b.md", "---\npart_of: index.md\n---\n");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let findings = block_on(ws.check("index.md")).unwrap();
        assert!(
            findings.iter().any(|f| matches!(f, Finding::DuplicateContainment { .. })),
            "{findings:?}"
        );
    }
}
