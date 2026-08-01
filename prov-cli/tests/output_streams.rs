//! The output-stream contract: **stdout carries the machine value, stderr the
//! human narration.** A mutation prints the identifier(s) of the object it
//! produced — one per line, undecorated — to stdout, and everything a person
//! reads ("created …", "moved …") to stderr; a reader prints its data to stdout
//! and any incidental chatter ("ok: no findings") to stderr. Success is the exit
//! code, so `2>/dev/null` silences narration without eating data and
//! `$(prov new …)` captures a bare, pipeable path.
//!
//! Unlike `smoke.rs` (which merges the two streams to check exit status), this
//! test keeps them apart on purpose — it is the regression guard for *which
//! stream* each token lands on.

use std::path::Path;
use std::process::Command;

/// Run a command, returning `(success, stdout, stderr)` as three separate values.
fn run(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_prov"))
        .current_dir(dir)
        .args(args)
        .env("PROV_QUIET", "1")
        .env("EDITOR", "true")
        .output()
        .expect("run prov");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Run a command with `input` on stdin — for the interactive prompts (`check
/// --fix`) that `output()`'s null stdin would otherwise answer with EOF.
fn run_with_input(dir: &Path, args: &[&str], input: &str) -> (bool, String, String) {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new(env!("CARGO_BIN_EXE_prov"))
        .current_dir(dir)
        .args(args)
        .env("PROV_QUIET", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn prov");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("run prov");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Assert a command succeeded, surfacing both streams on failure.
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
    let dir = std::env::temp_dir().join(format!("prov-streams-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn mutations_put_the_resulting_path_on_stdout_and_narration_on_stderr() {
    let dir = sandbox("mutations");

    // `init` — stdout is the root document's path; the friendly report is stderr.
    let (out, err) = ok(&dir, &["init", "--yes"]);
    assert!(
        out.trim().ends_with("index.md"),
        "init stdout is the root doc path: {out:?}"
    );
    assert!(
        err.contains("initialized"),
        "init narrates on stderr: {err:?}"
    );
    assert!(
        !out.contains("initialized"),
        "no narration leaks onto stdout: {out:?}"
    );

    // `new` — stdout is exactly the created node path, nothing else.
    let (out, err) = ok(&dir, &["new", "Rust", "--in", "index.md"]);
    assert_eq!(
        out.trim(),
        "rust.md",
        "new stdout is the bare path: {out:?}"
    );
    assert!(err.contains("created"), "new narrates on stderr: {err:?}");

    // The stdout path is real and pipeable: it round-trips straight into a reader.
    let (title, _) = ok(&dir, &["get", out.trim(), "title"]);
    assert_eq!(
        title.trim(),
        "Rust",
        "the captured path is usable: {title:?}"
    );

    // `mv` — stdout is the destination (the new handle), narration on stderr.
    let (out, err) = ok(&dir, &["mv", "rust.md", "notes/rust.md"]);
    assert_eq!(
        out.trim(),
        "notes/rust.md",
        "mv stdout is the destination: {out:?}"
    );
    assert!(err.contains("moved"), "mv narrates on stderr: {err:?}");

    // `duplicate` — stdout is the copy's path.
    ok(&dir, &["new", "Zig", "--in", "index.md"]);
    let (out, err) = ok(&dir, &["duplicate", "zig.md"]);
    assert_eq!(
        out.trim(),
        "zig-copy.md",
        "duplicate stdout is the copy: {out:?}"
    );
    assert!(
        err.contains("duplicated"),
        "duplicate narrates on stderr: {err:?}"
    );

    // `set`/`unset` — stdout is the edited document's path (was previously silent).
    let (out, _) = ok(&dir, &["set", "zig.md", "summary", "a note"]);
    assert_eq!(
        out.trim(),
        "zig.md",
        "set stdout is the edited path: {out:?}"
    );
    let (out, _) = ok(&dir, &["unset", "zig.md", "summary"]);
    assert_eq!(
        out.trim(),
        "zig.md",
        "unset stdout is the edited path: {out:?}"
    );
}

#[test]
fn an_idempotent_no_op_still_yields_the_path_the_contract_is_the_result() {
    // The stdout contract is the *resulting object*, not the *action taken*: a
    // `new -p` that finds the document already there prints the same path, so a
    // daily-note cron's `$(prov new -p …)` is stable across first and later runs.
    let dir = sandbox("idempotent");
    ok(&dir, &["init", "--yes"]);

    let (out, err) = ok(&dir, &["new", "Today", "--in", "index.md", "-p"]);
    assert_eq!(out.trim(), "today.md", "first run: {out:?}");
    assert!(
        err.contains("created"),
        "first run narrates create: {err:?}"
    );

    let (out, err) = ok(&dir, &["new", "Today", "--in", "index.md", "-p"]);
    assert_eq!(
        out.trim(),
        "today.md",
        "re-run yields the same path: {out:?}"
    );
    assert!(err.contains("exists"), "re-run narrates a no-op: {err:?}");
}

#[test]
fn a_dry_run_narrates_but_emits_nothing_pipeable() {
    // `--dry-run` previews on stderr and leaves stdout empty — nothing was created,
    // so there is no object to name. A pipeline reading stdout acts on nothing.
    let dir = sandbox("dryrun");
    ok(&dir, &["init", "--yes"]);
    let (out, err) = ok(&dir, &["new", "Draft", "--in", "index.md", "--dry-run"]);
    assert!(out.trim().is_empty(), "dry-run stdout is empty: {out:?}");
    assert!(
        err.contains("would create"),
        "dry-run previews on stderr: {err:?}"
    );
}

#[test]
fn convert_lists_the_changed_paths_on_stdout() {
    // A sweep's stdout is the set of documents it actually rewrote, one per line —
    // the `| git add` handle — with the count as stderr narration.
    let dir = sandbox("convert");
    ok(&dir, &["init", "--yes"]);
    ok(&dir, &["new", "A", "--in", "index.md"]);
    let (out, err) = ok(&dir, &["convert", "index.md", "path_style", "relative"]);
    assert_eq!(
        out.trim(),
        "index.md",
        "convert stdout is the changed path: {out:?}"
    );
    assert!(
        err.contains("converted"),
        "convert narrates the count on stderr: {err:?}"
    );
}

#[test]
fn readers_keep_data_on_stdout_and_chatter_on_stderr() {
    let dir = sandbox("readers");
    ok(&dir, &["init", "--yes"]);

    // `check` on a clean workspace: stdout empty (no findings), the "ok" on stderr.
    let (out, err) = ok(&dir, &["check"]);
    assert!(
        out.trim().is_empty(),
        "clean check stdout is empty: {out:?}"
    );
    assert!(err.contains("ok"), "clean check says ok on stderr: {err:?}");

    // `config <key>` is a reader: the value is stdout, and nothing else.
    let (out, _) = ok(&dir, &["config", "identity"]);
    assert_eq!(out.trim(), "lazy", "config get value on stdout: {out:?}");

    // `config <key> <value>` (a mutation) echoes the value on stdout, "set …" on
    // stderr.
    let (out, err) = ok(&dir, &["config", "references.target", "id"]);
    assert_eq!(
        out.trim(),
        "id",
        "config set echoes the value on stdout: {out:?}"
    );
    assert!(
        err.contains("set"),
        "config set narrates on stderr: {err:?}"
    );

    // `backlinks` with no results: stdout empty, the "no backlinks" note on stderr.
    let (out, err) = ok(&dir, &["backlinks", "index.md"]);
    assert!(
        out.trim().is_empty(),
        "empty backlinks stdout is empty: {out:?}"
    );
    assert!(
        err.contains("no backlinks"),
        "the note is on stderr: {err:?}"
    );
}

#[test]
fn a_fix_sweep_reports_the_outcome_of_a_second_check_not_the_first() {
    // `--fix` mutates the graph, so "applied N" is effort, not outcome. It
    // re-checks and reports the difference — and the only bucket that is this
    // run's own doing (what it introduced) is the data, on stdout.
    let dir = sandbox("fix-diff");
    ok(&dir, &["init", "--yes"]);
    ok(&dir, &["new", "Rust", "--in", "index.md"]);

    // Break the inverse, so there is exactly one finding and it is fixable.
    ok(&dir, &["unset", "rust.md", "part_of"]);
    let (still_ok, out, _) = run(&dir, &["check"]);
    assert!(!still_ok, "a broken inverse should fail check");
    assert!(out.contains("part_of"), "the finding is on stdout: {out:?}");

    let (ok_status, out, err) = run_with_input(&dir, &["check", "--fix"], "y\n");
    assert!(ok_status, "a fix that breaks nothing exits zero:\n{err}");
    assert!(
        err.contains("1 finding(s) resolved") && err.contains("0 introduced"),
        "the three buckets are stderr narration: {err:?}"
    );
    assert!(
        !out.contains("introduced"),
        "stdout carries findings, never the summary: {out:?}"
    );

    // And the repair was real, not just reported.
    let (clean, _, err) = run(&dir, &["check"]);
    assert!(clean, "the workspace should be clean now: {err}");
}

#[test]
fn the_history_readers_keep_the_manifest_on_stdout_and_the_warning_on_stderr() {
    // `history-show` and `history-log` are readers over what a capture wrote, and
    // both carry a *caveat* — "these bytes have not arrived", "this lineage is
    // keyed by path". The caveat is narration: it must never contaminate the
    // manifest a script is reading.
    let dir = sandbox("history");
    ok(&dir, &["init", "--yes"]);
    ok(&dir, &["config", "history", "manual"]);
    ok(&dir, &["new", "Alpha", "--in", "index.md"]);

    let (out, _) = ok(&dir, &["history-capture", "--label", "first"]);
    let event = out.trim().to_string();
    assert!(!event.is_empty(), "capture stdout is the event id: {out:?}");

    // A fully-synced event: the manifest is the data, and there is nothing to say
    // about it beyond that.
    let (out, err) = ok(&dir, &["history-show", &event]);
    assert!(
        out.contains(&event) && out.contains("alpha.md") && out.contains("index.md"),
        "show puts the manifest on stdout: {out:?}"
    );
    assert!(
        err.trim().is_empty(),
        "a complete event warns about nothing: {err:?}"
    );

    // The half-synced case a sync transport actually produces: the event document
    // is here, its blobs are not. Still a successful read — the marked-up manifest
    // on stdout, the warning on stderr.
    std::fs::remove_dir_all(dir.join("history/blobs")).unwrap();
    let (out, err) = ok(&dir, &["history-show", &event]);
    assert!(
        out.contains("(bytes missing)"),
        "show marks the unrecoverable rows: {out:?}"
    );
    assert!(
        err.contains("no bytes in this store"),
        "the half-synced warning is stderr narration: {err:?}"
    );

    // `history-log`: the change points are stdout, the count and the path-key
    // caveat are stderr.
    let (out, err) = ok(&dir, &["history-log", "alpha.md"]);
    assert!(
        out.contains(&event) && out.contains("alpha.md"),
        "log puts the lineage on stdout: {out:?}"
    );
    assert!(
        !out.contains("change point"),
        "the count must not leak onto stdout: {out:?}"
    );
    assert!(
        err.contains("change point"),
        "log narrates the count on stderr: {err:?}"
    );

    // A document no capture ever saw: stdout empty, so a pipeline acts on nothing.
    let (out, err) = ok(&dir, &["history-log", "never.md"]);
    assert!(out.trim().is_empty(), "an empty lineage is empty: {out:?}");
    assert!(
        err.contains("no history event"),
        "the note is on stderr: {err:?}"
    );
}
