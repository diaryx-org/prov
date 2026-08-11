//! What `prov` makes of what the history store reports.
//!
//! The store's own diagnostics are [`HistoryIssue`](prov_history::HistoryIssue)s
//! and are tested where they are produced, in `prov-history`. This file starts
//! one layer up, at the two things only this crate does with them: turn each
//! one into a [`Finding`] — with prose a reader has to be able to act on, in an
//! order that puts the disabling problem first — and attach the
//! [`Fix`](crate::Fix) that retires it.
//!
//! The other half is [`check`] itself: a history verb writes into a live
//! workspace, so "the store is valid" is not the same claim as "the workspace
//! is". Only [`check`] makes the second one, which is why each verb gets a
//! clean-afterwards test here rather than beside the verb.

use std::path::{Path, PathBuf};

use super::support::*;
use crate::validate::Finding;
use prov_graph::exec::block_on;
use prov_history::*;

// ---- every history verb leaves the whole workspace valid ----
//
// Not the store — `prov-history` asserts that for itself. These say the
// *workspace* survives: the pointer the capture authored is a link like any
// other, the shard indexes are documents `check` walks, and the tree a restore
// or prune rewrites has to come back reachable and orphan-free.

#[test]
fn a_capture_leaves_check_clean() {
    let dir = seed("capture-check");
    capture(&dir, "2026-07-31T09:15:22Z", Some("pre-sync"));
    assert_eq!(
        check(&dir),
        vec![],
        "a capture must leave the workspace valid"
    );

    // And again across a month boundary, which grows the shard tree: every new
    // index is a document the root now reaches.
    write(
        &dir,
        "notes/a.md",
        "---\ntitle: A\npart_of: '../index.md'\n---\nedited\n",
    );
    capture(&dir, "2026-08-01T09:00:00Z", None);
    assert!(dir.join("history/events/2026/08/index.md").exists());
    assert_eq!(check(&dir), vec![]);
}

#[test]
fn a_restore_leaves_check_clean() {
    let dir = seed("restore-check");
    let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
        panic!("the first capture must write an event");
    };
    // Damage of the shape a bad merge leaves, including the parent's child list
    // — the structural half a per-file undo would miss.
    write(&dir, "notes/a.md", "clobbered by a sync conflict");
    relink_live(&dir, &["notes/photo.jpg.yaml"]);

    let mut w = ws(&dir);
    let root = Path::new("index.md");
    let event = block_on(w.history_event(root, &id)).unwrap().unwrap();
    let plan = block_on(w.history_restore_plan(root, &event, &Scope::Whole, false)).unwrap();
    block_on(w.history_restore(root, &plan, false)).unwrap();

    assert!(read(&dir, "index.md").contains("notes/a.md"));
    assert_eq!(check(&dir), vec![]);
}

#[test]
fn a_prune_leaves_check_clean() {
    let dir = seed("prune-check");
    capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "july", "alpha");
    capture_edited(&dir, "2026-08-01T09:00:00.000000Z", "august", "beta");

    // Dropping July removes its shard index, so the year index the root reaches
    // must stop linking it — an unreachable or dangling index is exactly what
    // `check` exists to catch.
    let mut w = ws(&dir);
    let root = Path::new("index.md");
    let plan =
        block_on(w.history_prune_plan(root, &Retention::Before("2026-08-01".into()))).unwrap();
    block_on(w.history_prune(root, &plan)).unwrap();
    assert!(!dir.join("history/events/2026/07/index.md").exists());
    assert_eq!(check(&dir), vec![]);
}

#[test]
fn a_forget_leaves_check_clean() {
    let dir = seed("forget-check");
    capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
    std::fs::remove_file(dir.join("notes/a.md")).unwrap();
    relink_live(&dir, &["notes/photo.jpg.yaml"]);

    let mut w = ws(&dir);
    block_on(w.history_forget(
        Path::new("index.md"),
        &Subject::Path(PathBuf::from("notes/a.md")),
        "2026-08-01T12:00:00.000000Z",
        false,
    ))
    .unwrap();

    // The tombstone list is a new document beside the store index, and the
    // store index gained a link to it — both have to be reachable and valid.
    assert!(read(&dir, "history/index.md").contains("forgotten.yaml"));
    assert_eq!(
        check(&dir),
        vec![],
        "a recorded destruction is not a finding — a `check` that never came \
         back to clean would teach the user to stop reading it"
    );
}

// ---- HistoryIssue → Finding ----

#[test]
fn lost_bytes_are_reported_with_both_causes_a_reader_has_to_weigh() {
    let dir = seed("blob-missing");
    capture(&dir, "2026-07-31T09:00:00.000000Z", None);
    let payload = crate::fixity::digest(b"JPEGBYTES");
    std::fs::remove_file(dir.join(blob_of(b"JPEGBYTES"))).unwrap();

    let findings = check(&dir);
    assert_eq!(
        findings,
        vec![Finding::HistoryBlobMissing {
            store: PathBuf::from("history/index.md"),
            hash: payload,
            paths: vec![PathBuf::from("notes/photo.jpg")],
        }]
    );
    // Both causes have to be readable in the text — a store that syncs is in
    // this state routinely, and a finding that cries corruption at a
    // self-resolving state is one people learn to ignore.
    let text = findings[0].to_string();
    assert!(
        text.contains("has not arrived yet") && text.contains("gone"),
        "{text}"
    );
    assert!(text.contains("notes/photo.jpg"), "{text}");
}

#[test]
fn orphaned_bytes_name_the_verb_that_collects_them() {
    let dir = seed("blob-orphan");
    capture(&dir, "2026-07-31T09:00:00.000000Z", None);
    write(&dir, "history/blobs/ab/sync-conflict-20260731", "junk");

    let findings = check(&dir);
    assert_eq!(
        findings,
        vec![Finding::HistoryBlobOrphaned {
            store: PathBuf::from("history/index.md"),
            blobs: vec![PathBuf::from("history/blobs/ab/sync-conflict-20260731")],
        }]
    );
    assert!(
        findings[0].to_string().contains("history-prune"),
        "the report names the verb that collects them: {}",
        findings[0]
    );
}

#[test]
fn a_torn_event_is_reported_as_a_plain_unreadable_document() {
    // The store hands over `HistoryIssue::Unreadable`; what `check` must not do
    // is invent a history-specific finding for it. A document that will not
    // parse reads the same whether it is an event or a note.
    let dir = seed("check-torn");
    let first = capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
    let torn = event_path(Path::new("history/index.md"), &first, "md").unwrap();
    tear(&dir, torn.to_str().unwrap());

    let findings = check(&dir);
    assert!(
        findings
            .iter()
            .any(|f| matches!(f, Finding::Unreadable { doc, .. } if doc == &torn)),
        "missing the promised finding for {}: {findings:?}",
        torn.display()
    );
}

#[test]
fn an_unlinked_store_is_reported_first_and_its_fix_retires_it() {
    let dir = seed("check-unlinked");
    capture(&dir, "2026-07-31T09:00:00.000000Z", None);
    unlink_the_store(&dir);

    let findings = check(&dir);
    let unlinked = Finding::HistoryStoreUnlinked {
        root: PathBuf::from("index.md"),
        store: PathBuf::from("history/index.md"),
    };
    assert!(
        findings.contains(&unlinked),
        "a store nothing declares must not be silent: {findings:?}"
    );
    // Reported *first*: everything else about the store is about its contents,
    // and a reader has to know prov cannot see it from the root at all.
    assert_eq!(
        findings.iter().position(|f| f == &unlinked),
        Some(0),
        "{findings:?}"
    );
    let text = unlinked.to_string();
    assert!(
        text.contains("history/index.md") && text.contains("index.md"),
        "{text}"
    );

    // Metadata-only, and the pointer comes back spelled the way a bootstrap
    // capture would have spelled it.
    let fix = block_on(ws(&dir).suggest_fix(&unlinked)).unwrap().unwrap();
    assert_eq!(
        fix,
        crate::Fix::LinkHistoryStore {
            root: PathBuf::from("index.md"),
            store: PathBuf::from("history/index.md"),
        }
    );
    block_on(ws(&dir).apply_fix(&fix)).unwrap();
    assert!(read(&dir, "index.md").contains("history: history/index.md"));
    assert_eq!(
        check(&dir),
        vec![],
        "the fix has to actually retire the finding"
    );
}

/// With the axis off, a leftover `history/` is not a loss — the workspace said
/// it wants no store, and a finding would be prov objecting to a directory the
/// user is entitled to leave lying around. Declaring `manual` is what makes a
/// missing pointer a defect rather than a preference, and the axis is a
/// workspace setting, so this distinction only exists at this layer.
#[test]
fn an_undeclared_store_is_not_a_finding_when_history_is_off() {
    let dir = seed("check-unlinked-off");
    capture(&dir, "2026-07-31T09:00:00.000000Z", None);
    unlink_the_store(&dir);

    assert!(
        !block_on(ws_history_off(&dir).check(Path::new("index.md")))
            .unwrap()
            .iter()
            .any(|f| matches!(f, Finding::HistoryStoreUnlinked { .. }))
    );
}

#[test]
fn a_stale_shard_index_is_rebuilt_by_its_fix() {
    // The one mutable file in the store is the shard index, so it is the one a
    // sync transport can mangle — last-writer-wins on the only file two devices
    // both rewrote, staged here by putting back the text the *first* capture
    // left. That must be a finding with a mechanical fix, never data loss,
    // which is exactly what "the index is a cache" buys.
    let dir = seed("check-index-stale");
    capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
    let before = read(&dir, "history/events/2026/07/index.md");
    capture_edited(&dir, "2026-07-31T10:00:00.000000Z", "two", "beta");
    write(&dir, "history/events/2026/07/index.md", &before);

    let findings = check(&dir);
    let stale: Vec<_> = findings
        .iter()
        .filter(|f| matches!(f, Finding::HistoryIndexStale { .. }))
        .collect();
    assert_eq!(stale.len(), 1, "expected one stale shard: {findings:?}");

    let mut w = ws(&dir);
    let fix = block_on(w.suggest_fix(stale[0])).unwrap().expect("a fix");
    block_on(w.apply_fix(&fix)).unwrap();

    assert_eq!(
        check(&dir),
        vec![],
        "the rebuild should have settled the index"
    );
}

/// Strip the root's `history` line and everything else about the store goes
/// quiet: descent into it is through that pointer, so the walk never enters the
/// subtree and reports nothing about it — not even an orphan. This is the
/// finding that exists because the silence is total.
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
