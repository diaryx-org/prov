//! The list held to its claims: what gets a rule, what does not, and what the
//! rendered line says.
//!
//! Every fixture here is a **real** workspace — real frontmatter, real links,
//! the real reachable walk — because the whole computation is a subtraction
//! against that walk, and a fixture reachable set would let the two drift
//! apart without a test noticing.

use std::path::{Path, PathBuf};

use prov_graph::exec::block_on;
use prov_graph::fs::StdFs;
use prov_store::index::FileIndex;

use super::{Ignore, IgnoreList, Reason};
use crate::workspace::Workspace;

use prov_testkit::write;
fn tempdir(tag: &str) -> PathBuf {
    prov_testkit::scratch("ignore", tag)
}

fn ws(dir: &Path) -> Workspace<StdFs, crate::identity::Minter, FileIndex> {
    Workspace::builder(StdFs)
        .root(dir)
        .identity(crate::identity::Minter::lazy(42))
        .index(FileIndex::new(fig::Format::Yaml))
        .build()
}

/// A root that lists `children`, each of which claims it back.
fn root(dir: &Path, children: &[&str]) {
    let mut text = String::from("---\ntitle: Home\ncontents:\n");
    for child in children {
        text.push_str(&format!("- {child}\n"));
    }
    text.push_str("---\nroot\n");
    write(dir, "index.md", &text);
}

fn child(dir: &Path, rel: &str, title: &str) {
    let up = "../".repeat(rel.matches('/').count());
    write(
        dir,
        rel,
        format!("---\ntitle: {title}\npart_of: '{up}index.md'\n---\n{title}\n"),
    );
}

fn loose(dir: &Path, rel: &str, title: &str) {
    write(dir, rel, format!("---\ntitle: {title}\n---\nunlinked\n"));
}

fn list(dir: &Path) -> IgnoreList {
    block_on(ws(dir).ignore_list(Path::new("index.md"))).unwrap()
}

fn lines(list: &IgnoreList) -> Vec<String> {
    list.rules.iter().map(Ignore::to_string).collect()
}

#[test]
fn unreached_gets_a_rule_and_reachable_does_not() {
    let dir = tempdir("unreached");
    root(&dir, &["notes/a.md"]);
    child(&dir, "notes/a.md", "A");
    loose(&dir, "notes/loose.md", "Loose");

    let list = list(&dir);

    assert_eq!(lines(&list), ["/notes/loose.md"]);
    assert_eq!(list.rules[0].reason, Reason::Unreached);
}

#[test]
fn a_folder_nothing_reaches_collapses_to_one_rule() {
    let dir = tempdir("collapse");
    root(&dir, &[]);
    loose(&dir, "drafts/one.md", "One");
    loose(&dir, "drafts/deep/two.md", "Two");

    assert_eq!(lines(&list(&dir)), ["/drafts/"]);
}

#[test]
fn a_folder_the_graph_reaches_into_is_ruled_file_by_file() {
    let dir = tempdir("mixed");
    root(&dir, &["notes/a.md"]);
    child(&dir, "notes/a.md", "A");
    loose(&dir, "notes/loose.md", "Loose");
    loose(&dir, "notes/spare.md", "Spare");

    assert_eq!(lines(&list(&dir)), ["/notes/loose.md", "/notes/spare.md"]);
}

#[test]
fn a_hidden_directory_is_ruled_without_being_walked() {
    let dir = tempdir("hidden");
    root(&dir, &[]);
    write(&dir, ".git/objects/aa/bb", "loose object");
    write(&dir, ".git/HEAD", "ref");

    let list = list(&dir);

    assert_eq!(lines(&list), ["/.git/"]);
    assert_eq!(list.rules[0].reason, Reason::Hidden);
}

#[test]
fn a_hidden_directory_the_graph_reaches_into_is_walked() {
    let dir = tempdir("hidden-reached");
    root(&dir, &[".notes/kept.md"]);
    child(&dir, ".notes/kept.md", "Kept");
    loose(&dir, ".notes/loose.md", "Loose");

    assert_eq!(lines(&list(&dir)), ["/.notes/loose.md"]);
}

#[test]
fn a_hidden_file_is_ruled_with_its_own_reason() {
    let dir = tempdir("hidden-file");
    root(&dir, &[]);
    write(&dir, ".envrc", "use flake");

    let list = list(&dir);

    assert_eq!(lines(&list), ["/.envrc"]);
    assert_eq!(list.rules[0].reason, Reason::Hidden);
}

#[test]
fn bookkeeping_is_ruled_where_it_stands_reachable_or_not() {
    let dir = tempdir("bookkeeping");
    write(
        &dir,
        "index.md",
        "---\ntitle: Home\nrecycle_bin: recyclebin/index.md\nabout: about.md\n---\nroot\n",
    );
    write(&dir, "recyclebin/index.md", "deleted: []\n");
    write(&dir, "recyclebin/items/gone.md", "consigned");
    write(&dir, "about.md", "---\ntitle: About\n---\nderived\n");

    let list = list(&dir);

    assert_eq!(lines(&list), ["/about.md", "/recyclebin/items/"]);
    assert!(
        list.rules
            .iter()
            .all(|rule| rule.reason == Reason::Bookkeeping)
    );

    // The derived page is *deliberately* reachable — its pointer is what keeps
    // it from lying loose — and ruled even so.
    let reachable = block_on(ws(&dir).reachable_files("index.md")).unwrap();
    assert!(reachable.contains(Path::new("about.md")));
}

#[test]
fn a_claimed_archive_is_one_rule_and_never_walked() {
    let dir = tempdir("claimed");
    root(&dir, &[]);
    write(&dir, "photos/2024/a.jpg", "JPEG");
    write(&dir, "photos/2024/b.jpg", "JPEG");

    let mut workspace = ws(&dir);
    block_on(workspace.attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();

    let list = block_on(workspace.ignore_list(Path::new("index.md"))).unwrap();

    assert_eq!(lines(&list), ["/photos/"]);
    assert_eq!(list.rules[0].reason, Reason::Claimed);
}

#[test]
fn another_tool_s_store_beside_the_workspace_is_one_rule() {
    // prov knows nothing about what such a store is — it is simply a folder
    // the graph reaches nothing in, which is exactly what makes it one line
    // for the tool being told.
    let dir = tempdir("sibling-store");
    root(&dir, &[]);
    write(&dir, "history/historica.txt", "historica-v1\n");
    write(&dir, "history/revisions/keep.rev.txt", "not content");

    let list = list(&dir);

    assert_eq!(lines(&list), ["/history/"]);
    assert_eq!(list.rules[0].reason, Reason::Unreached);
}

#[test]
fn rules_come_out_in_path_order_whatever_the_walk_met_first() {
    let dir = tempdir("order");
    root(&dir, &[]);
    loose(&dir, "zebra.md", "Zebra");
    loose(&dir, "alpha.md", "Alpha");
    loose(&dir, "midway/loose.md", "Loose");

    assert_eq!(lines(&list(&dir)), ["/alpha.md", "/midway/", "/zebra.md"]);
}

#[test]
fn a_line_names_one_file_however_the_file_is_spelled() {
    let dir = tempdir("escaping");
    root(&dir, &[]);
    loose(&dir, "draft[1].md", "Draft");
    loose(&dir, "note*.md", "Star");

    let list = list(&dir);

    // Anchored to the root and escaped: gitignore reads `[1]` as a character
    // class and `*` as a wildcard, and neither is what the filename says.
    assert_eq!(lines(&list), ["/draft\\[1\\].md", "/note\\*.md"]);
    assert_eq!(list.render(), "/draft\\[1\\].md\n/note\\*.md\n");
}

#[test]
fn a_workspace_that_reaches_everything_lists_nothing() {
    let dir = tempdir("empty");
    root(&dir, &["notes/a.md"]);
    child(&dir, "notes/a.md", "A");

    let list = list(&dir);

    assert!(list.is_empty(), "{:?}", lines(&list));
    assert_eq!(list.render(), "");
}
