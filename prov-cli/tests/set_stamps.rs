//! `prov set` / `prov unset` inside a workspace — the bookkeeping the edit
//! implies, landed with the edit.
//!
//! `set` used to be a text rewrite wherever it ran. It still is outside a
//! workspace, which is the property `resolve_target` documents and a script
//! over loose files depends on. Inside one it now goes through the same seam
//! `edit` does, so closing a task with `set status done` stamps `updated` the
//! way opening `$EDITOR` and typing the same line would. What is asserted here
//! is the boundary: which files get the stamp, which field defers to the user,
//! and that nothing at all changes when the workspace keeps no such field.

use std::path::Path;
use std::process::Command;

fn run(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_prov"))
        .current_dir(dir)
        .args(args)
        .env("PROV_QUIET", "1")
        .output()
        .expect("run prov");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn ok(dir: &Path, args: &[&str]) -> (String, String) {
    let (ok, out, err) = run(dir, args);
    assert!(
        ok,
        "`prov {}` failed:\nstdout:{out}\nstderr:{err}",
        args.join(" ")
    );
    (out, err)
}

fn sandbox(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("prov-set-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).unwrap()
}

/// The line of `text` that sets `field`, if any.
fn line_for<'a>(text: &'a str, field: &str) -> Option<&'a str> {
    text.lines()
        .find(|line| line.starts_with(&format!("{field}:")))
}

/// A workspace recording an `updated` field, with one document to edit.
fn workspace(tag: &str) -> std::path::PathBuf {
    let dir = sandbox(tag);
    ok(&dir, &["init", "--yes"]);
    ok(&dir, &["config", "updated", "updated"]);
    ok(&dir, &["new", "Rust", "--in", "index.md"]);
    dir
}

#[test]
fn set_in_a_workspace_stamps_the_updated_field() {
    let dir = workspace("stamps");
    assert!(
        line_for(&read(&dir, "rust.md"), "updated").is_none(),
        "a fresh document carries no stamp"
    );

    let (out, err) = ok(&dir, &["set", "rust.md", "summary", "notes"]);
    assert_eq!(out.trim(), "rust.md", "stdout is still the edited path");
    assert!(
        err.contains("stamped `updated`"),
        "narrates the stamp: {err:?}"
    );

    let text = read(&dir, "rust.md");
    assert!(text.contains("summary: notes"), "the edit landed: {text}");
    let stamp = line_for(&text, "updated").expect("updated is stamped");
    // RFC 3339 UTC, as `edit` writes it — the value a reader can order and a
    // machine can parse, never a date a person would have typed.
    let value = stamp.trim_start_matches("updated:").trim();
    assert!(
        value.starts_with("20") && value.contains('T') && value.ends_with('Z'),
        "an RFC 3339 UTC instant: {value:?}"
    );
}

#[test]
fn unset_stamps_the_same_way() {
    let dir = workspace("unset");
    ok(&dir, &["set", "rust.md", "summary", "notes"]);
    let before = line_for(&read(&dir, "rust.md"), "updated")
        .unwrap()
        .to_string();

    // Long enough for a microsecond clock to move, short enough not to notice.
    std::thread::sleep(std::time::Duration::from_millis(2));
    let (_, err) = ok(&dir, &["unset", "rust.md", "summary"]);
    assert!(err.contains("stamped `updated`"), "{err:?}");

    let text = read(&dir, "rust.md");
    assert!(!text.contains("summary:"), "the field is gone: {text}");
    let after = line_for(&text, "updated").unwrap();
    assert_ne!(after, before, "the stamp moved with the second edit");
}

#[test]
fn setting_the_updated_field_itself_keeps_the_given_value() {
    let dir = workspace("own-field");
    let (_, err) = ok(&dir, &["set", "rust.md", "updated", "2020-01-01"]);
    assert!(
        !err.contains("stamped"),
        "naming the instant is not an edit to restamp: {err:?}"
    );
    assert_eq!(
        line_for(&read(&dir, "rust.md"), "updated"),
        Some("updated: 2020-01-01"),
        "the value the user gave is the value on disk"
    );
}

#[test]
fn a_workspace_without_an_updated_field_gets_a_bare_rewrite() {
    let dir = sandbox("no-field");
    ok(&dir, &["init", "--yes"]);
    ok(&dir, &["new", "Rust", "--in", "index.md"]);
    let (_, err) = ok(&dir, &["set", "rust.md", "summary", "notes"]);
    assert!(err.trim().is_empty(), "nothing to narrate: {err:?}");
    let text = read(&dir, "rust.md");
    assert!(text.contains("summary: notes"), "{text}");
    assert!(
        line_for(&text, "updated").is_none(),
        "no field is configured, so none is written: {text}"
    );
}

#[test]
fn a_loose_file_outside_any_workspace_is_rewritten_as_before() {
    let dir = sandbox("loose");
    std::fs::write(dir.join("loose.md"), "---\ntitle: Loose\n---\n\nbody\n").unwrap();
    let (out, err) = ok(&dir, &["set", "loose.md", "summary", "notes"]);
    assert_eq!(out.trim(), "loose.md");
    assert!(err.trim().is_empty(), "{err:?}");
    assert_eq!(
        read(&dir, "loose.md"),
        "---\ntitle: Loose\nsummary: notes\n---\n\nbody\n",
        "a text rewrite and nothing else"
    );
}

#[test]
fn the_workspace_node_is_edited_but_never_stamped() {
    let dir = workspace("node");
    // `updated: updated` is *configuration* in the node: the name of the field.
    // A stamp landing here would overwrite the name with an instant, and the
    // workspace would then be told to maintain a field called
    // `2026-…T…Z`.
    let (_, err) = ok(&dir, &["set", "prov.yaml", "fixity", "off"]);
    assert!(!err.contains("stamped"), "{err:?}");
    let node = read(&dir, "prov.yaml");
    assert!(node.contains("fixity: off"), "the edit landed: {node}");
    assert_eq!(
        line_for(&node, "updated"),
        Some("updated: updated"),
        "the axis still names the field: {node}"
    );
}

#[test]
fn edit_and_stamp_also_leave_the_node_alone() {
    let dir = workspace("edit-node");
    let before = read(&dir, "prov.yaml");
    // A no-op editor that still counts as a change: append a comment line.
    let (ok_, _, err) = {
        let out = Command::new(env!("CARGO_BIN_EXE_prov"))
            .current_dir(&dir)
            .args(["edit", "prov.yaml"])
            .env("PROV_QUIET", "1")
            .env("EDITOR", "sh -c 'printf \"# touched\\n\" >> \"$0\"'")
            .output()
            .expect("run prov");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    assert!(ok_, "{err}");
    assert!(!err.contains("stamped"), "{err:?}");
    let after = read(&dir, "prov.yaml");
    assert!(
        after.starts_with(&before),
        "only the comment was added:\n{after}"
    );

    let (_, err) = ok(&dir, &["stamp", "prov.yaml"]);
    assert!(!err.contains("stamped `updated`"), "{err:?}");
    assert_eq!(
        line_for(&read(&dir, "prov.yaml"), "updated"),
        Some("updated: updated")
    );
}
