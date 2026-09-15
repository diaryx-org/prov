//! `prov docs` — the census a view narrows: every reached document, listed.
//!
//! Three properties only the binary can show: the listing is *reached* rather
//! than *present* (a file in a directory nothing links into is not a row), the
//! root is a row of its own (unlike a scoped view's anchor), and the `id`
//! column reads the same whether the document carries its id or the registry
//! does.

use std::path::Path;
use std::process::Command;

fn run(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_prov"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run prov");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// A root with two children, one of them an index over a third document, and
/// a `stray/` directory nothing links into. `b.md` has no title, so the text
/// line has no dash to drop and the JSON has a `null` to keep.
fn vault(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("prov-docs-cli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    write(
        &dir,
        "index.md",
        "---\ntitle: Home\ncontents:\n- notes.md\n- b.md\n---\n",
    );
    write(
        &dir,
        "notes.md",
        "---\ntitle: Notes\npart_of: index.md\ncontents:\n- notes/a.md\n---\n",
    );
    write(
        &dir,
        "notes/a.md",
        "---\ntitle: A\npart_of: ../notes.md\nid: abc1234\nstatus: open\n---\nbody\n",
    );
    write(&dir, "b.md", "---\npart_of: index.md\n---\n");
    write(&dir, "stray/s.md", "---\ntitle: Stray\n---\n");
    dir
}

#[test]
fn lists_every_reached_document_in_path_order_and_nothing_else() {
    let dir = vault("text");
    let (ok, out, _) = run(&dir, &["docs"]);
    assert!(ok);
    // Path order, not tree order: `notes/a.md` sorts before `notes.md` because
    // `/` sorts before `.`, and that is the order a consumer can rely on.
    assert_eq!(
        out,
        "b.md\nindex.md — Home\nnotes/a.md — A\nnotes.md — Notes\n"
    );
    assert!(
        !out.contains("stray"),
        "an unlinked directory is not reached: {out}"
    );
}

#[test]
fn json_is_one_array_of_rows_with_an_id_column_and_a_silent_stderr() {
    let dir = vault("json");
    let (ok, out, err) = run(&dir, &["docs", "--json"]);
    assert!(ok);
    assert!(
        err.is_empty(),
        "stderr should be silent under --json: {err}"
    );
    assert!(out.starts_with("[\n"), "one array: {out}");
    // The root is a row — a census keeps its start where a scoped view drops
    // its anchor.
    assert!(out.contains("\"path\": \"index.md\""), "{out}");
    // The whole metadata block rides along, so a consumer need not go back to
    // the file for a field the row did not lift out.
    assert!(out.contains("\"status\": \"open\""), "{out}");
    // The id is a column, not only a key inside `meta`.
    assert!(out.contains("\"id\": \"abc1234\""), "{out}");
    // …and `null`, not absent, where a document has none.
    assert!(out.contains("\"id\": null"), "{out}");
    assert!(out.contains("\"title\": null"), "{out}");
    assert!(!out.contains("stray"), "{out}");
}

/// Under `id_storage: registry` the document carries no `id` field, so the
/// column has to come from the registry — and the registry, the config and
/// the about page are machinery reached one-way from the root, not rows.
#[test]
fn id_column_is_filled_from_the_registry_when_the_document_carries_none() {
    let dir = std::env::temp_dir().join(format!("prov-docs-cli-registry-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (ok, out, err) = run(
        &dir,
        &[
            "init",
            "--yes",
            "--id-storage",
            "registry",
            "--title",
            "Home",
        ],
    );
    assert!(ok, "{out}{err}");
    let (ok, out, err) = run(&dir, &["new", "Note", "--in", "index.md"]);
    assert!(ok, "{out}{err}");
    let (ok, out, err) = run(&dir, &["id", "note.md"]);
    assert!(ok, "{out}{err}");
    let id = out
        .trim()
        .strip_prefix("id:")
        .expect("an id target")
        .to_owned();
    assert!(
        !std::fs::read_to_string(dir.join("note.md"))
            .unwrap()
            .contains("id:"),
        "registry storage stamps nothing into the document"
    );

    let (ok, out, err) = run(&dir, &["docs", "--json"]);
    assert!(ok, "{err}");
    assert!(out.contains(&format!("\"id\": \"{id}\"")), "{out}");
    // The root's metadata *names* each of these — that is how they are
    // reached — so it is the path column that must not carry them.
    for machinery in ["prov.yaml", "registry.yaml", "about.md"] {
        assert!(
            !out.contains(&format!("\"path\": \"{machinery}\"")),
            "{machinery} is not a document: {out}"
        );
    }
}

#[test]
fn a_root_alone_is_one_row() {
    let dir = vault("alone");
    write(&dir, "index.md", "---\ntitle: Home\n---\n");
    let (ok, out, _) = run(&dir, &["docs", "--json"]);
    assert!(ok);
    assert_eq!(out.matches("\"path\":").count(), 1, "{out}");
}
