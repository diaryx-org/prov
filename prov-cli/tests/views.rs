//! `prov views` — the workspace's declared lenses, listed and executed.
//!
//! These drive the whole chain the library tests only see in pieces: a `views:`
//! block written into a real root document, read back through `WorkspaceConfig`,
//! executed against a real spanning tree on a real filesystem. Two of the three
//! properties asserted here (scope excluding an out-of-subtree document that
//! *would* have grouped, and a dead anchor exiting non-zero) are only observable
//! from outside.

use std::path::Path;
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

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// A journal declaring two views: a scoped date chain and an unscoped field.
/// `readme.md` carries a `created` stamp and is *not* under `Daily` — it is
/// what makes the scoped view's scope observable.
fn vault(tag: &str, under: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("prov-views-cli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    write(
        &dir,
        "index.md",
        &format!(
            "---\ntitle: Home\nprov:\n  views:\n    daily:\n      label: Daily\n      \
             group: [date_of_document, created]\n      by: month\n      under: '{under}'\n    \
             who:\n      group: people\ncontents:\n- daily.md\n- readme.md\n---\n"
        ),
    );
    write(
        &dir,
        "readme.md",
        "---\ntitle: Readme\npart_of: index.md\ncreated: 2026-01-02\npeople:\n- Ada\n---\n",
    );
    write(
        &dir,
        "daily.md",
        "---\ntitle: Daily\npart_of: index.md\ncontents:\n- daily/2026.md\n---\n",
    );
    write(
        &dir,
        "daily/2026.md",
        "---\ntitle: '2026'\npart_of: ../daily.md\ncontents:\n- 07-24.md\n- 08-01.md\n---\n",
    );
    write(
        &dir,
        "daily/07-24.md",
        "---\ntitle: July 24\npart_of: 2026.md\ndate_of_document: 2026-07-24\npeople:\n- Ada\n- Grace\n---\n",
    );
    write(
        &dir,
        "daily/08-01.md",
        "---\ntitle: August 1\npart_of: 2026.md\ncreated: 2026-08-01T09:00:00Z\n---\n",
    );
    dir
}

#[test]
fn bare_views_lists_what_the_workspace_declares() {
    let dir = vault("list", "[Daily](daily.md)");
    let (ok, out) = run(&dir, &["views"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("daily  Daily — group: date_of_document → created by month"),
        "{out}"
    );
    assert!(
        out.contains("who  Who — group: people (whole workspace)"),
        "{out}"
    );
}

/// The point of `under:`. `readme.md` carries `created: 2026-01-02` and would
/// group happily — it is excluded because it is not in the subtree, which is
/// what a lens over a whole vault cannot express.
#[test]
fn a_scoped_view_groups_only_its_subtree() {
    let dir = vault("scope", "[Daily](daily.md)");
    let (ok, out) = run(&dir, &["views", "daily"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("2026-07 (1)") && out.contains("daily/07-24.md — July 24"),
        "{out}"
    );
    assert!(out.contains("2026-08 (1)"), "{out}");
    assert!(
        !out.contains("readme.md"),
        "the README is out of scope: {out}"
    );
    // The year index has no date of its own, and is named rather than dropped.
    assert!(
        out.contains("(ungrouped) (1)") && out.contains("daily/2026.md"),
        "{out}"
    );
    assert!(!out.contains("2026-01"), "{out}");
}

/// The same corpus, unscoped and grouped by a multi-valued field: the README
/// joins, and one document files under both of its values.
#[test]
fn an_unscoped_view_covers_everything_and_repeats_multi_valued_rows() {
    let dir = vault("who", "[Daily](daily.md)");
    let (ok, out) = run(&dir, &["views", "who"]);
    assert!(ok, "{out}");
    assert!(out.contains("Ada (2)"), "{out}");
    assert!(out.contains("Grace (1)"), "{out}");
    assert!(
        out.contains("readme.md — Readme"),
        "the README is in scope: {out}"
    );
}

/// An anchor that names nothing must not read as an archive with nothing in it.
#[test]
fn a_dead_anchor_fails_loudly_rather_than_printing_an_empty_view() {
    let dir = vault("dead", "[Daily](gone.md)");
    let (ok, out) = run(&dir, &["views", "daily"]);
    assert!(!ok, "a broken view exits non-zero: {out}");
    assert!(out.contains("no document exists there"), "{out}");
}

#[test]
fn an_unknown_view_name_lists_the_ones_that_exist() {
    let dir = vault("unknown", "[Daily](daily.md)");
    let (ok, out) = run(&dir, &["views", "nope"]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("no view named `nope`") && out.contains("daily, who"),
        "{out}"
    );
}

/// A workspace that declares none says so, rather than printing nothing and
/// leaving the user unsure whether the command ran.
#[test]
fn a_workspace_with_no_views_says_so() {
    let dir = std::env::temp_dir().join(format!("prov-views-cli-none-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert!(run(&dir, &["init", "--yes"]).0, "init");

    let (ok, out) = run(&dir, &["views"]);
    assert!(ok, "{out}");
    assert!(out.contains("declares no views"), "{out}");
}

/// A misspelled key inside a view is caught by the config linter — the whole
/// reason the block was promoted out of an app's private namespace, where
/// nothing would have looked at it.
#[test]
fn a_misspelled_view_key_is_reported_by_check() {
    let dir = vault("lint", "[Daily](daily.md)");
    let text = std::fs::read_to_string(dir.join("index.md")).unwrap();
    std::fs::write(
        dir.join("index.md"),
        text.replace("      by: month", "      by: monthh\n      labl: Oops"),
    )
    .unwrap();

    let (_, out) = run(&dir, &["check"]);
    assert!(
        out.contains("views.daily.by") && out.contains("expected: year, month, day"),
        "{out}"
    );
    assert!(
        out.contains("views.daily.labl") && out.contains("views.daily.label"),
        "{out}"
    );

    // …and the view still runs, grouping on the uncut values rather than on a
    // grain nobody asked for.
    let (ok, out) = run(&dir, &["views", "daily"]);
    assert!(ok, "{out}");
    assert!(out.contains("2026-07-24 (1)"), "{out}");
}
