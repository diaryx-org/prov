//! A field declared `type: ref` is a link site — the path-valued fields of
//! `docs/proposals/path-valued-fields/`.
//!
//! One workspace, one card with a `sources` list whose entries are mappings
//! holding a `resource` beside a `title`, and the declaration
//! `sources[].resource: { type: ref }`. Then each thing the proposal promises:
//! `check` reports a `resource` that names nothing, at the address a repair
//! edits; `mv` rewrites the ones that resolve; `rm` reports the one it leaves
//! dangling; a document reached only through a `ref` is not an orphan; and the
//! carried keys beside the link are never touched. The URL in the same list
//! is external, and stays exactly as written throughout.

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
    let dir = std::env::temp_dir().join(format!("prov-ref-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(dir: &Path, rel: &str, text: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).unwrap()
}

/// The vault: a root declaring the field, a card citing two documents and a
/// URL, and the two documents — one in the tree, one reached only by the
/// citation.
fn vault(tag: &str) -> std::path::PathBuf {
    let dir = sandbox(tag);
    write(
        &dir,
        "index.md",
        "---\n\
         title: Vault\n\
         contents:\n\
         - cards/birth.md\n\
         - cards/census.md\n\
         prov:\n  fields:\n    sources[].resource:\n      type: ref\n\
         ---\n",
    );
    write(
        &dir,
        "cards/birth.md",
        "---\n\
         title: Birth of Ada\n\
         part_of: /index.md\n\
         sources:\n\
         - resource: census.md#line-12\n  \
           title: 1910 US Census, Ward 3\n\
         - resource: /cards/aunt.md\n  \
           title: A letter from her aunt\n\
         - resource: https://familysearch.org/ark:/61903/1\n  \
           title: FamilySearch image\n\
         ---\n",
    );
    write(
        &dir,
        "cards/census.md",
        "---\ntitle: 1910 census sheet\npart_of: /index.md\n---\n",
    );
    // Reached by the citation alone — no `part_of`, in no `contents` — in a
    // directory the tree occupies, which is where the orphan walk looks.
    write(&dir, "cards/aunt.md", "---\ntitle: From her aunt\n---\n");
    // The root was written by hand; the page that explains the workspace is
    // regenerated so `check` has nothing to say about it.
    ok(&dir, &["about"]);
    dir
}

#[test]
fn a_ref_field_is_checked_moved_and_reported_like_a_relation_entry() {
    let dir = vault("round-trip");

    // Clean: both in-workspace resources resolve, the locator is carried, the
    // URL is external, and the letter reached only by citation is no orphan.
    let (_, err) = ok(&dir, &["check"]);
    assert!(err.contains("no findings"), "{err}");

    // Break one: the census names it, at the concrete address.
    std::fs::remove_file(dir.join("cards/aunt.md")).unwrap();
    let (passed, out, err) = run(&dir, &["check"]);
    assert!(!passed, "a dangling resource is a finding");
    let report = format!("{out}{err}");
    assert!(
        report.contains("sources[1].resource") && report.contains("/cards/aunt.md"),
        "{report}"
    );
    // The JSON form carries the site whole, with no separate index.
    let (_, out, _) = run(&dir, &["check", "--json"]);
    assert!(
        out.contains("\"site\": \"sources[1].resource\"") && out.contains("\"index\": null"),
        "{out}"
    );

    // Put it back: the engine's own tests cover the repair that would have
    // dropped the value (a removal is destructive, so `--fix` asks first).
    write(&dir, "cards/aunt.md", "---\ntitle: From her aunt\n---\n");
    let (_, err) = ok(&dir, &["check"]);
    assert!(err.contains("no findings"), "{err}");

    // Move the census sheet: the citation follows, locator and title kept.
    ok(&dir, &["mv", "cards/census.md", "records/census-1910.md"]);
    let card = read(&dir, "cards/birth.md");
    assert!(
        card.contains("resource: /records/census-1910.md#line-12"),
        "the resource is rewritten in the workspace's path style, locator kept: {card}"
    );
    assert!(card.contains("title: 1910 US Census, Ward 3"), "{card}");
    assert!(
        card.contains("https://familysearch.org/ark:/61903/1"),
        "{card}"
    );
    let (_, err) = ok(&dir, &["check"]);
    assert!(err.contains("no findings"), "{err}");

    // Remove it: the citation is the reference left dangling, and `rm` says so.
    let (_, out, err) = run(&dir, &["rm", "records/census-1910.md", "--force"]);
    let report = format!("{out}{err}");
    assert!(
        report.contains("sources[0].resource"),
        "rm names the dangling citation: {report}"
    );
}

#[test]
fn a_document_reached_only_by_a_ref_field_is_not_an_orphan() {
    let dir = vault("orphan");
    let (_, err) = ok(&dir, &["check"]);
    assert!(err.contains("no findings"), "{err}");

    // Take the declaration away and the same letter is unreached.
    write(
        &dir,
        "index.md",
        "---\ntitle: Vault\ncontents:\n- cards/birth.md\n- cards/census.md\n---\n",
    );
    ok(&dir, &["about"]);
    let (_, out, err) = run(&dir, &["check"]);
    let report = format!("{out}{err}");
    assert!(
        report.contains("cards/aunt.md") && report.to_lowercase().contains("orphan"),
        "{report}"
    );
}

#[test]
fn a_scoped_ref_is_a_config_issue_and_still_read_everywhere() {
    let dir = vault("scoped");
    write(
        &dir,
        "index.md",
        "---\n\
         title: Vault\n\
         contents:\n\
         - cards/birth.md\n\
         - cards/census.md\n\
         prov:\n  fields:\n    sources[].resource:\n      type: ref\n      under: '[[Vault]]'\n\
         ---\n",
    );
    ok(&dir, &["about"]);
    std::fs::remove_file(dir.join("cards/aunt.md")).unwrap();
    let (_, out, err) = run(&dir, &["check"]);
    let report = format!("{out}{err}");
    assert!(
        report.contains("type: ref"),
        "the scope is reported: {report}"
    );
    assert!(
        report.contains("sources[1].resource"),
        "and the field is still a link site: {report}"
    );
}
