//! `prov gather` and `prov scatter` against the real binary: photographs
//! attached one at a time gathered into one manifest, refused while one of
//! them carries something a manifest row cannot, and scattered back.

use std::path::Path;
use std::process::Command;

fn run(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_prov"))
        .current_dir(dir)
        .args(args)
        .env("PROV_QUIET", "1")
        .output()
        .expect("run prov");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn ok(dir: &Path, args: &[&str]) -> (String, String) {
    let (ok, out, err) = run(dir, args);
    assert!(
        ok,
        "`prov {}` failed:\nstdout:{out}\nstderr:{err}",
        args.join(" ")
    );
    (out, err)
}

fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).unwrap()
}

fn workspace(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("prov-regroup-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    ok(&dir, &["init", "--yes"]);
    for (name, byte) in [("a.jpg", 1u8), ("b.jpg", 2)] {
        std::fs::write(dir.join(name), [0xff, 0xd8, byte]).unwrap();
        ok(&dir, &["attach", name, "--in", "index.md"]);
    }
    dir
}

#[test]
fn photographs_gather_into_an_album_and_scatter_back() {
    let dir = workspace("round-trip");

    let (_, err) = ok(
        &dir,
        &["gather", "a.jpg", "b.jpg", "--into", "album", "--dry-run"],
    );
    assert!(err.contains("would move a.jpg -> album/a.jpg"), "{err}");
    assert!(dir.join("a.jpg").exists(), "a dry run moves nothing");

    let (out, _) = ok(&dir, &["gather", "a.jpg", "b.jpg", "--into", "album"]);
    assert_eq!(out.trim(), "album.yaml");
    assert!(read(&dir, "album.yaml").contains("title: Album"));
    assert!(dir.join("album/b.jpg").exists());
    ok(&dir, &["check"]);

    let (out, err) = ok(&dir, &["scatter", "album"]);
    assert!(err.contains("the node is now their index"), "{err}");
    assert_eq!(out.lines().count(), 2, "{out}");
    assert!(!dir.join("album.manifest.yaml").exists());
    ok(&dir, &["check"]);
}

#[test]
fn a_field_only_one_photograph_carries_refuses_until_discarded() {
    let dir = workspace("discard");
    ok(
        &dir,
        &["set", "a.jpg.yaml", "date_of_document", "2025-05-02"],
    );

    let (ok_, _, err) = run(&dir, &["gather", "a.jpg", "b.jpg", "--into", "album"]);
    assert!(!ok_, "{err}");
    assert!(
        err.contains("a.jpg.yaml: field `date_of_document`") && err.contains("--discard"),
        "{err}"
    );
    assert!(dir.join("a.jpg").exists(), "a refusal moves nothing");

    ok(
        &dir,
        &[
            "gather",
            "a.jpg",
            "b.jpg",
            "--into",
            "album",
            "--discard",
            "date_of_document",
        ],
    );
    ok(&dir, &["check"]);
}
