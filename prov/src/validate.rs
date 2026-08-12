//! Validation — integrity findings over the workspace graph, from a root.
//!
//! The sleeper feature (DESIGN §8): walk the spanning tree and report every
//! violated invariant as a [`Finding`] — data, not a panic.
//!
//! Findings are a **view** over [`prov_graph::graph`]'s census
//! ([`Workspace::census`]): every forward link the walk resolves becomes a
//! finding when it fails to resolve cleanly, joined with the structural
//! findings (unreadable document, duplicate containment, missing inverse) the
//! same walk raises from traversal state. See `graph`'s module doc for why the
//! census is ground truth and everything here is downstream of it.
//! [`Workspace::check`] is the findings view. The checks:
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
//! External targets (URLs, `mailto:`) are never checked.
//!
//! **What is deliberately not here.** A finding says what is wrong; it carries
//! no opinion about what to do about it. Repairs live one layer downstream in
//! [`remedy`](crate::remedy) — [`Fix`](crate::remedy::Fix),
//! [`Remedy`](crate::remedy::Remedy), and
//! [`apply_fix`](crate::workspace::Workspace::apply_fix) — for the same reason
//! `graph` stays ignorant of `Finding`: a finding is a *statement*, and most
//! statements admit several defensible answers. Nothing in this module knows
//! how to change a document, and that is what keeps it a view.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::history::HistoryIssue;
use crate::workspace::Workspace;
use prov_graph::content::ContentFormat;
use prov_graph::error::{Error, Result};
use prov_graph::graph::{CensusEntry, LinkSite, Resolution, StructuralFact, Walk, reachable_set};
use prov_graph::identity::Id;
use prov_graph::link;
use prov_graph::meta::Value;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

/// The integrity finding a census entry represents when its target failed to
/// resolve cleanly — `None` for a link that resolves.
///
/// A free function rather than a method because [`CensusEntry`] belongs to
/// `prov-graph`, which deliberately knows nothing of [`Finding`]: the walk
/// reports what it saw, and naming it is this module's job. The orphan rule
/// enforcing that is the boundary working, not fighting it.
fn finding_for(entry: &CensusEntry) -> Option<Finding> {
    let doc = entry.source.clone();
    let site = entry.site.clone();
    let target = entry.target_text.clone();
    {
        match &entry.resolution {
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
            // `Foreign` sits with the resolutions that produce nothing, not
            // with the ones that produce a finding: prov has no evidence about
            // a workspace it cannot see, and a link reported broken on no
            // evidence is a false positive the host would have to filter back
            // out (see `Resolution::Foreign`).
            Resolution::Path(_)
            | Resolution::Id { .. }
            | Resolution::External
            | Resolution::Foreign { .. } => None,
        }
    }
}

/// The walk's [`StructuralFact`]s, named. Each variant here is exactly the
/// fact the walk observed — a document that would not load, a single-parent
/// invariant broken, and so on — so this is a relabeling, not a judgment call:
/// `graph` stays ignorant of `Finding` (see the module doc at
/// [`prov_graph::graph`]) and `validate` supplies the one vocabulary a report is
/// written in.
impl From<StructuralFact> for Finding {
    fn from(fact: StructuralFact) -> Self {
        match fact {
            StructuralFact::Unreadable { doc, error } => Finding::Unreadable { doc, error },
            StructuralFact::IdMismatch {
                doc,
                frontmatter,
                registry,
            } => Finding::IdMismatch {
                doc,
                frontmatter,
                registry,
            },
            StructuralFact::UnregisteredId { doc, frontmatter } => {
                Finding::UnregisteredId { doc, frontmatter }
            }
            StructuralFact::UnstampedId { doc, registry } => Finding::UnstampedId { doc, registry },
            StructuralFact::DuplicateContainment { doc, target } => {
                Finding::DuplicateContainment { doc, target }
            }
            StructuralFact::MissingInverse {
                doc,
                child,
                inverse,
            } => Finding::MissingInverse {
                doc,
                child,
                inverse,
            },
            StructuralFact::CaseMismatch {
                doc,
                site,
                target,
                actual,
            } => Finding::CaseMismatch {
                doc,
                site,
                target,
                actual,
            },
            StructuralFact::BrokenLink { doc, site, target } => {
                Finding::BrokenLink { doc, site, target }
            }
        }
    }
}

/// Translate history's bounded-context diagnostics into the global validation
/// vocabulary. The dependency points one way: history reports its own issues;
/// validation decides how those issues are presented alongside graph findings.
impl From<HistoryIssue> for Finding {
    fn from(issue: HistoryIssue) -> Self {
        match issue {
            HistoryIssue::IndexStale {
                index,
                missing,
                extra,
            } => Finding::HistoryIndexStale {
                index,
                missing,
                extra,
            },
            HistoryIssue::BlobMissing { store, hash, paths } => {
                Finding::HistoryBlobMissing { store, hash, paths }
            }
            HistoryIssue::BlobOrphaned { store, blobs } => {
                Finding::HistoryBlobOrphaned { store, blobs }
            }
            HistoryIssue::StoreUnlinked { root, store } => {
                Finding::HistoryStoreUnlinked { root, store }
            }
            HistoryIssue::Unreadable { doc, error } => Finding::Unreadable { doc, error },
        }
    }
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
    /// custom name. Auto-fixable by relabeling
    /// ([`Fix::RelabelLink`](crate::remedy::Fix::RelabelLink)); the in-app
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
    /// every unstamped document, and [`Fix::SetId`](crate::remedy::Fix::SetId)
    /// writes the registry's id into
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
    /// mechanical autofix
    /// ([`Fix::RebuildHistoryIndex`](crate::remedy::Fix::RebuildHistoryIndex))
    /// rather than data loss.
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
    /// by construction (see [`Fix`](crate::remedy::Fix)) — `history-prune` is
    /// where bytes are deleted,
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
    /// **Autofixed by regeneration**
    /// ([`Fix::RegenerateAbout`](crate::remedy::Fix::RegenerateAbout)), and — unlike
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
                crate::config::ConfigIssueKind::MalformedWorkspaceId { value } => write!(
                    f,
                    "{}: config `workspace_id` is `{value}` — a workspace name cannot be empty or contain `/`, `:` or whitespace (ignored; the workspace stays anonymous)",
                    doc.display(),
                ),
                crate::config::ConfigIssueKind::NestNotSingleValued { field } => write!(
                    f,
                    "{}: config `{}` nests by `{field}`, which is declared `type: seq` — a document with several values has several homes, and containment allows one (the view still groups; drop `nest`)",
                    doc.display(),
                    issue.key,
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

impl<FS: Storage, IdP, Ix: IndexStore> Workspace<FS, IdP, Ix> {
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
            facts,
            content_bodies,
        } = self.walk(start).await?;
        let mut findings: Vec<Finding> = facts.into_iter().map(Finding::from).collect();
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
            findings.extend(finding_for(entry));
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
        findings.extend(
            self.history_findings(start)
                .await?
                .into_iter()
                .map(Finding::from),
        );
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
        let text = match self.read_text(path).await {
            Ok(text) => text,
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let doc = prov_graph::document::Document::parse(path, &text)?;
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
                && prov_graph::document::require_whole_file(&path, carrier).is_err()
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
                    if !self.exists(&parked).await? {
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
                            if let Some(suggestion) = prov_config::nearest_vocabulary_term(
                                &term,
                                &vocab.live_term_names(),
                            ) {
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
                    match self.read_bytes(&target).await {
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
}

// These tests use YAML frontmatter fixtures, so they run under the `yaml` feature.
#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::*;
    use crate::identity::Minter;
    use prov_graph::exec::block_on;
    use prov_graph::fs::StdFs;
    use prov_graph::link::LinkStyle;
    use prov_store::index::FileIndex;
    // The three id round-trips below assert that the repair *clears the
    // finding*, so they name a `Fix` from the module downstream of this one.
    use crate::remedy::Fix;

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
        let fs = crate::fs_faults::CountingFs::default();
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
        let fs = crate::fs_faults::CountingFs::default();
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
        use prov_graph::link::{Addressing, ReferenceStyle, Wrapper};
        use prov_graph::relation::{Relation, RelationSet};

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
    fn a_cross_workspace_reference_is_carried_not_reported() {
        // The claim the whole design rests on: `check` has no evidence about a
        // workspace it cannot see, so it says nothing. A finding here would be a
        // false positive on every reference, which every host would then have to
        // filter back out.
        let dir = tempdir("foreign-check");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\nlinks:\n- id:notes/ajp7eq\n---\nAlso [[id:diaryx/xk4m2p]].\n",
        );
        let ws = Workspace::builder(StdFs).root(&dir).build();

        let census = block_on(ws.census("index.md")).unwrap();
        assert!(
            census.iter().any(|e| matches!(
                &e.resolution,
                Resolution::Foreign { workspace, id }
                    if workspace == "notes" && id.as_str() == "ajp7eq"
            )),
            "frontmatter reference resolved foreign: {census:?}"
        );
        assert!(
            census.iter().any(|e| matches!(e.site, LinkSite::Body(_))
                && matches!(&e.resolution, Resolution::Foreign { workspace, .. } if workspace == "diaryx")),
            "body reference resolved foreign: {census:?}"
        );

        let findings = block_on(ws.check("index.md")).unwrap();
        assert!(
            !findings.iter().any(|f| matches!(
                f,
                Finding::BrokenLink { .. }
                    | Finding::DanglingId { .. }
                    | Finding::MalformedId { .. }
            )),
            "no link finding for a workspace check cannot see: {findings:?}"
        );
    }

    #[test]
    fn a_self_qualified_reference_is_checked_like_a_local_one() {
        // The other side of the invariant: once the workspace claims the name,
        // a reference qualified with it stops being foreign — including when it
        // dangles, which is now a real finding rather than a silent pass.
        let dir = tempdir("self-qualified-check");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\nworkspace_id: notes\nlinks:\n- id:notes/0vn4182\n---\n",
        );
        let ws = Workspace::builder(StdFs)
            .root(&dir)
            .workspace_id("notes")
            .build();

        let findings = block_on(ws.check("index.md")).unwrap();
        assert!(
            findings.iter().any(|f| matches!(
                f,
                Finding::DanglingId { id, .. } if id.as_str() == "0vn4182"
            )),
            "the id is ours, and it resolves to nothing: {findings:?}"
        );
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
    fn id_mismatch_flags_a_frontmatter_id_disagreeing_with_the_registry() {
        use prov_graph::identity::Id;

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
        use prov_graph::identity::Id;
        use prov_graph::index::IdIndex;

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
        use prov_graph::identity::Id;
        use prov_graph::index::IdIndex;

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
