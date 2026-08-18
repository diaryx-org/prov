//! Stamps the binary's `--version` string with the commit it was built from,
//! whenever that commit is not the release tag.
//!
//! `CARGO_PKG_VERSION` alone cannot answer the question a bug report actually
//! asks — *which* build is this? It reads `0.6.1` for the tagged release and
//! for `main` twenty commits later alike. So this script resolves the git state
//! at build time and emits `PROV_VERSION`, which `cli::VERSION` hands to clap:
//!
//! ```text
//! prov 0.6.1                # HEAD is the v0.6.1 tag — the release
//! prov 0.6.1 (8cc0436e)     # any other commit — a dev build, and which one
//! ```
//!
//! The parenthetical *is* the signal: if it is there, the binary did not come
//! from this version's release tag, and the hash says where it did come from.
//!
//! Three things this must not break:
//!
//! - **No git, no repository.** The `.crate` tarball published to crates.io
//!   carries no git metadata, and neither does docs.rs or a distro tarball
//!   build. Every probe here is fallible and every failure falls back to the
//!   bare `CARGO_PKG_VERSION` — a release build is exactly the case that wants
//!   no suffix anyway.
//! - **Somebody else's repository.** A vendored copy of this crate inside an
//!   unrelated checkout would otherwise be stamped with *that* project's
//!   commit. [`in_prov_repo`] rejects a repository that does not track this
//!   crate's own manifest.
//! - **`about.md`.** The generated page's `generated_by:` field takes
//!   `CARGO_PKG_VERSION` directly (see `about_context` in `main.rs`) and is a
//!   committed file — a hash in there would fail `prov about --check` on every
//!   commit that did not regenerate it. This string is for `--version` alone.
//!
//! There is deliberately no `dirty` marker. Keeping one honest means re-running
//! this script on every build, since an edit to any crate the binary links
//! changes the answer and no watchable set of paths covers that. A build script
//! that re-runs invalidates its crate, so the cost would be a full recompile of
//! the CLI on every `cargo build` and `cargo test` — permanent, and paid by
//! everyone. The commit hash needs only the ref files watched below, and a
//! marker that is right about the commit and quietly wrong about the tree is
//! worse than no marker.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    watch_refs();
    let package = std::env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION");
    println!("cargo::rustc-env=PROV_VERSION={}", version(&package));
}

/// The `--version` string: the package version, plus the commit whenever this
/// is not a build of the matching release tag.
fn version(package: &str) -> String {
    if !in_prov_repo() {
        return package.to_string();
    }
    let Some(hash) = git(&["rev-parse", "--short=8", "HEAD"]) else {
        // A repository with no commits yet — nothing to name.
        return package.to_string();
    };
    // A release build is HEAD sitting exactly on this version's tag. Anything
    // else — a later commit, an earlier one, a tag for a different version —
    // is a dev build and says so.
    let released = git(&["describe", "--tags", "--exact-match", "HEAD"])
        .is_some_and(|tag| tag == format!("v{package}"));
    if released {
        package.to_string()
    } else {
        format!("{package} ({hash})")
    }
}

/// Re-run this script when HEAD moves — a commit, a checkout, a new tag.
///
/// Emitting any `rerun-if-changed` replaces Cargo's default (re-run when a file
/// in the package changed), which is right: this script's answer depends on the
/// repository, not on the sources. `refs` goes in as a directory so that a
/// commit which *creates* a loose ref, on a repository whose refs were packed,
/// still counts as a change.
///
/// Only paths that exist are emitted. A `rerun-if-changed` naming a missing
/// file re-runs the script every single build, which is the cost this design
/// exists to avoid.
fn watch_refs() {
    // In a linked worktree these differ: HEAD is per-worktree, refs are shared.
    let git_dir = git(&["rev-parse", "--absolute-git-dir"]).map(PathBuf::from);
    let common = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .map(PathBuf::from)
        .or_else(|| git_dir.clone());

    let watch = |path: PathBuf| {
        if path.exists() {
            println!("cargo::rerun-if-changed={}", path.display());
        }
    };
    if let Some(dir) = &git_dir {
        watch(dir.join("HEAD"));
    }
    if let Some(dir) = &common {
        watch(dir.join("refs"));
        watch(dir.join("packed-refs"));
    }
}

/// Whether the repository git discovers from this crate's directory is prov's
/// own, rather than one this crate has been vendored into.
///
/// The test is that the repository tracks this crate's manifest: true in a prov
/// checkout, false for a `.crate` unpacked inside somebody else's tree.
fn in_prov_repo() -> bool {
    git(&["ls-files", "--error-unmatch", "--", "Cargo.toml"]).is_some()
}

/// Run `git` in this crate's directory, returning its trimmed stdout, or
/// [`None`] if git is not installed, or exits non-zero. A command that succeeds
/// with no output is [`Some`] of the empty string, which is an answer rather
/// than a failure.
fn git(args: &[&str]) -> Option<String> {
    let dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let out = Command::new("git")
        .current_dir(Path::new(&dir))
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}
