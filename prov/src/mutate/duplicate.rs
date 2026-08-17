//! `duplicate` — a document copied as a fresh sibling under the same parent.
//!
//! A *shallow* copy, and deliberately so: no cloned identity (an ID names one
//! document), no cloned children (link-shaped containment would leave each child
//! claimed by two parents), and a name bumped until both the node and — for a
//! separated pair — its body file are free.

use std::path::{Path, PathBuf};

use fig::Segment;

use crate::identity::{IdentityPolicy, Trigger};
use crate::workspace::Workspace;
use prov_graph::error::{Error, Result};
use prov_graph::link;
use prov_store::edit::MetaEditor;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

use super::maintain::{body_sibling, content_target};

impl<FS: Storage, IdP: IdentityPolicy, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Duplicate the document at `source` as a fresh sibling under the same
    /// parent, returning the new file's path. The copy carries `source`'s title,
    /// body, and metadata verbatim, with three deliberate exceptions:
    ///
    /// - **No cloned identity.** A stamped frontmatter `id` (frontmatter storage,
    ///   DESIGN §5) is dropped — an ID names *one* document, never two — so the
    ///   copy is unregistered until something links or publishes it. When the
    ///   workspace authors id links, attaching the copy below mints it a *new* ID.
    /// - **No cloned children.** The spanning field is dropped, so the copy is a
    ///   childless duplicate of just this node. Deep-copying the subtree would
    ///   leave every child claimed by two parents (its inverse still names
    ///   `source`); prov's link-shaped containment — unlike diaryx's
    ///   directory-shaped copy, which recurses the folder — makes the shallow
    ///   copy the only unambiguous one.
    /// - **A unique name.** `foo.md` → `foo-copy.md`, then `foo-copy-2.md`, ….
    ///
    /// The copy inherits `source`'s parent: its inverse link (copied verbatim, and
    /// still valid — the copy sits in the same directory) already points there,
    /// and the parent's spanning field gains the copy — the same bidirectional
    /// link `create`/`adopt` author. A `source` with no parent (the spanning root,
    /// or an orphan) is copied without attaching. A **separated** node (a
    /// whole-file node with a `content` pointer) duplicates its body file too, and
    /// the copy points at *its own* body.
    pub async fn duplicate(&mut self, source: &Path) -> Result<PathBuf> {
        let source = link::normalize(source);
        if !self.exists(&source).await? {
            return Err(Error::NotFound(source.to_path_buf()));
        }
        let (source_text, doc) = self.load(&source).await?;
        // A manifest node has no meaningful copy. Sharing the manifest would put
        // two nodes' checksums over one record store; copying it would put two
        // records over one archive, free to drift apart with no rule about which
        // is right — the very state `attach --manifest` refuses to create. The
        // archive is the archive, and describing it twice describes nothing.
        if doc.is_manifest_node() {
            return Err(Error::Structure(format!(
                "{} covers a directory with a manifest, which has no copy — \
                 `attach --manifest` covers a different directory",
                source.display()
            )));
        }
        let meta = fig::Value::from(&doc.meta);
        let (spanning, inverse) = self.spanning_pair()?;

        // A separated node carries its prose/payload in a sibling file; the copy
        // needs its own. Resolve the source body up front so the unique-name
        // search can keep the node *and* its body collision-free together.
        let body_from = content_target(&doc, &source);
        let (dest, body_dest) = self.unique_copy_path(&source, body_from.as_deref()).await?;

        // The copy's text: `source`'s metadata and body verbatim, minus the cloned
        // `id` (identity is per-document) and the spanning field (no cloned
        // children), and — for a separated node — repointed `content` at its own
        // new body file. A carrier-less file (pure prose, not a linked node) is
        // copied byte-for-byte.
        let copy_text = if let Some(carrier) = doc.carrier {
            let mut editor = MetaEditor::open(&source_text, carrier)?;
            let _ = editor.delete(&[Segment::Key("id")]);
            let _ = editor.delete(&[Segment::Key(&spanning)]);
            if let Some((_, new_ref)) = &body_dest {
                editor
                    .replace_value(&[Segment::Key("content")], fig::Value::Str(new_ref.clone()))?;
            }
            editor.render()?
        } else {
            source_text.clone()
        };

        // The parent gains a spanning entry for the copy (going "down"). The copy
        // is not yet on disk, so `authored_target` mints its id directly rather
        // than register-by-path — exactly as `create`'s down-link does. The copy's
        // own inverse link "up" is already present (copied verbatim) and resolves
        // unchanged, so it is not re-authored. A parentless source just skips this.
        let parent = self.single_target(&doc, &inverse, &source);
        let mut cs = self.change();
        let parent_write = if let Some(parent) = &parent {
            let (parent_text, parent_doc) = self.load(parent).await?;
            let copy_title = meta
                .get("title")
                .and_then(fig::Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| link::path_to_title(&dest));
            let down = self
                .authored_target(&spanning, parent, &dest, &copy_title, false)
                .await?;
            let mut parent_editor = MetaEditor::open_or_init(&parent_text, parent_doc.carrier)?;
            let span_path = [Segment::Key(&spanning)];
            if parent_editor
                .append_value(&span_path, fig::Value::Str(down.clone()))
                .is_err()
            {
                parent_editor
                    .set_value(&span_path, fig::Value::Seq(vec![fig::Value::Str(down)]))?;
            }
            Some((parent.clone(), parent_editor.render()?))
        } else {
            None
        };

        // All edits computed; stage them. Node first, then its body copy (opaque
        // payload bytes carried verbatim), then the parent's updated entry.
        cs.write(&dest, copy_text);
        if let (Some(body_from), Some((body_to, _))) = (&body_from, &body_dest) {
            let bytes = self.read_bytes(body_from).await?;
            cs.write(body_to, bytes);
        }
        if let Some((parent, text)) = parent_write {
            cs.write(parent, text);
        }

        // Identity hook — an eager policy assigns the copy an ID from birth
        // (idempotent: the down-link above already registered it under `id_links`).
        if self.identity().registration().fires_on(Trigger::Create)
            && self.index().id_for_path(&dest).is_none()
        {
            let id = self.mint_unique(&dest);
            self.index_mut().register(&id, &dest);
        }
        self.commit(cs).await?;
        Ok(dest)
    }

    /// The first free `stem-copy[-N].ext` beside `source`, paired with its body
    /// file's destination when `source` is a separated node (`body_from` is its
    /// current body). The suffix is bumped until *both* the node and its body are
    /// free, so a duplicated pair never half-collides with an existing one.
    async fn unique_copy_path(
        &self,
        source: &Path,
        body_from: Option<&Path>,
    ) -> Result<(PathBuf, Option<(PathBuf, String)>)> {
        let stem = source.file_stem().and_then(|s| s.to_str()).ok_or_else(|| {
            Error::Structure(format!("{} has no filename to copy", source.display()))
        })?;
        let ext = source.extension().and_then(|e| e.to_str());
        for n in 1.. {
            let suffix = if n == 1 {
                "-copy".to_string()
            } else {
                format!("-copy-{n}")
            };
            let name = match ext {
                Some(ext) => format!("{stem}{suffix}.{ext}"),
                None => format!("{stem}{suffix}"),
            };
            let node = match source.parent() {
                Some(dir) => dir.join(name),
                None => PathBuf::from(name),
            };
            let body = body_from.map(|b| body_sibling(&node, b));
            let node_free = !self.exists(&node).await?;
            let body_free = match &body {
                Some((body_to, _)) => !self.exists(body_to).await?,
                None => true,
            };
            if node_free && body_free {
                return Ok((node, body));
            }
        }
        unreachable!("the copy-suffix search is unbounded")
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::super::support::*;
    use super::*;

    #[test]
    fn duplicate_copies_under_the_same_parent_and_links_both_ways() {
        let dir = tempdir("duplicate");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- a.md\n---\n",
        );
        write(
            &dir,
            "a.md",
            "---\ntitle: A\npart_of: index.md\n---\nA body, copied.\n",
        );

        let copy = block_on(ws(&dir).duplicate(Path::new("a.md"))).unwrap();
        assert_eq!(copy, PathBuf::from("a-copy.md"));

        // The copy carries the source's title and body verbatim, and keeps its
        // inverse link up (same directory → unchanged).
        let copied = read(&dir, "a-copy.md");
        assert!(copied.contains("title: A"), "{copied}");
        assert!(copied.contains("part_of: index.md"), "{copied}");
        assert!(copied.contains("A body, copied."), "{copied}");
        // The parent gained a spanning entry for the copy, keeping the original.
        let index = read(&dir, "index.md");
        assert!(index.contains("- a.md"), "original kept: {index}");
        assert!(index.contains("a-copy.md"), "copy attached: {index}");
        // The whole workspace validates — both directions are sound.
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn duplicate_bumps_the_suffix_and_drops_cloned_children() {
        // A container with a child, duplicated: the copy is childless (no double
        // parent), and a second duplicate takes the next free `-copy-N` name.
        let dir = tempdir("duplicate-suffix");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- mid.md\n---\n",
        );
        write(
            &dir,
            "mid.md",
            "---\ntitle: Mid\npart_of: index.md\ncontents:\n- leaf.md\n---\n",
        );
        write(&dir, "leaf.md", "---\ntitle: Leaf\npart_of: mid.md\n---\n");

        let first = block_on(ws(&dir).duplicate(Path::new("mid.md"))).unwrap();
        assert_eq!(first, PathBuf::from("mid-copy.md"));
        // The copy did not clone the child — no contested containment for leaf.md.
        let copy = read(&dir, "mid-copy.md");
        assert!(
            !copy.contains("leaf.md"),
            "children must not be cloned: {copy}"
        );
        assert!(copy.contains("part_of: index.md"), "{copy}");

        // A second duplicate of the same source bumps past the taken name.
        let second = block_on(ws(&dir).duplicate(Path::new("mid.md"))).unwrap();
        assert_eq!(second, PathBuf::from("mid-copy-2.md"));
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn duplicate_does_not_clone_a_registered_id() {
        // Frontmatter-stamped identity must not travel to the copy: an ID names
        // exactly one document. The copy is a distinct path with (here, lazy) no
        // id of its own until something links it.
        let dir = tempdir("duplicate-id");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- a.md\n---\n",
        );
        write(
            &dir,
            "a.md",
            "---\ntitle: A\npart_of: index.md\nid: aaaaaaa\n---\nbody\n",
        );

        let copy = block_on(id_ws(&dir).duplicate(Path::new("a.md"))).unwrap();
        let copied = read(&dir, &copy.to_string_lossy());
        assert!(
            !copied.contains("aaaaaaa"),
            "the source's id must not be cloned: {copied}"
        );
        assert!(
            !copied.contains("\nid:"),
            "the copy carries no stamped id: {copied}"
        );
    }

    #[test]
    fn duplicate_a_separated_node_copies_its_body_and_repoints_content() {
        // A separated node (whole-file metadata + a `content` body pointer): the
        // copy is its own pair, pointing at its own body, not the source's.
        let dir = tempdir("duplicate-separate");
        write(&dir, "index.yaml", "title: Root\ncontents:\n- notes.yaml\n");
        write(
            &dir,
            "notes.yaml",
            "title: Notes\npart_of: index.yaml\ncontent: notes.md\n",
        );
        write(&dir, "notes.md", "Prose body, duplicated.\n");

        let copy = block_on(ws(&dir).duplicate(Path::new("notes.yaml"))).unwrap();
        assert_eq!(copy, PathBuf::from("notes-copy.yaml"));

        // The copy node points at its own body; the body file is a real copy.
        let node = read(&dir, "notes-copy.yaml");
        assert!(
            node.contains("content: notes-copy.md"),
            "repointed content: {node}"
        );
        assert_eq!(read(&dir, "notes-copy.md"), "Prose body, duplicated.\n");
        // Source body untouched, and the workspace validates.
        assert_eq!(read(&dir, "notes.md"), "Prose body, duplicated.\n");
        assert_eq!(block_on(ws(&dir).check("index.yaml")).unwrap(), vec![]);
    }
}
