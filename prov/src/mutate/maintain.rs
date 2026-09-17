//! The plumbing every verb shares — link maintenance's own toolkit.
//!
//! Three jobs, none of which belongs to any one verb:
//!
//! - **Walking the spanning relation.** Its name and inverse
//!   ([`spanning_pair`](Workspace::spanning_pair)), up to the root a census must
//!   cover ([`spanning_root`](Workspace::spanning_root)), down the subtree a
//!   recursive op covers ([`spanning_subtree`](Workspace::spanning_subtree)),
//!   and along a single entry ([`single_target`](Workspace::single_target),
//!   [`entry_index`](Workspace::entry_index)).
//! - **Resolving a separated pair.** Which body file a node's `content` points
//!   at ([`content_target`]), and where that body sits beside a node placed
//!   somewhere new ([`body_sibling`]).
//! - **Retargeting inbound references.** Every document that links to a moved
//!   one by a path, rewritten to reach its new one — frontmatter entries and
//!   body links alike, labels and wrappers kept, id-form targets left untouched
//!   because the registry is what keeps *those* resolving.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

use fig::Segment;

use crate::identity::IdentityPolicy;
use crate::validate::Finding;
use crate::workspace::Workspace;
use crate::workspace::inbound::Form;

use super::delete::Diagnosis;
use prov_graph::document::{Document, whole_file_format};
use prov_graph::error::{Error, Result};
use prov_graph::graph::{LinkSite, Resolution, Target};
use prov_graph::link::{self, Link, LinkStyle};
use prov_graph::meta::Value;
use prov_store::edit::MetaEditor;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

/// One maintenance rewrite of an existing document: the text the op read there
/// — staged as the change set's *expectation*, so the apply refuses
/// ([`Error::Drifted`]) if another writer landed in the compute→apply gap —
/// and the text the op stages in its place.
///
/// Carrying the pre-image is what turns a maintenance sweep's read-modify-write
/// from last-write-wins into compare-and-swap: a rewrite computed from a
/// reading that no longer holds would silently drop the racing writer's edit,
/// and the census-shaped verbs (`rename`, `separate`, `combine`, `retitle`,
/// the `convert` sweeps) hold that reading open across a walk of the whole
/// reachable graph — the widest window any verb has.
pub(super) struct Rewrite {
    /// What the op read at the path — the expectation.
    pub(super) read: String,
    /// What it writes instead.
    pub(super) text: String,
}

/// What a move does to paths: where each path the op relocates lands, and
/// which paths it leaves alone.
///
/// One value for every shape a verb moves — a lone document, a separated
/// node with its body or payload beside it, a converted subtree, a whole
/// directory — so the inbound collector and the re-relativizing passes ask
/// one question of it ([`landed`](Self::landed)) and cannot disagree about
/// what moved. A directory is a rule rather than a list, because a book of
/// ten thousand photographs is one move and should cost one entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Moves {
    /// Named files, each to its own destination.
    Files(BTreeMap<PathBuf, PathBuf>),
    /// Everything under `from`, keeping its place within it, to under `to`.
    Tree { from: PathBuf, to: PathBuf },
}

impl Moves {
    /// One file, `from` to `to`.
    pub(crate) fn one(from: &Path, to: &Path) -> Self {
        Self::Files(BTreeMap::from([(from.to_path_buf(), to.to_path_buf())]))
    }

    /// Where `path` lands, or `None` when the op leaves it where it is.
    pub(crate) fn landed(&self, path: &Path) -> Option<PathBuf> {
        match self {
            Self::Files(map) => map.get(path).cloned(),
            Self::Tree { from, to } => path.strip_prefix(from).ok().map(|rest| to.join(rest)),
        }
    }

    /// `path` itself when the op leaves it alone, else where it lands — the
    /// form a pass that is *spelling* a resolved target wants.
    pub(super) fn landed_or_same(&self, path: &Path) -> PathBuf {
        self.landed(path).unwrap_or_else(|| path.to_path_buf())
    }
}

/// Whether the inbound collector rewrites a source that is itself one of the
/// movers — see [`collect_inbound_rewrites`](Workspace::collect_inbound_rewrites).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Movers {
    /// A mover's references to its fellow movers are retargeted here, because
    /// no other pass will touch its links: it stays in its directory.
    Rewrite,
    /// A mover is handed nothing: it changes directory and re-relativizes
    /// every link it holds itself, fellow movers included.
    Skip,
}

/// Walking the spanning relation needs the relation set and the resolver, and
/// neither of those is an identity concern — so these four sit outside the
/// `IdentityPolicy` bound the mutation verbs carry. `validate`'s remedy
/// suggestions read the tree without any power to mint, and that is a property
/// worth keeping in the type: a pass that only *offers* repairs must not be able
/// to register an id as a side effect of being asked.
impl<FS: Storage, IdP, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// The spanning relation's name and its inverse — mutations need both.
    pub(crate) fn spanning_pair(&self) -> Result<(String, String)> {
        let spanning = self
            .relations()
            .spanning_relation()
            .ok_or_else(|| Error::Structure("no spanning relation configured".into()))?;
        let inverse = self
            .relations()
            .relations()
            .iter()
            .find(|r| r.name == spanning)
            .and_then(|r| r.inverse.clone())
            .ok_or_else(|| {
                Error::Structure(format!("spanning relation `{spanning}` has no inverse"))
            })?;
        Ok((spanning.to_string(), inverse))
    }

    /// The single resolved target of `field` in `doc`, if it resolves to an
    /// on-workspace path (by relative path or through the registry).
    /// (`doc_path` anchors a relative target.)
    pub(crate) fn single_target(
        &self,
        doc: &Document,
        field: &str,
        doc_path: &Path,
    ) -> Option<PathBuf> {
        let raw = doc
            .meta
            .get(field)
            .map(Value::link_strings)?
            .into_iter()
            .next()?;
        match self.resolve_link(doc_path, &Link::parse(&raw)) {
            Target::Path(p) => Some(p),
            _ => None,
        }
    }

    /// The index of the entry in `doc`'s `field` sequence whose target
    /// resolves to `wanted` — by relative path or through the registry.
    pub(crate) fn entry_index(
        &self,
        doc: &Document,
        field: &str,
        doc_path: &Path,
        wanted: &Path,
    ) -> Option<usize> {
        doc.meta
            .get(field)
            .map(Value::link_strings)?
            .iter()
            .position(|raw| {
                self.resolve_link(doc_path, &Link::parse(raw)) == Target::Path(wanted.to_path_buf())
            })
    }

    /// Walk `part_of` (the spanning inverse) up from `from` to the spanning
    /// root — the document nothing contains — so a census can cover `from`'s
    /// whole workspace. A cycle or an unreadable ancestor stops the walk at the
    /// last good document, which still roots a scan over `from`'s neighborhood.
    ///
    /// **A walk that never moves is not an answer.** When `from` declares no
    /// `part_of` at all, the loop below returns `from` itself — which is right for
    /// the workspace root and wrong for every other such document, and prov
    /// authors one of those: the `about` page is reached by the root's `about`
    /// pointer and declares no parent, so it is in no spanning tree. Answering
    /// "about.md" here hands every caller a one-document workspace: `rename` and
    /// `convert` see none of its inbound references and leave the root's pointer
    /// naming a path that just moved, `retitle` relabels nothing, and
    /// [`remedy`](crate::remedy)'s config and vocabulary lookups read defaults
    /// instead of the workspace's own settings.
    ///
    /// So a walk that terminated where it started is checked against the
    /// workspace's actual root ([`root_document`](Workspace::root_document)) and
    /// yields to it. The cost falls only on that case — a document *with* a parent
    /// climbs to a genuine root and never asks — and a directory with no
    /// discoverable root (or an ambiguous one) keeps the old answer, since there
    /// is nothing better to give.
    pub(crate) async fn spanning_root(&self, from: &Path, inverse: &str) -> Result<PathBuf> {
        let mut current = from.to_path_buf();
        let mut seen = BTreeSet::new();
        while seen.insert(current.clone()) {
            let Ok((_, doc)) = self.load(&current).await else {
                break;
            };
            match self.single_target(&doc, inverse, &current) {
                Some(parent) => current = parent,
                None => break,
            }
        }
        if current == from
            && let Some(root) = self.root_document().await?
            && root != current
        {
            return Ok(root);
        }
        Ok(current)
    }
}

impl<FS: Storage, IdP: IdentityPolicy, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Every document reachable from `root` down the spanning relation, `root`
    /// included — the scope of a `recursive` per-file operation. A missing,
    /// cyclic, or unreadable child simply stops that branch; the walk never
    /// leaves the spanning tree.
    pub(super) async fn spanning_subtree(&self, root: &Path) -> Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        let mut queue = vec![root.to_path_buf()];
        while let Some(path) = queue.pop() {
            if !seen.insert(path.clone()) {
                continue;
            }
            let Ok((_, doc)) = self.load(&path).await else {
                continue;
            };
            out.push(path.clone());
            for raw in self.relations().children(&fig::Value::from(&doc.meta)) {
                if let Target::Path(child) = self.resolve_link(&path, &Link::parse(&raw)) {
                    queue.push(child);
                }
            }
        }
        Ok(out)
    }

    /// Every document that links to a path `moves` relocates, rewritten to
    /// reach where that path lands — the inbound half of a move, shared by
    /// `rename`, `move_tree`, `separate`, `combine` and the `convert` sweep.
    /// Id-form links are left untouched (the registry keeps them resolving).
    /// Keyed by the source's *current* path.
    ///
    /// One census, not one per moved path, and one accumulated text per source,
    /// not one per (source, move) pair. Both matter, and the second is the
    /// correctness half: when `a.md` and `b.md` move together and `a.md` links
    /// to `b.md`, running a single-move collector twice yields two texts for
    /// `a.md` — each computed from disk, so each missing the other's rewrite —
    /// and whichever is staged last silently drops the one before it. Folding
    /// every applicable move through the same text is what keeps a mover that
    /// references another mover correct. The same shape carries a *pair* that
    /// moves as one — a page whose `contents` names an attachment's sidecar and
    /// whose body embeds its payload is rewritten once, for both.
    ///
    /// A document's reference to *itself* is never retargeted. Its references
    /// to its fellow movers are, or not, per `movers`: a sweep that leaves each
    /// mover's directory alone ([`Movers::Rewrite`]) has no other pass that
    /// would touch them, while a mover whose directory changes re-relativizes
    /// *every* link it holds itself ([`Movers::Skip`]), fellow movers included,
    /// and handing it an inbound rewrite too would be two edits to one text.
    ///
    /// The sources come from the inbound index (`workspace::inbound`) — a
    /// census the first time a verb asks, a stat sweep and a lookup after that
    /// — which `retitle` shares, the two being the same question. `anchor` is a
    /// document whose spanning tree the census covers when it has to run.
    pub(super) async fn collect_inbound_rewrites(
        &self,
        anchor: &Path,
        moves: &Moves,
        movers: Movers,
    ) -> Result<BTreeMap<PathBuf, Rewrite>> {
        let by_source = self.inbound_sources_of(anchor, moves, Form::Path).await?;
        let mut writes = BTreeMap::new();
        for (source, froms) in by_source {
            if movers == Movers::Skip && moves.landed(&source).is_some() {
                continue;
            }
            // A memo hit where the census just read the source and the caller
            // holds the scope — so pairing the rewrite with the text it was
            // computed from costs no I/O.
            let (original, mut doc) = self.load(&source).await?;
            let mut text = original.clone();
            for from in &froms {
                let Some(to) = moves.landed(from) else {
                    continue;
                };
                if let Some(updated) = self.rewrite_inbound_text(&source, &text, &doc, from, &to)? {
                    doc = Document::parse(&source, &updated)?;
                    text = updated;
                }
            }
            if text != original {
                writes.insert(
                    source,
                    Rewrite {
                        read: original,
                        text,
                    },
                );
            }
        }
        Ok(writes)
    }

    /// Rewrite **every** entry of `field` in `doc` whose target resolves to
    /// `old` so it reaches `new` instead, preserving each entry's label and the
    /// document's formatting. Returns the updated text, or `None` when nothing
    /// matches.
    ///
    /// *Every*, not the first, and that is the whole point: one document may
    /// hold many references to the same target — a chapter of scripture cites
    /// another chapter once per verse — and rewriting one of them would leave
    /// the rest pointing at a path the move just emptied. The move would then be
    /// the author of the broken links `check` reports.
    ///
    /// Non-path entries are skipped rather than aborting the field, so a
    /// relation mixing an `id:` reference with a path reference to the same
    /// document still gets its path half rewritten. Id-form targets need no
    /// rewrite in any case — the registry keeps them resolving.
    fn retarget_entry(
        &self,
        text: &str,
        doc: &Document,
        field: &str,
        doc_path: &Path,
        old: &Path,
        new: &Path,
    ) -> Result<Option<String>> {
        let Some(value) = doc.meta.get(field) else {
            return Ok(None);
        };
        let matches = |raw: &str| {
            let link = Link::parse(raw);
            link.is_path_target()
                && self.resolve_link(doc_path, &link) == Target::Path(old.to_path_buf())
        };
        // Indices are into the *raw* sequence, not into `link_strings()` (which
        // filters non-string items and so skews every position taken from it).
        let hits: Vec<(usize, String)> = match value.as_sequence() {
            Some(items) => items
                .iter()
                .enumerate()
                .filter_map(|(i, item)| item.as_str().map(|raw| (i, raw.to_string())))
                .filter(|(_, raw)| matches(raw))
                .collect(),
            None => value
                .as_str()
                .filter(|raw| matches(raw))
                .map(|raw| vec![(0, raw.to_string())])
                .unwrap_or_default(),
        };
        if hits.is_empty() {
            return Ok(None);
        }
        let Some(carrier) = doc.carrier else {
            return Ok(None); // no metadata block: nothing to rewrite
        };
        let is_sequence = value.as_sequence().is_some();
        let style = self.reference_style_for(field).path_style;
        let mut editor = MetaEditor::open(text, carrier)?;
        for (index, raw) in hits {
            let updated = Link::parse(&raw).with_path(link::path_text(style, doc_path, new));
            // A scalar field is addressed by key; a sequence entry by key + index.
            // Replacing in place never changes the sequence's length, so indices
            // taken before the first edit stay valid through the last.
            if is_sequence {
                editor.replace_value(
                    &[Segment::Key(field), Segment::Index(index)],
                    fig::Value::Str(updated.render()),
                )?;
            } else {
                editor.replace_value(&[Segment::Key(field)], fig::Value::Str(updated.render()))?;
            }
        }
        Ok(Some(editor.render()?))
    }

    /// Retarget every path-form reference to `from` in the document at `source`
    /// so it reaches `to`: body links first (their spans index the current
    /// body), then each frontmatter relation entry (re-parsing between edits).
    /// Returns the updated text, or `None` when nothing in `source` pointed at
    /// `from`. Id-form links are skipped by [`retarget_entry`] and
    /// [`rewrite_body_inbound`] alike.
    ///
    /// Over text already in hand rather than text read from disk — the form a
    /// caller folding several moves through one document needs, since after
    /// the first rewrite the text that matters is no longer the one the
    /// filesystem holds.
    ///
    /// [`retarget_entry`]: Self::retarget_entry
    fn rewrite_inbound_text(
        &self,
        source: &Path,
        original: &str,
        doc0: &Document,
        from: &Path,
        to: &Path,
    ) -> Result<Option<String>> {
        let mut text =
            rewrite_body_inbound(original, &doc0.body, source, from, to, self.link_style());
        let mut doc = if text != original {
            Document::parse(source, &text)?
        } else {
            doc0.clone()
        };
        for relation in self.relations().relations() {
            if let Some(updated) =
                self.retarget_entry(&text, &doc, &relation.name, source, from, to)?
            {
                text = updated;
                doc = Document::parse(source, &text)?;
            }
        }
        Ok((text != original).then_some(text))
    }
}

/// The **fig index** of the entry in `doc`'s `field` whose target is written
/// exactly as `written` — the address a repair needs when the target resolves to
/// nothing, so [`entry_index`](Workspace::entry_index) (which matches on the
/// *resolved* path) cannot find it. A broken link, a dangling id, a malformed id
/// and an ambiguous alias are all in that position.
///
/// `written` is the bare target with any `[label](…)` / `[[…|…]]` wrapper
/// stripped — what [`CensusEntry::target_text`](crate::CensusEntry) and every
/// link [`Finding`](crate::Finding) carry, so a caller hands the finding's own
/// field straight through.
///
/// Two properties worth stating, because both bite:
///
/// - **The index is into the raw sequence**, not into [`Value::link_strings`],
///   which *filters* non-string items: `[a, 3, b]` yields `["a", "b"]`, so a
///   position taken from it addresses `3` when passed to
///   [`MetaEditor::remove_item`]. Enumerating the sequence itself is what keeps a
///   removal honest. (The three existing `entry_index` + `remove_item` sites
///   carry that skew; harmless while relation sequences hold only strings, and
///   left alone here rather than fixed in passing.)
/// - **A written target is not unique** — two entries in one relation may name
///   the same target. The first is returned, so a repair fixes one per run and a
///   second run finds the next.
///
/// `None` when the field is absent or nothing in it is written that way. A scalar
/// field that matches reports index 0; the caller tells scalar from sequence by
/// re-reading the value's shape, as [`retarget_entry`](Workspace::retarget_entry)
/// does.
pub(crate) fn written_entry_index(doc: &Document, field: &str, written: &str) -> Option<usize> {
    let matches = |raw: &str| Link::parse(raw).target == written;
    match doc.meta.get_path(field)? {
        Value::Sequence(items) => items
            .iter()
            .position(|item| item.as_str().is_some_and(matches)),
        other => other.as_str().is_some_and(matches).then_some(0),
    }
}

/// The fig address of a field — one key per dotted segment, so `generated.how`
/// addresses the key inside the mapping, as [`Value::get_path`] reads it.
fn field_address(field: &str) -> Vec<Segment<'_>> {
    field.split('.').map(Segment::Key).collect()
}

/// The fig address of that entry — the field alone for a scalar, field + index
/// for a sequence. The shape distinction [`MetaEditor`] needs, in one place so
/// the removal and the retarget cannot disagree about it.
fn entry_address<'a>(doc: &Document, field: &'a str, index: usize) -> Vec<Segment<'a>> {
    let mut address = field_address(field);
    if doc
        .meta
        .get_path(field)
        .and_then(Value::as_sequence)
        .is_some()
    {
        address.push(Segment::Index(index));
    }
    address
}

/// Drop the entry of `field` in `doc` written as `written`, comment- and
/// format-preservingly. Returns the updated text, or `None` when no entry is
/// written that way or the document carries no metadata block.
///
/// A scalar field loses the key itself; a sequence loses just the one item. That
/// asymmetry is the point — a `part_of:` whose only value was the offending link
/// has no meaningful empty form, while a `contents:` keeps its other children.
pub(crate) fn remove_written_entry(
    text: &str,
    doc: &Document,
    field: &str,
    written: &str,
) -> Result<Option<String>> {
    let (Some(index), Some(carrier)) = (written_entry_index(doc, field, written), doc.carrier)
    else {
        return Ok(None);
    };
    let address = entry_address(doc, field, index);
    let mut editor = MetaEditor::open(text, carrier)?;
    if matches!(address.last(), Some(Segment::Index(_))) {
        editor.remove_item(&field_address(field), index)?;
    } else {
        editor.delete(&address)?;
    }
    Ok(Some(editor.render()?))
}

/// Overwrite the entry of `field` in `doc` written as `written` with `replacement`,
/// verbatim. The shared mechanic behind both a retarget (whose replacement is a
/// rendered link) and a plain value correction (whose replacement is the value
/// itself, no link syntax involved).
pub(crate) fn replace_written_entry(
    text: &str,
    doc: &Document,
    field: &str,
    written: &str,
    replacement: &str,
) -> Result<Option<String>> {
    let (Some(index), Some(carrier)) = (written_entry_index(doc, field, written), doc.carrier)
    else {
        return Ok(None);
    };
    let mut editor = MetaEditor::open(text, carrier)?;
    editor.replace_value(
        &entry_address(doc, field, index),
        fig::Value::Str(replacement.to_string()),
    )?;
    Ok(Some(editor.render()?))
}

/// Repoint the entry of `field` in `doc` written as `written` at `new_target`
/// (a bare target, already spelled in the workspace's own style), keeping the
/// entry's label and wrapper so a `[Jul](jul.md)` stays labeled and a `[[jul]]`
/// stays a wikilink.
///
/// The sibling of [`retarget_entry`](Workspace::retarget_entry) for targets that
/// do not resolve — that one finds its entry by walking to a real path, which is
/// exactly what a broken or dangling link cannot offer.
pub(crate) fn retarget_written_entry(
    text: &str,
    doc: &Document,
    field: &str,
    written: &str,
    new_target: &str,
) -> Result<Option<String>> {
    let index = written_entry_index(doc, field, written);
    let raw = match (index, doc.meta.get_path(field)) {
        (Some(i), Some(Value::Sequence(items))) => items.get(i).and_then(Value::as_str),
        (Some(_), Some(other)) => other.as_str(),
        _ => None,
    };
    let Some(raw) = raw else { return Ok(None) };
    let rendered = Link::parse(raw).with_path(new_target.to_string()).render();
    replace_written_entry(text, doc, field, written, &rendered)
}

/// Replace the body text at `span` with `replacement`, refusing unless what is
/// there right now is exactly `expected`.
///
/// The guard is the whole point. A body span is an offset into bytes that were
/// read when `check` ran, and a repair may be applied minutes and several other
/// repairs later; splicing an offset that has since shifted would corrupt prose
/// silently and irreversibly, which is a far worse failure than declining. So the
/// span is treated as a *hint* and the text at it as the real address: if they
/// disagree, the document moved and the caller is told so.
pub(crate) fn splice_body_span(
    text: &str,
    body: &str,
    span: &Range<usize>,
    expected: &str,
    replacement: &str,
) -> Result<String> {
    if span.end > body.len() || body.get(span.clone()) != Some(expected) {
        return Err(Error::Structure(format!(
            "the document changed since it was checked — expected {expected:?} in the body, \
             found something else; re-run `check` and repair from a fresh reading"
        )));
    }
    let mut new_body = body.to_string();
    new_body.replace_range(span.clone(), replacement);
    Ok(splice_body(text, body, &new_body))
}

/// Where a separated node's body file sits beside a node placed at `node_to`,
/// and the `content` value (the body's basename) that points at it. `body_from`
/// is the current body file, whose shape decides the naming convention: an
/// **attachment** payload (opaque bytes) *is* the node's stem and already
/// carries its own extension (`hero.jpg.yaml` ↔ `hero.jpg`), while a separated
/// **prose** body shares the node's stem and keeps its own extension
/// (`notes.yaml` ↔ `notes.md`). Shared by [`rename`](super::rename)'s
/// `plan_body_move` and [`Workspace::duplicate`].
pub(super) fn body_sibling(node_to: &Path, body_from: &Path) -> (PathBuf, String) {
    let body_to = if prov_graph::document::is_opaque_payload(body_from) {
        let stem = node_to
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        node_to.with_file_name(stem)
    } else {
        let ext = body_from
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("md");
        node_to.with_extension(ext)
    };
    let new_ref = body_to
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    (body_to, new_ref)
}

/// The workspace-relative path a document's `content` attribute points at (its
/// separated body file), resolved against the document's own directory. `None`
/// for a combined document.
pub(super) fn content_target(doc: &Document, doc_path: &Path) -> Option<PathBuf> {
    let raw = doc.content_attr()?;
    let dir = doc_path.parent().unwrap_or(Path::new(""));
    Some(link::normalize(dir.join(raw)))
}

/// The workspace-relative path a document's `manifest` attribute points at (the
/// record store listing the directory it covers). `None` for a node that stands
/// for itself.
///
/// The counterpart of [`content_target`] for the bulk shape, and the two are
/// exclusive by construction — which is why the verbs that must move or remove
/// "the file that travels with this node" ask for one, then the other.
pub(super) fn manifest_target(doc: &Document, doc_path: &Path) -> Option<PathBuf> {
    let raw = doc.manifest_attr()?;
    let dir = doc_path.parent().unwrap_or(Path::new(""));
    Some(link::normalize(dir.join(raw)))
}

/// The file that travels with the node at `doc_path` — its separated body, its
/// attachment payload, or its manifest.
pub(super) fn paired_file(doc: &Document, doc_path: &Path) -> Option<PathBuf> {
    content_target(doc, doc_path).or_else(|| manifest_target(doc, doc_path))
}

impl<FS: Storage, IdP: IdentityPolicy, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// The node in `root`'s spanning subtree whose `content` names `body`, if
    /// any — the other half of [`content_target`], read backwards.
    ///
    /// A separated document is two files, and only one of them is the node: the
    /// metadata half carries the id, the links and the title, while the prose
    /// half is reached solely through its owner's `content` pointer. So a verb
    /// handed the prose half has been handed something that looks like a
    /// document and is not one, and the destructive verbs need to know that
    /// before they act — deleting the body alone leaves its node pointing at
    /// nothing, and the dangler census cannot see it, since that census walks
    /// relation and body links and `content` is neither.
    ///
    /// **Deliberately directory-local**, and for a reason the tree cannot
    /// supply: a body file is not in the spanning tree — it has no `part_of`, so
    /// walking up from it lands back on itself, and there is no subtree to
    /// search. What it does have is a *neighbourhood*: every body prov itself
    /// authors sits beside its node, because `separate`, `attach` and
    /// `duplicate` all place it with [`body_sibling`]. So one `read_dir` of the
    /// body's own directory is where the answer is, the same bound
    /// `validate`'s orphan pass draws for the same reason.
    ///
    /// The bound is a *false-negative* one, which is the safe direction here.
    /// Ownership is confirmed by resolving the candidate's actual `content`
    /// value, never inferred from a name, so this never refuses wrongly; a
    /// hand-edited `content` pointing across directories simply is not found,
    /// and the verb behaves as it did before. `check` still reports the
    /// resulting broken `content` link either way.
    ///
    /// A neighbour that fails to load is skipped rather than fatal: the question
    /// is "does something depend on these bytes", and an unreadable document is
    /// a finding `check` already raises, not a reason to block a delete.
    ///
    /// ## Only the whole-file neighbours are opened
    ///
    /// The directory bound is not enough on its own. A vault keeps a month of
    /// daily notes in one folder, so "one `read_dir`" was still nine document
    /// reads to delete the tenth — and it was *most* of what an undiagnosed
    /// delete cost (fifteen reads, nine of them here).
    ///
    /// A node that owns a body is a **whole-file metadata document**, and that is
    /// not a guess about layout: [`separate`](Workspace::separate) mints the
    /// metadata half with [`whole_file_extension`], `attach` names its sidecar by
    /// the same convention (which is why [`attachment_for`] can probe for it),
    /// and [`combine`](Workspace::combine) refuses outright to take a node that
    /// is not one. So a neighbour whose *extension* cannot carry whole-file
    /// metadata cannot be anyone's owner, and [`whole_file_format`] settles that
    /// from the path without opening the file. A folder of `.md` notes now costs
    /// the listing and nothing else.
    ///
    /// One shape is given up, and it is one prov cannot produce: frontmatter in a
    /// *markdown* document naming `content:`. `separate` will not author it and
    /// `combine` will not reverse it, so it can only be hand-written — and the
    /// cost of missing it is the cost already accepted above, a refusal that does
    /// not fire and a broken `content` link `check` reports afterwards. The
    /// converse shape is *not* given up: a body file carrying a stray
    /// frontmatter, which `combine` explicitly tolerates, is still found, because
    /// what is tested here is the neighbour's extension and never the subject's.
    ///
    /// [`whole_file_extension`]: prov_graph::document::whole_file_extension
    /// [`attachment_for`]: Workspace::attachment_for
    pub(super) async fn content_owner(&self, body: &Path) -> Result<Option<PathBuf>> {
        let dir = body.parent().unwrap_or(Path::new("")).to_path_buf();
        let neighbourhood = BTreeSet::from([dir]);
        for node in self.direct_child_files(&neighbourhood).await? {
            if node == body || whole_file_format(&node).is_none() {
                continue;
            }
            let Ok((_, doc)) = self.load(&node).await else {
                continue;
            };
            if content_target(&doc, &node).as_deref() == Some(body) {
                return Ok(Some(node));
            }
        }
        Ok(None)
    }

    /// The inbound references a removal of `path` would leave dangling —
    /// [`delete`](Workspace::delete)'s diagnosis and
    /// [`recycle`](Workspace::recycle)'s, which are the same diagnosis because a
    /// binned document is as far out of the live graph as a destroyed one.
    ///
    /// Every link that resolves to `path`, minus two that are not danglers: the
    /// document's references to itself, and the parent's spanning entry, which
    /// the verb removes rather than strands. An id-form reference becomes a
    /// [`Finding::DanglingId`] against the tombstone, everything else a
    /// [`Finding::BrokenLink`]. `owner` — set only when the caller forced away
    /// the body half of a separated pair — contributes the one finding the
    /// census structurally cannot see: `content` is neither a relation nor a
    /// body link, so no census entry exists for it.
    ///
    /// ## Cost, and why it is the caller's to spend
    ///
    /// "Which documents link here?" has one honest answer in a workspace with no
    /// backlink index, and it is a census of the whole reachable graph — every
    /// document read, to keep the handful of entries that name `path`. On a
    /// 2438-document archive that was the entire cost of a delete: 2383 document
    /// reads to move one file to the bin, where the move itself reads six.
    ///
    /// prov will not keep an index to make it cheap (DESIGN §8 — the census is
    /// ground truth precisely because nothing stores a second copy of it to
    /// drift), and it will not silently stop answering. So the choice is the
    /// caller's, and [`Diagnosis::Skip`] is for the caller that already ignores
    /// the answer — a GUI that deletes on a click and shows no diagnosis has
    /// been paying a whole-workspace pass per click to build a `Vec` it drops.
    /// Skipping costs nothing but the diagnosis: `check` reports exactly the
    /// same dangling references whenever it is next run.
    ///
    /// The root the census is anchored to is walked *here*, so `Skip` provably
    /// spends nothing — not the pass, and not the walk up to the document it
    /// starts from.
    pub(super) async fn removal_danglers(
        &self,
        diagnosis: Diagnosis,
        path: &Path,
        parent: Option<&Path>,
        owner: Option<&Path>,
    ) -> Result<Vec<Finding>> {
        if diagnosis == Diagnosis::Skip {
            return Ok(Vec::new());
        }
        let (spanning, inverse) = self.spanning_pair()?;
        let root = self.spanning_root(path, &inverse).await?;
        let mut danglers: Vec<Finding> = self
            .census(&root)
            .await?
            .into_iter()
            .filter(|e| e.resolution.resolved_path().map(PathBuf::as_path) == Some(path))
            .filter(|e| {
                e.source != path
                    && !(Some(e.source.as_path()) == parent
                        && e.site.relation() == Some(spanning.as_str()))
            })
            .map(|e| match e.resolution {
                Resolution::Id { id, .. } => Finding::DanglingId {
                    doc: e.source,
                    site: e.site,
                    id,
                    tombstoned: true,
                },
                _ => Finding::BrokenLink {
                    doc: e.source,
                    site: e.site,
                    target: e.target_text,
                },
            })
            .collect();

        // The forced-body case the census cannot reach, as above.
        if let Some(owner) = owner {
            let target = self
                .load(owner)
                .await
                .ok()
                .and_then(|(_, doc)| doc.content_attr().map(str::to_string))
                .unwrap_or_else(|| path.to_string_lossy().into_owned());
            danglers.push(Finding::BrokenLink {
                doc: owner.to_path_buf(),
                site: LinkSite::field("content"),
                target,
            });
        }
        Ok(danglers)
    }
}

/// Replace the single verbatim occurrence of `old_body` in `text` with
/// `new_body`. The body sits at one end of the document (a suffix under
/// frontmatter, a prefix under endmatter, or the whole text when there is no
/// metadata block), so those cases are matched directly; the general
/// single-replacement is the fallback.
pub(crate) fn splice_body(text: &str, old_body: &str, new_body: &str) -> String {
    if let Some(head) = text.strip_suffix(old_body) {
        format!("{head}{new_body}")
    } else if let Some(tail) = text.strip_prefix(old_body) {
        format!("{new_body}{tail}")
    } else {
        text.replacen(old_body, new_body, 1)
    }
}

/// Retarget the path-form body links in `source` that resolve to `from` so
/// they reach `to` instead, splicing the result back into `text` — both
/// `[[wikilinks]]` and markdown/djot `[t](a)` links. Id-form and external
/// targets are left untouched. Rewrites right-to-left so each span stays valid
/// as earlier ones are replaced. Returns `text` unchanged when no body link
/// pointed at `from`.
fn rewrite_body_inbound(
    text: &str,
    body: &str,
    source: &Path,
    from: &Path,
    to: &Path,
    style: LinkStyle,
) -> String {
    if body.is_empty() {
        return text.to_string();
    }
    let mut new_body = body.to_string();
    let mut changed = false;
    for bl in link::scan_body_links(source, body).into_iter().rev() {
        if !bl.is_path_target() {
            continue;
        }
        if link::resolve(source, &bl.link.target).as_path() != from {
            continue;
        }
        let retargeted = bl
            .link
            .with_path(link::path_text(style, source, to))
            .render();
        new_body.replace_range(bl.span.clone(), &retargeted);
        changed = true;
    }
    if !changed {
        return text.to_string();
    }
    splice_body(text, body, &new_body)
}

// The shared machinery has no fixtures of its own — each verb's tests exercise
// it through that verb. What follows is the exception: the boundary a
// sub-workspace draws is a property of `spanning_root` itself, and asking it
// through `rename` alone would leave the stop condition implied.
#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::super::support::*;
    use super::*;

    /// A workspace whose node names `README.md` as the root, and whose root says
    /// `part_of` an id in another workspace — a workspace inside a workspace,
    /// opened at the inner one.
    fn sub_workspace(tag: &str) -> PathBuf {
        let dir = tempdir(tag);
        write(
            &dir,
            "README.md",
            "---\ntitle: Inner\npart_of: id:outer/abc123\ncontents:\n- '[Note](/note.md)'\n---\n",
        );
        write(
            &dir,
            "note.md",
            "---\ntitle: Note\npart_of: '[Inner](/README.md)'\n---\n",
        );
        write(&dir, "prov.yaml", "workspace_id: inner\nroot: README.md\n");
        dir
    }

    #[test]
    fn spanning_root_stops_at_a_root_whose_parent_is_foreign() {
        // The foreign edge resolves to nothing local, so `single_target` yields
        // `None` and the climb ends here rather than walking out of the
        // workspace. This is what makes the sub-workspace a boundary without a
        // single line of boundary-handling code.
        let dir = sub_workspace("spanning-root-foreign");
        let ws = Workspace::builder(StdFs)
            .root(&dir)
            .workspace_id("inner")
            .build();
        assert_eq!(
            block_on(ws.spanning_root(Path::new("note.md"), "part_of")).unwrap(),
            PathBuf::from("README.md")
        );
    }

    #[test]
    fn a_move_inside_a_sub_workspace_leaves_the_foreign_parent_alone() {
        // Every rewrite site filters on a *path* target, and no id form is one —
        // so the edge that says what contains this workspace survives a move
        // that rewrites the root's `contents` right beside it.
        let dir = sub_workspace("move-under-foreign-parent");
        let mut ws = Workspace::builder(StdFs)
            .root(&dir)
            .workspace_id("inner")
            .build();
        block_on(ws.rename(Path::new("note.md"), Path::new("renamed.md"))).unwrap();

        let root = read(&dir, "README.md");
        assert!(
            root.contains("part_of: id:outer/abc123"),
            "the foreign parent is carried, never rewritten: {root}"
        );
        assert!(root.contains("renamed.md"), "the move landed: {root}");
        assert_eq!(block_on(ws.check("README.md")).unwrap(), vec![]);
    }
}
