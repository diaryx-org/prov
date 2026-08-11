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

use std::collections::BTreeSet;
use std::ops::Range;
use std::path::{Path, PathBuf};

use fig::Segment;

use crate::identity::IdentityPolicy;
use crate::workspace::Workspace;
use prov_graph::document::Document;
use prov_store::edit::MetaEditor;
use prov_graph::error::{Error, Result};
use prov_store::fs::Storage;
use prov_graph::graph::{Resolution, Target};
use prov_store::index::IndexStore;
use prov_graph::link::{self, Link};
use prov_graph::meta::Value;

/// Walking the spanning relation needs the relation set and the resolver, and
/// neither of those is an identity concern — so these three sit outside the
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

    /// Walk `part_of` (the spanning inverse) up from `from` to the spanning
    /// root — the document nothing contains — so a census can cover `from`'s
    /// whole workspace. A cycle or an unreadable ancestor stops the walk at the
    /// last good document, which still roots a scan over `from`'s neighborhood.
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
            for raw in self.relations().children(&doc.meta) {
                if let Target::Path(child) = self.resolve_link(&path, &Link::parse(&raw)) {
                    queue.push(child);
                }
            }
        }
        Ok(out)
    }

    /// Every document that links to `from` by a path, rewritten to point at `to`
    /// — the inbound half of a move. Reused by `rename`, `separate`, and
    /// `combine`. Id-form links are left untouched (the registry keeps them
    /// resolving); `from`'s own links are excluded (the mover rewrites those
    /// itself). Returns `(source_path, new_text)` pairs.
    pub(super) async fn collect_inbound_rewrites(
        &self,
        from: &Path,
        to: &Path,
    ) -> Result<Vec<(PathBuf, String)>> {
        let (_spanning, inverse) = self.spanning_pair()?;
        let root = self.spanning_root(from, &inverse).await?;
        let mut sources: BTreeSet<PathBuf> = self
            .census(&root)
            .await?
            .into_iter()
            .filter(|e| {
                matches!(&e.resolution,
                    Resolution::Path(p) | Resolution::CaseMismatch { got: p, .. } if p == from)
            })
            .map(|e| e.source)
            .collect();
        sources.remove(from);
        let mut writes = Vec::new();
        for source in sources {
            if let Some(updated) = self.rewrite_inbound_doc(&source, from, to).await? {
                writes.push((source, updated));
            }
        }
        Ok(writes)
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

    /// Rewrite the entry of `field` in `doc` whose target resolves to `old` so
    /// it reaches `new` instead, preserving the entry's label and the
    /// document's formatting. Returns the updated text, or `None` when no
    /// entry matches — or when the matching entry is a `colophon:<id>`
    /// reference, which needs no rewrite: the registry keeps it resolving.
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
        let entries = value.link_strings();
        let dir = doc_path.parent().unwrap_or(Path::new(""));
        let Some(index) = entries.iter().position(|raw| {
            self.resolve_link(doc_path, &Link::parse(raw)) == Target::Path(old.to_path_buf())
        }) else {
            return Ok(None);
        };
        let entry = Link::parse(&entries[index]);
        if entry.id_target().is_some() {
            // Linked by ID: stable across the move by construction.
            return Ok(None);
        }
        let updated = entry.with_target(link::relative(dir, new));
        let Some(carrier) = doc.carrier else {
            return Ok(None); // no metadata block: nothing to rewrite
        };
        let mut editor = MetaEditor::open(text, carrier)?;
        // A scalar field is addressed by key; a sequence entry by key + index.
        if value.as_sequence().is_some() {
            editor.replace_value(
                &[Segment::Key(field), Segment::Index(index)],
                fig::Value::Str(updated.render()),
            )?;
        } else {
            editor.replace_value(&[Segment::Key(field)], fig::Value::Str(updated.render()))?;
        }
        Ok(Some(editor.render()?))
    }

    /// Retarget every path-form reference to `from` in the document at `source`
    /// so it reaches `to`: body wikilinks first (their spans index the current
    /// body), then each frontmatter relation entry (re-parsing between edits).
    /// Returns the updated text, or `None` when nothing in `source` pointed at
    /// `from`. Id-form links are skipped by [`retarget_entry`] and
    /// [`rewrite_body_inbound`] alike.
    async fn rewrite_inbound_doc(
        &self,
        source: &Path,
        from: &Path,
        to: &Path,
    ) -> Result<Option<String>> {
        let (original, doc0) = self.load(source).await?;
        let mut text = rewrite_body_inbound(&original, &doc0.body, source, from, to);
        let mut doc = if text != original {
            Document::parse(source, &text)?
        } else {
            doc0
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
    match doc.meta.get(field)? {
        Value::Sequence(items) => items
            .iter()
            .position(|item| item.as_str().is_some_and(matches)),
        other => other.as_str().is_some_and(matches).then_some(0),
    }
}

/// The fig address of that entry — key alone for a scalar field, key + index for
/// a sequence. The shape distinction [`MetaEditor`] needs, in one place so the
/// removal and the retarget cannot disagree about it.
fn entry_address<'a>(doc: &Document, field: &'a str, index: usize) -> Vec<Segment<'a>> {
    match doc.meta.get(field).and_then(Value::as_sequence) {
        Some(_) => vec![Segment::Key(field), Segment::Index(index)],
        None => vec![Segment::Key(field)],
    }
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
    if address.len() == 1 {
        editor.delete(&address)?;
    } else {
        editor.remove_item(&[Segment::Key(field)], index)?;
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
    let raw = match (index, doc.meta.get(field)) {
        (Some(i), Some(Value::Sequence(items))) => items.get(i).and_then(Value::as_str),
        (Some(_), Some(other)) => other.as_str(),
        _ => None,
    };
    let Some(raw) = raw else { return Ok(None) };
    let rendered = Link::parse(raw)
        .with_target(new_target.to_string())
        .render();
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
    pub(super) async fn content_owner(&self, body: &Path) -> Result<Option<PathBuf>> {
        let dir = body.parent().unwrap_or(Path::new("")).to_path_buf();
        let neighbourhood = BTreeSet::from([dir]);
        for node in self.direct_child_files(&neighbourhood).await? {
            if node == body {
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
fn rewrite_body_inbound(text: &str, body: &str, source: &Path, from: &Path, to: &Path) -> String {
    if body.is_empty() {
        return text.to_string();
    }
    let source_dir = source.parent().unwrap_or(Path::new(""));
    let mut new_body = body.to_string();
    let mut changed = false;
    for bl in link::scan_body_links(source, body).into_iter().rev() {
        if !bl.is_path_target() {
            continue;
        }
        if link::resolve(source, &bl.link.target).as_path() != from {
            continue;
        }
        let retargeted = bl.link.with_target(link::relative(source_dir, to)).render();
        new_body.replace_range(bl.span.clone(), &retargeted);
        changed = true;
    }
    if !changed {
        return text.to_string();
    }
    splice_body(text, body, &new_body)
}
