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
use prov_store::edit::MetaEditor;
use prov_graph::error::{Error, Result};
use prov_store::fs::Storage;
use prov_graph::graph::Target;
use prov_store::index::IndexStore;
use prov_graph::link::{self, Link};
use prov_graph::meta::Value;

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
    /// instead leave a child pointing up at a parent that has forgotten it — the
    /// one inconsistency in this set that `check` does *not* look for, so it is
    /// deliberately the last write rather than the first.
    pub async fn reparent(&mut self, child: &Path, parent: &Path) -> Result<()> {
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
        if old_parent.as_ref() == Some(&parent) {
            // Already there. Idempotent like `adopt`, and for the same reason: a
            // caller re-running a script should not have to ask first.
            return Ok(());
        }

        let child_title = child_doc
            .meta
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| link::path_to_title(&child));
        let (parent_text, parent_doc) = self.load(&parent).await?;
        let parent_title = parent_doc
            .meta
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| link::path_to_title(&parent));

        let mut cs = self.change();

        // 1. The child's inverse, repointed up at the new parent.
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

        // 2. The new parent's spanning entry, appended (created if it had none).
        let already_down =
            self.relations().children(&parent_doc.meta).iter().any(|t| {
                self.resolve_link(&parent, &Link::parse(t)) == Target::Path(child.clone())
            });
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
        if let Some(old) = &old_parent
            && old != &parent
        {
            let (old_text, old_doc) = self.load_staged(&cs, old).await?;
            if let (Some(index), Some(carrier)) = (
                self.entry_index(&old_doc, &spanning, old, &child),
                old_doc.carrier,
            ) {
                let mut editor = MetaEditor::open(&old_text, carrier)?;
                editor.remove_item(&[Segment::Key(&spanning)], index)?;
                cs.write(old, editor.render()?);
            }
        }
        self.commit(cs).await
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

        block_on(ws(&dir).reparent(Path::new("loose.md"), Path::new("index.md"))).unwrap();
        let once = read(&dir, "index.md");
        block_on(ws(&dir).reparent(Path::new("loose.md"), Path::new("index.md"))).unwrap();
        assert_eq!(read(&dir, "index.md"), once, "second run changes nothing");
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
