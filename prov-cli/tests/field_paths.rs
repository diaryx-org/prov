//! The field path `get`, `set` and `unset` take.
//!
//! A list item is named `contents[0]`, as fig names it and as `prov check`
//! names a finding's field; `contents.0`, the spelling these commands took
//! first, names the same item. Both must keep working, for every command
//! that takes a path.

use std::path::{Path, PathBuf};
use std::process::Command;

fn run(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_prov"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run prov");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let (ok, out) = run(dir, args);
    assert!(ok, "prov {args:?} failed: {out}");
    out
}

/// A loose file, no workspace: the path grammar is the same either way.
fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("prov-field-paths-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("notes.md"),
        "---\ntitle: Notes\ncontents:\n  - a.md\n  - b.md\nsources:\n  - resource: x.md\n    title: X\n---\nbody\n",
    )
    .unwrap();
    dir
}

#[test]
fn get_takes_a_list_index_in_brackets_or_after_a_dot() {
    let dir = fixture("get");
    for (path, want) in [
        ("contents[0]", "a.md"),
        ("contents.0", "a.md"),
        ("contents[1]", "b.md"),
        ("sources[0].resource", "x.md"),
        ("sources.0.title", "X"),
    ] {
        assert_eq!(ok(&dir, &["get", "notes.md", path]).trim(), want, "{path}");
    }
    let (found, out) = run(&dir, &["get", "notes.md", "contents[2]"]);
    assert!(!found, "an index past the end is absent: {out}");
    assert!(out.contains("no `contents[2]`"), "{out}");
}

#[test]
fn set_and_unset_take_the_bracket_form() {
    let dir = fixture("set");
    ok(&dir, &["set", "notes.md", "contents[1]", "c.md"]);
    ok(&dir, &["set", "notes.md", "sources[0].title", "Y"]);
    assert_eq!(ok(&dir, &["get", "notes.md", "contents.1"]).trim(), "c.md");
    assert_eq!(
        ok(&dir, &["get", "notes.md", "sources.0.title"]).trim(),
        "Y"
    );

    ok(&dir, &["unset", "notes.md", "contents[0]"]);
    assert_eq!(ok(&dir, &["get", "notes.md", "contents[0]"]).trim(), "c.md");
    ok(&dir, &["unset", "notes.md", "sources.0.title"]);
    let text = std::fs::read_to_string(dir.join("notes.md")).unwrap();
    assert!(!text.contains("title: Y"), "{text}");
    assert!(text.contains("resource: x.md"), "{text}");
}
