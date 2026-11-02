//! The read primitive — root-escape clamp, read-scope memo, filesystem read,
//! [`Document::parse`] — that every pass built on top of the graph shares.
//! See the module doc at [`crate::graph`] for how this sits beside
//! [`resolve`](super::resolve) and the census.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::Graph;
use crate::document::{Body, Document, Loaded};
use crate::error::{Error, Result};
use crate::fs::ReadStorage;
use crate::link::{self, BodyLink};

/// One document [`prefetch`](Graph::prefetch) read: its path, its text, its
/// parse, and the links in its body when they were asked for.
#[cfg(all(feature = "parallel", not(target_family = "wasm")))]
pub(crate) type Prefetched = (PathBuf, Arc<Loaded>, Option<Arc<[BodyLink]>>);

impl<FS: ReadStorage, Ix> Graph<FS, Ix> {
    /// Read and parse the workspace-relative document at `path`, returning the
    /// raw text alongside. The building block traversal, validation, and
    /// mutation share.
    ///
    /// The owned form of [`read`](Self::read): a caller that only looks at
    /// what it gets should ask `read`, which inside a read scope hands back
    /// the memo's own copy rather than a fresh one.
    pub async fn load(&self, path: &Path) -> Result<(String, Document)> {
        let read = self.read(path).await?;
        let Loaded { text, doc } = Arc::try_unwrap(read).unwrap_or_else(|shared| (*shared).clone());
        Ok((text, doc))
    }

    /// [`load`](Self::load), shared: the document at `path` as one read found
    /// it. Inside a read scope a document already read this operation is the
    /// same [`Loaded`], so asking again costs a reference count.
    pub async fn read(&self, path: &Path) -> Result<Arc<Loaded>> {
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
        let read = Arc::new(Loaded { text, doc });
        self.memo_remember(path, Arc::clone(&read));
        Ok(read)
    }

    /// The links in `doc`'s body, as [`link::scan_body_links`] finds them —
    /// answered from the read memo when this operation has already scanned
    /// the body `path` was remembered with, and remembered there otherwise.
    /// Finding them is a parse of the prose, which costs more than reading it.
    pub fn body_links(&self, path: &Path, doc: &Document) -> Vec<BodyLink> {
        if let Some(links) = self.memo_links(path, &doc.body) {
            return links.to_vec();
        }
        let links: Arc<[BodyLink]> = link::scan_body_links(path, &doc.body).into();
        self.memo_remember_links(path, &doc.body, Arc::clone(&links));
        links.to_vec()
    }

    /// Read the documents at `paths` that this operation has not read yet,
    /// parse them on rayon's pool, and remember them for the rest of the
    /// operation, so that the [`load`](Self::load) each will get is answered
    /// from the memo. With `links`, each body's links are found there too
    /// ([`body_links`](Self::body_links)).
    ///
    /// A warmer and nothing else. Where the storage hands over a
    /// [`SharedRead`](crate::fs::SharedRead), each document is read and parsed
    /// in one task on the pool. Otherwise the reads go through the storage one
    /// at a time, as `load`'s do, and only the parsing runs in parallel. A
    /// path that fails to read or parse, or that the caller guessed wrong, is
    /// simply not remembered, and `load` meets it as it would have. Without
    /// the `parallel` feature, on a target without threads, from inside
    /// rayon's pool, or outside a read scope (where nothing would be kept) it
    /// does nothing at all.
    pub(crate) async fn prefetch(&self, paths: impl IntoIterator<Item = PathBuf>, links: bool) {
        #[cfg(all(feature = "parallel", not(target_family = "wasm")))]
        {
            use rayon::prelude::*;

            if rayon::current_thread_index().is_some() || !self.memo_in_scope() {
                return;
            }
            let mut asked = std::collections::HashSet::new();
            let wanted: Vec<PathBuf> = paths
                .into_iter()
                .filter(|path| {
                    !link::escapes_root(path)
                        && !self.memo_holds(path)
                        && asked.insert(path.clone())
                })
                .collect();
            let parse = |path: PathBuf, text: String| -> Option<Prefetched> {
                let doc = Document::parse(&path, &text).ok()?;
                let found: Option<Arc<[BodyLink]>> =
                    links.then(|| link::scan_body_links(&path, &doc.body).into());
                Some((path, Arc::new(Loaded { text, doc }), found))
            };
            let parsed: Vec<Prefetched> = match self.fs().shared_reader() {
                Some(reader) => {
                    let root = self.root();
                    wanted
                        .into_par_iter()
                        .filter_map(|path| {
                            let text = reader.read_to_string(&root.join(&path)).ok()?;
                            parse(path, text)
                        })
                        .collect()
                }
                None => {
                    let mut read = Vec::new();
                    for path in wanted {
                        if let Ok(text) = self.fs().read_to_string(&self.root().join(&path)).await {
                            read.push((path, text));
                        }
                    }
                    read.into_par_iter()
                        .filter_map(|(path, text)| parse(path, text))
                        .collect()
                }
            };
            self.memo_remember_all(parsed);
        }
        #[cfg(not(all(feature = "parallel", not(target_family = "wasm"))))]
        let _ = (paths, links);
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
        let read = self.read(&path).await?;
        Ok(Arc::try_unwrap(read).map_or_else(|shared| shared.doc.clone(), |own| own.doc))
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
        let read = self.read(&path).await?;
        let doc = &read.doc;
        let Some(content) = doc.content_path(&path) else {
            return Ok(Body {
                text: doc.body.clone(),
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

#[cfg(test)]
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

    #[cfg(all(feature = "parallel", not(target_family = "wasm")))]
    #[test]
    fn a_prefetch_remembers_what_it_read_and_nothing_it_could_not() {
        let dir = tempdir("prefetch");
        write(&dir, "a.md", "---\ntitle: A\n---\nsee [b](b.md)\n");
        write(&dir, "b.md", "---\ntitle: B\n---\nno links\n");
        let ws = Graph::new(StdFs, &dir, NoIndex, ReadSettings::default());
        let paths = || ["a.md", "b.md", "gone.md", "../outside.md"].map(PathBuf::from);

        block_on(ws.prefetch(paths(), true));
        assert!(!ws.memo_holds(Path::new("a.md")), "kept outside a scope");

        let _scope = ws.read_scope();
        block_on(ws.prefetch(paths(), true));
        assert!(ws.memo_holds(Path::new("a.md")));
        assert!(ws.memo_holds(Path::new("b.md")));
        assert!(!ws.memo_holds(Path::new("gone.md")));
        assert!(!ws.memo_holds(Path::new("../outside.md")));
        let (_, a) = block_on(ws.load(Path::new("a.md"))).unwrap();
        assert_eq!(
            ws.memo_links(Path::new("a.md"), &a.body).map(|l| l.len()),
            Some(1)
        );
        assert_eq!(
            ws.body_links(Path::new("a.md"), &a),
            link::scan_body_links(Path::new("a.md"), &a.body)
        );
    }

    /// `std::fs`, minus the shared reader: a backend whose reads prov must
    /// make one at a time.
    #[cfg(all(feature = "parallel", not(target_family = "wasm")))]
    struct OneAtATime;

    #[cfg(all(feature = "parallel", not(target_family = "wasm")))]
    impl ReadStorage for OneAtATime {
        async fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
            StdFs.read(path).await
        }
        async fn read_to_string(&self, path: &Path) -> std::io::Result<String> {
            StdFs.read_to_string(path).await
        }
        async fn read_dir(&self, path: &Path) -> std::io::Result<Vec<crate::fs::DirEntry>> {
            StdFs.read_dir(path).await
        }
        async fn metadata(&self, path: &Path) -> std::io::Result<crate::fs::Metadata> {
            StdFs.metadata(path).await
        }
    }

    #[cfg(all(feature = "parallel", not(target_family = "wasm")))]
    #[test]
    fn a_prefetch_without_a_shared_reader_reads_in_turn_and_remembers_the_same() {
        let dir = tempdir("prefetch-in-turn");
        write(&dir, "a.md", "---\ntitle: A\n---\nsee [b](b.md)\n");
        write(&dir, "b.md", "---\ntitle: B\n---\nno links\n");
        let ws = Graph::new(OneAtATime, &dir, NoIndex, ReadSettings::default());
        assert!(ws.fs().shared_reader().is_none());

        let _scope = ws.read_scope();
        block_on(ws.prefetch(["a.md", "b.md", "gone.md"].map(PathBuf::from), true));
        assert!(ws.memo_holds(Path::new("a.md")));
        assert!(ws.memo_holds(Path::new("b.md")));
        assert!(!ws.memo_holds(Path::new("gone.md")));
        let (_, a) = block_on(ws.load(Path::new("a.md"))).unwrap();
        assert_eq!(
            ws.memo_links(Path::new("a.md"), &a.body).map(|l| l.len()),
            Some(1)
        );
    }

    #[test]
    fn a_read_inside_a_scope_shares_what_the_first_read_found() {
        let dir = tempdir("read-shared");
        write(&dir, "a.md", "---\ntitle: A\n---\nbody\n");
        let ws = Graph::new(StdFs, &dir, NoIndex, ReadSettings::default());
        let a = Path::new("a.md");

        let first = block_on(ws.read(a)).unwrap();
        let again = block_on(ws.read(a)).unwrap();
        assert!(!Arc::ptr_eq(&first, &again), "nothing is shared outside a scope");

        let _scope = ws.read_scope();
        let first = block_on(ws.read(a)).unwrap();
        let again = block_on(ws.read(a)).unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        let (text, doc) = block_on(ws.load(a)).unwrap();
        assert_eq!(text, first.text);
        assert_eq!(doc.body, first.doc.body);
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
