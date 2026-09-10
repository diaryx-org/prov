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
    /// The **workspace node** found by convention, relative to `root_dir`, and
    /// any others it shadowed. See [`node`](crate::node).
    pub node: crate::node::Located,
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
/// `readme` (a *separated* root's node) — with metadata, no `part_of`, and no
/// prov byline marking it as generated (see [`is_root_candidate`]). A file
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
        // The workspace node, read before the root is chosen — which is the
        // whole point of finding it by convention rather than through the
        // root's `config` pointer. Free: the listing is already in hand.
        let located = crate::node::locate_in(fs, dir, &entries).await;
        if let Some(root_doc) = node_named_root(fs, dir, &located).await {
            let discovered = build(fs, dir.to_path_buf(), root_doc, located).await?;
            return Ok(Discovery::Found(discovered));
        }

        let shaped: Vec<PathBuf> = entries
            .iter()
            .map(|entry| entry.path().to_path_buf())
            .filter(|path| can_be_root(path))
            .collect();

        // Probe in the same priority order `choose_root` applies. Once a valid
        // `index` is found, no other file can change the answer; once all index
        // variants have failed, the same is true of a valid `readme`. Large,
        // flat workspaces therefore open one conventional root document rather
        // than every prose file in the directory on every cold start.
        for preferred in ["index", "readme"] {
            for path in shaped.iter().filter(|path| stem_is(path, preferred)) {
                if root_candidate_name(fs, path).await.is_some() {
                    let root_doc = path.file_name().expect("candidate has a filename");
                    let discovered =
                        build(fs, dir.to_path_buf(), PathBuf::from(root_doc), located).await?;
                    return Ok(Discovery::Found(discovered));
                }
            }
        }

        let mut candidates: Vec<String> = Vec::new();
        for path in shaped
            .iter()
            .filter(|path| !stem_is(path, "index") && !stem_is(path, "readme"))
        {
            if let Some(name) = root_candidate_name(fs, path).await {
                candidates.push(name);
            }
        }
        match choose_root(&candidates) {
            Some(root_doc) => {
                let discovered =
                    build(fs, dir.to_path_buf(), PathBuf::from(root_doc), located).await?;
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

/// The filename when `path` is a readable root candidate, otherwise nothing.
/// Kept separate so priority-ordered discovery does not duplicate the parsing
/// half of the root-candidate test.
async fn root_candidate_name<FS: Storage>(fs: &FS, path: &Path) -> Option<String> {
    let text = fs.read_to_string(path).await.ok()?;
    let doc = Document::parse(path, &text).ok()?;
    is_root_candidate(&doc)
        .then(|| path.file_name()?.to_str().map(str::to_owned))
        .flatten()
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

/// The other half: a root document has metadata, says nothing contains it, and
/// is not a page prov itself derived.
///
/// The third clause is what keeps a workspace from being bricked by its own
/// `about` page. Generated prose is machinery-shaped — reached one way from the
/// root, carrying no `part_of` and no id (spec §4) — and that is *precisely* the
/// shape of a root candidate. In a workspace whose root is stemmed `index` or
/// `readme` the tie breaks by name and nothing is noticed; in one whose root is
/// named anything else, `prov about` writes a second candidate into the root
/// directory and every later command refuses to guess which is the root. The
/// page cannot say `part_of` — the spec forbids the back-link, and prov would
/// have to census the page as a tree member — so what settles it is the byline
/// it already carries: a file prov generated is derived *from* the root and so
/// can never be the root.
///
/// The second clause asks whether the *key is present*, not whether it resolves
/// to anything here, and that is deliberate. A workspace inside a workspace says
/// what contains it with a foreign `part_of` on its root
/// (`docs/reference-styles.md`), and the escape it uses is
/// [`node_named_root`] — the node names the root, so nothing has to be guessed.
/// Widening this test instead would hand every directory a candidate it did not
/// have, turning a settled root into a tie; a document with a foreign parent
/// that no node names is therefore not a candidate.
fn is_root_candidate(doc: &Document) -> bool {
    doc.has_meta() && doc.meta.get("part_of").is_none() && !crate::about::is_generated(&doc.meta)
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
        // What the workspace node named, if it named one and the file is there.
        // Trusted without the candidate test: that test exists to *guess* which
        // document is the root, and guessing is over once the workspace says.
        // A `root` naming a document that carries `part_of` is a contradiction
        // for `check` to report, not a reason to go back to guessing; a `root`
        // naming nothing at all falls through, because a typo must not leave the
        // workspace unopenable.
        if let Some(named) = self.named_root().map(Path::to_path_buf)
            && let Ok((_, doc)) = self.load(&named).await
            && doc.has_meta()
        {
            return Ok(Some(named));
        }
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
            if is_root_candidate(&doc) {
                candidates.push(name.to_string());
            }
        }
        Ok(choose_root(&candidates).map(PathBuf::from))
    }
}

/// The root document the workspace node names, when it names one that is
/// actually there.
///
/// The value is taken through [`WorkspaceConfig::apply`] rather than read
/// straight off the mapping, so a malformed `root` is ignored here in exactly
/// the way [`diagnose`](crate::config::diagnose) says it was — one validation,
/// not two that can drift.
///
/// The candidate test is not applied — see [`Workspace::root_document`] for why
/// — which is what lets a **workspace inside a workspace** exist: a named root
/// may carry a `part_of` naming a document in *another* workspace, the way a
/// sub-workspace says what contains it. A local parent on a named root is still
/// a contradiction, and `check` reports it
/// ([`NamedRootContained`](crate::validate::Finding::NamedRootContained)).
///
/// `None` falls through to the candidate scan. That is the safe direction for
/// every way this can go wrong: a `root` naming a file that was moved or
/// misspelled leaves the workspace discoverable as it was before the key was
/// written, and `check` reports the dangling name. Refusing instead would mean
/// a one-character typo in a config file locks the workspace shut.
async fn node_named_root<FS: Storage>(
    fs: &FS,
    dir: &Path,
    located: &crate::node::Located,
) -> Option<PathBuf> {
    let node = located.node.as_ref()?;
    let text = fs.read_to_string(&dir.join(node)).await.ok()?;
    let doc = Document::parse(node, &text).ok()?;
    let mut config = WorkspaceConfig::default();
    config.apply(&doc.meta);
    let named = PathBuf::from(config.root?);
    // Present and readable as a document is the whole test — see
    // `Workspace::root_document` for why the candidate test is not applied to a
    // root the workspace has named outright.
    let text = fs.read_to_string(&dir.join(&named)).await.ok()?;
    Document::parse(&named, &text)
        .ok()
        .filter(Document::has_meta)
        .map(|_| named)
}

/// Assemble the [`Discovered`] for a chosen root: resolve the registry pointer
/// and layer the effective config, through a probe workspace rooted at
/// `root_dir`.
///
/// The layering is defaults → the root's `prov:` block → the **workspace node**
/// → the config document the root points at. The two policy homes of spec §1
/// rule 3 share a rung, and an explicit pointer wins over a convention: a
/// workspace that went to the trouble of naming its config document meant that
/// one. In the ordinary case they are the same file and the order cannot be
/// observed; where they differ, `check` reports it rather than letting the
/// precedence quietly decide.
async fn build<FS: Storage + Clone>(
    fs: &FS,
    root_dir: PathBuf,
    root_doc: PathBuf,
    node: crate::node::Located,
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
    // The workspace node, found by convention rather than pointed at.
    if let Some(node_doc) = &node.node
        && let Ok(text) = fs.read_to_string(&root_dir.join(node_doc)).await
        && let Ok(doc) = Document::parse(node_doc, &text)
    {
        config.apply(&doc.meta);
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
        node,
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

    /// A probe built from what `discover` worked out — the shape a real caller
    /// has, and the only one whose `root_document` can honor a named root.
    fn probe_with_node(dir: &Path) -> Workspace<StdFs> {
        let config = match block_on(discover(&StdFs, dir)).unwrap() {
            Discovery::Found(d) => d.config,
            other => panic!("expected a discovered workspace, got {other:?}"),
        };
        Workspace::builder(StdFs)
            .root(dir)
            .settings((&config).into())
            .build()
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
    fn the_generated_page_is_never_a_root_candidate() {
        // The bug this pins: a workspace whose root is named anything but
        // `index`/`readme` used to brick itself the first time it wrote its own
        // `about` page. The page carries metadata and — as spec §4 requires —
        // no `part_of`, so it tied with the real root and every later command
        // refused to guess between them.
        let root = tmp("generated-page");
        std::fs::write(
            root.join("root.md"),
            "---\ntitle: Home\nabout: about.md\n---\n",
        )
        .unwrap();
        std::fs::write(
            root.join("about.md"),
            "---\ntitle: How this workspace is organized\ngenerated_by: prov 0.5.0\n---\n",
        )
        .unwrap();

        assert_eq!(
            block_on(probe(&root).root_document()).unwrap(),
            Some(PathBuf::from("root.md"))
        );
        match block_on(discover(&StdFs, &root)).unwrap() {
            Discovery::Found(d) => assert_eq!(d.root_doc, PathBuf::from("root.md")),
            other => panic!("expected root.md, got {other:?}"),
        }
    }

    #[test]
    fn another_tool_s_byline_does_not_disqualify_a_root() {
        // Only *prov's* byline is read as "derived from the root". A README a
        // site generator stamped is still an ordinary document, and excluding it
        // would be a second way to lose a workspace's root.
        let root = tmp("foreign-byline");
        std::fs::write(
            root.join("readme.md"),
            "---\ntitle: Home\ngenerated_by: some-site-generator 2.0\n---\n",
        )
        .unwrap();
        assert_eq!(
            block_on(probe(&root).root_document()).unwrap(),
            Some(PathBuf::from("readme.md"))
        );
    }

    #[test]
    fn a_named_root_settles_a_directory_that_cannot_be_chosen_in() {
        // The escape the `.prov` pointer was invented for and never provided.
        // Two unnamed candidates is `Ambiguous`; a node that says which is the
        // root makes it ordinary.
        let root = tmp("named-root-tie");
        std::fs::write(root.join("one.md"), "---\ntitle: One\n---\n").unwrap();
        std::fs::write(root.join("two.md"), "---\ntitle: Two\n---\n").unwrap();
        match block_on(discover(&StdFs, &root)).unwrap() {
            Discovery::Ambiguous { .. } => {}
            other => panic!("expected a tie before the node exists, got {other:?}"),
        }

        std::fs::write(root.join("prov.yaml"), "root: two.md\n").unwrap();
        match block_on(discover(&StdFs, &root)).unwrap() {
            Discovery::Found(d) => {
                assert_eq!(d.root_doc, PathBuf::from("two.md"));
                assert_eq!(d.node.node, Some(PathBuf::from("prov.yaml")));
            }
            other => panic!("expected two.md, got {other:?}"),
        }
        assert_eq!(
            block_on(probe_with_node(&root).root_document()).unwrap(),
            Some(PathBuf::from("two.md")),
            "a located workspace makes the same judgment"
        );
    }

    #[test]
    fn a_named_root_beats_the_conventional_stem() {
        // `index` wins the scan, but the scan is a guess and the node is not.
        let root = tmp("named-root-wins");
        std::fs::write(root.join("index.md"), "---\ntitle: Index\n---\n").unwrap();
        std::fs::write(root.join("home.md"), "---\ntitle: Home\n---\n").unwrap();
        std::fs::write(root.join("prov.yaml"), "root: home.md\n").unwrap();
        match block_on(discover(&StdFs, &root)).unwrap() {
            Discovery::Found(d) => assert_eq!(d.root_doc, PathBuf::from("home.md")),
            other => panic!("expected home.md, got {other:?}"),
        }
    }

    #[test]
    fn a_named_root_that_is_not_there_falls_back_to_the_scan() {
        // A typo must not lock the workspace shut. `check` reports the dangling
        // name; discovery answers as it did before the key was written.
        let root = tmp("named-root-dangling");
        std::fs::write(root.join("index.md"), "---\ntitle: Index\n---\n").unwrap();
        std::fs::write(root.join("prov.yaml"), "root: hoem.md\n").unwrap();
        match block_on(discover(&StdFs, &root)).unwrap() {
            Discovery::Found(d) => assert_eq!(d.root_doc, PathBuf::from("index.md")),
            other => panic!("expected the scan's answer, got {other:?}"),
        }
    }

    #[test]
    fn a_malformed_named_root_is_ignored_like_a_malformed_workspace_id() {
        // A path is not a bare file name, so `apply` drops it — and discovery
        // must not half-honor it by stripping to the last segment, which would
        // agree to a root this directory may not hold.
        let root = tmp("named-root-malformed");
        std::fs::write(root.join("index.md"), "---\ntitle: Index\n---\n").unwrap();
        std::fs::write(root.join("prov.yaml"), "root: docs/index.md\n").unwrap();
        match block_on(discover(&StdFs, &root)).unwrap() {
            Discovery::Found(d) => assert_eq!(d.root_doc, PathBuf::from("index.md")),
            other => panic!("expected the scan's answer, got {other:?}"),
        }
    }

    #[test]
    fn the_node_is_policy_without_the_root_pointing_at_it() {
        // The circle rule 3 could not close: the root names no `config`, and the
        // node is read anyway.
        let root = tmp("node-policy");
        std::fs::write(root.join("index.md"), "---\ntitle: Home\n---\n").unwrap();
        std::fs::write(root.join("prov.yaml"), "workspace_id: notes\n").unwrap();
        match block_on(discover(&StdFs, &root)).unwrap() {
            Discovery::Found(d) => {
                assert_eq!(d.config.workspace_id, "notes");
                assert_eq!(d.node.node, Some(PathBuf::from("prov.yaml")));
            }
            other => panic!("expected a discovered workspace, got {other:?}"),
        }
    }

    #[test]
    fn a_node_under_config_is_read_the_same_way() {
        let root = tmp("node-under-config");
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::write(root.join("index.md"), "---\ntitle: Home\n---\n").unwrap();
        std::fs::write(root.join("config/prov.yaml"), "workspace_id: notes\n").unwrap();
        match block_on(discover(&StdFs, &root)).unwrap() {
            Discovery::Found(d) => {
                assert_eq!(d.config.workspace_id, "notes");
                assert_eq!(d.node.node, Some(PathBuf::from("config/prov.yaml")));
            }
            other => panic!("expected a discovered workspace, got {other:?}"),
        }
    }

    #[test]
    fn a_pointed_config_document_outranks_the_node() {
        // Two policy homes on one rung, and the explicit pointer wins: a
        // workspace that named its config document meant that one. `check`
        // reports the disagreement rather than letting this decide quietly.
        let root = tmp("node-vs-pointer");
        std::fs::write(
            root.join("index.md"),
            "---\ntitle: Home\nconfig: settings.yaml\n---\n",
        )
        .unwrap();
        std::fs::write(root.join("prov.yaml"), "workspace_id: from_node\n").unwrap();
        std::fs::write(root.join("settings.yaml"), "workspace_id: from_pointer\n").unwrap();
        match block_on(discover(&StdFs, &root)).unwrap() {
            Discovery::Found(d) => assert_eq!(d.config.workspace_id, "from_pointer"),
            other => panic!("expected a discovered workspace, got {other:?}"),
        }
    }

    #[test]
    fn a_workspace_with_no_node_discovers_exactly_as_before() {
        let root = tmp("no-node");
        std::fs::write(root.join("index.md"), "---\ntitle: Home\n---\n").unwrap();
        match block_on(discover(&StdFs, &root)).unwrap() {
            Discovery::Found(d) => {
                assert_eq!(d.root_doc, PathBuf::from("index.md"));
                assert_eq!(d.node, crate::node::Located::default());
            }
            other => panic!("expected index.md, got {other:?}"),
        }
    }

    /// An outer workspace holding a `sub/` directory that is a workspace in its
    /// own right: `sub/prov.yaml` names `sub/README.md` as the root, and that
    /// README says `part_of` an id in *another* workspace. Returns the outer
    /// root directory.
    fn nested_workspaces(tag: &str, with_node: bool) -> PathBuf {
        let outer = tmp(tag);
        std::fs::write(outer.join("index.md"), "---\ntitle: Outer\n---\n").unwrap();
        std::fs::create_dir_all(outer.join("sub")).unwrap();
        std::fs::write(
            outer.join("sub/README.md"),
            "---\ntitle: Inner\npart_of: id:outer/abc123\n---\n",
        )
        .unwrap();
        if with_node {
            std::fs::write(
                outer.join("sub/prov.yaml"),
                "workspace_id: inner\nroot: README.md\n",
            )
            .unwrap();
        }
        outer
    }

    #[test]
    fn a_named_root_may_say_what_contains_it() {
        // A workspace inside a workspace. The node names the root, so discovery
        // never asks the candidate test — and the root's `part_of` is a foreign
        // id, which names nothing here. Walking up from inside `sub/` stops at
        // the inner root and reads the inner workspace's own policy.
        let outer = nested_workspaces("nested-named", true);
        std::fs::create_dir_all(outer.join("sub/deep")).unwrap();

        match block_on(discover(&StdFs, &outer.join("sub/deep"))).unwrap() {
            Discovery::Found(d) => {
                assert_eq!(d.root_dir, outer.join("sub"));
                assert_eq!(d.root_doc, PathBuf::from("README.md"));
                assert_eq!(d.config.workspace_id, "inner");
            }
            other => panic!("expected the inner root, got {other:?}"),
        }
    }

    #[test]
    fn a_foreign_parent_alone_does_not_make_a_root() {
        // The other half of the rule, and the reason `is_root_candidate` did not
        // have to change: a document with a foreign `part_of` that *no node
        // names* is still not a candidate. Without `sub/prov.yaml` the walk
        // passes straight over `sub/README.md` and roots at the outer workspace.
        let outer = nested_workspaces("nested-anonymous", false);

        match block_on(discover(&StdFs, &outer.join("sub"))).unwrap() {
            Discovery::Found(d) => {
                assert_eq!(d.root_dir, outer);
                assert_eq!(d.root_doc, PathBuf::from("index.md"));
            }
            other => panic!("expected the outer root, got {other:?}"),
        }
    }

    #[test]
    fn root_document_honors_a_named_root_that_says_what_contains_it() {
        // The same judgment asked of the sub-workspace already located: the
        // named root is trusted, foreign parent and all.
        let outer = nested_workspaces("nested-root-doc", true);
        let inner = outer.join("sub");
        assert_eq!(
            block_on(probe_with_node(&inner).root_document()).unwrap(),
            Some(PathBuf::from("README.md"))
        );
        // And without the node there is nothing here to be the root.
        let bare = nested_workspaces("nested-root-doc-bare", false).join("sub");
        assert_eq!(block_on(probe(&bare).root_document()).unwrap(), None);
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
