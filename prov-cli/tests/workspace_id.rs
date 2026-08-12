//! `prov id --workspace [NAME]` — giving the *workspace* the name other
//! archives reference it by (`id:<name>/<id>`), rather than giving a document an
//! id within it.
//!
//! Two properties carry the whole design, and both are here: the name is
//! **never minted on prov's own initiative** (an anonymous workspace stays
//! anonymous until asked), and once set it is **never changed by a rerun** —
//! because by then it is out in the world, written into references this
//! workspace cannot see. Renaming stays available, and stays deliberate
//! (`prov config workspace_id <name>`).

use std::path::Path;
use std::process::Command;

/// Run a command, keeping the streams apart: stdout carries the name, stderr the
/// narration (`output_streams.rs` states the contract this relies on).
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

fn vault(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("prov-wsid-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (ok, _, err) = run(&dir, &["init", "--yes"]);
    assert!(ok, "init: {err}");
    dir
}

/// The name in effect, read back through `config` — the same surface a user or
/// another tool would read it from.
fn effective(dir: &Path) -> String {
    run(dir, &["config", "workspace_id"]).1.trim().to_string()
}

#[test]
fn a_workspace_is_anonymous_until_asked() {
    let dir = vault("anonymous");
    assert_eq!(effective(&dir), "", "init --yes leaves it unnamed");
    // Everything else works meanwhile: anonymity is a limit on being referenced
    // from outside, not on functioning.
    assert!(run(&dir, &["check"]).0);
}

#[test]
fn a_chosen_name_is_written_to_config_and_printed() {
    let dir = vault("chosen");
    let (ok, out, err) = run(&dir, &["id", "--workspace", "notes"]);
    assert!(ok, "{err}");
    assert_eq!(out.trim(), "notes", "the name on stdout, bare");
    assert!(err.contains("prov.yaml"), "the file named on stderr: {err}");
    assert_eq!(effective(&dir), "notes");
    assert!(run(&dir, &["check"]).0, "a named workspace is clean");
}

#[test]
fn a_bare_flag_mints_an_opaque_global_name() {
    let dir = vault("minted");
    let (ok, out, err) = run(&dir, &["id", "--workspace"]);
    assert!(ok, "{err}");
    let name = out.trim();
    // Wider than a document id (6+1) on purpose: nothing can check a workspace
    // name against the other workspaces in the world, so width is the only
    // uniqueness available.
    assert_eq!(name.chars().count(), prov::WORKSPACE_NAME_LEN);
    assert!(
        prov::is_valid_workspace_id(name),
        "{name} is writable as a qualifier"
    );
    assert_eq!(effective(&dir), name, "and it is what the config now says");
    assert!(run(&dir, &["check"]).0);
}

#[test]
fn two_workspaces_mint_different_names() {
    let a = vault("distinct-a");
    let b = vault("distinct-b");
    assert_ne!(
        run(&a, &["id", "--workspace"]).1.trim(),
        run(&b, &["id", "--workspace"]).1.trim(),
        "the seed differs per run, so the names do"
    );
}

#[test]
fn rerunning_prints_the_existing_name_and_writes_nothing() {
    let dir = vault("idempotent");
    let first = run(&dir, &["id", "--workspace"]).1.trim().to_string();
    let before = std::fs::read_to_string(dir.join("prov.yaml")).unwrap();

    let (ok, out, _) = run(&dir, &["id", "--workspace"]);
    assert!(ok);
    assert_eq!(out.trim(), first, "the same name, not a second one");
    assert_eq!(
        std::fs::read_to_string(dir.join("prov.yaml")).unwrap(),
        before,
        "config untouched on a rerun"
    );
}

/// The rule with teeth: a name already out in the world is not silently
/// replaced. The command refuses and points at the deliberate way to rename.
#[test]
fn a_second_different_name_is_refused_rather_than_applied() {
    let dir = vault("no-rename");
    assert!(run(&dir, &["id", "--workspace", "notes"]).0);

    let (ok, _, err) = run(&dir, &["id", "--workspace", "archive"]);
    assert!(!ok, "renaming through `id` is refused");
    assert!(
        err.contains("already named") && err.contains("prov config workspace_id"),
        "and says how to do it on purpose: {err}"
    );
    assert_eq!(effective(&dir), "notes", "unchanged");

    // The deliberate route works.
    assert!(run(&dir, &["config", "workspace_id", "archive"]).0);
    assert_eq!(effective(&dir), "archive");
}

#[test]
fn a_name_that_cannot_be_written_as_a_qualifier_is_refused() {
    let dir = vault("malformed");
    for bad in ["a/b", "a:b", "a b", ""] {
        let (ok, _, err) = run(&dir, &["id", "--workspace", bad]);
        assert!(!ok, "accepted {bad:?}");
        assert!(err.contains("not a valid workspace name"), "{err}");
        assert_eq!(effective(&dir), "", "still anonymous after {bad:?}");
    }
}

/// The mint draws from an alphabet that includes the digits, so an all-digit
/// name is a real (if unlikely) outcome. It must survive the round trip through
/// YAML as a *string*: read back as an integer it would be diagnosed malformed
/// and ignored, leaving the workspace silently anonymous right after being told
/// it was named. Both write paths are checked, since either can produce one.
#[test]
fn an_all_digit_name_stays_a_string() {
    let dir = vault("digits");
    assert!(run(&dir, &["id", "--workspace", "123456789012"]).0);
    let text = std::fs::read_to_string(dir.join("prov.yaml")).unwrap();
    assert!(
        text.contains("workspace_id: '123456789012'"),
        "quoted in config: {text}"
    );
    assert_eq!(effective(&dir), "123456789012", "and applies, not ignored");
    assert!(run(&dir, &["check"]).0, "no MalformedWorkspaceId finding");

    let other = vault("digits-config");
    assert!(run(&other, &["config", "workspace_id", "987654321098"]).0);
    assert_eq!(effective(&other), "987654321098");
}

/// Naming the workspace is not document identity, and does not consult that
/// axis: `identity: off` means no document ever earns an id, which says nothing
/// about whether this archive can be named from outside.
#[test]
fn naming_the_workspace_works_with_document_identity_off() {
    let dir = vault("identity-off");
    assert!(run(&dir, &["config", "identity", "off"]).0);
    // `prov id <doc>` refuses here …
    assert!(!run(&dir, &["id", "index.md"]).0);
    // … and `prov id --workspace` does not.
    let (ok, out, err) = run(&dir, &["id", "--workspace"]);
    assert!(ok, "{err}");
    assert_eq!(effective(&dir), out.trim());
}
