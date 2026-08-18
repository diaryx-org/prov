use std::path::{Path, PathBuf};

use super::support::*;
use crate::*;
use prov_graph::error::Result;
use prov_graph::exec::block_on;
use prov_store::index::IndexStore;

fn forget(dir: &Path, subject: &Subject, now: &str, force: bool) -> Result<Forgotten> {
    block_on(store(dir).forget(Path::new("index.md"), subject, now, force))
}

#[test]
fn a_forget_destroys_only_the_bytes_nothing_else_names() {
    let dir = seed("forget-basic");
    // Two documents with byte-identical content, so one hash is shared — the
    // case content addressing makes possible and a naive "delete every hash
    // this path ever had" would get catastrophically wrong.
    let shared = "---\ntitle: Same\npart_of: '../index.md'\n---\ntwin\n";
    write(&dir, "notes/twin.md", shared);
    write(&dir, "notes/other.md", shared);
    relink_live(
        &dir,
        &[
            "notes/a.md",
            "notes/twin.md",
            "notes/other.md",
            "notes/photo.jpg.yaml",
        ],
    );
    capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
    // A second version of the doomed document, so forget has to reach every
    // hash it ever had rather than only the newest.
    write(
        &dir,
        "notes/twin.md",
        "---\ntitle: Same\npart_of: '../index.md'\n---\nrevised\n",
    );
    capture_edited(&dir, "2026-07-31T10:00:00.000000Z", "two", "beta");

    // Out of the workspace first: forget refuses a live document, and the
    // point here is what it destroys, not that guard.
    std::fs::remove_file(dir.join("notes/twin.md")).unwrap();
    relink_live(
        &dir,
        &["notes/a.md", "notes/other.md", "notes/photo.jpg.yaml"],
    );

    let revised = blob_of(b"---\ntitle: Same\npart_of: '../index.md'\n---\nrevised\n");
    assert!(dir.join(&revised).exists());
    let out = forget(
        &dir,
        &Subject::Path(PathBuf::from("notes/twin.md")),
        "2026-08-01T12:00:00.000000Z",
        false,
    )
    .unwrap();

    assert_eq!(out.blobs, vec![revised.clone()]);
    assert!(!dir.join(&revised).exists(), "the unique version must go");
    assert_eq!(
        out.shared.len(),
        1,
        "the version it shares with notes/other.md survives, and is reported"
    );
    assert!(
        dir.join(blob_of(shared.as_bytes())).exists(),
        "forgetting one document must not reach into another's history"
    );
    assert!(out.bytes > 0);

    // The record of *what was captured* survives the destruction of the bytes.
    // That is the bargain, and it has to be visible in the store.
    let events = block_on(store(&dir).list(Path::new("index.md"))).unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.files.iter().any(|f| f.path == Path::new("notes/twin.md"))),
        "events are immutable: the manifest still names it"
    );

    // Tombstoned, reachable, and clean — the record must not itself be an
    // orphan, and a deliberate destruction must not leave the store reporting.
    let tombstone = read(&dir, "history/forgotten.yaml");
    assert!(tombstone.contains("notes/twin.md") && tombstone.contains("2026-08-01T12:00:00"));
    assert!(read(&dir, "history/index.md").contains("forgotten.yaml"));
    assert_eq!(findings(&dir), vec![]);
}

#[test]
fn a_tombstoned_hash_is_accounted_for_where_a_lost_one_is_not() {
    let dir = seed("forget-findings");
    capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
    std::fs::remove_file(dir.join("notes/a.md")).unwrap();
    relink_live(&dir, &["notes/photo.jpg.yaml"]);

    forget(
        &dir,
        &Subject::Path(PathBuf::from("notes/a.md")),
        "2026-08-01T12:00:00.000000Z",
        false,
    )
    .unwrap();
    assert_eq!(
        findings(&dir),
        vec![],
        "a recorded destruction is not a finding — a `check` that never came \
         back to clean would teach the user to stop reading it"
    );
    assert_eq!(
        block_on(store(&dir).forgotten(Path::new("index.md")))
            .unwrap()
            .len(),
        1
    );

    // …and the suppression is precise, not blanket: bytes that went missing
    // without a record still say so.
    std::fs::remove_file(dir.join(blob_of(b"JPEGBYTES"))).unwrap();
    let issues = findings(&dir);
    assert!(
        matches!(issues.as_slice(), [HistoryIssue::BlobMissing { paths, .. }]
            if paths == &[PathBuf::from("notes/photo.jpg")]),
        "{issues:?}"
    );
}

#[test]
fn a_forget_refuses_a_document_the_next_capture_would_park_again() {
    let dir = seed("forget-live");
    capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");

    let subject = Subject::Path(PathBuf::from("notes/a.md"));
    let err = forget(&dir, &subject, "2026-08-01T12:00:00.000000Z", false).unwrap_err();
    assert!(
        err.to_string().contains("notes/a.md")
            && err.to_string().contains("still in the workspace"),
        "the refusal has to name the document and say why: {err}"
    );
    assert!(
        dir.join(blob_of(
            b"---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n"
        ))
        .exists(),
        "a refused forget destroys nothing"
    );

    // Forced, for the deliberate "purge the history, keep the file" case.
    let out = forget(&dir, &subject, "2026-08-01T12:00:00.000000Z", true).unwrap();
    assert!(!out.is_empty());
}

#[test]
fn forgetting_by_id_reaches_the_versions_a_path_key_would_miss() {
    let dir = seed("forget-id");
    let mut w = store(&dir);
    let id = Id("b7k2m".into());
    w.host_mut()
        .index_mut()
        .register(&id, Path::new("notes/a.md"));
    block_on(w.capture(
        Path::new("index.md"),
        "2026-07-31T09:00:00.000000Z",
        CaptureNote::default(),
    ))
    .unwrap();

    // The move: the same document, a second path, and a hash a path-keyed
    // forget would leave behind.
    std::fs::rename(dir.join("notes/a.md"), dir.join("notes/b.md")).unwrap();
    write(
        &dir,
        "notes/b.md",
        "---\ntitle: A\npart_of: '../index.md'\n---\nrevised\n",
    );
    relink_live(&dir, &["notes/b.md", "notes/photo.jpg.yaml"]);
    w.host_mut()
        .index_mut()
        .set_path(&id, Path::new("notes/b.md"));
    block_on(w.capture(
        Path::new("index.md"),
        "2026-07-31T10:00:00.000000Z",
        CaptureNote::default(),
    ))
    .unwrap();

    // Out of the workspace, so the guard is not what is under test.
    std::fs::remove_file(dir.join("notes/b.md")).unwrap();
    relink_live(&dir, &["notes/photo.jpg.yaml"]);
    w.host_mut().index_mut().unregister(&id);

    let out = block_on(w.forget(
        Path::new("index.md"),
        &Subject::Id(id),
        "2026-08-01T12:00:00.000000Z",
        false,
    ))
    .unwrap();
    assert_eq!(out.hashes.len(), 2, "both versions, across the rename");
    assert!(
        !dir.join(blob_of(
            b"---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n"
        ))
        .exists()
    );
    assert!(
        !dir.join(blob_of(
            b"---\ntitle: A\npart_of: '../index.md'\n---\nrevised\n"
        ))
        .exists()
    );
}

#[test]
fn a_forget_refuses_while_any_event_is_unreadable() {
    // Same bug, `history-forget`'s side: `others` built only from the events
    // that parsed can miss a hash the torn event shared with the subject,
    // so a hash that should have survived (named elsewhere) reads as
    // belonging only to the subject and gets destroyed.
    let dir = seed("forget-torn");
    let first = capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
    capture_edited(&dir, "2026-07-31T10:00:00.000000Z", "two", "beta");
    let torn = event_path(Path::new("history/index.md"), &first, "md").unwrap();
    tear(&dir, torn.to_str().unwrap());

    std::fs::remove_file(dir.join("notes/a.md")).unwrap();
    relink_live(&dir, &["notes/photo.jpg.yaml"]);

    let err = forget(
        &dir,
        &Subject::Path(PathBuf::from("notes/a.md")),
        "2026-08-01T12:00:00.000000Z",
        false,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains(torn.to_str().unwrap()),
        "the refusal has to name the file that could not be read: {err}"
    );
    assert!(
        dir.join(blob_of(
            b"---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n"
        ))
        .exists(),
        "a refused forget destroys nothing"
    );
}
