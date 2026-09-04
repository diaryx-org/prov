//! The census types, the spanning-tree walker that fills them in, and the
//! reachability views built over the result. See the module doc at
//! [`crate::graph`] for why the census is ground truth.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::Graph;
use crate::error::Result;
use crate::fs::ReadStorage;
use crate::identity::{self, Id};
use crate::index::IdIndex;
use crate::link::{self, Link};
use crate::memo::DirNames;
use crate::title::{self, TitleIndex, TitleMatch};

use super::Target;

/// Where in a document a forward link is written — a frontmatter relation
/// field or a body wikilink. Carried by every link-resolution finding
/// (`prov`'s `Finding`, derived in `validate` — see
/// [`StructuralFact`]) and every [`CensusEntry`] so a report can point at the
/// exact site.
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
    /// A `prov:<id>` target the registry resolves to the live path `to`.
    Id { id: Id, to: PathBuf },
    /// A well-formed `prov:<id>` target with no live registry entry;
    /// `tombstoned` separates "deleted" from "never issued here" (§4 hazard).
    DanglingId { id: Id, tombstoned: bool },
    /// A `prov:<id>` target failing its check character — a typo.
    MalformedId,
    /// A nominal (alias) target several documents claim — unresolvable.
    /// `candidates` are the sharers, sorted.
    AmbiguousAlias {
        name: String,
        candidates: Vec<PathBuf>,
    },
    /// A URL / mail address — off-workspace, never resolved or rewritten.
    External,
    /// A target that is *only* a locator (`#3`) — a place inside the document
    /// the link is written in.
    ///
    /// A clean resolution, not a finding, on the same grounds as any other
    /// locator: prov does not read a document's internal address space, so it
    /// has no evidence about whether `#3` names anything. See
    /// [`Target::SameDocument`] for why this is its own case rather than a
    /// [`Resolution::Path`] of the citing document.
    SameDocument,
    /// An `id:<workspace>/<id>` target naming a document in another workspace.
    ///
    /// A clean resolution, not a finding: prov holds no map from a workspace
    /// name to a location (see
    /// [`Target::Foreign`]), so it has no
    /// evidence either way about whether the target exists. Reporting a link it
    /// cannot check as broken would be a false positive every host would then
    /// have to suppress — and a `check` that must be filtered is one nobody
    /// reads. The id is deliberately **not** check-verified: the foreign
    /// workspace owns its id space and need not be a prov workspace.
    Foreign { workspace: String, id: Id },
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
/// resolves. The unit of the
/// [`census`](Graph::census).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CensusEntry {
    /// The document that declares the link (workspace-relative).
    pub source: PathBuf,
    /// Where in `source` the link is written.
    pub site: LinkSite,
    /// The target exactly as written (bare — the `[label](…)` wrapper stripped).
    pub target_text: String,
    /// The display label the link carried, when written `[label](target)` /
    /// `[[target|label]]` — `None` for a bare target. Kept so a caller can check
    /// a label against the target's current title (stale-label detection) without
    /// re-reading the source.
    pub label: Option<String>,
    /// How the target resolves.
    pub resolution: Resolution,
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
    /// `true` when the link is a `prov:<id>` reference (location-independent),
    /// `false` when it is a path.
    pub by_id: bool,
}

enum NameMatch {
    Exact,
    CaseOnly(String),
    None,
}

/// A structural observation the walk makes as it traverses — not a verdict,
/// just what it saw: a document that would not load, a self-stored id
/// disagreeing with (or absent from) the registry, a spanning edge that
/// revisits an already-reached node, a spanning child whose inverse field
/// does not point back, or a `content` pointer that failed to resolve.
///
/// These are facts about *traversal state* — they need the queue, the
/// visited set, the inverse lookup — so only the walk can raise them; a
/// single [`CensusEntry`]'s [`Resolution`] is not enough (that half of the
/// story is `validate`'s [`CensusEntry`]-keyed
/// `prov`'s `validate` instead, since a resolution *is* already the
/// fact). `validate::check` turns each variant here into the
/// `prov`'s `Finding` that names it — one to one, since
/// the walk already knows exactly what happened and there is nothing left to
/// infer. Keeping the enum here rather than importing `Finding` is what
/// keeps `graph` a pure "plain text → walkable graph" layer: it reports what
/// it found, never how that should be judged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructuralFact {
    /// A document that exists but could not be read or parsed.
    Unreadable { doc: PathBuf, error: String },
    /// A document's self-stored `id` frontmatter disagrees with the registry
    /// (or claims an id the registry hands to a different document).
    /// `registry` is `None` when the registry has no record of the path at
    /// all under this id.
    IdMismatch {
        doc: PathBuf,
        frontmatter: Id,
        registry: Option<Id>,
    },
    /// A document carries a self-stored `id` the registry has no record of.
    UnregisteredId { doc: PathBuf, frontmatter: Id },
    /// A stamping workspace's registered document does not carry its own
    /// `id` frontmatter.
    UnstampedId { doc: PathBuf, registry: Id },
    /// A spanning target already reached by the walk — a cycle or a second
    /// parent.
    DuplicateContainment { doc: PathBuf, target: String },
    /// A spanning child whose inverse field does not link back to `doc`.
    MissingInverse {
        doc: PathBuf,
        child: PathBuf,
        inverse: String,
    },
    /// A `content` pointer resolving only case-insensitively.
    CaseMismatch {
        doc: PathBuf,
        site: LinkSite,
        target: String,
        actual: String,
    },
    /// A `content` pointer resolving to nothing on disk.
    BrokenLink {
        doc: PathBuf,
        site: LinkSite,
        target: String,
    },
    /// A node declaring both `content` and `manifest` — a sidecar for one
    /// payload and for a whole directory at once. The two are mutually
    /// exclusive ([`crate::manifest`]), and neither reading is safe to pick.
    ManifestConflict { doc: PathBuf },
}

/// The result of one spanning-tree
/// [`walk`](Graph::census): the forward-link census,
/// the structural facts observed from traversal state, and the prose body
/// files reached through separated nodes' `content` pointers (tracked for
/// the orphan check, deliberately absent from the census).
pub struct Walk {
    pub census: Vec<CensusEntry>,
    pub facts: Vec<StructuralFact>,
    pub content_bodies: Vec<PathBuf>,
}

/// The set of workspace-relative paths a walk from `start` reaches: `start`
/// itself, every path a census link resolves to (any relation, a body wikilink,
/// or an id through the registry), and every `content` target.
///
/// A **case-mismatched** link counts its *actual* on-disk file as reached, so a
/// file is never both case-mismatched and orphaned. Prose bodies (and attachment
/// payloads) arrive through `content_bodies` rather than the census, because a
/// `content` pointer is not a graph edge — but it does reach a file, which is
/// what every caller here cares about.
///
/// The one definition of "reachable" that the orphan check, the fixity pass, the
/// vocabulary pass, and the history capture set all share (DESIGN §8).
pub fn reachable_set(
    start: &Path,
    census: &[CensusEntry],
    content_bodies: &[PathBuf],
) -> BTreeSet<PathBuf> {
    let mut reachable: BTreeSet<PathBuf> = BTreeSet::new();
    reachable.insert(link::normalize(start));
    reachable.extend(content_bodies.iter().cloned());
    for entry in census {
        match &entry.resolution {
            Resolution::Path(p) | Resolution::Id { to: p, .. } => {
                reachable.insert(p.clone());
            }
            Resolution::CaseMismatch { got, actual } => {
                reachable.insert(got.with_file_name(actual));
            }
            _ => {}
        }
    }
    reachable
}

impl<FS: ReadStorage, Ix: IdIndex> Graph<FS, Ix> {
    /// [`reachable_set`], minus any **shadowed attachment payload**
    /// (`attach --opaque`) — the population a pass may parse *as a document*.
    ///
    /// A shadowed payload is still reachable (it must not be reported as an
    /// orphan, and it is still fixity-checked *through its sidecar*), but its
    /// bytes are an exhibit prov promised never to interpret. That is the same
    /// bound [`is_shadowed_payload`](Graph::is_shadowed_payload) already
    /// holds the flat title and id scans to; this is its reachability-walk
    /// counterpart, for `prov`'s `vocabulary_findings` and
    /// `prov`'s `fixity_findings` — the two passes that load
    /// every reachable path and read its frontmatter.
    ///
    /// The listing `is_shadowed_payload` needs is built the same way
    /// `prov`'s `orphans` builds one: the direct children of every
    /// directory the reachable set occupies, so a shadow check costs a set
    /// lookup per candidate extension rather than a stat.
    pub async fn reachable_documents(
        &self,
        start: &Path,
        census: &[CensusEntry],
        content_bodies: &[PathBuf],
    ) -> Result<BTreeSet<PathBuf>> {
        let reachable = reachable_set(start, census, content_bodies);
        let reached_dirs = Self::reached_dirs(&reachable);
        let probe = super::ShadowProbe::over(self.direct_child_files(&reached_dirs).await?.iter());
        let mut documents = BTreeSet::new();
        for path in reachable {
            if !self.is_shadowed_payload(&path, &probe).await {
                documents.insert(path);
            }
        }
        Ok(documents)
    }

    /// Every file the workspace reaches from `start` that actually exists on
    /// disk — [`reachable_set`] over a fresh walk, filtered to real files.
    ///
    /// This is §8's bounded walk expressed as a *file set* rather than a findings
    /// list: the same population `check` validates. `prov`'s `Workspace::ignore_list`
    /// subtracts it from a top-down walk of the folder to say what is *not* the
    /// workspace — so the two answers come from one definition of what the
    /// workspace considers its own, rather than two that can disagree.
    ///
    /// [`reachable_files_within`](Self::reachable_files_within) is the same walk
    /// bounded away from directories prov parks its own bytes in.
    pub async fn reachable_files(&self, start: impl AsRef<Path>) -> Result<BTreeSet<PathBuf>> {
        self.reachable_files_within(start, &[]).await
    }

    /// [`reachable_files`](Self::reachable_files), told which directories are
    /// parked — see [`title_index_scoped`](Self::title_index_scoped).
    pub async fn reachable_files_within(
        &self,
        start: impl AsRef<Path>,
        parked: &[PathBuf],
    ) -> Result<BTreeSet<PathBuf>> {
        let start = link::normalize(start);
        let Walk {
            census,
            content_bodies,
            ..
        } = self.walk(&start, parked).await?;
        let mut files = BTreeSet::new();
        for path in reachable_set(&start, &census, &content_bodies) {
            if self.fs().try_exists(&self.root().join(&path)).await? {
                files.insert(path);
            }
        }
        Ok(files)
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
        self.census_within(start, &[]).await
    }

    /// [`census`](Self::census), told which directories are parked — see
    /// [`title_index_scoped`](Self::title_index_scoped).
    pub async fn census_within(
        &self,
        start: impl AsRef<Path>,
        parked: &[PathBuf],
    ) -> Result<Vec<CensusEntry>> {
        Ok(self.walk(start.as_ref(), parked).await?.census)
    }

    /// The backlink map for the workspace reachable from `start`: every resolved
    /// target to the inbound references ([`Backlink`]s) that reach it, path- and
    /// id-form alike. This is the census inverted — recomputed from the
    /// documents, so it is always fresh (the Route-N "reconcile-on-load": no
    /// stored index to drift). Each target's backlinks are sorted by source.
    pub async fn backlinks(
        &self,
        start: impl AsRef<Path>,
    ) -> Result<BTreeMap<PathBuf, Vec<Backlink>>> {
        Ok(invert(self.census(start).await?))
    }

    /// The inbound references to a single `target` (workspace-relative) reachable
    /// from `start`, sorted by source. The focused form of
    /// [`backlinks`](Self::backlinks) for "who links here?".
    pub async fn backlinks_to(
        &self,
        start: impl AsRef<Path>,
        target: impl AsRef<Path>,
    ) -> Result<Vec<Backlink>> {
        Ok(inbound(self.census(start).await?, target.as_ref()))
    }

    /// The shared spanning-tree walk: gathers the forward-link census and the
    /// structural facts ([`StructuralFact`], which depend on traversal state,
    /// not on a single link's resolution) in one pass. Frontmatter edges may
    /// be spanning and so drive descent, the single-parent check, and the
    /// inverse check; body wikilinks are always overlay references —
    /// censused, never spanning.
    ///
    /// "One pass" describes what it *reports*, not how many times it opens a
    /// file: descent reads each document, the inverse check reads every spanning
    /// child again to see whether it points back, and a workspace using
    /// `[[alias]]` links pays a third read per document for the title index.
    /// Three reads of everything, for one walk. So the walk opens a scope of its
    /// own rather than waiting to be given one — a caller with no interest in
    /// memos still gets a walk that reads each document once, and a caller that
    /// already opened one (`check`, a `mutate` verb) nests inside it and keeps
    /// everything the walk read.
    pub async fn walk(&self, start: &Path, parked: &[PathBuf]) -> Result<Walk> {
        let _scope = self.read_scope();
        let mut census = Vec::new();
        let mut structural = Vec::new();
        // Prose bodies reached through a separated node's `content` pointer.
        // Kept out of the census (not a graph edge), but tracked so the orphan
        // check does not mistake a linked body file for an unlinked one.
        let mut content_bodies = Vec::new();
        let mut visited = BTreeSet::new();
        let mut queue = vec![link::normalize(start)];

        // The nominal-resolution index, built lazily — only if a `[[alias]]` link
        // is actually encountered. A path/id workspace never scans (which, at the
        // root of a larger repo, would read every file under `target/`, vendored
        // trees, and the rest — the reported multi-second `tree`/`check`).
        let mut titles: Option<TitleIndex> = None;

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
                    structural.push(StructuralFact::Unreadable {
                        doc: path,
                        error: e.to_string(),
                    });
                    continue;
                }
            };
            let meta = fig::Value::from(&doc.meta);

            // Reconcile a self-stored `id` against the registry (frontmatter
            // storage, DESIGN §5). Three outcomes when a document carries its own
            // `id`: the registry agrees (nothing to do); the registry records a
            // *different* id for this path, or hands this id to another document
            // (`IdMismatch` — a drift); or the registry has never heard of the id
            // (`UnregisteredId` — the shadow got ahead of the cache).
            if let Some(fm) = meta.get("id").and_then(fig::Value::as_str)
                && !fm.trim().is_empty()
            {
                let fm = Id(fm.trim().to_string());
                match self.index().id_for_path(&path) {
                    Some(reg) if reg != fm => structural.push(StructuralFact::IdMismatch {
                        doc: path.clone(),
                        frontmatter: fm,
                        registry: Some(reg),
                    }),
                    Some(_) => {} // the registry agrees with the frontmatter
                    None => match self.index().resolve(&fm) {
                        // The id is live, but points at a *different* document.
                        Some(other) if other != path => {
                            structural.push(StructuralFact::IdMismatch {
                                doc: path.clone(),
                                frontmatter: fm,
                                registry: None,
                            })
                        }
                        // resolve == this path but no reverse entry: consistent.
                        Some(_) => {}
                        // The registry has no record of this id at all.
                        None => structural.push(StructuralFact::UnregisteredId {
                            doc: path.clone(),
                            frontmatter: fm,
                        }),
                    },
                }
            } else if self.id_storage().stamps_frontmatter()
                && let Some(reg) = self.index().id_for_path(&path)
            {
                // The other direction: a stamping workspace expects every
                // registered document to carry its own id, and this one does not
                // (a workspace converted from registry-only storage, or an `id`
                // stripped out of band). The registry is the authority — the id
                // is already live and linked to — so the repair writes it down.
                structural.push(StructuralFact::UnstampedId {
                    doc: path.clone(),
                    registry: reg,
                });
            }

            // Frontmatter relation edges — the only links that can be spanning.
            for edge in self.relations().edges(&meta) {
                // Parse once: `link.target` is the bare target (any `[label](…)`
                // stripped), which is what both the census and findings record.
                let link = Link::parse(&edge.target);
                if titles.is_none() && title::is_alias_shaped(&link.target) {
                    titles = Some(self.title_index_scoped(start, parked).await?);
                }
                let resolution = self.resolve_forward(&path, &link, titles.as_ref()).await;

                if Some(edge.relation.as_str()) == spanning.as_deref()
                    && let Some(resolved) = resolution.resolved_path().cloned()
                {
                    // Single-parent check, inverse check, descent.
                    if visited.contains(&resolved) || queue.contains(&resolved) {
                        structural.push(StructuralFact::DuplicateContainment {
                            doc: path.clone(),
                            target: link.target.clone(),
                        });
                    } else {
                        if let Some(inverse) = inverse.as_deref()
                            && let Ok((_, child_doc)) = self.load(&resolved).await
                            && child_doc.has_meta()
                        {
                            let child_meta = fig::Value::from(&child_doc.meta);
                            let inverse_targets = child_meta
                                .get(inverse)
                                .map(crate::meta::link_strings)
                                .unwrap_or_default();
                            // Build the title index if a nominal inverse link needs it.
                            if titles.is_none()
                                && inverse_targets
                                    .iter()
                                    .any(|t| title::is_alias_shaped(&Link::parse(t).target))
                            {
                                titles = Some(self.title_index_scoped(start, parked).await?);
                            }
                            let points_back = inverse_targets.iter().any(|t| {
                                self.resolve_link_with(&resolved, &Link::parse(t), titles.as_ref())
                                    == Target::Path(path.clone())
                            });
                            if !points_back {
                                structural.push(StructuralFact::MissingInverse {
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
                    label: link.label,
                    target_text: link.target,
                    resolution,
                });
            }

            // Body links — `[[wikilinks]]` and markdown/djot `[t](a)` links
            // alike — overlay references, censused but never spanning.
            for body_link in link::scan_body_links(&path, &doc.body) {
                let wl = body_link.link;
                if titles.is_none() && title::is_alias_shaped(&wl.target) {
                    titles = Some(self.title_index_scoped(start, parked).await?);
                }
                let resolution = self.resolve_forward(&path, &wl, titles.as_ref()).await;
                census.push(CensusEntry {
                    source: path.clone(),
                    site: LinkSite::Body(body_link.span),
                    label: wl.label,
                    target_text: wl.target,
                    resolution,
                });
            }

            // A separated document's `content` must resolve to an existing body
            // file. Validated here (not a graph edge, so kept out of the census).
            if let Some(content) = doc.content_attr() {
                let target = link::resolve(&path, content);
                let site = LinkSite::Relation("content".to_string());
                match self.exact_name(&target).await {
                    NameMatch::Exact => content_bodies.push(target),
                    NameMatch::CaseOnly(actual) => {
                        // The linked body exists under a different case: record its
                        // real name as reached (so it is not also an orphan), and
                        // still flag the portability hazard.
                        content_bodies.push(target.with_file_name(&actual));
                        structural.push(StructuralFact::CaseMismatch {
                            doc: path.clone(),
                            site,
                            target: content.to_string(),
                            actual,
                        });
                    }
                    NameMatch::None => structural.push(StructuralFact::BrokenLink {
                        doc: path.clone(),
                        site,
                        target: content.to_string(),
                    }),
                }
            }

            // A manifest node's `manifest` must resolve to an existing document,
            // the same way and for the same reason: it is not a graph edge (the
            // manifest is machinery, carrying no `part_of` and no id), but it
            // does reach a file, so the orphan pass must count it as reached.
            //
            // The rows *inside* it reach files too, and deliberately do not
            // arrive here. A covered file is opaque bytes — never a content
            // document, so never an orphan candidate — and adding ten thousand
            // of them to every walk's reachable set would make a photo archive
            // pay for a check none of those files can fail. What the manifest
            // promises about them is `check`'s manifest pass, once, not the
            // census's, per document.
            if let Some(manifest) = doc.manifest_attr() {
                if doc.content_attr().is_some() {
                    structural.push(StructuralFact::ManifestConflict { doc: path.clone() });
                }
                let target = link::resolve(&path, manifest);
                let site = LinkSite::Relation(crate::manifest::MANIFEST_KEY.to_string());
                match self.exact_name(&target).await {
                    NameMatch::Exact => content_bodies.push(target),
                    NameMatch::CaseOnly(actual) => {
                        content_bodies.push(target.with_file_name(&actual));
                        structural.push(StructuralFact::CaseMismatch {
                            doc: path.clone(),
                            site,
                            target: manifest.to_string(),
                            actual,
                        });
                    }
                    NameMatch::None => structural.push(StructuralFact::BrokenLink {
                        doc: path.clone(),
                        site,
                        target: manifest.to_string(),
                    }),
                }
            }
        }
        Ok(Walk {
            census,
            facts: structural,
            content_bodies,
        })
    }

    /// Resolve one forward link (declared in the document at `source`) into a
    /// [`Resolution`]. A path target is checked against the on-disk name; an
    /// `id:<id>` target resolves through the registry and stays an id-form
    /// resolution; an `id:<workspace>/<id>` target naming another workspace
    /// stops at [`Resolution::Foreign`]; a nominal (`[[My File]]`) target
    /// resolves through `titles` — `Unique` to the on-disk path, `Ambiguous` to
    /// [`Resolution::AmbiguousAlias`], `Unknown` falling through to a path (so a
    /// nominal link to nothing reports as `Broken`, like any dead link).
    async fn resolve_forward(
        &self,
        source: &Path,
        link: &Link,
        titles: Option<&TitleIndex>,
    ) -> Resolution {
        if link.is_external() {
            return Resolution::External;
        }
        if link.is_same_document() {
            return Resolution::SameDocument;
        }
        // Mirrors `Workspace::resolve_link_with`: a reference qualified with
        // this workspace's own name is local, any other qualifier is foreign,
        // and a malformed `id:` body is a broken id rather than a filename that
        // happens to contain a colon.
        let local_id = match link.id_ref() {
            Some(crate::link::IdRef::Local(id)) => Some(id),
            Some(crate::link::IdRef::Foreign { workspace, id }) => {
                if self.workspace_id().is_empty() || workspace != self.workspace_id() {
                    return Resolution::Foreign { workspace, id };
                }
                Some(id)
            }
            Some(crate::link::IdRef::Malformed) => return Resolution::MalformedId,
            None => None,
        };
        if let Some(id) = local_id {
            if !identity::verify(id.as_str()) {
                return Resolution::MalformedId;
            }
            return match self.index().resolve(&id) {
                Some(path) => Resolution::Id {
                    id,
                    to: link::normalize(path),
                },
                None => Resolution::DanglingId {
                    tombstoned: self.index().is_known(&id),
                    id,
                },
            };
        }
        // Only a nominal link needs the title index; the caller builds it lazily
        // the first time one appears, so `titles` is `Some` here whenever it is
        // consulted. If absent, fall through to path resolution.
        //
        // The *addressed* target, not the whole one — the same care
        // `resolve_link_with` takes: a locator names a place inside the document
        // an alias names, so `[[My File#v2]]` is the nominal reference
        // `[[My File]]`. Asking the index for the spelling with the locator
        // still on it misses every time and falls through to the path branch,
        // which then reports a live document as a broken link.
        let addressed = link.addressed_target();
        if let Some(titles) = titles.filter(|_| title::is_alias_shaped(addressed)) {
            match titles.resolve(addressed) {
                TitleMatch::Unique(path) => {
                    return match self.exact_name(&path).await {
                        NameMatch::Exact => Resolution::Path(path),
                        NameMatch::CaseOnly(actual) => {
                            Resolution::CaseMismatch { got: path, actual }
                        }
                        NameMatch::None => Resolution::Broken,
                    };
                }
                TitleMatch::Ambiguous(candidates) => {
                    return Resolution::AmbiguousAlias {
                        name: link.target.clone(),
                        candidates,
                    };
                }
                TitleMatch::Unknown => {}
            }
        }
        let resolved = link::resolve(source, &link.target);
        match self.exact_name(&resolved).await {
            NameMatch::Exact => Resolution::Path(resolved),
            NameMatch::CaseOnly(actual) => Resolution::CaseMismatch {
                got: resolved,
                actual,
            },
            NameMatch::None => Resolution::Broken,
        }
    }

    /// How `path`'s final component matches its parent directory's listing:
    /// exactly, only case-insensitively (the portability hazard), or not at all.
    ///
    /// Answered from the read scope's directory memo where there is one
    /// ([`crate::memo`]). This runs once per link resolved, and a workspace's
    /// links point overwhelmingly at directories the same walk has already
    /// asked about — without the memo, a flat workspace of N documents reads
    /// one directory of N entries N times over, which is where `check`'s cost
    /// went quadratic. Outside a scope it is the plain directory read it always
    /// was.
    async fn exact_name(&self, path: &Path) -> NameMatch {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return NameMatch::None;
        };
        let names = match self.memo_dir(parent) {
            Some(hit) => hit,
            None => {
                let Ok(entries) = self.fs().read_dir(&self.root().join(parent)).await else {
                    return NameMatch::None;
                };
                let names = Arc::new(DirNames::index(&entries));
                self.memo_remember_dir(parent, Arc::clone(&names));
                names
            }
        };
        if names.holds(name) {
            return NameMatch::Exact;
        }
        match names.case_variant(name) {
            Some(actual) => NameMatch::CaseOnly(actual.to_string_lossy().into_owned()),
            None => NameMatch::None,
        }
    }
}

// These tests use YAML frontmatter fixtures, so they run under the `yaml` feature.
#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::*;
    use crate::exec::block_on;
    use crate::fs::StdFs;
    use crate::graph::ReadSettings;
    use crate::index::NoIndex;

    use prov_testkit::write;
    fn tempdir(tag: &str) -> PathBuf {
        prov_testkit::scratch("census", tag)
    }

    /// A [`StdFs`] that counts its directory reads — the observable behind the
    /// claim that resolution asks a directory about itself once per operation
    /// and not once per link.
    #[derive(Debug, Default)]
    struct CountingFs {
        reads: std::cell::Cell<usize>,
    }

    impl CountingFs {
        fn dir_reads(&self) -> usize {
            self.reads.get()
        }
    }

    impl ReadStorage for CountingFs {
        async fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
            StdFs.read(path).await
        }
        async fn read_to_string(&self, path: &Path) -> std::io::Result<String> {
            StdFs.read_to_string(path).await
        }
        async fn read_dir(&self, path: &Path) -> std::io::Result<Vec<crate::fs::DirEntry>> {
            self.reads.set(self.reads.get() + 1);
            StdFs.read_dir(path).await
        }
        async fn metadata(&self, path: &Path) -> std::io::Result<crate::fs::Metadata> {
            StdFs.metadata(path).await
        }
    }

    /// The regression this memo exists for. Every link resolved asks its target's
    /// parent directory whether the name is there, so without a memo a workspace
    /// of N documents in one directory reads that directory ~N times — the
    /// quadratic term that made `check` unusable on a few thousand documents.
    /// One directory, one read.
    #[test]
    fn a_walk_reads_each_directory_once_however_many_links_point_into_it() {
        let dir = tempdir("dir-memo");
        let children: Vec<String> = (0..24).map(|i| format!("n{i}.md")).collect();
        let contents = children
            .iter()
            .map(|c| format!("- {c}"))
            .collect::<Vec<_>>()
            .join("\n");
        write(
            &dir,
            "index.md",
            format!("---\ncontents:\n{contents}\n---\n"),
        );
        for child in &children {
            write(&dir, child, "---\npart_of: index.md\n---\n");
        }

        let ws = Graph::new(
            CountingFs::default(),
            &dir,
            NoIndex,
            ReadSettings::default(),
        );
        let census = block_on(ws.census("index.md")).unwrap();
        assert_eq!(census.len(), 48, "24 children, each edge and its inverse");
        assert_eq!(
            ws.fs().dir_reads(),
            1,
            "48 links into one directory should cost one listing, not 48"
        );
    }

    /// The memo is bounded by a scope, and `census` opens its own — so a second
    /// census sees the directory as it stands now, not as the first one found it.
    #[test]
    fn a_directory_read_does_not_outlive_the_operation_that_made_it() {
        let dir = tempdir("dir-memo-scope");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n---\n");

        let ws = Graph::new(StdFs, &dir, NoIndex, ReadSettings::default());
        let before = block_on(ws.census("index.md")).unwrap();
        assert!(
            before
                .iter()
                .any(|e| matches!(e.resolution, Resolution::Broken)),
            "a.md is not there yet: {before:?}"
        );

        write(&dir, "a.md", "---\npart_of: index.md\n---\n");
        let after = block_on(ws.census("index.md")).unwrap();
        assert!(
            after
                .iter()
                .all(|e| !matches!(e.resolution, Resolution::Broken)),
            "the second census resolved against a stale listing: {after:?}"
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
        let ws = Graph::new(StdFs, &dir, NoIndex, ReadSettings::default());
        let census = block_on(ws.census("index.md")).unwrap();

        // The frontmatter `contents` edge, resolving to the existing file.
        assert!(
            census.iter().any(
                |e| matches!(&e.site, LinkSite::Relation(r) if r == "contents")
                    && matches!(&e.resolution, Resolution::Path(p) if p == &PathBuf::from("a.md"))
            ),
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
            census
                .iter()
                .any(|e| e.target_text == "gone.md" && matches!(e.resolution, Resolution::Broken)),
            "{census:?}"
        );
    }

    #[test]
    fn a_same_document_anchor_is_a_clean_resolution_not_a_broken_link() {
        let dir = tempdir("anchor");
        write(
            &dir,
            "index.md",
            "---\ncontents:\n- a.md\n---\n## Section One\n\nSee [Section One](#section-one).\n",
        );
        write(&dir, "a.md", "---\npart_of: index.md\n---\n");
        let ws = Graph::new(StdFs, &dir, NoIndex, ReadSettings::default());
        let census = block_on(ws.census("index.md")).unwrap();

        let anchor = census
            .iter()
            .find(|e| e.target_text == "#section-one")
            .expect("the anchor is still a link the census reports");
        assert_eq!(anchor.resolution, Resolution::SameDocument, "{census:?}");
        // And so it is no backlink: index.md's inbound references are a.md's
        // `part_of` and nothing else — the anchor did not make the document
        // link to itself.
        let inbound = block_on(ws.backlinks_to("index.md", "index.md")).unwrap();
        assert!(
            inbound.iter().all(|bl| bl.source != Path::new("index.md")),
            "{inbound:?}"
        );
    }

    #[test]
    fn an_alias_with_a_locator_resolves_to_the_document_the_alias_names() {
        // §4's equivalence, at the layer `check` actually reads: the locator
        // changes where in a document a reader lands, never which document is
        // found. Asking the title index for `Mosiah 1#v2` misses every time and
        // used to fall through to a path, reporting a live document as broken.
        let dir = tempdir("alias-locator");
        write(
            &dir,
            "index.md",
            "---\ncontents:\n- mosiah-1.md\n---\nSee [[Mosiah 1#v2]] and [[Mosiah 1]].\n",
        );
        write(
            &dir,
            "mosiah-1.md",
            "---\ntitle: Mosiah 1\npart_of: index.md\n---\n",
        );
        let ws = Graph::new(StdFs, &dir, NoIndex, ReadSettings::default());
        let census = block_on(ws.census("index.md")).unwrap();

        let located = census
            .iter()
            .find(|e| e.target_text == "Mosiah 1#v2")
            .expect("the located alias is in the census");
        let plain = census
            .iter()
            .find(|e| e.target_text == "Mosiah 1")
            .expect("the plain alias is in the census");
        assert_eq!(located.resolution, plain.resolution, "{census:?}");
        assert_eq!(
            located.resolution,
            Resolution::Path(PathBuf::from("mosiah-1.md")),
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
        let ws = Graph::new(StdFs, &dir, NoIndex, ReadSettings::default());

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
            to_a.iter()
                .any(|bl| bl.source == Path::new("b.md") && matches!(bl.site, LinkSite::Body(_))),
            "{to_a:?}"
        );
        // All path-form (this workspace has no registry / id links).
        assert!(to_a.iter().all(|bl| !bl.by_id), "{to_a:?}");

        // The full map keys targets by path; a.md is one of them.
        let map = block_on(ws.backlinks("index.md")).unwrap();
        assert_eq!(map[&PathBuf::from("a.md")].len(), 3);
    }
}

/// Invert a census into a backlink map: every resolved target to the inbound
/// references that reach it, each target's sorted by source.
///
/// A free function over an already-taken census, rather than a method that takes
/// one, because the caller who has to bound the walk — `prov`, which knows where
/// it parks its own bytes — has already done the walking. Taking the census as
/// an argument is what lets the bounded and unbounded callers share this.
pub fn invert(census: Vec<CensusEntry>) -> BTreeMap<PathBuf, Vec<Backlink>> {
    let mut map: BTreeMap<PathBuf, Vec<Backlink>> = BTreeMap::new();
    for entry in census {
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
    map
}

/// The inbound references to one `target` within an already-taken census,
/// sorted by source — [`invert`] focused on a single entry.
pub fn inbound(census: Vec<CensusEntry>, target: &Path) -> Vec<Backlink> {
    let target = link::normalize(target);
    let mut links: Vec<Backlink> = census
        .into_iter()
        .filter(|entry| entry.resolution.resolved_path() == Some(&target))
        .map(|entry| {
            let by_id = matches!(entry.resolution, Resolution::Id { .. });
            Backlink {
                source: entry.source,
                site: entry.site,
                by_id,
            }
        })
        .collect();
    links.sort_by(|a, b| a.source.cmp(&b.source).then(a.by_id.cmp(&b.by_id)));
    links
}
