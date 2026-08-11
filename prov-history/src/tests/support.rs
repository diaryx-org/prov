//! Fixtures the history test modules share.
//!
//! Each verb's tests live in its own file, so the store-building helpers they
//! all need cannot hang off any one of them.

use std::path::{Path, PathBuf};

use prov_graph::exec::block_on;

use crate::*;

// Re-exported so each verb's file can pull the whole fixture surface — helpers
// and the concrete types they hand back — from one glob.
pub(super) use super::counting_fs::CountingFs;
pub(super) use super::host::TestHost;
pub(super) use prov_fixity::{FixityCache, digest};
pub(super) use prov_graph::fs::StdFs;

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

/// The store over a workspace at `dir`.
///
/// A [`HistoryStore`] owning its host rather than borrowing one, because that
/// is the shape a test wants: the read verbs need `&host` and the four writing
/// ones need `&mut host`, and a store that owns the host offers both without
/// the caller keeping two bindings alive.
pub(super) fn store(dir: &Path) -> HistoryStore<TestHost> {
    HistoryStore::new(TestHost::new(StdFs, dir))
}

/// [`store`] with the `history` axis off — for the one distinction that turns
/// on it: a leftover store nobody declared is only a defect in a workspace that
/// says it wants one.
pub(super) fn store_history_off(dir: &Path) -> HistoryStore<TestHost> {
    HistoryStore::new(TestHost::new(StdFs, dir).history_off())
}

/// [`store`] with a declared metadata embedding — see
/// [`TestHost::authoring`](super::host::TestHost::authoring).
pub(super) fn store_authoring(
    dir: &Path,
    style: prov_graph::document::EmbedStyle,
    format: fig::Format,
) -> HistoryStore<TestHost> {
    HistoryStore::new(TestHost::authoring(StdFs, dir, style, format))
}

/// [`store`] over a [`CountingFs`], returning the counter alongside.
pub(super) fn store_counting(dir: &Path) -> (HistoryStore<TestHost<CountingFs>>, CountingFs) {
    let fs = CountingFs::default();
    (HistoryStore::new(TestHost::new(fs.clone(), dir)), fs)
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
    block_on(store(dir).capture(Path::new("index.md"), now, label)).unwrap()
}

pub(super) fn event_ids(dir: &Path) -> Vec<String> {
    block_on(store(dir).list(Path::new("index.md")))
        .unwrap()
        .into_iter()
        .map(|e| e.id)
        .collect()
}

pub(super) fn findings(dir: &Path) -> Vec<HistoryIssue> {
    block_on(store(dir).findings(Path::new("index.md"))).unwrap()
}

pub(super) fn entry(path: &str, hash_of: &[u8]) -> FileEntry {
    FileEntry {
        path: PathBuf::from(path),
        id: None,
        hash: digest(hash_of),
    }
}

/// Corrupt an event document the way a sync transport actually does: a
/// conflict lands **inside** the frontmatter fence rather than beside it —
/// `a_transport_conflict_copy_is_not_mistaken_for_an_event` covers the
/// filename shape; this is the one no filename check can catch. The result
/// still has an event-shaped filename, so [`HistoryStore::shard_event_ids`]
/// finds it, but nothing in its content parses any more.
pub(super) fn tear(dir: &Path, rel: &str) {
    let text = read(dir, rel);
    let mangled = text.replacen("---\n", "---\n<<<<<<< ours\n=======\n>>>>>>> theirs\n", 1);
    write(dir, rel, &mangled);
}

pub(super) fn blob_of(bytes: &[u8]) -> PathBuf {
    blob_path(Path::new("history/index.md"), &digest(bytes)).unwrap()
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

/// Re-point the root at `contents`, so a rename is visible to the reachable
/// walk the capture set is taken from. The workspace it writes is the one that
/// existed *before* the store did, which is what the lineage tests want: a
/// capture follows, and re-bootstraps the pointer.
pub(super) fn relink(dir: &Path, contents: &[&str]) {
    let list = contents
        .iter()
        .map(|c| format!("- {c}\n"))
        .collect::<String>();
    write(
        dir,
        "index.md",
        &format!("---\ntitle: Home\ncontents:\n{list}---\nroot\n"),
    );
}

/// [`relink`], keeping the `history` pointer.
///
/// A restore has no capture behind it to re-bootstrap the pointer: strip it and
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
