//! `prov backup --to <path> [--zip]` — the whole-tree, outside-the-graph copy
//! (see `prov_cli::backup`'s module doc for the design intent). These tests
//! drive the built binary end to end: a real vault, a real destination
//! directory, real files on disk.

use std::path::{Path, PathBuf};
use std::process::Command;

fn prov() -> Command {
    Command::new(env!("CARGO_BIN_EXE_prov"))
}

fn run(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = prov()
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run prov");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A fresh `base/vault` — `vault` an initialized workspace with a nested
/// document and a hidden file, `base` a neutral sibling directory to hold
/// backup destinations outside the vault.
fn isolated(tag: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("prov-backup-cli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let vault = base.join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    let (ok, _, err) = run(&vault, &["init", "--yes"]);
    assert!(ok, "init the vault: {err}");
    let (ok, _, err) = run(&vault, &["new", "Rust", "--in", "index.md"]);
    assert!(ok, "seed a nested document: {err}");
    // A hidden file the spec says a backup must carry too.
    std::fs::write(vault.join(".hidden"), "shh").unwrap();
    (base, vault)
}

#[test]
fn copies_the_whole_tree_including_nested_dirs_and_hidden_files() {
    let (base, vault) = isolated("roundtrip");
    let dest = base.join("dest");
    let (ok, out, err) = run(&vault, &["backup", "--to", dest.to_str().unwrap()]);
    assert!(ok, "backup failed: {err}");
    assert_eq!(out.trim(), dest.to_str().unwrap());

    // Every source path exists, verbatim, at the destination.
    let walk = |root: &Path| -> Vec<String> {
        fn go(dir: &Path, root: &Path, out: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.is_dir() {
                    go(&path, root, out);
                } else {
                    out.push(
                        path.strip_prefix(root)
                            .unwrap()
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
        }
        let mut out = Vec::new();
        go(root, root, &mut out);
        out.sort();
        out
    };
    let src_files = walk(&vault);
    let dst_files = walk(&dest);
    assert_eq!(src_files, dst_files);
    assert!(dst_files.iter().any(|f| f == ".hidden"));
    assert_eq!(
        std::fs::read_to_string(dest.join(".hidden")).unwrap(),
        "shh"
    );
    // A nested document's bytes match exactly.
    for f in &src_files {
        assert_eq!(
            std::fs::read(vault.join(f)).unwrap(),
            std::fs::read(dest.join(f)).unwrap(),
            "byte mismatch for {f}"
        );
    }
}

#[test]
fn refuses_a_destination_inside_the_workspace_root() {
    let (_base, vault) = isolated("selfcopy");
    let inside = vault.join("nested-backup");
    let (ok, _out, err) = run(&vault, &["backup", "--to", inside.to_str().unwrap()]);
    assert!(!ok, "must refuse a self-nested destination");
    assert!(
        err.contains("inside the workspace root") || err.contains("itself"),
        "error should explain the self-copy refusal: {err}"
    );
    assert!(!inside.exists(), "must not have created anything");
}

#[test]
fn refuses_the_workspace_root_itself_as_the_destination() {
    let (_base, vault) = isolated("selfcopy-root");
    let (ok, _out, err) = run(&vault, &["backup", "--to", "."]);
    assert!(!ok, "must refuse backing up onto its own root");
    assert!(err.contains("itself") || err.contains("inside the workspace root"));
}

#[test]
fn refuses_a_nonempty_existing_directory() {
    let (base, vault) = isolated("nonempty");
    let dest = base.join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("stray.txt"), "already here").unwrap();
    let (ok, _out, err) = run(&vault, &["backup", "--to", dest.to_str().unwrap()]);
    assert!(!ok, "must refuse a non-empty existing directory");
    assert!(err.contains("not empty") || err.contains("already exists"));
    // The stray file must be untouched — no merge, no partial overwrite.
    assert_eq!(
        std::fs::read_to_string(dest.join("stray.txt")).unwrap(),
        "already here"
    );
}

#[test]
fn accepts_an_empty_existing_directory() {
    let (base, vault) = isolated("empty-dir-ok");
    let dest = base.join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    let (ok, _out, err) = run(&vault, &["backup", "--to", dest.to_str().unwrap()]);
    assert!(ok, "an empty existing directory must be accepted: {err}");
    assert!(dest.join("index.md").exists());
}

#[test]
fn refuses_an_existing_file_as_the_destination() {
    let (base, vault) = isolated("file-refused");
    let dest = base.join("dest");
    std::fs::write(&dest, "not a directory").unwrap();
    let (ok, _out, err) = run(&vault, &["backup", "--to", dest.to_str().unwrap()]);
    assert!(!ok, "must refuse clobbering an existing file");
    assert!(err.contains("already exists"));
}

#[test]
fn zip_produces_an_archive_with_the_expected_entries() {
    let (base, vault) = isolated("zip-entries");
    let dest = base.join("vault-backup.zip");
    let (ok, out, err) = run(&vault, &["backup", "--to", dest.to_str().unwrap(), "--zip"]);
    assert!(ok, "zip backup failed: {err}");
    assert_eq!(out.trim(), dest.to_str().unwrap());
    assert!(dest.is_file());

    let bytes = std::fs::read(&dest).unwrap();
    // Parse the end-of-central-directory record (fixed 22 bytes, no comment).
    let eocd = &bytes[bytes.len() - 22..];
    assert_eq!(&eocd[0..4], &0x0605_4b50u32.to_le_bytes(), "EOCD signature");
    let count = u16::from_le_bytes([eocd[10], eocd[11]]);
    let central_offset = u32::from_le_bytes([eocd[16], eocd[17], eocd[18], eocd[19]]) as usize;

    let mut pos = central_offset;
    let mut names = Vec::new();
    for _ in 0..count {
        assert_eq!(
            &bytes[pos..pos + 4],
            &0x0201_4b50u32.to_le_bytes(),
            "central directory signature"
        );
        let method = u16::from_le_bytes([bytes[pos + 10], bytes[pos + 11]]);
        assert_eq!(method, 0, "store-only, no compression");
        let name_len = u16::from_le_bytes([bytes[pos + 28], bytes[pos + 29]]) as usize;
        let name_start = pos + 46;
        let name = String::from_utf8(bytes[name_start..name_start + name_len].to_vec()).unwrap();
        names.push(name);
        pos = name_start + name_len;
    }

    let vault_name = vault.file_name().unwrap().to_string_lossy().into_owned();
    for expect in [
        format!("{vault_name}/"),
        format!("{vault_name}/index.md"),
        format!("{vault_name}/.hidden"),
    ] {
        assert!(
            names.contains(&expect),
            "missing entry {expect} in {names:?}"
        );
    }
}

#[test]
fn zip_refuses_a_nonempty_directory_path() {
    let (base, vault) = isolated("zip-dir-refused");
    let dest = base.join("dest-dir");
    std::fs::create_dir_all(&dest).unwrap();
    let (ok, _out, err) = run(&vault, &["backup", "--to", dest.to_str().unwrap(), "--zip"]);
    assert!(!ok, "zip mode must refuse a directory destination");
    assert!(err.contains("file path") || err.contains("directory"));
}
