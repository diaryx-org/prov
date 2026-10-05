//! `adopt` — an existing file linked under a parent, both ways, without
//! creating, moving, or rewriting a body.
//!
//! The onboarding complement of [`create`](super::create) for content that
//! predates the workspace: additive and idempotent, and refusing rather than
//! overwriting a containment the child already claims — the case
//! [`reparent`](super::reparent) exists to answer.

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

impl<FS: Storage, IdP: IdentityPolicy, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Adopt an existing document at `child` as a spanning child of `parent`,
    /// authoring **both** directions — the child's inverse link up and the
    /// parent's spanning entry down — without creating, moving, or rewriting the
    /// body of any file. The complement of [`create`](Self::create) for content
    /// that predates the workspace (`docs/init-adoption.md`, Phase 1).
    ///
    /// Additive and idempotent: whichever direction already exists is left as-is,
    /// so re-running (or adopting a partially-linked file) is a no-op. Both files
    /// must exist, and neither may be an attachment's payload (opaque bytes, or a
    /// file a sidecar shadows): the sidecar is the node. Refuses when `child`
    /// already declares the inverse relation to a
    /// *different* parent — a contested containment a human must resolve, never
    /// overwritten (mirrors [`suggest_fix`](Self::suggest_fix) declining the same
    /// case). Registers `parent` when the workspace authors id links, exactly as
    /// `create` and the missing-inverse autofix do.
    pub async fn adopt(&mut self, child: &Path, parent: &Path) -> Result<()> {
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
        // An attachment's payload is not a node: its sidecar is, and adopting
        // the payload would write frontmatter into bytes prov holds unread —
        // corrupting an image, or editing the exhibit a specimen is.
        for existing in [&child, &parent] {
            if let Some(sidecar) = self.attachment_for(existing).await? {
                return Err(Error::Structure(format!(
                    "{} is an attachment's payload — its sidecar {} is the node; \
                     adopt that instead",
                    existing.display(),
                    sidecar.display()
                )));
            }
            if prov_graph::document::is_opaque_payload(existing) {
                return Err(Error::Structure(format!(
                    "{} is not a document prov reads — `attach` it to give it a node",
                    existing.display()
                )));
            }
        }

        let (child_text, child_doc) = self.load(&child).await?;
        let (parent_text, parent_doc) = self.load(&parent).await?;
        let child_meta = fig::Value::from(&child_doc.meta);
        let parent_meta = fig::Value::from(&parent_doc.meta);

        // Up: does the child already declare the inverse relation? If it points
        // here, that direction is done; if it points elsewhere, refuse rather than
        // clobber a deliberate parent claim.
        let already_up = match child_meta.get(inverse.as_str()) {
            Some(existing) => {
                let points_here = prov_graph::meta::link_strings(existing).iter().any(|t| {
                    self.resolve_link(&child, &Link::parse(t)) == Target::Path(parent.clone())
                });
                if !points_here {
                    return Err(Error::Structure(format!(
                        "{} already declares {inverse} to a different parent — resolve the \
                         contested containment by hand",
                        child.display()
                    )));
                }
                true
            }
            None => false,
        };
        // Down: does the parent's spanning field already resolve to the child?
        let already_down =
            self.relations().children(&parent_meta).iter().any(|t| {
                self.resolve_link(&parent, &Link::parse(t)) == Target::Path(child.clone())
            });

        if already_up && already_down {
            return Ok(());
        }

        let child_title = child_doc
            .meta
            .get("title")
            .and_then(prov_graph::title::title_text)
            .unwrap_or_else(|| link::path_to_title(&child));
        let parent_title = parent_doc
            .meta
            .get("title")
            .and_then(prov_graph::title::title_text)
            .unwrap_or_else(|| link::path_to_title(&parent));

        let mut cs = self.change();
        // The child's inverse link back up. Comment-/format-preserving edit of the
        // existing document (its body is untouched), in the `inverse` relation's
        // reference style — the parent exists, so an id link registers it by path.
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
        // The parent's spanning entry going down (the child exists on disk, so an
        // id link registers it by path). Append to the sequence, creating it if
        // the parent had no spanning field yet.
        if !already_down {
            let down = self
                .authored_target(&spanning, &parent, &child, &child_title, true)
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
            cs.write(&parent, parent_editor.render()?);
        }
        self.commit(cs).await
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::super::support::*;
    use super::*;
    use prov_graph::link::LinkStyle;

    #[test]
    fn adopt_links_an_existing_document_both_ways_preserving_its_body() {
        // A loose note that predates the workspace: adoption links it under the
        // root in both directions and leaves its prose untouched.
        let dir = tempdir("adopt");
        write(&dir, "index.md", "---\ntitle: Home\n---\n");
        write(
            &dir,
            "notes/loose.md",
            "---\ntitle: Loose\n---\nOriginal body, kept.\n",
        );
        // The default markdown-root style authors `/index.md`, which resolves from
        // a subdirectory child (a bare relative path would need `../`).
        let mut w = Workspace::builder(StdFs).root(&dir).build();

        block_on(w.adopt(Path::new("notes/loose.md"), Path::new("index.md"))).unwrap();

        // Down: the root's spanning field gained the child.
        assert!(
            read(&dir, "index.md").contains("notes/loose.md"),
            "{}",
            read(&dir, "index.md")
        );
        // Up: the child declares part_of back to the root (workspace-absolute), and
        // keeps its body.
        let child = read(&dir, "notes/loose.md");
        assert!(child.contains("/index.md"), "{child}");
        assert!(
            child.contains("Original body, kept."),
            "body must be preserved: {child}"
        );
        // The whole workspace validates — no orphan, no missing inverse.
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn adopt_is_idempotent_and_refuses_a_contested_parent() {
        let dir = tempdir("adopt-idem");
        write(&dir, "index.md", "---\ntitle: Home\n---\n");
        write(&dir, "other.md", "---\ntitle: Other\n---\n");
        write(&dir, "a.md", "---\ntitle: A\n---\n");
        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .link_style(LinkStyle::PlainRelative)
            .build();

        // First adoption links it; a second is a clean no-op (no duplicate entry).
        block_on(w.adopt(Path::new("a.md"), Path::new("index.md"))).unwrap();
        block_on(w.adopt(Path::new("a.md"), Path::new("index.md"))).unwrap();
        assert_eq!(
            read(&dir, "index.md").matches("a.md").count(),
            1,
            "no duplicate spanning entry"
        );

        // a.md now claims index.md; adopting it under a different parent is refused.
        let contested = block_on(w.adopt(Path::new("a.md"), Path::new("other.md")));
        assert!(contested.is_err(), "a contested parent must be refused");
    }

    #[test]
    fn adopt_refuses_an_attachments_payload_and_names_its_sidecar() {
        // A specimen is a `.md` adopt could write `part_of` into — editing the
        // exhibit. Its sidecar is the node, and the refusal says so.
        let dir = tempdir("adopt-specimen");
        write(&dir, "index.md", "---\ntitle: Home\n---\n");
        write(&dir, "other.md", "---\ntitle: Other\n---\n");
        attach_specimen(&dir, "sample.md", "index.md");

        let err =
            block_on(ws(&dir).adopt(Path::new("sample.md"), Path::new("other.md"))).unwrap_err();
        assert!(
            err.to_string().contains("its sidecar sample.md.yaml"),
            "{err}"
        );
        let err =
            block_on(ws(&dir).adopt(Path::new("other.md"), Path::new("sample.md"))).unwrap_err();
        assert!(
            err.to_string().contains("its sidecar sample.md.yaml"),
            "{err}"
        );
        assert_eq!(read(&dir, "sample.md"), SPECIMEN);

        // An opaque file with no sidecar yet is pointed at `attach`.
        std::fs::write(dir.join("photo.jpg"), [0xff, 0xd8]).unwrap();
        let err =
            block_on(ws(&dir).adopt(Path::new("photo.jpg"), Path::new("index.md"))).unwrap_err();
        assert!(err.to_string().contains("attach"), "{err}");
        assert_eq!(std::fs::read(dir.join("photo.jpg")).unwrap(), [0xff, 0xd8]);
    }
}
