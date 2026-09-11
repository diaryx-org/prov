//! `prov presets` and `init --preset` — a stencil written out, in full, and
//! then forgotten.
//!
//! The fixture is this repository's own `presets/tasks/`, applied to a scratch
//! workspace and then read back by the same binary: `check` over the result
//! is what proves the stencil is usable and not merely well-formed, and a
//! second application finding nothing to add is what pins that a preset is
//! idempotent. The built-in preset is exercised through `init`, since that is
//! where it is written.

use std::path::{Path, PathBuf};
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

fn sandbox(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("prov-presets-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).unwrap()
}

/// The `tasks` preset this repository ships, by path.
fn tasks_preset() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../presets/tasks")
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

#[test]
fn init_writes_the_builtin_preset_and_says_so() {
    let dir = sandbox("builtin");
    let (_, err) = ok(&dir, &["init", "--yes"]);
    assert!(
        err.contains("updates `updated`") && err.contains("stamps `created`"),
        "{err}"
    );
    let node = read(&dir, "prov.yaml");
    assert!(
        node.contains("updated: updated\n") && node.contains("created: created\n"),
        "{node}"
    );
    // And it is the same preset `presets` shows, with nothing left to add.
    let (out, err) = ok(&dir, &["presets"]);
    assert!(
        out.contains("= created  (already so)") && out.contains("= updated  (already so)"),
        "{out}"
    );
    assert!(err.contains("nothing to write"), "{err}");
}

#[test]
fn a_flag_wins_over_the_builtin_presets_axis() {
    let dir = sandbox("flag-wins");
    ok(&dir, &["init", "--yes", "--updated-field", "modified"]);
    let node = read(&dir, "prov.yaml");
    assert!(node.contains("updated: modified\n"), "{node}");
    assert!(
        node.contains("created: created\n"),
        "the other axis still lands: {node}"
    );
}

#[test]
fn a_preset_directory_replaces_the_builtin_at_init() {
    let dir = sandbox("replace");
    let bare = sandbox("replace-preset");
    std::fs::write(bare.join("prov.yaml"), "fixity: off\n").unwrap();
    let (_, err) = ok(&dir, &["init", "--yes", "--preset", bare.to_str().unwrap()]);
    assert!(err.contains("preset "), "the summary names it: {err}");
    let node = read(&dir, "prov.yaml");
    assert!(node.contains("fixity: off\n"), "{node}");
    assert!(
        node.contains("updated: ''\n") && node.contains("created: ''\n"),
        "the built-in was not applied: {node}"
    );
}

#[test]
fn the_tasks_preset_applies_checks_clean_and_is_idempotent() {
    let dir = sandbox("tasks");
    ok(&dir, &["init", "--yes"]);
    let preset = tasks_preset();

    // The plan first, writing nothing.
    let (out, err) = ok(&dir, &["presets", &preset]);
    assert!(
        out.contains("+ fields.status") && out.contains("+ vocab/statuses.yaml"),
        "{out}"
    );
    assert!(err.contains("pass --write"), "{err}");
    assert!(!dir.join("vocab/statuses.yaml").exists());

    let (_, err) = ok(&dir, &["presets", &preset, "--write"]);
    assert!(err.contains("wrote 6 config entries"), "{err}");
    assert!(dir.join("vocab/statuses.yaml").exists());
    let page = read(&dir, "about.md");
    assert!(
        page.contains("Open tasks"),
        "the page was regenerated: {page}"
    );

    // The workspace the preset is for: an index the views hang under, a task
    // that opens as `status: open`, and a term the vocabulary refuses.
    ok(
        &dir,
        &["new", "Tasks", "--in", "index.md", "--set", "status=null"],
    );
    ok(&dir, &["new", "Fix the build", "--in", "tasks.md"]);
    let task = read(&dir, "fix-the-build.md");
    assert!(
        task.contains("status: open\n") && task.contains("created: 20"),
        "{task}"
    );
    let (out, err) = ok(&dir, &["check"]);
    assert!(err.contains("no findings"), "{out}{err}");
    let (out, _) = ok(&dir, &["views", "open-tasks"]);
    assert!(
        out.contains("open (1)") && out.contains("fix-the-build.md"),
        "{out}"
    );

    ok(&dir, &["set", "fix-the-build.md", "status", "wontfix"]);
    let (ok_, out, err) = run(&dir, &["check"]);
    assert!(!ok_ && (out + &err).contains("not a known term"));
    ok(&dir, &["set", "fix-the-build.md", "status", "done"]);
    let (out, _) = ok(&dir, &["views", "open-tasks"]);
    assert!(out.contains("no documents in scope"), "{out}");

    // Applying it again finds nothing to add.
    let (out, err) = ok(&dir, &["presets", &preset, "--write"]);
    assert!(!out.contains("+ ") && out.contains("(already"), "{out}");
    assert!(err.contains("nothing to write"), "{err}");
}

#[test]
fn a_collision_is_reported_and_nothing_is_written() {
    let dir = sandbox("collision");
    ok(&dir, &["init", "--yes"]);
    // The workspace has its own idea of `status`.
    let mut node = read(&dir, "prov.yaml");
    node.push_str("fields:\n  status:\n    type: str\n");
    std::fs::write(dir.join("prov.yaml"), node).unwrap();

    let preset = tasks_preset();
    let (ok_, out, err) = run(&dir, &["presets", &preset, "--write"]);
    assert!(!ok_);
    assert!(
        out.contains("! fields.status  (declared differently)"),
        "{out}"
    );
    assert!(
        out.contains("+ views.open-tasks"),
        "the rest of the plan is still shown: {out}"
    );
    assert!(err.contains("1 collision"), "{err}");
    assert!(!dir.join("vocab/statuses.yaml").exists(), "nothing written");
    assert!(
        !read(&dir, "prov.yaml").contains("open-tasks"),
        "not even the clean entries"
    );
}

#[test]
fn a_directory_that_is_not_a_preset_is_refused() {
    let dir = sandbox("not-a-preset");
    ok(&dir, &["init", "--yes"]);
    let empty = sandbox("not-a-preset-dir");
    let (ok_, _, err) = run(&dir, &["presets", empty.to_str().unwrap()]);
    assert!(!ok_ && err.contains("not a preset"), "{err}");

    std::fs::write(empty.join("prov.yaml"), "veiws: {}\n").unwrap();
    let (ok_, _, err) = run(&dir, &["presets", empty.to_str().unwrap()]);
    assert!(
        !ok_ && err.contains("veiws") && err.contains("views"),
        "{err}"
    );
}
