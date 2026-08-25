//! The read primitive — root-escape clamp, read-scope memo, filesystem read,
//! [`Document::parse`] — that every pass built on top of the graph shares.
//! See the module doc at [`crate::graph`] for how this sits beside
//! [`resolve`](super::resolve) and the census.

use std::path::Path;

use super::Graph;
use crate::document::{Body, Document};
use crate::error::{Error, Result};
use crate::fs::ReadStorage;
use crate::link;

impl<FS: ReadStorage, Ix> Graph<FS, Ix> {
    /// Read and parse the workspace-relative document at `path`, returning the
    /// raw text alongside. The building block traversal, validation, and
    /// mutation share.
    pub async fn load(&self, path: &Path) -> Result<(String, Document)> {
        // Clamp reads to the workspace root: `path` may originate in a document's
        // own metadata (a `contents`/`part_of` target), so a hostile or careless
        // `../../../etc/passwd` must be refused here rather than opened. The
        // traversal turns this error into an `Unreadable` node; a direct caller
        // sees the `Escape` error itself.
        if link::escapes_root(path) {
            return Err(Error::Escape(path.to_path_buf()));
        }
        // Inside a `read_scope`, a document already read this operation is
        // answered from memory — the escape check above still runs first, so a
        // memo can never be the thing that lets a hostile path through.
        if let Some(hit) = self.memo_hit(path) {
            return Ok(hit);
        }
        let text = self.fs().read_to_string(&self.root().join(path)).await?;
        let doc = Document::parse(path, &text)?;
        self.memo_remember(path, &text, &doc);
        Ok((text, doc))
    }

    /// Read and parse the workspace-relative document at `path`, returning its
    /// full [`Document`] — the public counterpart to [`load`](Self::load), for
    /// a caller walking a [`Node`](crate::graph::Node) tree who needs more than
    /// [`Node::title`](crate::graph::Node::title) (the rest of the frontmatter,
    /// the body, the carrier) without re-reading and re-parsing the file by
    /// hand.
    ///
    /// Unlike the traversal, which degrades a bad target to a
    /// [`NodeKind::Unreadable`](crate::graph::NodeKind::Unreadable) node, this
    /// surfaces the [`Error`] directly — a caller who names a path expects to
    /// know why it failed, not to receive a placeholder.
    pub async fn document(&self, path: impl AsRef<Path>) -> Result<Document> {
        let path = link::normalize(path);
        self.load(&path).await.map(|(_, doc)| doc)
    }

    /// The prose body of the document at `path`, wherever it physically lives —
    /// the read-side counterpart to the resolution `content_hash` already makes
    /// (`prov`'s `Workspace::covered_digest`) and the census already makes
    /// ([`Graph::census`](super::Graph::census)).
    ///
    /// For a combined document this is [`Document::body`] and the document's own
    /// path, so a caller pays one read and gets what it always had. For a
    /// *separated* one — a whole-file metadata node whose `content` names a
    /// sibling prose file — it is the sibling's text. [`Document`] is a per-file
    /// parse and deliberately stays one: it reports what its own file says, and
    /// the splicing belongs to the layer that can reach the other file.
    ///
    /// The `content` target is clamped the same way [`load`](Self::load) clamps
    /// its own argument, and for the same reason: it is a path read *out of a
    /// document*, so `content: ../../../etc/passwd` is data naming a file
    /// outside the workspace and must be refused rather than opened.
    ///
    /// An **attachment sidecar** is refused rather than read. Its `content`
    /// names opaque bytes — a JPEG, a PDF — and `attach --opaque` promises prov
    /// will never open them as a document; returning them as a `String` would
    /// break that promise, and on most payloads would fail as invalid UTF-8
    /// anyway, which is a confusing way to learn the file was never prose.
    pub async fn body(&self, path: impl AsRef<Path>) -> Result<Body> {
        let path = link::normalize(path);
        let (_, doc) = self.load(&path).await?;
        let Some(content) = doc.content_path(&path) else {
            return Ok(Body {
                text: doc.body,
                path,
            });
        };
        // Clamped before it is classified. `is_attachment` reads the *target's*
        // extension, so an escaping path is also an opaque-looking one more often
        // than not (`../../../etc/passwd` has no extension prov reads) — and
        // "that is a payload, not prose" is a description of the file, offered
        // where "that file is not yours to name" is the answer.
        if link::escapes_root(&content) {
            return Err(Error::Escape(content));
        }
        if doc.is_attachment() {
            return Err(Error::Structure(format!(
                "{}: attachment sidecar for {} — an opaque payload, not a prose body",
                path.display(),
                content.display(),
            )));
        }
        let text = self.read_text(&content).await?;
        Ok(Body {
            text,
            path: content,
        })
    }
}

// These tests use YAML frontmatter fixtures, so they run under the `yaml` feature.
#[cfg(all(test, feature = "yaml"))]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::exec::block_on;
    use crate::fs::StdFs;
    use crate::graph::ReadSettings;
    use crate::index::NoIndex;

    use prov_testkit::write;
    fn tempdir(tag: &str) -> PathBuf {
        prov_testkit::scratch("load", tag)
    }

    #[test]
    fn document_reads_full_metadata_for_a_workspace_relative_path() {
        let dir = tempdir("document");
        write(
            &dir,
            "notes/a.md",
            "---\ntitle: A\nauthor: Ada\n---\nbody text\n",
        );

        let ws = Graph::new(StdFs, &dir, NoIndex, ReadSettings::default());
        let doc = block_on(ws.document("notes/a.md")).unwrap();
        let meta = fig::Value::from(&doc.meta);
        assert_eq!(meta.get("title").and_then(fig::Value::as_str), Some("A"));
        assert_eq!(meta.get("author").and_then(fig::Value::as_str), Some("Ada"));
        assert_eq!(doc.body, "body text\n");
    }

    #[test]
    fn document_surfaces_the_error_for_an_unreadable_path() {
        let dir = tempdir("document-missing");
        let ws = Graph::new(StdFs, &dir, NoIndex, ReadSettings::default());
        assert!(block_on(ws.document("nope.md")).is_err());
    }

    #[test]
    fn body_of_a_combined_document_is_its_own_prose_and_its_own_path() {
        let dir = tempdir("body-combined");
        write(&dir, "notes/a.md", "---\ntitle: A\n---\nbody text\n");

        let ws = Graph::new(StdFs, &dir, NoIndex, ReadSettings::default());
        let body = block_on(ws.body("notes/a.md")).unwrap();
        assert_eq!(body.text, "body text\n");
        assert_eq!(body.path, PathBuf::from("notes/a.md"));
    }

    /// The gap this method exists to close: the node's *own* body is empty, and
    /// reading that as the document's prose reports "no prose" for a document
    /// that has plenty.
    #[test]
    fn body_of_a_separated_document_is_the_file_its_content_names() {
        let dir = tempdir("body-separated");
        write(&dir, "notes/a.yaml", "title: A\ncontent: a.md\n");
        write(&dir, "notes/a.md", "# Heading\n\nseparated prose\n");

        let ws = Graph::new(StdFs, &dir, NoIndex, ReadSettings::default());
        let node = block_on(ws.document("notes/a.yaml")).unwrap();
        assert_eq!(node.body, "", "the node's own file carries no prose");

        let body = block_on(ws.body("notes/a.yaml")).unwrap();
        assert_eq!(body.text, "# Heading\n\nseparated prose\n");
        assert_eq!(
            body.path,
            PathBuf::from("notes/a.md"),
            "the path a caller asks for the body's grammar"
        );
    }

    /// `attach --opaque` promises prov never opens the payload as a document.
    #[test]
    fn body_refuses_an_attachment_sidecar_rather_than_reading_its_payload() {
        let dir = tempdir("body-attachment");
        write(&dir, "photo.jpg.yaml", "title: Photo\ncontent: photo.jpg\n");
        write(&dir, "photo.jpg", "not really a jpeg");

        let ws = Graph::new(StdFs, &dir, NoIndex, ReadSettings::default());
        let err = block_on(ws.body("photo.jpg.yaml")).unwrap_err();
        assert!(
            err.to_string().contains("opaque payload"),
            "unexpected error: {err}"
        );
    }

    /// `content` is a path read *out of a document*, so it is data and gets the
    /// clamp every other data-borne path gets.
    #[test]
    fn body_refuses_a_content_target_that_escapes_the_root() {
        let dir = tempdir("body-escape");
        write(&dir, "a.yaml", "title: A\ncontent: ../../../etc/passwd\n");

        let ws = Graph::new(StdFs, &dir, NoIndex, ReadSettings::default());
        assert!(matches!(block_on(ws.body("a.yaml")), Err(Error::Escape(_))));
    }
}
