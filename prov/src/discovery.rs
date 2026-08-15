//! Root discovery — finding the workspace a directory belongs to.
//!
//! A prov workspace is *self-describing*, so where it begins is a fact to be
//! found, not configured: walk up from a starting directory and the first
//! directory holding a **root document** — one with metadata and no `part_of`
//! (nothing contains it) — is the workspace root. This is the counterpart to the
//! traversal in [`prov_graph::graph::tree`]: that walk goes *down* the spanning tree from a
//! known root; this one goes *up* the filesystem to locate the root in the first
//! place.
//!
//! It lives in the library, over the [`Storage`] seam, rather than in the CLI:
//! discovery is workspace semantics (which file is the root, ties broken by
//! `index`/`readme`, an ambiguous directory refused), not presentation, and a
//! programmatic embedder needs it exactly as the CLI does. The CLI keeps only the
//! thin shell around it — reading the real current directory, and phrasing the
//! "no workspace here" advice — while the judgment lives here and is testable
//! against a fake filesystem.

use std::path::{Path, PathBuf};

use crate::config::{ROOT_CONFIG_KEY, WorkspaceConfig};
use crate::workspace::Workspace;
use prov_graph::content::ContentFormat;
use prov_graph::document::{self, Document};
use prov_graph::error::Result;
use prov_store::fs::Storage;

/// A located workspace: where the root directory is, which document in it is the
/// root, the registry that root declares (if any), and the effective config
/// (defaults, overlaid by the root's `prov:` block, overlaid by the linked
/// config document).
#[derive(Debug, Clone)]
pub struct Discovered {
    /// The workspace root directory (as reached by walking up from the start).
    pub root_dir: PathBuf,
    /// The root document, relative to `root_dir`.
    pub root_doc: PathBuf,
    /// The registry document the root declares, relative to `root_dir`, if any.
    pub registry: Option<PathBuf>,
    /// The effective workspace configuration.
    pub config: WorkspaceConfig,
}

/// The outcome of a [`discover`] walk — one of the three answers "which workspace
/// is this directory in?" genuinely has.
// `Found` carries a whole `WorkspaceConfig` and so dwarfs the other two
// variants — which is the shape this type is *for*: `discover` returns exactly
// one of these, once, and every caller destructures it immediately. Boxing to
// even the variants out would put an allocation in the signature of the
// function every consumer starts with, to save a stack copy on a path taken
// once per process.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum Discovery {
    /// A single unambiguous root was found.
    Found(Discovered),
    /// A directory held two or more root candidates and no `index`/`readme` to
    /// break the tie — prov will not guess which is the root. Carries the
    /// directory and the candidate filenames so a caller can name them.
    Ambiguous {
        /// The directory that held the competing candidates.
        dir: PathBuf,
        /// The candidate filenames, in directory order.
        candidates: Vec<String>,
    },
    /// No ancestor directory held a root document at all.
    NotFound,
}

/// Whether a file `stem` is the conventional root name that wins ties.
fn stem_is(name: &Path, want: &str) -> bool {
    name.file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case(want))
}

/// Walk up from `from` (an absolute directory) and locate the workspace root.
///
/// In each directory a **root candidate** is a document — a content document
/// (Markdown/Djot/HTML), or a whole-file metadata document stemmed `index`/
/// `readme` (a *separated* root's node) — with metadata and no `part_of`. A file
/// stemmed `index` wins, then `readme`, then a lone candidate; two or more
/// unnamed candidates are [`Discovery::Ambiguous`]. The first ancestor with a
/// winner is the root; a walk that reaches the filesystem top with none is
/// [`Discovery::NotFound`].
///
/// `FS: Clone` because the effective config is read through a throwaway probe
/// [`Workspace`] rooted at the found directory (its `registry_path`/`config_path`
/// resolve the pointer relations) — the same machinery every command uses, so
/// discovery and operation agree on where the registry and config live.
pub async fn discover<FS: Storage + Clone>(fs: &FS, from: &Path) -> Result<Discovery> {
    for dir in from.ancestors() {
        let Ok(entries) = fs.read_dir(dir).await else {
            continue;
        };
        let mut candidates: Vec<String> = Vec::new();
        for entry in entries {
            let path = entry.path();
            if !can_be_root(path) {
                continue;
            }
            let Ok(text) = fs.read_to_string(path).await else {
                continue;
            };
            let Ok(doc) = Document::parse(path, &text) else {
                continue;
            };
            if declares_no_parent(&doc)
                && let Some(name) = path.file_name().and_then(|n| n.to_str())
            {
                candidates.push(name.to_string());
            }
        }
        match choose_root(&candidates) {
            Some(root_doc) => {
                let discovered = build(fs, dir.to_path_buf(), PathBuf::from(root_doc)).await?;
                return Ok(Discovery::Found(discovered));
            }
            None if candidates.len() > 1 => {
                return Ok(Discovery::Ambiguous {
                    dir: dir.to_path_buf(),
                    candidates,
                });
            }
            None => continue,
        }
    }
    Ok(Discovery::NotFound)
}

/// Whether `path` is *shaped* like a root document — the cheap half of the test,
/// applied before anything is read.
///
/// A content document (Markdown/Djot/HTML) qualifies. So does a whole-file
/// metadata document (a *separated* root's node, `index.yaml` and friends), but
/// only under the conventional `index`/`readme` stem — otherwise a stray
/// `.json`/`.yaml` config file, which is a mapping at its root and declares no
/// `part_of`, would masquerade as a root.
fn can_be_root(path: &Path) -> bool {
    let is_content_ext = ContentFormat::from_extension(path).is_some();
    let is_meta_ext = document::whole_file_format(path).is_some();
    if is_content_ext {
        return true;
    }
    is_meta_ext && (stem_is(path, "index") || stem_is(path, "readme"))
}

/// The other half: a root document has metadata and says nothing contains it.
fn declares_no_parent(doc: &Document) -> bool {
    doc.has_meta() && doc.meta.get("part_of").is_none()
}

/// Pick the root from a directory's candidates: an `index` stem wins, then
/// `readme`, then a lone candidate. Two or more unnamed candidates are a tie this
/// will not break — prov refuses to guess which is the root.
fn choose_root(candidates: &[String]) -> Option<String> {
    candidates
        .iter()
        .find(|n| stem_is(Path::new(n), "index"))
        .or_else(|| candidates.iter().find(|n| stem_is(Path::new(n), "readme")))
        .cloned()
        .or_else(|| (candidates.len() == 1).then(|| candidates[0].clone()))
}

impl<FS: prov_graph::fs::ReadStorage, Id, Ix: prov_graph::index::IdIndex> Workspace<FS, Id, Ix> {
    /// This workspace's own root document — the same judgment [`discover`] makes,
    /// asked of a workspace already located rather than of a directory being
    /// searched for one. `None` when the root directory holds no candidate, or
    /// holds several with no `index`/`readme` to break the tie.
    ///
    /// Where [`discover`] walks *up* the filesystem to find which workspace a
    /// directory belongs to, this reads one directory — the root this workspace is
    /// already rooted at — so it needs neither `Clone` nor a second config layering
    /// pass. It exists because "walk the spanning relation up to the root" is not a
    /// complete answer to "which document roots this workspace": a document that
    /// declares no `part_of` roots that walk at *itself*, whether it is the root or
    /// merely outside the tree. See
    /// [`spanning_root`](Workspace::spanning_root), which uses this to tell those
    /// two apart.
    pub async fn root_document(&self) -> Result<Option<PathBuf>> {
        let mut candidates = Vec::new();
        for entry in self.listing(Path::new("")).await? {
            if entry.file_type().is_dir() {
                continue;
            }
            // `listing` yields *absolute* paths; every document verb below takes
            // workspace-relative ones, and for entries of the root directory the
            // relative path is exactly the file name.
            let Some(name) = entry.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let path = PathBuf::from(name);
            if !can_be_root(&path) {
                continue;
            }
            let Ok((_, doc)) = self.load(&path).await else {
                continue;
            };
            if declares_no_parent(&doc) {
                candidates.push(name.to_string());
            }
        }
        Ok(choose_root(&candidates).map(PathBuf::from))
    }
}

/// Assemble the [`Discovered`] for a chosen root: resolve the registry pointer
/// and layer the effective config (defaults → root `prov:` block → linked
/// config document), through a probe workspace rooted at `root_dir`.
async fn build<FS: Storage + Clone>(
    fs: &FS,
    root_dir: PathBuf,
    root_doc: PathBuf,
) -> Result<Discovered> {
    let probe: Workspace<FS> = Workspace::builder(fs.clone()).root(&root_dir).build();
    let registry = probe.registry_path(&root_doc).await?;

    let mut config = WorkspaceConfig::default();
    // The root's `prov:` frontmatter block (config's description home).
    if let Ok(text) = fs.read_to_string(&root_dir.join(&root_doc)).await
        && let Ok(doc) = Document::parse(&root_doc, &text)
        && let Some(block) = doc.meta.get(ROOT_CONFIG_KEY)
    {
        config.apply(block);
    }
    // The linked config document (the policy home) wins over the root block.
    if let Ok(Some(config_doc)) = probe.config_path(&root_doc).await
        && let Ok(text) = fs.read_to_string(&root_dir.join(&config_doc)).await
        && let Ok(doc) = Document::parse(&config_doc, &text)
    {
        config.apply(&doc.meta);
    }

    Ok(Discovered {
        root_dir,
        root_doc,
        registry,
        config,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use prov_graph::exec::block_on;
    use prov_graph::fs::StdFs;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prov-discover-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn finds_the_root_by_walking_up_from_a_nested_dir() {
        let root = tmp("walk-up");
        std::fs::write(root.join("index.md"), "---\ntitle: Home\n---\n# Home\n").unwrap();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(
            root.join("a/child.md"),
            "---\ntitle: Child\npart_of: '[Home](/index.md)'\n---\n",
        )
        .unwrap();

        let outcome = block_on(discover(&StdFs, &root.join("a/b"))).unwrap();
        match outcome {
            Discovery::Found(d) => {
                assert_eq!(d.root_dir, root);
                assert_eq!(d.root_doc, Path::new("index.md"));
            }
            other => panic!("expected Found, got {other:?}"),
        }
    }

    #[test]
    fn two_unnamed_candidates_are_ambiguous() {
        let root = tmp("ambiguous");
        std::fs::write(root.join("one.md"), "---\ntitle: One\n---\n").unwrap();
        std::fs::write(root.join("two.md"), "---\ntitle: Two\n---\n").unwrap();
        match block_on(discover(&StdFs, &root)).unwrap() {
            Discovery::Ambiguous { candidates, .. } => assert_eq!(candidates.len(), 2),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn index_stem_breaks_a_tie() {
        let root = tmp("index-wins");
        std::fs::write(root.join("index.md"), "---\ntitle: Home\n---\n").unwrap();
        std::fs::write(root.join("other.md"), "---\ntitle: Other\n---\n").unwrap();
        match block_on(discover(&StdFs, &root)).unwrap() {
            Discovery::Found(d) => assert_eq!(d.root_doc, Path::new("index.md")),
            other => panic!("expected Found, got {other:?}"),
        }
    }

    #[test]
    fn a_directory_holding_no_document_yields_no_candidate_there() {
        // A directory with only non-documents contributes no root candidate, so
        // discovery keeps walking up rather than rooting here. (A full "reaches the
        // filesystem top with nothing" NotFound is covered end-to-end by the CLI's
        // `a_route_outside_a_workspace_says_so` test, which can control the whole
        // ancestor chain; a unit test cannot, since the walk climbs to `/`.)
        let root = tmp("no-doc-here");
        std::fs::write(root.join("plain.txt"), "not a document").unwrap();
        // Rooting *would* happen if this dir had a candidate; assert it does not by
        // giving it a child that IS a root and confirming discovery picks the
        // child's dir, never this one.
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/index.md"), "---\ntitle: Sub\n---\n").unwrap();
        match block_on(discover(&StdFs, &root.join("sub"))).unwrap() {
            Discovery::Found(d) => assert_eq!(d.root_dir, root.join("sub")),
            other => panic!("expected Found at sub, got {other:?}"),
        }
    }

    fn probe(dir: &Path) -> Workspace<StdFs> {
        Workspace::builder(StdFs).root(dir).build()
    }

    #[test]
    fn root_document_names_the_root_of_a_located_workspace() {
        // The same judgment `discover` makes, asked of a workspace already rooted:
        // `index` wins over another parentless document in the same directory, and
        // a child (which declares `part_of`) is not a candidate at all.
        let root = tmp("root-doc");
        std::fs::write(root.join("index.md"), "---\ntitle: Home\n---\n").unwrap();
        // The about page: parentless, so a *candidate*, but `index` outranks it.
        std::fs::write(root.join("about.md"), "---\ntitle: About\n---\n").unwrap();
        std::fs::write(
            root.join("child.md"),
            "---\ntitle: Child\npart_of: index.md\n---\n",
        )
        .unwrap();
        assert_eq!(
            block_on(probe(&root).root_document()).unwrap(),
            Some(PathBuf::from("index.md"))
        );
    }

    #[test]
    fn root_document_declines_to_guess() {
        // Two unnamed candidates and no `index`/`readme` to break the tie is the
        // one case `discover` refuses; asked this way it answers `None` rather
        // than picking, so a caller falls back instead of acting on a guess.
        let root = tmp("root-doc-tie");
        std::fs::write(root.join("one.md"), "---\ntitle: One\n---\n").unwrap();
        std::fs::write(root.join("two.md"), "---\ntitle: Two\n---\n").unwrap();
        assert_eq!(block_on(probe(&root).root_document()).unwrap(), None);

        // And a directory with no document at all has no root to name.
        let bare = tmp("root-doc-bare");
        std::fs::write(bare.join("plain.txt"), "not a document").unwrap();
        assert_eq!(block_on(probe(&bare).root_document()).unwrap(), None);
    }
}
