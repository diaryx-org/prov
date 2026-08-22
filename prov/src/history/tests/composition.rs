//! What `prov` supplies to the skiplist, witnessed through a real workspace
//! and a real historica store.

use std::path::Path;

use prov_history::{SkipHost, Standing, apply, historica};

use super::support::*;

#[test]
fn the_skiplist_scopes_recording_to_the_graph() {
    let dir = seed("scopes");
    historica::store::Store::init(dir.join("history")).unwrap();

    let ws = ws(&dir);
    let standing = Standing::read(&dir).unwrap();
    let plan = block_on(ws.skiplist(Path::new("index.md"), &standing)).unwrap();
    apply(&dir, &plan).unwrap();

    // The rule is drawn from the real reachable walk, and lands in a file
    // historica itself reads back.
    let store = historica::store::Store::open(dir.join("history")).unwrap();
    assert!(
        store.skipped().skips("notes/loose.md"),
        "the loose note is not skipped"
    );
    assert!(!store.skipped().skips("notes/a.md"));
    assert!(!store.skipped().skips("index.md"));
}

#[test]
fn the_bin_and_the_derived_page_are_bookkeeping() {
    let dir = tempdir("bookkeeping");
    write(
        &dir,
        "index.md",
        "---\ntitle: Home\nrecycle_bin: recyclebin/index.md\nabout: about.md\n---\nroot\n",
    );
    write(&dir, "recyclebin/index.md", "deleted: []\n");
    write(&dir, "about.md", "---\ntitle: About\n---\nderived\n");

    let ws = ws(&dir);
    let prefixes = block_on(SkipHost::bookkeeping(&ws, Path::new("index.md"))).unwrap();

    assert_eq!(
        prefixes,
        [
            Path::new("recyclebin/items").to_path_buf(),
            Path::new("about.md").to_path_buf()
        ]
    );
}

#[test]
fn a_manifest_claim_answers_for_its_directory() {
    let dir = tempdir("claimed");
    write(&dir, "index.md", "---\ntitle: Home\n---\nroot\n");
    write(&dir, "photos/a.jpg", "JPEGBYTES");

    let mut workspace = ws(&dir);
    block_on(workspace.attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();

    assert!(block_on(SkipHost::claimed(&workspace, Path::new("photos"))).unwrap());
    assert!(!block_on(SkipHost::claimed(&workspace, Path::new("notes"))).unwrap());
}

#[test]
fn a_historica_store_is_parked_out_of_every_walk() {
    let dir = seed("parked");
    historica::store::Store::init(dir.join("history")).unwrap();

    let workspace = ws(&dir);
    let parked = block_on(workspace.parked_dirs(Path::new("index.md"))).unwrap();
    assert!(parked.contains(&Path::new("history").to_path_buf()));

    // Parked means unreached: the store's own documents never enter the
    // reachable set, so nothing indexes, orphan-sweeps, or records them.
    let reachable = block_on(workspace.reachable_files("index.md")).unwrap();
    assert!(reachable.iter().all(|p| !p.starts_with("history")));

    // And a folder merely called `history` is content like any other.
    let plain = seed("parked-plain");
    write(
        &plain,
        "history/essay.md",
        "---\ntitle: Essay\n---\nprose\n",
    );
    let workspace = ws(&plain);
    let parked = block_on(workspace.parked_dirs(Path::new("index.md"))).unwrap();
    assert!(!parked.contains(&Path::new("history").to_path_buf()));
}

#[test]
fn a_workspace_with_a_store_checks_clean() {
    let dir = seed("check-clean");
    historica::store::Store::init(dir.join("history")).unwrap();

    let ws = ws(&dir);
    let standing = Standing::read(&dir).unwrap();
    let plan = block_on(ws.skiplist(Path::new("index.md"), &standing)).unwrap();
    apply(&dir, &plan).unwrap();

    // The store's interior — marker file, skipped.txt, empty shards — raises
    // nothing: not an orphan, not an unreadable document, not a loose file.
    let findings = block_on(ws.check("index.md")).unwrap();
    let about_store: Vec<_> = findings
        .iter()
        .filter(|f| f.subject().starts_with("history"))
        .collect();
    assert!(about_store.is_empty(), "{about_store:?}");
}
