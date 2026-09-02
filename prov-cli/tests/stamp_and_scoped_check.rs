//! `prov stamp`, and `check`'s `--only` / `--json` flags.
//!
//! The three exist for one situation: a document that changed **outside prov**.
//! `check --fix` cannot settle it — its re-stamp is a judgment (so `--fix
//! mechanical` skips it) and it never writes `updated`, because nothing on disk
//! tells it when the edit happened. `stamp` is the user supplying that missing
//! half; `--only` is how a per-file question gets a per-file answer without
//! narrowing the walk past the evidence; `--json` is the same answer for a
//! script.

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
    let dir = std::env::temp_dir().join(format!("prov-stamp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A workspace whose documents are **separated** — each one a small metadata
/// node beside the prose file it names — and which records an `updated` field.
/// The only configuration in which `stamp` has both halves to write, because a
/// checksum is recorded exactly where it covers a file of its own: `note.yaml`
/// holds the hash, `note.md` holds the bytes it vouches for.
fn workspace(tag: &str) -> std::path::PathBuf {
    let dir = sandbox(tag);
    ok(&dir, &["init", "--yes", "--embed", "separate"]);
    ok(&dir, &["config", "updated", "updated"]);
    dir
}

/// Create a document in the root — a `<slug>.yaml` node and the `<slug>.md`
/// body it points at.
fn new_doc(dir: &Path, title: &str) {
    ok(dir, &["new", title, "--in", "index.yaml"]);
}

fn read(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap()
}

/// Append to a document's *body file* without prov's knowledge — the whole
/// premise. `name` is the node; the bytes its checksum covers sit beside it.
fn edit_externally(dir: &Path, name: &str) {
    let body = name.replace(".yaml", ".md");
    let text = read(dir, &body);
    std::fs::write(dir.join(&body), format!("{text}\nedited elsewhere\n")).unwrap();
}

#[test]
fn stamp_records_an_out_of_band_edit_and_is_a_no_op_when_run_again() {
    let dir = workspace("roundtrip");
    new_doc(&dir, "Note");
    edit_externally(&dir, "note.yaml");

    // First stamp: the document had no checksum, so both halves land.
    let (out, err) = ok(&dir, &["stamp", "note.yaml"]);
    assert_eq!(out.trim(), "note.yaml", "stdout is the bare path: {out:?}");
    assert!(err.contains("checksum"), "narrates the checksum: {err:?}");
    let text = read(&dir, "note.yaml");
    assert!(text.contains("content_hash: sha256:"), "{text}");
    assert!(text.contains("updated: "), "{text}");
    // `check` is the independent verdict: a stamped document is a clean one.
    ok(&dir, &["check"]);

    // Second stamp, nothing changed: writes nothing, says so, and leaves the
    // file byte-identical. This is what makes it safe in a sync hook.
    let before = read(&dir, "note.yaml");
    let (out, err) = ok(&dir, &["stamp", "note.yaml"]);
    assert_eq!(out.trim(), "", "an unchanged document is not restamped");
    assert!(err.contains("nothing to stamp"), "{err:?}");
    assert_eq!(read(&dir, "note.yaml"), before, "re-running must not write");

    // A real out-of-band edit is caught, and both stamps move again.
    edit_externally(&dir, "note.yaml");
    let stale = read(&dir, "note.yaml");
    let (out, _) = ok(&dir, &["stamp", "note.yaml"]);
    assert_eq!(out.trim(), "note.yaml");
    let fresh = read(&dir, "note.yaml");
    assert_ne!(
        stale.lines().find(|l| l.starts_with("content_hash:")),
        fresh.lines().find(|l| l.starts_with("content_hash:")),
        "the checksum follows the bytes"
    );
    assert_ne!(
        stale.lines().find(|l| l.starts_with("updated:")),
        fresh.lines().find(|l| l.starts_with("updated:")),
        "the timestamp follows the edit"
    );
}

#[test]
fn stamp_no_timestamp_moves_only_the_checksum() {
    let dir = workspace("no-timestamp");
    new_doc(&dir, "Note");
    edit_externally(&dir, "note.yaml");

    ok(&dir, &["stamp", "note.yaml", "--no-timestamp"]);
    let text = read(&dir, "note.yaml");
    assert!(text.contains("content_hash: sha256:"), "{text}");
    assert!(
        !text.contains("updated: "),
        "--no-timestamp claims no edit time: {text}"
    );
    ok(&dir, &["check"]);
}

#[test]
fn stamp_all_seeds_missing_checksums_but_claims_an_edit_time_for_none_of_them() {
    let dir = workspace("all-seeds");
    new_doc(&dir, "Alpha");
    new_doc(&dir, "Beta");

    // Nothing has a checksum yet, and nothing was edited. The sweep owes every
    // document a checksum (it only restates the bytes) and owes none of them an
    // `updated` (it has no evidence any of them changed) — the distinction the
    // whole flag turns on.
    let (out, err) = ok(&dir, &["stamp", "--all"]);
    assert!(
        out.contains("alpha.yaml"),
        "stdout lists what moved: {out:?}"
    );
    assert!(err.contains("seeded"), "{err:?}");
    for name in ["alpha.yaml", "beta.yaml"] {
        let text = read(&dir, name);
        assert!(text.contains("content_hash: sha256:"), "{name}: {text}");
        assert!(
            !text.contains("updated: "),
            "a sweep must not claim an edit time for {name}: {text}"
        );
    }
    ok(&dir, &["check"]);

    // Run again with nothing changed: no writes at all.
    let before: Vec<String> = ["alpha.yaml", "beta.yaml"]
        .iter()
        .map(|n| read(&dir, n))
        .collect();
    let (out, _) = ok(&dir, &["stamp", "--all"]);
    assert_eq!(out.trim(), "", "a settled workspace restamps nothing");
    for (name, was) in ["alpha.yaml", "beta.yaml"].iter().zip(&before) {
        assert_eq!(&read(&dir, name), was, "{name} must be untouched");
    }

    // Now edit one out of band. Only that one is stamped, and only it earns the
    // timestamp — the other is not touched at all.
    edit_externally(&dir, "alpha.yaml");
    let beta_before = read(&dir, "beta.yaml");
    let (out, _) = ok(&dir, &["stamp", "--all"]);
    assert_eq!(
        out.trim(),
        "alpha.yaml",
        "only the drifted document: {out:?}"
    );
    assert!(read(&dir, "alpha.yaml").contains("updated: "));
    assert_eq!(read(&dir, "beta.yaml"), beta_before);
}

#[test]
fn stamp_dry_run_writes_nothing() {
    let dir = workspace("dry-run");
    new_doc(&dir, "Note");
    edit_externally(&dir, "note.yaml");
    let before = read(&dir, "note.yaml");

    let (out, err) = ok(&dir, &["stamp", "note.yaml", "--dry-run"]);
    assert_eq!(out.trim(), "note.yaml", "stdout still names the target");
    assert!(err.contains("would stamp"), "{err:?}");
    assert_eq!(read(&dir, "note.yaml"), before, "--dry-run must not write");
}

#[test]
fn stamp_needs_a_target_or_all() {
    let dir = workspace("no-target");
    let (ok_, _, err) = run(&dir, &["stamp"]);
    assert!(!ok_, "a bare `stamp` is not a whole instruction");
    assert!(
        err.contains("--all"),
        "and it says what is missing: {err:?}"
    );
}

#[test]
fn check_only_reports_the_findings_lodged_against_one_document() {
    let dir = workspace("only");
    new_doc(&dir, "Alpha");
    new_doc(&dir, "Beta");
    // Both start settled; only alpha then drifts. A document with no checksum
    // on record cannot drift, so the baseline is what makes the edit visible.
    ok(&dir, &["stamp", "--all"]);
    edit_externally(&dir, "alpha.yaml");

    // Unscoped: alpha's drift is in there somewhere.
    let (_, out, _) = run(&dir, &["check"]);
    assert!(out.contains("alpha.yaml"), "{out:?}");

    // Scoped to alpha: the same finding, and the exit code still reports it.
    let (success, out, err) = run(&dir, &["check", "--only", "alpha.yaml"]);
    assert!(!success, "findings still fail the exit code");
    assert!(out.contains("alpha.yaml"), "{out:?}");
    assert!(err.contains("for alpha.yaml"), "names the scope: {err:?}");

    // Scoped to beta: genuinely clean, and exits 0.
    let (success, out, err) = run(&dir, &["check", "--only", "beta.yaml"]);
    assert!(success, "a clean subject exits 0: {err}");
    assert_eq!(out.trim(), "");
    assert!(err.contains("no findings"), "{err:?}");

    // Stamping alpha settles the scoped question too.
    ok(&dir, &["stamp", "alpha.yaml"]);
    ok(&dir, &["check", "--only", "alpha.yaml"]);
}

#[test]
fn check_only_sees_a_relational_finding_a_scoped_walk_could_not() {
    let dir = workspace("relational");
    new_doc(&dir, "Child");
    // Strip the back-link: the child no longer says who contains it. The
    // evidence lives in the *root*, which is why `--only` filters results
    // rather than narrowing the walk — checking *from* `child.yaml` cannot see
    // this, and would report the file clean.
    ok(&dir, &["unset", "child.yaml", "part_of"]);

    let (success, out, _) = run(&dir, &["check", "--only", "child.yaml"]);
    assert!(!success, "the child is not clean");
    assert!(
        out.contains("part_of"),
        "the missing inverse is the child's finding: {out:?}"
    );

    // And it is filed against the child — the document a repair rewrites — not
    // against the parent that reported it.
    let (_, out, _) = run(&dir, &["check", "--only", "index.yaml", "--json"]);
    assert!(
        !out.contains("missing_inverse"),
        "the parent is not the subject: {out:?}"
    );

    // Scoped `--fix` repairs it, and the scoped diff does not count the rest of
    // the workspace as newly introduced.
    let (_, _, err) = run(
        &dir,
        &["check", "--only", "child.yaml", "--fix", "mechanical"],
    );
    assert!(err.contains("1 finding(s) resolved"), "{err:?}");
    assert!(read(&dir, "child.yaml").contains("part_of"));
}

#[test]
fn check_only_refuses_a_path_that_is_not_in_the_workspace() {
    let dir = workspace("only-typo");
    // The failure mode this guard exists for: a typo would filter every finding
    // away and print "no findings", which is indistinguishable from a clean
    // bill of health.
    let (success, _, err) = run(&dir, &["check", "--only", "nope.md"]);
    assert!(!success, "a subject that does not exist is an error");
    assert!(err.contains("no such document"), "{err:?}");
}

#[test]
fn check_json_is_parseable_and_carries_kind_subject_and_message() {
    let dir = workspace("json");
    new_doc(&dir, "Alpha");
    ok(&dir, &["stamp", "alpha.yaml"]);
    edit_externally(&dir, "alpha.yaml");

    let (_, out, err) = run(&dir, &["check", "--json"]);
    // Nothing on stderr: the count line is narration for a person, and this is
    // the mode where there isn't one. A shell that shows stderr inline would
    // otherwise print it over the top of a pipeline's result.
    assert_eq!(err, "", "--json narrates nothing: {err:?}");
    // No JSON parser in the test deps either, so this checks the shape the flag
    // promises rather than re-implementing one: the three common keys, the
    // variant's own fields, and a bracketed array around them.
    assert!(out.starts_with("[\n"), "{out:?}");
    assert!(out.trim_end().ends_with(']'), "{out:?}");
    assert!(out.contains("\"kind\": \"fixity_mismatch\""), "{out:?}");
    assert!(out.contains("\"subject\": \"alpha.yaml\""), "{out:?}");
    assert!(out.contains("\"message\": \"alpha.yaml: fixity"), "{out:?}");
    assert!(out.contains("\"recorded\": \"sha256:"), "{out:?}");
    assert!(out.contains("\"actual\": \"sha256:"), "{out:?}");

    // Clean prints `[]`, not nothing — so a consumer can tell a clean run from
    // a run that produced no output for some other reason.
    ok(&dir, &["stamp", "alpha.yaml"]);
    let (success, out, err) = run(&dir, &["check", "--json"]);
    assert!(success);
    assert_eq!(out.trim(), "[]", "{out:?}");
    assert_eq!(err, "", "a clean run is silent too: {err:?}");
}

#[test]
fn check_json_still_exits_non_zero_on_findings() {
    let dir = workspace("json-exit");
    new_doc(&dir, "Alpha");
    ok(&dir, &["stamp", "alpha.yaml"]);
    edit_externally(&dir, "alpha.yaml");

    // `--json` silences the narration but not the verdict: findings still fail
    // the exit code, which is what lets `prov check` stand as a CI gate. A shell
    // that treats a non-zero exit as a failed pipeline (nushell) has to capture
    // the status rather than pipe through it.
    let (success, out, _) = run(&dir, &["check", "--json"]);
    assert!(!success, "findings still exit non-zero under --json");
    assert!(out.contains("fixity_mismatch"), "{out:?}");
}

#[test]
fn check_json_and_fix_are_mutually_exclusive() {
    let dir = workspace("json-fix");
    // `--fix`'s stdout already means something else (the findings a repair
    // introduced), so the two output contracts are not combined.
    let (success, _, err) = run(&dir, &["check", "--json", "--fix", "mechanical"]);
    assert!(!success);
    assert!(err.contains("cannot be used with"), "{err:?}");
}

#[test]
fn stamp_all_leaves_a_shadowed_payload_untouched() {
    let dir = workspace("shadowed");
    // `attach --opaque` shadows a file prov *could* read — a specimen it is
    // holding without interpreting. Any `content_hash` inside one describes the
    // exhibit, not this workspace, so a sweep must not parse it, compare it, or
    // rewrite it. The sidecar beside it carries the workspace's own checksum of
    // the same bytes, which is how its fixity is actually kept.
    std::fs::write(
        dir.join("specimen.md"),
        "---\ntitle: Someone Else's Note\ncontent_hash: sha256:0000\n---\nnot ours\n",
    )
    .unwrap();
    ok(&dir, &["attach", "specimen.md", "--opaque"]);
    let before = read(&dir, "specimen.md");

    ok(&dir, &["stamp", "--all"]);
    assert_eq!(
        read(&dir, "specimen.md"),
        before,
        "a shadowed payload is not this workspace's to stamp"
    );
    ok(&dir, &["check"]);
}
