//! `prov --version` names the build, not just the release.
//!
//! The string is assembled in `build.rs` from the repository's git state, which
//! means the two things worth pinning are the two things a bug report relies
//! on: the package version is always there, and a build that did not come from
//! the release tag says which commit it *did* come from. Which of the two forms
//! this test sees depends on where it runs — a tagged release build, a working
//! checkout, or an unpacked `.crate` with no git at all — so it accepts either
//! and checks the shape of each.

use std::process::Command;

#[test]
fn version_names_the_package_and_the_commit_when_untagged() {
    let out = Command::new(env!("CARGO_BIN_EXE_prov"))
        .arg("--version")
        .output()
        .expect("run prov");
    assert!(out.status.success(), "`prov --version` failed");
    let text = String::from_utf8(out.stdout)
        .expect("utf-8")
        .trim()
        .to_string();

    let release = format!("prov {}", env!("CARGO_PKG_VERSION"));
    if text == release {
        return; // built from the release tag, or from a tree with no git
    }

    // Otherwise: `prov 0.6.1 (8cc0436e)` — the same prefix, then the commit.
    let commit = text
        .strip_prefix(&format!("{release} ("))
        .and_then(|rest| rest.strip_suffix(')'))
        .unwrap_or_else(|| panic!("expected `{release}` or `{release} (<commit>)`, got `{text}`"));
    assert!(
        commit.len() == 8 && commit.chars().all(|c| c.is_ascii_hexdigit()),
        "the dev-build marker is an abbreviated commit hash, got `{commit}`"
    );
}
