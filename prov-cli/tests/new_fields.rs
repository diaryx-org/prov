//! `prov new` and the fields a document opens with: the workspace's `created`
//! stamp, each `fields.<name>.default`, and `--set`.
//!
//! The library writes what it is handed and reads no declaration; the CLI is
//! where the three sources are gathered and ordered. So what is asserted here
//! is that ordering and its boundaries — a `--set` overrides a default, a
//! default is not a rule, and prov's own fields are not overridable — against
//! the real binary and a real config document.

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
    let dir = std::env::temp_dir().join(format!("prov-new-fields-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).unwrap()
}

/// The frontmatter lines of a document, in order, without the fences.
fn frontmatter(text: &str) -> Vec<&str> {
    text.trim_start_matches("---\n")
        .split("\n---")
        .next()
        .unwrap()
        .lines()
        .collect()
}

/// A workspace stamping `created`, whose config declares a `status` that
/// starts as `open` and a `priority` that starts as an integer.
fn workspace(tag: &str) -> std::path::PathBuf {
    let dir = sandbox(tag);
    ok(&dir, &["init", "--yes", "--created-field", "created"]);
    let mut node = read(&dir, "prov.yaml");
    node.push_str("fields:\n  status:\n    default: open\n  priority:\n    default: 2\n");
    std::fs::write(dir.join("prov.yaml"), node).unwrap();
    // The page is derived from the config, and the config was just edited by
    // hand; regenerate it so `check` has nothing to say about it.
    ok(&dir, &["about"]);
    dir
}

#[test]
fn a_new_document_opens_with_the_stamp_and_each_default() {
    let dir = workspace("defaults");
    ok(&dir, &["new", "Fix the build", "--in", "index.md"]);
    let text = read(&dir, "fix-the-build.md");
    let lines = frontmatter(&text);

    // prov's own fields first, then the stamp, then the defaults in the order
    // the config declares them (`fields` is keyed by name, so alphabetical).
    assert_eq!(lines[0], "title: Fix the build", "{text}");
    let created = lines
        .iter()
        .position(|l| l.starts_with("created: "))
        .expect("a created stamp");
    let part_of = lines
        .iter()
        .position(|l| l.starts_with("part_of: "))
        .expect("the link back");
    assert!(part_of < created, "the link precedes the stamp:\n{text}");
    let value = lines[created].trim_start_matches("created: ");
    assert!(
        value.starts_with("20") && value.contains('T') && value.ends_with('Z'),
        "an RFC 3339 UTC instant, as `updated` is written: {value:?}"
    );
    let position = |line: &str| {
        lines
            .iter()
            .position(|l| *l == line)
            .unwrap_or_else(|| panic!("no `{line}` in:\n{text}"))
    };
    let (priority, status) = (position("priority: 2"), position("status: open"));
    assert!(created < priority && priority < status, "{text}");
}

#[test]
fn set_overrides_a_default_and_adds_what_no_default_names() {
    let dir = workspace("set");
    ok(
        &dir,
        &[
            "new",
            "Triage",
            "--in",
            "index.md",
            "--set",
            "status=in-progress",
            "--set",
            "owner=ada",
            "--set",
            "urgent=true",
        ],
    );
    let text = read(&dir, "triage.md");
    let lines = frontmatter(&text);
    assert!(lines.contains(&"status: in-progress"), "{text}");
    assert!(!lines.contains(&"status: open"), "{text}");
    assert!(
        lines.contains(&"priority: 2"),
        "the other default stands: {text}"
    );
    assert!(lines.contains(&"owner: ada"), "{text}");
    // Typed like `set`'s value: a bare `true` is a bool, not the word.
    assert!(lines.contains(&"urgent: true"), "{text}");
    ok(&dir, &["get", "triage.md", "urgent"]);
}

#[test]
fn provs_own_fields_are_not_overridable_from_set() {
    let dir = workspace("own");
    ok(
        &dir,
        &[
            "new",
            "Kept",
            "--in",
            "index.md",
            "--set",
            "title=Other",
            "--set",
            "part_of=nowhere.md",
        ],
    );
    let text = read(&dir, "kept.md");
    let lines = frontmatter(&text);
    assert_eq!(lines[0], "title: Kept", "{text}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("part_of: ") && l.contains("index.md")),
        "{text}"
    );
    assert!(!text.contains("nowhere"), "{text}");
    // And the tree is intact: the parent lists the child, the child points back.
    let (_, out, err) = run(&dir, &["check"]);
    assert!(
        out.contains("no findings") || err.contains("no findings"),
        "{out}{err}"
    );
}

#[test]
fn a_malformed_set_refuses_before_anything_is_written() {
    let dir = workspace("malformed");
    let (ok_, _, err) = run(
        &dir,
        &["new", "Half", "--in", "index.md", "--set", "status"],
    );
    assert!(!ok_, "a --set without `=` is refused");
    assert!(err.contains("KEY=VALUE"), "{err}");
    assert!(!dir.join("half.md").exists(), "nothing was created");
    assert!(
        !read(&dir, "index.md").contains("half.md"),
        "and the parent gained no entry"
    );
}

#[test]
fn a_workspace_declaring_nothing_writes_nothing_extra() {
    let dir = sandbox("plain");
    ok(&dir, &["init", "--yes"]);
    ok(&dir, &["new", "Plain", "--in", "index.md"]);
    let text = read(&dir, "plain.md");
    let lines = frontmatter(&text);
    assert!(
        lines
            .iter()
            .all(|l| l.starts_with("title: ") || l.starts_with("part_of: ")),
        "only prov's own fields: {text}"
    );
}

#[test]
fn the_about_page_says_what_a_document_starts_with() {
    let dir = workspace("about");
    ok(&dir, &["about"]);
    // The page wraps its prose, so compare with the wrapping undone.
    let page = read(&dir, "about.md")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        page.contains("A field named `created` is written when a document is made"),
        "{page}"
    );
    assert!(
        page.contains(
            "**Starting values.** A document made here opens with `priority` set to `2` \
             and `status` set to `open`."
        ),
        "{page}"
    );
}
