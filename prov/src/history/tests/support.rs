//! Fixtures the composition tests share.

use std::path::{Path, PathBuf};

pub(super) use prov_graph::exec::block_on;

pub(super) use crate::workspace::Workspace;
pub(super) use prov_graph::fs::StdFs;
pub(super) use prov_store::index::FileIndex;

pub(super) fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("prov-history-ws-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub(super) fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

pub(super) fn ws(dir: &Path) -> Workspace<StdFs, crate::identity::Minter, FileIndex> {
    Workspace::builder(StdFs)
        .root(dir)
        .identity(crate::identity::Minter::lazy(42))
        .index(FileIndex::new(fig::Format::Yaml))
        .build()
}

/// A small workspace: a root, a linked note, and a loose one nothing reaches.
pub(super) fn seed(tag: &str) -> PathBuf {
    let dir = tempdir(tag);
    write(
        &dir,
        "index.md",
        "---\ntitle: Home\ncontents:\n- notes/a.md\n---\nroot\n",
    );
    write(
        &dir,
        "notes/a.md",
        "---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n",
    );
    write(&dir, "notes/loose.md", "---\ntitle: Loose\n---\nunlinked\n");
    dir
}
