//! The one CI job of prov's that is more than a command.
//!
//! CI is the `job` list in `.config/ship.figl`, run by `ship ci`. Most of its
//! jobs are a cargo invocation; `package-isolation` is a list of crates and a
//! test that the list is complete, so it lives here and the job runs
//! `cargo xtask package-isolation`.
//!
//! Cutting a release does not live here either. It is `ship`, configured by
//! `.config/ship.figl` — the same tool the rest of the org cuts releases with.
//!
//! There are no dependencies on purpose: the job builds this crate before it
//! can start.

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// Anything that goes wrong here is a message for whoever is reading the log;
/// there is nothing for a CI runner to recover from.
type Result<T> = std::result::Result<T, String>;

/// Workspace feature unification means `cargo check --workspace` can pass even
/// when a crate cannot compile on its own — some other member's feature
/// selection quietly fills the gap. Each entry below builds one crate in
/// isolation (or with a single explicit format feature), which is what catches
/// that before it becomes a publish-time surprise.
///
/// A new workspace member belongs in this list. `prov` itself is covered by the
/// per-format rows rather than a bare check: its default features pull in every
/// parser backend, which is precisely the case that cannot go wrong.
const ISOLATED: &[&[&str]] = &[
    &["-p", "prov-graph"],
    &["-p", "prov-store"],
    &["-p", "prov-config"],
    &["-p", "prov-grain"],
    &["-p", "prov-views"],
    &["-p", "prov-filing"],
    &["-p", "prov-exports"],
    &["-p", "prov-cli"],
    &["-p", "prov", "--no-default-features", "--features", "yaml"],
    &["-p", "prov", "--no-default-features", "--features", "json"],
    &["-p", "prov", "--no-default-features", "--features", "toml"],
    &[
        "-p",
        "prov",
        "--no-default-features",
        "--features",
        "fig-lang",
    ],
];

fn package_isolation(sh: &Sh) -> Result<()> {
    for spec in ISOLATED {
        let mut args = vec!["check"];
        args.extend_from_slice(spec);
        sh.cargo(&args)?;
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let sh = Sh::new();

    let outcome = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        [] | ["-h" | "--help" | "help"] => {
            print!("{}", usage());
            return ExitCode::SUCCESS;
        }
        ["package-isolation"] => package_isolation(&sh),
        // CI moved to ship rather than being retired, and a muscle-memory
        // `cargo xtask ci` should say where it went.
        ["ci" | "ci-matrix", ..] => Err(format!(
            "CI moved out of xtask: it is the `job` list in .config/ship.figl.\n\
             `ship ci` runs every job, `ship ci <id>` one, `ship ci --list` names them.\n\n{}",
            usage()
        )),
        // So did releasing.
        [
            command @ ("version" | "bump" | "changelog" | "release" | "release-notes"),
            ..,
        ] => {
            let now = match command {
                "release" => "ship <spec>",
                "release-notes" => "ship notes",
                "version" => "ship version",
                "bump" => "ship bump",
                _ => "ship changelog",
            };
            Err(format!(
                "releasing moved out of xtask: `cargo xtask {command}` is now `{now}`,\n\
                 configured in .config/ship.figl.\n\n{}",
                usage()
            ))
        }
        ["github-release", ..] => Err(format!(
            "releasing moved out of xtask: `cargo xtask github-release` is now `ship github`.\n\n{}",
            usage()
        )),
        [other, ..] => Err(format!("unknown task `{other}`\n\n{}", usage())),
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("\nxtask: {message}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> String {
    String::from(
        "usage: cargo xtask <task>\n\n\
         \x20 package-isolation   build each crate alone, without workspace feature unification\n\n\
         CI is `ship ci`, and releasing is `ship`; both read .config/ship.figl.\n",
    )
}

/// Whether a member's manifest lets it go to crates.io.
///
/// `publish = false` is how a member opts out — `xtask` because it is tooling,
/// `prov-testkit` because it is test scaffolding. Read from the manifest rather
/// than listed here, so adding another one is a manifest edit and nothing else.
#[cfg(test)]
fn publishable(manifest: &str) -> bool {
    !manifest.lines().any(|line| {
        let line = line.trim();
        line.starts_with("publish") && line.contains("false")
    })
}

/// A shell rooted at the workspace, so a task never has to think about where
/// it was invoked from.
struct Sh {
    root: PathBuf,
    /// Cargo tells its subprocesses which cargo it is; prefer that over
    /// whichever one happens to be first on PATH.
    cargo: String,
}

impl Sh {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask/ always has a parent")
            .to_path_buf();
        let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        Sh { root, cargo }
    }

    /// Run cargo at the workspace root, echoing the command first so a CI log
    /// reads as a transcript of commands anyone can paste back.
    fn cargo(&self, args: &[&str]) -> Result<()> {
        println!("\x1b[2m$ cargo {}\x1b[0m", args.join(" "));

        let status = Command::new(&self.cargo)
            .args(args)
            .current_dir(&self.root)
            .status()
            .map_err(|e| format!("could not run `cargo`: {e}"))?;

        if status.success() {
            Ok(())
        } else {
            Err(format!("`cargo {}` failed ({status})", args.join(" ")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every *publishable* member of the workspace should be built alone by the
    /// isolation job; that is the whole point of it. `prov` is covered by its
    /// per-format rows instead, and a `publish = false` member is never
    /// packaged, so there is no isolated build to check.
    #[test]
    fn package_isolation_covers_every_member() {
        let sh = Sh::new();
        let manifest = std::fs::read_to_string(sh.root.join("Cargo.toml")).unwrap();
        let members = manifest
            .lines()
            .find(|line| line.trim_start().starts_with("members"))
            .expect("workspace members");

        for member in members.split('"').skip(1).step_by(2) {
            let member_manifest =
                std::fs::read_to_string(sh.root.join(member).join("Cargo.toml")).unwrap();
            if member == "prov" || !publishable(&member_manifest) {
                continue;
            }
            assert!(
                ISOLATED.iter().any(|spec| spec == &["-p", member]),
                "workspace member `{member}` is not built in isolation by `cargo xtask package-isolation`",
            );
        }
        assert!(ISOLATED.iter().any(|spec| spec.contains(&"--features")));
    }

    /// `publish = false` anywhere in the manifest opts a member out; nothing
    /// else does. The isolation job and the test above both ask this one
    /// question, so it is the one that has to be right.
    #[test]
    fn publishable_reads_the_manifest_not_a_list() {
        assert!(publishable("[package]\nname = \"prov\"\n"));
        assert!(publishable(
            "[package]\nname = \"prov\"\npublish = [\"crates-io\"]\n"
        ));
        assert!(!publishable(
            "[package]\nname = \"xtask\"\npublish = false\n"
        ));
        assert!(!publishable(
            "[package]\npublish = false  # it is tooling\n"
        ));
        // A `publish` key belonging to something else does not count.
        assert!(publishable(
            "[package]\ndescription = \"publish = false, they said\"\n"
        ));
    }
}
