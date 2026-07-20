//! `prov config --home root|sidecar` — relocating workspace policy between its
//! two homes (the root's inline `prov:` block and the `prov.yaml` sidecar)
//! without changing the effective config. The two homes read identically; this
//! command only moves where the bytes live (DESIGN §2, "two homes, one
//! vocabulary").

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

fn vault(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("prov-home-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (ok, out) = run(&dir, &["init", "--yes"]);
    assert!(ok, "init: {out}");
    dir
}

fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).unwrap_or_default()
}

#[test]
fn policy_round_trips_between_the_two_homes_without_changing_the_effective_config() {
    let dir = vault("round-trip");
    // Two settings land in the sidecar (the default write home).
    assert!(run(&dir, &["config", "fixity", "all"]).0);
    assert!(run(&dir, &["config", "identity", "eager"]).0);
    assert!(dir.join("prov.yaml").exists());

    // → root: policy inlines into the root block; the sidecar is gone.
    let (ok, out) = run(&dir, &["config", "--home", "root"]);
    assert!(ok, "{out}");
    assert!(!dir.join("prov.yaml").exists(), "sidecar removed: {out}");
    assert!(read(&dir, "index.md").contains("prov:"), "inline block present");
    assert_eq!(run(&dir, &["config", "fixity"]).1.trim(), "all");
    assert_eq!(run(&dir, &["config", "identity"]).1.trim(), "eager");
    assert!(run(&dir, &["check"]).0, "clean after --home root");

    // → sidecar: policy moves back out; the root block is cleared.
    let (ok, out) = run(&dir, &["config", "--home", "sidecar"]);
    assert!(ok, "{out}");
    assert!(dir.join("prov.yaml").exists(), "sidecar recreated");
    assert!(
        !read(&dir, "index.md").contains("\nprov:"),
        "root block cleared: {}",
        read(&dir, "index.md")
    );
    // The effective config is unchanged by the round-trip.
    assert_eq!(run(&dir, &["config", "fixity"]).1.trim(), "all");
    assert_eq!(run(&dir, &["config", "identity"]).1.trim(), "eager");
    assert!(run(&dir, &["check"]).0, "clean after --home sidecar");
}

#[test]
fn only_recognized_policy_travels_and_hand_added_fields_are_never_lost() {
    let dir = vault("safety");
    assert!(run(&dir, &["config", "fixity", "all"]).0);
    // Hand-add a non-policy field to the sidecar.
    let mut sidecar = read(&dir, "prov.yaml");
    sidecar.push_str("note: keep me\n");
    std::fs::write(dir.join("prov.yaml"), sidecar).unwrap();

    // --home root moves policy but must NOT delete a sidecar carrying `note`.
    let (ok, out) = run(&dir, &["config", "--home", "root"]);
    assert!(ok, "{out}");
    assert!(dir.join("prov.yaml").exists(), "sidecar kept: {out}");
    assert!(read(&dir, "prov.yaml").contains("note: keep me"), "note kept");
    // The policy moved to the root; the user field did not leak into it.
    assert!(read(&dir, "index.md").contains("fixity: all"), "policy inlined");
    assert!(
        !read(&dir, "index.md").contains("note:"),
        "user field did not leak into the prov: block"
    );
    assert_eq!(run(&dir, &["config", "fixity"]).1.trim(), "all");
}
