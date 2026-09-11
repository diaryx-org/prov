//! `prov confirm` — the keeper's own record that a document was read and found
//! correct, measured against the workspace's `updated` stamp from then on.
//!
//! What is asserted here is the command's boundary rather than the rule (the
//! library's own tests hold that): where the actor comes from and that none is
//! a refusal, that confirming leaves `updated` alone, that an edit through
//! `set` is what makes `check` report the entry, and that machinery is refused.

use std::path::{Path, PathBuf};
use std::process::Command;

fn run(dir: &Path, peers: &Path, args: &[&str], env: &[(&str, &str)]) -> (bool, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_prov"));
    cmd.current_dir(dir)
        .args(args)
        .env("PROV_QUIET", "1")
        // The actor file lives beside the peer map, so pointing the map into
        // the sandbox is what keeps this machine's own `actor` out of the test.
        .env("PROV_PEERS", peers)
        .env_remove("PROV_ACTOR");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run prov");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn sandbox(tag: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("prov-confirm-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let peers = dir.join("device").join("peers");
    std::fs::create_dir_all(peers.parent().unwrap()).unwrap();
    (dir, peers)
}

fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).unwrap()
}

/// A workspace recording an `updated` field, with one document to confirm.
fn workspace(tag: &str) -> (PathBuf, PathBuf) {
    let (dir, peers) = sandbox(tag);
    for args in [
        &["init", "--yes"][..],
        &["config", "updated", "updated"],
        &["new", "Rust", "--in", "index.md"],
    ] {
        let (ok, out, err) = run(&dir, &peers, args, &[]);
        assert!(ok, "`prov {}`: {out}{err}", args.join(" "));
    }
    (dir, peers)
}

#[test]
fn confirm_appends_and_an_edit_makes_check_report_it() {
    let (dir, peers) = workspace("edit");

    let (ok, out, err) = run(&dir, &peers, &["confirm", "rust.md", "--by", "amh"], &[]);
    assert!(ok, "{out}{err}");
    assert_eq!(out.trim(), "rust.md");
    assert!(err.contains("confirmed rust.md — amh at 2"), "{err:?}");
    assert!(err.contains("(human-confirmed)"), "{err:?}");
    let text = read(&dir, "rust.md");
    assert!(text.contains("confirmed:\n- by: amh\n  at: 2"), "{text}");
    assert!(
        !text.contains("updated:"),
        "confirming never stamps `updated`: {text}"
    );

    let (ok, out, err) = run(&dir, &peers, &["check"], &[]);
    assert!(ok, "{out}{err}");

    // `set` is an edit through prov and stamps `updated`, which is what the
    // confirmation is measured against.
    let (ok, out, err) = run(&dir, &peers, &["set", "rust.md", "summary", "notes"], &[]);
    assert!(ok, "{out}{err}");
    let (ok, out, err) = run(&dir, &peers, &["check"], &[]);
    assert!(!ok, "a stale confirmation is a finding: {out}{err}");
    assert!(
        out.contains("rust.md: confirmation stale — amh confirmed it at"),
        "{out}{err}"
    );
    assert!(
        out.contains("`prov confirm` to confirm it again"),
        "{out}{err}"
    );

    let (ok, out, err) = run(&dir, &peers, &["confirm", "rust.md", "--show"], &[]);
    assert!(ok, "{out}{err}");
    assert!(
        out.starts_with("amh\t2") && out.trim_end().ends_with("\tstale"),
        "{out:?}"
    );
    assert_eq!(err.trim(), "rust.md: unconfirmed");

    // The actor from the environment; confirming again is the repair.
    let (ok, out, err) = run(
        &dir,
        &peers,
        &["confirm", "rust.md"],
        &[("PROV_ACTOR", "agent:tester")],
    );
    assert!(ok, "{out}{err}");
    assert!(err.contains("(machine-confirmed)"), "{err:?}");
    let (ok, out, err) = run(&dir, &peers, &["check"], &[]);
    assert!(ok, "{out}{err}");
}

#[test]
fn no_actor_is_a_refusal_and_the_device_file_is_one_source() {
    let (dir, peers) = workspace("actor");

    let (ok, _, err) = run(&dir, &peers, &["confirm", "rust.md"], &[]);
    assert!(!ok);
    assert!(
        err.contains("--by <ACTOR>") && err.contains("PROV_ACTOR"),
        "{err}"
    );
    assert!(
        err.contains(&peers.with_file_name("actor").display().to_string()),
        "{err}"
    );
    assert!(
        !read(&dir, "rust.md").contains("confirmed:"),
        "nothing was written"
    );

    std::fs::write(
        peers.with_file_name("actor"),
        "# who is at this keyboard\namh\n",
    )
    .unwrap();
    let (ok, out, err) = run(&dir, &peers, &["confirm", "rust.md"], &[]);
    assert!(ok, "{out}{err}");
    assert!(err.contains("— amh at"), "{err:?}");
}

#[test]
fn machinery_is_refused() {
    let (dir, peers) = workspace("machinery");
    let (ok, _, err) = run(&dir, &peers, &["confirm", "prov.yaml", "--by", "amh"], &[]);
    assert!(!ok);
    assert!(err.contains("machinery"), "{err}");
    assert!(!read(&dir, "prov.yaml").contains("confirmed"));
}
