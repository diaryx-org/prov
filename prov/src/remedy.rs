//! Remedies — what prov offers to *do* about a [`Finding`].
//!
//! [`validate`](crate::validate) names what is wrong; this module names what
//! could be done about it, and does it. The split is the same one that keeps
//! [`graph`](crate::graph) ignorant of `Finding`, applied one layer up: a
//! finding is a statement about the workspace, and a statement carries no
//! opinion about which of several defensible repairs a person wants.
//!
//! Three types, in widening order:
//!
//! - [`Fix`] — a fully-determined action, the thing [`apply_fix`] performs.
//! - [`Warrant`] — how settled a repair is: [`Derived`](Warrant::Derived) (a
//!   pure function of an authority, safe unattended), [`Judgment`] (rivals
//!   exist), [`Destructive`] (removes something authored).
//! - [`Remedy`] — a `Fix` plus its `Warrant`, a [`RemedyKind`] slug, and the
//!   sentence describing what it would do.
//!
//! [`remedies`] is the general surface: a finding may offer several, ranked, and
//! a caller picks. [`suggest_fix`] survives as the one-answer view over it (the
//! first non-destructive remedy) for callers that do not want to choose.
//!
//! **What a repair may touch.** Structure only — frontmatter, or a span a
//! *parser* reported as a link — and never ordinary prose, and never a file.
//! [`Fix`]'s own docs carry the argument; it is the rule the whole module is
//! written to keep.
//!
//! [`Judgment`]: Warrant::Judgment
//! [`Destructive`]: Warrant::Destructive
//! [`apply_fix`]: Workspace::apply_fix
//! [`remedies`]: Workspace::remedies
//! [`suggest_fix`]: Workspace::suggest_fix

use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::fs::Storage;
use crate::graph::{LinkSite, Target};
use crate::identity::{Id, IdentityPolicy};
use crate::index::IndexStore;
use crate::link::{self, Link};
use crate::meta::Value;
use crate::mutate::maintain;
use crate::validate::Finding;
use crate::workspace::Workspace;

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

impl<FS: Storage, IdP, Ix: IndexStore> Workspace<FS, IdP, Ix> {
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
        let Ok(entries) = self.listing(dir).await else {
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
            if let Ok(entries) = self.listing(&current).await {
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
                // Only the author knows what this workspace should be called,
                // and picking a name for them would put it in every reference
                // that ever points here. Diagnosis only.
                crate::config::ConfigIssueKind::MalformedWorkspaceId { .. } => Ok(Vec::new()),
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

/// Remedies — the repairs a finding offers when more than one is defensible.
// These tests use YAML frontmatter fixtures, so they run under the `yaml` feature.
#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::*;
    use crate::exec::block_on;
    use crate::fs::StdFs;
    use crate::identity::Minter;
    use crate::index::FileIndex;
    use crate::index::IdIndex;
    use crate::link::LinkStyle;

    fn write(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prov-remedy-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

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

    #[test]
    fn suggests_and_applies_a_missing_inverse_fix() {
        let dir = tempdir("autofix");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n---\n");
        write(&dir, "a.md", "---\ntitle: A\n---\n"); // no part_of → MissingInverse
        // Bare relative style keeps the assertion about the fix simple.
        let mut ws = Workspace::builder(StdFs)
            .root(&dir)
            .link_style(LinkStyle::PlainRelative)
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
        // a.md now declares the back-link (bare relative), and it validates.
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
}
