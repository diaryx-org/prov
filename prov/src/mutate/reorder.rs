//! `reorder` — a container's children change places; nothing else does.
//!
//! The one containment verb that repoints no link and rewrites no inverse. The
//! spanning sequence is a container's *order* as well as its membership — a
//! shelf reads left to right, a book chapter by chapter — and a drag that puts
//! the books in a new order is a change to the shelf alone. The children never
//! learn of it: their inverse names the parent, not a position in it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use fig::Segment;

use crate::identity::IdentityPolicy;
use crate::workspace::Workspace;
use prov_graph::error::{Error, Result};
use prov_graph::graph::Target;
use prov_graph::link::{self, Link};
use prov_store::edit::MetaEditor;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

/// What [`reorder`](Workspace::reorder) actually did.
///
/// Two answers, because a caller that reports "reordered" for
/// [`Unchanged`](Reordered::Unchanged) is telling the user a file changed when
/// nothing was written — and a drop that lands a book back where it was picked
/// up is the commonest drop there is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reordered {
    /// The parent's spanning sequence was rewritten in the new order.
    Moved,
    /// The order asked for is the order the parent already had. Nothing was
    /// written.
    Unchanged,
}

impl<FS: Storage, IdP: IdentityPolicy, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Put the children of `parent` in the order `order` names, touching
    /// nothing else.
    ///
    /// `order` names children by workspace-relative path. Each must be one the
    /// parent's spanning sequence already lists — this verb changes *where* a
    /// child sits, never *whether* it is there, which is [`adopt`](Self::adopt)'s
    /// and [`reparent`](Self::reparent)'s job — so a path the parent does not
    /// list is refused, and so is a path named twice, since a permutation with a
    /// repeat in it is not one. Refused means nothing written.
    ///
    /// The children themselves are not edited: their inverse names the parent,
    /// not a position in it, so it is already right. The one file that changes
    /// is the parent, through the same comment-preserving editor as every other
    /// verb here, so a comment beside an entry and the spelling of each link
    /// (its label, its style) survive the move. When the order asked for is the
    /// order already held, nothing is written and the return value says so.
    ///
    /// ## The entries the caller did not name
    ///
    /// A spanning sequence can hold more than the paths a caller can see: a
    /// foreign `id:` reference into another workspace, an id the registry no
    /// longer resolves, a URL. A shelf being dragged shows the books it could
    /// resolve, so those are the entries `order` names, and an entry it could
    /// not resolve cannot be in it. The caller may also, deliberately, name only
    /// some of the children.
    ///
    /// Whatever it did not name **keeps its place**. The slots the named entries
    /// occupy are refilled with those entries in the caller's order, and every
    /// other slot keeps the entry it had — a permutation of the named entries
    /// among themselves, around the rest. The alternative, which the underlying
    /// editor's [`reorder_items`](MetaEditor::reorder_items) offers as its
    /// default (the named entries first, the rest after), would make a drag of
    /// two books quietly move a foreign reference to the end of the shelf: a
    /// change nobody asked for, in a file the caller thought it understood. An
    /// entry a caller cannot see is an entry a caller must not move.
    ///
    /// A child the sequence lists twice — a `check` finding in its own right — is
    /// named by its first occurrence; the second is an entry the caller did not
    /// name, and stays put like any other.
    pub async fn reorder(&mut self, parent: &Path, order: &[PathBuf]) -> Result<Reordered> {
        let parent = link::normalize(parent);
        let (spanning, _) = self.spanning_pair()?;
        if !self.exists(&parent).await? {
            return Err(Error::NotFound(parent));
        }
        let (parent_text, parent_doc) = self.load(&parent).await?;
        let parent_meta = fig::Value::from(&parent_doc.meta);

        // Each entry as the path it names here, or `None` for one that names no
        // path in this workspace — the entries `order` cannot mention, and which
        // therefore keep their places.
        let entries: Vec<Option<PathBuf>> = self
            .relations()
            .children(&parent_meta)
            .iter()
            .map(|t| match self.resolve_link(&parent, &Link::parse(t)) {
                Target::Path(p) => Some(p),
                _ => None,
            })
            .collect();

        // Where each named child sits now, in the caller's order.
        let mut named = Vec::with_capacity(order.len());
        let mut seen = BTreeSet::new();
        for wanted in order {
            let wanted = link::normalize(wanted);
            let Some(index) = entries
                .iter()
                .position(|entry| entry.as_deref() == Some(wanted.as_path()))
            else {
                return Err(Error::Structure(format!(
                    "{} is not a child of {} — reorder changes where a child sits, not \
                     whether it is there",
                    wanted.display(),
                    parent.display()
                )));
            };
            if !seen.insert(index) {
                return Err(Error::Structure(format!(
                    "{} is named twice in the order for {}",
                    wanted.display(),
                    parent.display()
                )));
            }
            named.push(index);
        }

        // The permutation over the whole sequence: the named entries' own slots,
        // in sequence order, take the named entries in the caller's order; every
        // other slot keeps what it had.
        let mut indices: Vec<usize> = (0..entries.len()).collect();
        for (slot, from) in seen.iter().zip(&named) {
            indices[*slot] = *from;
        }
        if indices.iter().enumerate().all(|(slot, from)| slot == *from) {
            return Ok(Reordered::Unchanged);
        }

        // A sequence with entries in it was read out of a metadata block, so the
        // carrier is there; the refusal is for the type, not a case.
        let Some(carrier) = parent_doc.carrier else {
            return Err(Error::Structure(format!(
                "{} has no metadata block to reorder",
                parent.display()
            )));
        };
        let mut editor = MetaEditor::open(&parent_text, carrier)?;
        editor.reorder_items(&[Segment::Key(&spanning)], &indices)?;
        let mut cs = self.change();
        cs.write(&parent, editor.render()?);
        self.commit(cs).await?;
        Ok(Reordered::Moved)
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::super::support::*;
    use super::*;

    /// A shelf of three, each child claiming the shelf back.
    fn shelf(tag: &str) -> PathBuf {
        let dir = tempdir(tag);
        write(
            &dir,
            "index.md",
            "---\ntitle: Shelf\ncontents:\n- '[A](/a.md)'\n- '[B](/b.md)'\n- '[C](/c.md)'\n---\n",
        );
        for name in ["a", "b", "c"] {
            write(
                &dir,
                &format!("{name}.md"),
                format!(
                    "---\ntitle: {}\npart_of: '[Shelf](/index.md)'\n---\n{name} body.\n",
                    name.to_uppercase()
                ),
            );
        }
        dir
    }

    /// The byte offsets of each child's entry in the shelf, so a test can assert
    /// an order rather than a substring.
    fn positions(text: &str) -> Vec<usize> {
        ["/a.md", "/b.md", "/c.md"]
            .iter()
            .map(|needle| {
                text.find(needle)
                    .unwrap_or_else(|| panic!("{needle} in {text}"))
            })
            .collect()
    }

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn reorder_permutes_the_children_and_touches_nothing_else() {
        // The shelf changes; the books do not, because their `part_of` names
        // the shelf and not a position in it.
        let dir = shelf("reorder");
        let books_before: Vec<String> = ["a.md", "b.md", "c.md"]
            .iter()
            .map(|p| read(&dir, p))
            .collect();

        let outcome =
            block_on(ws(&dir).reorder(Path::new("index.md"), &paths(&["c.md", "a.md", "b.md"])))
                .unwrap();
        assert_eq!(outcome, Reordered::Moved);

        let shelf = read(&dir, "index.md");
        let [a, b, c] = positions(&shelf)[..] else {
            unreachable!()
        };
        assert!(c < a && a < b, "c, a, b in that order: {shelf}");
        let books_after: Vec<String> = ["a.md", "b.md", "c.md"]
            .iter()
            .map(|p| read(&dir, p))
            .collect();
        assert_eq!(books_after, books_before, "the children are not edited");
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn reorder_into_the_order_already_held_writes_nothing() {
        // A drop that lands a book where it was picked up: the commonest drop,
        // and the one a caller must not report as a change.
        let dir = shelf("reorder-idem");
        let before = snapshot(&dir);

        let outcome =
            block_on(ws(&dir).reorder(Path::new("index.md"), &paths(&["a.md", "b.md", "c.md"])))
                .unwrap();
        assert_eq!(outcome, Reordered::Unchanged);
        assert_eq!(snapshot(&dir), before, "nothing written");

        // Naming nothing at all is the same nothing.
        let outcome = block_on(ws(&dir).reorder(Path::new("index.md"), &[])).unwrap();
        assert_eq!(outcome, Reordered::Unchanged);
        assert_eq!(snapshot(&dir), before);
    }

    #[test]
    fn reorder_refuses_a_path_the_parent_does_not_list() {
        // The verb changes where a child sits, never whether it is there —
        // that is `adopt`'s and `reparent`'s job. Refused means untouched.
        let dir = shelf("reorder-stranger");
        write(&dir, "loose.md", "---\ntitle: Loose\n---\n");
        let before = snapshot(&dir);

        let err = block_on(ws(&dir).reorder(Path::new("index.md"), &paths(&["loose.md", "a.md"])))
            .unwrap_err();
        assert!(
            matches!(err, Error::Structure(_)) && err.to_string().contains("not a child"),
            "{err}"
        );
        assert_eq!(snapshot(&dir), before, "a refusal writes nothing");
    }

    #[test]
    fn reorder_refuses_a_child_named_twice() {
        // A permutation with a repeat in it is not one.
        let dir = shelf("reorder-twice");
        let before = snapshot(&dir);

        let err =
            block_on(ws(&dir).reorder(Path::new("index.md"), &paths(&["b.md", "a.md", "b.md"])))
                .unwrap_err();
        assert!(
            matches!(err, Error::Structure(_)) && err.to_string().contains("named twice"),
            "{err}"
        );
        assert_eq!(snapshot(&dir), before, "a refusal writes nothing");
    }

    #[test]
    fn an_entry_the_caller_did_not_name_keeps_its_place() {
        // A foreign reference sits second on the shelf. The caller cannot see
        // it, so it cannot name it; swapping the two it can see must leave the
        // stranger exactly where it was — not pushed to the end.
        let dir = tempdir("reorder-unnamed");
        write(
            &dir,
            "index.md",
            "---\ntitle: Shelf\ncontents:\n- '[A](/a.md)'\n- id:other/zzzzzzz\n- '[C](/c.md)'\n---\n",
        );
        for name in ["a", "c"] {
            write(
                &dir,
                &format!("{name}.md"),
                format!("---\ntitle: {name}\npart_of: '[Shelf](/index.md)'\n---\n"),
            );
        }

        let outcome =
            block_on(ws(&dir).reorder(Path::new("index.md"), &paths(&["c.md", "a.md"]))).unwrap();
        assert_eq!(outcome, Reordered::Moved);
        let shelf = read(&dir, "index.md");
        let c = shelf.find("/c.md").unwrap();
        let foreign = shelf.find("id:other/zzzzzzz").unwrap();
        let a = shelf.find("/a.md").unwrap();
        assert!(
            c < foreign && foreign < a,
            "c, then the stranger where it was, then a: {shelf}"
        );
    }

    #[test]
    fn reorder_keeps_a_comment_and_the_spelling_of_every_link() {
        // The whole reason to go through the lossless editor: a comment beside
        // an entry, and a sequence whose entries are spelled three different
        // ways, come through the move as written.
        let dir = tempdir("reorder-lossless");
        write(
            &dir,
            "index.md",
            "---\ntitle: Shelf\ncontents:\n- '[A](/a.md)' # the first book\n- b.md\n- '[[c.md]]'\n---\n",
        );
        for name in ["a", "b", "c"] {
            write(
                &dir,
                &format!("{name}.md"),
                format!("---\ntitle: {name}\npart_of: '[Shelf](/index.md)'\n---\n"),
            );
        }

        block_on(ws(&dir).reorder(Path::new("index.md"), &paths(&["b.md", "c.md", "a.md"])))
            .unwrap();
        let shelf = read(&dir, "index.md");
        assert!(
            shelf.contains("- b.md"),
            "the bare spelling survives: {shelf}"
        );
        assert!(
            shelf.contains("- '[[c.md]]'"),
            "the wikilink survives: {shelf}"
        );
        assert!(
            shelf.contains("- '[A](/a.md)' # the first book"),
            "the labelled link and its comment survive together: {shelf}"
        );
        let b = shelf.find("- b.md").unwrap();
        let c = shelf.find("[[c.md]]").unwrap();
        let a = shelf.find("/a.md").unwrap();
        assert!(b < c && c < a, "b, c, a: {shelf}");
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }
}
