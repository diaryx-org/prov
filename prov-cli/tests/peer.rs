//! `prov peer` — following a cross-workspace reference, and refusing to.
//!
//! The library resolves `id:notes/ajp7eq` to "workspace `notes`, id `ajp7eq`"
//! and stops. Everything past that is this device's peer map plus the peer's own
//! registry, and the interesting property is not that it works — it is *when it
//! declines to*.
//!
//! A peer map is a claim about a name, and a wrong claim does not fail loudly on
//! its own: it resolves to real documents in the wrong archive. That is why
//! there is no peer table in `prov.yaml`, and it stays true of a device-local
//! map, which can be hand-edited or simply outlived by the directory it names.
//! So the claim is checked where it is *used*: the peer is asked what it calls
//! itself, and a workspace that answers with a different name is reported rather
//! than followed. These tests pin that refusal, and pin that `--unverified`
//! cannot buy past it — the escape hatch is for missing evidence, never for
//! evidence pointing the other way.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Run a command with this test's own peer map, keeping the streams apart:
/// stdout carries the resolved path, stderr the narration.
fn run(dir: &Path, peers: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_prov"))
        .current_dir(dir)
        .args(args)
        .env("PROV_QUIET", "1")
        .env("PROV_PEERS", peers)
        .output()
        .expect("run prov");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A scratch directory holding two workspaces and a peer map, all thrown away
/// together.
struct Scratch {
    root: PathBuf,
    peers: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("prov-peer-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let peers = root.join("peers");
        Self { root, peers }
    }

    /// An initialized workspace under this scratch, named `name` unless it is
    /// empty (in which case it stays anonymous — a state the map has to cope
    /// with, not an oversight).
    fn workspace(&self, dir: &str, name: &str) -> PathBuf {
        let path = self.root.join(dir);
        std::fs::create_dir_all(&path).unwrap();
        // Canonical, because `peer add` records a canonical path and the
        // resolved answer comes back through it — on macOS the temp dir is a
        // symlink (`/var` → `/private/var`), so an uncanonicalized expectation
        // would compare two spellings of the same directory.
        let path = path.canonicalize().unwrap();
        let (ok, _, err) = run(&path, &self.peers, &["init", "--yes"]);
        assert!(ok, "init {dir}: {err}");
        if !name.is_empty() {
            let (ok, _, err) = run(&path, &self.peers, &["id", "--workspace", name]);
            assert!(ok, "name {dir}: {err}");
        }
        path
    }

    fn run(&self, dir: &Path, args: &[&str]) -> (bool, String, String) {
        run(dir, &self.peers, args)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Register the root document of `ws` and return its bare id.
fn register_root(scratch: &Scratch, ws: &Path) -> String {
    let (ok, out, err) = scratch.run(ws, &["id", "index.md"]);
    assert!(ok, "id: {out}{err}");
    // `id` bootstraps the registry on first use and narrates that too, so take
    // the line that is actually the handle.
    let target = out
        .lines()
        .find(|l| l.starts_with("id:"))
        .unwrap_or_else(|| panic!("id prints an id target: {out}{err}"));
    target.trim_start_matches("id:").to_string()
}

#[test]
fn a_confirmed_peer_resolves_to_a_file_in_it() {
    let scratch = Scratch::new("confirmed");
    let notes = scratch.workspace("notes", "notes");
    let here = scratch.workspace("here", "here");
    let id = register_root(&scratch, &notes);

    let (ok, _, err) = scratch.run(&here, &["peer", "add", "notes", notes.to_str().unwrap()]);
    assert!(ok, "{err}");

    let (ok, out, err) = scratch.run(&here, &["peer", "resolve", &format!("id:notes/{id}")]);
    assert!(ok, "{err}");
    // The peer's own registry answered — an absolute path, since the caller is
    // standing in a different workspace by construction.
    let resolved = PathBuf::from(out.trim());
    assert!(resolved.is_absolute(), "{out}");
    assert!(resolved.ends_with("index.md"), "{out}");
    assert!(
        resolved.starts_with(&notes),
        "resolved outside the peer: {out}"
    );
}

#[test]
fn a_peer_that_calls_itself_something_else_is_refused_at_use_time() {
    // The failure the whole design is arranged around. The entry was recorded
    // when it was true; the directory now holds a workspace named `journal`.
    // Following it would print a path to real documents in the wrong archive —
    // a wrong answer indistinguishable from a right one.
    let scratch = Scratch::new("mismatch");
    let other = scratch.workspace("other", "notes");
    let here = scratch.workspace("here", "here");
    let id = register_root(&scratch, &other);

    let (ok, _, err) = scratch.run(&here, &["peer", "add", "notes", other.to_str().unwrap()]);
    assert!(ok, "{err}");
    // It resolves while the claim holds.
    assert!(
        scratch
            .run(&here, &["peer", "resolve", &format!("id:notes/{id}")])
            .0
    );

    // Now the peer renames itself, and the map is stale without being touched.
    let (ok, _, err) = scratch.run(&other, &["config", "workspace_id", "journal"]);
    assert!(ok, "{err}");

    let (ok, out, err) = scratch.run(&here, &["peer", "resolve", &format!("id:notes/{id}")]);
    assert!(!ok, "a stale entry must not resolve: {out}");
    assert!(
        err.contains("journal"),
        "the complaint names what it found instead: {err}"
    );
    assert!(out.trim().is_empty(), "nothing on stdout to pipe: {out}");
}

#[test]
fn unverified_does_not_buy_past_a_mismatch() {
    // `--unverified` accepts *absent* evidence. Evidence pointing the other way
    // is not something a flag can override, because there is no reading of it
    // under which the answer is right.
    let scratch = Scratch::new("insist");
    let other = scratch.workspace("other", "journal");
    let here = scratch.workspace("here", "here");
    let id = register_root(&scratch, &other);

    let (ok, _, err) = scratch.run(&here, &["peer", "add", "notes", other.to_str().unwrap()]);
    assert!(ok, "{err}");
    let (ok, out, err) = scratch.run(
        &here,
        &["peer", "resolve", &format!("id:notes/{id}"), "--unverified"],
    );
    assert!(!ok, "insisting must not follow a known-wrong entry: {out}");
    assert!(err.contains("journal"), "{err}");
}

#[test]
fn an_anonymous_peer_is_refused_by_default_and_followed_on_insistence() {
    // Nothing to compare against is not the same as comparing and disagreeing.
    // The peer may well be `notes` — it just has not said so — so this is the
    // case the escape hatch exists for.
    let scratch = Scratch::new("anonymous");
    let notes = scratch.workspace("notes", "");
    let here = scratch.workspace("here", "here");
    let id = register_root(&scratch, &notes);

    let (ok, _, err) = scratch.run(&here, &["peer", "add", "notes", notes.to_str().unwrap()]);
    assert!(ok, "{err}");
    assert!(
        err.contains("does not name itself"),
        "add says so early: {err}"
    );

    let reference = format!("id:notes/{id}");
    let (ok, _, err) = scratch.run(&here, &["peer", "resolve", &reference]);
    assert!(!ok, "an unconfirmed peer is not followed by default");
    assert!(
        err.contains("--unverified"),
        "and the way past is named: {err}"
    );

    let (ok, out, err) = scratch.run(&here, &["peer", "resolve", &reference, "--unverified"]);
    assert!(ok, "{err}");
    assert!(PathBuf::from(out.trim()).starts_with(&notes), "{out}");
}

#[test]
fn a_workspace_nobody_recorded_is_reported_as_absent_not_as_broken() {
    // A foreign reference is carried whether or not it resolves, so "no peer"
    // is a state of this device, and the message says what to do about it.
    let scratch = Scratch::new("unknown");
    let here = scratch.workspace("here", "here");
    let (ok, out, err) = scratch.run(&here, &["peer", "resolve", "id:notes/ajp7eq"]);
    assert!(!ok);
    assert!(err.contains("peer add notes"), "{err}");
    assert!(out.trim().is_empty(), "{out}");
}
