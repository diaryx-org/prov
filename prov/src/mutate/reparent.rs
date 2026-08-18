//! `reparent` — a document's place in the containment tree changes, its path
//! does not.
//!
//! The mirror image of [`rename`](super::rename), and deliberately orthogonal to
//! it: containment is link-shaped rather than directory-shaped (DESIGN §3), so
//! moving a node in the tree and moving its file are separate decisions and
//! separate calls.

use std::collections::BTreeSet;
use std::path::Path;

use fig::Segment;

use crate::identity::IdentityPolicy;
use crate::workspace::Workspace;
use prov_graph::error::{Error, Result};
use prov_graph::graph::Target;
use prov_graph::link::{self, Link};
use prov_store::edit::MetaEditor;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

/// What [`reparent`](Workspace::reparent) actually did.
///
/// The three answers are worth telling apart because two of them look like
/// success and only one of them is a move. A caller that reports "reparented"
/// for [`Unchanged`](Reparented::Unchanged) is telling the user a containment
/// changed when nothing did — which is exactly how a workspace full of
/// documents claiming a parent that never listed them stays that way through a
/// repair run that reported success on every one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reparented {
    /// The child changed hands: its inverse repointed, the new parent gained a
    /// spanning entry, the old parent lost the one it had.
    Moved,
    /// The child already claimed this parent and the parent did not list it, so
    /// only the missing forward link was written. The repair `adopt` would have
    /// made — see [`reparent`](Workspace::reparent)'s note on the half-linked
    /// child.
    Linked,
    /// Both directions already held. Nothing was written.
    Unchanged,
}

impl<FS: Storage, IdP: IdentityPolicy, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Move the document at `child` to a different `parent` in the containment
    /// tree, leaving the file exactly where it is.
    ///
    /// The mirror image of [`rename`](Self::rename), and deliberately orthogonal
    /// to it: `rename` changes a document's **path** and preserves its place in
    /// the tree; `reparent` changes its **place in the tree** and preserves its
    /// path. Neither implies the other, because containment is link-shaped rather
    /// than directory-shaped (DESIGN §3) — a node may live in any directory, so
    /// relocating the file is a separate decision and a separate call.
    ///
    /// Distinct from [`adopt`](Self::adopt), which is *additive* and refuses a
    /// child that already claims a different parent. This is the verb for that
    /// refusal's other half: it *replaces* the claim, removing the old parent's
    /// spanning entry rather than leaving a document contained twice. An
    /// unparented child is accepted too, in which case there is nothing to remove
    /// and the effect is exactly `adopt`'s.
    ///
    /// ## The child that already claims this parent
    ///
    /// A child whose inverse already names `parent` is **not** finished, and
    /// used to be treated as though it were: the call returned success having
    /// written nothing, while the parent went on not listing the child — so the
    /// document stayed unreachable and the "repair" reported that it had worked.
    /// That is the state most orphans are actually in (they declared `part_of`
    /// at an index that never listed them back), which made the silent return
    /// the wrong answer for the commonest case rather than a rare one.
    ///
    /// So the two directions are now judged separately. Whichever one is missing
    /// is written and the other is left alone, exactly as [`adopt`](Self::adopt)
    /// does; only when both already hold is nothing written. The return value
    /// says which of the three happened, so a caller need not report a move that
    /// did not occur.
    ///
    /// ## The old parent that is no longer there
    ///
    /// The child's `part_of` may name a document that has been deleted out of
    /// band — an `id:` reference whose registry entry outlived its file is the
    /// usual way. There is then no spanning entry to remove, so its absence is
    /// not an error: step 3 skips it. A dangling parent is precisely the state
    /// in which reparenting is most needed, and refusing to run until the stale
    /// key is cleared by hand made the verb useless exactly there.
    ///
    /// ## Failure ordering
    ///
    /// Three documents change, and they land as one [`ChangeSet`]: an I/O
    /// failure at any of them unwinds the rest, so no error leaves the child
    /// contained twice or the old parent claiming a child that has moved on.
    ///
    /// The write *order* still matters, because a change set cannot rule out a
    /// crash (see [`crate::change`]). It is therefore chosen so that the windows
    /// a crash could expose are all findings `check` already reports: repointing
    /// the child first leaves the old parent claiming a child that does not claim
    /// it back ([`Finding::MissingInverse`](crate::validate::Finding::MissingInverse));
    /// adding the new entry before removing the old leaves the child contained
    /// twice ([`Finding::DuplicateContainment`](crate::validate::Finding::DuplicateContainment)).
    /// Removing the old entry first would
    /// instead leave a child pointing up at a parent that has forgotten it —
    /// which, when this order was chosen, was the one inconsistency in the set
    /// that `check` did *not* look for. It now does
    /// ([`Finding::MissingContainment`](crate::validate::Finding::MissingContainment),
    /// for a child nothing reaches), so the ordering is belt-and-braces rather
    /// than the only thing standing between that window and silence. It stays as
    /// it is: the window is still the one worth making smallest.
    pub async fn reparent(&mut self, child: &Path, parent: &Path) -> Result<Reparented> {
        // The cycle check walks up from the new parent loading every rung, and
        // the parent — the first rung — is loaded again below for its title and
        // its spanning entry. A deep tree pays that walk once now.
        let _scope = self.read_scope();
        let child = link::normalize(child);
        let parent = link::normalize(parent);
        if child == parent {
            return Err(Error::Structure(format!(
                "{} cannot contain itself",
                parent.display()
            )));
        }
        let (spanning, inverse) = self.spanning_pair()?;
        for existing in [&child, &parent] {
            if !self.exists(existing).await? {
                return Err(Error::NotFound(existing.to_path_buf()));
            }
        }

        // Refuse a cycle: walking up from the *new* parent must not arrive at the
        // child. Reparenting a node beneath its own descendant would sever the
        // pair from the tree entirely — both would still claim each other, so
        // nothing would look broken from inside the loop, and a spanning walk from
        // the root would simply never reach them again.
        let mut rung = parent.clone();
        let mut seen = BTreeSet::new();
        while seen.insert(rung.clone()) {
            if rung == child {
                return Err(Error::Structure(format!(
                    "cannot reparent {} into {} — {} is contained by it, so the move would \
                     detach both from the tree",
                    child.display(),
                    parent.display(),
                    parent.display(),
                )));
            }
            let Ok((_, doc)) = self.load(&rung).await else {
                break;
            };
            match self.single_target(&doc, &inverse, &rung) {
                Some(up) => rung = up,
                None => break,
            }
        }

        let (child_text, child_doc) = self.load(&child).await?;
        let old_parent = self.single_target(&child_doc, &inverse, &child);
        // Up and down are asked separately, because they fail separately: a
        // child pointing here while the parent lists nothing is the half-linked
        // state this verb exists to finish, not a no-op to report success for.
        let already_up = old_parent.as_ref() == Some(&parent);

        let child_meta = fig::Value::from(&child_doc.meta);
        let child_title = child_meta
            .get("title")
            .and_then(fig::Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| link::path_to_title(&child));
        let (parent_text, parent_doc) = self.load(&parent).await?;
        let parent_meta = fig::Value::from(&parent_doc.meta);
        let parent_title = parent_meta
            .get("title")
            .and_then(fig::Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| link::path_to_title(&parent));
        let already_down =
            self.relations().children(&parent_meta).iter().any(|t| {
                self.resolve_link(&parent, &Link::parse(t)) == Target::Path(child.clone())
            });

        // Both directions hold. Idempotent like `adopt`, and for the same
        // reason: a caller re-running a script should not have to ask first.
        if already_up && already_down {
            return Ok(Reparented::Unchanged);
        }

        let mut cs = self.change();

        // 1. The child's inverse, repointed up at the new parent — unless it
        // already points here, in which case rewriting it would restyle a link
        // the caller did not ask about.
        if !already_up {
            let up = self
                .authored_target(&inverse, &child, &parent, &parent_title, true)
                .await?;
            let updated = prov_store::edit::set_in_text(
                &child_text,
                child_doc.carrier,
                &inverse,
                fig::Value::Str(up),
            )?;
            cs.write(&child, updated);
        }

        // 2. The new parent's spanning entry, appended (created if it had none).
        if !already_down {
            let down = self
                .authored_target(&spanning, &parent, &child, &child_title, true)
                .await?;
            let mut editor = MetaEditor::open_or_init(&parent_text, parent_doc.carrier)?;
            let span_path = [Segment::Key(&spanning)];
            if editor
                .append_value(&span_path, fig::Value::Str(down.clone()))
                .is_err()
            {
                editor.set_value(&span_path, fig::Value::Seq(vec![fig::Value::Str(down)]))?;
            }
            cs.write(&parent, editor.render()?);
        }

        // 3. The old parent's entry, removed last (see the ordering note above).
        // Read through the change set: when the old parent is a document some
        // earlier step already staged, that staged text is what must be edited,
        // not the stale copy on disk.
        //
        // A parent that is not on disk has no entry to remove: `single_target`
        // answers with a path, not with a promise that a file is at it, and a
        // registry entry keeps resolving long after its document is deleted. So
        // a missing old parent is skipped rather than raised — see the note on
        // the dangling parent above.
        if let Some(old) = &old_parent
            && old != &parent
        {
            match self.load_staged(&cs, old).await {
                Ok((old_text, old_doc)) => {
                    if let (Some(index), Some(carrier)) = (
                        self.entry_index(&old_doc, &spanning, old, &child),
                        old_doc.carrier,
                    ) {
                        let mut editor = MetaEditor::open(&old_text, carrier)?;
                        editor.remove_item(&[Segment::Key(&spanning)], index)?;
                        cs.write(old, editor.render()?);
                    }
                }
                Err(e) if is_missing(&e) => {}
                Err(e) => return Err(e),
            }
        }
        self.commit(cs).await?;
        Ok(match already_up {
            true => Reparented::Linked,
            false => Reparented::Moved,
        })
    }
}

/// Whether an error is "that document is not there" in either of the two shapes
/// a read can produce it: the typed guard the mutation verbs raise themselves,
/// and the raw `ENOENT` a storage backend returns for a path nothing wrote.
fn is_missing(error: &Error) -> bool {
    match error {
        Error::NotFound(_) => true,
        Error::Io(e) => e.kind() == std::io::ErrorKind::NotFound,
        _ => false,
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::super::support::*;
    use super::*;

    #[test]
    fn reparent_moves_a_node_in_the_tree_and_leaves_the_file_alone() {
        // The complement of `rename`: the document's *path* is untouched, only its
        // place in the tree changes. The old parent forgets it, the new one gains
        // it, and its inverse points somewhere new — three documents, one verb.
        let dir = tempdir("reparent");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- '[Jul](/jul.md)'\n- '[Aug](/aug.md)'\n---\n",
        );
        write(
            &dir,
            "jul.md",
            "---\ntitle: Jul\npart_of: '[Home](/index.md)'\ncontents:\n- '[Day](/day.md)'\n---\n",
        );
        write(
            &dir,
            "aug.md",
            "---\ntitle: Aug\npart_of: '[Home](/index.md)'\ncontents:\n---\n",
        );
        write(
            &dir,
            "day.md",
            "---\ntitle: Day\npart_of: '[Jul](/jul.md)'\n---\nProse survives.\n",
        );

        block_on(ws(&dir).reparent(Path::new("day.md"), Path::new("aug.md"))).unwrap();

        assert!(
            !read(&dir, "jul.md").contains("day.md"),
            "old parent forgot it: {}",
            read(&dir, "jul.md")
        );
        assert!(
            read(&dir, "aug.md").contains("day.md"),
            "new parent gained it: {}",
            read(&dir, "aug.md")
        );
        let day = read(&dir, "day.md");
        assert!(day.contains("/aug.md"), "inverse repointed: {day}");
        assert!(!day.contains("/jul.md"), "old inverse gone: {day}");
        assert!(day.contains("Prose survives."), "body untouched: {day}");
        // The file never moved — that is `mv`'s job, not this one's.
        assert!(dir.join("day.md").exists(), "the path is preserved");
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn reparent_is_idempotent_and_adopts_an_unparented_child() {
        // Re-running is a no-op (a script should not have to ask first), and a
        // child with no parent at all is accepted: there is simply nothing to
        // remove, so the effect is exactly `adopt`'s.
        let dir = tempdir("reparent-idem");
        write(&dir, "index.md", "---\ntitle: Home\n---\n");
        write(&dir, "loose.md", "---\ntitle: Loose\n---\n");

        let first =
            block_on(ws(&dir).reparent(Path::new("loose.md"), Path::new("index.md"))).unwrap();
        assert_eq!(first, Reparented::Moved);
        let once = read(&dir, "index.md");
        let again =
            block_on(ws(&dir).reparent(Path::new("loose.md"), Path::new("index.md"))).unwrap();
        assert_eq!(read(&dir, "index.md"), once, "second run changes nothing");
        assert_eq!(again, Reparented::Unchanged, "and says so");
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn reparent_finishes_a_child_that_already_claims_the_parent() {
        // The commonest orphan there is: the child declared `part_of` at its
        // index and the index never listed it back. Reparenting used to return
        // success here having written nothing at all, so a repair run could
        // report success on every document and leave every one of them
        // unreachable. The missing half is the whole job.
        let dir = tempdir("reparent-half-linked");
        write(&dir, "index.md", "---\ntitle: Home\n---\n");
        write(
            &dir,
            "loose.md",
            "---\ntitle: Loose\npart_of: '[Home](/index.md)'\n---\n",
        );

        let outcome =
            block_on(ws(&dir).reparent(Path::new("loose.md"), Path::new("index.md"))).unwrap();
        assert_eq!(outcome, Reparented::Linked, "it linked rather than moved");
        assert!(
            read(&dir, "index.md").contains("loose.md"),
            "the parent now lists the child it was already claimed by: {}",
            read(&dir, "index.md")
        );
        // The child's own claim was already right, so it was left untouched —
        // rewriting it would restyle a link nobody asked about.
        assert_eq!(
            read(&dir, "loose.md"),
            "---\ntitle: Loose\npart_of: '[Home](/index.md)'\n---\n"
        );
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn reparent_works_when_the_old_parent_is_no_longer_on_disk() {
        // A `part_of` naming a document that has been deleted out of band — the
        // registry keeps an `id:` reference resolving long after its file is
        // gone. There is no entry to remove, so there is nothing to fail at:
        // this is the state in which reparenting is most needed, and it used to
        // be the one state the verb refused to run in.
        let dir = tempdir("reparent-dangling");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- '[Kid](/kid.md)'\n---\n",
        );
        write(&dir, "kid.md", "---\ntitle: Kid\npart_of: /gone.md\n---\n");

        let outcome =
            block_on(ws(&dir).reparent(Path::new("kid.md"), Path::new("index.md"))).unwrap();
        assert_eq!(outcome, Reparented::Moved);
        let kid = read(&dir, "kid.md");
        assert!(
            kid.contains("/index.md"),
            "repointed at a live parent: {kid}"
        );
        assert!(!kid.contains("gone.md"), "the stale claim is gone: {kid}");
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn reparent_refuses_to_detach_a_subtree_under_its_own_descendant() {
        // Reparenting a node beneath something it contains would sever both from
        // the tree: they would still claim each other, so nothing looks broken
        // from inside the loop — a spanning walk would just never reach them
        // again. Refusing is the only way that stays visible.
        let dir = tempdir("reparent-cycle");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- '[A](/a.md)'\n---\n",
        );
        write(
            &dir,
            "a.md",
            "---\ntitle: A\npart_of: '[Home](/index.md)'\ncontents:\n- '[B](/b.md)'\n---\n",
        );
        write(&dir, "b.md", "---\ntitle: B\npart_of: '[A](/a.md)'\n---\n");

        let err = block_on(ws(&dir).reparent(Path::new("a.md"), Path::new("b.md"))).unwrap_err();
        assert!(
            err.to_string().contains("detach both from the tree"),
            "{err}"
        );
        // Refused means untouched, not half-done.
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn a_failed_reparent_leaves_the_old_containment_intact() {
        // Three documents change, and the middle window is the dangerous one: the
        // child repointed at its new parent while the old parent still claims it.
        let dir = tempdir("atomic-reparent");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- old.md\n- new.md\n---\n",
        );
        write(
            &dir,
            "old.md",
            "---\ntitle: Old\npart_of: index.md\ncontents:\n- kid.md\n---\n",
        );
        write(&dir, "new.md", "---\ntitle: New\npart_of: index.md\n---\n");
        write(&dir, "kid.md", "---\ntitle: Kid\npart_of: old.md\n---\n");
        let before = snapshot(&dir);

        // Write 0 repoints the kid, 1 adds the new parent's entry, 2 removes the
        // old parent's. Failing the last is the worst case — both the other two
        // have landed, and the kid is contained twice.
        let mut w = failing_ws(&dir, 2);
        let err = block_on(w.reparent(Path::new("kid.md"), Path::new("new.md"))).unwrap_err();
        assert!(err.to_string().contains("disk full"), "{err}");
        assert_eq!(
            snapshot(&dir),
            before,
            "a failed reparent left the kid contained twice"
        );
    }
}
