//! Fixtures the mutation test modules share.
//!
//! Each verb's tests live in its own file, so the workspace-building helpers
//! they all need cannot hang off any one of them.
//!
//! These engine tests use YAML fixtures throughout, so every one of those
//! modules — and this one — runs whenever the (default) `yaml` feature is on.

use std::path::{Path, PathBuf};

use crate::workspace::Workspace;

// Re-exported so each verb's `mod tests` can pull the whole fixture surface —
// helpers and the concrete workspace types they hand back — from one glob.
pub(super) use crate::fs_faults::FailAtWrite;
pub(super) use crate::identity::Minter;
pub(super) use prov_graph::exec::block_on;
pub(super) use prov_graph::fs::StdFs;
use prov_store::fs::Storage;
pub(super) use prov_store::index::FileIndex;

pub(super) fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

pub(super) fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).unwrap()
}

pub(super) fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("prov-mutate-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub(super) fn ws(dir: &Path) -> Workspace<StdFs> {
    Workspace::builder(StdFs).root(dir).build()
}

/// An identity-bearing workspace: lazy minting, persistent-style index.
pub(super) fn id_ws(dir: &Path) -> Workspace<StdFs, Minter, FileIndex> {
    Workspace::builder(StdFs)
        .root(dir)
        .identity(Minter::lazy(42))
        .index(FileIndex::new(fig::Format::Yaml))
        .build()
}

/// A workspace whose registry is a real document on disk, so its write is
/// staged rather than left to the caller.
///
/// Seeds the host only if it is not already there, so a test can rebuild the
/// workspace over a directory mid-flight — the way a second CLI run picks up
/// the registry the first one left — instead of wiping it.
pub(super) fn hosted_registry_ws<FS: Storage>(
    dir: &Path,
    fs: FS,
) -> Workspace<FS, Minter, FileIndex> {
    let host = "registry.yaml";
    if !dir.join(host).exists() {
        write(dir, host, "title: ID registry\n");
    }
    let text = std::fs::read_to_string(dir.join(host)).unwrap();
    Workspace::builder(fs)
        .root(dir)
        .identity(Minter::eager(7))
        .index(FileIndex::parse(Path::new(host), &text).unwrap())
        .build()
}

// ---- transactional writes (see `crate::change`) ----
//
// The property the fixtures below exist for is the crate's whole reason to
// exist: link maintenance spans documents, so a mutation that half-lands is
// worse than one that does not land at all. Each test that uses them drives a
// real operation over a backend that fails one write, and asserts the workspace
// is byte-for-byte as it was found — not merely "check-clean", which a
// torn-but-detectable state would also be.

/// The whole workspace as `(relative path, contents)`, sorted — so a test can
/// assert nothing anywhere changed, rather than spot-checking the files it
/// happened to think of.
pub(super) fn snapshot(dir: &Path) -> Vec<(String, String)> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, String)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap())
            .collect();
        entries.sort_by_key(std::fs::DirEntry::path);
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, base, out);
            } else {
                out.push((
                    path.strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    std::fs::read_to_string(&path).unwrap_or_default(),
                ));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out
}

/// A workspace over a backend that fails the `fail_at`th write.
pub(super) fn failing_ws(dir: &Path, fail_at: usize) -> Workspace<FailAtWrite> {
    Workspace::builder(FailAtWrite::nth(fail_at))
        .root(dir)
        .build()
}

pub(super) fn linked_tree(tag: &str) -> PathBuf {
    let dir = tempdir(tag);
    write(
        &dir,
        "index.md",
        "---\ntitle: Root\ncontents:\n- a.md\n- b.md\n---\nbody\n",
    );
    write(
        &dir,
        "a.md",
        "---\ntitle: A\npart_of: index.md\n---\nsee [[b]]\n",
    );
    write(
        &dir,
        "b.md",
        "---\ntitle: B\npart_of: index.md\nlinks:\n- a.md\n---\nbody\n",
    );
    dir
}
