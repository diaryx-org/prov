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
//! - **malformed / dangling ID** — a `prov:<id>` reference (in a relation
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

use crate::content::ContentFormat;
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

impl CensusEntry {
    /// The integrity finding this entry represents when its target failed to
    /// resolve cleanly — `None` for a link that resolves.
    fn finding(&self) -> Option<Finding> {
        let doc = self.source.clone();
        let site = self.site.clone();
        let target = self.target_text.clone();
        match &self.resolution {
            Resolution::CaseMismatch { actual, .. } => Some(Finding::CaseMismatch {
                doc,
                site,
                target,
                actual: actual.clone(),
            }),
            Resolution::Broken => Some(Finding::BrokenLink { doc, site, target }),
            Resolution::MalformedId => Some(Finding::MalformedId { doc, site, target }),
            Resolution::DanglingId { id, tombstoned } => Some(Finding::DanglingId {
                doc,
                site,
                id: id.clone(),
                tombstoned: *tombstoned,
            }),
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
    /// `true` when the link is a `prov:<id>` reference (location-independent),
    /// `false` when it is a path.
    pub by_id: bool,
}

/// One integrity finding. `doc` is always the document that *declares* the
/// problem (workspace-relative); `site` is where in it the offending link sits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    /// `target` (written at `site`) resolves to nothing on disk.
    BrokenLink {
        doc: PathBuf,
        site: LinkSite,
        target: String,
    },
    /// `target` only resolves case-insensitively; the exact on-disk name is
    /// `actual`. Portable workspaces need the exact name.
    CaseMismatch {
        doc: PathBuf,
        site: LinkSite,
        target: String,
        actual: String,
    },
    /// A spanning target that was already reached — a containment cycle or a
    /// second parent, either of which breaks the single-parent spanning tree.
    DuplicateContainment { doc: PathBuf, target: String },
    /// A spanning child whose inverse field does not link back to `doc`.
    MissingInverse {
        doc: PathBuf,
        child: PathBuf,
        inverse: String,
    },
    /// A document that exists but could not be read or parsed.
    Unreadable { doc: PathBuf, error: String },
    /// A `prov:<id>` reference whose ID fails the shape/check-character
    /// test — almost certainly a typo, caught before it dangles silently.
    MalformedId {
        doc: PathBuf,
        site: LinkSite,
        target: String,
    },
    /// A well-formed `id:<id>` reference with no live registry entry.
    /// `tombstoned` distinguishes "that document was deleted" from "this ID
    /// was never issued here" (an out-of-band reference the registry has not
    /// reconciled — DESIGN §4's known hazard).
    DanglingId {
        doc: PathBuf,
        site: LinkSite,
        id: Id,
        tombstoned: bool,
    },
    /// A nominal (alias) reference whose name several documents claim, so it
    /// cannot resolve to one — the fallible edge of title-based linking.
    /// `candidates` are the documents that share the name, sorted.
    AmbiguousAlias {
        doc: PathBuf,
        site: LinkSite,
        name: String,
        candidates: Vec<PathBuf>,
    },
    /// An **id-addressed** link (`[label](id:…)`) whose display `label` no longer
    /// matches the current `title` of the document it resolves to — the target was
    /// retitled out of band (another editor, a merge) without the label following.
    /// `expected` is the target's current title; `actual` is the stale label;
    /// `target` is the link exactly as written. Only id links are flagged: their
    /// label is decorative (the id is the real reference), so a divergence is
    /// almost certainly staleness — a path link's label may be an intentional
    /// custom name. Auto-fixable by relabeling ([`Fix::RelabelLink`]); the in-app
    /// path keeps labels fresh via [`Workspace::retitle`](crate::Workspace::retitle),
    /// so this catches only what changed behind prov's back.
    StaleLabel {
        doc: PathBuf,
        site: LinkSite,
        target: String,
        expected: String,
        actual: String,
    },
    /// A document's self-stored `id` frontmatter disagrees with the registry —
    /// the portable shadow copy and the registry entry have drifted (an
    /// out-of-band edit or move). `frontmatter` is the ID the document claims;
    /// `registry` is the ID the registry records for this path, or `None` when
    /// the registry instead assigns the claimed ID to a *different* document. A
    /// reconcile hazard specific to frontmatter storage (DESIGN §5).
    IdMismatch {
        doc: PathBuf,
        frontmatter: Id,
        registry: Option<Id>,
    },
    /// A document carries a self-stored `id` the registry has no record of — the
    /// portable shadow got ahead of the cache (a document copied in with its
    /// `id`, or a registry rebuilt from a stale snapshot). Reconcilable by
    /// adopting the id into the registry.
    UnregisteredId { doc: PathBuf, frontmatter: Id },
    /// A content document that exists on disk but nothing reachable from the
    /// checked root links to it — the self-describing structure silently omits
    /// it. The onboarding signal (DESIGN §8): a folder of notes that predates the
    /// workspace, or a file that fell out of the tree. Diagnosis only for now;
    /// adopting it under a parent is the eventual fix.
    Orphan { doc: PathBuf },
    /// A document's stored content checksum no longer matches its bytes — the
    /// bit-rot signal (fixity). `recorded` is the hash on file; `actual` is what
    /// the bytes hash to now. Unlike a broken link there is nothing to re-point:
    /// the finding asks whether the change was *intended* (an out-of-band edit →
    /// re-stamp) or *corruption* (→ restore from backup), a judgment prov
    /// surfaces rather than makes.
    FixityMismatch {
        doc: PathBuf,
        recorded: String,
        actual: String,
    },
    /// A key in the workspace's config document that [`WorkspaceConfig::apply`]
    /// silently ignores — a misspelled key that resembles a real axis, or a
    /// recognized axis with a value prov does not understand. In both cases
    /// `apply` keeps the default, so the policy the author wrote never takes
    /// effect; this makes that visible instead of leaving it to be discovered by
    /// surprise. Diagnosis only — the fix (correct the spelling/value) is the
    /// author's, not a mechanical rewrite.
    ///
    /// [`WorkspaceConfig::apply`]: crate::config::WorkspaceConfig::apply
    ConfigIssue {
        doc: PathBuf,
        issue: crate::config::ConfigIssue,
    },
    /// A config surface declares a `spec` (`declared`) newer than this build
    /// understands ([`SPEC_VERSION`](crate::config::SPEC_VERSION)), so prov
    /// may be silently ignoring settings a newer prov wrote. Diagnosis only —
    /// the resolution is to upgrade prov, not to edit the workspace.
    ConfigSpecAhead { doc: PathBuf, declared: i64 },
    /// A record store — reached through the `pointer` relation (`registry`,
    /// `recycle_bin`, or a `fields` vocabulary) — is a **markdown** document
    /// (fenced frontmatter) rather than a whole-file config document. prov
    /// re-lays-out these stores as sorted records (DESIGN §5), so a prose carrier
    /// has no stable home; make it a `.yaml`/`.json`/`.figl` file. Diagnosis only.
    MalformedStore { doc: PathBuf, pointer: String },
    /// A **closed** controlled field (`field`) carries a `value` that is not a
    /// known term in its vocabulary — the consistency guarantee closed vocabularies
    /// exist for (a mistyped diaryx `audience` is a disclosure bug). `retired` is
    /// true when the value *was* a term but has been retired. Diagnosis only.
    UnknownTerm {
        doc: PathBuf,
        field: String,
        value: String,
        retired: bool,
    },
    /// An **open** controlled field (`field`) carries a `value` that is not a
    /// known term but closely resembles `suggestion` — casing/spelling drift in a
    /// folksonomy (`todo` vs `to-do`). A warning, not an error: open vocabularies
    /// admit new values, so this only nudges toward an existing spelling.
    TermNearMiss {
        doc: PathBuf,
        field: String,
        value: String,
        suggestion: String,
    },
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Finding::BrokenLink { doc, site, target } => {
                write!(f, "{}: broken {site} link: {target}", doc.display())
            }
            Finding::CaseMismatch {
                doc,
                site,
                target,
                actual,
            } => write!(
                f,
                "{}: case mismatch in {site} link: {target} is {actual} on disk",
                doc.display()
            ),
            Finding::DuplicateContainment { doc, target } => write!(
                f,
                "{}: {target} is already contained elsewhere (cycle or second parent)",
                doc.display()
            ),
            Finding::MissingInverse {
                doc,
                child,
                inverse,
            } => write!(
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
            Finding::DanglingId {
                doc,
                site,
                id,
                tombstoned,
            } => write!(
                f,
                "{}: dangling {site} ID: id:{id} ({})",
                doc.display(),
                if *tombstoned {
                    "document was deleted"
                } else {
                    "never issued in this registry"
                }
            ),
            Finding::AmbiguousAlias {
                doc,
                site,
                name,
                candidates,
            } => write!(
                f,
                "{}: ambiguous {site} alias: [[{name}]] matches {} documents ({})",
                doc.display(),
                candidates.len(),
                candidates
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Finding::StaleLabel {
                doc,
                site,
                expected,
                actual,
                ..
            } => write!(
                f,
                "{}: stale {site} label: reads \"{actual}\" but the target is now titled \"{expected}\"",
                doc.display()
            ),
            Finding::IdMismatch {
                doc,
                frontmatter,
                registry,
            } => match registry {
                Some(reg) => write!(
                    f,
                    "{}: id mismatch: frontmatter says id:{frontmatter} but the registry records id:{reg} for this path",
                    doc.display()
                ),
                None => write!(
                    f,
                    "{}: id mismatch: frontmatter says id:{frontmatter}, which the registry assigns to another document",
                    doc.display()
                ),
            },
            Finding::UnregisteredId { doc, frontmatter } => write!(
                f,
                "{}: unregistered id: frontmatter says id:{frontmatter} but the registry has no such entry",
                doc.display()
            ),
            Finding::Orphan { doc } => {
                write!(
                    f,
                    "{}: orphan — on disk but not linked into the workspace",
                    doc.display()
                )
            }
            Finding::FixityMismatch { doc, .. } => write!(
                f,
                "{}: fixity mismatch — content changed since its checksum was recorded \
                 (bit-rot, or an out-of-band edit)",
                doc.display()
            ),
            Finding::ConfigIssue { doc, issue } => match &issue.kind {
                crate::config::ConfigIssueKind::UnknownKey { suggestion } => write!(
                    f,
                    "{}: unknown config key `{}` — did you mean `{suggestion}`? (ignored, keeping the default)",
                    doc.display(),
                    issue.key
                ),
                crate::config::ConfigIssueKind::InvalidValue { value, expected } => write!(
                    f,
                    "{}: config `{}` has unrecognized value `{value}` (expected: {}) — keeping the default",
                    doc.display(),
                    issue.key,
                    expected.join(", ")
                ),
                crate::config::ConfigIssueKind::SpanningNotSingleParent { inverse } => write!(
                    f,
                    "{}: spanning relation's inverse `{inverse}` is `cardinality: many` — a spanning tree needs a single parent (make `{inverse}` cardinality `one`)",
                    doc.display(),
                ),
            },
            Finding::ConfigSpecAhead { doc, declared } => write!(
                f,
                "{}: config declares spec {declared}, newer than this build's spec {} — some settings may be ignored (upgrade prov)",
                doc.display(),
                crate::config::SPEC_VERSION
            ),
            Finding::MalformedStore { doc, pointer } => write!(
                f,
                "{}: `{pointer}` store is markdown — a record store must be a whole-file config document (.yaml/.json/.figl)",
                doc.display(),
            ),
            Finding::UnknownTerm {
                doc,
                field,
                value,
                retired,
            } => {
                if *retired {
                    write!(
                        f,
                        "{}: `{field}: {value}` names a retired term (no longer a valid value)",
                        doc.display(),
                    )
                } else {
                    write!(
                        f,
                        "{}: `{field}: {value}` is not a known term in this closed vocabulary",
                        doc.display(),
                    )
                }
            }
            Finding::TermNearMiss {
                doc,
                field,
                value,
                suggestion,
            } => write!(
                f,
                "{}: `{field}: {value}` is not a known term — did you mean `{suggestion}`?",
                doc.display(),
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
    /// style, or a `prov:<id>` when the workspace authors id links — is
    /// produced when the fix is applied (which may register `parent`), so the
    /// repair matches how the workspace authors every other link.
    AddInverse {
        doc: PathBuf,
        relation: String,
        parent: PathBuf,
        title: String,
    },
    /// Repair a [`Finding::StaleLabel`]: rewrite the display label of every link
    /// in `doc` that resolves to `target` to `new_label` (the target's current
    /// title), leaving the id/path target untouched. The same mechanic
    /// [`Workspace::retitle`](crate::Workspace::retitle) runs, applied after the
    /// fact to a label a title change bypassed.
    RelabelLink {
        doc: PathBuf,
        target: PathBuf,
        new_label: String,
    },
    /// Repair a [`Finding::IdMismatch`] by *trusting the registry*: rewrite the
    /// document's `id` frontmatter to `id` (the ID the registry records for its
    /// path). The registry is the durable, tombstone-bearing side, so it wins.
    SetId { doc: PathBuf, id: Id },
    /// Repair a [`Finding::UnregisteredId`] by adopting the document's self-stored
    /// `id` into the registry — registering `id` at this path so the cache
    /// catches up with the shadow.
    RegisterId { doc: PathBuf, id: Id },
    /// Repair a [`Finding::FixityMismatch`] by *re-stamping*: record the current
    /// bytes' hash, accepting the change as intended. The pressure-release valve
    /// for a legitimate out-of-band edit — its opposite, restoring from backup
    /// when the change was *not* intended, is the one thing prov cannot decide
    /// for you, which is why this is never applied without confirmation.
    RestampFixity { doc: PathBuf, hash: String },
}

impl fmt::Display for Fix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fix::AddInverse {
                doc,
                relation,
                parent,
                ..
            } => {
                write!(
                    f,
                    "declare {relation} → {} in {}",
                    parent.display(),
                    doc.display()
                )
            }
            Fix::RelabelLink {
                doc,
                target,
                new_label,
            } => {
                write!(
                    f,
                    "relabel the link to {} in {} as \"{new_label}\"",
                    target.display(),
                    doc.display()
                )
            }
            Fix::SetId { doc, id } => {
                write!(
                    f,
                    "set id:{id} in {} (matching the registry)",
                    doc.display()
                )
            }
            Fix::RegisterId { doc, id } => {
                write!(f, "register id:{id} → {} in the registry", doc.display())
            }
            Fix::RestampFixity { doc, .. } => {
                write!(
                    f,
                    "re-stamp the content checksum in {} to the current bytes",
                    doc.display()
                )
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
        match finding {
            Finding::MissingInverse {
                doc: parent,
                child,
                inverse,
            } => {
                // Safe only when the child makes no other (cardinality-one) parent claim.
                let (_, child_doc) = self.load(child).await?;
                if child_doc.meta.get(inverse).is_some() {
                    return Ok(None);
                }
                // Title the back-link with the parent's own title (else the path),
                // so a markdown-style repair reads well; the target itself is
                // produced at apply time, in the workspace's link style (or by id).
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
            // Trust the registry: rewrite the frontmatter to the id it records for
            // this path. Only when the registry actually names an id for the path;
            // the `None` case (the id belongs to *another* document) is a genuine
            // conflict a human must resolve, so it is left as diagnosis.
            Finding::IdMismatch {
                doc,
                registry: Some(reg),
                ..
            } => Ok(Some(Fix::SetId {
                doc: doc.clone(),
                id: reg.clone(),
            })),
            Finding::IdMismatch { registry: None, .. } => Ok(None),
            // Adopt the self-stored id into the registry.
            Finding::UnregisteredId { doc, frontmatter } => Ok(Some(Fix::RegisterId {
                doc: doc.clone(),
                id: frontmatter.clone(),
            })),
            // Re-stamp to the current bytes — accept the change. The current hash
            // is already computed in the finding, so no re-read is needed.
            Finding::FixityMismatch { doc, actual, .. } => Ok(Some(Fix::RestampFixity {
                doc: doc.clone(),
                hash: actual.clone(),
            })),
            // Relabel the stale link to the target's current title. Resolve its
            // (id) target to a path so the fix can locate it; a link that no
            // longer resolves has nothing safe to relabel.
            Finding::StaleLabel {
                doc,
                target,
                expected,
                ..
            } => match self.resolve_link(doc, &Link::parse(target)) {
                crate::Target::Path(path) => Ok(Some(Fix::RelabelLink {
                    doc: doc.clone(),
                    target: path,
                    new_label: expected.clone(),
                })),
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    /// Check the workspace reachable from `start`, returning every finding.
    /// An empty result means the reachable graph holds its invariants. This is
    /// the findings view over [`census`](Workspace::census): each forward link
    /// that fails to resolve becomes a finding, joined with the structural
    /// findings (unreadable document, duplicate containment, missing inverse)
    /// the walk raises from traversal state.
    pub async fn check(&self, start: impl AsRef<Path>) -> Result<Vec<Finding>> {
        let start = start.as_ref();
        let Walk {
            census,
            mut findings,
            content_bodies,
        } = self.walk(start).await?;
        for entry in &census {
            findings.extend(entry.finding());
        }
        findings.extend(self.orphans(start, &census, &content_bodies).await?);
        findings.extend(
            self.fixity_findings(start, &census, &content_bodies)
                .await?,
        );
        findings.extend(self.config_findings(start).await?);
        findings.extend(self.store_findings(start).await?);
        findings.extend(
            self.vocabulary_findings(start, &census, &content_bodies)
                .await?,
        );
        findings.extend(self.stale_label_findings(&census).await?);
        Ok(findings)
    }

    /// Flag every **id-addressed** link whose display label has drifted from the
    /// current title of the document it resolves to — a target retitled out of
    /// band. Only id links are checked: their label is decorative (the id is the
    /// reference), so divergence is staleness, where a path link's label may be an
    /// intentional custom name. Bounded to the census already walked; each target
    /// title is read once and cached.
    async fn stale_label_findings(&self, census: &[CensusEntry]) -> Result<Vec<Finding>> {
        let mut titles: std::collections::BTreeMap<PathBuf, Option<String>> =
            std::collections::BTreeMap::new();
        let mut findings = Vec::new();
        for entry in census {
            // Only id-addressed links with a label: `Resolution::Id` marks the
            // id form (its `to` is the live target path), and a label is what
            // there is to keep fresh.
            let Some(label) = &entry.label else { continue };
            let Resolution::Id { to: target, .. } = &entry.resolution else {
                continue;
            };
            if !titles.contains_key(target) {
                let title = self.title_of(target).await?;
                titles.insert(target.clone(), title);
            }
            if let Some(Some(current)) = titles.get(target)
                && label != current
            {
                findings.push(Finding::StaleLabel {
                    doc: entry.source.clone(),
                    site: entry.site.clone(),
                    target: entry.target_text.clone(),
                    expected: current.clone(),
                    actual: label.clone(),
                });
            }
        }
        Ok(findings)
    }

    /// The `title` a document declares, or `None` when it is missing or the file
    /// cannot be read.
    async fn title_of(&self, path: &Path) -> Result<Option<String>> {
        let text = match self.fs().read_to_string(&self.root().join(path)).await {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let doc = crate::document::Document::parse(path, &text)?;
        Ok(doc
            .meta
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned))
    }

    /// Verify every **record store** the workspace reaches — the id registry, the
    /// recycle-bin index, and each `fields` vocabulary — is a whole-file config
    /// document, emitting a [`Finding::MalformedStore`] for any found in a markdown
    /// carrier (DESIGN §5, the whole-file rule). This *reports* rather than aborts:
    /// the loaders themselves hard-error on a markdown store, but `check` surfaces
    /// the same problem as a finding so a diagnosis run lists it alongside the rest.
    async fn store_findings(&self, start: &Path) -> Result<Vec<Finding>> {
        let mut stores: Vec<(&'static str, PathBuf)> = Vec::new();
        if let Some(p) = self.registry_path(start).await? {
            stores.push(("registry", p));
        }
        if let Some(p) = self.recycle_bin_path(start).await? {
            stores.push(("recycle_bin", p));
        }
        let config = self.effective_config(start).await?;
        for spec in config.fields.values() {
            if let Some(p) = self.vocabulary_path(start, &spec.vocabulary) {
                stores.push(("vocabulary", p));
            }
        }
        let mut findings = Vec::new();
        for (pointer, path) in stores {
            if let Ok((_, doc)) = self.load(&path).await
                && let Some(carrier) = doc.carrier
                && crate::document::require_whole_file(&path, carrier).is_err()
            {
                findings.push(Finding::MalformedStore {
                    doc: path,
                    pointer: pointer.to_string(),
                });
            }
        }
        Ok(findings)
    }

    /// Check every controlled `fields` value against its vocabulary over the
    /// reachable document set (§8's reachability bound, the same set
    /// [`fixity_findings`](Self::fixity_findings) walks). A **closed** field emits
    /// a [`Finding::UnknownTerm`] for any value not a known term; an **open** field
    /// emits a [`Finding::TermNearMiss`] only when an unknown value closely
    /// resembles a known term (typo/casing drift). A field whose vocabulary cannot
    /// be loaded contributes no term findings — its store is reported separately by
    /// [`store_findings`](Self::store_findings).
    async fn vocabulary_findings(
        &self,
        start: &Path,
        census: &[CensusEntry],
        content_bodies: &[PathBuf],
    ) -> Result<Vec<Finding>> {
        let config = self.effective_config(start).await?;
        if config.fields.is_empty() {
            return Ok(Vec::new());
        }
        // Load each field's vocabulary once. A store that fails to load (missing,
        // markdown) simply drops out — its own finding comes from `store_findings`.
        let mut vocabs: Vec<(String, crate::config::OpenClosed, crate::vocabulary::Vocabulary)> =
            Vec::new();
        for (field, spec) in &config.fields {
            if let Ok(Some(vocab)) = self.load_vocabulary(start, &spec.vocabulary).await {
                vocabs.push((field.clone(), spec.values, vocab));
            }
        }
        if vocabs.is_empty() {
            return Ok(Vec::new());
        }

        // The reachable document set (mirrors `fixity_findings`).
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

        let mut findings = Vec::new();
        for path in reachable {
            let Ok((_, doc)) = self.load(&path).await else {
                continue;
            };
            for (field, values, vocab) in &vocabs {
                let Some(field_value) = doc.meta.get(field) else {
                    continue;
                };
                for term in field_value.link_strings() {
                    if vocab.accepts(&term) {
                        continue;
                    }
                    match values {
                        crate::config::OpenClosed::Closed => findings.push(Finding::UnknownTerm {
                            doc: path.clone(),
                            field: field.clone(),
                            value: term.clone(),
                            retired: vocab.is_retired(&term),
                        }),
                        crate::config::OpenClosed::Open => {
                            if let Some(suggestion) =
                                crate::textdist::nearest_owned(&term, &vocab.live_term_names())
                            {
                                findings.push(Finding::TermNearMiss {
                                    doc: path.clone(),
                                    field: field.clone(),
                                    value: term.clone(),
                                    suggestion,
                                });
                            }
                        }
                    }
                }
            }
        }
        Ok(findings)
    }

    /// Lint both config surfaces the workspace reads — the root's `prov:`
    /// frontmatter block and the dedicated config document — one
    /// [`Finding::ConfigIssue`] per key [`WorkspaceConfig::apply`] would silently
    /// ignore (a typo'd key, or a recognized axis with a value prov doesn't
    /// understand). Both are closed policy namespaces (the block is nested under
    /// one key; the config document is wholly policy), so `diagnose` runs fully on
    /// each without mistaking a user field for a setting. A no-op surface — no
    /// `prov:` block, no config document — contributes nothing.
    ///
    /// [`WorkspaceConfig::apply`]: crate::config::WorkspaceConfig::apply
    async fn config_findings(&self, start: &Path) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        // The root's inline `prov:` block (the description home).
        if let Ok((_, root)) = self.load(start).await
            && let Some(block) = root.meta.get(crate::config::ROOT_CONFIG_KEY)
        {
            let doc = start.to_path_buf();
            findings.extend(crate::config::diagnose(block).into_iter().map(|issue| {
                Finding::ConfigIssue {
                    doc: doc.clone(),
                    issue,
                }
            }));
            if let Some(declared) = crate::config::spec_ahead(block) {
                findings.push(Finding::ConfigSpecAhead { doc, declared });
            }
        }
        // The dedicated config document (the `config`-relation target).
        if let Some(config_doc) = self.config_path(start).await? {
            let (_, doc) = self.load(&config_doc).await?;
            findings.extend(crate::config::diagnose(&doc.meta).into_iter().map(|issue| {
                Finding::ConfigIssue {
                    doc: config_doc.clone(),
                    issue,
                }
            }));
            if let Some(declared) = crate::config::spec_ahead(&doc.meta) {
                findings.push(Finding::ConfigSpecAhead {
                    doc: config_doc.clone(),
                    declared,
                });
            }
        }
        Ok(findings)
    }

    /// Verify every recorded content checksum reachable from `start` — one
    /// [`Finding::FixityMismatch`] per document whose bytes no longer hash to what
    /// it recorded. This is the bit-rot pass, the integrity question link
    /// validation cannot answer: *are the bytes still the bytes?*
    ///
    /// It honors whatever hash is on record, independent of the workspace's
    /// fixity *setting* — the setting governs what is written, never what is
    /// checked, so a hash present on disk is always verified. A document with no
    /// recorded hash is skipped (a document predating fixity is not "corrupt"),
    /// and a digest prov does not recognize (a future algorithm) is left
    /// unverified rather than flagged. The reachable set is exactly the one
    /// [`orphans`](Self::orphans) uses.
    ///
    /// The bytes a document's hash covers depend on its shape: a document that
    /// points `content` at a sibling (an attachment payload, or a separated prose
    /// body) hashes *that file*; a combined document hashes its own body.
    async fn fixity_findings(
        &self,
        start: &Path,
        census: &[CensusEntry],
        content_bodies: &[PathBuf],
    ) -> Result<Vec<Finding>> {
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

        let mut findings = Vec::new();
        for path in reachable {
            // A reached payload file (a `.png`) will not parse as a document —
            // skip it; it is verified through its sidecar, not on its own.
            let Ok((_, doc)) = self.load(&path).await else {
                continue;
            };
            let Some(recorded) = doc.meta.get("content_hash").and_then(Value::as_str) else {
                continue;
            };
            if !crate::fixity::is_recognized(recorded) {
                continue;
            }
            // What the hash covers: the `content` sibling if this document points
            // at one, else the document's own body.
            let actual = match doc.content_attr() {
                Some(raw) => {
                    let dir = path.parent().unwrap_or(Path::new(""));
                    let target = link::normalize(dir.join(raw));
                    match self.fs().read(&self.root().join(&target)).await {
                        Ok(bytes) => crate::fixity::digest(&bytes),
                        // A missing payload is a broken-`content` matter, not a
                        // fixity one — leave it for that check, don't double-report.
                        Err(_) => continue,
                    }
                }
                None => crate::fixity::digest(doc.body.as_bytes()),
            };
            if actual != recorded {
                findings.push(Finding::FixityMismatch {
                    doc: path,
                    recorded: recorded.to_string(),
                    actual,
                });
            }
        }
        Ok(findings)
    }

    /// Record that a document's content just changed — the single seam for the
    /// bookkeeping an edit implies, done as one crash-safe write.
    ///
    /// Two independent effects, each self-gating:
    /// - **Fixity**: (re)stamp `content_hash` when the workspace records fixity
    ///   for this document's kind (a payload for an attachment, a body otherwise)
    ///   *and* the bytes have actually drifted from what is recorded — so an
    ///   unchanged document restamps nothing.
    /// - **Timestamp**: when `updated` is `Some((field, at))`, set that frontmatter
    ///   `field` to `at`. The *caller* decides an edit happened and supplies the
    ///   time — the library stays clockless and deterministic (DESIGN §2: the
    ///   client produces the instant, prov owns the field and its RFC 3339
    ///   convention). Pass `None` to reconcile the checksum only.
    ///
    /// Returns whether anything was written. Hashes the same bytes `check`
    /// verifies: the `content` sibling for a document that points at one, else the
    /// document's own body.
    pub async fn record_content_update(
        &mut self,
        path: impl AsRef<Path>,
        updated: Option<(&str, &str)>,
    ) -> Result<bool> {
        let path = link::normalize(path.as_ref());
        let (original, doc) = self.load(&path).await?;

        // Fixity: does this document's kind get hashed, and has it drifted?
        let covered = if doc.is_attachment() {
            self.fixity().covers_payloads()
        } else {
            self.fixity().covers_bodies()
        };
        let new_hash = if covered {
            let hash = match doc.content_attr() {
                Some(raw) => {
                    let dir = path.parent().unwrap_or(Path::new(""));
                    let target = link::normalize(dir.join(raw));
                    crate::fixity::digest(&self.fs().read(&self.root().join(&target)).await?)
                }
                None => crate::fixity::digest(doc.body.as_bytes()),
            };
            (doc.meta.get("content_hash").and_then(Value::as_str) != Some(hash.as_str()))
                .then_some(hash)
        } else {
            None
        };

        // Apply both frontmatter edits (if any) to the one text, write once.
        let mut text = original;
        let mut wrote = false;
        if let Some(hash) = new_hash {
            text = crate::edit::set_in_text(
                &text,
                doc.carrier,
                "content_hash",
                fig::Value::Str(hash),
            )?;
            wrote = true;
        }
        if let Some((field, at)) = updated
            && !field.is_empty()
        {
            text = crate::edit::set_in_text(
                &text,
                doc.carrier,
                field,
                fig::Value::Str(at.to_string()),
            )?;
            wrote = true;
        }
        if !wrote {
            return Ok(false);
        }
        let mut cs = self.change();
        cs.write(&path, text);
        self.commit(cs).await?;
        Ok(true)
    }

    /// Reconcile the content checksum for the document at `path` — [
    /// `record_content_update`](Self::record_content_update) with no timestamp.
    /// The prov-mediated way to keep fixity true across an edit, and how a
    /// document first *earns* a body hash under the `full` tier.
    pub async fn restamp_fixity(&mut self, path: impl AsRef<Path>) -> Result<bool> {
        self.record_content_update(path, None).await
    }

    /// The content documents in the workspace's *reached* directories that
    /// nothing reachable from `start` links to — [`Finding::Orphan`] for each. The
    /// reachable set is `start` itself plus every path a census link resolves to
    /// (any relation, a body wikilink, or an id through the registry); a
    /// case-mismatched link counts its *actual* on-disk file as reached, so a file
    /// is never both case-mismatched and orphaned. Findings are sorted by path for
    /// a stable report.
    ///
    /// Scope is **reachability-bounded** (DESIGN §8): only directories a linked
    /// document already occupies are scanned, and never recursively — a
    /// subdirectory nothing links into (a vendored tree, a nested prov
    /// workspace, a `scratch/` folder) is not read and yields no orphans. A new
    /// directory enters scope by an explicit act that links into it (`new`,
    /// `adopt`, `attach`, a `mirror` import); `check` then keeps it honest. The
    /// deliberate trade: a document dropped into a not-yet-linked folder is
    /// invisible here rather than flagged.
    ///
    /// Orphanhood is relative to `start`: run from the workspace root (the usual
    /// case) it means "on disk in a known directory but unlinked."
    async fn orphans(
        &self,
        start: &Path,
        census: &[CensusEntry],
        content_bodies: &[PathBuf],
    ) -> Result<Vec<Finding>> {
        let mut reachable: BTreeSet<PathBuf> = BTreeSet::new();
        reachable.insert(link::normalize(start));
        // Prose bodies of separated nodes are linked via `content`, not a census
        // edge — reach them so a separated workspace's bodies are not orphans.
        reachable.extend(content_bodies.iter().cloned());
        for entry in census {
            match &entry.resolution {
                Resolution::Path(p) | Resolution::Id { to: p, .. } => {
                    reachable.insert(p.clone());
                }
                // The link is by the wrong case, but the file it *means* is on
                // disk and thereby linked — reach the real name, not the miss.
                Resolution::CaseMismatch { got, actual } => {
                    reachable.insert(got.with_file_name(actual));
                }
                _ => {}
            }
        }
        // Scan only the directories the reachable set occupies (their direct
        // children), never descending into unreached subdirectories.
        let reached_dirs = Self::reached_dirs(&reachable);
        let mut docs: Vec<PathBuf> = self
            .direct_child_files(&reached_dirs)
            .await?
            .into_iter()
            .filter(|p| ContentFormat::from_extension(p).is_some() && !reachable.contains(p))
            .collect();
        docs.sort();
        Ok(docs
            .into_iter()
            .map(|doc| Finding::Orphan { doc })
            .collect())
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
        Ok(self.walk(start.as_ref()).await?.census)
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
                Backlink {
                    source: entry.source,
                    site: entry.site,
                    by_id,
                }
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
    async fn walk(&self, start: &Path) -> Result<Walk> {
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
                    structural.push(Finding::Unreadable {
                        doc: path,
                        error: e.to_string(),
                    });
                    continue;
                }
            };

            // Reconcile a self-stored `id` against the registry (frontmatter
            // storage, DESIGN §5). Three outcomes when a document carries its own
            // `id`: the registry agrees (nothing to do); the registry records a
            // *different* id for this path, or hands this id to another document
            // (`IdMismatch` — a drift); or the registry has never heard of the id
            // (`UnregisteredId` — the shadow got ahead of the cache).
            if let Some(fm) = doc.meta.get("id").and_then(Value::as_str)
                && !fm.trim().is_empty()
            {
                let fm = Id(fm.trim().to_string());
                match self.index().id_for_path(&path) {
                    Some(reg) if reg != fm => structural.push(Finding::IdMismatch {
                        doc: path.clone(),
                        frontmatter: fm,
                        registry: Some(reg),
                    }),
                    Some(_) => {} // the registry agrees with the frontmatter
                    None => match self.index().resolve(&fm) {
                        // The id is live, but points at a *different* document.
                        Some(other) if other != path => structural.push(Finding::IdMismatch {
                            doc: path.clone(),
                            frontmatter: fm,
                            registry: None,
                        }),
                        // resolve == this path but no reverse entry: consistent.
                        Some(_) => {}
                        // The registry has no record of this id at all.
                        None => structural.push(Finding::UnregisteredId {
                            doc: path.clone(),
                            frontmatter: fm,
                        }),
                    },
                }
            }

            // Frontmatter relation edges — the only links that can be spanning.
            for edge in self.relations().edges(&doc.meta) {
                // Parse once: `link.target` is the bare target (any `[label](…)`
                // stripped), which is what both the census and findings record.
                let link = Link::parse(&edge.target);
                if titles.is_none() && title::is_alias_shaped(&link.target) {
                    titles = Some(self.title_index_scoped(start).await?);
                }
                let resolution = self.resolve_forward(&path, &link, titles.as_ref()).await;

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
                            let inverse_targets = child_doc
                                .meta
                                .get(inverse)
                                .map(Value::link_strings)
                                .unwrap_or_default();
                            // Build the title index if a nominal inverse link needs it.
                            if titles.is_none()
                                && inverse_targets
                                    .iter()
                                    .any(|t| title::is_alias_shaped(&Link::parse(t).target))
                            {
                                titles = Some(self.title_index_scoped(start).await?);
                            }
                            let points_back = inverse_targets.iter().any(|t| {
                                self.resolve_link_with(&resolved, &Link::parse(t), titles.as_ref())
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
                    titles = Some(self.title_index_scoped(start).await?);
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
                        structural.push(Finding::CaseMismatch {
                            doc: path.clone(),
                            site,
                            target: content.to_string(),
                            actual,
                        });
                    }
                    NameMatch::None => structural.push(Finding::BrokenLink {
                        doc: path.clone(),
                        site,
                        target: content.to_string(),
                    }),
                }
            }
        }
        Ok(Walk {
            census,
            findings: structural,
            content_bodies,
        })
    }

    /// Resolve one forward link (declared in the document at `source`) into a
    /// [`Resolution`]. A path target is checked against the on-disk name; an
    /// `id:<id>` target resolves through the registry and stays an id-form
    /// resolution; a nominal (`[[My File]]`) target resolves through `titles` —
    /// `Unique` to the on-disk path, `Ambiguous` to
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
        if let Some(id) = link.id_target() {
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
        if let Some(titles) = titles.filter(|_| title::is_alias_shaped(&link.target)) {
            match titles.resolve(&link.target) {
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
            let Some(entry_name) = entry.file_name() else {
                continue;
            };
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
        let mut cs = self.change();
        match fix {
            Fix::AddInverse {
                doc,
                relation,
                parent,
                title,
            } => {
                // The parent exists (this repair points a child back at it), so an
                // id link registers it by path. Authored in `relation`'s style.
                let target = self
                    .authored_target(relation, doc, parent, title, true)
                    .await?;
                let (text, parsed) = self.load(doc).await?;
                let updated = crate::edit::set_in_text(
                    &text,
                    parsed.carrier,
                    relation,
                    fig::Value::Str(target),
                )?;
                cs.write(doc, updated);
            }
            // Relabel every link in `doc` resolving to `target` to the new label,
            // reusing the same mechanic `retitle` runs.
            Fix::RelabelLink {
                doc,
                target,
                new_label,
            } => {
                if let Some(updated) = self.relabel_inbound_doc(doc, target, new_label).await? {
                    cs.write(doc, updated);
                }
            }
            // Trust the registry: overwrite the document's `id` frontmatter.
            Fix::SetId { doc, id } => {
                let (text, parsed) = self.load(doc).await?;
                let updated = crate::edit::set_in_text(
                    &text,
                    parsed.carrier,
                    "id",
                    fig::Value::Str(id.0.clone()),
                )?;
                cs.write(doc, updated);
            }
            // Adopt the frontmatter id into the registry (a cache update, no doc
            // edit — but the registry write it implies is staged by `commit`).
            Fix::RegisterId { doc, id } => {
                self.index_mut().register(id, doc);
            }
            // Re-stamp: overwrite the document's `content_hash` with the current
            // bytes' hash (comment-/format-preservingly, like `SetId`).
            Fix::RestampFixity { doc, hash } => {
                let (text, parsed) = self.load(doc).await?;
                let updated = crate::edit::set_in_text(
                    &text,
                    parsed.carrier,
                    "content_hash",
                    fig::Value::Str(hash.clone()),
                )?;
                cs.write(doc, updated);
            }
        }
        self.commit(cs).await
    }
}

enum NameMatch {
    Exact,
    CaseOnly(String),
    None,
}

/// The result of one spanning-tree [`walk`](Workspace::walk): the forward-link
/// census, the structural findings raised from traversal state, and the prose
/// body files reached through separated nodes' `content` pointers (tracked for
/// the orphan check, deliberately absent from the census).
struct Walk {
    census: Vec<CensusEntry>,
    findings: Vec<Finding>,
    content_bodies: Vec<PathBuf>,
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
        let dir = std::env::temp_dir().join(format!("prov-check-{tag}-{}", std::process::id()));
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
    fn check_flags_and_fixes_a_stale_id_link_label() {
        use crate::link::{Addressing, ReferenceStyle, Wrapper};
        use crate::relation::{Relation, RelationSet};

        let by_id_labeled = ReferenceStyle {
            wrapper: Wrapper::Markdown,
            addressing: Addressing::Id,
            label: true,
            path_style: LinkStyle::default(),
        };
        let relations = RelationSet::new()
            .with(Relation::many("contents").inverse("part_of"))
            .with(
                Relation::one("part_of")
                    .inverse("contents")
                    .style(by_id_labeled),
            )
            .spanning("contents");

        let dir = tempdir("stale-label");
        write(&dir, "index.md", "---\ntitle: Root\n---\n");
        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .relations(relations)
            .identity(Minter::eager(7))
            .index(FileIndex::new(fig::Format::Yaml))
            .build();
        block_on(w.create_with_title(Path::new("child.md"), Path::new("index.md"), "Child"))
            .unwrap();

        // Retitle the parent OUT OF BAND — edit its `title` directly, so the
        // inbound label on the child is never refreshed (the merge/other-editor
        // case `retitle` cannot catch).
        let idx = std::fs::read_to_string(dir.join("index.md")).unwrap();
        std::fs::write(
            dir.join("index.md"),
            idx.replace("title: Root", "title: Renamed"),
        )
        .unwrap();

        // check flags the drift…
        let findings = block_on(w.check("index.md")).unwrap();
        let stale = findings
            .iter()
            .find(|f| matches!(f, Finding::StaleLabel { .. }));
        assert!(stale.is_some(), "expected a StaleLabel finding, got {findings:?}");

        // …and the suggested fix relabels the child to the parent's new title.
        let fix = block_on(w.suggest_fix(stale.unwrap()))
            .unwrap()
            .expect("stale label is auto-fixable");
        block_on(w.apply_fix(&fix)).unwrap();
        let child = std::fs::read_to_string(dir.join("child.md")).unwrap();
        assert!(child.contains("[Renamed](id:"), "relabeled: {child}");

        // Clean afterward.
        assert!(
            !block_on(w.check("index.md"))
                .unwrap()
                .iter()
                .any(|f| matches!(f, Finding::StaleLabel { .. })),
            "no stale labels remain"
        );
    }

    #[test]
    fn a_closed_vocabulary_flags_an_unknown_term() {
        let dir = tempdir("vocab-closed");
        write(
            &dir,
            "index.md",
            "---\n\
             contents:\n- a.md\n\
             audience: public\n\
             prov:\n  fields:\n    audience:\n      values: closed\n      vocabulary: vocab/audiences.yaml\n\
             ---\n",
        );
        // a.md carries a typo'd audience — in a closed vocabulary that is a hard finding.
        write(
            &dir,
            "a.md",
            "---\npart_of: index.md\naudience: freinds\n---\n",
        );
        write(
            &dir,
            "vocab/audiences.yaml",
            "title: Audiences\npart_of: /index.md\nvocabulary:\n  field: audience\n  values: closed\nterms:\n  public: {}\n  friends: {}\n",
        );
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let findings = block_on(ws.check("index.md")).unwrap();
        assert!(
            findings.iter().any(|f| matches!(
                f,
                Finding::UnknownTerm { field, value, .. } if field == "audience" && value == "freinds"
            )),
            "{findings:?}"
        );
        // The valid `audience: public` on the root raises nothing.
        assert!(
            !findings
                .iter()
                .any(|f| matches!(f, Finding::UnknownTerm { value, .. } if value == "public")),
            "{findings:?}"
        );
    }

    #[test]
    fn an_open_vocabulary_only_warns_on_a_near_miss() {
        let dir = tempdir("vocab-open");
        write(
            &dir,
            "index.md",
            "---\n\
             contents:\n- near.md\n- novel.md\n\
             prov:\n  fields:\n    tags:\n      values: open\n      vocabulary: vocab/tags.yaml\n\
             ---\n",
        );
        // `todi` ~ `todo` (near miss → warn); `research` is genuinely new (allowed).
        write(&dir, "near.md", "---\npart_of: index.md\ntags: todi\n---\n");
        write(
            &dir,
            "novel.md",
            "---\npart_of: index.md\ntags: research\n---\n",
        );
        write(
            &dir,
            "vocab/tags.yaml",
            "vocabulary:\n  field: tags\n  values: open\nterms:\n  todo: {}\n  idea: {}\n",
        );
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let findings = block_on(ws.check("index.md")).unwrap();
        assert!(
            findings.iter().any(|f| matches!(
                f,
                Finding::TermNearMiss { value, suggestion, .. } if value == "todi" && suggestion == "todo"
            )),
            "{findings:?}"
        );
        // An unrelated new value in an open vocabulary is allowed silently.
        assert!(
            !findings
                .iter()
                .any(|f| matches!(f, Finding::TermNearMiss { value, .. } if value == "research")),
            "{findings:?}"
        );
    }

    #[test]
    fn a_markdown_registry_store_is_flagged() {
        let dir = tempdir("vocab-store");
        write(
            &dir,
            "index.md",
            "---\ncontents:\n- a.md\nregistry: registry.md\n---\n",
        );
        write(&dir, "a.md", "---\npart_of: index.md\n---\n");
        // A registry in a markdown carrier — refused as a record store.
        write(
            &dir,
            "registry.md",
            "---\ntitle: Registry\nregistry:\n  bcdfghj: a.md\n---\nprose\n",
        );
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let findings = block_on(ws.check("index.md")).unwrap();
        assert!(
            findings.iter().any(|f| matches!(
                f,
                Finding::MalformedStore { pointer, .. } if pointer == "registry"
            )),
            "{findings:?}"
        );
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
            findings
                .iter()
                .any(|f| matches!(f, Finding::BrokenLink { target, .. } if target == "gone.md")),
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

    #[test]
    fn check_flags_a_broken_body_wikilink() {
        let dir = tempdir("body-broken");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\n---\nSee [[gone.md]] for more.\n",
        );
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
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\n---\nSee [[Alpha]] and [[Dup]].\n",
        );
        write(&dir, "alpha.md", "---\ntitle: Alpha\n---\n");
        write(&dir, "one.md", "---\ntitle: Dup\n---\n");
        write(&dir, "two.md", "---\ntitle: Dup\n---\n");
        let ws = Workspace::builder(StdFs).root(&dir).build();

        let findings = block_on(ws.check("index.md")).unwrap();
        // The unique alias produced no finding; the ambiguous one did.
        assert!(
            !findings
                .iter()
                .any(|f| matches!(f, Finding::AmbiguousAlias { name, .. } if name == "Alpha")),
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

    #[test]
    fn alias_resolution_is_scoped_to_reached_directories() {
        // The title index is bounded to directories the workspace reaches
        // (DESIGN §8), so a document in an *unreached* subtree — a vendored copy,
        // a nested workspace — cannot collide with a workspace title. Here two
        // documents are titled "Target": one in the reached tree, one in an
        // unlinked `vendor/`. A whole-repo scan would make `[[Target]]` ambiguous;
        // the scoped scan resolves it to the one in the workspace.
        let dir = tempdir("alias-scope");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- notes/a.md\n- notes/target.md\n---\n",
        );
        write(
            &dir,
            "notes/a.md",
            "---\ntitle: A\npart_of: ../index.md\n---\nSee [[Target]].\n",
        );
        write(
            &dir,
            "notes/target.md",
            "---\ntitle: Target\npart_of: ../index.md\n---\n",
        );
        // A same-titled document in an unreached directory — never linked.
        write(&dir, "vendor/dup.md", "---\ntitle: Target\n---\n");
        let ws = Workspace::builder(StdFs).root(&dir).build();

        let findings = block_on(ws.check("index.md")).unwrap();
        // `[[Target]]` resolves to the workspace document, not flagged ambiguous…
        assert!(
            !findings
                .iter()
                .any(|f| matches!(f, Finding::AmbiguousAlias { name, .. } if name == "Target")),
            "the vendored duplicate must not make the alias ambiguous: {findings:?}"
        );
        // …and the unreached `vendor/` is invisible — no orphan for its document.
        assert_eq!(
            findings,
            vec![],
            "clean: vendored subtree neither collides nor orphans: {findings:?}"
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

        let broken: Vec<_> = findings
            .iter()
            .filter(|f| matches!(f, Finding::BrokenLink { .. }))
            .collect();
        assert_eq!(broken.len(), 1, "{findings:?}");
        assert!(matches!(broken[0], Finding::BrokenLink { target, .. } if target == "gone.md"));
    }

    #[test]
    fn a_resolving_body_wikilink_is_not_a_finding() {
        let dir = tempdir("body-clean");
        write(
            &dir,
            "index.md",
            "---\ncontents:\n- a.md\n---\nSee [[a.md]].\n",
        );
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
        write(
            &dir,
            "index.md",
            "---\ncontents:\n- '[Notes](</My Notes/notes.md>)'\n---\n",
        );
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
        let mut ws = Workspace::builder(StdFs)
            .root(&dir)
            .link_style(LinkStyle::PlainCanonical)
            .build();

        let findings = block_on(ws.check("index.md")).unwrap();
        let mi = findings
            .iter()
            .find(|f| matches!(f, Finding::MissingInverse { .. }))
            .unwrap();
        let fix = block_on(ws.suggest_fix(mi))
            .unwrap()
            .expect("safely fixable");
        assert!(
            matches!(&fix, Fix::AddInverse { doc, relation, parent, .. }
                if doc == &PathBuf::from("a.md") && relation == "part_of"
                    && parent == &PathBuf::from("index.md")),
            "{fix:?}"
        );

        block_on(ws.apply_fix(&fix)).unwrap();
        // a.md now declares the back-link (plain-canonical), and it validates.
        assert!(
            std::fs::read_to_string(dir.join("a.md"))
                .unwrap()
                .contains("part_of: index.md")
        );
        assert_eq!(block_on(ws.check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn autofix_matches_the_workspace_link_style() {
        // The Adam's-Archive concern: the repair must be written in the
        // workspace's declared style (markdown-root, titled with the parent's
        // own title) — never a bare fifth style prov invented.
        let dir = tempdir("autofix-style");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- '[A](/a.md)'\n---\n",
        );
        write(&dir, "a.md", "---\ntitle: A\n---\n");
        let mut ws = Workspace::builder(StdFs)
            .root(&dir)
            .link_style(LinkStyle::MarkdownRoot)
            .build();

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
            std::fs::read_to_string(dir.join("a.md"))
                .unwrap()
                .contains("[Home](/index.md)"),
            "{:?}",
            std::fs::read_to_string(dir.join("a.md"))
        );
    }

    #[test]
    fn autofix_authors_an_id_link_when_configured() {
        // Obsidian-style: the repair is authored by id (registering the parent),
        // so it survives a later move untouched.
        let dir = tempdir("autofix-id");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- a.md\n---\n",
        );
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

        let parent_id = ws
            .index()
            .id_for_path(Path::new("index.md"))
            .expect("parent registered");
        assert!(
            std::fs::read_to_string(dir.join("a.md"))
                .unwrap()
                .contains(&format!("part_of: id:{parent_id}"))
        );
    }

    #[test]
    fn id_mismatch_flags_a_frontmatter_id_disagreeing_with_the_registry() {
        use crate::identity::Id;
        use crate::index::IndexStore;

        // A document that carries its own `id` (frontmatter storage, DESIGN §5).
        let dir = tempdir("id-mismatch");
        write(&dir, "index.md", "---\ntitle: Home\nid: aaaaaaa\n---\n");
        let build = || {
            Workspace::builder(StdFs)
                .root(&dir)
                .identity(Minter::lazy(9))
                .index(FileIndex::new(fig::Format::Yaml))
                .build()
        };

        // Registry agrees with the frontmatter → nothing to reconcile.
        let mut ws = build();
        ws.index_mut()
            .register(&Id("aaaaaaa".into()), Path::new("index.md"));
        let clean = block_on(ws.check("index.md")).unwrap();
        assert!(
            !clean
                .iter()
                .any(|f| matches!(f, Finding::IdMismatch { .. })),
            "agreeing id should not flag: {clean:?}"
        );

        // Registry records a *different* id for this path → mismatch surfaced.
        let mut ws = build();
        ws.index_mut()
            .register(&Id("bbbbbbb".into()), Path::new("index.md"));
        let findings = block_on(ws.check("index.md")).unwrap();
        assert!(
            findings.iter().any(|f| matches!(f,
                Finding::IdMismatch { frontmatter, registry: Some(reg), .. }
                if frontmatter.0 == "aaaaaaa" && reg.0 == "bbbbbbb")),
            "expected an IdMismatch: {findings:?}"
        );

        // Trust-the-registry fix rewrites the frontmatter to the registry's id.
        let mi = findings
            .iter()
            .find(|f| matches!(f, Finding::IdMismatch { .. }))
            .unwrap()
            .clone();
        let fix = block_on(ws.suggest_fix(&mi)).unwrap().unwrap();
        assert!(matches!(&fix, Fix::SetId { id, .. } if id.0 == "bbbbbbb"));
        block_on(ws.apply_fix(&fix)).unwrap();
        assert!(
            std::fs::read_to_string(dir.join("index.md"))
                .unwrap()
                .contains("id: bbbbbbb")
        );
        assert!(
            block_on(ws.check("index.md")).unwrap().is_empty(),
            "reconciled → clean"
        );
    }

    #[test]
    fn unregistered_id_is_found_and_adopted_into_the_registry() {
        use crate::identity::Id;
        use crate::index::IndexStore;

        // A document carries an `id` the (empty) registry has never seen.
        let dir = tempdir("unregistered-id");
        write(&dir, "index.md", "---\ntitle: Home\nid: aaaaaaa\n---\n");
        let mut ws = Workspace::builder(StdFs)
            .root(&dir)
            .identity(Minter::lazy(9))
            .index(FileIndex::new(fig::Format::Yaml))
            .build();

        let findings = block_on(ws.check("index.md")).unwrap();
        let f = findings
            .iter()
            .find(|f| matches!(f, Finding::UnregisteredId { frontmatter, .. } if frontmatter.0 == "aaaaaaa"))
            .expect("expected an UnregisteredId")
            .clone();

        // The fix adopts the self-stored id into the registry.
        let fix = block_on(ws.suggest_fix(&f)).unwrap().unwrap();
        assert!(matches!(&fix, Fix::RegisterId { id, .. } if id.0 == "aaaaaaa"));
        block_on(ws.apply_fix(&fix)).unwrap();
        assert_eq!(
            ws.index().id_for_path(Path::new("index.md")),
            Some(Id("aaaaaaa".into()))
        );
        assert!(
            block_on(ws.check("index.md")).unwrap().is_empty(),
            "adopted → clean"
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
        let mi = findings
            .iter()
            .find(|f| matches!(f, Finding::MissingInverse { .. }))
            .unwrap();
        assert!(
            block_on(ws.suggest_fix(mi)).unwrap().is_none(),
            "contested → not auto-fixed"
        );
    }

    #[test]
    fn body_link_findings_are_never_auto_fixed() {
        // The code-block-false-positive guard: a broken *body* wikilink is
        // diagnosis only — autofix must not offer to edit prose.
        let dir = tempdir("autofix-body");
        // A nested list comprehension: `[[…]]` that is code, not a wikilink.
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\n---\ndp = [[inf] * n for _ in range(m)]]\n",
        );
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let findings = block_on(ws.check("index.md")).unwrap();
        let broken = findings
            .iter()
            .find(|f| {
                matches!(
                    f,
                    Finding::BrokenLink {
                        site: LinkSite::Body(_),
                        ..
                    }
                )
            })
            .expect("the code fragment scanned as a broken body link");
        assert!(block_on(ws.suggest_fix(broken)).unwrap().is_none());
    }

    #[test]
    fn an_unlinked_document_in_a_known_directory_is_reported_as_an_orphan() {
        let dir = tempdir("orphan");
        // index links a.md; a.md links back. loose.md sits in the *root*
        // directory (which is reached) but nobody points at it — the onboarding
        // signal.
        write(&dir, "index.md", "---\ncontents:\n- a.md\n---\n");
        write(&dir, "a.md", "---\npart_of: index.md\n---\n");
        write(
            &dir,
            "loose.md",
            "---\ntitle: Loose\n---\njust sitting here\n",
        );
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let findings = block_on(ws.check("index.md")).unwrap();

        // loose.md is flagged…
        assert!(
            findings
                .iter()
                .any(|f| matches!(f, Finding::Orphan { doc } if doc == &PathBuf::from("loose.md"))),
            "{findings:?}"
        );
        // …but the linked files (root + reachable child) are not.
        assert!(
            !findings.iter().any(|f| matches!(f, Finding::Orphan { doc }
                if doc == &PathBuf::from("index.md") || doc == &PathBuf::from("a.md"))),
            "linked files must not be orphans: {findings:?}"
        );
    }

    #[test]
    fn a_document_in_an_unreached_directory_is_not_an_orphan() {
        // Reachability-bounded discovery (DESIGN §8): a subdirectory nothing links
        // into — a nested workspace, a vendored tree, a scratch folder — is never
        // scanned, so its documents are invisible to `check` rather than orphaned.
        let dir = tempdir("orphan-bounded");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n---\n");
        write(&dir, "a.md", "---\npart_of: index.md\n---\n");
        write(&dir, "vendor/other.md", "---\ntitle: Vendored\n---\n");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let findings = block_on(ws.check("index.md")).unwrap();
        assert_eq!(
            findings,
            vec![],
            "an unlinked subdirectory yields no findings: {findings:?}"
        );
    }

    #[test]
    fn an_orphan_in_a_reached_subdirectory_is_still_flagged() {
        // Scope grows with the links: once a directory is reached (a document in
        // it is linked), its *other* unlinked files become orphans.
        let dir = tempdir("orphan-reached-sub");
        write(&dir, "index.md", "---\ncontents:\n- notes/one.md\n---\n");
        write(&dir, "notes/one.md", "---\npart_of: ../index.md\n---\n");
        write(&dir, "notes/stray.md", "---\ntitle: Stray\n---\n");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let findings = block_on(ws.check("index.md")).unwrap();
        assert!(
            findings.iter().any(
                |f| matches!(f, Finding::Orphan { doc } if doc == &PathBuf::from("notes/stray.md"))
            ),
            "a stray file in a reached directory is an orphan: {findings:?}"
        );
    }

    #[test]
    fn a_case_mismatched_link_target_is_not_also_an_orphan() {
        // docs/DESIGN.md is linked, but by the wrong case (docs/design.md). It
        // must surface as a CaseMismatch, never doubly as an Orphan.
        let dir = tempdir("orphan-case");
        write(&dir, "index.md", "---\ncontents:\n- docs/design.md\n---\n");
        write(&dir, "docs/DESIGN.md", "---\npart_of: ../index.md\n---\n");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let findings = block_on(ws.check("index.md")).unwrap();

        assert!(
            findings
                .iter()
                .any(|f| matches!(f, Finding::CaseMismatch { .. })),
            "{findings:?}"
        );
        assert!(
            !findings.iter().any(|f| matches!(f, Finding::Orphan { .. })),
            "the case-mismatched file's real name is reached, so it is not an orphan: {findings:?}"
        );
    }

    #[test]
    fn duplicate_containment_is_found() {
        let dir = tempdir("dup");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n- b.md\n---\n");
        write(
            &dir,
            "a.md",
            "---\npart_of: index.md\ncontents:\n- b.md\n---\n",
        );
        write(&dir, "b.md", "---\npart_of: index.md\n---\n");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let findings = block_on(ws.check("index.md")).unwrap();
        assert!(
            findings
                .iter()
                .any(|f| matches!(f, Finding::DuplicateContainment { .. })),
            "{findings:?}"
        );
    }

    #[test]
    fn full_tier_body_fixity_round_trips_through_restamp_and_check() {
        // The `full` tier: a document's *body* carries its own checksum. The whole
        // prov-edit loop, exercised at the library level (no $EDITOR needed):
        // stamp → verify → out-of-band body edit is caught → restamp re-blesses.
        use crate::config::Fixity;
        let dir = tempdir("fixity-body");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- note.md\n---\n",
        );
        write(
            &dir,
            "note.md",
            "---\ntitle: Note\npart_of: index.md\n---\nhello world\n",
        );

        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .fixity(Fixity::Full)
            .build();

        // The document earns a body hash; restamping unchanged bytes is a no-op.
        assert!(
            block_on(w.restamp_fixity("note.md")).unwrap(),
            "first stamp records a hash"
        );
        assert!(
            !block_on(w.restamp_fixity("note.md")).unwrap(),
            "restamp of unchanged bytes writes nothing"
        );
        assert!(
            std::fs::read_to_string(dir.join("note.md"))
                .unwrap()
                .contains("content_hash: sha256:")
        );
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);

        // Edit the body out-of-band (bypassing `prov edit`) — check catches it.
        let stamped = std::fs::read_to_string(dir.join("note.md")).unwrap();
        std::fs::write(
            dir.join("note.md"),
            stamped.replace("hello world", "goodbye world"),
        )
        .unwrap();
        let findings = block_on(w.check("index.md")).unwrap();
        assert!(
            findings.iter().any(
                |f| matches!(f, Finding::FixityMismatch { doc, .. } if doc == Path::new("note.md"))
            ),
            "an out-of-band body edit must be caught: {findings:?}"
        );

        // Restamp (what `prov edit` does on save) re-blesses it.
        assert!(block_on(w.restamp_fixity("note.md")).unwrap());
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn record_content_update_stamps_the_timestamp_field_and_the_hash_together() {
        use crate::config::Fixity;
        let dir = tempdir("content-update");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- note.md\n---\n",
        );
        write(
            &dir,
            "note.md",
            "---\ntitle: Note\npart_of: index.md\n---\nbody\n",
        );
        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .fixity(Fixity::Full)
            .build();

        // A content edit at a caller-supplied instant: both the `updated` field
        // (the client's chosen name + RFC-3339 value) and the body hash land in
        // one write.
        assert!(
            block_on(w.record_content_update("note.md", Some(("updated", "2026-07-16T10:00:00Z"))))
                .unwrap()
        );
        let text = std::fs::read_to_string(dir.join("note.md")).unwrap();
        assert!(text.contains("updated: 2026-07-16T10:00:00Z"), "{text}");
        assert!(text.contains("content_hash: sha256:"), "{text}");
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);

        // The library never reads a clock: the exact string it is handed is what
        // it writes (DESIGN §2 — the client produces the instant).
        assert!(
            block_on(w.record_content_update("note.md", Some(("updated", "2099-01-01T00:00:00Z"))))
                .unwrap()
        );
        assert!(
            std::fs::read_to_string(dir.join("note.md"))
                .unwrap()
                .contains("updated: 2099-01-01T00:00:00Z")
        );
    }

    #[test]
    fn record_content_update_writes_a_timestamp_even_with_fixity_off() {
        // The timestamp axis is independent of fixity: `updated` tracking works
        // with no checksums at all (and writes no content_hash then).
        use crate::config::Fixity;
        let dir = tempdir("content-update-nofix");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- note.md\n---\n",
        );
        write(
            &dir,
            "note.md",
            "---\ntitle: Note\npart_of: index.md\n---\nbody\n",
        );
        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .fixity(Fixity::Off)
            .build();

        assert!(
            block_on(
                w.record_content_update("note.md", Some(("modified", "2026-07-16T10:00:00Z")))
            )
            .unwrap()
        );
        let text = std::fs::read_to_string(dir.join("note.md")).unwrap();
        assert!(text.contains("modified: 2026-07-16T10:00:00Z"), "{text}");
        assert!(
            !text.contains("content_hash"),
            "fixity off records no hash: {text}"
        );
    }
}
