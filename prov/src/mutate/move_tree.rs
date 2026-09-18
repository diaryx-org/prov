//! `move_tree` — a directory moves, and every document under it is a mover.
//!
//! [`rename`](super::rename) is one document. A *book* — a folder note and the
//! directory beneath it, its chapters, its `attachments/` — has no verb of its
//! own without this one, so a consumer moved it as N renames: N censuses, N
//! change sets, and between any two of them a workspace that was consistent
//! but half-moved. Worse, each rename respelled the moved page's images for
//! where the payload *was*, and the sidecar's own move later carried the
//! payload away from under every one of them.
//!
//! Here the whole directory is one change set: the inbound half is one census
//! folded over every path under it, each document's own links are
//! re-relativized with its fellow movers' new places already known, files
//! nothing describes move as bytes, the registry follows every id, and it all
//! lands or none of it does.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use fig::Segment;

use crate::identity::IdentityPolicy;
use crate::workspace::Workspace;
use prov_graph::error::{Error, Result};
use prov_graph::link;
use prov_store::edit::MetaEditor;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

use super::maintain::{Movers, Moves, Rewrite, manifest_target};
use super::rename::{rerelativize, rerelativize_body_links};

impl<FS: Storage, IdP: IdentityPolicy, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Move the directory at `from_dir` to `to_dir`, everything under it
    /// keeping its place within it, maintaining every affected link across the
    /// workspace as one change set.
    ///
    /// Every document under `from_dir` is a mover, on [`rename`](Self::rename)'s
    /// terms: each inbound reference that resolves to it by a path is
    /// retargeted, and every relative link it declares itself — frontmatter
    /// and body, links and images — is recomputed for its new directory, with
    /// a target that is *also* moving spelled for where it lands. A body
    /// reference to an attachment's payload counts as a reference to the
    /// payload, so a page that embeds `attachments/photo.jpg` still shows it
    /// wherever the page and the picture end up, together or apart. A file
    /// prov does not read — a payload, a hidden entry, whatever sits in a
    /// directory the workspace declares out of scope — moves as bytes.
    ///
    /// The set is *N* file renames in one journal rather than one directory
    /// rename, because everything that follows a move — the registry finding
    /// its host, the read memo forgetting what moved, a pending id stamp
    /// finding its document — follows a file. The directory `from_dir` itself
    /// is removed once the set has landed, if the move emptied it; a directory
    /// left behind is litter, not a torn tree, and is never a reason to fail.
    ///
    /// A `content` or `manifest` pointer is a basename by construction
    /// ([`body_sibling`](super::maintain::body_sibling),
    /// [`manifest_sibling`](prov_graph::manifest::manifest_sibling)), so a
    /// node's body or store moves with it and the pointer stays true. A
    /// manifest's `root` is respelled when the store and the directory it
    /// covers part company — one inside the move, one outside — and the node's
    /// pin re-stamped, as `rename` does for a lone manifest node.
    ///
    /// Refused, before anything is touched: the workspace root; a `from_dir`
    /// that is not a directory (`rename` moves a document); a `to_dir` that
    /// exists (a merge is not a move) or lies under `from_dir`; and a mover
    /// whose id the registry already binds to its destination.
    pub async fn move_tree(&mut self, from_dir: &Path, to_dir: &Path) -> Result<()> {
        // One walk of the directory, one census, and a load of every document
        // either turns up; the scope makes each document one read.
        let _scope = self.read_scope();
        let from_dir = link::normalize(from_dir);
        let to_dir = link::normalize(to_dir);

        if from_dir.as_os_str().is_empty() {
            return Err(Error::Structure(
                "the workspace root is not a directory a move can take".into(),
            ));
        }
        if !self.exists(&from_dir).await? {
            return Err(Error::NotFound(from_dir));
        }
        if !self.stat(&from_dir).await?.is_dir() {
            return Err(Error::Structure(format!(
                "{} is a document, not a directory — `rename` moves one",
                from_dir.display()
            )));
        }
        if to_dir.as_os_str().is_empty() || to_dir.starts_with(&from_dir) {
            return Err(Error::Structure(format!(
                "{} cannot move to {}: the destination is inside it",
                from_dir.display(),
                to_dir.display()
            )));
        }
        if self.exists(&to_dir).await? {
            return Err(Error::AlreadyExists(to_dir));
        }
        let moves = Moves::Tree {
            from: from_dir.clone(),
            to: to_dir.clone(),
        };

        // Everything under the directory, hidden entries included: all of it
        // moves. Which of it prov also *reads* — and so maintains — is decided
        // per file below, on the same terms every walk draws: not hidden, not
        // parked, not an opaque payload.
        let files = self.files_under(&from_dir).await?;
        let root = self.root_document().await?;
        let parked = match &root {
            Some(root) => self.parked_dirs(root).await?,
            None => self.out_of_scope().to_vec(),
        };
        let maintained = |path: &Path| {
            let below = path.strip_prefix(&from_dir).unwrap_or(path);
            !below
                .iter()
                .any(|part| part.to_str().is_some_and(|p| p.starts_with('.')))
                && !parked.iter().any(|dir| path.starts_with(dir))
                && !prov_graph::document::is_opaque_payload(path)
        };

        // The registry is asked before any rewrite is computed, as `rename`
        // asks: a destination the registry binds to a *different* id is a tear
        // `set_path` cannot see, and refusing costs nothing yet.
        let mut ids = Vec::new();
        for path in &files {
            if let Some(id) = self.index().id_for_path(path) {
                let to = moves.landed_or_same(path);
                if let Some(conflict) = self.move_conflict(&id, &to) {
                    return Err(conflict.into());
                }
                ids.push((id, to));
            }
        }

        // 1. The inbound half: every document outside the directory that
        //    reaches something inside it, rewritten once for everything it
        //    reaches. A mover is handed nothing here — its own pass respells
        //    every link it holds, fellow movers included. The census is
        //    anchored at the workspace root rather than at a document under
        //    `from_dir`, since a directory of payloads may hold none.
        let anchor = root.clone().unwrap_or_else(|| from_dir.clone());
        let inbound = self
            .collect_inbound_rewrites(&anchor, &moves, Movers::Skip)
            .await?;

        // 2. Each mover's own links. Every rewrite is planned here, keyed by
        //    the path it was read from, and staged only once all are computed:
        //    a manifest's store and its node are two files one fact spans, and
        //    planning them into one map is what keeps the pass that respells
        //    `root` from staging a second write over the pass that
        //    re-relativized the same file's links.
        let mut planned: BTreeMap<PathBuf, Rewrite> = BTreeMap::new();
        let mut manifests = Vec::new();
        for path in &files {
            if !maintained(path) {
                continue;
            }
            // A file prov reads but cannot parse moves as bytes — an unreadable
            // document is a finding `check` already raises, not a reason to
            // refuse the move of everything around it.
            let Ok((text, doc)) = self.load(path).await else {
                continue;
            };
            let to = moves.landed_or_same(path);
            let rewritten = rerelativize(
                &text,
                &doc,
                &self.frontmatter_links(&fig::Value::from(&doc.meta)),
                path,
                &to,
                &moves,
                |site| self.site_path_style(site),
            )?;
            let rewritten = rerelativize_body_links(
                &rewritten,
                &doc.body,
                path,
                &to,
                &moves,
                self.link_style(),
            );
            if let Some(store) = manifest_target(&doc, path) {
                manifests.push((path.clone(), store, doc.meta.get("content_hash").is_some()));
            }
            if rewritten != text {
                planned.insert(
                    path.clone(),
                    Rewrite {
                        read: text,
                        text: rewritten,
                    },
                );
            }
        }
        for (node, store, pinned) in manifests {
            self.plan_manifest_root(&node, &store, pinned, &moves, &mut planned)
                .await?;
        }

        // 3. Stage: every file's move, then every rewrite over its landing
        //    place — with each rewrite's pre-image as the set's expectation, so
        //    a document another writer edited in the compute→apply gap refuses
        //    the move ([`Error::Drifted`]) rather than having its racer's text
        //    moved and overwritten by a rewrite of older bytes.
        let mut cs = self.change();
        for path in &files {
            let to = moves.landed_or_same(path);
            cs.expect_absent(&to);
            cs.rename(path, &to);
        }
        for (path, rw) in planned {
            cs.expect(&path, rw.read);
            cs.write(moves.landed_or_same(&path), rw.text);
        }
        for (source, rw) in inbound {
            cs.expect(&source, rw.read);
            cs.write(source, rw.text);
        }
        // The registry follows every id, in the same set as the documents.
        for (id, to) in ids {
            self.index_mut().set_path(&id, &to);
        }
        self.commit(cs).await?;

        // The emptied directory. Outside the set, and deliberately so: the set
        // knows files, and a directory that stays behind is litter rather than
        // a torn tree. Removed only when the move left nothing in it — a file
        // that appeared in the meantime is somebody's, and stays.
        if self.is_empty_tree(&from_dir).await? {
            let _ = self.fs().remove_dir_all(&self.fs_path(&from_dir)).await;
        }
        Ok(())
    }

    /// Respell a manifest's `root` for where its store and the directory it
    /// covers each land, and re-stamp the node's pin over the new bytes —
    /// planned into `planned` over whatever the own-links pass already holds
    /// for either file. Nothing when the spelling comes out the same, which is
    /// the ordinary case: store and archive move together, and a relative
    /// path between two things that moved together is the path it was.
    async fn plan_manifest_root(
        &self,
        node: &Path,
        store: &Path,
        pinned: bool,
        moves: &Moves,
        planned: &mut BTreeMap<PathBuf, Rewrite>,
    ) -> Result<()> {
        let (raw, parsed) = self.load(store).await?;
        let Ok(manifest) = prov_graph::manifest::Manifest::from_meta(&parsed.meta) else {
            return Ok(()); // malformed: `check`'s to report, not a move's to touch
        };
        let covered = moves.landed_or_same(&manifest.covered_root(store));
        let store_to = moves.landed_or_same(store);
        let root = format!(
            "{}/",
            link::relative(store_to.parent().unwrap_or(Path::new("")), &covered)
        );
        let base = planned
            .get(store)
            .map_or(raw.as_str(), |rw| rw.text.as_str());
        let respelled = prov_store::edit::set_in_text(
            base,
            parsed.carrier,
            prov_graph::manifest::ROOT_KEY,
            fig::Value::Str(root),
        )?;
        if respelled == base {
            return Ok(());
        }
        planned.insert(
            store.to_path_buf(),
            Rewrite {
                read: raw,
                text: respelled.clone(),
            },
        );
        if !pinned {
            return Ok(());
        }
        // The node pins the store's bytes, and those just changed — re-stamped
        // in the same set, so the move cannot author a fixity alarm.
        let (node_raw, node_doc) = self.load(node).await?;
        let Some(carrier) = node_doc.carrier else {
            return Ok(());
        };
        let node_base = planned
            .get(node)
            .map_or(node_raw.clone(), |rw| rw.text.clone());
        let mut editor = MetaEditor::open(&node_base, carrier)?;
        editor.set_value(
            &[Segment::Key("content_hash")],
            fig::Value::Str(crate::fixity::digest(respelled.as_bytes())),
        )?;
        planned.insert(
            node.to_path_buf(),
            Rewrite {
                read: node_raw,
                text: editor.render()?,
            },
        );
        Ok(())
    }

    /// Every file under `dir`, at any depth, hidden entries included — the
    /// population a directory move carries. Sorted, so the set stages the same
    /// way every run. A symbolic link is a file here: the rename moves the
    /// link, never what it points at.
    async fn files_under(&self, dir: &Path) -> Result<BTreeSet<PathBuf>> {
        let mut files = BTreeSet::new();
        let mut queue = vec![dir.to_path_buf()];
        while let Some(dir) = queue.pop() {
            for entry in self.listing(&dir).await? {
                let Some(name) = entry.file_name().map(|n| n.to_owned()) else {
                    continue;
                };
                let path = dir.join(name);
                if entry.file_type().is_dir() {
                    queue.push(path);
                } else {
                    files.insert(path);
                }
            }
        }
        Ok(files)
    }

    /// Whether nothing but directories remains under `dir`.
    async fn is_empty_tree(&self, dir: &Path) -> Result<bool> {
        Ok(self.files_under(dir).await?.is_empty())
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::super::support::*;
    use super::*;
    use crate::identity::Trigger;
    use prov_graph::index::IdIndex;
    use prov_graph::link::LinkStyle;

    /// The task's repro: a book `a/` — a folder note, a chapter, and a picture
    /// both pages embed — beside a second book `b/`.
    fn book(tag: &str) -> PathBuf {
        let dir = tempdir(tag);
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- a/a.md\n- b/b.md\n---\n",
        );
        write(
            &dir,
            "a/a.md",
            "---\ntitle: A\npart_of: /index.md\ncontents:\n- chapter.md\n- attachments/photo.jpg.yaml\n---\n\
             ![](attachments/photo.jpg)\n\nSee [the chapter](chapter.md) and [B](/b/b.md).\n",
        );
        write(
            &dir,
            "a/chapter.md",
            "---\ntitle: Chapter\npart_of: a.md\n---\n![A photo](attachments/photo.jpg)\n",
        );
        std::fs::create_dir_all(dir.join("a/attachments")).unwrap();
        std::fs::write(dir.join("a/attachments/photo.jpg"), b"\xff\xd8jpeg").unwrap();
        write(
            &dir,
            "a/attachments/photo.jpg.yaml",
            "title: Photo\npart_of: ../a.md\ncontent: photo.jpg\n",
        );
        write(
            &dir,
            "b/b.md",
            "---\ntitle: B\npart_of: /index.md\n---\nA note on [A](/a/a.md) with ![its photo](/a/attachments/photo.jpg).\n",
        );
        dir
    }

    #[test]
    fn a_book_moves_by_one_call_with_every_image_resolving() {
        // docs/tasks/a-directory-moves-one-document-at-a-time.md. Moved as N
        // renames, steps 1 and 2 respelled each page's image for where the
        // payload was, and step 3 carried the payload away from under both.
        // Moved as one, every page still has its picture, spelled — like
        // every other link a mover holds — in the workspace's path style,
        // root-absolute by default; the relative-style case is beside this.
        let dir = book("move-tree-book");
        block_on(ws(&dir).move_tree(Path::new("a"), Path::new("b/a"))).unwrap();

        assert!(!dir.join("a").exists(), "the emptied directory is gone");
        assert!(dir.join("b/a/attachments/photo.jpg").is_file());

        let a = read(&dir, "b/a/a.md");
        assert!(a.contains("![](/b/a/attachments/photo.jpg)"), "{a}");
        assert!(a.contains("[the chapter](/b/a/chapter.md)"), "{a}");
        assert!(a.contains("[B](/b/b.md)"), "{a}");
        assert!(a.contains("part_of: /index.md"), "{a}");
        assert!(a.contains("- /b/a/chapter.md"), "{a}");
        assert!(a.contains("- /b/a/attachments/photo.jpg.yaml"), "{a}");

        let chapter = read(&dir, "b/a/chapter.md");
        assert!(
            chapter.contains("![A photo](/b/a/attachments/photo.jpg)"),
            "{chapter}"
        );
        assert!(chapter.contains("part_of: /b/a/a.md"), "{chapter}");

        // The sidecar's `content` is a basename, not a link: it is not
        // respelled, and the payload beside it is where it says.
        let sidecar = read(&dir, "b/a/attachments/photo.jpg.yaml");
        assert!(sidecar.contains("part_of: /b/a/a.md"), "{sidecar}");
        assert!(sidecar.contains("content: photo.jpg"), "{sidecar}");

        // Outside the book: the parent's entry and the other book's link and
        // image all follow.
        let index = read(&dir, "index.md");
        assert!(index.contains("- /b/a/a.md"), "{index}");
        let b = read(&dir, "b/b.md");
        assert!(b.contains("[A](/b/a/a.md)"), "{b}");
        assert!(
            b.contains("![its photo](/b/a/attachments/photo.jpg)"),
            "{b}"
        );

        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn rename_of_a_directory_is_a_tree_move() {
        // `mv a b/a` on a directory does the directory thing, so a CLI or a
        // consumer that already calls `rename` gets the verb without asking.
        let dir = book("move-tree-via-rename");
        block_on(ws(&dir).rename(Path::new("a"), Path::new("b/a"))).unwrap();
        assert!(read(&dir, "b/b.md").contains("![its photo](/b/a/attachments/photo.jpg)"));
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn a_tree_move_respects_a_relative_path_style() {
        // Under `../`-relative links, a mover's link to something outside the
        // directory is respelled from its new depth, and a link between two
        // movers stays what it was.
        let dir = book("move-tree-relative");
        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .link_style(LinkStyle::MarkdownRelative)
            .build();
        block_on(w.move_tree(Path::new("a"), Path::new("b/a"))).unwrap();

        let a = read(&dir, "b/a/a.md");
        assert!(a.contains("part_of: ../../index.md"), "{a}");
        assert!(a.contains("[B](../b.md)"), "{a}");
        assert!(a.contains("![](attachments/photo.jpg)"), "{a}");
        let b = read(&dir, "b/b.md");
        assert!(b.contains("![its photo](a/attachments/photo.jpg)"), "{b}");
        assert!(b.contains("[A](a/a.md)"), "{b}");
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn loose_files_move_as_bytes_and_ids_follow() {
        // A file nothing describes, a hidden file, and a registered document:
        // the first two travel untouched, the third keeps its id at its new
        // path.
        let dir = book("move-tree-bytes-and-ids");
        std::fs::write(dir.join("a/notes.txt"), "plain\n").unwrap();
        std::fs::write(dir.join("a/.hidden"), "secret\n").unwrap();
        let mut w = id_ws(&dir);
        let id = block_on(w.register(Path::new("a/chapter.md"), Trigger::Link)).unwrap();

        block_on(w.move_tree(Path::new("a"), Path::new("b/a"))).unwrap();

        assert_eq!(read(&dir, "b/a/notes.txt"), "plain\n");
        assert_eq!(read(&dir, "b/a/.hidden"), "secret\n");
        assert_eq!(
            std::fs::read(dir.join("b/a/attachments/photo.jpg")).unwrap(),
            b"\xff\xd8jpeg"
        );
        assert_eq!(
            w.index().resolve(&id),
            Some(PathBuf::from("b/a/chapter.md"))
        );
        assert!(!dir.join("a").exists());
    }

    #[test]
    fn a_tree_move_refuses_the_root_a_document_an_occupied_or_a_nested_destination() {
        let dir = book("move-tree-refusals");
        let before = snapshot(&dir);
        let mut w = ws(&dir);
        assert!(block_on(w.move_tree(Path::new(""), Path::new("x"))).is_err());
        assert!(block_on(w.move_tree(Path::new("a/a.md"), Path::new("x"))).is_err());
        assert!(matches!(
            block_on(w.move_tree(Path::new("a"), Path::new("b"))),
            Err(Error::AlreadyExists(_))
        ));
        assert!(block_on(w.move_tree(Path::new("a"), Path::new("a/inner"))).is_err());
        assert!(matches!(
            block_on(w.move_tree(Path::new("missing"), Path::new("x"))),
            Err(Error::NotFound(_))
        ));
        assert_eq!(snapshot(&dir), before, "a refusal touches nothing");
    }

    #[test]
    fn a_failed_tree_move_leaves_the_workspace_exactly_as_it_was() {
        // Every write past the first fails: N renames and several rewrites,
        // and the tree is byte-for-byte what it was, `a/` included.
        let dir = book("move-tree-atomic");
        let before = snapshot(&dir);
        let mut w = failing_ws(&dir, 2);
        assert!(block_on(w.move_tree(Path::new("a"), Path::new("b/a"))).is_err());
        assert_eq!(snapshot(&dir), before);
        assert!(dir.join("a/attachments/photo.jpg").is_file());
    }

    #[test]
    fn a_manifest_parting_from_its_archive_has_its_root_respelled() {
        // A manifest node and its store inside the directory, the archive it
        // covers outside it: `root` is respelled from the store's new home and
        // the node's pin re-stamped, as `rename` does for a lone manifest node.
        // Moved together (the common shape) the spelling would not change.
        let dir = tempdir("move-tree-manifest");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- docs/photos.md\n---\n",
        );
        std::fs::create_dir_all(dir.join("photos")).unwrap();
        std::fs::write(dir.join("photos/one.jpg"), b"1").unwrap();
        let store = "root: ../photos/\nfiles:\n- path: one.jpg\n";
        write(&dir, "docs/photos.manifest.yaml", store);
        let pin = crate::fixity::digest(store.as_bytes());
        write(
            &dir,
            "docs/photos.md",
            format!(
                "---\ntitle: Photos\npart_of: /index.md\nmanifest: photos.manifest.yaml\ncontent_hash: {pin}\n---\n"
            ),
        );
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);

        block_on(ws(&dir).move_tree(Path::new("docs"), Path::new("archive/docs"))).unwrap();

        let store = read(&dir, "archive/docs/photos.manifest.yaml");
        assert!(store.contains("root: ../../photos/"), "{store}");
        let node = read(&dir, "archive/docs/photos.md");
        assert!(
            node.contains(&crate::fixity::digest(store.as_bytes())),
            "pin re-stamped: {node}"
        );
        assert!(
            dir.join("photos/one.jpg").is_file(),
            "the archive stays put"
        );
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }
}
