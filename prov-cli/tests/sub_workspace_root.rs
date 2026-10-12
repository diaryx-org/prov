//! A sub-workspace made the ordinary way — `prov init` inside an initialized
//! workspace — whose root then says what contains it with `set part_of`. The
//! node `init` writes names no `root`, and the walk used to pass straight over
//! the now-contained root and open the *outer* workspace from inside the inner
//! one. The node is what declares the workspace, so it is where the walk stops.

use std::path::{Path, PathBuf};
use std::process::Command;

fn prov() -> Command {
    Command::new(env!("CARGO_BIN_EXE_prov"))
}

fn run(dir: &Path, args: &[&str]) -> String {
    let out = prov().current_dir(dir).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "prov {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// `base/outer` and `base/outer/nested`, each `prov init`ed with a name.
fn outer_and_nested(tag: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("prov-subws-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    run(
        &base,
        &["init", "outer", "--yes", "--workspace-id", "outer"],
    );
    run(
        &base,
        &["init", "outer/nested", "--yes", "--workspace-id", "nested"],
    );
    (base.join("outer"), base.join("outer/nested"))
}

#[test]
fn a_sub_workspace_root_with_a_foreign_parent_still_opens_the_sub_workspace() {
    let (outer, nested) = outer_and_nested("foreign-parent");
    run(&nested, &["set", "index.md", "part_of", "id:outer/abc1234"]);

    let tree = run(&outer, &["-C", "nested", "tree"]);
    assert!(
        tree.starts_with("index.md — Nested"),
        "`-C nested tree` must open the nested workspace, got:\n{tree}"
    );
    let tree = run(&nested, &["tree"]);
    assert!(
        tree.starts_with("index.md — Nested"),
        "`tree` from inside must open the nested workspace, got:\n{tree}"
    );
    // And the outer workspace still opens to itself.
    let tree = run(&outer, &["tree"]);
    assert!(tree.starts_with("index.md — Outer"), "got:\n{tree}");
}

#[test]
fn a_root_named_otherwise_warns_when_part_of_hides_it() {
    // The stem is what lets a node imply its root; a root called anything else
    // needs `root:`, and `set` says so at the moment the edit hides it rather
    // than leaving the next command to open the outer workspace in silence.
    let (_outer, nested) = outer_and_nested("named-otherwise");
    std::fs::rename(nested.join("index.md"), nested.join("home.md")).unwrap();
    let out = prov()
        .current_dir(&nested)
        .args(["set", "home.md", "part_of", "id:outer/abc1234"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no longer discovered") && stderr.contains("prov config root home.md"),
        "expected a warning naming the fix, got:\n{stderr}"
    );

    // The shape `init` writes is quiet: the node implies `index.md`.
    let (_outer, nested) = outer_and_nested("named-index");
    let out = prov()
        .current_dir(&nested)
        .args(["set", "index.md", "part_of", "id:outer/abc1234"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("warning"), "unexpected warning:\n{stderr}");
}
