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
//! - **missing containment** — the mirror of it: a document nothing reaches
//!   whose `part_of` names a parent that does not list it, which is how a whole
//!   unlinked subtree becomes visible to a walk that by construction cannot
//!   reach it;
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

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::workspace::Workspace;
use prov_graph::content::ContentFormat;
use prov_graph::error::{Error, Result};
use prov_graph::graph::{CensusEntry, LinkSite, Resolution, StructuralFact, Walk, reachable_set};
use prov_graph::identity::Id;
use prov_graph::link;
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
            //
            // `SameDocument` is there for the same reason, one level down: `#3`
            // addresses a place inside this document, and prov does not read a
            // document's internal address space, so it has no evidence about
            // whether that place exists either.
            Resolution::Path(_)
            | Resolution::Id { .. }
            | Resolution::External
            | Resolution::SameDocument
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
            StructuralFact::ManifestConflict { doc } => Finding::ManifestConflict { doc },
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
    /// A document nothing reaches that *names its own parent* — its inverse
    /// (`part_of`) points at a document the workspace does reach, and that
    /// document does not list it back. The mirror of
    /// [`MissingInverse`](Finding::MissingInverse), which is the same broken
    /// pair seen from the parent's side.
    ///
    /// This is the finding that lets `check` see a **disconnected island**.
    /// [`Orphan`](Finding::Orphan) is reachability-bounded (DESIGN §8) and so
    /// cannot be: an unlinked directory is never scanned, so the whole subtree
    /// under it — however well-linked internally, however large — produces no
    /// finding at all, and `check` reports clean precisely *because* it cannot
    /// see it. A document's own `part_of` is the one piece of evidence that
    /// survives that bound, because it is written down in the island rather than
    /// in the tree: a file that says which parent it belongs to is workspace
    /// content that lost a forward link, not a vendored copy or a nested
    /// workspace that never claimed membership.
    ///
    /// Only the island's **entry point** is reported. Its interior — every
    /// document whose parent does list it — is left alone, because writing the
    /// one missing entry makes the entire subtree reachable and every ordinary
    /// pass then sees it. So a 400-note folder is one finding and one repair,
    /// not four hundred of each.
    MissingContainment { doc: PathBuf, parent: PathBuf },
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
    /// Documents carrying a `content_hash` of their **own body** — the coverage
    /// the retired `fixity: all` tier wrote, which nothing writes now. `count` is
    /// how many were reached and `example` names one, because reporting each
    /// would mean one finding per document in a workspace that ran that tier.
    ///
    /// The checksums themselves are still verified: `check` honors any hash on
    /// record regardless of the setting, so a flipped bit in one of these bodies
    /// is still a [`FixityMismatch`](Finding::FixityMismatch). What has gone is
    /// their *upkeep* — no write verb restamps one any more, so the next ordinary
    /// edit will drift the hash and leave a mismatch that only `check --fix` can
    /// settle, once per edit, forever.
    ///
    /// **Diagnosis only.** Two answers are defensible and they are not prov's to
    /// pick between: unset the field, which is the coverage this build stands
    /// behind, or keep it as a private record and re-stamp on demand. Either
    /// beats the treadmill, and only the author knows which the archive wants.
    LegacyBodyHash {
        root: PathBuf,
        count: usize,
        example: PathBuf,
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
    /// The root reaches its deletion log through `recycle_bin`, the pointer
    /// relation `deletions` replaced. `root` is the document declaring it,
    /// `relation` the old spelling, and `log` what it points at.
    ///
    /// Nothing is broken: the old spelling still resolves, and every verb reads
    /// the log through it. What the workspace has is a key whose name outlived
    /// its meaning — there is no bin, because prov no longer keeps the bytes of
    /// a deleted document — and a `recyclebin/items/` that is still parked out
    /// of every walk on the strength of that name.
    ///
    /// **Diagnosis only,** and deliberately: renaming the key is one edit, but
    /// the parked bytes under the old store are the last copy of whatever was
    /// binned before the rename, and prov will not decide their fate. Restore
    /// what is worth keeping ([`restore`](crate::Workspace::restore) still moves
    /// parked bytes home), or forget the rest
    /// ([`clear_deletions`](crate::Workspace::clear_deletions)), and then rename
    /// the pointer.
    LegacyDeletionsPointer {
        root: PathBuf,
        relation: String,
        log: PathBuf,
    },
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
    /// A node declares both `content` and `manifest` — a sidecar for one payload
    /// and for a whole directory at once. The two are mutually exclusive: a node
    /// stands for one set of bytes or for a set of files, and every pass that
    /// asks "what does this node cover" would get two answers.
    ///
    /// Diagnosis only. Which key is the mistake is the author's to say — dropping
    /// either one is a claim about what this node was meant to be, and prov has
    /// no evidence for it.
    ManifestConflict { doc: PathBuf },
    /// A manifest document could not be read as one: its `root` is missing or
    /// climbs out of the workspace, a row carries no `path`, or `files` is not a
    /// sequence. `doc` is the manifest, `error` what parsing said.
    ///
    /// Distinct from [`Unreadable`](Finding::Unreadable) on purpose: the file
    /// parsed fine *as a document* and failed as a *record store*, which is a
    /// different repair (fix the rows) and a different risk — a manifest that
    /// will not parse is a fixity baseline nothing is checking.
    ManifestMalformed { doc: PathBuf, error: String },
    /// The covered directory and the manifest disagree about what is in it:
    /// `missing` names rows whose file is not on disk, `extra` names opaque files
    /// under the root that no row claims. Both are relative to the manifest's
    /// `root`, as the rows are.
    ///
    /// This is the finding a bulk attachment exists for. One node stands for ten
    /// thousand files, so "did one of them vanish, did one appear" is a question
    /// nothing else in prov can answer: the files are not documents, so the
    /// orphan pass ignores them, and the census never sees them.
    ///
    /// **Cheap by construction.** One directory walk, no file reads — which is
    /// what lets it run inside every `check` over an archive. Corruption *inside*
    /// a present, listed file is the other half, and costs a full read of the
    /// archive: [`verify_manifest`](crate::Workspace::verify_manifest).
    ///
    /// Repaired by regenerating the manifest
    /// ([`Fix::RegenerateManifest`](crate::remedy::Fix::RegenerateManifest)) —
    /// confirmation-gated, because accepting the directory as it is now is a
    /// judgment, exactly as re-stamping a checksum is.
    ManifestDrift {
        node: PathBuf,
        manifest: PathBuf,
        missing: Vec<PathBuf>,
        extra: Vec<PathBuf>,
    },
    /// A manifest row's recorded digest does not match the bytes of the file it
    /// names — bit-rot inside a covered file. `path` is workspace-relative (the
    /// file a person has to go and look at), `manifest` the record that pinned it.
    ///
    /// Raised **per file**, unlike [`ManifestDrift`](Finding::ManifestDrift):
    /// one corrupted photograph is one thing to restore, and a report that
    /// collapsed fifty of them into a count would hide which fifty.
    ///
    /// Only [`verify_manifest`](crate::Workspace::verify_manifest) raises this;
    /// `check` does not read covered files. The same judgment
    /// [`FixityMismatch`](Finding::FixityMismatch) makes applies — intended
    /// change or corruption is not prov's to decide.
    ManifestMismatch {
        node: PathBuf,
        manifest: PathBuf,
        path: PathBuf,
        recorded: String,
        actual: String,
    },
}

impl Finding {
    /// The document this finding is lodged against — **the file to open to act
    /// on it**.
    ///
    /// Every finding names several paths (a link has a source and a target, a
    /// manifest has a node and a directory), so "which file is this about" is a
    /// choice, not a field. The rule here is the one a person uses: the document
    /// a repair rewrites, or, where no repair is offered, the file they have to
    /// go and look at. That makes it a total function — every finding has
    /// exactly one — which is what lets a caller group findings by file, or
    /// filter them to one.
    ///
    /// Two of them are worth naming, because the obvious field is not the
    /// answer:
    ///
    /// - [`MissingInverse`](Self::MissingInverse) reports a *parent* whose child
    ///   does not link back, and its `doc` is that parent — but
    ///   [`AddInverse`](crate::remedy::Fix::AddInverse) writes the **child**, so
    ///   the child is the subject.
    /// - [`ManifestMismatch`](Self::ManifestMismatch) is a corrupted file inside
    ///   a covered directory: the **file**, not the node that pinned it.
    ///
    /// **This is not "every finding that mentions the file".** A broken link in
    /// `a.md` pointing at `b.md` is `a.md`'s finding, because `a.md` is what a
    /// repair rewrites — asking for `b.md`'s findings will not surface it. Which
    /// is the honest answer: nothing is wrong with `b.md`.
    ///
    /// A repair may still write a *derived* companion alongside the subject —
    /// the registry for [`RegisterId`](crate::remedy::Fix::RegisterId), the
    /// manifest for [`RegenerateManifest`](crate::remedy::Fix::RegenerateManifest)
    /// — since those are caches of the subject rather than documents in their
    /// own right.
    pub fn subject(&self) -> &Path {
        match self {
            Finding::BrokenLink { doc, .. }
            | Finding::CaseMismatch { doc, .. }
            | Finding::DuplicateContainment { doc, .. }
            | Finding::Unreadable { doc, .. }
            | Finding::MalformedId { doc, .. }
            | Finding::DanglingId { doc, .. }
            | Finding::AmbiguousAlias { doc, .. }
            | Finding::StaleLabel { doc, .. }
            | Finding::IdMismatch { doc, .. }
            | Finding::UnregisteredId { doc, .. }
            | Finding::UnstampedId { doc, .. }
            | Finding::Orphan { doc, .. }
            | Finding::MissingContainment { doc, .. }
            | Finding::FixityMismatch { doc, .. }
            | Finding::ConfigIssue { doc, .. }
            | Finding::ConfigSpecAhead { doc, .. }
            | Finding::MalformedStore { doc, .. }
            | Finding::UnknownTerm { doc, .. }
            | Finding::TermNearMiss { doc, .. }
            | Finding::ManifestConflict { doc }
            | Finding::ManifestMalformed { doc, .. } => doc,
            // The child is what gains the back-link; `doc` is the parent that
            // reported it missing.
            Finding::MissingInverse { child, .. } => child,
            // The root is what declares the outdated spelling, and what the
            // rename edits; the log it names is fine as it is.
            Finding::LegacyDeletionsPointer { root, .. } => root,
            // The workspace, not the example: the finding is about a population.
            Finding::LegacyBodyHash { root, .. } => root,
            Finding::AboutStale { path, .. } => path,
            Finding::ManifestDrift { node, .. } => node,
            // The one corrupted file, not the node covering ten thousand.
            Finding::ManifestMismatch { path, .. } => path,
        }
    }

    /// A stable snake_case name for this finding's kind — the discriminant on
    /// its own, for a consumer that branches on the kind rather than reading the
    /// prose. Matches the variant name, so the two never have to be reconciled
    /// by hand.
    pub fn kind(&self) -> &'static str {
        match self {
            Finding::BrokenLink { .. } => "broken_link",
            Finding::CaseMismatch { .. } => "case_mismatch",
            Finding::DuplicateContainment { .. } => "duplicate_containment",
            Finding::MissingInverse { .. } => "missing_inverse",
            Finding::Unreadable { .. } => "unreadable",
            Finding::MalformedId { .. } => "malformed_id",
            Finding::DanglingId { .. } => "dangling_id",
            Finding::AmbiguousAlias { .. } => "ambiguous_alias",
            Finding::StaleLabel { .. } => "stale_label",
            Finding::IdMismatch { .. } => "id_mismatch",
            Finding::UnregisteredId { .. } => "unregistered_id",
            Finding::UnstampedId { .. } => "unstamped_id",
            Finding::Orphan { .. } => "orphan",
            Finding::MissingContainment { .. } => "missing_containment",
            Finding::FixityMismatch { .. } => "fixity_mismatch",
            Finding::ConfigIssue { .. } => "config_issue",
            Finding::ConfigSpecAhead { .. } => "config_spec_ahead",
            Finding::MalformedStore { .. } => "malformed_store",
            Finding::UnknownTerm { .. } => "unknown_term",
            Finding::TermNearMiss { .. } => "term_near_miss",
            Finding::LegacyDeletionsPointer { .. } => "legacy_deletions_pointer",
            Finding::LegacyBodyHash { .. } => "legacy_body_hash",
            Finding::AboutStale { .. } => "about_stale",
            Finding::ManifestConflict { .. } => "manifest_conflict",
            Finding::ManifestMalformed { .. } => "manifest_malformed",
            Finding::ManifestDrift { .. } => "manifest_drift",
            Finding::ManifestMismatch { .. } => "manifest_mismatch",
        }
    }
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
            Finding::MissingContainment { doc, parent } => write!(
                f,
                "{}: claims {} as its parent, but {} does not list it — nothing reaches this \
                 document or anything under it",
                doc.display(),
                parent.display(),
                parent.display()
            ),
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
            Finding::LegacyBodyHash {
                root,
                count,
                example,
            } => write!(
                f,
                "{}: {count} document(s) record a `content_hash` of their own body \
                 (e.g. {}) — coverage prov no longer writes, so nothing restamps \
                 one after an edit; unset the field, or keep it and re-stamp with \
                 `check --fix`",
                root.display(),
                example.display(),
            ),
            Finding::LegacyDeletionsPointer {
                root,
                relation,
                log,
            } => write!(
                f,
                "{}: reaches its deletion log ({}) through `{relation}`, the pointer \
                 `deletions` replaced — restore or forget anything still parked \
                 under it, then rename the key",
                root.display(),
                log.display(),
            ),
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
            Finding::ManifestConflict { doc } => write!(
                f,
                "{}: declares both content and manifest — a node covers one payload or a directory, not both",
                doc.display()
            ),
            Finding::ManifestMalformed { doc, error } => {
                write!(f, "{}: not a readable manifest: {error}", doc.display())
            }
            Finding::ManifestDrift {
                manifest,
                missing,
                extra,
                ..
            } => {
                let mut parts = Vec::new();
                if !missing.is_empty() {
                    parts.push(format!("{} listed but gone", missing.len()));
                }
                if !extra.is_empty() {
                    parts.push(format!("{} unlisted", extra.len()));
                }
                write!(
                    f,
                    "{}: the directory it covers has drifted ({}) — `prov manifest` names them",
                    manifest.display(),
                    parts.join(", ")
                )
            }
            Finding::ManifestMismatch { manifest, path, .. } => write!(
                f,
                "{}: bytes no longer match the checksum recorded in {}",
                path.display(),
                manifest.display()
            ),
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
    /// Ten passes follow, most of them over the same documents: the walk loads
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
        // Islands first: what it finds is what the (reachability-bounded) orphan
        // sweep must not report a second time under a vaguer name.
        let (islands, island_members) = self
            .missing_containment(start, &census, &content_bodies)
            .await?;
        findings.extend(islands);
        findings.extend(
            self.orphans(start, &census, &content_bodies, &island_members)
                .await?,
        );
        findings.extend(
            self.fixity_findings(start, &census, &content_bodies)
                .await?,
        );
        findings.extend(
            self.manifest_findings(start, &census, &content_bodies)
                .await?,
        );
        findings.extend(self.config_findings(start).await?);
        findings.extend(self.store_findings(start).await?);
        findings.extend(self.deletions_findings(start).await?);
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
        let text = match self.read_text(path).await {
            Ok(text) => text,
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let doc = prov_graph::document::Document::parse(path, &text)?;
        Ok(fig::Value::from(&doc.meta)
            .get("title")
            .and_then(fig::Value::as_str)
            .map(str::to_owned))
    }

    /// Verify every **record store** the workspace reaches — the id registry, the
    /// deletion log, and each `fields` vocabulary — is a whole-file config
    /// document, emitting a [`Finding::MalformedStore`] for any found in a markdown
    /// carrier (DESIGN §5, the whole-file rule). This *reports* rather than aborts:
    /// the loaders themselves hard-error on a markdown store, but `check` surfaces
    /// the same problem as a finding so a diagnosis run lists it alongside the rest.
    async fn store_findings(&self, start: &Path) -> Result<Vec<Finding>> {
        let mut stores: Vec<(&'static str, PathBuf)> = Vec::new();
        if let Some(p) = self.registry_path(start).await? {
            stores.push(("registry", p));
        }
        if let Some((p, relation)) = self.deletions_pointer(start).await? {
            // Named by the relation that found it, so a workspace still on the
            // old spelling reads its own key back in the finding.
            let label: &'static str =
                if Some(relation.as_str()) == self.relations().deletions_relation() {
                    "deletions"
                } else {
                    "recycle_bin"
                };
            stores.push((label, p));
        }
        let config = self.effective_config(start).await?;
        for spec in config.fields.values() {
            // A type-only field declares no vocabulary, so it has no store. A
            // *reified* one declares content rather than machinery — an index node
            // whose children are term documents — so the whole-file rule does not
            // reach it, and demanding a config carrier of it would report the
            // markdown it is supposed to be. What it gets in exchange is the thing
            // a machinery store deliberately gives up: its terms are in the
            // reachable set, inverse-checked and orphan-checked like any content.
            if spec.reify {
                continue;
            }
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

    /// Report a root that reaches its deletion log through the legacy
    /// `recycle_bin` pointer — one [`Finding::LegacyDeletionsPointer`], or none.
    ///
    /// The old spelling resolves everywhere, so this is not a repair prov is
    /// withholding for want of information: it is one the workspace has to make
    /// in the right order, because whatever the bin parked under `items/` is
    /// still parked on the strength of that pointer and stops being so the
    /// moment it is renamed.
    ///
    /// Nothing here reads the log's *records*. There is nothing left to check in
    /// them: a record names no bytes prov is keeping, so the only way for one to
    /// be wrong is for a caller to have hand-edited it, and `store_findings`
    /// already establishes the document itself.
    async fn deletions_findings(&self, start: &Path) -> Result<Vec<Finding>> {
        let Some((log, relation)) = self.deletions_pointer(start).await? else {
            return Ok(Vec::new());
        };
        if Some(relation.as_str()) == self.relations().deletions_relation() {
            return Ok(Vec::new());
        }
        Ok(vec![Finding::LegacyDeletionsPointer {
            root: start.to_path_buf(),
            relation,
            log,
        }])
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
            // Membership is only checkable for a field that names a vocabulary; a
            // type-only field has nothing to be a member of. `load_field_vocabulary`
            // is where flat and reified are told apart — both yield the same term
            // set, so everything below reads one shape.
            if let Ok(Some(vocab)) = self.load_field_vocabulary(start, field, spec).await {
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
        // Documents whose recorded hash covers their own body — see
        // [`Finding::LegacyBodyHash`]. Counted here rather than in a pass of its
        // own because this loop already has the two facts it takes, so the
        // report costs no read that `check` was not making anyway.
        let mut body_hashed: (usize, Option<PathBuf>) = (0, None);
        for path in reachable {
            // A reached payload file (a `.png`) will not parse as a document —
            // skip it; it is verified through its sidecar, not on its own.
            let Ok((_, doc)) = self.load(&path).await else {
                continue;
            };
            let meta = fig::Value::from(&doc.meta);
            let Some(recorded) = meta.get("content_hash").and_then(fig::Value::as_str) else {
                continue;
            };
            if !crate::fixity::is_recognized(recorded) {
                continue;
            }
            // What the hash covers: the `content` sibling if this document points
            // at one, the `manifest` document if it stands for a directory, else
            // the document's own body. A manifest node pinning its manifest is
            // the same relationship an attachment sidecar has with its payload —
            // and it is what makes the per-row digests inside worth anything,
            // since a rewritten row is then a rewritten file the node has hashed.
            let actual = match doc.content_attr().or_else(|| doc.manifest_attr()) {
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
                // Neither pointer: the hash covers this document's own parsed
                // body. Still verified — a hash on record is always checked —
                // but it is a hash nothing maintains, so note the document and
                // report the population once the walk is done.
                None => {
                    body_hashed.0 += 1;
                    body_hashed.1.get_or_insert_with(|| path.clone());
                    crate::fixity::digest(doc.body.as_bytes())
                }
            };
            if actual != recorded {
                findings.push(Finding::FixityMismatch {
                    doc: path,
                    recorded: recorded.to_string(),
                    actual,
                });
            }
        }
        if let (count, Some(example)) = body_hashed {
            findings.push(Finding::LegacyBodyHash {
                root: start.to_path_buf(),
                count,
                example,
            });
        }
        Ok(findings)
    }

    /// Compare every reachable manifest against the directory it covers —
    /// [`Finding::ManifestDrift`] for a disagreement, and
    /// [`Finding::ManifestMalformed`] for a manifest that will not parse as one.
    ///
    /// The bulk-attachment counterpart of the orphan pass, and it exists for the
    /// same reason: a file nothing accounts for should be visible. The covered
    /// files are *not* documents, so the orphan walk cannot see them, and they
    /// are not in the census, so nothing else can either — which is why a
    /// manifest claims its root **completely**. Anything opaque under it that no
    /// row names is drift, and any row whose file is gone is drift the other way.
    ///
    /// **One directory walk, no file reads.** That bound is the reason this can
    /// run in every `check` over an archive of ten thousand photographs. What it
    /// cannot see — a file still present, still listed, with different bytes —
    /// costs a full read of the archive and is
    /// [`verify_manifest`](Workspace::verify_manifest), run on purpose.
    ///
    /// A malformed manifest yields *only* that finding for its directory: with
    /// no trustworthy row set there is nothing to compare a listing against, and
    /// reporting every file in the directory as unlisted would bury the one
    /// finding that matters under ten thousand that do not.
    async fn manifest_findings(
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
            let Ok((_, doc)) = self.load(&path).await else {
                continue;
            };
            let Some(raw) = doc.manifest_attr() else {
                continue;
            };
            let manifest_doc = link::resolve(&path, raw);
            // An absent manifest is the census's broken-link finding, already
            // raised against the node; saying it twice in different words helps
            // nobody.
            if !self.exists(&manifest_doc).await? {
                continue;
            }
            let manifest = match self.graph().read_manifest(&manifest_doc).await {
                Ok(manifest) => manifest,
                Err(error) => {
                    findings.push(Finding::ManifestMalformed {
                        doc: manifest_doc,
                        error: error.to_string(),
                    });
                    continue;
                }
            };
            let root = manifest.covered_root(&manifest_doc);
            if !self
                .graph()
                .stat(&root)
                .await
                .map(|m| m.is_dir())
                .unwrap_or(false)
            {
                findings.push(Finding::BrokenLink {
                    doc: manifest_doc,
                    site: LinkSite::Relation(prov_graph::manifest::ROOT_KEY.to_string()),
                    target: manifest.root.clone(),
                });
                continue;
            }

            let on_disk = self.graph().scan_covered(&root).await?;
            let (missing, extra) = prov_graph::manifest::diff(&manifest.files, &on_disk);
            if !missing.is_empty() || !extra.is_empty() {
                findings.push(Finding::ManifestDrift {
                    node: path,
                    manifest: manifest_doc,
                    missing,
                    extra,
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
    /// invisible here rather than flagged — with one exception, which is
    /// [`missing_containment`](Self::missing_containment): a document that names
    /// its own parent has said it belongs here, and that claim is evidence a
    /// reachability bound cannot reach past. `island` is what that pass found,
    /// and it is excluded here so one broken forward link is one finding: an
    /// island member is unreachable *through its island*, and repairing the
    /// island's entry point brings the whole of it back into the tree.
    ///
    /// Orphanhood is relative to `start`: run from the workspace root (the usual
    /// case) it means "on disk in a known directory but unlinked."
    async fn orphans(
        &self,
        start: &Path,
        census: &[CensusEntry],
        content_bodies: &[PathBuf],
        island: &BTreeSet<PathBuf>,
    ) -> Result<Vec<Finding>> {
        let reachable = reachable_set(start, census, content_bodies);
        // Scan only the directories the reachable set occupies (their direct
        // children), never descending into unreached subdirectories.
        let reached_dirs = Self::reached_dirs(&reachable);
        let mut docs: Vec<PathBuf> = self
            .direct_child_files(&reached_dirs)
            .await?
            .into_iter()
            .filter(|p| {
                ContentFormat::from_extension(p).is_some()
                    && !reachable.contains(p)
                    && !island.contains(p)
            })
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

    /// The **disconnected islands**: every document outside the reachable graph
    /// whose `part_of` chain lands inside it, and the
    /// [`Finding::MissingContainment`] for each one whose named parent does not
    /// list it back. Also returns the island's full membership, which
    /// [`orphans`](Self::orphans) excludes so one broken link is one finding.
    ///
    /// This is the one pass in `check` whose scan is **not** reachability-bounded,
    /// and it has to be: "what does the workspace fail to reach?" is precisely the
    /// question a reachability bound cannot answer, so the bounded orphan sweep
    /// finds only the orphans *adjacent* to the tree and a whole unlinked subtree
    /// stays silent (see [`Finding::MissingContainment`]). The scan reads every
    /// content document on disk that nothing reaches; what keeps that from
    /// becoming a report about someone else's files is the claim itself. A
    /// vendored tree, a nested prov workspace, a `scratch/` folder — none of them
    /// name a parent in *this* workspace, so none of them appear here, and DESIGN
    /// §8's trade is preserved where it was actually protecting something.
    ///
    /// prov's own parked bytes are skipped outright: a recycled document keeps
    /// the `part_of` it had when it was binned, and reading that as a claim would
    /// report every deleted file as an island the moment it was deleted.
    ///
    /// Membership is a closure, not a single step: an island's interior claims
    /// the island, not the tree, so the set grows until it stops growing. That is
    /// what makes a stack of unlinked years — `Daily/2025`, then `Daily/2025/10`
    /// beneath it — resolve in one run instead of one run per layer.
    async fn missing_containment(
        &self,
        start: &Path,
        census: &[CensusEntry],
        content_bodies: &[PathBuf],
    ) -> Result<(Vec<Finding>, BTreeSet<PathBuf>)> {
        let empty = BTreeSet::new();
        // No spanning relation, no containment to be missing from.
        let Ok((spanning, inverse)) = self.spanning_pair() else {
            return Ok((Vec::new(), empty));
        };
        let reachable = reachable_set(start, census, content_bodies);
        let parked = self.parked_dirs(start).await?;

        // Every unreached content document that names a parent, in path order so
        // the closure below and the findings it produces are deterministic.
        let mut claims: Vec<(PathBuf, PathBuf)> = Vec::new();
        for doc in self.content_documents().await? {
            if reachable.contains(&doc) || parked.iter().any(|dir| doc.starts_with(dir)) {
                continue;
            }
            let Ok((_, parsed)) = self.load(&doc).await else {
                continue;
            };
            if let Some(parent) = self.single_target(&parsed, &inverse, &doc) {
                claims.push((doc, parent));
            }
        }

        // A claim on the tree makes an island member; a claim on a member does
        // too. Repeat until nothing new joins.
        let mut island: BTreeSet<PathBuf> = BTreeSet::new();
        loop {
            let before = island.len();
            for (doc, parent) in &claims {
                if !island.contains(doc) && (reachable.contains(parent) || island.contains(parent))
                {
                    island.insert(doc.clone());
                }
            }
            if island.len() == before {
                break;
            }
        }

        // The entry points: a member whose parent does not list it. Its
        // neighbours further in are already accounted for by their own parents,
        // and become reachable the moment this entry is written.
        let mut findings = Vec::new();
        for (doc, parent) in &claims {
            if !island.contains(doc) {
                continue;
            }
            let Ok((_, parent_doc)) = self.load(parent).await else {
                continue;
            };
            if self
                .entry_index(&parent_doc, &spanning, parent, doc)
                .is_some()
            {
                continue;
            }
            // An archive claimed in bulk is accounted for by its manifest, and
            // its rows are pinned in a document the tree does reach — the same
            // exclusion `attach --all` and the capture-set report make.
            if self.graph().under_manifest(doc).await? {
                continue;
            }
            findings.push(Finding::MissingContainment {
                doc: doc.clone(),
                parent: parent.clone(),
            });
        }
        Ok((findings, island))
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
    use crate::remedy::{Fix, Warrant};

    pub(super) use prov_testkit::write;
    pub(super) fn tempdir(tag: &str) -> PathBuf {
        prov_testkit::scratch("check", tag)
    }

    #[test]
    fn a_body_hash_a_workspace_kept_from_the_retired_tier_is_reported_once() {
        // A workspace that ran `fixity: all` arrives here with a `content_hash`
        // in every combined document. They are still *verified* — a hash on
        // record is always checked — but nothing restamps one after an edit, so
        // the population is reported so the author can settle it.
        let dir = tempdir("legacy-body-hash");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n- b.md\n---\n");
        for (name, body) in [("a.md", "alpha\n"), ("b.md", "beta\n")] {
            write(
                &dir,
                name,
                format!(
                    "---\npart_of: index.md\ncontent_hash: {}\n---\n{body}",
                    crate::fixity::digest(body.as_bytes())
                ),
            );
        }
        let ws = Workspace::builder(StdFs).root(&dir).build();

        // One finding for two documents: reporting each would mean one per
        // document in exactly the workspaces that have the problem.
        let findings = block_on(ws.check("index.md")).unwrap();
        assert!(
            matches!(
                findings.as_slice(),
                [Finding::LegacyBodyHash { count: 2, .. }]
            ),
            "{findings:?}"
        );

        // Diagnosis only: unsetting the field and keeping it are both
        // defensible, and prov does not pick.
        assert_eq!(block_on(ws.remedies(&findings[0])).unwrap().len(), 0);

        // The checksums still do their job in the meantime — corrupt one body
        // and the mismatch is raised alongside.
        std::fs::write(dir.join("a.md"), "---\npart_of: index.md\ncontent_hash: sha256:0000000000000000000000000000000000000000000000000000000000000000\n---\nalpha\n").unwrap();
        let findings = block_on(ws.check("index.md")).unwrap();
        assert!(
            findings.iter().any(
                |f| matches!(f, Finding::FixityMismatch { doc, .. } if doc == Path::new("a.md"))
            ),
            "a recorded hash is verified whatever the setting says: {findings:?}"
        );

        // Unsetting the field is what clears it, and nothing else has to move.
        for name in ["a.md", "b.md"] {
            let text = std::fs::read_to_string(dir.join(name)).unwrap();
            let kept: String = text
                .lines()
                .filter(|l| !l.starts_with("content_hash:"))
                .map(|l| format!("{l}\n"))
                .collect();
            std::fs::write(dir.join(name), kept).unwrap();
        }
        assert_eq!(block_on(ws.check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn a_clean_workspace_has_no_findings() {
        let dir = tempdir("clean");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n---\n");
        write(&dir, "a.md", "---\npart_of: index.md\n---\n");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        assert_eq!(block_on(ws.check("index.md")).unwrap(), vec![]);
    }

    /// `check` is ten passes over one graph, and several of them want the same
    /// documents. The read scope it opens is what makes that composition cost
    /// one read per document instead of one per pass.
    #[test]
    fn check_reads_each_document_once() {
        let dir = tempdir("memo");
        write(&dir, "index.md", "---\ncontents:\n- a.yaml\n---\n");
        write(
            &dir,
            "a.yaml",
            format!(
                "part_of: index.md\ncontent: a.md\ncontent_hash: {}\n",
                crate::fixity::digest(b"alpha\n")
            ),
        );
        write(&dir, "a.md", "alpha\n");
        let fs = crate::fs_faults::CountingFs::default();
        let ws = Workspace::builder(fs.clone()).root(&dir).build();

        assert_eq!(block_on(ws.check("index.md")).unwrap(), vec![]);
        // `a.yaml` is wanted by the walk (for the census) and again by the
        // fixity pass (for the hash it records) at the very least.
        assert_eq!(
            fs.doc_reads(&dir, "a.yaml"),
            1,
            "a document was read more than once inside one `check`"
        );
        assert_eq!(fs.doc_reads(&dir, "index.md"), 1);
    }

    /// A bare walk — no `check`, no verb, nothing composed on top — still reads
    /// each document three times without a scope: once to descend into it, once
    /// as the inverse check reads every spanning child to see whether it points
    /// back, and once more when a `[[alias]]` link sends the title index over
    /// the reached directories. So the walk opens its own scope rather than
    /// waiting for a caller to think of it.
    #[test]
    fn a_walk_reads_each_document_once() {
        let dir = tempdir("memo-walk");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- a.md\n---\nsee [[A]]\n",
        );
        write(&dir, "a.md", "---\ntitle: A\npart_of: index.md\n---\n");
        let fs = crate::fs_faults::CountingFs::default();
        let ws = Workspace::builder(fs.clone()).root(&dir).build();

        block_on(ws.backlinks_to("index.md", "a.md")).unwrap();
        assert_eq!(
            fs.doc_reads(&dir, "a.md"),
            1,
            "a document was read more than once inside one walk"
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

    /// A workspace whose root still declares the pointer `deletions` replaced,
    /// with one document binned by it and its bytes parked under `items/` —
    /// exactly what an unmigrated workspace looks like on disk.
    fn with_a_legacy_bin(tag: &str) -> PathBuf {
        let dir = tempdir(tag);
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\nrecycle_bin: recyclebin/index.yaml\ncontents:\n- note.md\n---\n",
        );
        write(
            &dir,
            "note.md",
            "---\ntitle: My Note\npart_of: index.md\n---\nbody\n",
        );
        write(
            &dir,
            "recyclebin/index.yaml",
            "title: Recycle Bin\ndeleted:\n- from: gone.md\n  title: Gone\n  bin: recyclebin/items/gone.md\n  parent: index.md\n",
        );
        write(&dir, "recyclebin/items/gone.md", "---\ntitle: Gone\n---\n");
        dir
    }

    #[test]
    fn a_root_on_the_old_pointer_spelling_is_reported_once() {
        let dir = with_a_legacy_bin("legacy-pointer");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let findings: Vec<Finding> = block_on(ws.check("index.md"))
            .unwrap()
            .into_iter()
            .filter(|f| matches!(f, Finding::LegacyDeletionsPointer { .. }))
            .collect();

        assert_eq!(
            findings,
            vec![Finding::LegacyDeletionsPointer {
                root: PathBuf::from("index.md"),
                relation: "recycle_bin".to_string(),
                log: PathBuf::from("recyclebin/index.yaml"),
            }],
        );
        // The message has to say what to do *before* renaming, since renaming is
        // what un-parks the bytes the old store is still holding.
        let text = findings[0].to_string();
        assert!(text.contains("`recycle_bin`"), "{text}");
        assert!(text.contains("rename the key"), "{text}");
        // Diagnosis only: the parked bytes are prov's to report, not to judge.
        assert!(block_on(ws.suggest_fix(&findings[0])).unwrap().is_none());
    }

    #[test]
    fn a_root_on_the_current_pointer_spelling_is_not_reported() {
        // The load-bearing negative: every workspace that has been through a
        // recorded delete declares `deletions`, and none of them wants a finding.
        let dir = tempdir("current-pointer");
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
        block_on(w.delete(Path::new("note.md"), false)).unwrap();
        assert!(
            std::fs::read_to_string(dir.join("index.md"))
                .unwrap()
                .contains("deletions")
        );

        let findings = block_on(
            Workspace::builder(StdFs)
                .root(&dir)
                .build()
                .check("index.md"),
        )
        .unwrap();
        assert!(
            findings.is_empty(),
            "a recorded delete leaves check clean: {findings:?}"
        );
    }

    #[test]
    fn the_legacy_bin_still_parks_its_items_out_of_the_orphan_walk() {
        // The reason the finding is diagnosis-only. `items/` is unreached by
        // design, and it is the old pointer that keeps it parked — so as long as
        // the workspace declares one, a binned document must not surface as an
        // orphan on top of the rename notice.
        let dir = with_a_legacy_bin("legacy-parked");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let findings = block_on(ws.check("index.md")).unwrap();
        assert!(
            findings
                .iter()
                .all(|f| matches!(f, Finding::LegacyDeletionsPointer { .. })),
            "the parked bytes were walked: {findings:?}"
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

    /// A reified vocabulary in the shape `check` has to accept: a markdown index
    /// node under the spine, its `contents` the term documents, each with a
    /// `part_of` back — content all the way down, which is what makes the
    /// whole-file store rule inapplicable.
    fn a_workspace_with_reified_audiences(tag: &str, values: &str, note: &str) -> PathBuf {
        let dir = tempdir(tag);
        write(
            &dir,
            "index.md",
            format!(
                "---\n\
                 title: Root\n\
                 contents:\n- note.md\n- vocab/index.md\n\
                 prov:\n  fields:\n    audience:\n      values: {values}\n      \
                 vocabulary: vocab/index.md\n      reify: true\n\
                 ---\n"
            ),
        );
        write(&dir, "note.md", note);
        write(
            &dir,
            "vocab/index.md",
            "---\ntitle: Audiences\npart_of: /index.md\ncontents:\n- public.md\n- friends.md\n---\nWho may read what.\n",
        );
        write(
            &dir,
            "vocab/public.md",
            "---\ntitle: public\npart_of: index.md\n---\nAnyone.\n",
        );
        write(
            &dir,
            "vocab/friends.md",
            "---\ntitle: friends\npart_of: index.md\n---\nPeople I know.\n",
        );
        dir
    }

    #[test]
    fn a_closed_reified_vocabulary_flags_a_value_naming_no_term_node() {
        let dir = a_workspace_with_reified_audiences(
            "reified-closed",
            "closed",
            "---\ntitle: Note\npart_of: index.md\naudience: freinds\n---\n",
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
        // The load-bearing negative: the index node is a markdown *content*
        // document on purpose, so demanding a whole-file carrier of it would
        // report the shape the declaration asked for.
        assert!(
            !findings
                .iter()
                .any(|f| matches!(f, Finding::MalformedStore { .. })),
            "a reified vocabulary is content, not machinery: {findings:?}"
        );
    }

    #[test]
    fn a_reified_vocabulary_whose_values_all_name_term_nodes_is_quiet() {
        let dir = a_workspace_with_reified_audiences(
            "reified-clean",
            "closed",
            "---\ntitle: Note\npart_of: index.md\naudience: friends\n---\n",
        );
        let ws = Workspace::builder(StdFs).root(&dir).build();
        assert_eq!(block_on(ws.check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn an_open_reified_vocabulary_only_warns_on_a_near_miss() {
        let dir = a_workspace_with_reified_audiences(
            "reified-open",
            "open",
            "---\ntitle: Note\npart_of: index.md\naudience:\n- publik\n- strangers\n---\n",
        );
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let findings = block_on(ws.check("index.md")).unwrap();
        assert!(
            findings.iter().any(|f| matches!(
                f,
                Finding::TermNearMiss { value, suggestion, .. } if value == "publik" && suggestion == "public"
            )),
            "{findings:?}"
        );
        // A genuinely new value in an open vocabulary is allowed silently, term
        // nodes or not.
        assert!(
            !findings
                .iter()
                .any(|f| matches!(f, Finding::TermNearMiss { value, .. } if value == "strangers")),
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
    fn check_says_nothing_about_a_same_document_anchor() {
        // The ordinary shape of a long markdown document: headings, and links
        // down to them. None of it names a file, so none of it is prov's to
        // check — reporting it broken made `check` a thing to be filtered.
        let dir = tempdir("anchor-check");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- a.md\n---\n## Section One\n\n\
             See [Section One](#section-one), [[#section-one]], and [nothing](#no-such-heading).\n",
        );
        write(&dir, "a.md", "---\npart_of: index.md\n---\n");

        let ws = Workspace::builder(StdFs).root(&dir).build();
        let findings = block_on(ws.check("index.md")).unwrap();
        assert!(findings.is_empty(), "{findings:?}");
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

    /// The declared scope bounds `check` the way prov's own parked stores do.
    ///
    /// The case the axis exists for: another tool's store sits beside the root,
    /// its documents internally well-linked and claiming each other as parents.
    /// Undeclared, that claim reaches `missing_containment` and the store's
    /// interior becomes this workspace's findings. Declared, the walk never
    /// enters it.
    #[test]
    fn a_declared_directory_keeps_its_interior_out_of_check() {
        let dir = tempdir("scope-check");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n---\n");
        write(&dir, "a.md", "---\npart_of: index.md\n---\n");
        // A store whose top document claims the tree — the shape a bounded
        // sweep cannot ignore, because the claim is what survives the bound.
        write(&dir, "history/rev.md", "---\npart_of: ../index.md\n---\n");

        let bare = Workspace::builder(StdFs).root(&dir).build();
        let unbounded = block_on(bare.check("index.md")).unwrap();
        assert!(
            !unbounded.is_empty(),
            "undeclared, the store's claim is this workspace's problem"
        );

        let scoped = Workspace::builder(StdFs)
            .root(&dir)
            .out_of_scope([PathBuf::from("history")])
            .build();
        let findings = block_on(scoped.check("index.md")).unwrap();
        assert_eq!(
            findings,
            vec![],
            "declared out of scope, nothing in it is reported: {findings:?}"
        );
    }

    #[test]
    fn a_disconnected_island_is_reported_once_at_the_link_it_is_missing() {
        // The failure the bounded sweep cannot see: a whole subtree, internally
        // well-linked, that no `contents` anywhere points into. Every document in
        // it is unreachable and a capture would take none of them, and `check`
        // used to report **clean** — precisely *because* it never scanned the
        // directory. What survives the bound is the island's own claim: its top
        // document names a parent in the tree, and that parent does not list it.
        //
        // One finding, at that one link. The interior is left alone: writing the
        // entry makes the whole subtree reachable, so reporting its members too
        // would be four findings for one broken link.
        let dir = tempdir("island");
        write(&dir, "index.md", "---\ntitle: Home\n---\n");
        write(
            &dir,
            "daily/index.md",
            "---\ntitle: Daily\npart_of: /index.md\ncontents:\n- '[Oct](/daily/oct/index.md)'\n---\n",
        );
        write(
            &dir,
            "daily/oct/index.md",
            "---\ntitle: Oct\npart_of: /daily/index.md\ncontents:\n- '[D1](/daily/oct/d1.md)'\n---\n",
        );
        write(
            &dir,
            "daily/oct/d1.md",
            "---\ntitle: D1\npart_of: /daily/oct/index.md\n---\n",
        );
        // Never claimed anything, so it is still none of prov's business.
        write(&dir, "vendor/dup.md", "---\ntitle: Vendored\n---\n");

        let mut ws = Workspace::builder(StdFs).root(&dir).build();
        let findings = block_on(ws.check("index.md")).unwrap();
        assert_eq!(
            findings,
            vec![Finding::MissingContainment {
                doc: PathBuf::from("daily/index.md"),
                parent: PathBuf::from("index.md"),
            }],
            "one island, one finding, and the vendored tree still invisible: {findings:?}"
        );

        // And the repair is derived — the document already said where it goes —
        // so it needs no answer from anyone, and it brings the subtree back whole.
        let remedies = block_on(ws.remedies(&findings[0])).unwrap();
        assert_eq!(remedies.len(), 1);
        assert_eq!(remedies[0].warrant, Warrant::Derived);
        block_on(ws.apply_fix(&remedies[0].fix)).unwrap();
        assert_eq!(block_on(ws.check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn stacked_islands_are_all_reported_in_one_run() {
        // An island whose own parent is an island: `2025` claims `daily`, which
        // claims the root, and neither is listed. Membership is a closure for
        // exactly this reason — settling for "claims something *reachable*"
        // would report one layer per run and make a five-deep vault five runs of
        // `check --fix`.
        let dir = tempdir("island-stacked");
        write(&dir, "index.md", "---\ntitle: Home\n---\n");
        write(
            &dir,
            "d/index.md",
            "---\ntitle: Daily\npart_of: /index.md\n---\n",
        );
        write(
            &dir,
            "d/y/index.md",
            "---\ntitle: Year\npart_of: /d/index.md\n---\n",
        );

        let ws = Workspace::builder(StdFs).root(&dir).build();
        let findings = block_on(ws.check("index.md")).unwrap();
        assert_eq!(
            findings,
            vec![
                Finding::MissingContainment {
                    doc: PathBuf::from("d/index.md"),
                    parent: PathBuf::from("index.md"),
                },
                Finding::MissingContainment {
                    doc: PathBuf::from("d/y/index.md"),
                    parent: PathBuf::from("d/index.md"),
                },
            ],
            "both layers, one run: {findings:?}"
        );
    }

    #[test]
    fn an_unlinked_document_that_claims_its_parent_is_not_also_an_orphan() {
        // The same document, seen by two passes. `loose.md` sits in a reached
        // directory, so the bounded sweep calls it an orphan ("adopt it — where?"),
        // while its own `part_of` already answers the question. The specific
        // finding wins and the vague one stands down: one broken link, one finding,
        // one repair with nothing to choose.
        let dir = tempdir("orphan-claims-parent");
        write(&dir, "index.md", "---\ntitle: Home\n---\n");
        write(
            &dir,
            "loose.md",
            "---\ntitle: Loose\npart_of: /index.md\n---\n",
        );

        let findings = block_on(
            Workspace::builder(StdFs)
                .root(&dir)
                .build()
                .check("index.md"),
        )
        .unwrap();
        assert_eq!(
            findings,
            vec![Finding::MissingContainment {
                doc: PathBuf::from("loose.md"),
                parent: PathBuf::from("index.md"),
            }],
            "reported as the missing link it is, and not twice: {findings:?}"
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
