//! `tree --follow` and `check --follow` — the reader crossing a confirmed
//! boundary.
//!
//! The federation these build is the motivating shape: an `org` workspace whose
//! `contents` is a manifest of foreign ids, committed with none of the checkouts
//! present because a name is not a path, plus the checkouts themselves — each a
//! workspace in its own right, each naming `org` as what contains it.
//!
//! What is worth pinning is not that following works. It is that it is *opt-in*
//! (reachability-boundedness is what makes prov usable inside a larger
//! repository, and every invocation would otherwise cost the size of the
//! federation), that a boundary it declines to cross says why instead of going
//! quiet, and that a followed `check` stays **grouped** — one report per
//! workspace, each in its own terms, because a relative path means nothing once
//! it has crossed a root.

use std::path::{Path, PathBuf};
use std::process::Command;

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

/// Three workspaces beside each other and a peer map, all thrown away together.
///
/// Siblings rather than nested, so no workspace scans another's documents into
/// its own index — the boundary has to be the only way across.
struct Federation {
    root: PathBuf,
    peers: PathBuf,
    org: PathBuf,
    alpha: PathBuf,
    beta: PathBuf,
}

impl Federation {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("prov-follow-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // Canonical, because the peer map records what it is given and the
        // output quotes it back — on macOS the temp dir is a symlink
        // (`/var` → `/private/var`), so an uncanonicalized expectation would
        // compare two spellings of one directory.
        let root = root.canonicalize().unwrap();
        let peers = root.join("peers");
        std::fs::write(&peers, "# prov peer map\n").unwrap();

        // The outer root: a manifest of foreign ids, one of which (`gamma`) this
        // device has never heard of.
        let org = Self::workspace(
            &root,
            "org",
            "workspace_id: org\nroot: README.md\nid_storage: frontmatter\n",
            "---\nid: org1\ntitle: Org\ncontents:\n- 'id:alpha/alpha1'\n\
             - 'id:beta/beta1'\n- 'id:gamma/gamma1'\n---\n",
        );
        // Each peer is the phase-0 shape: a node naming its root, and a root
        // that says what contains it by foreign id.
        let alpha = Self::workspace(
            &root,
            "alpha",
            "workspace_id: alpha\nroot: README.md\nid_storage: frontmatter\n",
            "---\nid: alpha1\ntitle: Alpha\npart_of: 'id:org/org1'\ncontents:\n- notes.md\n---\n",
        );
        std::fs::write(
            alpha.join("notes.md"),
            "---\nid: alpha2\ntitle: Alpha notes\npart_of: README.md\n---\n",
        )
        .unwrap();
        let beta = Self::workspace(
            &root,
            "beta",
            "workspace_id: beta\nroot: README.md\nid_storage: frontmatter\n",
            "---\nid: beta1\ntitle: Beta\npart_of: 'id:org/org1'\n---\n",
        );

        let federation = Self {
            root,
            peers,
            org,
            alpha,
            beta,
        };
        federation.record("alpha", &federation.alpha);
        federation.record("beta", &federation.beta);
        // The generated page is part of a clean workspace, and `check` says so
        // in every one of them — so a federation that is meant to check clean
        // has to have written it everywhere, not just at home.
        for ws in [&federation.org, &federation.alpha, &federation.beta] {
            let (ok, _, err) = federation.run(ws, &["about"]);
            assert!(ok, "about: {err}");
        }
        federation
    }

    fn workspace(root: &Path, dir: &str, node: &str, doc: &str) -> PathBuf {
        let at = root.join(dir);
        std::fs::create_dir_all(&at).unwrap();
        std::fs::write(at.join("prov.yaml"), node).unwrap();
        std::fs::write(at.join("README.md"), doc).unwrap();
        at
    }

    /// One line of the peer map: the workspace's own name, then where it lives
    /// on this device.
    fn record(&self, name: &str, dir: &Path) {
        let mut text = std::fs::read_to_string(&self.peers).unwrap();
        text.push_str(&format!("{name} {}\n", dir.display()));
        std::fs::write(&self.peers, text).unwrap();
    }

    fn run(&self, dir: &Path, args: &[&str]) -> (bool, String, String) {
        run(dir, &self.peers, args)
    }
}

impl Drop for Federation {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn without_follow_a_boundary_is_the_leaf_it_always_was() {
    // Off by default is the property, not a default anyone should have to think
    // about: the walk is bounded by what one root reaches, and the peers are
    // on record the whole time.
    let fed = Federation::new("unfollowed");
    let (ok, out, _) = fed.run(&fed.org, &["tree"]);
    assert!(ok);
    assert!(
        out.contains("(workspace alpha, id alpha1 — not followed)"),
        "{out}"
    );
    assert!(
        !out.contains("Alpha notes"),
        "nothing of the peer's own tree crossed: {out}"
    );
    assert!(
        !out.contains(&fed.alpha.display().to_string()),
        "and the peer map was not consulted: {out}"
    );
}

#[test]
fn follow_hangs_each_peers_own_subtree_where_its_leaf_was() {
    let fed = Federation::new("followed");
    let (ok, out, _) = fed.run(&fed.org, &["tree", "--follow"]);
    assert!(ok, "{out}");
    // The node *is* the peer's document now, so its path is in the peer's terms
    // — which is why the marker has to say whose terms, and where they are.
    assert!(
        out.contains(&format!(
            "README.md — Alpha  ⇒ workspace alpha ({})",
            fed.alpha.display()
        )),
        "{out}"
    );
    assert!(
        out.contains(&format!("⇒ workspace beta ({})", fed.beta.display())),
        "{out}"
    );
    // And the walk carried on inside the peer.
    assert!(out.contains("notes.md — Alpha notes"), "{out}");
}

#[test]
fn a_peer_this_device_does_not_have_stays_a_leaf_with_the_reason() {
    // The difference `--follow` makes to a boundary it cannot cross: the same
    // leaf an unfollowed tree shows, now saying why it is still a leaf.
    let fed = Federation::new("refused");
    let (ok, out, _) = fed.run(&fed.org, &["tree", "--follow"]);
    assert!(ok, "{out}");
    assert!(
        out.contains(
            "(workspace gamma, id gamma1 — not followed: no workspace of that name is on record)"
        ),
        "{out}"
    );
}

#[test]
fn a_bound_of_zero_crosses_nothing_and_says_so() {
    // The depth counts crossings, so zero is "look, do not cross" rather than
    // "print nothing".
    let fed = Federation::new("depth-zero");
    let (ok, out, _) = fed.run(&fed.org, &["tree", "--follow=0"]);
    assert!(ok, "{out}");
    assert!(out.contains("README.md — Org"), "{out}");
    assert!(
        out.contains("not followed: the descent's crossing bound was reached"),
        "{out}"
    );
    assert!(!out.contains("Alpha notes"), "{out}");
}

#[test]
fn check_follow_reports_every_reachable_workspace_and_a_clean_one_exits_zero() {
    let fed = Federation::new("check-clean");
    let (ok, out, err) = fed.run(&fed.org, &["check", "--follow"]);
    assert!(ok, "a clean federation exits 0: {out}{err}");
    assert!(out.trim().is_empty(), "no findings on stdout: {out}");
    for (name, dir) in [
        ("org", &fed.org),
        ("alpha", &fed.alpha),
        ("beta", &fed.beta),
    ] {
        assert!(
            err.contains(&format!("── workspace {name} ({}) ──", dir.display())),
            "a header per workspace, saying where it is: {err}"
        );
    }
    // A refusal is not a finding — it is the reason the report is shorter than
    // the federation — so it narrates and does not touch the exit code.
    assert!(err.contains("not followed: `gamma`"), "{err}");
    assert!(err.contains("no findings across 3 workspace(s)"), "{err}");
}

#[test]
fn a_finding_in_a_peer_carries_that_peers_name_and_fails_the_run() {
    let fed = Federation::new("check-broken");
    // Planted in the *peer*, so the origin is clean and the run fails anyway:
    // what `--follow` promises is every reachable workspace's own check.
    let doc = fed.alpha.join("README.md");
    let text = std::fs::read_to_string(&doc)
        .unwrap()
        .replace("- notes.md", "- notes.md\n- nowhere.md");
    std::fs::write(&doc, text).unwrap();

    let (ok, out, err) = fed.run(&fed.org, &["check", "--follow"]);
    assert!(!ok, "findings anywhere fail the run: {out}{err}");
    // Prefixed, because a subject is a path in one workspace's terms and the
    // line may be read on its own at the end of a pipe.
    assert!(
        out.lines()
            .any(|l| l.starts_with("alpha: ") && l.contains("nowhere.md")),
        "{out}"
    );
    assert!(err.contains("1 finding(s) across 3 workspace(s)"), "{err}");

    // The same run, grouped for a program: an array of workspaces rather than
    // an array of findings, origin first.
    let (ok, json, err) = fed.run(&fed.org, &["check", "--follow", "--json"]);
    assert!(!ok, "the exit code is unchanged by --json: {json}");
    assert!(err.trim().is_empty(), "nothing narrated in --json: {err}");
    let workspaces: Vec<&str> = json
        .lines()
        .filter_map(|l| l.trim().strip_prefix("\"workspace\": "))
        .collect();
    assert_eq!(
        workspaces,
        ["\"org\",", "\"alpha\",", "\"beta\","],
        "{json}"
    );
    assert!(
        json.contains(&format!("\"root\": \"{}\"", fed.alpha.display())),
        "each workspace says what its paths are relative to: {json}"
    );
    assert!(json.contains("\"kind\": \"broken_link\""), "{json}");
    // Grouped, not merged: the clean workspaces are present and empty rather
    // than absent.
    assert_eq!(
        json.matches("\"findings\": []").count(),
        2,
        "org and beta report clean: {json}"
    );
}

#[test]
fn follow_writes_nothing_and_narrows_nothing() {
    let fed = Federation::new("conflicts");
    // No writes across a boundary, and no subject path that would mean one
    // workspace's file in another's terms. Both are clap's refusals, so neither
    // costs the handler a branch.
    let (ok, _, err) = fed.run(&fed.org, &["check", "--follow", "--fix"]);
    assert!(!ok);
    assert!(err.contains("cannot be used with"), "{err}");
    let (ok, _, err) = fed.run(&fed.org, &["check", "--follow", "--only", "README.md"]);
    assert!(!ok);
    assert!(err.contains("cannot be used with"), "{err}");
}

#[test]
fn explore_takes_unverified() {
    // Explore is interactive, so what is testable from here is that the flag is
    // on the command and that the command gets past argument parsing into the
    // workspace it was pointed at.
    let fed = Federation::new("explore");
    let (ok, out, err) = fed.run(&fed.org, &["explore", "--unverified", "nope.md"]);
    assert!(!ok, "{out}{err}");
    assert!(
        !err.contains("unexpected argument"),
        "the flag is on `explore`: {err}"
    );
    assert!(err.contains("cannot open nope.md"), "{err}");
}
