//! `prov help`: the one-line summary, and the help that is still clap's.
//!
//! `help --oneline` is read by tools that list every command-line tool they
//! may use, one line each, so its contract is the shape: exactly one line on
//! stdout, nothing on stderr, success, and a pointer to the full help.
//!
//! Also here: an error the old skill text had to warn about — a missing
//! `--in` parent — now names the file, so the warning could go.

use std::process::{Command, Output};

fn prov(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_prov"))
        .args(args)
        .output()
        .expect("run prov")
}

#[test]
fn oneline_is_one_line_that_points_at_the_full_help() {
    let out = prov(&["help", "--oneline"]);
    assert!(out.status.success());
    assert!(out.stderr.is_empty());
    let text = String::from_utf8(out.stdout).unwrap();
    let line = text.strip_suffix('\n').expect("ends in a newline");
    assert!(!line.contains('\n'), "one line, got {text:?}");
    assert!(line.starts_with("prov: "), "{line}");
    assert!(line.ends_with("More: `prov help`."), "{line}");
    assert!(
        line.chars().count() <= 200,
        "{} chars",
        line.chars().count()
    );
}

#[test]
fn help_without_oneline_is_still_clap_help() {
    let full = prov(&["help"]);
    assert!(full.status.success());
    assert_eq!(full.stdout, prov(&["--help"]).stdout);
    assert!(String::from_utf8_lossy(&full.stdout).contains("prov help --oneline"));

    let new = prov(&["help", "new"]);
    assert!(new.status.success());
    assert_eq!(new.stdout, prov(&["new", "--help"]).stdout);

    let nested = prov(&["peer", "help", "add"]);
    assert!(nested.status.success());
    assert_eq!(nested.stdout, prov(&["help", "peer", "add"]).stdout);
}

#[test]
fn a_missing_parent_is_named() {
    let dir = std::env::temp_dir().join(format!("prov-help-parent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_prov"))
            .current_dir(&dir)
            .args(args)
            .env("PROV_QUIET", "1")
            .output()
            .expect("run prov")
    };
    assert!(run(&["init", "--yes"]).status.success());
    let out = run(&["new", "Notes", "--in", "README.md"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("--in README.md") && !err.contains("os error"),
        "{err}"
    );
    assert!(!dir.join("notes.md").exists(), "nothing is written");
    let _ = std::fs::remove_dir_all(&dir);
}
