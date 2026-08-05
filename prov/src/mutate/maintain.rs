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
use std::path::{Path, PathBuf};

use fig::Segment;

use crate::document::Document;
use crate::edit::MetaEditor;
use crate::error::{Error, Result};
use crate::fs::Storage;
use crate::identity::IdentityPolicy;
use crate::index::IndexStore;
use crate::link::{self, Link};
use crate::meta::Value;
use crate::validate::Resolution;
use crate::workspace::{Target, Workspace};

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
    pub(super) fn single_target(
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
    pub(super) fn entry_index(
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

    /// Walk `part_of` (the spanning inverse) up from `from` to the spanning
    /// root — the document nothing contains — so a census can cover `from`'s
    /// whole workspace. A cycle or an unreadable ancestor stops the walk at the
    /// last good document, which still roots a scan over `from`'s neighborhood.
    pub(super) async fn spanning_root(&self, from: &Path, inverse: &str) -> Result<PathBuf> {
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

/// Where a separated node's body file sits beside a node placed at `node_to`,
/// and the `content` value (the body's basename) that points at it. `body_from`
/// is the current body file, whose shape decides the naming convention: an
/// **attachment** payload (opaque bytes) *is* the node's stem and already
/// carries its own extension (`hero.jpg.yaml` ↔ `hero.jpg`), while a separated
/// **prose** body shares the node's stem and keeps its own extension
/// (`notes.yaml` ↔ `notes.md`). Shared by [`rename`](super::rename)'s
/// `plan_body_move` and [`Workspace::duplicate`].
pub(super) fn body_sibling(node_to: &Path, body_from: &Path) -> (PathBuf, String) {
    let body_to = if crate::document::is_opaque_payload(body_from) {
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

/// Replace the single verbatim occurrence of `old_body` in `text` with
/// `new_body`. The body sits at one end of the document (a suffix under
/// frontmatter, a prefix under endmatter, or the whole text when there is no
/// metadata block), so those cases are matched directly; the general
/// single-replacement is the fallback.
pub(super) fn splice_body(text: &str, old_body: &str, new_body: &str) -> String {
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
        if bl.id_target().is_some() || bl.link.is_external() {
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
