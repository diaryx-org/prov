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
use crate::mutate::maintain;
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
    /// The registry holds an id for a document that does not carry it — the
    /// inverse of [`UnregisteredId`](Finding::UnregisteredId), and raised only
    /// under a stamping mode ([`IdStorage::stamps_frontmatter`], DESIGN §5),
    /// where every document is meant to be self-describing. Raised for a
    /// workspace converted from registry-only storage, and for any document
    /// whose `id` was stripped out of band.
    ///
    /// It is the finding that makes the conversion mechanical: `check` names
    /// every unstamped document, and [`Fix::SetId`] writes the registry's id into
    /// each one — the registry is the authority here, since it is the home the
    /// id has had all along.
    ///
    /// [`IdStorage::stamps_frontmatter`]: crate::config::IdStorage::stamps_frontmatter
    UnstampedId { doc: PathBuf, registry: Id },
    /// A content document that exists on disk but nothing reachable from the
    /// checked root links to it — the self-describing structure silently omits
    /// it. The onboarding signal (DESIGN §8): a folder of notes that predates the
    /// workspace, or a file that fell out of the tree.
    ///
    /// Repaired by adopting it under a parent, and `root` is why the finding
    /// carries two paths instead of one: the *nearest* container above the orphan
    /// is usually the right home, but a workspace whose root has no children yet
    /// declares no spanning relation and so answers no structural test for being
    /// one. Only the pass that walked the tree knows which document it walked
    /// *from*, so it records it — leaving the root always offered as the home of
    /// last resort, which is what the CLI used to hardcode as the only one.
    Orphan { doc: PathBuf, root: PathBuf },
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
    /// A history-store index document has drifted from the directory it
    /// describes: `missing` holds what is on disk but unlinked, `extra` what is
    /// linked but gone.
    ///
    /// The **expected** outcome of a sync transport mangling a derived cache, and
    /// the reason the store can tolerate having any mutable file at all: the
    /// index is a rebuildable cache, so a conflicted one is a finding with a
    /// mechanical autofix ([`Fix::RebuildHistoryIndex`]) rather than data loss.
    /// Authority lives in the immutable event documents, which is why the repair
    /// can be a pure function of the directory listing.
    ///
    /// Raised per shard, so a mangled `2026/07/index.<ext>` is reported — and
    /// repaired — without touching any other month.
    HistoryIndexStale {
        index: PathBuf,
        missing: Vec<PathBuf>,
        extra: Vec<PathBuf>,
    },
    /// A recycle-bin record promises a recovery it cannot deliver: the bytes it
    /// parked under `recyclebin/items/` are not on disk. `index` is the bin
    /// index holding the record, `from` the path the document was deleted from
    /// (the record's identity), and `missing` the absent parked file(s) — two
    /// when a separated document lost both its metadata and its prose body.
    ///
    /// The parked bytes are deliberately *unreached* (nothing links into
    /// `items/`, so §8's orphan walk ignores them), which is exactly why they
    /// need their own check: no other pass looks inside the bin. Without this,
    /// the loss surfaces only when [`restore`](crate::Workspace::restore) fails
    /// to rename a file that is not there.
    ///
    /// Reachable by ordinary means — a partial sync, a transport pruning an
    /// unreached subtree, a hand-deletion inside `recyclebin/` — and also the
    /// residue of restoring an old bin index that lists items since purged.
    ///
    /// **Diagnosis only.** Dropping the record would destroy the last evidence
    /// of what was deleted and foreclose the real repair (putting the bytes
    /// back from a backup, which makes the record valid again); purging the
    /// records wholesale is what [`empty_bin`](crate::Workspace::empty_bin) is
    /// for. The same judgment [`FixityMismatch`](Finding::FixityMismatch)
    /// declines to make on the author's behalf.
    RecycledBytesMissing {
        index: PathBuf,
        from: PathBuf,
        missing: Vec<PathBuf>,
    },
    /// An event manifest names a content hash with **no blob behind it**, so the
    /// files captured under that hash cannot be restored from this store. `store`
    /// is the store index, `hash` the digest as a manifest spells it, and `paths`
    /// the captured path(s) that named it, deduped across every event.
    ///
    /// Raised per **hash**, not per event: one lost blob is one thing to put back,
    /// and a store where fifty events all captured the same unchanged file should
    /// say "these bytes are gone" once rather than fifty times. Which *events* are
    /// thereby incomplete is [`history-show`]'s question, and it already marks the
    /// rows.
    ///
    /// **Two causes, and the wording must admit both.** Bytes genuinely lost — and
    /// a sync still in flight, because an event document and the blobs it names
    /// travel over the transport independently, and a small document routinely
    /// lands well before a hundred megabytes it points at. A finding that cries
    /// corruption at a routine, self-resolving state is one users learn to ignore.
    ///
    /// **Diagnosis only.** Nothing here can synthesize bytes, and the real repair —
    /// letting the transport finish, or restoring `blobs/` from a backup — makes
    /// the finding go away on its own. Deleting the manifest rows that name the
    /// hash would be the only "fix" available, and it would destroy the record of
    /// what was captured to silence a report about it: the judgment
    /// [`RecycledBytesMissing`](Finding::RecycledBytesMissing) also declines.
    ///
    /// [`history-show`]: crate::Workspace::history_missing_blobs
    HistoryBlobMissing {
        store: PathBuf,
        hash: String,
        paths: Vec<PathBuf>,
    },
    /// Bytes parked under `blobs/` that **no event manifest names** — storage
    /// nothing in the store can reach. `store` is the store index, `blobs` the
    /// unreferenced files, workspace-relative and sorted.
    ///
    /// Plain mark-and-sweep, which is what full manifests buy: union every event's
    /// `files` hashes and subtract the blob listing. Under a delta log the same
    /// question would require folding ancestry.
    ///
    /// **Expected transiently** — a blob can arrive from another device before the
    /// event that references it — so this is not evidence of damage on its own.
    /// [`history-prune`](crate::Workspace::history_prune) and `history-forget` are
    /// the durable producers, and both collect after themselves, which is what
    /// makes a *persistent* orphan worth reporting.
    ///
    /// Anything non-hidden under `blobs/` that is not a referenced blob counts,
    /// not only well-formed digests: a transport's `.sync-conflict` copy of a blob
    /// is exactly the cruft this should surface, and it would never match a hash.
    ///
    /// **Diagnosis only.** Collecting is destruction, and autofix is metadata-only
    /// by construction (see [`Fix`]) — `history-prune` is where bytes are deleted,
    /// deliberately and on request.
    HistoryBlobOrphaned { store: PathBuf, blobs: Vec<PathBuf> },
    /// A history store is on disk at the conventional path, the workspace's
    /// `history` axis is on, and the **root document does not point at it**.
    /// `root` is the root, `store` the store index nothing declares.
    ///
    /// The store is reached one way only, through that pointer — so a transport
    /// that mangles a single line of the root takes the whole safety net out of
    /// prov's view. Every other finding in this family assumes the store was
    /// found; this is the one that fires when it was not, and without it the
    /// failure is **completely silent**: `history-list` prints nothing, the walk
    /// never descends into `history/`, so not even an orphan is reported, and the
    /// first sign of trouble is a restore that cannot find the event you need.
    ///
    /// Conditioned on the axis on purpose. A workspace with `history: off` and a
    /// leftover `history/` directory has not lost anything — it declared it wants
    /// no store, and a finding there would be prov nagging about a directory the
    /// user is entitled to leave lying around. Declaring `manual` is the statement
    /// that makes a missing pointer a defect rather than a preference.
    ///
    /// Autofixable, and one of the few repairs that is unambiguous: the pointer's
    /// target is not a guess (only the conventional path is ever probed —
    /// [`StoreLocation`](crate::history::StoreLocation)), the edit is
    /// metadata-only, and the alternative is a workspace that keeps capturing into
    /// a store it cannot read back.
    HistoryStoreUnlinked { root: PathBuf, store: PathBuf },
    /// The generated `about.md` does not match what prov would produce from the
    /// current configuration — or the `about` pointer names a file that is not
    /// there. `path` is the page, `expected` what prov would write, and
    /// `missing` distinguishes "gone" from "drifted".
    ///
    /// This is what keeps the page's byline honest. "Derived from this
    /// workspace's own settings" is a claim a human byline cannot make, and it is
    /// worth more than a name precisely *because* it is checkable — so it has to
    /// actually be checked.
    ///
    /// Reached by ordinary means: a config edit made by hand rather than through
    /// `prov config`, a sync transport resolving a conflict by merging, someone
    /// improving the prose in place, or a prov old enough to predate a wording
    /// change. Every one of them is repaired the same way.
    ///
    /// **Autofixed by regeneration** ([`Fix::RegenerateAbout`]), and — unlike
    /// [`FixityMismatch`](Finding::FixityMismatch), which declines to guess on
    /// the author's behalf — with no confirmation gate. The correct content is
    /// fully determined by the configuration, so there is no judgment to get
    /// wrong, and nothing user-authored can be destroyed: spec §4 calls the page
    /// discardable, and it means it.
    ///
    /// A workspace with `about: off` and no pointer is silent here — not a
    /// finding. Nothing was promised, so nothing is broken.
    AboutStale {
        path: PathBuf,
        expected: String,
        missing: bool,
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
            Finding::UnstampedId { doc, registry } => write!(
                f,
                "{}: unstamped id: the registry records id:{registry} but the document does not carry it",
                doc.display()
            ),
            Finding::Orphan { doc, .. } => {
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
            Finding::HistoryIndexStale {
                index,
                missing,
                extra,
            } => {
                let mut parts = Vec::new();
                if !missing.is_empty() {
                    parts.push(format!("{} unlisted", missing.len()));
                }
                if !extra.is_empty() {
                    parts.push(format!("{} listed but gone", extra.len()));
                }
                write!(
                    f,
                    "{}: history index is stale ({}) — rebuildable from the directory",
                    index.display(),
                    parts.join(", ")
                )
            }
            Finding::RecycledBytesMissing {
                index,
                from,
                missing,
            } => {
                let gone: Vec<String> = missing.iter().map(|p| p.display().to_string()).collect();
                write!(
                    f,
                    "{}: {} cannot be restored — parked bytes missing ({})",
                    index.display(),
                    from.display(),
                    gone.join(", ")
                )
            }
            Finding::HistoryBlobMissing { store, hash, paths } => {
                let named: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
                // Both causes, in the order of likelihood: a store that syncs is
                // in this state routinely, and only the second reading is damage.
                write!(
                    f,
                    "{}: no bytes for {hash} — {} cannot be restored from this store \
                     (the blob has not arrived yet, or it is gone)",
                    store.display(),
                    named.join(", ")
                )
            }
            Finding::HistoryBlobOrphaned { store, blobs } => {
                let stray: Vec<String> = blobs.iter().map(|p| p.display().to_string()).collect();
                write!(
                    f,
                    "{}: {} parked blob(s) no event references ({}) — `prov history-prune` collects them",
                    store.display(),
                    blobs.len(),
                    stray.join(", ")
                )
            }
            Finding::HistoryStoreUnlinked { root, store } => {
                write!(
                    f,
                    "{}: a history store at {} is not declared here — it is invisible \
                     to prov until it is (`prov check --fix` re-declares it)",
                    root.display(),
                    store.display()
                )
            }
            // The expected content is deliberately not printed: it is the whole
            // page, and the repair is one command away.
            // Covers both shapes of "missing": a pointer naming a page that is
            // not there, and a workspace whose `about` axis is on but which has
            // never generated one.
            Finding::AboutStale {
                path,
                missing: true,
                ..
            } => {
                write!(
                    f,
                    "{}: not written — `prov about` writes the page that explains this workspace",
                    path.display()
                )
            }
            Finding::AboutStale { path, .. } => {
                write!(
                    f,
                    "{}: does not match what this workspace's configuration describes — `prov about` rewrites it",
                    path.display()
                )
            }
        }
    }
}

/// What an operation did to the workspace's integrity: the difference between a
/// [`check`](Workspace::check) taken before it and one taken after.
///
/// The reason this exists rather than a bare post-operation list: the operations
/// that most need to report their effect on integrity are the ones you run
/// *because something is already wrong* — an autofix sweep, a restore from a
/// captured event. A list of findings afterwards cannot distinguish the damage
/// the operation repaired from the damage it caused from the damage it merely
/// inherited, and those three call for entirely different responses. Only
/// [`introduced`](CheckDiff::introduced) is a reason to stop; only
/// [`fixed`](CheckDiff::fixed) is a reason to celebrate;
/// [`pre_existing`](CheckDiff::pre_existing) is a count, not a reprint.
///
/// [`Finding`] is `Eq`, so this is set arithmetic over values. A finding carries
/// the document, the site and the target, so two findings that compare equal
/// *are* the same problem — there is nothing to key on beyond the value itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckDiff {
    /// Present before, gone after — what the operation repaired.
    pub fixed: Vec<Finding>,
    /// Absent before, present after — what the operation broke. The bucket that
    /// matters, and the one that should drive an exit code.
    pub introduced: Vec<Finding>,
    /// Present before and still present after — untouched, and not this
    /// operation's doing.
    pub pre_existing: Vec<Finding>,
}

impl CheckDiff {
    /// Bucket two `check` runs against each other.
    pub fn between(before: &[Finding], after: &[Finding]) -> Self {
        Self {
            fixed: before
                .iter()
                .filter(|f| !after.contains(f))
                .cloned()
                .collect(),
            introduced: after
                .iter()
                .filter(|f| !before.contains(f))
                .cloned()
                .collect(),
            // Drawn from `after`, so this reads "still there" rather than "was
            // there" — the two differ once anything has been fixed.
            pre_existing: after
                .iter()
                .filter(|f| before.contains(f))
                .cloned()
                .collect(),
        }
    }

    /// Whether the operation broke nothing — the question an exit code asks.
    /// True even when the workspace is still dirty: findings this operation
    /// inherited are not its verdict.
    pub fn is_clean(&self) -> bool {
        self.introduced.is_empty()
    }

    /// Whether the operation changed nothing about the workspace's integrity —
    /// it neither fixed nor broke anything.
    pub fn is_empty(&self) -> bool {
        self.fixed.is_empty() && self.introduced.is_empty()
    }
}

/// A concrete repair for a finding — the fully-determined action
/// [`apply_fix`](Workspace::apply_fix) takes, and what a [`Remedy`] commits to
/// once chosen.
///
/// **Structure only, never prose.** A fix edits frontmatter, or it rewrites a
/// span twig itself identified as a link ([`link::parsed_link_spans`]); it never
/// touches ordinary body text. DESIGN §8's objection stands and is the reason for
/// the second half of that rule: a `[[…]]` that is really code
/// (`[[None] * width]`) must not be "repaired", and a lexical wikilink span
/// cannot tell prose from a link well enough to write into it. A parser-reported
/// `[label](target)` can.
///
/// **Never deletes bytes.** A fix may drop a link — a broken entry, a dangling
/// reference — but files, blobs, and recycle-bin records are outside its reach.
/// Destroying data is what a deliberate verb (`rm`, `history-prune`,
/// `empty-bin`) is for, on request and by name.
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
    /// Repair a [`Finding::IdMismatch`] or [`Finding::UnstampedId`] by
    /// *trusting the registry*: rewrite the
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
    /// Repair a [`Finding::HistoryIndexStale`] by rebuilding `index` from the
    /// directory it describes. Safe because a history index is a *derived* cache
    /// — the immutable event documents are the authority — so the rebuild is a
    /// pure function of that one directory's listing, and touches no other shard.
    RebuildHistoryIndex { index: PathBuf },
    /// Repair a [`Finding::HistoryStoreUnlinked`] by declaring the `history`
    /// pointer in `root` again, at the store that is already there. Metadata-only,
    /// and the target is the one the finding found rather than anything this fix
    /// decides — it re-declares a store, it never adopts one.
    LinkHistoryStore { root: PathBuf, store: PathBuf },
    /// Rewrite the generated page whole. Carries the content rather than
    /// recomputing it: the finding already generated it to detect the drift, and
    /// carrying it keeps `apply_fix` free of any dependency on configuration —
    /// the repair is a write, not a decision.
    RegenerateAbout { path: PathBuf, content: String },
    /// Drop the entry of `relation` in `doc` whose target is written as
    /// `target` — the repair for a link with nowhere left to point and no
    /// candidate worth repointing it at.
    ///
    /// Addressed by the target *as written*, because the findings that need this
    /// are precisely the ones whose target does not resolve. A written target is
    /// not unique, so the first matching entry goes; a second run takes the next.
    RemoveEntry {
        doc: PathBuf,
        relation: String,
        target: String,
    },
    /// Repoint the entry of `relation` in `doc` written as `from` at `to`,
    /// keeping its label and wrapper. `to` is a bare target, rendered at
    /// suggestion time in the workspace's own reference style, so the repaired
    /// entry reads like every other link in the document.
    RetargetEntry {
        doc: PathBuf,
        relation: String,
        from: String,
        to: String,
    },
    /// Repoint a body link at `to`. `span` is the byte range of the whole link
    /// construct within `doc`'s **body**, and `from` is the exact text that range
    /// held when the finding was raised — checked before the splice, so a span
    /// that has drifted refuses rather than corrupting prose.
    ///
    /// Only ever offered for a span [`link::parsed_link_spans`] reported.
    RetargetBodyLink {
        doc: PathBuf,
        span: Range<usize>,
        from: String,
        to: String,
    },
    /// Unlink a body link, leaving its label as plain text — the least
    /// destructive reading of "remove this link", since the words the author
    /// wrote survive and only the broken reference goes. `span` and `from` carry
    /// the same guarantee as [`RetargetBodyLink`](Fix::RetargetBodyLink).
    RemoveBodyLink {
        doc: PathBuf,
        span: Range<usize>,
        from: String,
    },
    /// Bring an unlinked document into the tree under `parent`, both directions —
    /// [`adopt`](crate::Workspace::adopt)'s exact effect, which is why it
    /// delegates rather than reimplementing it.
    Adopt { child: PathBuf, parent: PathBuf },
    /// Settle a contested containment in `parent`'s favor:
    /// [`reparent`](crate::Workspace::reparent) repoints the child's inverse and
    /// removes the rival's spanning entry, so the tree keeps one parent per node.
    Reparent { child: PathBuf, parent: PathBuf },
    /// Replace the value `from` in `doc`'s controlled `field` with `to` — the
    /// repair that spells a term the way its vocabulary does.
    SetFieldValue {
        doc: PathBuf,
        field: String,
        from: String,
        to: String,
    },
    /// Add `term` to the vocabulary at `store` with a null value — one of the
    /// shapes [`Vocabulary::from_meta`](crate::vocabulary::Vocabulary::from_meta)
    /// reads as a live term carrying no metadata (a bare `term:` in hand-written
    /// YAML is the same thing; this is how fig spells it). An `id` and a `means`
    /// are the author's to add afterward — minting one here would be this repair
    /// deciding the term is permanent, which is not what was asked.
    ///
    /// The answer to "the vocabulary is wrong, not the document". Never offered
    /// for a *retired* term: the entry already exists, and writing over it would
    /// un-retire it while destroying the id and gloss it carries.
    AddTerm { store: PathBuf, term: String },
    /// Rename the config key `from` to `to` in `doc`, keeping its value,
    /// position, and comments — a misspelled axis, spelled the way `apply` reads.
    SetConfigKey {
        doc: PathBuf,
        from: String,
        to: String,
    },
    /// Replace the value at the dotted config `key` in `doc` with a spelling prov
    /// understands.
    SetConfigValue {
        doc: PathBuf,
        key: String,
        value: String,
    },
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
            Fix::RebuildHistoryIndex { index } => {
                write!(
                    f,
                    "rebuild {} from the events in its directory",
                    index.display()
                )
            }
            Fix::LinkHistoryStore { root, store } => {
                write!(
                    f,
                    "declare the history store at {} in {}",
                    store.display(),
                    root.display()
                )
            }
            Fix::RegenerateAbout { path, .. } => {
                write!(
                    f,
                    "regenerate {} from this workspace's configuration",
                    path.display()
                )
            }
            Fix::RemoveEntry {
                doc,
                relation,
                target,
            } => write!(f, "remove {target} from {relation} in {}", doc.display()),
            Fix::RetargetEntry {
                doc,
                relation,
                from,
                to,
            } => write!(
                f,
                "point the {relation} entry {from} at {to} in {}",
                doc.display()
            ),
            Fix::RetargetBodyLink { doc, from, to, .. } => {
                write!(f, "point the body link {from} at {to} in {}", doc.display())
            }
            Fix::RemoveBodyLink { doc, from, .. } => {
                write!(f, "unlink {from} in {}, keeping its text", doc.display())
            }
            Fix::Adopt { child, parent } => {
                write!(f, "adopt {} under {}", child.display(), parent.display())
            }
            Fix::Reparent { child, parent } => write!(
                f,
                "make {} the parent of {}",
                parent.display(),
                child.display()
            ),
            Fix::SetFieldValue {
                doc,
                field,
                from,
                to,
            } => write!(f, "set {field} from {from} to {to} in {}", doc.display()),
            Fix::AddTerm { store, term } => {
                write!(f, "add the term {term} to {}", store.display())
            }
            Fix::SetConfigKey { doc, from, to } => {
                write!(
                    f,
                    "rename the config key {from} to {to} in {}",
                    doc.display()
                )
            }
            Fix::SetConfigValue { doc, key, value } => {
                write!(f, "set {key} to {value} in {}", doc.display())
            }
        }
    }
}

/// How much judgment a [`Remedy`] embodies — the axis that decides whether it may
/// be applied without asking.
///
/// This is what the old one-answer `suggest_fix` encoded in prose and in its
/// `None` returns. A finding whose repair is a pure function of an authority and
/// a finding whose repair is one of three defensible rewrites are not different
/// in *arity*; they are different in whether anything is being **chosen**.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Warrant {
    /// A pure function of an authority — configuration, a directory listing, the
    /// registry. Nothing is chosen, so nothing can be chosen wrongly: safe to
    /// apply unattended. [`Fix::RegenerateAbout`] is the archetype (the page is
    /// derived, so the repaired file is what a fresh `about` would have written).
    Derived,
    /// Rival answers exist and prov cannot rank them for you — which of two
    /// parents is the real one, which near-match a broken link meant. Offered,
    /// never assumed.
    Judgment,
    /// Removes something a person authored. A link records intent
    /// ([`delete`](crate::Workspace::delete) reports inbound danglers rather than
    /// rewriting them for exactly this reason), so a removal is never batched,
    /// never unattended, and never what an "apply all of this kind" covers.
    Destructive,
}

impl Warrant {
    /// The slug a report or a prompt prints.
    pub fn as_str(self) -> &'static str {
        match self {
            Warrant::Derived => "derived",
            Warrant::Judgment => "judgment",
            Warrant::Destructive => "destructive",
        }
    }
}

impl fmt::Display for Warrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a [`Remedy`] *does*, as a stable slug — the handle a caller names when it
/// wants to repeat a choice.
///
/// Deliberately coarser than [`Fix`]: several remedies of one kind may be offered
/// for a single finding (one `Retarget` per near-match), and a caller that says
/// "do this to all of them" is naming the kind, not the individual fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RemedyKind {
    /// Point a link somewhere that resolves.
    Retarget,
    /// Drop the offending link.
    RemoveLink,
    /// Bring a stale display label back in line with its target's title.
    Relabel,
    /// Declare the missing half of a containment pair.
    Link,
    /// Bring an unlinked document into the tree under a parent.
    Adopt,
    /// Move a contested document under one of its rival parents.
    Reparent,
    /// Resolve an identity disagreement in the registry's favor.
    TrustRegistry,
    /// Resolve an identity disagreement in the document's favor.
    TrustDocument,
    /// Accept the current bytes as intended, and re-record their checksum.
    Restamp,
    /// Replace a controlled-field value with a known term.
    SetTerm,
    /// Widen the vocabulary to admit the value as written.
    AddTerm,
    /// Correct a misspelled configuration key.
    SetConfigKey,
    /// Correct an unreadable configuration value.
    SetConfigValue,
    /// Rebuild a derived cache from the authority behind it.
    Rebuild,
    /// Regenerate a derived page from the configuration behind it.
    Regenerate,
}

impl RemedyKind {
    /// The stable slug — what a flag, a policy, or an "all of this kind" names.
    pub fn as_str(self) -> &'static str {
        match self {
            RemedyKind::Retarget => "retarget",
            RemedyKind::RemoveLink => "remove-link",
            RemedyKind::Relabel => "relabel",
            RemedyKind::Link => "link",
            RemedyKind::Adopt => "adopt",
            RemedyKind::Reparent => "reparent",
            RemedyKind::TrustRegistry => "trust-registry",
            RemedyKind::TrustDocument => "trust-document",
            RemedyKind::Restamp => "restamp",
            RemedyKind::SetTerm => "set-term",
            RemedyKind::AddTerm => "add-term",
            RemedyKind::SetConfigKey => "set-config-key",
            RemedyKind::SetConfigValue => "set-config-value",
            RemedyKind::Rebuild => "rebuild",
            RemedyKind::Regenerate => "regenerate",
        }
    }
}

impl fmt::Display for RemedyKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One way to repair a [`Finding`] — an **offer**, carrying the [`Fix`] that
/// performs it.
///
/// The unit [`remedies`](Workspace::remedies) deals in, and the reason it
/// replaced a `-> Option<Fix>` signature. That signature could hold one answer,
/// so a finding with two defensible repairs got none — and the split it created
/// tracked not a property of findings but how settled each repair was when it was
/// written. `FixityMismatch` returned a re-stamp while its own documentation said
/// the other arm was the thing prov cannot decide; `IdMismatch` returned "trust
/// the registry" while "trust the document" sat implemented a few variants away;
/// `Orphan` returned nothing at all, so the CLI hardcoded the workspace root as
/// every orphan's parent because a batch had nowhere to ask.
///
/// Remedy is the offer and [`Fix`] the commitment: a `Fix` is fully determined,
/// which is what [`apply_fix`](Workspace::apply_fix) and the journal behind it
/// require. Where prov can enumerate the candidates it emits one remedy per
/// candidate, so a choice is just a longer list and no parameter-passing
/// machinery is needed; where a caller wants something prov did not enumerate (an
/// adoptive parent of its own choosing) it builds the `Fix` itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remedy {
    /// What this remedy does, coarsely — the handle for "do this to all of them".
    pub kind: RemedyKind,
    /// Whether it may be applied without asking.
    pub warrant: Warrant,
    /// A one-line phrasing of the *choice*, where [`Fix`]'s own `Display` phrases
    /// the *action*: "trust the registry" against "set id:… in notes/jul.md".
    pub effect: String,
    /// The repair itself, ready for [`apply_fix`](Workspace::apply_fix).
    pub fix: Fix,
}

impl Remedy {
    /// Assemble a remedy. Private so the phrasing stays in one place.
    fn new(kind: RemedyKind, warrant: Warrant, effect: impl Into<String>, fix: Fix) -> Self {
        Self {
            kind,
            warrant,
            effect: effect.into(),
            fix,
        }
    }
}

impl fmt::Display for Remedy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} [{}]", self.effect, self.warrant)
    }
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
pub(crate) fn reachable_set(
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

impl<FS: Storage, IdP, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// [`reachable_set`], minus any **shadowed attachment payload**
    /// (`attach --opaque`) — the population a pass may parse *as a document*.
    ///
    /// A shadowed payload is still reachable (it must not be reported as an
    /// orphan, and it is still fixity-checked *through its sidecar*), but its
    /// bytes are an exhibit prov promised never to interpret. That is the same
    /// bound [`is_shadowed_payload`](Workspace::is_shadowed_payload) already
    /// holds the flat title and id scans to; this is its reachability-walk
    /// counterpart, for [`vocabulary_findings`](Self::vocabulary_findings) and
    /// [`fixity_findings`](Self::fixity_findings) — the two passes that load
    /// every reachable path and read its frontmatter.
    ///
    /// The listing `is_shadowed_payload` needs is built the same way
    /// [`orphans`](Self::orphans) builds one: the direct children of every
    /// directory the reachable set occupies, so a shadow check costs a set
    /// lookup per candidate extension rather than a stat.
    async fn reachable_documents(
        &self,
        start: &Path,
        census: &[CensusEntry],
        content_bodies: &[PathBuf],
    ) -> Result<BTreeSet<PathBuf>> {
        let reachable = reachable_set(start, census, content_bodies);
        let reached_dirs = Self::reached_dirs(&reachable);
        let listing: BTreeSet<PathBuf> = self
            .direct_child_files(&reached_dirs)
            .await?
            .into_iter()
            .collect();
        let mut documents = BTreeSet::new();
        for path in reachable {
            if !self.is_shadowed_payload(&path, &listing).await {
                documents.insert(path);
            }
        }
        Ok(documents)
    }

    /// Every file the workspace reaches from `start` that actually exists on
    /// disk — [`reachable_set`] over a fresh walk, filtered to real files.
    ///
    /// This is §8's bounded walk expressed as a *file set* rather than a findings
    /// list: the same population `check` validates. [`history_capture`] captures
    /// it (minus prov's two byte-parking stores) precisely so that an event is a
    /// consistent cut across everything the workspace considers its own.
    ///
    /// [`history_capture`]: Workspace::history_capture
    pub async fn reachable_files(&self, start: impl AsRef<Path>) -> Result<BTreeSet<PathBuf>> {
        let start = link::normalize(start);
        let Walk {
            census,
            content_bodies,
            ..
        } = self.walk(&start).await?;
        let mut files = BTreeSet::new();
        for path in reachable_set(&start, &census, &content_bodies) {
            if self.fs().try_exists(&self.root().join(&path)).await? {
                files.insert(path);
            }
        }
        Ok(files)
    }

    /// The **recommended** metadata-only [`Fix`] for `finding`, or `None` when
    /// prov has nothing safe to offer.
    ///
    /// A convenience view over [`remedies`](Self::remedies): the first remedy
    /// that is not [`Warrant::Destructive`], since a caller asking for one
    /// answer is not asking to have an authored link deleted. Callers that want
    /// to *choose* — or that want only the unattended-safe repairs — should use
    /// `remedies` directly and filter on [`Warrant`].
    pub async fn suggest_fix(&self, finding: &Finding) -> Result<Option<Fix>> {
        Ok(self
            .remedies(finding)
            .await?
            .into_iter()
            .find(|r| r.warrant != Warrant::Destructive)
            .map(|r| r.fix))
    }

    /// Files beside a broken target whose names closely resemble the one that is
    /// missing — the candidates a retarget offers, nearest spelling first.
    ///
    /// Deliberately **directory-local**: one `read_dir` of the directory the
    /// target points into, the same call [`exact_name`](Self::exact_name) makes to
    /// decide the link was broken in the first place. A workspace-wide search
    /// would find a document that merely *moved*, but it would also cost a walk
    /// per broken link, and a wrong guess from far away reads as authoritative
    /// when it is not. A link whose target moved to another directory is a rename
    /// prov did not perform, and saying so is more honest than guessing.
    async fn near_matches(&self, doc: &Path, target: &str) -> Vec<PathBuf> {
        let wanted = link::resolve(doc, target);
        let Some(name) = wanted.file_name().and_then(|n| n.to_str()) else {
            return Vec::new();
        };
        let dir = wanted.parent().unwrap_or(Path::new(""));
        let Ok(entries) = self.fs().read_dir(&self.root().join(dir)).await else {
            return Vec::new();
        };
        let mut scored: Vec<(usize, PathBuf)> = entries
            .iter()
            // Same population `direct_child_files` walks: real files, nothing
            // hidden. A directory whose name resembles the missing one is not
            // somewhere a link can point.
            .filter(|entry| entry.file_type().is_file())
            .filter_map(|entry| entry.file_name()?.to_str().map(str::to_owned))
            .filter(|name| !name.starts_with('.'))
            .filter_map(|candidate| {
                // The same tight threshold `textdist::nearest` uses: recognized
                // spellings are distinctive enough that an ordinary sibling never
                // falls inside it.
                let distance = crate::textdist::levenshtein(name, &candidate);
                (1..=2)
                    .contains(&distance)
                    .then(|| (distance, dir.join(candidate)))
            })
            .collect();
        scored.sort();
        scored.into_iter().map(|(_, path)| path).collect()
    }

    /// The text at `span` in `doc`'s body, **only when twig itself reported that
    /// span as a link** — the predicate that decides whether a body-link finding
    /// gets remedies at all.
    ///
    /// DESIGN §8 refuses to edit body prose, and the reason is a real one: a
    /// lexical `[[…]]` scan cannot tell a link from a Python list comprehension.
    /// [`link::parsed_link_spans`] is the part of that scan which *can* — it is
    /// twig's own `link` nodes — so a span it reports is a link a parser
    /// recognized, and the objection does not reach it. A wikilink span is never
    /// in this set (twig has no wikilink concept, it only masks code), so
    /// `[[…]]` stays diagnosis-only exactly as before.
    async fn parsed_body_link(&self, doc: &Path, span: &Range<usize>) -> Option<String> {
        let (_, parsed) = self.load(doc).await.ok()?;
        link::parsed_link_spans(doc, &parsed.body)
            .contains(span)
            .then(|| parsed.body.get(span.clone()).map(str::to_owned))
            .flatten()
    }

    /// How `doc` writes its `relation` entry that reaches `wanted` — the handle
    /// [`Fix::RemoveEntry`] addresses by, recovered for a finding that names the
    /// target as a resolved path rather than as written text.
    async fn written_target_for(
        &self,
        doc: &Path,
        relation: &str,
        wanted: &Path,
    ) -> Option<String> {
        let (_, parsed) = self.load(doc).await.ok()?;
        parsed
            .meta
            .get(relation)?
            .link_strings()
            .into_iter()
            .find(|raw| {
                self.resolve_link(doc, &Link::parse(raw)) == Target::Path(wanted.to_path_buf())
            })
            .map(|raw| Link::parse(&raw).target)
    }

    /// The remedy pair every unresolvable-link finding shares: point it at each
    /// plausible target, or drop it.
    ///
    /// One shape serves a broken path, a dangling id, a malformed id and an
    /// ambiguous alias because they differ only in *why* the target does not
    /// resolve, never in what can be done about it. `candidates` is whatever the
    /// finding could supply — near-matches on disk, the documents sharing an
    /// alias — and may be empty, in which case dropping the link is all that is
    /// left.
    ///
    /// A body site yields remedies only for a span twig parsed
    /// ([`parsed_body_link`](Self::parsed_body_link)); otherwise the list is
    /// empty and the finding stays diagnosis-only.
    async fn link_remedies(
        &self,
        doc: &Path,
        site: &LinkSite,
        written: &str,
        candidates: &[PathBuf],
        retarget_warrant: Warrant,
    ) -> Result<Vec<Remedy>> {
        let mut out = Vec::new();
        match site {
            LinkSite::Relation(relation) => {
                for candidate in candidates {
                    let to = link::path_text(self.link_style(), doc, candidate);
                    out.push(Remedy::new(
                        RemedyKind::Retarget,
                        retarget_warrant,
                        format!("point it at {}", candidate.display()),
                        Fix::RetargetEntry {
                            doc: doc.to_path_buf(),
                            relation: relation.clone(),
                            from: written.to_string(),
                            to,
                        },
                    ));
                }
                out.push(Remedy::new(
                    RemedyKind::RemoveLink,
                    Warrant::Destructive,
                    format!("remove it from {relation}"),
                    Fix::RemoveEntry {
                        doc: doc.to_path_buf(),
                        relation: relation.clone(),
                        target: written.to_string(),
                    },
                ));
            }
            LinkSite::Body(span) => {
                let Some(from) = self.parsed_body_link(doc, span).await else {
                    return Ok(Vec::new());
                };
                for candidate in candidates {
                    let to = link::path_text(self.link_style(), doc, candidate);
                    out.push(Remedy::new(
                        RemedyKind::Retarget,
                        retarget_warrant,
                        format!("point it at {}", candidate.display()),
                        Fix::RetargetBodyLink {
                            doc: doc.to_path_buf(),
                            span: span.clone(),
                            from: from.clone(),
                            to,
                        },
                    ));
                }
                out.push(Remedy::new(
                    RemedyKind::RemoveLink,
                    Warrant::Destructive,
                    "unlink it, keeping its text".to_string(),
                    Fix::RemoveBodyLink {
                        doc: doc.to_path_buf(),
                        span: span.clone(),
                        from,
                    },
                ));
            }
        }
        Ok(out)
    }

    /// The documents that could plausibly adopt an orphan: every container in its
    /// own directory, then in each directory above it up to the workspace root,
    /// nearest first.
    ///
    /// Structural, not by filename — a candidate is a document that *declares the
    /// spanning relation*, which is what being a container actually means. That
    /// distinction matters here more than most places: `init`'s
    /// `index`/`readme` name check is documented as collision-avoidance wearing
    /// detection's clothes, and inheriting it would make a structureless
    /// `README.md` look like a parent.
    ///
    /// Nearest-first ordering is the whole value over the CLI's old hardcoded
    /// root: a file dropped into `notes/2026/` almost always belongs to
    /// `notes/2026/`'s own node, and the root is merely the last resort — which
    /// it still is, since the walk ends there.
    async fn adoptive_parents(&self, orphan: &Path) -> Vec<PathBuf> {
        let Ok((spanning, _)) = self.spanning_pair() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut dir = orphan.parent().map(Path::to_path_buf);
        while let Some(current) = dir {
            if let Ok(entries) = self.fs().read_dir(&self.root().join(&current)).await {
                let mut here: Vec<PathBuf> = Vec::new();
                for entry in entries {
                    let Some(name) = entry.file_name().and_then(|n| n.to_str()) else {
                        continue;
                    };
                    if !entry.file_type().is_file() || name.starts_with('.') {
                        continue;
                    }
                    let candidate = current.join(name);
                    if candidate == orphan {
                        continue;
                    }
                    if let Ok((_, parsed)) = self.load(&candidate).await
                        && parsed.meta.get(&spanning).is_some()
                    {
                        here.push(candidate);
                    }
                }
                here.sort();
                out.extend(here);
            }
            dir = match current.parent() {
                Some(parent) if current != Path::new("") => Some(parent.to_path_buf()),
                _ => None,
            };
        }
        out
    }

    /// The workspace root document, as reached from any document inside it —
    /// what the vocabulary and config lookups a remedy needs are anchored on.
    async fn root_doc_from(&self, doc: &Path) -> Result<PathBuf> {
        let (_, inverse) = self.spanning_pair()?;
        self.spanning_root(doc, &inverse).await
    }

    /// Every repair prov can offer for `finding`, most-recommended first.
    ///
    /// Empty means prov genuinely has nothing to do — either because the repair
    /// is outside prov (bytes a transport has not delivered yet, a `spec` newer
    /// than this build) or because performing it would destroy the evidence of
    /// what went wrong ([`Finding::RecycledBytesMissing`],
    /// [`Finding::HistoryBlobMissing`]).
    ///
    /// Where more than one repair is defensible, they are all here and the caller
    /// picks; see [`Remedy`] for why that replaced a single-answer signature.
    /// Enumeration is cheap enough to do per finding on demand — the only
    /// candidate search is a broken link's near-match scan, one directory listing.
    pub async fn remedies(&self, finding: &Finding) -> Result<Vec<Remedy>> {
        match finding {
            Finding::MissingInverse {
                doc: parent,
                child,
                inverse,
            } => {
                // A child that claims no other parent has nothing to weigh: the
                // back-link is the only reading of the parent's own claim.
                //
                // A child that names a *rival* parent is a different question
                // wearing the same finding: adding a second claim would author the
                // duplicate containment rather than repair anything, so the two
                // answers are the duplicate's two.
                let (_, child_doc) = self.load(child).await?;
                if child_doc.meta.get(inverse).is_some() {
                    return self.contested_parent_remedies(parent, child).await;
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
                Ok(vec![Remedy::new(
                    RemedyKind::Link,
                    Warrant::Derived,
                    format!("declare {inverse} back to {}", parent.display()),
                    Fix::AddInverse {
                        doc: child.clone(),
                        relation: inverse.clone(),
                        parent: parent.clone(),
                        title,
                    },
                )])
            }
            // The registry is the durable, tombstone-bearing side, so it leads —
            // but the document's claim is a real alternative, not a mistake, and
            // `RegisterId` has always been able to honor it.
            Finding::IdMismatch {
                doc,
                frontmatter,
                registry: Some(reg),
            } => Ok(vec![
                Remedy::new(
                    RemedyKind::TrustRegistry,
                    Warrant::Judgment,
                    format!("trust the registry — make the document say id:{reg}"),
                    Fix::SetId {
                        doc: doc.clone(),
                        id: reg.clone(),
                    },
                ),
                Remedy::new(
                    RemedyKind::TrustDocument,
                    Warrant::Judgment,
                    format!("trust the document — register id:{frontmatter} at this path"),
                    Fix::RegisterId {
                        doc: doc.clone(),
                        id: frontmatter.clone(),
                    },
                ),
            ]),
            // The registry records no id for this path *and* assigns the claimed
            // id to another document. Neither side can be honored mechanically:
            // trusting the document would give one id two homes, and there is no
            // registry id to trust instead. A human has to say which document is
            // really the one that id names.
            Finding::IdMismatch { registry: None, .. } => Ok(Vec::new()),
            // Adopt the self-stored id into the registry. No rival claim exists —
            // the registry simply has not heard of this id yet.
            Finding::UnregisteredId { doc, frontmatter } => Ok(vec![Remedy::new(
                RemedyKind::TrustDocument,
                Warrant::Derived,
                format!(
                    "register id:{frontmatter} → {} in the registry",
                    doc.display()
                ),
                Fix::RegisterId {
                    doc: doc.clone(),
                    id: frontmatter.clone(),
                },
            )]),
            // Write the registry's id down into the document, making it
            // self-describing. Unambiguous — unlike `IdMismatch`, there is no
            // competing claim to weigh, only a home that is still empty.
            Finding::UnstampedId { doc, registry } => Ok(vec![Remedy::new(
                RemedyKind::TrustRegistry,
                Warrant::Derived,
                format!("stamp id:{registry} into the document"),
                Fix::SetId {
                    doc: doc.clone(),
                    id: registry.clone(),
                },
            )]),
            // Re-stamp to the current bytes — accept the change. The current hash
            // is already computed in the finding, so no re-read is needed.
            //
            // `Judgment`, not `Derived`, and this is the finding that proves the
            // distinction is worth drawing: prov cannot tell an intended
            // out-of-band edit from bit-rot, so accepting the bytes is a choice
            // being made, not a fact being restated. Its opposite — restoring from
            // a backup or a captured event — is not offered yet.
            Finding::FixityMismatch { doc, actual, .. } => Ok(vec![Remedy::new(
                RemedyKind::Restamp,
                Warrant::Judgment,
                "accept the current bytes and re-record the checksum",
                Fix::RestampFixity {
                    doc: doc.clone(),
                    hash: actual.clone(),
                },
            )]),
            // Relabel the stale link to the target's current title. Resolve its
            // (id) target to a path so the fix can locate it; a link that no
            // longer resolves has nothing safe to relabel.
            Finding::StaleLabel {
                doc,
                target,
                expected,
                ..
            } => match self.resolve_link(doc, &Link::parse(target)) {
                crate::Target::Path(path) => Ok(vec![Remedy::new(
                    RemedyKind::Relabel,
                    Warrant::Derived,
                    format!("relabel as \"{expected}\""),
                    Fix::RelabelLink {
                        doc: doc.clone(),
                        target: path,
                        new_label: expected.clone(),
                    },
                )]),
                _ => Ok(Vec::new()),
            },
            // Rebuild the drifted index from its own directory. Unambiguous: the
            // event documents are the authority and the index only caches them,
            // so there is no competing claim to weigh.
            Finding::HistoryIndexStale { index, .. } => Ok(vec![Remedy::new(
                RemedyKind::Rebuild,
                Warrant::Derived,
                format!("rebuild {} from its own directory", index.display()),
                Fix::RebuildHistoryIndex {
                    index: index.clone(),
                },
            )]),
            // Re-declare the store the root has stopped pointing at. Unambiguous
            // because the finding never guessed: it fires only for a store at the
            // conventional path, so the pointer goes back to the one place prov
            // would have put it.
            Finding::HistoryStoreUnlinked { root, store } => Ok(vec![Remedy::new(
                RemedyKind::Link,
                Warrant::Derived,
                format!("declare the history store at {}", store.display()),
                Fix::LinkHistoryStore {
                    root: root.clone(),
                    store: store.clone(),
                },
            )]),
            // Rewrite the derived page. Unambiguous for the same reason as the
            // index rebuild: configuration is the authority and the page only
            // restates it, so there is no competing claim to weigh.
            Finding::AboutStale { path, expected, .. } => Ok(vec![Remedy::new(
                RemedyKind::Regenerate,
                Warrant::Derived,
                format!("regenerate {} from the configuration", path.display()),
                Fix::RegenerateAbout {
                    path: path.clone(),
                    content: expected.clone(),
                },
            )]),
            // A path with nothing behind it. The candidates are whatever sits
            // beside where it pointed under a name close enough to be the one
            // meant; where nothing is, dropping the link is the only offer.
            Finding::BrokenLink { doc, site, target } => {
                let candidates = self.near_matches(doc, target).await;
                self.link_remedies(doc, site, target, &candidates, Warrant::Judgment)
                    .await
            }
            // The file is right there under a different spelling, and the finding
            // already carries the exact on-disk name — so the repair restates a
            // fact rather than choosing between readings. `Derived`, and one of
            // the few link repairs that is.
            Finding::CaseMismatch {
                doc,
                site,
                target,
                actual,
            } => {
                let corrected = link::resolve(doc, target).with_file_name(actual);
                let mut remedies = self
                    .link_remedies(doc, site, target, &[corrected], Warrant::Derived)
                    .await?;
                // Nothing here is broken — the link resolves, just not portably —
                // so removing it is not one of the answers.
                remedies.retain(|r| r.kind != RemedyKind::RemoveLink);
                Ok(remedies)
            }
            // A check character that does not check out: prov cannot tell which id
            // was meant, so there is nothing to retarget *to*, only the link to
            // drop. (Recovering a mistyped id from a uniquely-resolving neighbor
            // is `next-steps`' gated malformed-id heal, and is not this.)
            Finding::MalformedId { doc, site, target } => {
                self.link_remedies(doc, site, target, &[], Warrant::Judgment)
                    .await
            }
            // A well-formed id no live entry resolves. Same shape: nothing to
            // point it at that prov can name, so the offer is to drop it. The
            // tombstoned case is deliberately not treated differently — "that
            // document was deleted" is a reason to *keep* the dangling reference
            // as a record of intent at least as often as it is a reason to cut it.
            Finding::DanglingId { doc, site, id, .. } => {
                self.link_remedies(doc, site, &format!("prov:{id}"), &[], Warrant::Judgment)
                    .await
            }
            // Several documents answer to the name, and the finding already lists
            // them — so the choice is exactly the candidate set, and disambiguating
            // by pointing at one of them is the repair.
            Finding::AmbiguousAlias {
                doc,
                site,
                name,
                candidates,
            } => {
                self.link_remedies(doc, site, name, candidates, Warrant::Judgment)
                    .await
            }
            // Two containers claim one document, which the spanning tree cannot
            // represent. Either this container is the real parent — in which case
            // `reparent` settles it, repointing the child and dropping the rival's
            // entry — or it is not, and its entry is the one to go.
            Finding::DuplicateContainment { doc, target } => {
                let Target::Path(child) = self.resolve_link(doc, &Link::parse(target)) else {
                    return Ok(Vec::new());
                };
                self.contested_parent_remedies(doc, &child).await
            }
            // A document on disk that the tree does not reach. The repair is to
            // attach it, and the only question is where — so every container above
            // it is offered, nearest first, rather than the workspace root by
            // fiat as the CLI used to.
            Finding::Orphan { doc, root } => {
                let mut parents = self.adoptive_parents(doc).await;
                // The root is always a home, even before it has children to prove
                // it is a container — which is exactly the state a workspace is in
                // right after `init`, and the state in which orphans are most
                // likely to be found.
                if !parents.contains(root) && root != doc {
                    parents.push(root.clone());
                }
                Ok(parents
                    .into_iter()
                    .map(|parent| {
                        Remedy::new(
                            RemedyKind::Adopt,
                            Warrant::Judgment,
                            format!("adopt it under {}", parent.display()),
                            Fix::Adopt {
                                child: doc.clone(),
                                parent,
                            },
                        )
                    })
                    .collect())
            }
            // A value its vocabulary does not admit. Offer the spellings close
            // enough to be what was meant — and, when the value is genuinely new
            // rather than retired, admitting it as a term.
            Finding::UnknownTerm {
                doc,
                field,
                value,
                retired,
            } => {
                let mut out = Vec::new();
                let Some((store, vocab)) = self.vocabulary_for(doc, field).await? else {
                    return Ok(out);
                };
                for candidate in vocab.live_term_names() {
                    if (1..=2).contains(&crate::textdist::levenshtein(value, &candidate)) {
                        out.push(Remedy::new(
                            RemedyKind::SetTerm,
                            Warrant::Judgment,
                            format!("spell it {candidate}"),
                            Fix::SetFieldValue {
                                doc: doc.clone(),
                                field: field.clone(),
                                from: value.clone(),
                                to: candidate,
                            },
                        ));
                    }
                }
                // Never for a retired term: the entry already exists, carrying an
                // id and a gloss, and writing a bare key over it would un-retire
                // it *and* destroy both. Reviving a retirement is a deliberate
                // edit to the vocabulary, not a repair to this document.
                if !retired {
                    out.push(Remedy::new(
                        RemedyKind::AddTerm,
                        Warrant::Judgment,
                        format!("admit {value} as a term in {}", store.display()),
                        Fix::AddTerm {
                            store,
                            term: value.clone(),
                        },
                    ));
                }
                Ok(out)
            }
            // An open vocabulary, so the value is legal — it just does not match a
            // spelling already in use. Both readings are real: drift to be
            // corrected, or a genuinely new term.
            Finding::TermNearMiss {
                doc,
                field,
                value,
                suggestion,
            } => {
                let mut out = vec![Remedy::new(
                    RemedyKind::SetTerm,
                    Warrant::Judgment,
                    format!("spell it {suggestion}"),
                    Fix::SetFieldValue {
                        doc: doc.clone(),
                        field: field.clone(),
                        from: value.clone(),
                        to: suggestion.clone(),
                    },
                )];
                if let Some((store, _)) = self.vocabulary_for(doc, field).await? {
                    out.push(Remedy::new(
                        RemedyKind::AddTerm,
                        Warrant::Judgment,
                        format!("keep {value} and admit it as a term"),
                        Fix::AddTerm {
                            store,
                            term: value.clone(),
                        },
                    ));
                }
                Ok(out)
            }
            // A key `apply` silently ignores. The value the author wrote was right
            // — only its key was misspelled — so the repair keeps the value,
            // position, and comments and renames the key over them.
            Finding::ConfigIssue { doc, issue } => match &issue.kind {
                crate::config::ConfigIssueKind::UnknownKey { suggestion } => {
                    let from = self.config_key_path(doc, &issue.key).await;
                    let to = match issue.key.rsplit_once('.') {
                        Some((prefix, _)) => format!("{prefix}.{suggestion}"),
                        None => suggestion.clone(),
                    };
                    Ok(vec![Remedy::new(
                        RemedyKind::SetConfigKey,
                        Warrant::Judgment,
                        format!("spell the key {suggestion}"),
                        Fix::SetConfigKey {
                            doc: doc.clone(),
                            from,
                            to,
                        },
                    )])
                }
                crate::config::ConfigIssueKind::InvalidValue { expected, .. } => {
                    let key = self.config_key_path(doc, &issue.key).await;
                    Ok(expected
                        .iter()
                        .map(|spelling| {
                            Remedy::new(
                                RemedyKind::SetConfigValue,
                                Warrant::Judgment,
                                format!("set it to {spelling}"),
                                Fix::SetConfigValue {
                                    doc: doc.clone(),
                                    key: key.clone(),
                                    value: spelling.clone(),
                                },
                            )
                        })
                        .collect())
                }
                // The spanning relation's inverse is `many`, so no single-parent
                // tree can form. Repairing it means changing that relation's
                // cardinality — a decision about what the workspace *is*, not a
                // key to rewrite.
                crate::config::ConfigIssueKind::SpanningNotSingleParent { .. } => Ok(Vec::new()),
            },
            _ => Ok(Vec::new()),
        }
    }

    /// The two answers to "this document is claimed by a parent it does not claim
    /// back, and it already names another one": make this parent the real one, or
    /// let this parent let go.
    ///
    /// Shared by [`Finding::DuplicateContainment`] and the contested half of
    /// [`Finding::MissingInverse`] because they are one situation seen from the
    /// two ends — the container that has a child it should not, and the child that
    /// is contained twice. `reparent` is what settles it either way, since it
    /// repoints the child *and* removes the rival's entry in one change set.
    async fn contested_parent_remedies(&self, parent: &Path, child: &Path) -> Result<Vec<Remedy>> {
        let (spanning, _) = self.spanning_pair()?;
        let mut out = vec![Remedy::new(
            RemedyKind::Reparent,
            Warrant::Judgment,
            format!(
                "make {} the parent of {}",
                parent.display(),
                child.display()
            ),
            Fix::Reparent {
                child: child.to_path_buf(),
                parent: parent.to_path_buf(),
            },
        )];
        if let Some(written) = self.written_target_for(parent, &spanning, child).await {
            out.push(Remedy::new(
                RemedyKind::RemoveLink,
                Warrant::Destructive,
                format!("drop {} from {spanning} here", child.display()),
                Fix::RemoveEntry {
                    doc: parent.to_path_buf(),
                    relation: spanning,
                    target: written,
                },
            ));
        }
        Ok(out)
    }

    /// The vocabulary governing `field`, and the store document it lives in —
    /// what a term repair needs to know before it can offer anything.
    ///
    /// Anchored by walking up the spanning relation from `doc` to the workspace
    /// root, because a [`Finding`] names the document that has the problem and
    /// not the root the configuration hangs off. That is the same move
    /// `collect_inbound_rewrites` makes to bound a census.
    async fn vocabulary_for(
        &self,
        doc: &Path,
        field: &str,
    ) -> Result<Option<(PathBuf, crate::vocabulary::Vocabulary)>> {
        let root = self.root_doc_from(doc).await?;
        let config = self.effective_config(&root).await?;
        let Some(pointer) = config
            .fields
            .get(field)
            .and_then(|spec| spec.vocabulary.as_ref())
        else {
            return Ok(None);
        };
        let Some(store) = self.vocabulary_path(&root, pointer) else {
            return Ok(None);
        };
        Ok(self
            .load_vocabulary(&root, pointer)
            .await?
            .map(|vocab| (store, vocab)))
    }

    /// A config issue's key, qualified from the *document's* root rather than the
    /// config block's.
    ///
    /// [`ConfigIssue::key`](crate::config::ConfigIssue) is dotted from the block
    /// it was found in, and prov reads two surfaces: a dedicated config document,
    /// where the block *is* the document, and the root's inline `prov:` block,
    /// where it is one key down. An editor addresses the file, so the prefix has
    /// to come back before the key can be written to.
    async fn config_key_path(&self, doc: &Path, key: &str) -> String {
        let inline = self
            .load(doc)
            .await
            .ok()
            .and_then(|(_, parsed)| parsed.meta.get(crate::config::ROOT_CONFIG_KEY).cloned())
            .is_some_and(|block| {
                key.split('.')
                    .next()
                    .is_some_and(|head| block.get(head).is_some())
            });
        if inline {
            format!("{}.{key}", crate::config::ROOT_CONFIG_KEY)
        } else {
            key.to_string()
        }
    }

    /// Check the workspace reachable from `start`, returning every finding.
    /// An empty result means the reachable graph holds its invariants. This is
    /// the findings view over [`census`](Workspace::census): each forward link
    /// that fails to resolve becomes a finding, joined with the structural
    /// findings (unreadable document, duplicate containment, missing inverse)
    /// the walk raises from traversal state.
    /// Nine passes follow, most of them over the same documents: the walk loads
    /// every reachable document to build the census, and the fixity, orphan,
    /// vocabulary and label passes each go back for their own reasons. A
    /// [`read_scope`](Self::read_scope) makes that composition cost one read per
    /// document instead of one per pass — and ends here, so nothing survives
    /// into whatever the caller does next.
    pub async fn check(&self, start: impl AsRef<Path>) -> Result<Vec<Finding>> {
        let _scope = self.read_scope();
        let start = start.as_ref();
        let Walk {
            census,
            mut findings,
            content_bodies,
        } = self.walk(start).await?;
        for entry in &census {
            // An `about` pointer at a page that is not there is not a broken
            // link. The page is *derived* (spec §4, generated prose — "a pure
            // function of configuration, therefore discardable"), so an absent
            // one is a page waiting to be written, not a reference to something
            // lost. Reporting it here would also duplicate
            // [`Finding::AboutStale`], which says the same thing and names the
            // repair; and a generic broken-link fix would invite the wrong one.
            if matches!(entry.resolution, Resolution::Broken)
                && matches!(&entry.site, LinkSite::Relation(name)
                    if Some(name.as_str()) == self.relations().about_relation())
            {
                continue;
            }
            findings.extend(entry.finding());
        }
        findings.extend(self.orphans(start, &census, &content_bodies).await?);
        findings.extend(
            self.fixity_findings(start, &census, &content_bodies)
                .await?,
        );
        findings.extend(self.config_findings(start).await?);
        findings.extend(self.store_findings(start).await?);
        // The bin index's own validity is established above; this reads its
        // records and checks the parked bytes they point at, which live in an
        // unreached subtree no other pass visits.
        findings.extend(self.recycle_findings(start).await?);
        findings.extend(
            self.vocabulary_findings(start, &census, &content_bodies)
                .await?,
        );
        findings.extend(self.stale_label_findings(&census).await?);
        // The history store's interior is validated from the directories
        // themselves rather than by this walk — descent is spanning-only, and the
        // store is reached through the one-way `history` pointer. See
        // [`history_findings`](Workspace::history_findings).
        findings.extend(self.history_findings(start).await?);
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
            // A type-only field declares no vocabulary, so it has no store.
            if let Some(pointer) = &spec.vocabulary
                && let Some(p) = self.vocabulary_path(start, pointer)
            {
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

    /// Verify that every recycle-bin record's **parked bytes are still on
    /// disk**, emitting one [`Finding::RecycledBytesMissing`] per record that
    /// has lost any of them.
    ///
    /// This is the one pass that looks inside `recyclebin/items/`. Those bytes
    /// are deliberately unreached — nothing links into the items directory, so
    /// §8's reachability-bounded walk ignores them, which is what keeps a binned
    /// document from being reported as an orphan. The same exclusion means a
    /// vanished parked file is invisible to every other check, and would surface
    /// only as a raw rename failure inside
    /// [`restore`](crate::Workspace::restore).
    ///
    /// Checked per *record* rather than per file: a separated document parks its
    /// metadata and its prose body, both move in one [`ChangeSet`](crate::ChangeSet),
    /// and losing either one makes the record equally unrestorable — so the two
    /// paths belong in one finding, not two.
    ///
    /// A bin index that cannot be loaded contributes nothing here; the walk
    /// reports it as `Unreadable` and [`store_findings`](Self::store_findings)
    /// reports a markdown carrier, and neither wants a second complaint layered
    /// on top. A record carrying no `bin` key at all (only reachable by hand
    /// editing) likewise names no parked path, so it yields no finding —
    /// malformed record *shape* is a separate question from missing bytes.
    async fn recycle_findings(&self, start: &Path) -> Result<Vec<Finding>> {
        let Some(index) = self.recycle_bin_path(start).await? else {
            return Ok(Vec::new());
        };
        let Ok((_, bin_doc)) = self.load(&index).await else {
            return Ok(Vec::new());
        };
        let records = bin_doc
            .meta
            .get("deleted")
            .and_then(Value::as_sequence)
            .map(<[Value]>::to_vec)
            .unwrap_or_default();

        let mut findings = Vec::new();
        for record in &records {
            let field = |key: &str| record.get(key).and_then(Value::as_str);
            // `from` is the record's identity — the path the user would name to
            // restore it, and so the path the finding must report.
            let Some(from) = field("from") else { continue };
            let mut missing = Vec::new();
            // `bin` holds the document itself; `body_bin` the prose body of a
            // separated document, present only when one travelled with it.
            for key in ["bin", "body_bin"] {
                if let Some(parked) = field(key) {
                    let parked = PathBuf::from(parked);
                    if !self.fs().try_exists(&self.root().join(&parked)).await? {
                        missing.push(parked);
                    }
                }
            }
            if !missing.is_empty() {
                findings.push(Finding::RecycledBytesMissing {
                    index: index.clone(),
                    from: PathBuf::from(from),
                    missing,
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
        let mut vocabs: Vec<(
            String,
            crate::config::OpenClosed,
            crate::vocabulary::Vocabulary,
        )> = Vec::new();
        for (field, spec) in &config.fields {
            // Membership is only checkable for a field that names a vocabulary;
            // a type-only field has nothing to be a member of.
            let Some(pointer) = &spec.vocabulary else {
                continue;
            };
            if let Ok(Some(vocab)) = self.load_vocabulary(start, pointer).await {
                vocabs.push((field.clone(), spec.values, vocab));
            }
        }
        if vocabs.is_empty() {
            return Ok(Vec::new());
        }

        // The reachable document set (mirrors `fixity_findings`), minus any
        // shadowed attachment payload — its `fields` values are an exhibit's,
        // not this workspace's, and `attach --opaque` promises never to read
        // them (see `reachable_documents`).
        let reachable = self
            .reachable_documents(start, census, content_bodies)
            .await?;

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
    ///
    /// A **shadowed** payload (`attach --opaque`) is excluded from the loop below
    /// via [`reachable_documents`](Self::reachable_documents) — it is never
    /// parsed for a `content_hash` of its *own*, because that field, if present,
    /// belongs to the exhibit, not this workspace. Its actual fixity is still
    /// checked: the sidecar beside it is an ordinary (unshadowed) document whose
    /// own `content_hash` covers the payload's bytes via `content_attr`, so that
    /// check runs the normal way, through the sidecar.
    async fn fixity_findings(
        &self,
        start: &Path,
        census: &[CensusEntry],
        content_bodies: &[PathBuf],
    ) -> Result<Vec<Finding>> {
        let reachable = self
            .reachable_documents(start, census, content_bodies)
            .await?;

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
        let Some(text) = self.stamped(&path, &original, &doc, updated).await? else {
            return Ok(false);
        };
        let mut cs = self.change();
        cs.write(&path, text);
        self.commit(cs).await?;
        Ok(true)
    }

    /// Write `text` to the document at `path`, stamping what the write itself
    /// implies — the counterpart to
    /// [`record_content_update`](Self::record_content_update) for a caller who
    /// *has* the new text rather than one reconciling text already on disk.
    ///
    /// Same two stamps, decided the same way and documented there: `content_hash`
    /// where the workspace records fixity for this document's kind and the bytes
    /// have drifted, and the `updated` frontmatter field when a caller supplies
    /// one. The difference is only when they are applied. Stamping text on its way
    /// to the disk costs one journaled write; stamping it afterwards costs the
    /// first write, a read back, and a second write of the same document —
    /// three atomic-write protocols where one will do, and, on a synced
    /// filesystem, two uploads of one document per save.
    ///
    /// It is sound to stamp first because neither stamp can invalidate the other
    /// or itself: both live in the frontmatter, and the hash covers the body (or a
    /// `content` sibling), so amending the frontmatter cannot change what the hash
    /// is *of*. The hash the caller's text arrived carrying is what drift is
    /// measured against, exactly as if the text had been read back.
    pub async fn save_document(
        &mut self,
        path: impl AsRef<Path>,
        text: &str,
        updated: Option<(&str, &str)>,
    ) -> Result<()> {
        let path = link::normalize(path.as_ref());
        // The same clamp `load` applies on the way in, owed here too: `path` may
        // have come from a document's own metadata, and this call reaches the
        // filesystem without `load` in front of it to refuse an escape.
        if link::escapes_root(&path) {
            return Err(crate::error::Error::Escape(path));
        }
        let doc = crate::document::Document::parse(&path, text)?;
        let stamped = self.stamped(&path, text, &doc, updated).await?;
        let mut cs = self.change();
        // No stamp applying is not "nothing to do" here, the way it is for
        // `record_content_update`: the caller's text is the point, stamped or not.
        cs.write(&path, stamped.unwrap_or_else(|| text.to_string()));
        self.commit(cs).await
    }

    /// Apply to `text` the frontmatter stamps a content change implies, given the
    /// `doc` that text parses to. `None` when neither applies and `text` already
    /// says what it should.
    ///
    /// The shared middle of [`save_document`](Self::save_document) and
    /// [`record_content_update`](Self::record_content_update). Both make the same
    /// two decisions over the same text; all that differs is whether that text is
    /// on its way to the disk or already there.
    async fn stamped(
        &self,
        path: &Path,
        text: &str,
        doc: &crate::document::Document,
        updated: Option<(&str, &str)>,
    ) -> Result<Option<String>> {
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
        let mut text = text.to_string();
        let mut stamped = false;
        if let Some(hash) = new_hash {
            text = crate::edit::set_in_text(
                &text,
                doc.carrier,
                "content_hash",
                fig::Value::Str(hash),
            )?;
            stamped = true;
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
            stamped = true;
        }
        Ok(stamped.then_some(text))
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
        let reachable = reachable_set(start, census, content_bodies);
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
            .map(|doc| Finding::Orphan {
                doc,
                root: link::normalize(start),
            })
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
            } else if self.id_storage().stamps_frontmatter()
                && let Some(reg) = self.index().id_for_path(&path)
            {
                // The other direction: a stamping workspace expects every
                // registered document to carry its own id, and this one does not
                // (a workspace converted from registry-only storage, or an `id`
                // stripped out of band). The registry is the authority — the id
                // is already live and linked to — so the repair writes it down.
                structural.push(Finding::UnstampedId {
                    doc: path.clone(),
                    registry: reg,
                });
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
        // Two repairs are whole mutation verbs rather than metadata edits, and
        // each already lands its own change set — with the cycle refusals,
        // idempotence, and three-document ordering that make them safe. Delegating
        // keeps one implementation of "put this document under that parent";
        // reproducing it here would be a second, worse one.
        match fix {
            Fix::Adopt { child, parent } => return self.adopt(child, parent).await,
            Fix::Reparent { child, parent } => return self.reparent(child, parent).await,
            _ => {}
        }
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
            // Rebuild the index from its own directory. Wholesale rather than a
            // surgical edit, because the index is derived: the events are the
            // authority, so the repaired file is byte-identical to one a fresh
            // capture would have written.
            Fix::RebuildHistoryIndex { index } => {
                let text = self.history_index_text(index).await?;
                cs.write(index, text);
            }
            // One frontmatter key, set the way a bootstrap capture would have set
            // it — the same `history_pointer_text` path, so a re-declared pointer
            // is spelled identically to an originally-declared one.
            Fix::LinkHistoryStore { root, store } => {
                let text = self.history_pointer_text(root, store).await?;
                cs.write(root, text);
            }
            // Wholesale, like the index rebuild and for the same reason: the page
            // is derived, so the repaired file is byte-identical to one a fresh
            // `prov about` would have written.
            Fix::RegenerateAbout { path, content } => {
                cs.write(path, content.clone());
            }
            // Drop the entry, addressed by how it is written — the one handle a
            // link with nothing behind it still offers.
            Fix::RemoveEntry {
                doc,
                relation,
                target,
            } => {
                let (text, parsed) = self.load(doc).await?;
                if let Some(updated) =
                    maintain::remove_written_entry(&text, &parsed, relation, target)?
                {
                    cs.write(doc, updated);
                }
            }
            // Repoint it, keeping the label and wrapper the author chose.
            Fix::RetargetEntry {
                doc,
                relation,
                from,
                to,
            } => {
                let (text, parsed) = self.load(doc).await?;
                if let Some(updated) =
                    maintain::retarget_written_entry(&text, &parsed, relation, from, to)?
                {
                    cs.write(doc, updated);
                }
            }
            // A body splice, guarded by the text at the span: see
            // `splice_body_span` for why the span alone is not trusted.
            Fix::RetargetBodyLink {
                doc,
                span,
                from,
                to,
            } => {
                let (text, parsed) = self.load(doc).await?;
                let rendered = Link::parse(from).with_target(to.clone()).render();
                let updated =
                    maintain::splice_body_span(&text, &parsed.body, span, from, &rendered)?;
                cs.write(doc, updated);
            }
            // Unlink rather than delete: the label is prose the author wrote, and
            // only the reference is broken. A bare link with no label leaves its
            // target text behind, which is the same words minus the brackets.
            Fix::RemoveBodyLink { doc, span, from } => {
                let (text, parsed) = self.load(doc).await?;
                let link = Link::parse(from);
                let kept = link.label.clone().unwrap_or_else(|| link.target.clone());
                let updated = maintain::splice_body_span(&text, &parsed.body, span, from, &kept)?;
                cs.write(doc, updated);
            }
            // Correct a controlled value in place. Not a link, so the replacement
            // is written verbatim rather than rendered through the link seam.
            Fix::SetFieldValue {
                doc,
                field,
                from,
                to,
            } => {
                let (text, parsed) = self.load(doc).await?;
                if let Some(updated) =
                    maintain::replace_written_entry(&text, &parsed, field, from, to)?
                {
                    cs.write(doc, updated);
                }
            }
            // A bare `term:` key — the shape `Vocabulary::from_meta` reads as a
            // live term carrying no metadata. Anything richer (an id, a gloss) is
            // the author's to add afterward.
            Fix::AddTerm { store, term } => {
                let (text, parsed) = self.load(store).await?;
                let Some(carrier) = parsed.carrier else {
                    return Err(crate::error::Error::Structure(format!(
                        "{} has no metadata block to add a term to",
                        store.display()
                    )));
                };
                let mut editor = crate::edit::MetaEditor::open(&text, carrier)?;
                editor.set_value(
                    &[fig::Segment::Key("terms"), fig::Segment::Key(term)],
                    fig::Value::Null,
                )?;
                cs.write(store, editor.render()?);
            }
            // Rename the key, keeping its value, position, and comments — the
            // whole reason a misspelled axis is worth repairing mechanically is
            // that the value the author wrote was right all along.
            Fix::SetConfigKey { doc, from, to } => {
                let (text, parsed) = self.load(doc).await?;
                let leaf = to.rsplit('.').next().unwrap_or(to);
                let mut editor = crate::edit::MetaEditor::open(
                    &text,
                    parsed.carrier.ok_or_else(|| {
                        crate::error::Error::Structure(format!(
                            "{} has no metadata block to edit",
                            doc.display()
                        ))
                    })?,
                )?;
                editor.replace_key(&crate::edit::key_path(from), leaf)?;
                cs.write(doc, editor.render()?);
            }
            Fix::SetConfigValue { doc, key, value } => {
                let (text, parsed) = self.load(doc).await?;
                let updated = crate::edit::set_in_text(
                    &text,
                    parsed.carrier,
                    key,
                    fig::Value::Str(value.clone()),
                )?;
                cs.write(doc, updated);
            }
            // Delegated above — a whole verb, not a metadata edit.
            Fix::Adopt { .. } | Fix::Reparent { .. } => unreachable!("delegated above"),
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

    pub(super) fn write(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    pub(super) fn tempdir(tag: &str) -> PathBuf {
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

    /// `check` is nine passes over one graph, and several of them want the same
    /// documents. The read scope it opens is what makes that composition cost
    /// one read per document instead of one per pass.
    #[test]
    fn check_reads_each_document_once() {
        let dir = tempdir("memo");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n---\n");
        write(
            &dir,
            "a.md",
            &format!(
                "---\npart_of: index.md\ncontent_hash: {}\n---\nalpha\n",
                crate::fixity::digest(b"alpha\n")
            ),
        );
        let fs = crate::fs::CountingFs::default();
        let ws = Workspace::builder(fs.clone()).root(&dir).build();

        assert_eq!(block_on(ws.check("index.md")).unwrap(), vec![]);
        // `a.md` is wanted by the walk (for the census) and again by the fixity
        // pass (to hash its body) at the very least.
        assert_eq!(
            fs.doc_reads(&dir, "a.md"),
            1,
            "a document was read more than once inside one `check`"
        );
        assert_eq!(fs.doc_reads(&dir, "index.md"), 1);
    }

    /// The scope is bounded by the operation, so a second `check` re-reads
    /// everything. That is the property that lets the memo have no invalidation
    /// policy at all: it never outlives the operation that opened it.
    #[test]
    fn a_second_check_reads_the_documents_again() {
        let dir = tempdir("memo-scope");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n---\n");
        write(&dir, "a.md", "---\npart_of: index.md\n---\n");
        let fs = crate::fs::CountingFs::default();
        let ws = Workspace::builder(fs.clone()).root(&dir).build();

        block_on(ws.check("index.md")).unwrap();
        block_on(ws.check("index.md")).unwrap();
        assert_eq!(
            fs.doc_reads(&dir, "a.md"),
            2,
            "a memo outlived the operation that opened it"
        );
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
        assert!(
            stale.is_some(),
            "expected a StaleLabel finding, got {findings:?}"
        );

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

    /// Every parked-bytes test needs the same starting point: a workspace with
    /// one document binned and its bytes sitting in `recyclebin/items/`.
    fn with_a_binned_note(tag: &str) -> PathBuf {
        let dir = tempdir(tag);
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- note.md\n---\n",
        );
        write(
            &dir,
            "note.md",
            "---\ntitle: My Note\npart_of: index.md\n---\nbody\n",
        );
        let mut w = Workspace::builder(StdFs).root(&dir).build();
        block_on(w.recycle(Path::new("note.md"), false, None)).unwrap();
        assert!(dir.join("recyclebin/items/note.md").exists());
        dir
    }

    fn missing_bytes(dir: &Path) -> Vec<Finding> {
        let ws = Workspace::builder(StdFs).root(dir).build();
        block_on(ws.check("index.md"))
            .unwrap()
            .into_iter()
            .filter(|f| matches!(f, Finding::RecycledBytesMissing { .. }))
            .collect()
    }

    #[test]
    fn a_bin_record_with_its_parked_bytes_intact_is_not_flagged() {
        let dir = with_a_binned_note("bin-intact");
        assert_eq!(missing_bytes(&dir), vec![]);
    }

    #[test]
    fn a_bin_record_whose_parked_bytes_vanished_is_reported() {
        // The bytes go behind prov's back — a partial sync, a transport pruning
        // an unreached subtree, a hand-deletion inside the bin. The record still
        // promises a restore it can no longer perform.
        let dir = with_a_binned_note("bin-vanished");
        std::fs::remove_file(dir.join("recyclebin/items/note.md")).unwrap();

        let findings = missing_bytes(&dir);
        assert_eq!(
            findings,
            vec![Finding::RecycledBytesMissing {
                index: PathBuf::from("recyclebin/index.yaml"),
                from: PathBuf::from("note.md"),
                missing: vec![PathBuf::from("recyclebin/items/note.md")],
            }],
        );
        // The finding names the document the user would ask to restore, not the
        // internal parked path — which is all `restore`'s raw rename failure
        // would have given them.
        assert!(
            findings[0]
                .to_string()
                .contains("note.md cannot be restored"),
            "{}",
            findings[0]
        );
        // Diagnosis only: there is no mechanical repair for absent bytes.
        let ws = Workspace::builder(StdFs).root(&dir).build();
        assert!(block_on(ws.suggest_fix(&findings[0])).unwrap().is_none());
    }

    #[test]
    fn a_separated_document_that_lost_only_its_body_is_reported() {
        // A separated document parks two files and they move as one ChangeSet,
        // so losing either makes the record equally unrestorable — but the
        // finding must name which one actually went.
        let dir = tempdir("bin-body");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- note.md\n---\n",
        );
        write(
            &dir,
            "note.md",
            "---\ntitle: My Note\npart_of: index.md\ncontent: note.body.md\n---\n",
        );
        write(&dir, "note.body.md", "prose\n");
        let mut w = Workspace::builder(StdFs).root(&dir).build();
        block_on(w.recycle(Path::new("note.md"), false, None)).unwrap();
        std::fs::remove_file(dir.join("recyclebin/items/note.body.md")).unwrap();

        assert_eq!(
            missing_bytes(&dir),
            vec![Finding::RecycledBytesMissing {
                index: PathBuf::from("recyclebin/index.yaml"),
                from: PathBuf::from("note.md"),
                missing: vec![PathBuf::from("recyclebin/items/note.body.md")],
            }],
        );
    }

    #[test]
    fn emptying_the_bin_removes_the_records_with_the_bytes_so_nothing_is_reported() {
        // The load-bearing negative: `empty_bin` deletes exactly these bytes on
        // purpose. If the finding could not tell a deliberate purge from a loss,
        // every emptied bin would report one finding per document ever deleted.
        let dir = with_a_binned_note("bin-emptied");
        let mut w = Workspace::builder(StdFs).root(&dir).build();
        assert_eq!(block_on(w.empty_bin(Path::new("index.md"))).unwrap(), 1);
        assert!(!dir.join("recyclebin/items/note.md").exists());
        assert_eq!(missing_bytes(&dir), vec![]);
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
    fn check_diagnoses_a_broken_markdown_body_link() {
        // A markdown body link to a missing file is a broken-link finding — the
        // diagnosis half of body-link ownership. A wikilink to nowhere was
        // already caught above; this is parity for markdown/djot links.
        let dir = tempdir("md-body-check");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- a.md\n---\n",
        );
        write(
            &dir,
            "a.md",
            "---\npart_of: index.md\n---\nSee [gone](nope.md).\n",
        );

        let ws = Workspace::builder(StdFs).root(&dir).build();
        let findings = block_on(ws.check("index.md")).unwrap();
        assert!(
            findings.iter().any(|f| matches!(f,
                Finding::BrokenLink { doc, site: LinkSite::Body(_), target }
                    if doc == &PathBuf::from("a.md") && target == "nope.md")),
            "expected a broken markdown body link, got {findings:?}"
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
    fn unstamped_id_is_found_and_written_into_the_document() {
        use crate::config::IdStorage;
        use crate::identity::Id;
        use crate::index::IndexStore;

        // A registry-only workspace: the id lives in the registry and the
        // document carries nothing — exactly the state a vault is in the moment
        // it converts to stamping storage.
        let dir = tempdir("unstamped-id");
        write(&dir, "index.md", "---\ntitle: Home\n---\n");
        let build = |storage| {
            let mut ws = Workspace::builder(StdFs)
                .root(&dir)
                .identity(Minter::lazy(9))
                .index(FileIndex::new(fig::Format::Yaml))
                .id_storage(storage)
                .build();
            ws.index_mut()
                .register(&Id("aaaaaaa".into()), Path::new("index.md"));
            ws
        };

        // Registry-only storage does not expect a stamp, so it raises nothing.
        let ws = build(IdStorage::Registry);
        assert!(
            block_on(ws.check("index.md")).unwrap().is_empty(),
            "registry-only storage should not ask for a stamp"
        );

        // Stamping storage names the gap.
        let mut ws = build(IdStorage::Frontmatter);
        let findings = block_on(ws.check("index.md")).unwrap();
        let f = findings
            .iter()
            .find(|f| matches!(f, Finding::UnstampedId { registry, .. } if registry.0 == "aaaaaaa"))
            .expect("expected an UnstampedId")
            .clone();

        // And the fix writes the registry's id down into the document, leaving
        // the registry itself untouched.
        let fix = block_on(ws.suggest_fix(&f)).unwrap().unwrap();
        assert!(matches!(&fix, Fix::SetId { id, .. } if id.0 == "aaaaaaa"));
        block_on(ws.apply_fix(&fix)).unwrap();
        assert!(
            std::fs::read_to_string(dir.join("index.md"))
                .unwrap()
                .contains("id: aaaaaaa")
        );
        assert_eq!(
            ws.index().id_for_path(Path::new("index.md")),
            Some(Id("aaaaaaa".into()))
        );
        assert!(
            block_on(ws.check("index.md")).unwrap().is_empty(),
            "stamped → clean"
        );
    }

    #[test]
    fn a_contested_parent_offers_a_choice_rather_than_a_single_fix() {
        // index claims a.md, but a.md already claims a *different* parent — a
        // contested containment, not a mechanical missing-inverse. There is no
        // one right answer, which is exactly why it now yields two remedies
        // instead of the `None` a single-answer signature had to return: settle
        // it in this parent's favor, or let this parent let go.
        //
        // Neither is `Derived`, so an unattended sweep still touches nothing —
        // the old refusal survives as a warrant rather than as silence.
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
        let remedies = block_on(ws.remedies(mi)).unwrap();
        assert_eq!(
            remedies.iter().map(|r| r.kind).collect::<Vec<_>>(),
            vec![RemedyKind::Reparent, RemedyKind::RemoveLink],
            "both readings of a contested containment: {remedies:?}"
        );
        assert!(
            remedies.iter().all(|r| r.warrant != Warrant::Derived),
            "a contested parent is never mechanical: {remedies:?}"
        );
        assert!(
            matches!(
                block_on(ws.suggest_fix(mi)).unwrap(),
                Some(Fix::Reparent { .. })
            ),
            "the recommendation is the non-destructive one"
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
            findings.iter().any(
                |f| matches!(f, Finding::Orphan { doc, .. } if doc == &PathBuf::from("loose.md"))
            ),
            "{findings:?}"
        );
        // …but the linked files (root + reachable child) are not.
        assert!(
            !findings
                .iter()
                .any(|f| matches!(f, Finding::Orphan { doc, .. }
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
                |f| matches!(f, Finding::Orphan { doc, .. } if doc == &PathBuf::from("notes/stray.md"))
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
    fn save_document_lands_the_new_text_and_both_stamps_in_one_write() {
        // The stamp-first path has to reach the same place the read-back path
        // does: new body on disk, `updated` set, `content_hash` covering the body
        // that was actually saved — and `check` agreeing the hash is true, which
        // is the assertion that would fail if the hash were taken of the wrong
        // bytes (the old body, or the text before its frontmatter was stamped).
        use crate::config::Fixity;
        let dir = tempdir("save-document");
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

        let edited = "---\ntitle: Note\npart_of: index.md\n---\na different body\n";
        block_on(w.save_document("note.md", edited, Some(("updated", "2026-08-06T09:00:00Z"))))
            .unwrap();

        let text = std::fs::read_to_string(dir.join("note.md")).unwrap();
        assert!(text.contains("a different body"), "{text}");
        assert!(text.contains("updated: 2026-08-06T09:00:00Z"), "{text}");
        assert!(text.contains("content_hash: sha256:"), "{text}");
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn save_document_agrees_byte_for_byte_with_writing_then_recording() {
        // The change is meant to be a saving, not a difference: the same edit
        // through the old two-step route must produce the same file. Anything
        // else would be a silent format or ordering change in every document a
        // client saves.
        use crate::config::Fixity;
        let one_step = tempdir("save-equivalence-new");
        let two_step = tempdir("save-equivalence-old");
        let edited = "---\ntitle: Note\npart_of: index.md\n---\nrewritten\n";
        let stamp = Some(("updated", "2026-08-06T09:00:00Z"));

        for dir in [&one_step, &two_step] {
            write(
                dir,
                "index.md",
                "---\ntitle: Home\ncontents:\n- note.md\n---\n",
            );
            write(
                dir,
                "note.md",
                "---\ntitle: Note\npart_of: index.md\n---\nbody\n",
            );
        }

        let mut w = Workspace::builder(StdFs)
            .root(&one_step)
            .fixity(Fixity::Full)
            .build();
        block_on(w.save_document("note.md", edited, stamp)).unwrap();

        let mut old = Workspace::builder(StdFs)
            .root(&two_step)
            .fixity(Fixity::Full)
            .build();
        std::fs::write(two_step.join("note.md"), edited).unwrap();
        assert!(block_on(old.record_content_update("note.md", stamp)).unwrap());

        assert_eq!(
            std::fs::read_to_string(one_step.join("note.md")).unwrap(),
            std::fs::read_to_string(two_step.join("note.md")).unwrap(),
        );
    }

    #[test]
    fn save_document_writes_the_text_even_when_no_stamp_applies() {
        // With fixity off and no `updated` field there is nothing to stamp — which
        // makes `record_content_update` a no-op, but must not make a *save* one.
        // The text is the point; the stamps are bookkeeping around it.
        use crate::config::Fixity;
        let dir = tempdir("save-document-unstamped");
        write(&dir, "index.md", "---\ntitle: Home\n---\nold\n");
        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .fixity(Fixity::Off)
            .build();

        block_on(w.save_document("index.md", "---\ntitle: Home\n---\nnew\n", None)).unwrap();

        let text = std::fs::read_to_string(dir.join("index.md")).unwrap();
        assert!(text.contains("new"), "{text}");
        assert!(!text.contains("content_hash"), "{text}");
    }

    #[test]
    fn save_document_refuses_a_path_that_escapes_the_root() {
        let dir = tempdir("save-document-escape");
        write(&dir, "index.md", "---\ntitle: Home\n---\n");
        let mut w = Workspace::builder(StdFs).root(&dir).build();

        let err = block_on(w.save_document("../escape.md", "---\ntitle: X\n---\n", None))
            .expect_err("an escaping path must be refused");

        assert!(matches!(err, crate::error::Error::Escape(_)), "{err:?}");
        assert!(!dir.parent().unwrap().join("escape.md").exists());
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

    #[test]
    fn a_check_diff_separates_what_an_operation_fixed_broke_and_inherited() {
        let orphan = |name: &str| Finding::Orphan {
            doc: PathBuf::from(name),
            root: PathBuf::from("index.md"),
        };
        let (repaired, inherited, broken) = (orphan("a.md"), orphan("b.md"), orphan("c.md"));

        let diff = CheckDiff::between(
            &[repaired.clone(), inherited.clone()],
            &[inherited.clone(), broken.clone()],
        );
        assert_eq!(diff.fixed, vec![repaired]);
        assert_eq!(diff.introduced, vec![broken]);
        assert_eq!(diff.pre_existing, vec![inherited]);
        // The verdict is about what this operation *did* — a workspace still
        // dirty from problems it inherited is not this operation's failure.
        assert!(!diff.is_clean());
        assert!(!diff.is_empty());
    }

    #[test]
    fn a_check_diff_over_an_unchanged_workspace_is_empty_but_not_clean_of_findings() {
        let standing = Finding::Orphan {
            doc: PathBuf::from("a.md"),
            root: PathBuf::from("index.md"),
        };
        let same = std::slice::from_ref(&standing);
        let diff = CheckDiff::between(same, same);
        assert!(diff.fixed.is_empty() && diff.introduced.is_empty());
        assert_eq!(diff.pre_existing, vec![standing]);
        // Broke nothing, so clean — even though the workspace is not.
        assert!(diff.is_clean());
        assert!(diff.is_empty());

        // And two clean runs agree on everything.
        assert_eq!(CheckDiff::between(&[], &[]), CheckDiff::default());
    }
}

// The `about` page's own findings — its staleness check and the autofix that
// repairs it. YAML fixtures, so gated like the rest of this module's tests.
#[cfg(all(test, feature = "yaml"))]
mod about_tests {
    use super::tests::{tempdir, write};
    use super::*;
    use crate::about::AboutContext;
    use crate::config::WorkspaceConfig;
    use crate::exec::block_on;
    use crate::fs::StdFs;

    fn fixture(tag: &str) -> (std::path::PathBuf, WorkspaceConfig, AboutContext) {
        let dir = tempdir(tag);
        write(
            &dir,
            "index.md",
            "---\ntitle: T\nabout: about.md\n---\nbody\n",
        );
        let config = WorkspaceConfig::default();
        let ctx = AboutContext::new("index.md", "0.0.0");
        (dir, config, ctx)
    }

    #[test]
    fn a_current_page_is_not_a_finding() {
        let (dir, config, ctx) = fixture("about-current");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        block_on(ws.write_about(std::path::Path::new("index.md"), &config, &ctx)).unwrap();
        let finding =
            block_on(ws.check_about(std::path::Path::new("index.md"), &config, &ctx)).unwrap();
        assert!(finding.is_none(), "{finding:?}");
    }

    #[test]
    fn a_missing_page_the_pointer_promises_is_a_finding_but_not_a_broken_link() {
        let (dir, config, ctx) = fixture("about-missing");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let finding =
            block_on(ws.check_about(std::path::Path::new("index.md"), &config, &ctx)).unwrap();
        assert!(matches!(
            finding,
            Some(Finding::AboutStale { missing: true, .. })
        ));

        // The derived page is discardable, so a pointer at an absent one must not
        // also surface as a broken link — that would be a duplicate finding
        // inviting the wrong repair.
        let findings = block_on(ws.check("index.md")).unwrap();
        assert!(
            !findings
                .iter()
                .any(|f| matches!(f, Finding::BrokenLink { target, .. } if target == "about.md")),
            "{findings:?}"
        );
    }

    #[test]
    fn a_hand_edited_page_is_stale_and_the_fix_restores_it() {
        let (dir, config, ctx) = fixture("about-edited");
        let mut ws = Workspace::builder(StdFs).root(&dir).build();
        let path =
            block_on(ws.write_about(std::path::Path::new("index.md"), &config, &ctx)).unwrap();
        let generated = std::fs::read_to_string(dir.join(&path)).unwrap();

        std::fs::write(
            dir.join(&path),
            generated.replace("is the root", "is definitely the root"),
        )
        .unwrap();
        let finding = block_on(ws.check_about(std::path::Path::new("index.md"), &config, &ctx))
            .unwrap()
            .expect("stale");
        assert!(matches!(
            finding,
            Finding::AboutStale { missing: false, .. }
        ));

        let fix = block_on(ws.suggest_fix(&finding)).unwrap().expect("a fix");
        assert!(matches!(fix, Fix::RegenerateAbout { .. }));
        block_on(ws.apply_fix(&fix)).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(&path)).unwrap(), generated);
    }

    #[test]
    fn a_version_bump_in_the_byline_is_not_staleness() {
        // The comparison is over the body only, so upgrading prov must not mark
        // every workspace on earth stale and rewrite files whose prose is
        // identical.
        let (dir, config, ctx) = fixture("about-version");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let path =
            block_on(ws.write_about(std::path::Path::new("index.md"), &config, &ctx)).unwrap();
        let page = std::fs::read_to_string(dir.join(&path)).unwrap();
        std::fs::write(
            dir.join(&path),
            page.replace("generated_by: prov 0.0.0", "generated_by: prov 99.0.0"),
        )
        .unwrap();

        let finding =
            block_on(ws.check_about(std::path::Path::new("index.md"), &config, &ctx)).unwrap();
        assert!(
            finding.is_none(),
            "a stale byline is not a stale page: {finding:?}"
        );
    }

    #[test]
    fn a_prov_upgrade_between_generation_and_check_is_not_staleness() {
        // The byline test above only ever tampers with the metadata line by
        // hand; it never actually varies `ctx.version`, so it would pass even
        // if the body itself leaked the version. Here the page is *generated*
        // under one version and *checked* under another — the scenario that
        // happens for real when two synced devices run different prov builds,
        // or a workspace is checked the day after an upgrade. Nothing about
        // the page's prose changed, so this must not be a finding.
        let (dir, config, old_ctx) = fixture("about-upgrade");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        block_on(ws.write_about(std::path::Path::new("index.md"), &config, &old_ctx)).unwrap();

        let new_ctx = AboutContext::new("index.md", "99.0.0");
        let finding =
            block_on(ws.check_about(std::path::Path::new("index.md"), &config, &new_ctx)).unwrap();
        assert!(
            finding.is_none(),
            "a page generated under one prov version must read as current under \
             another: {finding:?}"
        );
    }

    #[test]
    fn a_workspace_that_asked_for_no_page_is_silent() {
        let dir = tempdir("about-off");
        // No `about` pointer, and the axis is off: nothing was promised.
        write(&dir, "index.md", "---\ntitle: T\n---\nbody\n");
        let config = WorkspaceConfig {
            about: crate::config::About::Off,
            ..WorkspaceConfig::default()
        };
        let ctx = AboutContext::new("index.md", "0.0.0");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let finding =
            block_on(ws.check_about(std::path::Path::new("index.md"), &config, &ctx)).unwrap();
        assert!(finding.is_none(), "{finding:?}");
    }

    #[test]
    fn the_derived_page_is_never_parked_in_the_history_store() {
        // Capturing it would park a new blob on every config change to store
        // something the captured config already determines — and the first
        // capture bootstraps the store, which changes what the page says, so the
        // captured copy would be one the capture itself invalidated.
        let (dir, config, ctx) = fixture("about-not-captured");
        write(
            &dir,
            "index.md",
            "---\ntitle: T\nabout: about.md\n---\nbody\n",
        );
        let ws = Workspace::builder(StdFs).root(&dir).build();
        block_on(ws.write_about(std::path::Path::new("index.md"), &config, &ctx)).unwrap();

        let set = block_on(ws.history_capture_set(std::path::Path::new("index.md"))).unwrap();
        assert!(
            !set.iter().any(|p| p == std::path::Path::new("about.md")),
            "the derived page must stay out of the capture set: {set:?}"
        );
        // The root itself is still captured — only the derived page is excluded.
        assert!(
            set.iter().any(|p| p == std::path::Path::new("index.md")),
            "{set:?}"
        );
    }

    #[test]
    fn a_pointer_left_behind_is_still_checked_even_with_the_axis_off() {
        // Turning the axis off does not retract a promise the root still makes.
        let (dir, _, ctx) = fixture("about-off-but-pointed");
        let config = WorkspaceConfig {
            about: crate::config::About::Off,
            ..WorkspaceConfig::default()
        };
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let finding =
            block_on(ws.check_about(std::path::Path::new("index.md"), &config, &ctx)).unwrap();
        assert!(matches!(
            finding,
            Some(Finding::AboutStale { missing: true, .. })
        ));
    }
}

/// Remedies — the repairs a finding offers when more than one is defensible.
#[cfg(all(test, feature = "yaml"))]
mod remedy_tests {
    use super::tests::{tempdir, write};
    use super::*;
    use crate::exec::block_on;
    use crate::fs::StdFs;

    fn read(dir: &Path, rel: &str) -> String {
        std::fs::read_to_string(dir.join(rel)).unwrap()
    }

    fn kinds(remedies: &[Remedy]) -> Vec<RemedyKind> {
        remedies.iter().map(|r| r.kind).collect()
    }

    /// The single finding matching `want`, or a panic naming what `check` really
    /// found — a remedy test that quietly matched the wrong finding asserts
    /// nothing at all.
    fn sole(findings: &[Finding], want: fn(&Finding) -> bool) -> &Finding {
        let mut hits = findings.iter().filter(|f| want(f));
        let first = hits
            .next()
            .unwrap_or_else(|| panic!("no finding of the wanted shape in {findings:#?}"));
        assert!(hits.next().is_none(), "more than one match: {findings:#?}");
        first
    }

    #[test]
    fn a_broken_relation_link_offers_the_near_match_then_removal() {
        // The shape the whole change exists for: two defensible repairs, ordered
        // by which one prov would stand behind, and the destructive one last.
        let dir = tempdir("remedy-broken");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- notes.md\n- dya.md\n---\n",
        );
        write(&dir, "notes.md", "---\npart_of: index.md\n---\n");
        write(&dir, "day.md", "---\ntitle: Day\n---\n");
        let ws = Workspace::builder(StdFs).root(&dir).build();

        let findings = block_on(ws.check("index.md")).unwrap();
        let broken = sole(&findings, |f| matches!(f, Finding::BrokenLink { .. }));
        let remedies = block_on(ws.remedies(broken)).unwrap();

        assert_eq!(
            kinds(&remedies),
            vec![RemedyKind::Retarget, RemedyKind::RemoveLink],
            "{remedies:#?}"
        );
        assert_eq!(remedies[0].warrant, Warrant::Judgment);
        assert_eq!(remedies[1].warrant, Warrant::Destructive);
        assert!(
            remedies[0].effect.contains("day.md"),
            "the near match is named: {}",
            remedies[0].effect
        );
    }

    #[test]
    fn a_broken_link_with_nothing_beside_it_offers_only_removal() {
        // No candidate is not a failure to look — it is the honest answer, and
        // `suggest_fix` must not turn a deletion into a recommendation.
        let dir = tempdir("remedy-broken-bare");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- gone.md\n---\n",
        );
        let ws = Workspace::builder(StdFs).root(&dir).build();

        let findings = block_on(ws.check("index.md")).unwrap();
        let broken = sole(&findings, |f| matches!(f, Finding::BrokenLink { .. }));
        let remedies = block_on(ws.remedies(broken)).unwrap();

        assert_eq!(kinds(&remedies), vec![RemedyKind::RemoveLink]);
        assert!(
            block_on(ws.suggest_fix(broken)).unwrap().is_none(),
            "a destructive-only finding recommends nothing"
        );
    }

    #[test]
    fn removing_a_broken_entry_leaves_the_others_alone() {
        let dir = tempdir("remedy-remove-entry");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- a.md\n- gone.md\n- b.md\n---\n",
        );
        write(&dir, "a.md", "---\npart_of: index.md\n---\n");
        write(&dir, "b.md", "---\npart_of: index.md\n---\n");
        let mut ws = Workspace::builder(StdFs).root(&dir).build();

        let findings = block_on(ws.check("index.md")).unwrap();
        let broken = sole(&findings, |f| matches!(f, Finding::BrokenLink { .. }));
        let fix = block_on(ws.remedies(broken)).unwrap()[0].fix.clone();
        block_on(ws.apply_fix(&fix)).unwrap();

        let text = read(&dir, "index.md");
        assert!(
            !text.contains("gone.md"),
            "the broken entry is gone: {text}"
        );
        assert!(text.contains("a.md") && text.contains("b.md"), "{text}");
        assert!(
            block_on(ws.check("index.md")).unwrap().is_empty(),
            "and the workspace is clean"
        );
    }

    #[test]
    fn a_case_mismatch_is_derived_and_never_offers_removal() {
        // The exact on-disk name is in the finding, so nothing is being chosen —
        // the one link repair a sweep may apply unattended. And the link is not
        // broken, so deleting it is not one of the readings.
        let dir = tempdir("remedy-case");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- Notes.md\n---\n",
        );
        write(&dir, "notes.md", "---\npart_of: index.md\n---\n");
        let mut ws = Workspace::builder(StdFs).root(&dir).build();

        let findings = block_on(ws.check("index.md")).unwrap();
        let Some(mismatch) = findings
            .iter()
            .find(|f| matches!(f, Finding::CaseMismatch { .. }))
        else {
            // A case-sensitive filesystem never raises it; nothing to test.
            return;
        };
        let remedies = block_on(ws.remedies(mismatch)).unwrap();
        assert_eq!(
            kinds(&remedies),
            vec![RemedyKind::Retarget],
            "{remedies:#?}"
        );
        assert_eq!(remedies[0].warrant, Warrant::Derived);

        block_on(ws.apply_fix(&remedies[0].fix.clone())).unwrap();
        assert!(read(&dir, "index.md").contains("notes.md"));
    }

    #[test]
    fn an_ambiguous_alias_offers_each_document_that_claims_the_name() {
        let dir = tempdir("remedy-alias");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- one/dup.md\n- two/dup.md\n- ref.md\n---\n",
        );
        write(
            &dir,
            "one/dup.md",
            "---\ntitle: Dup\npart_of: /index.md\n---\n",
        );
        write(
            &dir,
            "two/dup.md",
            "---\ntitle: Dup\npart_of: /index.md\n---\n",
        );
        write(
            &dir,
            "ref.md",
            "---\ntitle: Ref\npart_of: index.md\nlinks: '[[Dup]]'\n---\n",
        );
        let ws = Workspace::builder(StdFs)
            .root(&dir)
            .relations(
                crate::relation::RelationSet::new()
                    .with(crate::relation::Relation::many("contents").inverse("part_of"))
                    .with(crate::relation::Relation::one("part_of").inverse("contents"))
                    .with(crate::relation::Relation::many("links"))
                    .spanning("contents"),
            )
            .build();

        let findings = block_on(ws.check("index.md")).unwrap();
        let Some(ambiguous) = findings
            .iter()
            .find(|f| matches!(f, Finding::AmbiguousAlias { .. }))
        else {
            panic!("expected an ambiguous alias in {findings:#?}");
        };
        let remedies = block_on(ws.remedies(ambiguous)).unwrap();
        assert_eq!(
            kinds(&remedies),
            vec![
                RemedyKind::Retarget,
                RemedyKind::Retarget,
                RemedyKind::RemoveLink
            ],
            "one per candidate, then the drop: {remedies:#?}"
        );
    }

    #[test]
    fn an_orphan_offers_its_containers_nearest_first() {
        // The finding the CLI used to answer with a hardcoded root. A file in
        // `notes/` almost always belongs to `notes/`'s own node, so that is what
        // leads; the root is the last resort, not the only one.
        let dir = tempdir("remedy-orphan");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- notes/notes.md\n---\n",
        );
        write(
            &dir,
            "notes/notes.md",
            "---\ntitle: Notes\npart_of: /index.md\ncontents:\n- /notes/kept.md\n---\n",
        );
        write(
            &dir,
            "notes/kept.md",
            "---\npart_of: /notes/notes.md\n---\n",
        );
        write(&dir, "notes/stray.md", "---\ntitle: Stray\n---\n");
        let mut ws = Workspace::builder(StdFs).root(&dir).build();

        let findings = block_on(ws.check("index.md")).unwrap();
        let orphan = sole(&findings, |f| matches!(f, Finding::Orphan { .. }));
        let remedies = block_on(ws.remedies(orphan)).unwrap();

        assert!(
            remedies.iter().all(|r| r.kind == RemedyKind::Adopt),
            "{remedies:#?}"
        );
        assert!(
            matches!(&remedies[0].fix, Fix::Adopt { parent, .. } if parent == Path::new("notes/notes.md")),
            "the nearest container leads: {:#?}",
            remedies[0].fix
        );
        assert!(
            remedies.iter().any(
                |r| matches!(&r.fix, Fix::Adopt { parent, .. } if parent == Path::new("index.md"))
            ),
            "and the root is still offered: {remedies:#?}"
        );

        block_on(ws.apply_fix(&remedies[0].fix.clone())).unwrap();
        assert!(
            block_on(ws.check("index.md")).unwrap().is_empty(),
            "adopting it makes the workspace clean"
        );
    }

    #[test]
    fn an_orphan_beside_a_childless_root_is_still_offered_it() {
        // A root with no children yet declares no spanning relation, so it fails
        // every structural test for being a container — while being, in fact, the
        // only one there is. That is the state a workspace is in immediately after
        // `init`, which is precisely when orphans turn up, so the finding carries
        // the root the walk started from rather than leaving it to be inferred.
        let dir = tempdir("remedy-orphan-bare-root");
        write(&dir, "index.md", "---\ntitle: Root\n---\n");
        write(&dir, "stray.md", "---\ntitle: Stray\n---\n");
        let mut ws = Workspace::builder(StdFs).root(&dir).build();

        let findings = block_on(ws.check("index.md")).unwrap();
        let orphan = sole(&findings, |f| matches!(f, Finding::Orphan { .. }));
        let remedies = block_on(ws.remedies(orphan)).unwrap();
        assert!(
            matches!(&remedies[..], [r] if matches!(&r.fix, Fix::Adopt { parent, .. } if parent == Path::new("index.md"))),
            "the root is the home of last resort: {remedies:#?}"
        );

        block_on(ws.apply_fix(&remedies[0].fix.clone())).unwrap();
        assert!(block_on(ws.check("index.md")).unwrap().is_empty());
    }

    #[test]
    fn an_id_mismatch_offers_both_sides_of_the_disagreement() {
        // Two applyable fixes had existed all along; only the signature had room
        // for one. Order still puts the registry first — it is the durable,
        // tombstone-bearing side — but the document's claim is now reachable.
        let dir = tempdir("remedy-idmismatch");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\nid: aaaaaaaa\nregistry: registry.yaml\n---\n",
        );
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let mismatch = Finding::IdMismatch {
            doc: PathBuf::from("index.md"),
            frontmatter: Id("aaaaaaaa".into()),
            registry: Some(Id("bbbbbbbb".into())),
        };
        let remedies = block_on(ws.remedies(&mismatch)).unwrap();
        assert_eq!(
            kinds(&remedies),
            vec![RemedyKind::TrustRegistry, RemedyKind::TrustDocument]
        );
        assert!(remedies.iter().all(|r| r.warrant == Warrant::Judgment));
    }

    #[test]
    fn a_body_wikilink_stays_diagnosis_only_but_a_parsed_link_does_not() {
        // The rule the body-prose exception turns on: twig has no wikilink
        // concept, so a `[[…]]` span is lexical and DESIGN §8's objection still
        // reaches it. A `[label](target)` span is a link an actual parser
        // reported, and it does not.
        let dir = tempdir("remedy-body");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\n---\nSee [Day](dya.md) and [[also-gone]].\n",
        );
        write(&dir, "day.md", "---\ntitle: Day\n---\n");
        let mut ws = Workspace::builder(StdFs).root(&dir).build();

        let findings = block_on(ws.check("index.md")).unwrap();
        let parsed = findings
            .iter()
            .find(|f| matches!(f, Finding::BrokenLink { target, .. } if target == "dya.md"))
            .unwrap_or_else(|| panic!("expected the markdown link finding in {findings:#?}"));
        let wiki = findings
            .iter()
            .find(|f| matches!(f, Finding::BrokenLink { target, .. } if target == "also-gone"))
            .unwrap_or_else(|| panic!("expected the wikilink finding in {findings:#?}"));

        assert!(
            block_on(ws.remedies(wiki)).unwrap().is_empty(),
            "a wikilink in prose is never rewritten"
        );
        let remedies = block_on(ws.remedies(parsed)).unwrap();
        assert_eq!(
            kinds(&remedies),
            vec![RemedyKind::Retarget, RemedyKind::RemoveLink],
            "{remedies:#?}"
        );

        block_on(ws.apply_fix(&remedies[0].fix.clone())).unwrap();
        let text = read(&dir, "index.md");
        // Retargeted in the workspace's own link style (root-relative by
        // default), label and wrapper intact — the same seam `create` authors
        // through, so a repair reads like every other link in the workspace.
        assert!(
            text.contains("[Day](/day.md)"),
            "retargeted in place: {text}"
        );
        assert!(
            text.contains("[[also-gone]]"),
            "and the wikilink is untouched: {text}"
        );
    }

    #[test]
    fn unlinking_a_body_link_keeps_the_words() {
        let dir = tempdir("remedy-body-unlink");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\n---\nSee [the old plan](gone.md) for context.\n",
        );
        let mut ws = Workspace::builder(StdFs).root(&dir).build();

        let findings = block_on(ws.check("index.md")).unwrap();
        let broken = sole(&findings, |f| matches!(f, Finding::BrokenLink { .. }));
        let remedies = block_on(ws.remedies(broken)).unwrap();
        assert_eq!(kinds(&remedies), vec![RemedyKind::RemoveLink]);

        block_on(ws.apply_fix(&remedies[0].fix.clone())).unwrap();
        let text = read(&dir, "index.md");
        assert!(
            text.contains("See the old plan for context."),
            "the label survives, only the reference goes: {text}"
        );
    }

    #[test]
    fn a_body_fix_refuses_a_span_the_document_has_moved_out_from_under() {
        // A span is an offset into bytes read when `check` ran. Splicing a stale
        // one would corrupt prose silently, which is far worse than declining.
        let dir = tempdir("remedy-body-stale");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\n---\nSee [Day](gone.md).\n",
        );
        let mut ws = Workspace::builder(StdFs).root(&dir).build();
        let findings = block_on(ws.check("index.md")).unwrap();
        let broken = sole(&findings, |f| matches!(f, Finding::BrokenLink { .. }));
        let fix = block_on(ws.remedies(broken)).unwrap()[0].fix.clone();

        // Someone edits the prose between the check and the repair.
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\n---\nA whole new paragraph first.\n\nSee [Day](gone.md).\n",
        );
        let err = block_on(ws.apply_fix(&fix)).unwrap_err();
        assert!(
            err.to_string().contains("changed since it was checked"),
            "it declines rather than splicing blind: {err}"
        );
    }

    #[test]
    fn a_near_miss_term_offers_the_spelling_or_the_vocabulary() {
        let dir = tempdir("remedy-term");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\nconfig: prov.yaml\ncontents:\n- vocab.yaml\n- note.md\n---\n",
        );
        write(
            &dir,
            "prov.yaml",
            "spec: 1\nfields:\n  status:\n    values: open\n    vocabulary: /vocab.yaml\n",
        );
        write(
            &dir,
            "vocab.yaml",
            "title: Statuses\npart_of: /index.md\nvocabulary:\n  field: status\n  values: open\nterms:\n  todo:\n",
        );
        write(
            &dir,
            "note.md",
            "---\ntitle: Note\npart_of: /index.md\nstatus: to-do\n---\n",
        );
        let mut ws = Workspace::builder(StdFs).root(&dir).build();

        let findings = block_on(ws.check("index.md")).unwrap();
        let near = sole(&findings, |f| matches!(f, Finding::TermNearMiss { .. }));
        let remedies = block_on(ws.remedies(near)).unwrap();
        assert_eq!(
            kinds(&remedies),
            vec![RemedyKind::SetTerm, RemedyKind::AddTerm],
            "{remedies:#?}"
        );

        // Taking the second reading widens the vocabulary rather than editing
        // the document — and the workspace goes quiet either way.
        block_on(ws.apply_fix(&remedies[1].fix.clone())).unwrap();
        assert!(read(&dir, "vocab.yaml").contains("to-do"));
        assert!(read(&dir, "note.md").contains("to-do"));
        assert!(block_on(ws.check("index.md")).unwrap().is_empty());
    }

    #[test]
    fn a_retired_term_is_never_offered_the_add_remedy() {
        // Writing a bare `term:` over a retired entry would un-retire it *and*
        // destroy the id and gloss it carries — a repair that loses more than it
        // fixes.
        let dir = tempdir("remedy-retired");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\nconfig: prov.yaml\ncontents:\n- vocab.yaml\n- note.md\n---\n",
        );
        write(
            &dir,
            "prov.yaml",
            "spec: 1\nfields:\n  status:\n    values: closed\n    vocabulary: /vocab.yaml\n",
        );
        write(
            &dir,
            "vocab.yaml",
            "title: Statuses\npart_of: /index.md\nvocabulary:\n  field: status\n  values: closed\nterms:\n  todo:\n  draft:\n    retired: true\n",
        );
        write(
            &dir,
            "note.md",
            "---\ntitle: Note\npart_of: /index.md\nstatus: draft\n---\n",
        );
        let ws = Workspace::builder(StdFs).root(&dir).build();

        let findings = block_on(ws.check("index.md")).unwrap();
        let unknown = sole(&findings, |f| matches!(f, Finding::UnknownTerm { .. }));
        let remedies = block_on(ws.remedies(unknown)).unwrap();
        assert!(
            !remedies.iter().any(|r| r.kind == RemedyKind::AddTerm),
            "a retirement is not reversed by a repair: {remedies:#?}"
        );
    }

    #[test]
    fn a_misspelled_config_key_is_renamed_over_its_value() {
        // The value the author wrote was right all along; only the key was
        // wrong. Renaming keeps the value, and the position, and the comment.
        let dir = tempdir("remedy-config");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\nconfig: prov.yaml\n---\n",
        );
        write(
            &dir,
            "prov.yaml",
            "spec: 1\nreferences:\n  # how links are written\n  notaton: markdown\n",
        );
        let mut ws = Workspace::builder(StdFs).root(&dir).build();

        let findings = block_on(ws.check("index.md")).unwrap();
        let issue = sole(&findings, |f| matches!(f, Finding::ConfigIssue { .. }));
        let remedies = block_on(ws.remedies(issue)).unwrap();
        assert_eq!(kinds(&remedies), vec![RemedyKind::SetConfigKey]);

        block_on(ws.apply_fix(&remedies[0].fix.clone())).unwrap();
        let text = read(&dir, "prov.yaml");
        assert!(text.contains("notation: markdown"), "{text}");
        assert!(
            text.contains("# how links are written"),
            "the comment survives: {text}"
        );
    }
}
