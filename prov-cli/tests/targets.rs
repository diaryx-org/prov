//! The CLI target grammar: one argument spelling, every addressing mode, every
//! slot.
//!
//! A document argument says how it addresses its target in the *value* — a bare
//! path, `@` for a route of titles, `id:` for a registry handle — mirroring the
//! library's `Addressing::{Path, Id, Alias}` and `Link::parse`, which have always
//! disambiguated by syntax. These tests exist because the grammar replaced a
//! flag-per-mode design (`--in-path` / `--in-title`) whose failure was *silent*:
//! a path handed to the route flag resolved several segments before dying, since
//! a route and a path are spelled alike. Under a grammar that class of confusion
//! is not caught — it is unrepresentable, which is the point worth pinning.

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

/// A date-nested journal: each index's title mirrors its own directory's name
/// (`2026` in `2026/`, `07` in `07/`) — the shape that made a path and a route
/// indistinguishable under the old flag pair, and the shape routes exist for.
fn journal(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("prov-targets-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    run(&dir, &["init", "--yes"]);
    let (ok, out) = run(&dir, &["new", "2026-07-14", "--in", "@Daily/2026/07", "-p"]);
    assert!(ok, "fixture route must build: {out}");
    dir
}

#[test]
fn a_route_and_a_path_name_the_same_parent_and_cannot_be_confused() {
    // The two spellings are now distinguished by the value itself, so the old
    // failure — a path silently walking as a route — has no way to occur.
    let dir = journal("equivalence");
    let (ok, out) = run(&dir, &["new", "By Route", "--in", "@Daily/2026/07"]);
    assert!(ok, "{out}");
    let (ok, out) = run(&dir, &["new", "By Path", "--in", "daily/2026/07/index.md"]);
    assert!(ok, "{out}");

    let index = std::fs::read_to_string(dir.join("daily/2026/07/index.md")).unwrap();
    assert!(
        index.contains("by-route.md"),
        "route-placed child linked: {index}"
    );
    assert!(
        index.contains("by-path.md"),
        "path-placed child linked: {index}"
    );
    let (ok, out) = run(&dir, &["check"]);
    assert!(ok, "workspace validates: {out}");
}

#[test]
fn a_subject_can_be_addressed_by_route_not_only_a_parent() {
    // The whole reason for the grammar: under flag-per-mode only the *parent*
    // could be named semantically, because only it had a flag to spare. A subject
    // positional has no flag, so it was path-only — backwards, since the subject
    // is what you know by meaning.
    let dir = journal("subject-route");
    let (ok, out) = run(&dir, &["show", "@Daily/2026/07"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("daily/2026/07/index.md"),
        "route resolved to the node: {out}"
    );
    assert!(out.contains("07"), "{out}");
}

#[test]
fn a_subject_can_be_addressed_by_id() {
    // Third mode, same slot, no third flag — the thing flag-per-mode could not do
    // without an --in-id and a fourth of every other argument.
    let dir = journal("subject-id");
    let (ok, out) = run(&dir, &["id", "daily/2026/07/2026-07-14.md"]);
    assert!(ok, "{out}");
    // `id` bootstraps the registry on first use, so it prints that too — take the
    // line that is actually the handle rather than the whole output.
    let id = out
        .lines()
        .find(|l| l.starts_with("id:"))
        .unwrap_or_else(|| panic!("id command prints an id target: {out}"))
        .to_string();

    let (ok, shown) = run(&dir, &["show", &id]);
    assert!(ok, "{shown}");
    assert!(
        shown.contains("2026-07-14"),
        "id resolved to the document: {shown}"
    );
}

#[test]
fn a_bare_path_needs_no_workspace() {
    // Root discovery is lazy: `show`/`meta`/`get`/`body` read a *file*, and only
    // @ and id: make the argument mean a *node*. Losing that would have made every
    // read command require a workspace, which is a real regression the grammar
    // must not smuggle in.
    let dir = std::env::temp_dir().join(format!("prov-targets-loose-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("loose.md"), "---\ntitle: Loose\n---\nbody\n").unwrap();
    // No `init` — there is no workspace here at all.
    let (ok, out) = run(&dir, &["get", "loose.md", "title"]);
    assert!(ok, "a path must resolve with no workspace: {out}");
    assert_eq!(out.trim(), "Loose");
}

#[test]
fn a_route_outside_a_workspace_says_so_rather_than_reading_a_file() {
    let dir = std::env::temp_dir().join(format!("prov-targets-noroot-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (ok, _) = run(&dir, &["show", "@Daily/2026"]);
    assert!(!ok, "a route has no meaning without a workspace");
}

#[test]
fn a_file_literally_named_with_an_at_sign_is_still_addressable() {
    // The grammar's escape hatch. `@foo.md` parses as a route; `./@foo.md` is a
    // path, because the classifier only strips a *leading* @.
    let dir = journal("at-file");
    std::fs::write(dir.join("@foo.md"), "---\ntitle: At Foo\n---\n").unwrap();
    let (ok, out) = run(&dir, &["get", "./@foo.md", "title"]);
    assert!(ok, "./@foo.md must parse as a path: {out}");
    assert_eq!(out.trim(), "At Foo");
}

#[test]
fn parents_with_a_path_parent_is_inert_for_the_parent_but_enables_leaf_idempotency() {
    // `-p` synthesizes missing *route segments*; a path parent has none, so `-p`
    // is inert there — but it still applies to the *leaf* (`mkdir -p` semantics),
    // so `-p` with a path parent is allowed, not an error.
    let dir = journal("p-nonroute");
    let (ok, out) = run(&dir, &["new", "X", "--in", "daily/2026/07/index.md", "-p"]);
    assert!(ok, "-p with a path parent creates the leaf: {out}");
    // And it is idempotent: a second run is a no-op, not a collision.
    let (ok, out) = run(&dir, &["new", "X", "--in", "daily/2026/07/index.md", "-p"]);
    assert!(ok && out.contains("exists"), "rerun is a no-op: {out}");
}

#[test]
fn a_route_subject_refuses_to_create_what_it_cannot_find() {
    // Only a `--in` destination may be synthesized, and only with -p. A *subject*
    // that does not resolve is a mistake, never an instruction.
    let dir = journal("subject-nocreate");
    let (ok, out) = run(&dir, &["show", "@Daily/2026/09"]);
    assert!(!ok, "a missing subject route must fail: {out}");
    assert!(
        out.contains("no child titled"),
        "says how far it got: {out}"
    );
    assert!(!dir.join("daily/2026/09").exists(), "and creates nothing");
}

// ── config vocabulary (docs/config-vocab.md) ────────────────────────────────

/// A throwaway workspace initialized fresh in a temp dir.
fn workspace(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("prov-cfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (ok, out) = run(&dir, &["init", "--yes"]);
    assert!(ok, "init: {out}");
    dir
}

#[test]
fn config_reads_and_writes_nested_axes_by_dotted_key() {
    let dir = workspace("dotted");
    let (ok, out) = run(&dir, &["config", "references.notation"]);
    assert!(ok && out.trim() == "markdown", "get default: {out}");
    let (ok, _) = run(&dir, &["config", "references.notation", "wikilink"]);
    assert!(ok);
    let (ok, out) = run(&dir, &["config", "references.notation"]);
    assert!(ok && out.trim() == "wikilink", "get after set: {out}");
}

#[test]
fn config_set_refuses_a_setting_check_would_ignore() {
    let dir = workspace("refuse");
    let (ok, out) = run(&dir, &["config", "fixity", "alll"]);
    assert!(!ok, "a bad value must be refused: {out}");
    assert!(
        out.contains("attachments"),
        "lists the accepted values: {out}"
    );
    let (ok, out) = run(&dir, &["config", "references.notaton", "bare"]);
    assert!(!ok, "a typo'd nested key must be refused: {out}");
    assert!(
        out.contains("references.notation"),
        "suggests the real key: {out}"
    );
}

#[test]
fn config_setup_materializes_the_full_effective_config() {
    let dir = workspace("setup");
    // A partial config with a user field and one non-default setting.
    std::fs::write(
        dir.join("prov.yaml"),
        "title: prov config\npart_of: '[Setup](/index.md)'\nmaintainer: adam\nfixity: all\n",
    )
    .unwrap();
    let (ok, out) = run(&dir, &["config", "--setup"]);
    assert!(ok, "{out}");
    let cfg = std::fs::read_to_string(dir.join("prov.yaml")).unwrap();
    assert!(
        cfg.contains("maintainer: adam"),
        "preserves a user field: {cfg}"
    );
    assert!(
        cfg.contains("fixity: all"),
        "preserves a non-default: {cfg}"
    );
    assert!(
        cfg.contains("notation: markdown"),
        "fills a reference default: {cfg}"
    );
    assert!(
        cfg.contains("identity: lazy"),
        "fills the identity default: {cfg}"
    );
}

#[test]
fn check_flags_a_typo_in_the_config_document() {
    let dir = workspace("lint");
    let mut cfg = std::fs::read_to_string(dir.join("prov.yaml")).unwrap();
    cfg.push_str("record_deletion: false\n");
    std::fs::write(dir.join("prov.yaml"), cfg).unwrap();
    let (ok, out) = run(&dir, &["check"]);
    assert!(!ok, "check fails on a config issue: {out}");
    assert!(
        out.contains("record_deletions"),
        "suggests the real key: {out}"
    );
}

#[test]
fn check_reports_a_config_spec_newer_than_this_build() {
    let dir = workspace("spec");
    let mut cfg = std::fs::read_to_string(dir.join("prov.yaml")).unwrap();
    // The config document ships `spec: 1`; bump it past what this build knows.
    let bumped = cfg.replace("spec: 1", "spec: 99");
    assert_ne!(bumped, cfg, "fixture must carry a spec line");
    cfg = bumped;
    std::fs::write(dir.join("prov.yaml"), cfg).unwrap();
    let (ok, out) = run(&dir, &["check"]);
    assert!(!ok, "check fails when spec is ahead: {out}");
    assert!(out.contains("spec 99"), "names the declared spec: {out}");
    assert!(
        out.contains("upgrade prov"),
        "points at the resolution: {out}"
    );
}

#[test]
fn a_command_warns_about_config_that_will_be_ignored_unless_quiet() {
    let dir = workspace("warn");
    let mut cfg = std::fs::read_to_string(dir.join("prov.yaml")).unwrap();
    cfg.push_str("identis: lazy\n"); // near-miss of `identity` → a real typo
    std::fs::write(dir.join("prov.yaml"), cfg).unwrap();
    let (ok, out) = run(&dir, &["tree"]);
    assert!(ok, "tree still succeeds: {out}");
    assert!(
        out.contains("will not take effect"),
        "warns proactively: {out}"
    );
    // PROV_QUIET silences the reminder.
    let quiet = Command::new(env!("CARGO_BIN_EXE_prov"))
        .current_dir(&dir)
        .args(["tree"])
        .env("PROV_QUIET", "1")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&quiet.stderr);
    assert!(
        !text.contains("will be ignored"),
        "PROV_QUIET suppresses it: {text}"
    );
}
