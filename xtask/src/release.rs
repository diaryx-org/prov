//! Cutting a release, as one program.
//!
//! prov releases on a tag: pushing `vX.Y.Z` starts `publish.yml` (crates.io)
//! and `homebrew.yml` (binaries and the tap). Everything before that push is
//! mechanical and easy to get half-right by hand — the workspace version lives
//! in a dozen places in one manifest, the lockfile has to follow it, and the
//! changelog's unreleased region has to be cut into a released section — so it
//! lives here instead:
//!
//!     cargo xtask version                 what the workspace calls itself
//!     cargo xtask bump <patch|minor|major|X.Y.Z>
//!     cargo xtask changelog [--write|--check]
//!     cargo xtask release <patch|minor|major|X.Y.Z> [--push] [--no-verify]
//!     cargo xtask publish [--list]
//!
//! `release` stops at the tag unless it is given `--push`. That asymmetry is the
//! whole safety model: every step before the push is a local commit that can be
//! amended or thrown away, and the push is the one that puts a version number on
//! crates.io, where it can be yanked but never reused. So the push is asked for
//! explicitly, each time, and the default run prints the two commands it did not
//! run.
//!
//! `publish` is what `publish.yml` invokes, so the release workflow holds no
//! more knowledge about this workspace than the CI workflow does: it asks the
//! program. It is also the manual recovery path — publishing is idempotent
//! per crate, so a run that died halfway is finished by running it again.

use std::fmt;

use crate::{Result, Sh};

/// The changelog, and the config that generates half of it.
const CHANGELOG: &str = "docs/CHANGELOG.md";
const CLIFF_CONFIG: &str = ".config/cliff.toml";

/// The generated region inside `## Unreleased`. Only the bytes between these
/// two lines are ever rewritten; a handwritten release intro lives below the
/// end marker, in the released section, where regeneration cannot reach it.
const BEGIN: &str = "<!-- git-cliff:begin — generated; edits here are overwritten -->";
const END: &str = "<!-- git-cliff:end -->";
/// What the region says when there is nothing unreleased — the normal state
/// immediately after a release.
const EMPTY_REGION: &str = "_No commits since the last tag._";

/// crates.io asks for a descriptive User-Agent and answers 403 without one.
const USER_AGENT: &str = "prov-release (xtask; https://github.com/diaryx-org/prov)";

// ---------------------------------------------------------------------------
// Versions
// ---------------------------------------------------------------------------

/// A semver triple, which is all prov has ever used. Pre-release and build
/// metadata are deliberately unparsed rather than silently dropped: a version
/// this cannot read is a version it must not rewrite.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    fn parse(text: &str) -> Result<Self> {
        let mut parts = text.trim().split('.');
        let mut next = || -> Result<u64> {
            parts
                .next()
                .and_then(|p| p.parse().ok())
                .ok_or_else(|| format!("`{text}` is not an x.y.z version"))
        };
        let version = Version {
            major: next()?,
            minor: next()?,
            patch: next()?,
        };
        match parts.next() {
            None => Ok(version),
            Some(_) => Err(format!("`{text}` is not an x.y.z version")),
        }
    }

    /// `patch`, `minor`, `major`, or a literal version to move to. A literal is
    /// checked against the current version rather than trusted: a release that
    /// goes backwards is a typo every time, and the tag it would cut is the one
    /// thing that cannot be taken back.
    fn bump(self, spec: &str) -> Result<Self> {
        match spec {
            "patch" => Ok(Version {
                patch: self.patch + 1,
                ..self
            }),
            "minor" => Ok(Version {
                minor: self.minor + 1,
                patch: 0,
                ..self
            }),
            "major" => Ok(Version {
                major: self.major + 1,
                minor: 0,
                patch: 0,
            }),
            literal => {
                let next = Version::parse(literal)?;
                if next.ordered() <= self.ordered() {
                    return Err(format!(
                        "{next} is not ahead of the current {self}\n\
                         hint: releases only move forward — a published version number can never be reused",
                    ));
                }
                Ok(next)
            }
        }
    }

    fn ordered(self) -> (u64, u64, u64) {
        (self.major, self.minor, self.patch)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// `workspace.package.version` — the version every member inherits, and the one
/// the publish workflow compares the tag against.
fn workspace_version(sh: &Sh) -> Result<Version> {
    let manifest = sh.read("Cargo.toml")?;
    let line = manifest
        .lines()
        .find(|line| line.starts_with("version = \""))
        .ok_or_else(|| "no `version` in [workspace.package]".to_string())?;
    Version::parse(line.split('"').nth(1).unwrap_or_default())
}

pub fn print_version(sh: &Sh) -> Result<()> {
    println!("{}", workspace_version(sh)?);
    Ok(())
}

/// Move the workspace to `next`, in the one file that holds it twice over.
///
/// Two rewrites, both in the root manifest: `[workspace.package] version`, and
/// the `version = "…"` inside every internal `{ path = "…", version = "…" }`
/// entry in `[workspace.dependencies]`. The second is not cosmetic — it is what
/// `cargo publish` uploads as the dependency requirement, so a stale one either
/// fails the publish (the version is not on the index yet) or, worse, succeeds
/// and ships a crate pinned to last release's siblings.
fn set_version(sh: &Sh, next: Version) -> Result<()> {
    let manifest = sh.read("Cargo.toml")?;
    let mut out = String::with_capacity(manifest.len());
    let (mut package, mut internal) = (0, 0);

    for line in manifest.lines() {
        if line.starts_with("version = \"") {
            out.push_str(&format!("version = \"{next}\""));
            package += 1;
        } else if let Some(rewritten) = retarget_path_dependency(line, next) {
            out.push_str(&rewritten);
            internal += 1;
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }

    if package != 1 {
        return Err(format!(
            "expected exactly one `version = \"…\"` line in Cargo.toml, found {package}"
        ));
    }
    sh.write("Cargo.toml", &out)?;
    println!("Cargo.toml -> {next} (workspace.package, and {internal} internal dependencies)");

    // The lockfile records the members' own versions, so it moves with them.
    // `--workspace` touches nothing else: a release is not the moment to pick up
    // a new upstream dependency.
    sh.cargo(&["update", "--workspace", "--quiet"])
}

/// `prov-graph = { path = "prov-graph", version = "0.5.0", … }` with the version
/// moved, or `None` if this line is not such an entry.
fn retarget_path_dependency(line: &str, next: Version) -> Option<String> {
    if !line.contains("path = \"") {
        return None;
    }
    let marker = "version = \"";
    let start = line.find(marker)? + marker.len();
    let end = start + line[start..].find('"')?;
    Some(format!("{}{next}{}", &line[..start], &line[end..]))
}

pub fn bump(sh: &Sh, spec: &str) -> Result<()> {
    let current = workspace_version(sh)?;
    let next = current.bump(spec)?;
    println!("{current} -> {next}");
    set_version(sh, next)
}

// ---------------------------------------------------------------------------
// The changelog
// ---------------------------------------------------------------------------

/// The unreleased commits, rendered by git-cliff through `.config/cliff.toml`.
///
/// git-cliff exits non-zero when there is nothing unreleased, which is a normal
/// state right after a tag rather than a failure — hence the placeholder rather
/// than an error.
fn generated(sh: &Sh) -> Result<String> {
    sh.require(
        "git-cliff",
        "nix profile install nixpkgs#git-cliff, or cargo install git-cliff",
    )?;
    let rendered = sh
        .capture(
            "git-cliff",
            &["--config", CLIFF_CONFIG, "--unreleased", "--strip", "all"],
        )
        .unwrap_or_default();
    let body = rendered.trim();
    Ok(if body.is_empty() {
        EMPTY_REGION.to_string()
    } else {
        body.to_string()
    })
}

fn region(body: &str) -> String {
    format!("{BEGIN}\n\n{body}\n\n{END}")
}

/// Replace the marked region, and optionally drop a fresh released section in
/// immediately below it. Everything above `## Unreleased` and every released
/// section below is left byte-for-byte alone.
fn rewrite(text: &str, body: &str, released: Option<&str>) -> Result<String> {
    for marker in [BEGIN, END] {
        if !text.lines().any(|line| line == marker) {
            return Err(format!("marker not found in {CHANGELOG}:\n  {marker}"));
        }
    }

    let mut out = String::with_capacity(text.len());
    let mut skipping = false;
    for line in text.lines() {
        if line == BEGIN {
            out.push_str(&region(body));
            out.push('\n');
            skipping = true;
        } else if line == END {
            skipping = false;
            if let Some(section) = released {
                out.push('\n');
                out.push_str(section);
                out.push('\n');
            }
        } else if !skipping {
            out.push_str(line);
            out.push('\n');
        }
    }
    Ok(out)
}

/// `cargo xtask changelog [--write|--check]` — print, splice, or verify.
pub fn changelog(sh: &Sh, args: &[&str]) -> Result<()> {
    let mode = match args {
        [] => "print",
        ["--write"] => "write",
        ["--check"] => "check",
        _ => return Err("usage: cargo xtask changelog [--write|--check]".into()),
    };

    let body = generated(sh)?;
    if mode == "print" {
        println!("{}", region(&body));
        return Ok(());
    }

    let current = sh.read(CHANGELOG)?;
    let spliced = rewrite(&current, &body, None)?;

    if mode == "check" {
        if spliced != current {
            return Err(format!(
                "{CHANGELOG}'s generated region is stale\nhint: run `cargo xtask changelog --write`"
            ));
        }
        println!("{CHANGELOG}'s generated region is up to date");
        return Ok(());
    }

    sh.write(CHANGELOG, &spliced)?;
    println!("wrote {CHANGELOG}");
    Ok(())
}

/// Turn the unreleased region into a released section headed `## vX.Y.Z — date`,
/// and reset the region. Called by `release`, between the version bump and the
/// commit, so the release commit carries both.
fn cut_changelog(sh: &Sh, version: Version) -> Result<()> {
    let body = generated(sh)?;
    let date = sh.capture("date", &["+%Y-%m-%d"])?.trim().to_string();
    let released = format!("## v{version} — {date}\n\n{body}\n");
    let current = sh.read(CHANGELOG)?;
    let cut = rewrite(&current, EMPTY_REGION, Some(&released))?;
    sh.write(CHANGELOG, &cut)?;
    println!("{CHANGELOG} -> new section `## v{version} — {date}`");
    Ok(())
}

// ---------------------------------------------------------------------------
// Publishing
// ---------------------------------------------------------------------------

/// The workspace members, in manifest order.
fn members(sh: &Sh) -> Result<Vec<String>> {
    let manifest = sh.read("Cargo.toml")?;
    let line = manifest
        .lines()
        .find(|line| line.trim_start().starts_with("members"))
        .ok_or_else(|| "no `members` in [workspace]".to_string())?;
    Ok(line
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect())
}

/// Which other members a manifest depends on, in any dependency table —
/// `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`, and the
/// `[target.'cfg(…)'.dependencies]` forms. Dev-dependencies count: cargo
/// verifies a published crate by building it, tests and all, so a dev-dependency
/// on a sibling has to be on the index just as much as a real one.
fn dependencies_on_members(manifest: &str, members: &[String]) -> Vec<String> {
    let is_member = |name: &str| members.iter().any(|m| m == name);
    let mut found = Vec::new();
    let mut in_dependencies = false;

    for line in manifest.lines() {
        let line = line.trim();
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let segments: Vec<&str> = header.split('.').collect();
            let table = |s: &str| s.ends_with("dependencies");
            in_dependencies = segments.last().is_some_and(|s| table(s));
            // `[dependencies.prov-graph]` — the dependency is named by the
            // header itself, and the lines under it are its fields.
            if let Some(position) = segments.iter().position(|s| table(s))
                && let Some(name) = segments.get(position + 1)
                && is_member(name)
            {
                found.push((*name).to_string());
            }
            continue;
        }
        if !in_dependencies {
            continue;
        }
        // `prov-store = { workspace = true }` and `prov-fixity.workspace = true`
        // are the same dependency written two ways.
        if let Some(key) = line.split('=').next() {
            let name = key.trim().trim_matches('"');
            let name = name.strip_suffix(".workspace").unwrap_or(name);
            if is_member(name) {
                found.push(name.to_string());
            }
        }
    }

    found.sort();
    found.dedup();
    found
}

/// Every publishable member, ordered so that no crate is published before
/// something it depends on. crates.io enforces this — an upload whose
/// dependencies are not yet on the index is rejected — and the order is derived
/// rather than written down so that adding a crate to the workspace is enough.
fn publish_order(sh: &Sh) -> Result<Vec<String>> {
    let members = members(sh)?;
    let mut manifests = Vec::new();
    for member in &members {
        let text = sh.read(&format!("{member}/Cargo.toml"))?;
        // `publish = false` is how xtask stays out of this list.
        let publishable = !text.lines().any(|line| {
            let line = line.trim();
            line.starts_with("publish") && line.contains("false")
        });
        let deps = dependencies_on_members(&text, &members);
        manifests.push((member.clone(), publishable, deps));
    }

    let mut order: Vec<String> = Vec::new();
    let mut visiting: Vec<String> = Vec::new();
    fn visit(
        member: &str,
        manifests: &[(String, bool, Vec<String>)],
        order: &mut Vec<String>,
        visiting: &mut Vec<String>,
    ) -> Result<()> {
        if order.iter().any(|done| done == member) {
            return Ok(());
        }
        if visiting.iter().any(|open| open == member) {
            return Err(format!(
                "dependency cycle through `{member}`: {}",
                visiting.join(" -> ")
            ));
        }
        visiting.push(member.to_string());
        let (_, publishable, deps) = manifests
            .iter()
            .find(|(name, _, _)| name == member)
            .ok_or_else(|| format!("`{member}` is not a workspace member"))?;
        for dep in deps {
            visit(dep, manifests, order, visiting)?;
        }
        visiting.pop();
        if *publishable {
            order.push(member.to_string());
        }
        Ok(())
    }

    for member in &members {
        visit(member, &manifests, &mut order, &mut visiting)?;
    }
    Ok(order)
}

/// Is this exact version already on crates.io?
///
/// `GET /api/v1/crates/<crate>/<version>` is 200 only when it is (a yanked
/// version also 200s — correct to skip, since a version number can never be
/// reused). Anything else — 404 for a new version, 404 for a crate nobody has
/// ever published — means "go publish".
fn already_published(sh: &Sh, name: &str, version: Version) -> Result<bool> {
    let url = format!("https://crates.io/api/v1/crates/{name}/{version}");
    let code = sh.capture(
        "curl",
        &[
            "-sS",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            "-A",
            USER_AGENT,
            &url,
        ],
    )?;
    Ok(code.trim() == "200")
}

/// `cargo xtask publish [--list]` — what `publish.yml` runs on a tag.
///
/// Idempotent per crate: each version already on crates.io is skipped rather
/// than attempted, so re-running after a partial release publishes exactly the
/// crates that are missing.
pub fn publish(sh: &Sh, args: &[&str]) -> Result<()> {
    let list_only = match args {
        [] => false,
        ["--list"] => true,
        _ => return Err("usage: cargo xtask publish [--list]".into()),
    };

    let version = workspace_version(sh)?;
    let order = publish_order(sh)?;
    println!("publishing {} crates at {version}, in order:", order.len());
    for (n, name) in order.iter().enumerate() {
        println!("  {}. {name}", n + 1);
    }

    if list_only {
        return Ok(());
    }

    for name in &order {
        if already_published(sh, name, version)? {
            println!("\n✅ {name} {version} already on crates.io — skipping");
            continue;
        }
        println!("\n📦 publishing {name} {version}");
        // No manual wait between crates: cargo blocks until a freshly published
        // version is visible on the index before it returns, which is exactly
        // what the next crate in the order needs.
        sh.cargo(&["publish", "-p", name])?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Releasing
// ---------------------------------------------------------------------------

/// `cargo xtask release <patch|minor|major|X.Y.Z> [--push] [--no-verify]`.
///
/// Bump, regenerate, commit, tag — and push only when asked. What the push
/// starts is worth stating plainly: `publish.yml` uploads every crate to
/// crates.io and `homebrew.yml` builds the binaries, cuts a GitHub release and
/// writes the tap. None of it is reversible; a yanked crates.io version is still
/// a spent version number.
pub fn release(sh: &Sh, spec: &str, args: &[&str]) -> Result<()> {
    let (mut push, mut verify) = (false, true);
    for arg in args {
        match *arg {
            "--push" => push = true,
            "--no-verify" => verify = false,
            other => {
                return Err(format!(
                    "unknown option `{other}`\n\
                     usage: cargo xtask release <patch|minor|major|X.Y.Z> [--push] [--no-verify]"
                ));
            }
        }
    }

    let current = workspace_version(sh)?;
    let next = current.bump(spec)?;
    let tag = format!("v{next}");

    // Everything that can say "no" says it before anything is written. A
    // half-applied release is a working tree to untangle by hand, and the whole
    // point of this command is not doing that.
    preflight(sh, next, &tag)?;

    if verify {
        println!("\n\x1b[1m━━ CI ━━\x1b[0m");
        crate::ci(sh)?;
    } else {
        println!("skipping CI (--no-verify)");
    }

    println!("\n\x1b[1m━━ {current} -> {next} ━━\x1b[0m");
    set_version(sh, next)?;
    cut_changelog(sh, next)?;

    // Only the three files a release moves, named explicitly: whatever else is
    // in the tree stays out of the release commit.
    sh.run("git", &["add", "Cargo.toml", "Cargo.lock", CHANGELOG])?;
    sh.run("git", &["commit", "-m", &format!("chore: bump to {next}")])?;
    // Annotated, like every tag before it — the release workflows read
    // `github.ref_name`, and `git describe` wants an object to read.
    sh.run("git", &["tag", "-a", &tag, "-m", &tag])?;

    let branch = sh.capture("git", &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let branch = branch.trim().to_string();

    if !push {
        println!(
            "\n\x1b[32m{tag} is committed and tagged locally.\x1b[0m\n\n\
             Nothing has left this machine. To release:\n\n    \
             git push origin {branch}\n    \
             git push origin {tag}\n\n\
             The tag is what publishes: `publish.yml` uploads {} crates to crates.io\n\
             and `homebrew.yml` builds the binaries and writes the tap. Neither can be undone.\n\n\
             To undo locally instead: git tag -d {tag} && git reset --hard HEAD~1\n",
            publish_order(sh)?.len(),
        );
        return Ok(());
    }

    sh.run("git", &["push", "origin", &branch])?;
    sh.run("git", &["push", "origin", &tag])?;
    println!(
        "\n\x1b[32m{tag} pushed.\x1b[0m Publish and Homebrew are running:\n    \
         https://github.com/diaryx-org/prov/actions\n"
    );
    Ok(())
}

/// Refuse a release that is already doomed: dirty tree, wrong branch, a tag that
/// exists, a version crates.io has already seen, or no git-cliff to write the
/// changelog with.
fn preflight(sh: &Sh, next: Version, tag: &str) -> Result<()> {
    sh.require(
        "git-cliff",
        "nix profile install nixpkgs#git-cliff, or cargo install git-cliff",
    )?;

    if !sh
        .capture("git", &["status", "--porcelain"])?
        .trim()
        .is_empty()
    {
        return Err(
            "the working tree is dirty — commit or stash first, so the release commit holds \
             only the version bump and the changelog"
                .into(),
        );
    }

    let branch = sh.capture("git", &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if branch.trim() != "main" {
        return Err(format!(
            "on branch `{}`, and prov releases from `main`",
            branch.trim()
        ));
    }

    if !sh
        .capture("git", &["tag", "--list", tag])?
        .trim()
        .is_empty()
    {
        return Err(format!("tag {tag} already exists locally"));
    }
    if !sh
        .capture("git", &["ls-remote", "--tags", "origin", tag])?
        .trim()
        .is_empty()
    {
        return Err(format!("tag {tag} already exists on origin"));
    }

    // The tag is not the only way a version gets spent — 0.5.0 went to crates.io
    // from a laptop, untagged — so ask the registry rather than the tag list.
    for name in publish_order(sh)? {
        if already_published(sh, &name, next)? {
            return Err(format!(
                "{name} {next} is already on crates.io\n\
                 hint: a published version number can never be reused; release {} instead",
                Version {
                    patch: next.patch + 1,
                    ..next
                }
            ));
        }
    }

    // A release cut on a stale main is a release missing commits. Fetch is
    // advisory — a laptop offline enough to fail it can still cut the local
    // commit and push later.
    if sh
        .capture("git", &["fetch", "--quiet", "origin", "main"])
        .is_ok()
    {
        let behind = sh.capture("git", &["rev-list", "--count", "HEAD..origin/main"])?;
        if behind.trim() != "0" {
            return Err(format!(
                "main is {} commits behind origin/main — pull first",
                behind.trim()
            ));
        }
    } else {
        println!("warning: could not reach origin; releasing against the local main");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_move_forward_only() {
        let current = Version::parse("0.5.0").unwrap();
        assert_eq!(current.bump("patch").unwrap().to_string(), "0.5.1");
        assert_eq!(current.bump("minor").unwrap().to_string(), "0.6.0");
        assert_eq!(current.bump("major").unwrap().to_string(), "1.0.0");
        assert_eq!(current.bump("0.9.3").unwrap().to_string(), "0.9.3");
        assert!(current.bump("0.4.0").is_err(), "a release cannot go back");
        assert!(current.bump("0.5.0").is_err(), "nor stand still");
        assert!(current.bump("0.5").is_err());
        assert!(
            current.bump("0.5.0-rc.1").is_err(),
            "unparsed, not truncated"
        );
    }

    /// The internal dependency rewrite is the half of the bump that nothing
    /// downstream would notice going wrong until a consumer resolved last
    /// release's siblings.
    #[test]
    fn path_dependencies_follow_the_workspace_version() {
        let next = Version::parse("0.6.0").unwrap();
        assert_eq!(
            retarget_path_dependency(
                r#"prov-graph = { path = "prov-graph", version = "0.5.0", default-features = false }"#,
                next
            )
            .unwrap(),
            r#"prov-graph = { path = "prov-graph", version = "0.6.0", default-features = false }"#
        );
        // An external dependency has no path, and must not be touched.
        assert_eq!(
            retarget_path_dependency(r#"fig = { version = "3.1" }"#, next),
            None
        );
        assert_eq!(retarget_path_dependency(r#"version = "0.5.0""#, next), None);
    }

    /// Both spellings of an inherited dependency, and the section forms the
    /// manifests actually use.
    #[test]
    fn member_dependencies_are_found_in_every_spelling() {
        let members: Vec<String> = ["prov-graph", "prov-store", "prov-cli"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let manifest = r#"
[package]
name = "prov-cli"

[dependencies]
prov-graph = { workspace = true, default-features = false }
prov-store.workspace = true
fig.workspace = true

[dev-dependencies.prov-cli]
path = "."
"#;
        assert_eq!(
            dependencies_on_members(manifest, &members),
            vec!["prov-cli", "prov-graph", "prov-store"]
        );
        // `[package]`'s own keys are not dependencies.
        assert!(dependencies_on_members("[package]\nprov-graph = 1\n", &members).is_empty());
    }

    /// The real workspace, ordered: every crate after the ones it depends on,
    /// xtask left out, and nothing missing.
    #[test]
    fn publish_order_respects_the_workspace() {
        let sh = Sh::new();
        let order = publish_order(&sh).unwrap();
        let members = members(&sh).unwrap();

        assert!(
            !order.contains(&"xtask".to_string()),
            "xtask is publish = false"
        );
        for member in &members {
            if member != "xtask" {
                assert!(
                    order.contains(member),
                    "`{member}` would never be published"
                );
            }
        }

        for (position, name) in order.iter().enumerate() {
            let manifest = sh.read(&format!("{name}/Cargo.toml")).unwrap();
            for dep in dependencies_on_members(&manifest, &members) {
                let dep_position = order.iter().position(|c| *c == dep);
                assert!(
                    dep_position.is_some_and(|d| d < position),
                    "`{name}` is published before its dependency `{dep}`",
                );
            }
        }
    }

    /// The splice touches the region and nothing else — not the prose above it,
    /// and not a single released section below.
    #[test]
    fn rewriting_leaves_everything_outside_the_region_alone() {
        let text = format!(
            "# Changelog\n\nprose\n\n## Unreleased\n\n{BEGIN}\n\nold\n\n{END}\n\n## v0.4.0 — 2026-08-07\n\nkept\n"
        );
        let refreshed = rewrite(&text, "new", None).unwrap();
        assert!(refreshed.contains("\nnew\n") && !refreshed.contains("\nold\n"));
        assert!(refreshed.contains("# Changelog\n\nprose"));
        assert!(refreshed.ends_with("## v0.4.0 — 2026-08-07\n\nkept\n"));

        let cut = rewrite(&text, EMPTY_REGION, Some("## v0.5.0 — 2026-08-18\n\nnew\n")).unwrap();
        let released = cut.find("## v0.5.0").unwrap();
        assert!(
            released > cut.find(END).unwrap(),
            "released sections go below the region"
        );
        assert!(
            released < cut.find("## v0.4.0").unwrap(),
            "newest release first"
        );
        assert!(cut.contains(EMPTY_REGION));
    }

    #[test]
    fn rewriting_a_changelog_without_markers_is_an_error() {
        assert!(rewrite("# Changelog\n", "new", None).is_err());
    }
}
