//! The library's public API, exercised as an *external* consumer sees it.
//!
//! Everything here reaches prov only through its published surface (`use
//! prov::…`) — no `pub(crate)` internals, no test-only backends. That is the
//! point: the in-crate unit tests can lean on `FailAtWrite` and friends, but a
//! downstream embedder cannot, so this file proves the exported types are enough
//! to *drive* a workspace, that they are `Send` (usable from a real multi-threaded
//! async runtime), and that the one failure the unit tests can't reach — a
//! rollback that itself faults, [`prov::Error::Torn`] — is reachable and
//! reported through nothing but the public [`prov::Storage`] trait.

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};

use prov::config::{FieldSpec, OpenClosed};
use prov::fs::{DirEntry, Metadata};
use prov::{
    Capabilities, ChangeSet, Discovery, Document, Durability, Error, InMemoryFs, InMemoryIndex,
    Minter, ReadStorage, RelationSet, StdFs, Storage, Vocabulary, Workspace, block_on,
};

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("prov-pubapi-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ───────────────────────── driving a workspace ──────────────────────────────

#[test]
fn a_workspace_can_be_built_and_traversed_through_the_public_api() {
    let root = tmp("drive");
    std::fs::write(root.join("index.md"), "---\ntitle: Home\n---\n# Home\n").unwrap();
    std::fs::write(
        root.join("child.md"),
        "---\ntitle: Child\npart_of: '[Home](/index.md)'\n---\n",
    )
    .unwrap();
    // The root links its child, so the spanning tree has two nodes.
    std::fs::write(
        root.join("index.md"),
        "---\ntitle: Home\ncontents:\n- '[Child](/child.md)'\n---\n# Home\n",
    )
    .unwrap();

    let ws = Workspace::builder(StdFs)
        .root(&root)
        .relations(RelationSet::diaryx())
        .build();
    let node = block_on(ws.tree("index.md")).expect("tree");
    assert_eq!(node.children.len(), 1, "root reaches its one child");

    // `check` returns findings as public `Finding` values; a well-formed pair is clean.
    let findings = block_on(ws.check("index.md")).expect("check");
    assert!(
        findings.is_empty(),
        "consistent workspace has no findings: {findings:?}"
    );

    // The bounded counterpart to the walk, and the `Document` it hands back:
    // both have to be nameable from out here, or the API is only usable by the
    // crate that defined it.
    let children: Vec<(PathBuf, Document)> =
        block_on(ws.spanning_children("index.md")).expect("children");
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].0, Path::new("child.md"));
    assert_eq!(
        children[0].1.meta.get("title").and_then(|v| v.as_str()),
        Some("Child"),
        "the document arrives parsed, so a caller need not read it again"
    );
}

/// A reified vocabulary is read from out here the way a schema-aware frontend
/// reads one: declare the field, load the term set, then follow a value to the
/// node so the tier-3 payload hanging off the term is reachable at all. That
/// second step is the whole reason to reify, so it has to be public.
#[test]
fn a_reified_vocabulary_is_loadable_and_its_terms_are_reachable_as_nodes() {
    let root = tmp("reified-vocab");
    std::fs::write(
        root.join("index.md"),
        "---\ntitle: Home\ncontents:\n- vocab/index.md\n---\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("vocab")).unwrap();
    std::fs::write(
        root.join("vocab/index.md"),
        "---\ntitle: Audiences\npart_of: /index.md\ncontents:\n- friends.md\n---\n",
    )
    .unwrap();
    std::fs::write(
        root.join("vocab/friends.md"),
        "---\ntitle: Friends\nterm: friends\npart_of: index.md\ngate: circle:friends\n---\n",
    )
    .unwrap();

    let ws = Workspace::builder(StdFs)
        .root(&root)
        .relations(RelationSet::diaryx())
        .build();
    let spec = FieldSpec {
        ty: None,
        values: OpenClosed::Closed,
        vocabulary: Some("vocab/index.md".into()),
        reify: true,
    };
    let vocab: Vocabulary =
        block_on(ws.load_reified_vocabulary(Path::new("index.md"), "audience", &spec))
            .expect("load")
            .expect("a reified vocabulary");
    assert!(vocab.accepts("friends"), "{:?}", vocab.terms);

    let term_node =
        block_on(ws.reified_term_path(Path::new("index.md"), "vocab/index.md", "friends"))
            .expect("term path")
            .expect("the node declaring the term");
    assert_eq!(term_node, Path::new("vocab/friends.md"));
    // Payload prov carries and never reads, read by the consumer it is for.
    let node = block_on(ws.document(&term_node)).expect("document");
    assert_eq!(
        node.meta.get("gate").and_then(|v| v.as_str()),
        Some("circle:friends")
    );
}

#[test]
fn a_change_set_lands_through_the_public_api() {
    let root = tmp("changeset");
    let mut cs = ChangeSet::new();
    cs.write("a.md", "---\ntitle: A\n---\n");
    cs.write("sub/b.md", "---\ntitle: B\n---\n");
    block_on(cs.apply(&StdFs, &root)).expect("apply");
    assert!(root.join("a.md").exists());
    assert!(root.join("sub/b.md").exists());
}

#[test]
fn discovery_locates_a_root_through_the_public_api() {
    let root = tmp("discover");
    std::fs::write(root.join("index.md"), "---\ntitle: Home\n---\n").unwrap();
    std::fs::create_dir_all(root.join("deep")).unwrap();
    match block_on(prov::discover(&StdFs, &root.join("deep"))).expect("discover") {
        Discovery::Found(d) => assert_eq!(d.root_doc, Path::new("index.md")),
        other => panic!("expected Found, got {other:?}"),
    }
}

#[test]
fn a_path_escaping_the_root_is_refused_by_apply() {
    // The workspace-clamp guard, from a public caller: a staged op that climbs out
    // of the root is refused with the typed `Escape` variant, and nothing lands.
    let root = tmp("escape");
    let mut cs = ChangeSet::new();
    cs.write("../escapee.md", "should never be written");
    // `apply` answers in `fs_transaction`'s vocabulary; a prov caller reaches
    // prov's through the `From` impl, which is what `?` would do for them.
    let err: Error = block_on(cs.apply(&StdFs, &root)).unwrap_err().into();
    assert!(
        matches!(err, Error::Escape(_)),
        "expected Escape, got {err:?}"
    );
    assert!(!root.parent().unwrap().join("escapee.md").exists());
}

// ───────────────────────── InMemoryFs and the borrow blanket impls ──────────

#[test]
fn in_memory_fs_drives_a_workspace_through_the_public_api() {
    let fs = InMemoryFs::new();
    block_on(fs.write(Path::new("index.md"), b"---\ntitle: Home\n---\n# Home\n")).unwrap();

    let ws = Workspace::builder(fs)
        .root(Path::new(""))
        .relations(RelationSet::diaryx())
        .build();
    let node = block_on(ws.tree("index.md")).expect("tree");
    assert_eq!(node.title.as_deref(), Some("Home"));
}

// The `&fs` receivers below are not redundant despite what `needless_borrow`
// thinks: method resolution stops at the first type in the autoderef chain
// with a match, so calling through a value already typed `&InMemoryFs`
// resolves to the blanket `impl Storage for &S`, while calling on `fs`
// directly would resolve to `InMemoryFs`'s own impl instead. The whole point
// of this test is to prove the former compiles and behaves identically to the
// latter, so the borrow must stay.
#[allow(clippy::needless_borrow)]
#[test]
fn a_borrowed_storage_backend_is_itself_storage() {
    // The blanket `impl<S: Storage + ?Sized> Storage for &S` is what lets an
    // owned backend be lent to something generic over `S: Storage` — proven
    // here by driving reads and writes through `&InMemoryFs` rather than the
    // owned value, and by checking that the *real* capabilities come through
    // rather than the trait's pessimistic defaults.
    let fs = InMemoryFs::new();
    block_on((&fs).write(Path::new("doc.md"), b"hello")).unwrap();
    assert_eq!(
        block_on((&fs).read_to_string(Path::new("doc.md"))).unwrap(),
        "hello"
    );
    assert_eq!((&fs).capabilities(), Capabilities::IN_MEMORY);
}

#[test]
fn an_arc_wrapped_storage_backend_is_itself_storage() {
    let fs = std::sync::Arc::new(InMemoryFs::new());
    block_on(fs.write(Path::new("doc.md"), b"hello")).unwrap();
    assert_eq!(
        block_on(fs.read_to_string(Path::new("doc.md"))).unwrap(),
        "hello"
    );
    assert_eq!(fs.capabilities(), Capabilities::IN_MEMORY);
}

#[test]
fn a_borrowed_backend_can_build_a_workspace_and_is_still_usable_afterward() {
    // The motivating use case from the migration this blanket impl exists
    // for: a caller holds an owned backend and wants to lend it to a
    // temporary `Workspace` without moving it or wrapping it in an `Arc` it
    // doesn't otherwise need.
    let fs = InMemoryFs::new();
    block_on(fs.write(Path::new("index.md"), b"---\ntitle: Home\n---\n# Home\n")).unwrap();

    let ws = Workspace::builder(&fs)
        .root(Path::new(""))
        .relations(RelationSet::diaryx())
        .build();
    let node = block_on(ws.tree("index.md")).expect("tree");
    assert_eq!(node.title.as_deref(), Some("Home"));

    // `fs` was only borrowed, not consumed — it's still usable here.
    assert!(block_on(fs.try_exists(Path::new("index.md"))).unwrap());
}

// ───────────────────────── Send-ness ────────────────────────────────────────
//
// prov's exported *values* and its non-recursive futures must be `Send`, so
// an embedder can move a workspace between threads and drive the transactional and
// discovery entry points from a multi-threaded async runtime. A regression (an
// internal `Rc`, a non-`Send` guard held across an `.await`) would surface here as
// a compile error rather than a mysterious downstream one.
//
// The one deliberate exception is the *recursive traversal* (`tree`/`check` and
// the scans they drive): those box their futures as `Pin<Box<dyn Future>>` without
// a `+ Send` bound, so they are not `Send`. prov runs them through its own
// single-threaded [`prov::block_on`], which never required it, and adding the
// bound would force a `Sync` constraint down through `load` and most of the
// mutation/validation surface for no benefit to the executor that exists. This
// test pins that boundary explicitly: everything below is asserted `Send`, and the
// traversal futures are knowingly outside it.

fn assert_send<T: Send>() {}
fn require_send_future<F: Future + Send>(_: F) {}

#[test]
fn public_types_are_send() {
    assert_send::<Workspace<StdFs>>();
    assert_send::<Workspace<StdFs, Minter, InMemoryIndex>>();
    assert_send::<ChangeSet>();
    assert_send::<Error>();
    assert_send::<prov::Discovered>();
    assert_send::<prov::Node>();
}

/// Compile-time only: never called, but type-checked. If either the transactional
/// `apply` or the `discover` future stopped being `Send`, this would fail to
/// compile — the guarantee enforced at build time. (`tree`/`check` are
/// deliberately absent; see the module comment above.)
#[allow(dead_code)]
async fn futures_stay_send(fs: &StdFs, cs: ChangeSet) {
    require_send_future(cs.apply(fs, Path::new(".")));
    require_send_future(prov::discover(fs, Path::new(".")));
}

// ───────────────────────── the Torn path ────────────────────────────────────

/// A public [`Storage`] backend that drives [`ChangeSet::apply`] into
/// [`Error::Torn`] — the one outcome the in-crate tests never reach, because it
/// needs *two* faults: a write that fails (triggering rollback) and a rollback
/// that fails too (leaving prov unable to say what is on disk).
///
/// It wraps a real [`StdFs`] and fails exactly two writes by their final path:
/// the second op's landing (so op-1's undo runs) and op-1's *restore* write (so
/// the undo itself faults). Op-1's atomic staging write goes to a `prov-tmp`
/// sibling, which is left alone — only the direct restore write to op-1's own
/// path is failed, which is precisely the rollback step.
#[derive(Clone)]
struct DoubleFault {
    /// The op-1 path whose restore write must fail (its undo step).
    restore_victim: PathBuf,
    /// A filename fragment identifying the op-2 landing write to fail.
    boom_fragment: String,
}

impl DoubleFault {
    fn should_fail(&self, path: &Path) -> bool {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        // Op-1's restore is a *direct* write to its path (not the atomic staging
        // sibling, which carries a `prov-tmp` marker in its name).
        let is_restore =
            path.file_name() == self.restore_victim.file_name() && !name.contains("prov-tmp");
        let is_boom = name.contains(&self.boom_fragment);
        is_restore || is_boom
    }
}

impl ReadStorage for DoubleFault {
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        StdFs.read(path).await
    }
    async fn read_to_string(&self, path: &Path) -> io::Result<String> {
        StdFs.read_to_string(path).await
    }
    async fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        StdFs.read_dir(path).await
    }
    async fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        StdFs.metadata(path).await
    }
    async fn executable(&self, path: &Path) -> io::Result<Option<bool>> {
        StdFs.executable(path).await
    }
    async fn read_link(&self, path: &Path) -> io::Result<Option<PathBuf>> {
        StdFs.read_link(path).await
    }
}

impl Storage for DoubleFault {
    async fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        if self.should_fail(path) {
            return Err(io::Error::other("double fault (test)"));
        }
        StdFs.write(path, contents).await
    }
    // Delegated, not defaulted: `capabilities` below claims `LOCAL_FS`, whose
    // `exclusive_create` promises a working `create_new` — the default's
    // `Unsupported` refusal would break that promise.
    async fn create_new(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        StdFs.create_new(path, contents).await
    }
    async fn set_executable(&self, path: &Path, executable: bool) -> io::Result<()> {
        StdFs.set_executable(path, executable).await
    }
    async fn set_link(&self, path: &Path, target: &Path) -> io::Result<()> {
        StdFs.set_link(path, target).await
    }
    async fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        StdFs.create_dir_all(path).await
    }
    async fn remove_file(&self, path: &Path) -> io::Result<()> {
        StdFs.remove_file(path).await
    }
    async fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        StdFs.remove_dir_all(path).await
    }
    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        StdFs.rename(from, to).await
    }
    // Report the local-filesystem guarantees so `write_atomic` runs its real
    // staging/rename protocol — the same path a production `StdFs` workspace takes.
    fn capabilities(&self) -> Capabilities {
        Capabilities::LOCAL_FS
    }
    async fn sync(&self, path: &Path, need: Durability) -> io::Result<()> {
        StdFs.sync(path, need).await
    }
}

#[test]
fn a_rollback_that_itself_fails_surfaces_as_torn() {
    let root = tmp("torn");
    // Op-1 overwrites an existing document, so its undo is a *restore* write (the
    // step we will fault). Op-2's write is the one that triggers the rollback.
    std::fs::write(root.join("victim.md"), "original").unwrap();

    let fs = DoubleFault {
        restore_victim: PathBuf::from("victim.md"),
        boom_fragment: "boom".into(),
    };
    let mut cs = ChangeSet::new();
    cs.write("victim.md", "rewritten"); // op-1: lands, then must be rolled back
    cs.write("boom.md", "never lands"); // op-2: its write faults

    // Through prov's own journal, as every prov mutation goes — not the bare
    // `ChangeSet::apply`, whose default journal name prov's recovery does not
    // read.
    let journal = prov::journal::workspace_journal();
    let err: Error = block_on(journal.apply(&cs, &fs, &root)).unwrap_err().into();
    match err {
        Error::Torn { cause, rollback } => {
            assert!(
                cause.contains("double fault"),
                "cause names the write fault: {cause}"
            );
            assert!(
                rollback.contains("double fault"),
                "rollback names its own fault: {rollback}"
            );
        }
        other => panic!("expected Torn, got {other:?}"),
    }

    // Torn keeps the journal so recovery can later roll the set *forward* to the
    // consistent applied state (prov refuses to claim a state it cannot name).
    assert!(
        journal.path_in(&root).exists(),
        "a torn apply leaves its journal for recovery"
    );
}

#[test]
fn provs_journal_is_the_one_provs_recovery_reads() {
    // The coupling that has no compiler to enforce it: a set applied under one
    // journal name and recovered under another fails *silently* — recovery
    // finds nothing and reports success, leaving the change stranded
    // half-applied. Anything in prov that reaches for a bare `ChangeSet::apply`
    // reintroduces exactly that, so pin both ends here.
    assert_eq!(prov::journal::JOURNAL_NAME, ".prov-journal");
    let journal = prov::journal::workspace_journal();
    assert_eq!(journal.name(), prov::journal::JOURNAL_NAME);

    // And it is emphatically not the transaction crate's default, which is what
    // a bare `ChangeSet::apply` would write.
    let default = prov::journal::Journal::default();
    assert_ne!(default.name(), journal.name());

    let root = tmp("journal-agreement");
    std::fs::write(root.join("parent.md"), "old").unwrap();
    let mut cs = ChangeSet::new();
    cs.write("child.md", "child");
    cs.write("parent.md", "new");

    // The on-disk state a crash just after the commit point leaves behind.
    std::fs::write(
        journal.path_in(&root),
        prov::journal::encode(cs.ops()).unwrap(),
    )
    .unwrap();

    assert_eq!(
        block_on(prov::recover(&StdFs, &root)).unwrap(),
        prov::Recovered::Applied(2),
        "prov's recovery must find the journal prov's apply writes"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("parent.md")).unwrap(),
        "new"
    );
}
