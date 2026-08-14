//! The read primitive — root-escape clamp, read-scope memo, filesystem read,
//! [`Document::parse`] — that every pass built on top of the graph shares.
//! See the module doc at [`crate::graph`] for how this sits beside
//! [`resolve`](super::resolve) and the census.

use std::path::Path;

use super::Graph;
use crate::document::Document;
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

    fn write(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prov-load-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
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
}
