//! Fixtures the history test modules share.
//!
//! Each verb's tests live in its own file, so the workspace-building
//! helpers they all need cannot hang off any one of them.

use std::path::{Path, PathBuf};

use prov_graph::exec::block_on;
use prov_history::*;

// Re-exported so each verb's file can pull the whole fixture surface — helpers
// and the concrete workspace types they hand back — from one glob.
pub(super) use crate::fs_faults::CountingFs;
pub(super) use crate::identity::{Id, Minter};
pub(super) use crate::workspace::Workspace;
pub(super) use prov_graph::fs::StdFs;
pub(super) use prov_graph::index::FileIndex;

pub(super) fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("prov-history-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub(super) fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

pub(super) fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).unwrap()
}

pub(super) fn ws(dir: &Path) -> Workspace<StdFs, Minter, FileIndex> {
    Workspace::builder(StdFs)
        .root(dir)
        .identity(Minter::lazy(42))
        .index(FileIndex::new(fig::Format::Yaml))
        // The axis the store's own verbs never consult, and `check` does: with it
        // off, a store the root has stopped declaring is not a finding. Every
        // fixture here has a store on purpose, so `manual` is the honest default.
        .history(crate::config::History::Manual)
        .build()
}

/// [`ws`] with the `history` axis off — for the one distinction that turns on it:
/// a leftover store nobody declared is only a defect in a workspace that says it
/// wants one.
pub(super) fn ws_history_off(dir: &Path) -> Workspace<StdFs, Minter, FileIndex> {
    Workspace::builder(StdFs)
        .root(dir)
        .identity(Minter::lazy(42))
        .index(FileIndex::new(fig::Format::Yaml))
        .build()
}

/// [`ws`] with a declared metadata embedding. The store's *content* grammar comes
/// from the root document's own extension, so a caller wanting an HTML store
/// writes an `index.html` root rather than passing a format here.
pub(super) fn ws_authoring(
    dir: &Path,
    style: prov_graph::document::EmbedStyle,
    format: fig::Format,
) -> Workspace<StdFs, Minter, FileIndex> {
    Workspace::builder(StdFs)
        .root(dir)
        .identity(Minter::lazy(42))
        .index(FileIndex::new(format))
        .history(crate::config::History::Manual)
        .default_embed_format(format)
        .embed_style(style)
        .build()
}

/// [`ws`] over a [`CountingFs`], returning the counter alongside.
pub(super) fn ws_counting(dir: &Path) -> (Workspace<CountingFs, Minter, FileIndex>, CountingFs) {
    let fs = CountingFs::default();
    let ws = Workspace::builder(fs.clone())
        .root(dir)
        .identity(Minter::lazy(42))
        .index(FileIndex::new(fig::Format::Yaml))
        .history(crate::config::History::Manual)
        .build();
    (ws, fs)
}

/// A small workspace: a root, two notes, and an attachment (payload plus
/// sidecar) — so the capture set covers the shapes that actually matter.
pub(super) fn seed(tag: &str) -> PathBuf {
    let dir = tempdir(tag);
    write(
        &dir,
        "index.md",
        "---\ntitle: Home\ncontents:\n- notes/a.md\n- notes/photo.jpg.yaml\n---\nroot\n",
    );
    write(
        &dir,
        "notes/a.md",
        "---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n",
    );
    write(&dir, "notes/photo.jpg", "JPEGBYTES");
    write(
        &dir,
        "notes/photo.jpg.yaml",
        "title: Photo\npart_of: '../index.md'\ncontent: photo.jpg\n",
    );
    dir
}

pub(super) fn capture(dir: &Path, now: &str, label: Option<&str>) -> Captured {
    block_on(ws(dir).history_capture(Path::new("index.md"), now, label)).unwrap()
}

pub(super) fn event_ids(dir: &Path) -> Vec<String> {
    block_on(ws(dir).history_list(Path::new("index.md")))
        .unwrap()
        .into_iter()
        .map(|e| e.id)
        .collect()
}

pub(super) fn entry(path: &str, hash_of: &[u8]) -> FileEntry {
    FileEntry {
        path: PathBuf::from(path),
        id: None,
        hash: crate::fixity::digest(hash_of),
    }
}

/// Corrupt an event document the way a sync transport actually does: a
/// conflict lands **inside** the frontmatter fence rather than beside it —
/// `a_transport_conflict_copy_is_not_mistaken_for_an_event` covers the
/// filename shape; this is the one no filename check can catch. The result
/// still has an event-shaped filename, so [`shard_event_ids`] finds it, but
/// nothing in its content parses any more.
pub(super) fn tear(dir: &Path, rel: &str) {
    let text = read(dir, rel);
    let mangled = text.replacen("---\n", "---\n<<<<<<< ours\n=======\n>>>>>>> theirs\n", 1);
    write(dir, rel, &mangled);
}

pub(super) fn blob_of(bytes: &[u8]) -> PathBuf {
    blob_path(Path::new("history/index.md"), &crate::fixity::digest(bytes)).unwrap()
}

/// Capture with `notes/a.md` rewritten first, so each capture has something to
/// record — and so the untouched files keep sharing the blob they already
/// parked.
pub(super) fn capture_edited(dir: &Path, now: &str, label: &str, body: &str) -> String {
    write(
        dir,
        "notes/a.md",
        &format!("---\ntitle: A\npart_of: '../index.md'\n---\n{body}\n"),
    );
    match capture(dir, now, Some(label)) {
        Captured::Written { id, .. } => id,
        Captured::Unchanged { id } => panic!("expected a new event, got {id}"),
    }
}

/// `relink` (in `read`'s lineage tests), keeping the `history` pointer.
///
/// `relink` writes the root a workspace had *before* its store existed, which
/// is what the lineage tests want (a capture follows, and re-bootstraps the
/// pointer). A restore has no such capture behind it: strip the pointer and
/// the store is simply not there to restore from.
pub(super) fn relink_live(dir: &Path, contents: &[&str]) {
    let list = contents
        .iter()
        .map(|c| format!("- {c}\n"))
        .collect::<String>();
    write(
        dir,
        "index.md",
        &format!("---\ntitle: Home\nhistory: history/index.md\ncontents:\n{list}---\nroot\n"),
    );
}
