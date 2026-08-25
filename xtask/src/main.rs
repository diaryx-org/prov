//! prov's CI, as one program.
//!
//! Every job the CI workflow runs is one entry in [`JOBS`] and one
//! `cargo xtask <id>` invocation. The workflow itself holds no build knowledge:
//! it asks `cargo xtask ci-matrix` what the jobs are, then runs each one by id.
//! Adding, renaming, reordering, or retiring a job is an edit to this file and
//! nothing else — the YAML does not change.
//!
//! Locally, `cargo xtask ci` runs the same jobs in the same order against the
//! same commands, so a green run here is a green run there.
//!
//! Cutting a release lives here too, in [`release`], for the same reason: the
//! publish workflow asks the program what to publish rather than holding a list
//! of crates that goes stale the moment the workspace gains one.
//!
//! There are no dependencies on purpose. Every CI job builds this crate before
//! it can start, so its build time is paid a dozen times over per push.

mod release;

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// Anything that goes wrong here is a message for whoever is reading the log;
/// there is nothing for a CI runner to recover from.
type Result<T> = std::result::Result<T, String>;

/// One CI job: what to call it, what the runner must install for it, and the
/// work itself.
struct Job {
    /// `cargo xtask <id>`, and the key the workflow dispatches on.
    id: &'static str,
    /// The name GitHub shows in the checks list. Renaming it renames the
    /// required status check, so branch protection has to be updated to match.
    name: &'static str,
    /// rustup components the job needs, comma-joined for
    /// `dtolnay/rust-toolchain`. Empty means the default toolchain is enough.
    components: &'static str,
    /// Does this job *compile* the workspace? If so the runner needs the pinned
    /// Zig toolchain — prov's `fig` and `twig-doc` dependencies are Zig-backed,
    /// their build.rs runs `zig build` — and restoring the cargo cache is worth
    /// its cost. `fmt` is the one job that only ever parses.
    builds: bool,
    /// One line of explanation, printed by `cargo xtask` with no arguments.
    about: &'static str,
    run: fn(&Sh) -> Result<()>,
}

/// The whole of CI, in the order `cargo xtask ci` runs it: cheapest and most
/// likely to fail first.
const JOBS: &[Job] = &[
    Job {
        id: "fmt",
        name: "Format",
        components: "rustfmt",
        builds: false,
        about: "rustfmt, in check mode",
        run: fmt,
    },
    Job {
        id: "clippy",
        name: "Clippy",
        components: "clippy",
        builds: true,
        about: "clippy over every target, warnings denied",
        run: clippy,
    },
    Job {
        id: "test",
        name: "Test",
        components: "",
        builds: true,
        about: "the workspace test suite",
        run: test,
    },
    Job {
        id: "docs",
        name: "Getting-started transcript",
        components: "",
        builds: true,
        about: "replay docs/getting-started.md against a real binary",
        run: docs,
    },
    Job {
        id: "package-isolation",
        name: "Package isolation",
        components: "",
        builds: true,
        about: "build each crate alone, without workspace feature unification",
        run: package_isolation,
    },
    Job {
        id: "msrv",
        name: "MSRV",
        components: "",
        builds: true,
        about: "build on the minimum supported Rust version",
        run: msrv,
    },
];

// ---------------------------------------------------------------------------
// The jobs
// ---------------------------------------------------------------------------

fn fmt(sh: &Sh) -> Result<()> {
    sh.cargo(&["fmt", "--all", "--check"])
}

/// Warnings are errors in CI, so they are errors here too — a lint that only
/// fires on the runner is a lint found too late.
fn clippy(sh: &Sh) -> Result<()> {
    sh.cargo(&[
        "clippy",
        "--workspace",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ])
}

fn test(sh: &Sh) -> Result<()> {
    sh.cargo(&["test", "--workspace"])
}

/// The getting-started guide's command transcript, executed against a freshly
/// built binary so the docs can never drift from the CLI.
///
/// The runner stays in shell: it replays the guide's `console` blocks as one
/// continuous session, which is what a reader following along actually
/// experiences. See `ci/check-getting-started.sh` and the note at the top of
/// the guide.
fn docs(sh: &Sh) -> Result<()> {
    sh.cargo(&["build", "-p", "prov-cli"])?;
    sh.run("ci/check-getting-started.sh", &[])
}

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
    &["-p", "prov-views"],
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

/// Build on the crate's declared minimum supported Rust version. A build, not a
/// test run: MSRV is a promise about who can *compile* prov, and the
/// dev-dependencies and test tooling need not hold to it.
///
/// The version is read from `workspace.package.rust-version`, so the pin can
/// never drift from the declared floor — bump it in Cargo.toml and this follows.
fn msrv(sh: &Sh) -> Result<()> {
    let version = sh.workspace_rust_version()?;
    println!("MSRV from Cargo.toml: {version}");
    // Idempotent: rustup reports an already-installed toolchain and returns 0.
    sh.run(
        "rustup",
        &[
            "toolchain",
            "install",
            &version,
            "--profile",
            "minimal",
            "--no-self-update",
        ],
    )
    .map_err(|e| format!("{e}\n\nthe MSRV job needs rustup on PATH to pin Rust {version}"))?;
    // `rustup run`, not `cargo +{version}`: the `+toolchain` shorthand is a
    // rustup-proxy feature, and $CARGO may well point past the proxy at a real
    // toolchain binary that does not understand it.
    sh.run(
        "rustup",
        &["run", &version, "cargo", "build", "--workspace"],
    )
}

// ---------------------------------------------------------------------------
// Driving them
// ---------------------------------------------------------------------------

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let sh = Sh::new();

    let outcome = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        [] | ["-h" | "--help" | "help"] => {
            print!("{}", usage());
            return ExitCode::SUCCESS;
        }
        ["ci"] => ci(&sh),
        ["ci-matrix"] => {
            println!("{}", ci_matrix());
            Ok(())
        }
        ["version"] => release::print_version(&sh),
        ["bump", spec] => release::bump(&sh, spec),
        ["changelog", ref rest @ ..] => release::changelog(&sh, rest),
        ["publish", ref rest @ ..] => release::publish(&sh, rest),
        ["release-notes"] => release::release_notes(&sh, None),
        ["release-notes", tag] => release::release_notes(&sh, Some(tag)),
        ["release", spec, ref rest @ ..] => release::release(&sh, spec, rest),
        // Both take a version, and neither should guess one.
        [command @ ("bump" | "release")] => Err(format!(
            "`{command}` needs a version: patch, minor, major, or x.y.z\n\n{}",
            usage()
        )),
        [id] => match JOBS.iter().find(|job| job.id == id) {
            Some(job) => (job.run)(&sh),
            None => Err(format!("unknown job `{id}`\n\n{}", usage())),
        },
        [id, ..] => Err(format!("`{id}` takes no arguments\n\n{}", usage())),
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("\nxtask: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Every job, in order — what CI does, on one machine. Stops at the first
/// failure, on the theory that a red build is worth reading before the next one
/// buries it.
fn ci(sh: &Sh) -> Result<()> {
    for job in JOBS {
        println!("\n\x1b[1m━━ {} ━━\x1b[0m", job.name);
        (job.run)(sh)?;
    }
    println!("\n\x1b[32mall {} jobs passed\x1b[0m", JOBS.len());
    Ok(())
}

/// The job table as a single line of JSON, for the workflow's `strategy.matrix`.
///
/// Hand-rolled rather than serde-derived: the crate has no dependencies, and
/// every value here is a `&'static str` literal from [`JOBS`] with nothing in it
/// that JSON would need escaped. A job name with a quote or a backslash in it
/// would produce invalid JSON, and `cargo xtask ci-matrix` in the test below is
/// what would notice.
fn ci_matrix() -> String {
    let entries: Vec<String> = JOBS
        .iter()
        .map(|job| {
            format!(
                r#"{{"id":"{}","name":"{}","components":"{}","builds":{}}}"#,
                job.id, job.name, job.components, job.builds
            )
        })
        .collect();
    format!("[{}]", entries.join(","))
}

fn usage() -> String {
    let mut out = String::from(
        "prov's CI, and its releases. Each job below is exactly what the CI \
         workflow runs.\n\n\
         usage: cargo xtask <command>\n\njobs:\n\n",
    );
    for job in JOBS {
        out.push_str(&format!("  {:<18}{}\n", job.id, job.about));
    }
    out.push_str(&format!("  {:<18}{}\n", "ci", "every job above, in order"));
    out.push_str(&format!(
        "  {:<18}{}\n",
        "ci-matrix", "the job table as JSON, for the workflow matrix"
    ));
    // Releasing is not CI, so it is not in the table above — these are run by
    // hand (and `publish` by the release workflow), not by every push.
    out.push_str("\nreleasing:\n\n");
    for (command, about) in RELEASE_COMMANDS {
        out.push_str(&format!("  {command:<18}{about}\n"));
    }
    out
}

/// The release commands, for `cargo xtask` with no arguments. See
/// [`release`] for what each one does and why the push is opt-in.
const RELEASE_COMMANDS: &[(&str, &str)] = &[
    ("version", "the workspace version"),
    ("bump <spec>", "move to patch | minor | major | x.y.z"),
    (
        "changelog",
        "regenerate the unreleased region (--write, --check)",
    ),
    (
        "release <spec>",
        "bump, changelog, commit, tag — and push only with --push",
    ),
    (
        "publish",
        "publish every crate crates.io is missing (--list)",
    ),
    (
        "release-notes [tag]",
        "that release's changelog section, for the GitHub release body",
    ),
];

// ---------------------------------------------------------------------------
// Running things
// ---------------------------------------------------------------------------

/// A shell rooted at the workspace, so a job never has to think about where it
/// was invoked from.
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

    fn cargo(&self, args: &[&str]) -> Result<()> {
        let cargo = self.cargo.clone();
        self.run(&cargo, args)
    }

    /// Run a command at the workspace root, echoing it first so a CI log reads
    /// as a transcript of commands anyone can paste back.
    fn run(&self, program: &str, args: &[&str]) -> Result<()> {
        let shown = if program == self.cargo {
            "cargo"
        } else {
            program
        };
        println!("\x1b[2m$ {} {}\x1b[0m", shown, args.join(" "));

        let status = Command::new(program)
            .args(args)
            .current_dir(&self.root)
            .status()
            .map_err(|e| format!("could not run `{shown}`: {e}"))?;

        if status.success() {
            Ok(())
        } else {
            Err(format!("`{shown} {}` failed ({status})", args.join(" ")))
        }
    }

    /// Run a command and hand back its stdout, for the answers a job needs to
    /// act on rather than show — an HTTP status, a branch name, a tag list. The
    /// command is not echoed: these are questions, and a log of them reads as
    /// noise between the commands that actually did something.
    fn capture(&self, program: &str, args: &[&str]) -> Result<String> {
        let output = Command::new(program)
            .args(args)
            .current_dir(&self.root)
            .output()
            .map_err(|e| format!("could not run `{program}`: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "`{program} {}` failed ({})\n{}",
                args.join(" "),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim(),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Fail early, and with the install line, when a tool the task needs is
    /// missing — rather than halfway through a release, with the version
    /// already bumped.
    fn require(&self, program: &str, hint: &str) -> Result<()> {
        Command::new(program)
            .arg("--version")
            .current_dir(&self.root)
            .output()
            .map(|_| ())
            .map_err(|_| format!("`{program}` not found on PATH\nhint: {hint}"))
    }

    /// Read a workspace file, by its path from the root.
    fn read(&self, path: &str) -> Result<String> {
        let path = self.root.join(path);
        std::fs::read_to_string(&path)
            .map_err(|e| format!("could not read {}: {e}", path.display()))
    }

    /// Write a workspace file, by its path from the root.
    fn write(&self, path: &str, contents: &str) -> Result<()> {
        let path = self.root.join(path);
        std::fs::write(&path, contents)
            .map_err(|e| format!("could not write {}: {e}", path.display()))
    }

    /// `workspace.package.rust-version`, the single source of truth for the MSRV.
    fn workspace_rust_version(&self) -> Result<String> {
        let manifest = self.root.join("Cargo.toml");
        let text = std::fs::read_to_string(&manifest)
            .map_err(|e| format!("could not read {}: {e}", manifest.display()))?;
        text.lines()
            .find_map(|line| line.trim().strip_prefix("rust-version"))
            .and_then(|rest| rest.split('"').nth(1))
            .map(str::to_owned)
            .ok_or_else(|| format!("no `rust-version` in {}", manifest.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The workflow's `fromJSON` is the only thing that parses `ci-matrix`, and
    /// it fails at a point where the fix costs a push. Check the shape here
    /// instead: one object per job, every field present, nothing needing an
    /// escape.
    #[test]
    fn ci_matrix_is_well_formed_json() {
        let json = ci_matrix();
        assert!(json.starts_with('[') && json.ends_with(']'));
        assert_eq!(json.matches("\"id\":").count(), JOBS.len());
        assert_eq!(json.lines().count(), 1, "the workflow reads it as one line");

        for job in JOBS {
            for field in [job.id, job.name, job.components] {
                assert!(
                    !field.contains(['"', '\\']),
                    "`{field}` would need JSON escaping, which ci_matrix does not do",
                );
            }
            assert!(json.contains(&format!("\"id\":\"{}\"", job.id)));
        }
    }

    /// `ci` and `ci-matrix` are handled before the table is consulted, so a job
    /// by either name would be unreachable.
    #[test]
    fn job_ids_are_distinct_and_dispatchable() {
        let mut ids: Vec<&str> = JOBS.iter().map(|job| job.id).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate job id");
        assert!(!ids.contains(&"ci") && !ids.contains(&"ci-matrix"));
    }

    /// Every member of the workspace should be built alone by the isolation
    /// job; that is the whole point of it. `prov` is covered by its per-format
    /// rows instead, and `xtask` is not a published crate.
    #[test]
    fn package_isolation_covers_every_member() {
        let sh = Sh::new();
        let manifest = std::fs::read_to_string(sh.root.join("Cargo.toml")).unwrap();
        let members = manifest
            .lines()
            .find(|line| line.trim_start().starts_with("members"))
            .expect("workspace members");

        for member in members.split('"').skip(1).step_by(2) {
            if member == "prov" || member == "xtask" {
                continue;
            }
            assert!(
                ISOLATED.iter().any(|spec| spec == &["-p", member]),
                "workspace member `{member}` is not built in isolation by `cargo xtask package-isolation`",
            );
        }
        assert!(ISOLATED.iter().any(|spec| spec.contains(&"--features")));
    }

    /// The MSRV job reads this; if the parse breaks, the job silently pins the
    /// wrong compiler or fails far from the cause.
    #[test]
    fn msrv_is_readable_from_the_manifest() {
        let version = Sh::new().workspace_rust_version().unwrap();
        assert!(
            version.split('.').all(|part| part.parse::<u32>().is_ok()),
            "`{version}` does not look like a Rust version",
        );
    }
}
