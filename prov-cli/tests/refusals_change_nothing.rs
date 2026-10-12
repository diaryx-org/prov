//! A structural command that refuses leaves the workspace as it found it.
//!
//! Each verb here is a write preceded by checks, and the checks have to come
//! first. `mv --in missing.md` used to move the file and only then find that
//! its new parent did not exist, leaving the document at its new path without
//! the parent it was moved to join; a `-p` route used to be built before the
//! command found out it would refuse, leaving index nodes nobody asked for.
//! Every test snapshots the whole tree, runs a command that must refuse, and
//! asserts the tree is byte-for-byte what it was.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn run(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_prov"))
        .current_dir(dir)
        .args(args)
        .env("PROV_QUIET", "1")
        .output()
        .expect("run prov");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

fn ok(dir: &Path, args: &[&str]) {
    let (ok, out) = run(dir, args);
    assert!(ok, "`prov {}` failed:\n{out}", args.join(" "));
}

/// `index.md` → `a.md` (titled `A`) → `child.md` (titled `Child`), with
/// every file the fixture's own commands bootstrap already on disk.
fn workspace(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("prov-refusals-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    ok(&dir, &["init", "--yes"]);
    ok(&dir, &["new", "A", "--in", "index.md"]);
    ok(&dir, &["new", "Child", "--in", "a.md"]);
    dir
}

/// Every file under `dir`, by relative path, with its bytes.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap().to_path_buf();
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// Run `args`, which must refuse, and assert nothing on disk changed. The
/// refusal's text is returned for the caller to check what it said.
fn refuses_cleanly(dir: &Path, args: &[&str]) -> String {
    let before = snapshot(dir);
    let (success, out) = run(dir, args);
    assert!(!success, "`prov {}` should refuse:\n{out}", args.join(" "));
    let after = snapshot(dir);
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>(),
        "`prov {}` refused but changed which files exist:\n{out}",
        args.join(" ")
    );
    for (path, bytes) in &before {
        assert!(
            after[path] == *bytes,
            "`prov {}` refused but rewrote {}:\n{out}",
            args.join(" "),
            path.display()
        );
    }
    out
}

#[test]
fn mv_into_a_parent_that_does_not_exist_moves_nothing() {
    let dir = workspace("mv-missing");
    let out = refuses_cleanly(&dir, &["mv", "a.md", "b.md", "--in", "missing.md"]);
    assert!(
        out.contains("--in missing.md: missing.md does not exist in this workspace"),
        "{out}"
    );
}

#[test]
fn mv_into_its_own_descendant_moves_nothing() {
    let dir = workspace("mv-cycle");
    let out = refuses_cleanly(&dir, &["mv", "a.md", "b.md", "--in", "child.md"]);
    assert!(out.contains("detach"), "{out}");
}

#[test]
fn mv_into_itself_moves_nothing() {
    let dir = workspace("mv-self");
    refuses_cleanly(&dir, &["mv", "a.md", "b.md", "--in", "a.md"]);
}

#[test]
fn mv_onto_an_occupied_path_builds_no_route() {
    let dir = workspace("mv-route");
    refuses_cleanly(&dir, &["mv", "a.md", "child.md", "--in", "@Fresh", "-p"]);
}

#[test]
fn mv_still_moves_and_reparents() {
    // The reordering must not cost the command its job: a path parent and a
    // route `-p` synthesizes both still land, and the result checks clean.
    let dir = workspace("mv-works");
    ok(&dir, &["new", "Other", "--in", "index.md"]);
    ok(&dir, &["mv", "child.md", "moved.md", "--in", "other.md"]);
    let other = std::fs::read_to_string(dir.join("other.md")).unwrap();
    assert!(other.contains("moved.md"), "{other}");
    ok(
        &dir,
        &["mv", "moved.md", "again.md", "--in", "@Fresh", "-p"],
    );
    let fresh = std::fs::read_to_string(dir.join("fresh/index.md")).unwrap();
    assert!(fresh.contains("again.md"), "{fresh}");
    ok(&dir, &["check"]);
}

#[test]
fn reparent_of_a_missing_document_builds_no_route() {
    let dir = workspace("reparent-missing");
    refuses_cleanly(&dir, &["reparent", "missing.md", "--in", "@Fresh", "-p"]);
}

#[test]
fn reparent_under_its_own_new_descendant_builds_no_route() {
    // The route runs through `a.md` itself, so its synthesized terminal would
    // sit beneath the document being moved there.
    let dir = workspace("reparent-cycle");
    refuses_cleanly(&dir, &["reparent", "a.md", "--in", "@A/Below", "-p"]);
}

#[test]
fn new_onto_a_differently_titled_document_builds_no_route() {
    let dir = workspace("new-collision");
    refuses_cleanly(
        &dir,
        &["new", "Other", "--in", "@Fresh", "-p", "--as", "a.md"],
    );
}

#[test]
fn new_outside_the_workspace_builds_no_route() {
    let dir = workspace("new-outside");
    refuses_cleanly(
        &dir,
        &[
            "new",
            "Other",
            "--in",
            "@Fresh",
            "-p",
            "--as",
            "../outside.md",
        ],
    );
}

#[test]
fn attach_of_a_missing_file_builds_no_route() {
    let dir = workspace("attach-missing");
    refuses_cleanly(&dir, &["attach", "missing.png", "--in", "@Fresh", "-p"]);
}

#[test]
fn mv_of_a_directory_into_a_parent_moves_nothing() {
    // A directory moves as a tree, but has no `part_of` of its own to repoint:
    // the reparent after the move could only fail.
    let dir = workspace("mv-dir");
    ok(&dir, &["new", "Deep", "--in", "@Fresh", "-p"]);
    let out = refuses_cleanly(&dir, &["mv", "fresh", "elsewhere", "--in", "a.md"]);
    assert!(out.contains("is a directory"), "{out}");
}
