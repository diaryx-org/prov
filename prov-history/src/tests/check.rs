use std::path::{Path, PathBuf};

use super::support::*;
use crate::*;
use prov_graph::exec::block_on;

/// What `findings` is *for*, stated once: the store validates itself — every
/// index against the directory it describes, and the blob mark-and-sweep in
/// both directions. `prov` maps each of these onto a `Finding`, writes its
/// prose, and attaches an autofix; that mapping is `prov`'s to test, and the
/// tests here stop at the [`HistoryIssue`] this crate hands over.
#[test]
fn lost_bytes_are_reported_once_per_hash_however_many_events_named_them() {
    let dir = seed("blob-missing");
    capture(&dir, "2026-07-31T09:00:00.000000Z", None);
    // A second capture that changes one file: everything else keeps the blob
    // the first capture parked, so one blob is now named by two manifests.
    write(
        &dir,
        "notes/a.md",
        "---\ntitle: A\npart_of: '../index.md'\n---\nrevised\n",
    );
    capture(&dir, "2026-07-31T10:00:00.000000Z", None);

    let payload = digest(b"JPEGBYTES");
    std::fs::remove_file(dir.join(blob_path(Path::new("history/index.md"), &payload).unwrap()))
        .unwrap();

    assert_eq!(
        findings(&dir),
        vec![HistoryIssue::BlobMissing {
            store: PathBuf::from("history/index.md"),
            hash: payload.clone(),
            paths: vec![PathBuf::from("notes/photo.jpg")],
        }],
        "one lost blob is one thing to put back, not one report per event"
    );

    // Deleting the blob left nothing behind, so there is no orphan to pair
    // with it: the two issues answer opposite questions.
    assert!(
        !findings(&dir)
            .iter()
            .any(|f| matches!(f, HistoryIssue::BlobOrphaned { .. }))
    );
}

#[test]
fn a_manifest_row_prov_could_never_have_parked_reports_rather_than_failing() {
    // A foreign event has to stay legible: the sweep reads what arrived from
    // another device, and a digest in a scheme this build does not know is a
    // report, not a parse error that takes the whole run down.
    let dir = seed("blob-foreign");
    let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:00:00.000000Z", None) else {
        panic!("the first capture must write an event");
    };
    let event = event_path(Path::new("history/index.md"), &id, "md").unwrap();
    let text = read(&dir, event.to_str().unwrap());
    write(
        &dir,
        event.to_str().unwrap(),
        &text.replace(&digest(b"JPEGBYTES"), "blake3:beefbeefbeefbeef"),
    );

    let issues = findings(&dir);
    let missing: Vec<&HistoryIssue> = issues
        .iter()
        .filter(|f| matches!(f, HistoryIssue::BlobMissing { .. }))
        .collect();
    assert_eq!(missing.len(), 1, "{issues:?}");
    assert!(
        matches!(missing[0], HistoryIssue::BlobMissing { hash, .. } if hash.starts_with("blake3:")),
        "{:?}",
        missing[0]
    );
    // …and the blob it no longer names is now unreferenced, which is the
    // other half of the same sweep.
    assert!(
        issues
            .iter()
            .any(|f| matches!(f, HistoryIssue::BlobOrphaned { .. })),
        "{issues:?}"
    );
}

#[test]
fn bytes_no_manifest_claims_are_reported_as_orphaned() {
    let dir = seed("blob-orphan");
    capture(&dir, "2026-07-31T09:00:00.000000Z", None);
    assert_eq!(
        findings(&dir),
        vec![],
        "a fresh capture claims every blob it parked"
    );

    // Cruft of the two shapes a transport actually leaves: a conflict copy
    // beside a real blob, and a stray at the top of the store. Neither could
    // ever match a hash, which is the point — this is not a digest check.
    write(&dir, "history/blobs/ab/sync-conflict-20260731", "junk");
    write(&dir, "history/blobs/stray.txt", "junk");
    // A hidden file is transport bookkeeping, not cruft prov should name.
    write(&dir, "history/blobs/.DS_Store", "junk");

    assert_eq!(
        findings(&dir),
        vec![HistoryIssue::BlobOrphaned {
            store: PathBuf::from("history/index.md"),
            blobs: vec![
                PathBuf::from("history/blobs/ab/sync-conflict-20260731"),
                PathBuf::from("history/blobs/stray.txt"),
            ],
        }],
        "one sweep, one issue, sorted — and the dotfile left alone"
    );
}

#[test]
fn an_unreadable_event_is_reported_and_its_blobs_are_never_swept() {
    // The promise docs/history-format.md §7 makes and the codebase did not
    // keep: an event document that fails to parse is a plain `Unreadable`.
    // And the other half of the bug: while it is unreadable, its blobs must
    // not be reported orphaned — that issue's whole point is that `prune` may
    // collect what it names, so a false orphan here is a diagnostic
    // recommending the destructive verb `prune` itself refuses to run.
    let dir = seed("check-torn");
    let first = capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
    let torn = event_path(Path::new("history/index.md"), &first, "md").unwrap();
    tear(&dir, torn.to_str().unwrap());

    let issues = findings(&dir);
    assert!(
        issues
            .iter()
            .any(|f| matches!(f, HistoryIssue::Unreadable { doc, .. } if doc == &torn)),
        "missing the promised issue for {}: {issues:?}",
        torn.display()
    );
    assert!(
        !issues
            .iter()
            .any(|f| matches!(f, HistoryIssue::BlobOrphaned { .. })),
        "a torn event's blobs must not be reported as orphans: {issues:?}"
    );

    // Reading the diagnostics must not be what destroys the bytes: the blob
    // only this (now unreadable) event named is still exactly where it was.
    assert!(
        dir.join(blob_of(
            b"---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n"
        ))
        .exists()
    );
}

/// Strip the root's `history` line and everything else about the store goes
/// quiet: descent into it is through that pointer, so the walk never enters
/// the subtree and reports nothing about it — not even an orphan. This is the
/// issue that exists because the silence is total.
fn unlink_the_store(dir: &Path) {
    let root = read(dir, "index.md");
    write(
        dir,
        "index.md",
        &root
            .lines()
            .filter(|l| !l.starts_with("history:"))
            .map(|l| format!("{l}\n"))
            .collect::<String>(),
    );
}

#[test]
fn a_store_the_root_stopped_declaring_is_reported_first_and_repairable() {
    let dir = seed("check-unlinked");
    capture(&dir, "2026-07-31T09:00:00.000000Z", None);
    unlink_the_store(&dir);

    let issues = findings(&dir);
    let unlinked = HistoryIssue::StoreUnlinked {
        root: PathBuf::from("index.md"),
        store: PathBuf::from("history/index.md"),
    };
    assert!(
        issues.contains(&unlinked),
        "a store nothing declares must not be silent: {issues:?}"
    );
    // Reported *first*: everything else about the store is about its contents,
    // and a reader has to know prov cannot see it from the root at all.
    assert_eq!(
        issues.iter().position(|f| f == &unlinked),
        Some(0),
        "{issues:?}"
    );

    // The repair the issue points at is metadata-only, and spells the pointer
    // the way a bootstrap capture would have spelled it. (`prov` runs the
    // other half: that its `Fix` retires the corresponding `Finding`.)
    let repaired =
        block_on(store(&dir).pointer_text(Path::new("index.md"), Path::new("history/index.md")))
            .unwrap();
    assert!(
        repaired.contains("history: /history/index.md"),
        "{repaired}"
    );
    write(&dir, "index.md", &repaired);
    assert!(
        !findings(&dir)
            .iter()
            .any(|f| matches!(f, HistoryIssue::StoreUnlinked { .. })),
        "the repair has to actually retire the issue"
    );
}

#[test]
fn the_repair_pointer_respects_the_hosts_path_style() {
    // The default host authors root-absolute, as `prov`'s own workspace
    // default does (the case above). A host configured for `../`-relative
    // links must get that shape out of the very same repair — the pointer
    // defers to the host's style rather than assuming one.
    let dir = seed("check-unlinked-relative");
    capture(&dir, "2026-07-31T09:00:00.000000Z", None);
    unlink_the_store(&dir);

    let repaired = block_on(
        store_with_link_style(&dir, prov_graph::link::LinkStyle::MarkdownRelative)
            .pointer_text(Path::new("index.md"), Path::new("history/index.md")),
    )
    .unwrap();
    assert!(repaired.contains("history: history/index.md"), "{repaired}");
}

/// With the axis off, a leftover `history/` is not a loss — the workspace said
/// it wants no store, and an issue would be prov objecting to a directory the
/// user is entitled to leave lying around. Declaring `manual` is what makes a
/// missing pointer a defect rather than a preference.
#[test]
fn an_undeclared_store_is_not_an_issue_when_history_is_off() {
    let dir = seed("check-unlinked-off");
    capture(&dir, "2026-07-31T09:00:00.000000Z", None);
    unlink_the_store(&dir);

    assert!(
        !block_on(store_history_off(&dir).findings(Path::new("index.md")))
            .unwrap()
            .iter()
            .any(|f| matches!(f, HistoryIssue::StoreUnlinked { .. }))
    );
}
