//! `delete` — the hard delete, and the only one that destroys bytes outright.
//!
//! The parent's spanning entry goes with the document; every *other* inbound
//! reference is reported rather than rewritten, because a link records intent
//! and there is no new target to send it to. [`recycle`](super::recycle) is the
//! recoverable counterpart, and the CLI's default.

use std::path::{Path, PathBuf};

use fig::Segment;

use crate::edit::MetaEditor;
use crate::error::{Error, Result};
use crate::fs::Storage;
use crate::graph::{LinkSite, Resolution};
use crate::identity::IdentityPolicy;
use crate::index::IndexStore;
use crate::link::{self, Link};
use crate::validate::Finding;
use crate::workspace::Workspace;

use super::maintain::content_target;

impl<FS: Storage, IdP: IdentityPolicy, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Delete the document at `path`, removing the parent's spanning entry for
    /// it. Refuses when the document has spanning children (they would be
    /// orphaned) unless `force` is set. A registered ID is retired — with a
    /// tombstoning store it is never reissued, so dangling references stay
    /// diagnosable.
    ///
    /// Also refuses the *body* half of a separated document unless forced — the
    /// prose file some other node reaches through its `content` pointer. Naming
    /// it is almost always a mistake for the node beside it: the node is what
    /// carries the id, the links and the title, and deleting its body leaves it
    /// pointing at nothing. The error names the node to delete instead. Under
    /// `force` the delete proceeds and the orphaned `content` pointer is
    /// reported like any other dangler. (Deleting the *node* takes its body with
    /// it, below — the pair is handled in both directions.)
    ///
    /// Returns the inbound references *left* dangling by the delete: every
    /// other document's overlay link or body wikilink that resolved to `path`
    /// (as [`Finding::BrokenLink`]), plus any `colophon:<id>` reference now
    /// pointing at the tombstone (as [`Finding::DanglingId`]), plus the
    /// `content` pointer of a node whose body was forced away. The parent's
    /// spanning entry is *not* reported — it is removed here — and a delete that
    /// nothing pointed at returns an empty list. Unlike `rename`, these are not
    /// rewritten: a link records intent, and there is no new target to send it
    /// to; the caller decides what to do with the diagnosis.
    pub async fn delete(&mut self, path: &Path, force: bool) -> Result<Vec<Finding>> {
        let path = link::normalize(path);
        let (spanning, inverse) = self.spanning_pair()?;
        let (_, doc) = self.load(&path).await?;

        let children: Vec<String> = self
            .relations()
            .children(&doc.meta)
            .iter()
            .map(|raw| Link::parse(raw).target)
            .collect();
        if !children.is_empty() && !force {
            return Err(Error::Structure(format!(
                "{} contains {} document(s) ({}); delete them first or force",
                path.display(),
                children.len(),
                children.join(", ")
            )));
        }

        let parent = self.single_target(&doc, &inverse, &path);
        let root = self.spanning_root(&path, &inverse).await?;

        // The body half of a separated pair: refuse, naming the node instead.
        let owner = self.content_owner(&path).await?;
        if let Some(owner) = &owner
            && !force
        {
            return Err(Error::Structure(format!(
                "{} is the body of {}; delete that instead, or force to destroy \
                 the body and leave {} pointing at nothing",
                path.display(),
                owner.display(),
                owner.display()
            )));
        }

        // Diagnose inbound references that will dangle: census the tree and keep
        // every link resolving to `path`, except the parent's spanning entry
        // (removed below) and any self-reference in the doomed document itself.
        let mut danglers: Vec<Finding> = self
            .census(&root)
            .await?
            .into_iter()
            .filter(|e| e.resolution.resolved_path() == Some(&path))
            .filter(|e| {
                e.source != path
                    && !(Some(&e.source) == parent.as_ref()
                        && matches!(&e.site, LinkSite::Relation(r) if *r == spanning))
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

        // A forced body delete strands its node's `content` pointer. The census
        // above cannot report it — `content` is not a relation and not a body
        // link — so it is added here rather than left for `check` to discover,
        // which is what makes this verb's promise ("returns the inbound
        // references left dangling") true of the separated shape too.
        if let Some(owner) = owner {
            let target = self
                .load(&owner)
                .await
                .ok()
                .and_then(|(_, doc)| doc.content_attr().map(str::to_string))
                .unwrap_or_else(|| path.to_string_lossy().into_owned());
            danglers.push(Finding::BrokenLink {
                doc: owner,
                site: LinkSite::Relation("content".to_string()),
                target,
            });
        }

        let mut parent_write: Option<(PathBuf, String)> = None;
        if let Some(parent) = &parent {
            let (parent_text, parent_doc) = self.load(parent).await?;
            if let (Some(index), Some(carrier)) = (
                self.entry_index(&parent_doc, &spanning, parent, &path),
                parent_doc.carrier,
            ) {
                let mut editor = MetaEditor::open(&parent_text, carrier)?;
                editor.remove_item(&[Segment::Key(&spanning)], index)?;
                parent_write = Some((parent.clone(), editor.render()?));
            }
        }

        // A separated node's body lives in a sibling file; delete the pair.
        let body_file = content_target(&doc, &path);
        let body_exists = match &body_file {
            Some(body) => self.exists(body).await?,
            None => false,
        };

        let mut cs = self.change();
        cs.remove(&path);
        if let (Some(body), true) = (&body_file, body_exists) {
            cs.remove(body);
        }
        if let Some((parent, text)) = parent_write {
            cs.write(parent, text);
        }

        // Identity hook — retire the ID (a tombstoning store keeps it known
        // forever, so it is never minted again to mean something else).
        if let Some(id) = self.index().id_for_path(&path) {
            self.index_mut().unregister(&id);
        }
        self.commit(cs).await?;
        Ok(danglers)
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::super::support::*;
    use super::*;
    use crate::document::Document;
    use crate::identity::Trigger;

    #[test]
    fn delete_refuses_children_then_forces() {
        let dir = tempdir("delete");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n- b.md\n---\n");
        write(
            &dir,
            "a.md",
            "---\npart_of: index.md\ncontents:\n- b.md\n---\n",
        );
        write(&dir, "b.md", "---\npart_of: index.md\n---\n");

        let err = block_on(ws(&dir).delete(Path::new("a.md"), false)).unwrap_err();
        assert!(err.to_string().contains("contains 1 document"), "{err}");

        block_on(ws(&dir).delete(Path::new("a.md"), true)).unwrap();
        assert!(!dir.join("a.md").exists());
        let index = read(&dir, "index.md");
        assert!(!index.contains("a.md"), "parent entry removed: {index}");
        assert!(index.contains("- b.md"), "sibling kept: {index}");
    }

    #[test]
    fn delete_diagnoses_inbound_references_left_dangling() {
        // A sibling links the doomed document two ways (overlay `links` + a body
        // wikilink). Delete removes the parent's spanning entry silently, but
        // reports the sibling's references it cannot rewrite — there is no new
        // target to send them to.
        let dir = tempdir("delete-inbound");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n- b.md\n---\n");
        write(&dir, "a.md", "---\npart_of: index.md\n---\n");
        write(
            &dir,
            "b.md",
            "---\npart_of: index.md\nlinks:\n- a.md\n---\nSee [[a.md]].\n",
        );

        let danglers = block_on(ws(&dir).delete(Path::new("a.md"), false)).unwrap();
        assert_eq!(danglers.len(), 2, "{danglers:?}");
        assert!(
            danglers.iter().any(|f| matches!(f,
                Finding::BrokenLink { doc, site: LinkSite::Relation(r), target }
                    if doc == &PathBuf::from("b.md") && r == "links" && target == "a.md")),
            "{danglers:?}"
        );
        assert!(
            danglers.iter().any(|f| matches!(f,
                Finding::BrokenLink { doc, site: LinkSite::Body(_), target }
                    if doc == &PathBuf::from("b.md") && target == "a.md")),
            "{danglers:?}"
        );
        // The parent's spanning entry was removed, not reported.
        assert!(
            !read(&dir, "index.md").contains("a.md"),
            "parent entry cleaned"
        );
    }

    #[test]
    fn delete_refuses_a_separated_body_and_names_its_node() {
        // The pair, handled in both directions. Deleting the *node* takes its
        // body with it (proven elsewhere in this file); naming the *body* is
        // almost always a mistake for the node beside it, so it is refused with
        // the node named — the node is what carries the id, the links and the
        // title, and it is what the user meant.
        //
        // Found by the generated sequences in `super::properties`, which reached
        // `separate` then `delete` on the resulting body in two operations.
        let dir = tempdir("delete-separated-body");
        write(&dir, "index.md", "---\ncontents:\n- b.yaml\n---\n");
        write(&dir, "b.yaml", "part_of: index.md\ncontent: b.md\n");
        write(&dir, "b.md", "B body.\n");

        let err = block_on(ws(&dir).delete(Path::new("b.md"), false)).unwrap_err();
        assert!(err.to_string().contains("is the body of b.yaml"), "{err}");
        assert!(dir.join("b.md").exists(), "and nothing was destroyed");

        // Forced, it proceeds — and says what it stranded, rather than leaving
        // `check` to discover it later.
        let danglers = block_on(ws(&dir).delete(Path::new("b.md"), true)).unwrap();
        assert!(!dir.join("b.md").exists(), "the body is gone");
        assert!(
            danglers.iter().any(|f| matches!(f,
                Finding::BrokenLink { doc, site: LinkSite::Relation(r), target }
                    if doc == &PathBuf::from("b.yaml") && r == "content" && target == "b.md")),
            "{danglers:?}"
        );
        // And what the verb reported is exactly what `check` goes on to find.
        let findings = block_on(ws(&dir).check("index.md")).unwrap();
        for reported in &danglers {
            assert!(
                findings.contains(reported),
                "{reported:?} not in {findings:?}"
            );
        }
    }

    #[test]
    fn delete_tombstones_and_check_diagnoses_the_dangler() {
        let dir = tempdir("id-delete");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n---\n");
        write(&dir, "a.md", "---\npart_of: index.md\n---\n");

        let mut w = id_ws(&dir);
        let id = block_on(w.register(Path::new("a.md"), Trigger::Link)).unwrap();
        let text = read(&dir, "index.md");
        let carrier = Document::parse("index.md", &text).unwrap().carrier;
        let updated = crate::edit::set_in_text(
            &text,
            carrier,
            "contents.0",
            fig::Value::Str(link::id_target(&id)),
        )
        .unwrap();
        std::fs::write(dir.join("index.md"), &updated).unwrap();

        block_on(w.delete(Path::new("a.md"), false)).unwrap();
        // Deleting removed the parent's entry too (matched through the registry
        // before the tombstone landed)… so re-add a dangling reference by hand
        // to simulate the out-of-band case.
        let text = read(&dir, "index.md");
        let carrier = Document::parse("index.md", &text).unwrap().carrier;
        let updated = crate::edit::set_in_text(
            &text,
            carrier,
            "contents",
            fig::Value::Str(link::id_target(&id)),
        )
        .unwrap();
        std::fs::write(dir.join("index.md"), &updated).unwrap();

        assert!(w.index().resolve(&id).is_none(), "tombstoned");
        assert!(w.index().is_known(&id), "but never forgotten");
        let findings = block_on(w.check("index.md")).unwrap();
        assert!(
            findings.iter().any(|f| matches!(
                f,
                crate::validate::Finding::DanglingId {
                    tombstoned: true,
                    ..
                }
            )),
            "{findings:?}"
        );
    }

    #[test]
    fn a_failed_delete_restores_the_document_it_removed() {
        let dir = linked_tree("atomic-delete");
        let before = snapshot(&dir);

        // `delete` removes the file, then rewrites the parent's entry. Failing
        // that write must bring the document back, not leave the parent pointing
        // at a hole.
        let mut w = failing_ws(&dir, 0);
        let err = block_on(w.delete(Path::new("a.md"), true)).unwrap_err();
        assert!(err.to_string().contains("disk full"), "{err}");
        assert_eq!(snapshot(&dir), before, "a failed delete lost a document");
    }
}
