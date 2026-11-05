//! `prov new … -p` — idempotent creation. `-p` completes the `mkdir -p` analogy:
//! it already synthesizes missing route segments; now an already-existing leaf (a
//! same-titled child) is a no-op rather than an error, so a recurring-entry
//! workflow (a daily-note cron) can re-run the same command safely.

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

fn vault(tag: &str) -> prov_testkit::Scratch {
    let dir = prov_testkit::scratch("new-idem", tag);
    assert!(run(&dir, &["init", "--yes"]).0, "init");
    dir
}

#[test]
fn a_rerun_with_dash_p_is_a_no_op_but_without_it_errors() {
    let dir = vault("rerun");
    let (ok, out) = run(&dir, &["new", "A Note", "--in", "index.md", "-p"]);
    assert!(ok && out.contains("created"), "first creates: {out}");

    // Re-run with -p: no-op success.
    let (ok, out) = run(&dir, &["new", "A Note", "--in", "index.md", "-p"]);
    assert!(ok, "rerun succeeds: {out}");
    assert!(out.contains("exists"), "rerun reports a no-op: {out}");

    // Without -p, an existing leaf is still an error (the interactive safety net).
    let (ok, out) = run(&dir, &["new", "A Note", "--in", "index.md"]);
    assert!(
        !ok && out.contains("already exists"),
        "no -p still errors: {out}"
    );
}

#[test]
fn a_different_title_at_the_same_path_is_a_collision_not_a_no_op() {
    let dir = vault("collision");
    assert!(
        run(
            &dir,
            &["new", "A Note", "--as", "note.md", "--in", "index.md", "-p"]
        )
        .0
    );
    // Same path, different title — must not silently reuse a stranger.
    let (ok, out) = run(
        &dir,
        &[
            "new",
            "Different",
            "--as",
            "note.md",
            "--in",
            "index.md",
            "-p",
        ],
    );
    assert!(!ok, "different title must error: {out}");
    assert!(
        out.contains("different title"),
        "explains the collision: {out}"
    );
}

#[test]
fn a_route_leaf_is_idempotent_end_to_end() {
    let dir = vault("route");
    // First run builds the whole route and the leaf.
    let (ok, out) = run(&dir, &["new", "2026-07-20", "--in", "@Daily/2026/07", "-p"]);
    assert!(
        ok && out.contains("created"),
        "first builds route + leaf: {out}"
    );
    // A second identical run is a clean no-op — the daily-note cron shape.
    let (ok, out) = run(&dir, &["new", "2026-07-20", "--in", "@Daily/2026/07", "-p"]);
    assert!(ok, "rerun succeeds: {out}");
    assert!(out.contains("exists"), "leaf no-op on rerun: {out}");
    assert!(run(&dir, &["check"]).0, "workspace validates");
}

#[test]
fn dash_p_relinks_an_existing_but_unlinked_leaf() {
    let dir = vault("relink");
    assert!(run(&dir, &["new", "Orphan", "--in", "index.md", "-p"]).0);

    // Sever the parent's link to the child, leaving the file on disk (its own
    // `part_of` intact). The child is now reachable-but-unlisted.
    let index = std::fs::read_to_string(dir.join("index.md")).unwrap();
    let severed: String = index
        .lines()
        .filter(|l| !l.contains("orphan.md"))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(dir.join("index.md"), severed).unwrap();
    assert!(
        !std::fs::read_to_string(dir.join("index.md"))
            .unwrap()
            .contains("orphan.md"),
        "precondition: link removed"
    );

    // `new -p` converges: it re-adopts the existing file rather than erroring.
    let (ok, out) = run(&dir, &["new", "Orphan", "--in", "index.md", "-p"]);
    assert!(
        ok && out.contains("exists"),
        "re-link is a no-op success: {out}"
    );
    assert!(
        std::fs::read_to_string(dir.join("index.md"))
            .unwrap()
            .contains("orphan.md"),
        "the parent links the child again: {}",
        std::fs::read_to_string(dir.join("index.md")).unwrap()
    );
}
