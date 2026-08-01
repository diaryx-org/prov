//! A per-command smoke test: every non-interactive subcommand run once, end to
//! end, against a real workspace built by the earlier commands.
//!
//! This is breadth, not depth — the detailed behavior of routes, config, and the
//! target grammar lives in `targets.rs`. The job here is to catch a command that
//! panics, mis-parses its arguments, or regresses to a non-zero exit: the class of
//! break that a library refactor (the CLI is a thin adapter over one) can cause
//! without any single command's own tests noticing. Each command is asserted on
//! its exit status; output is spot-checked only where a word proves the command
//! actually did its job.

use std::path::Path;
use std::process::Command;

fn run(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_prov"))
        .current_dir(dir)
        .args(args)
        .env("PROV_QUIET", "1")
        // `edit` shells out to $EDITOR; `true` makes it a successful no-op so the
        // command's own bookkeeping (not the editor) is what's under test.
        .env("EDITOR", "true")
        .output()
        .expect("run prov");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

/// Assert a command succeeded, surfacing its combined output on failure.
fn ok(dir: &Path, args: &[&str]) -> String {
    let (ok, out) = run(dir, args);
    assert!(ok, "`prov {}` failed:\n{out}", args.join(" "));
    out
}

fn sandbox(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("prov-smoke-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn every_command_runs_end_to_end() {
    let dir = sandbox("all");

    // ── create a workspace and grow a small tree ──
    ok(&dir, &["init", "--yes"]);
    ok(&dir, &["new", "Rust", "--in", "index.md"]);
    ok(&dir, &["new", "Zig", "--in", "index.md"]);

    // ── single-document readers ──
    let show = ok(&dir, &["show", "index.md"]);
    assert!(
        show.contains("Rust") && show.contains("Zig"),
        "show lists children: {show}"
    );
    ok(&dir, &["links", "index.md"]);
    ok(&dir, &["meta", "index.md"]);
    assert_eq!(ok(&dir, &["get", "rust.md", "title"]).trim(), "Rust");
    ok(&dir, &["body", "rust.md"]);
    ok(&dir, &["render", "rust.md"]);

    // ── metadata editing (format-preserving) ──
    ok(&dir, &["set", "rust.md", "summary", "notes on rust"]);
    assert_eq!(
        ok(&dir, &["get", "rust.md", "summary"]).trim(),
        "notes on rust"
    );
    ok(&dir, &["unset", "rust.md", "summary"]);
    // ── `edit` with EDITOR=true: a no-op edit still exits cleanly ──
    ok(&dir, &["edit", "rust.md"]);

    // ── structure views ──
    let tree = ok(&dir, &["tree"]);
    assert!(
        tree.contains("Rust") && tree.contains("Zig"),
        "tree shows the vault: {tree}"
    );
    ok(&dir, &["check"]); // a fresh vault is consistent → exit 0
    ok(&dir, &["backlinks", "index.md"]);

    // ── stable IDs ──
    let id_out = ok(&dir, &["id", "rust.md"]);
    let id = id_out
        .lines()
        .find(|l| l.starts_with("id:"))
        .expect("id printed")
        .to_string();
    assert_eq!(ok(&dir, &["resolve", &id]).trim(), "rust.md");

    // ── attach a non-document file ──
    std::fs::write(dir.join("logo.png"), b"\x89PNGfake").unwrap();
    ok(&dir, &["attach", "logo.png"]);

    // ── move / reparent / duplicate ──
    ok(&dir, &["mv", "rust.md", "notes/rust.md"]);
    ok(&dir, &["reparent", "notes/rust.md", "--in", "zig.md"]);
    ok(&dir, &["duplicate", "zig.md"]);

    // ── convert a document's link spelling ──
    ok(&dir, &["convert", "index.md", "path_style", "relative"]);

    // ── recycle bin: delete → restore → empty ──
    ok(&dir, &["rm", "zig-copy.md"]);
    ok(&dir, &["restore", "zig-copy.md"]);
    ok(&dir, &["rm", "zig-copy.md"]);
    ok(&dir, &["empty-bin"]);

    // ── history: capture → read → restore ──
    ok(&dir, &["config", "history", "manual"]);
    let event = ok(&dir, &["history-capture", "--label", "smoke"])
        .lines()
        .next()
        .expect("capture prints the event id")
        .to_string();
    ok(&dir, &["history-list"]);
    ok(&dir, &["history-show", &event]);
    ok(&dir, &["history-log", "index.md"]);
    ok(&dir, &["history-restore", &event, "--dry-run"]);
    std::fs::write(dir.join("notes/rust.md"), "clobbered by a sync conflict").unwrap();
    let restored = ok(&dir, &["history-restore", &event]);
    assert!(
        restored.contains("notes/rust.md"),
        "restore names what it wrote back: {restored}"
    );
    ok(&dir, &["history-restore", &event, "--exact", "--yes"]);
    ok(&dir, &["history-prune", "--keep", "5", "--dry-run"]);
    ok(&dir, &["history-prune", "--keep", "1", "--yes"]);
    // The bound is required: a verb that deletes bytes must not have a default.
    let (bounded, out) = run(&dir, &["history-prune"]);
    assert!(!bounded, "history-prune needs a bound: {out}");

    // ── config: read, write, materialize ──
    ok(&dir, &["config"]);
    assert_eq!(ok(&dir, &["config", "identity"]).trim(), "lazy");
    ok(&dir, &["config", "references.target", "id"]);
    ok(&dir, &["config", "--setup"]);

    // ── backup: whole-tree copy, outside the graph entirely ──
    let backup_dir = dir.parent().unwrap().join("smoke-all-backup");
    let _ = std::fs::remove_dir_all(&backup_dir);
    ok(&dir, &["backup", "--to", backup_dir.to_str().unwrap()]);
    assert!(backup_dir.join("index.md").exists());
    let backup_zip = dir.parent().unwrap().join("smoke-all-backup.zip");
    let _ = std::fs::remove_file(&backup_zip);
    ok(
        &dir,
        &["backup", "--to", backup_zip.to_str().unwrap(), "--zip"],
    );
    assert!(backup_zip.is_file());
}

#[test]
fn a_failing_command_exits_nonzero() {
    // The negative control: `check` reports and *fails* on a broken workspace, so a
    // smoke run that only ever saw exit 0 would prove nothing. Break the inverse
    // link and confirm the non-zero exit the CI contract relies on.
    let dir = sandbox("fails");
    ok(&dir, &["init", "--yes"]);
    ok(&dir, &["new", "Loose", "--in", "index.md"]);
    ok(&dir, &["unset", "loose.md", "part_of"]);
    let (ok_status, out) = run(&dir, &["check"]);
    assert!(!ok_status, "check must fail on a missing inverse: {out}");
    assert!(out.contains("part_of"), "and name the problem: {out}");
}
