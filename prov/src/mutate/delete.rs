//! `delete` — the one verb that destroys a document, and what it writes down.
//!
//! The parent's spanning entry goes with the document; every *other* inbound
//! reference is reported rather than rewritten, because a link records intent
//! and there is no new target to send it to.
//!
//! prov does not keep the bytes. Recovering those belongs to whatever tool the
//! workspace is recorded under; what the delete leaves behind is the record of
//! what it would take to put the *graph* back around them, in the workspace's
//! deletion log — see [`tombstone`](super::tombstone), and
//! [`restore`](crate::Workspace::restore), its inverse.

use std::path::{Path, PathBuf};

use fig::Segment;

use crate::identity::IdentityPolicy;
use crate::validate::Finding;
use crate::workspace::Workspace;
use prov_graph::error::{Error, Result};
use prov_graph::link::{self, Link};
use prov_store::edit::MetaEditor;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

use super::maintain::paired_file;
use super::tombstone::Deletion;

/// Whether a removal should work out what it breaks.
///
/// [`delete`](Workspace::delete) returns the inbound references it leaves
/// dangling, and finding them is a census of the whole reachable graph — the
/// removal's entire cost on any workspace bigger than a few hundred documents.
/// This is how a caller says it does not want the answer.
///
/// Not a judgment about which is right. A person at a terminal who has just
/// deleted something wants to be told what now points at nothing; a GUI that
/// deletes on a click and shows the user a sidebar has been building that list
/// and dropping it. `Report` stays the default, so the verb that says it
/// diagnoses still does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Diagnosis {
    /// Census the reachable graph and report every inbound reference the
    /// removal leaves dangling. The default, and what the bare verbs do.
    #[default]
    Report,
    /// Remove without looking. The verb returns no findings and reads only the
    /// documents it edits. Nothing is lost but the report: `check` raises the
    /// same dangling references whenever it is next run.
    Skip,
}

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
    ///
    /// That diagnosis is a whole-workspace census, and it is most of what a
    /// delete costs — see [`delete_with`](Self::delete_with) for the caller that
    /// does not want it, or that wants to date the record.
    ///
    /// # What it writes down
    ///
    /// Unless the workspace turns it off
    /// ([`record_deletions`](Self::record_deletions)), the delete records what
    /// it destroyed in the workspace's **deletion log** — the path, the title,
    /// the id it retired, the parent whose entry it removed, and the body that
    /// went with it. That is what [`restore`](Self::restore) repairs the graph
    /// from once the bytes are back, and it is the half no version-control tool
    /// has. The record lands in the same [`ChangeSet`](crate::ChangeSet) as the
    /// removal, so a delete is never recorded without happening or the reverse.
    pub async fn delete(&mut self, path: &Path, force: bool) -> Result<Vec<Finding>> {
        self.delete_with(path, force, None, Diagnosis::Report).await
    }

    /// [`delete`](Self::delete), told whether to work out what it breaks, and
    /// when the deletion happened.
    ///
    /// `Diagnosis::Report` is exactly `delete`. [`Diagnosis::Skip`] returns an
    /// empty list and skips the census behind it — the difference between
    /// reading every document in the workspace and reading the handful this
    /// edits. Everything else about the delete is unchanged: the same refusals,
    /// the same parent edit, the same id tombstone, the same deletion record,
    /// the same change set.
    ///
    /// `at` is an optional caller-supplied deletion timestamp for that record
    /// (the CLI passes the current time). The library takes it as an argument
    /// rather than reading a clock, so the op stays deterministic.
    pub async fn delete_with(
        &mut self,
        path: &Path,
        force: bool,
        at: Option<&str>,
        diagnosis: Diagnosis,
    ) -> Result<Vec<Finding>> {
        // The dangler census reads every reachable document, and the subject,
        // its parent and the root are all among them — each of which this verb
        // then goes back for by name. One scope makes that one read apiece
        // instead of two.
        let _scope = self.read_scope();
        let path = link::normalize(path);
        let (spanning, inverse) = self.spanning_pair()?;
        let (_, doc) = self.load(&path).await?;

        let children: Vec<String> = self
            .relations()
            .children(&fig::Value::from(&doc.meta))
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

        // What the delete breaks — the census, the walk up to the root it is
        // anchored to, and the forced-body case the census cannot see. All of it
        // behind `diagnosis`, so a caller that ignores the answer pays nothing
        // for it.
        let danglers = self
            .removal_danglers(diagnosis, &path, parent.as_deref(), owner.as_deref())
            .await?;

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

        // The file that travels with the node — a separated body, an attachment
        // payload, or a manifest — is deleted with it.
        //
        // The directory a manifest *covers* is not touched. A body is the node's
        // content and goes where it goes; a manifest is a description of files
        // that exist on their own terms, so deleting the description must not
        // delete ten thousand photographs. What is left behind is an uncovered
        // directory, which is exactly what it was before anything described it.
        let body_file = paired_file(&doc, &path);
        let body_exists = match &body_file {
            Some(body) => self.exists(body).await?,
            None => false,
        };

        let id = self.index().id_for_path(&path);

        let mut cs = self.change();
        cs.remove(&path);
        if let (Some(body), true) = (&body_file, body_exists) {
            cs.remove(body);
        }

        // What this destroyed, written down — in the same change set, so the
        // record and the removal cannot come apart.
        if self.record_deletions() {
            let deletion = Deletion {
                title: doc
                    .meta
                    .get("title")
                    .and_then(prov_graph::meta::Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| link::path_to_title(&path)),
                id: id.clone(),
                from: path.clone(),
                parent: parent.clone(),
                body: body_exists.then(|| body_file.clone()).flatten(),
                at: at.map(str::to_owned),
            };
            let root = self.spanning_root(&path, &inverse).await?;
            // The root is very often the deleted document's parent, and the
            // pointer to a just-created log has to go into the *same* rendering
            // as the parent edit — two writes to one path in one change set is
            // one of them silently winning.
            let root_base = parent_write
                .as_ref()
                .filter(|(parent, _)| *parent == root)
                .map(|(_, text)| text.clone());
            if let Some(text) = self
                .stage_deletion(&mut cs, &root, &deletion, root_base)
                .await?
            {
                match &mut parent_write {
                    Some((parent, parent_text)) if *parent == root => *parent_text = text,
                    _ => {
                        cs.write(root.clone(), text);
                    }
                }
            }
        }

        if let Some((parent, text)) = parent_write {
            cs.write(parent, text);
        }

        // Identity hook — retire the ID (a tombstoning store keeps it known
        // forever, so it is never minted again to mean something else). The
        // record above keeps its value, so `restore` can re-register it.
        if let Some(id) = id {
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
    use crate::identity::Trigger;
    use prov_graph::document::Document;
    use prov_graph::graph::LinkSite;
    use prov_graph::index::IdIndex;

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

    /// The diagnosis is the delete's whole cost on a large workspace, and a
    /// caller that ignores it should not pay for it. `sub/bystander.md` is the
    /// witness: reachable, so a census reads it, and untouched by the delete, so
    /// nothing else has a reason to.
    ///
    /// It sits in a subdirectory deliberately. What `Skip` does *not* remove is
    /// the separated-body guard, and [`content_owner`] is one `read_dir` of the
    /// subject's own directory — so a document beside `a.md` is read either way,
    /// and only a document elsewhere separates "reads what it edits" from "reads
    /// the workspace".
    ///
    /// [`content_owner`]: Workspace::content_owner
    #[test]
    fn a_skipped_diagnosis_reads_nothing_it_does_not_edit() {
        let dir = tempdir("delete-skip");
        write(
            &dir,
            "index.md",
            "---\ncontents:\n- a.md\n- sub/linker.md\n- sub/bystander.md\n---\n",
        );
        write(&dir, "a.md", "---\npart_of: index.md\n---\n");
        write(
            &dir,
            "sub/linker.md",
            "---\npart_of: /index.md\nlinks:\n- /a.md\n---\n",
        );
        write(&dir, "sub/bystander.md", "---\npart_of: /index.md\n---\n");

        let fs = crate::fs_faults::CountingFs::default();
        let mut workspace = Workspace::builder(fs.clone()).root(&dir).build();
        let danglers =
            block_on(workspace.delete_with(Path::new("a.md"), false, None, Diagnosis::Skip))
                .unwrap();

        assert!(danglers.is_empty(), "{danglers:?}");
        // The delete itself happened, in full.
        assert!(!dir.join("a.md").exists());
        assert!(
            !read(&dir, "index.md").contains("a.md"),
            "parent entry still removed without the diagnosis"
        );
        assert_eq!(
            fs.doc_reads(&dir, "sub/bystander.md"),
            0,
            "a skipped diagnosis still read the workspace"
        );
        // `sub/linker.md` holds the reference that would have been reported. Not
        // read either: `Skip` does not look for it, and a delete rewrites nothing.
        assert_eq!(fs.doc_reads(&dir, "sub/linker.md"), 0);
    }

    /// The same delete, diagnosed: what `Skip` costs is exactly the finding, and
    /// the reads behind it.
    #[test]
    fn the_same_delete_diagnosed_reports_it_and_reads_the_workspace() {
        let dir = tempdir("delete-report");
        write(
            &dir,
            "index.md",
            "---\ncontents:\n- a.md\n- sub/linker.md\n---\n",
        );
        write(&dir, "a.md", "---\npart_of: index.md\n---\n");
        write(
            &dir,
            "sub/linker.md",
            "---\npart_of: /index.md\nlinks:\n- /a.md\n---\n",
        );

        let fs = crate::fs_faults::CountingFs::default();
        let mut workspace = Workspace::builder(fs.clone()).root(&dir).build();
        let danglers =
            block_on(workspace.delete_with(Path::new("a.md"), false, None, Diagnosis::Report))
                .unwrap();

        assert_eq!(danglers.len(), 1, "{danglers:?}");
        assert_eq!(
            fs.doc_reads(&dir, "sub/linker.md"),
            1,
            "the census reads every reachable document — once, inside the scope"
        );
    }

    /// The separated-body guard is one `read_dir` of the subject's own
    /// directory, and it used to open every document in it. A vault keeping a
    /// month of daily notes in one folder therefore paid nine document reads to
    /// delete the tenth — more than everything else an undiagnosed delete does,
    /// put together. Only a whole-file metadata neighbour can own a body, and
    /// that is a question about the path.
    #[test]
    fn the_body_guard_does_not_open_markdown_neighbours() {
        let dir = tempdir("delete-neighbours");
        write(
            &dir,
            "index.md",
            "---\ncontents:\n- day/a.md\n- day/b.md\n- day/c.md\n---\n",
        );
        for name in ["a", "b", "c"] {
            write(
                &dir,
                &format!("day/{name}.md"),
                "---\npart_of: /index.md\n---\n",
            );
        }

        let fs = crate::fs_faults::CountingFs::default();
        let mut workspace = Workspace::builder(fs.clone()).root(&dir).build();
        block_on(workspace.delete_with(Path::new("day/a.md"), false, None, Diagnosis::Skip))
            .unwrap();

        assert!(!dir.join("day/a.md").exists());
        for bystander in ["day/b.md", "day/c.md"] {
            assert_eq!(
                fs.doc_reads(&dir, bystander),
                0,
                "{bystander} was opened to ask whether it owns a body it cannot own"
            );
        }
    }

    /// The shape the cheaper guard must *not* give up. `combine` tolerates a
    /// stray frontmatter on the prose half of a separated pair, so a body can
    /// look like an ordinary document — and the guard still has to catch it. It
    /// does, because what the path test asks about is the *neighbour's*
    /// extension, never the subject's.
    #[test]
    fn the_body_guard_still_catches_a_body_that_carries_frontmatter() {
        let dir = tempdir("delete-stray-frontmatter");
        write(&dir, "index.md", "---\ncontents:\n- day/n.yaml\n---\n");
        write(&dir, "day/n.yaml", "part_of: /index.md\ncontent: body.md\n");
        write(&dir, "day/body.md", "---\ntitle: Stray\n---\nB body.\n");
        // A markdown neighbour, to prove the sweep is still reaching the right
        // file rather than simply refusing everything in a mixed directory.
        write(&dir, "day/other.md", "---\npart_of: /index.md\n---\n");

        let err = block_on(ws(&dir).delete(Path::new("day/body.md"), false)).unwrap_err();
        assert!(
            err.to_string().contains("is the body of day/n.yaml"),
            "{err}"
        );
        assert!(
            dir.join("day/body.md").exists(),
            "and nothing was destroyed"
        );
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
        let updated = prov_store::edit::set_in_text(
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
        let updated = prov_store::edit::set_in_text(
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
