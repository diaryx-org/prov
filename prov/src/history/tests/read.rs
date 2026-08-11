use std::path::{Path, PathBuf};

use super::support::*;
use prov_graph::exec::block_on;
use prov_graph::index::IndexStore;
use prov_history::*;

/// The summary's whole contract: the same answer `history_list` gives, for
/// the price of a listing. A store with no events at all is the boundary
/// case a cadence check meets first, on the vault where history was just
/// switched on.
#[test]
fn a_summary_names_the_event_history_list_would_have_named() {
    let dir = seed("summary-agrees");

    // Before any capture: no store, and nothing to be newest.
    let empty = block_on(ws(&dir).history_summary(Path::new("index.md"))).unwrap();
    assert_eq!(empty, Summary::default());
    assert!(!empty.store_exists);

    capture_edited(&dir, "2026-07-29T09:15:22.000000Z", "one", "alpha");
    capture_edited(&dir, "2026-08-02T11:04:07.000000Z", "two", "beta");
    let newest = capture_edited(&dir, "2026-08-02T11:59:00.000000Z", "three", "gamma");

    let summary = block_on(ws(&dir).history_summary(Path::new("index.md"))).unwrap();
    let listed = block_on(ws(&dir).history_list(Path::new("index.md"))).unwrap();
    let latest = summary
        .latest
        .expect("a store with three events has a newest");

    assert!(summary.store_exists);
    assert_eq!(summary.events, 3);
    assert_eq!(latest.id, newest);
    assert_eq!(latest.id, listed.last().unwrap().id);
    assert_eq!(latest.created, listed.last().unwrap().created);
    // The shard tree grew a second month, and the probe crossed it.
    assert_eq!(listed.len(), 3);
}

/// The case a filename cannot settle, and the reason the probe reads a
/// document at all: two captures inside one minute stamp identically, so the
/// answer is in their `created` — at two different precisions, which is
/// ordinary in a store that outlives a version of prov.
///
/// Note what a raw string comparison does to this pair: `.` sorts before `Z`,
/// so `…22.000001Z` compares *less* than `…22Z` and the older event wins.
/// Only [`comparable`]'s normalization gets it right, which is exactly why
/// this probe defers to it rather than sorting stems.
#[test]
fn a_summary_settles_a_minute_two_captures_share() {
    let dir = seed("summary-same-minute");
    let older = capture_edited(&dir, "2026-07-31T09:15:22Z", "second-precision", "alpha");
    let newer = capture_edited(&dir, "2026-07-31T09:15:22.000001Z", "microseconds", "beta");

    assert_eq!(
        id_stamp_of(&older),
        id_stamp_of(&newer),
        "the fixture is pointless unless both ids stamp the same minute"
    );
    assert!(
        "2026-07-31T09:15:22.000001Z" < "2026-07-31T09:15:22Z",
        "and pointless unless a raw comparison would get it backwards"
    );

    let latest = block_on(ws(&dir).history_summary(Path::new("index.md")))
        .unwrap()
        .latest
        .expect("two events have a newest");
    let listed = block_on(ws(&dir).history_list(Path::new("index.md"))).unwrap();

    assert_eq!(latest.id, newer);
    assert_eq!(latest.id, listed.last().unwrap().id);
}

/// A torn newest event must not blank the answer. The slot still counts — a
/// file that cannot be parsed is still evidence a capture happened — but the
/// search falls to the newest event that *can* be read, because a cadence
/// check that reports "no history" would capture again immediately and pile a
/// second event on top of the damage.
#[test]
fn a_summary_counts_a_torn_slot_and_looks_past_it_for_the_newest() {
    let dir = seed("summary-torn");
    let readable = capture_edited(&dir, "2026-07-31T09:15:22.000000Z", "intact", "alpha");
    let torn = capture_edited(&dir, "2026-08-01T10:00:00.000000Z", "torn", "beta");
    tear(&dir, &format!("history/events/2026/08/{torn}.md"));

    let summary = block_on(ws(&dir).history_summary(Path::new("index.md"))).unwrap();
    let latest = summary.latest.expect("the intact event is still there");

    assert_eq!(
        summary.events, 2,
        "the torn file is a slot: something captured, even if its bytes are now unreadable"
    );
    assert_eq!(latest.id, readable);
    assert_eq!(
        latest.id,
        block_on(ws(&dir).history_list(Path::new("index.md")))
            .unwrap()
            .last()
            .unwrap()
            .id,
        "`history_list` skips the torn document too, so the two still agree"
    );
}

/// Size is the number a settings screen shows, and it is deliberately not in
/// the summary — one `metadata` call per file is the per-file cost the
/// summary exists to avoid.
#[test]
fn store_bytes_totals_the_store_and_answers_zero_when_there_is_none() {
    let dir = seed("summary-bytes");
    assert_eq!(
        block_on(ws(&dir).history_store_bytes(Path::new("index.md"))).unwrap(),
        0,
        "no store is zero bytes, not an error"
    );

    capture_edited(&dir, "2026-07-31T09:15:22.000000Z", "one", "alpha");
    let first = block_on(ws(&dir).history_store_bytes(Path::new("index.md"))).unwrap();
    assert!(first > 0);

    // A second capture parks the changed document's new bytes and writes
    // another event, so the store grows — while the untouched files go on
    // sharing the blobs they already parked.
    capture_edited(&dir, "2026-08-01T10:00:00.000000Z", "two", "beta");
    assert!(
        block_on(ws(&dir).history_store_bytes(Path::new("index.md"))).unwrap() > first,
        "a second event and its blobs are more bytes than one"
    );
}

/// A root that has stopped declaring its store must not take the store with
/// it. The pointer is one line in one mutable file — the single most likely
/// thing for a transport to mangle — and it is the *only* declared way in.
#[test]
fn a_store_at_the_conventional_path_is_read_with_no_pointer_declaring_it() {
    let dir = seed("read-unlinked");
    capture(&dir, "2026-07-31T09:15:22.000000Z", Some("pre-sync"));
    let before = event_ids(&dir);
    assert_eq!(before.len(), 1);

    // Exactly the damage: the `history` line, gone, everything else intact.
    let root = read(&dir, "index.md");
    write(
        &dir,
        "index.md",
        &root
            .lines()
            .filter(|l| !l.starts_with("history:"))
            .map(|l| format!("{l}\n"))
            .collect::<String>(),
    );
    assert!(!read(&dir, "index.md").contains("history:"));

    // Read verbs carry on. Recovery is never gated behind repairing the thing
    // that broke — least of all on the machine that just suffered the damage.
    assert_eq!(
        event_ids(&dir),
        before,
        "an undeclared store is still a store"
    );
    let (store, found) =
        block_on(ws(&dir).history_store().store_index(Path::new("index.md"))).unwrap();
    assert_eq!(found, StoreLocation::Conventional);
    assert_eq!(store, PathBuf::from("history/index.md"));
    // And the event is restorable, which is the whole point of still finding it.
    assert!(
        block_on(ws(&dir).history_event(Path::new("index.md"), &before[0]))
            .unwrap()
            .is_some()
    );
}

/// Only the conventional path, never a search: a store the root declared
/// somewhere unusual and then stopped declaring is not recoverable by
/// guessing, and sweeping the tree for anything store-shaped is how a backup
/// copy gets adopted as the live one.
#[test]
fn discovery_probes_the_conventional_path_and_nowhere_else() {
    let dir = seed("read-unconventional");
    capture(&dir, "2026-07-31T09:15:22.000000Z", None);
    std::fs::rename(dir.join("history"), dir.join("archive")).unwrap();
    write(
        &dir,
        "index.md",
        "---\ntitle: Home\ncontents:\n- notes/a.md\n- notes/photo.jpg.yaml\n---\nroot\n",
    );

    let (_, found) = block_on(ws(&dir).history_store().store_index(Path::new("index.md"))).unwrap();
    assert_eq!(
        found,
        StoreLocation::Absent,
        "a store at an undeclared, unconventional path is not found by guessing"
    );
    assert!(event_ids(&dir).is_empty());
}

#[test]
fn an_event_resolves_by_id_with_every_index_destroyed() {
    let dir = seed("show-resolve");
    let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", Some("pre-sync"))
    else {
        panic!("the first capture must write an event");
    };
    // The indexes are a cache. Burn all three; the id still resolves, because
    // its path is a pure function of it.
    for index in [
        "history/index.md",
        "history/events/2026/index.md",
        "history/events/2026/07/index.md",
    ] {
        std::fs::remove_file(dir.join(index)).unwrap();
    }
    let event = block_on(ws(&dir).history_event(Path::new("index.md"), &id))
        .unwrap()
        .expect("the event must resolve without any index");
    assert_eq!(event.id, id);
    assert_eq!(event.label.as_deref(), Some("pre-sync"));
    assert_eq!(event.files.len(), 4);

    // An id that names nothing is absence, not an error; a string that is not
    // an event id at all is an error.
    assert!(
        block_on(ws(&dir).history_event(Path::new("index.md"), "2026-07-31-0000-deadbeef"))
            .unwrap()
            .is_none()
    );
    assert!(block_on(ws(&dir).history_event(Path::new("index.md"), "yesterday")).is_err());
}

#[test]
fn missing_blobs_name_the_paths_a_restore_could_not_recover() {
    let dir = seed("show-blobs");
    let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
        panic!("the first capture must write an event");
    };
    let event = block_on(ws(&dir).history_event(Path::new("index.md"), &id))
        .unwrap()
        .unwrap();
    assert!(
        block_on(ws(&dir).history_missing_blobs(Path::new("index.md"), &event))
            .unwrap()
            .is_empty(),
        "a capture parks every file's bytes"
    );

    // The half-synced case: the event document arrived, one blob did not.
    let payload = crate::fixity::digest(b"JPEGBYTES");
    let blob = blob_path(Path::new("history/index.md"), &payload).unwrap();
    std::fs::remove_file(dir.join(&blob)).unwrap();
    let missing = block_on(ws(&dir).history_missing_blobs(Path::new("index.md"), &event)).unwrap();
    assert_eq!(
        missing.into_iter().collect::<Vec<_>>(),
        vec![PathBuf::from("notes/photo.jpg")],
        "only the file whose bytes are gone should be reported"
    );

    // A row prov could never have parked reports as missing rather than
    // failing the read — a foreign event must stay legible.
    let foreign = Event {
        files: vec![FileEntry {
            path: PathBuf::from("notes/a.md"),
            id: None,
            hash: "blake3:beef".into(),
        }],
        ..event
    };
    assert_eq!(
        block_on(ws(&dir).history_missing_blobs(Path::new("index.md"), &foreign))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn read_only_verbs_keep_degrading_gracefully_around_an_unreadable_event() {
    // §7's flip side, restated as a test: the destructive verbs and `check`
    // must refuse or report, but `history-list` (and anything built on it)
    // has always been allowed to skip what it cannot read — that is
    // graceful degradation, not the destruction this fix guards against.
    let dir = seed("list-torn");
    let first = capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
    let second = capture_edited(&dir, "2026-07-31T10:00:00.000000Z", "two", "beta");
    let torn = event_path(Path::new("history/index.md"), &first, "md").unwrap();
    tear(&dir, torn.to_str().unwrap());

    let events = block_on(ws(&dir).history_list(Path::new("index.md"))).unwrap();
    assert_eq!(
        events.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        vec![second.as_str()],
        "a read still answers with whatever it could parse"
    );
}

/// Re-point the root at `contents`, so a rename is visible to the reachable
/// walk the capture set is taken from.
fn relink(dir: &Path, contents: &[&str]) {
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

#[test]
fn a_lineage_follows_an_id_through_a_rename_no_path_key_could() {
    let dir = seed("log-rename");
    let mut w = ws(&dir);
    let id = Id("b7k2m".into());
    w.index_mut().register(&id, Path::new("notes/a.md"));
    let take = |w: &mut Workspace<StdFs, Minter, FileIndex>, now: &str| {
        block_on(w.history_capture(Path::new("index.md"), now, None)).unwrap()
    };
    take(&mut w, "2026-07-31T09:00:00Z");

    // The move: same bytes, new path. A path-keyed store shows two unrelated
    // lineages here; the id column shows one document that moved.
    std::fs::rename(dir.join("notes/a.md"), dir.join("notes/b.md")).unwrap();
    relink(&dir, &["notes/b.md", "notes/photo.jpg.yaml"]);
    w.index_mut().set_path(&id, Path::new("notes/b.md"));
    take(&mut w, "2026-07-31T10:00:00Z");

    // An edit at the new path.
    write(
        &dir,
        "notes/b.md",
        "---\ntitle: A\npart_of: '../index.md'\n---\nrevised\n",
    );
    take(&mut w, "2026-07-31T11:00:00Z");

    // …and a capture that changes nothing about this document, which must not
    // add a point to its lineage.
    write(&dir, "notes/photo.jpg", "OTHERBYTES");
    take(&mut w, "2026-07-31T12:00:00Z");

    let log = block_on(w.history_log(Path::new("index.md"), &Subject::Id(id.clone()))).unwrap();
    let paths: Vec<&Path> = log
        .iter()
        .map(|v| match &v.state {
            Presence::At { path, .. } => path.as_path(),
            Presence::Gone => Path::new("(gone)"),
        })
        .collect();
    assert_eq!(
        paths,
        vec![
            Path::new("notes/a.md"),
            Path::new("notes/b.md"),
            Path::new("notes/b.md")
        ],
        "the move must be a point in the lineage, and the untouched capture must not"
    );
    // Deduping on the hash alone would have swallowed the move: the bytes did
    // not change when the path did.
    let (Presence::At { hash: first, .. }, Presence::At { hash: second, .. }) =
        (&log[0].state, &log[1].state)
    else {
        panic!("both points should be present states");
    };
    assert_eq!(first, second, "a rename leaves the bytes identical");

    // The same document asked for by its old *path*: the lineage fragments at
    // the move, which is the nature of a path key. But the row it does find
    // still remembers the id — which is what lets the weaker query hand the
    // caller the stronger one instead of quietly under-reporting.
    let by_path = block_on(w.history_log(
        Path::new("index.md"),
        &Subject::Path(PathBuf::from("notes/a.md")),
    ))
    .unwrap();
    assert!(matches!(
        &by_path[0].state,
        Presence::At { id: Some(found), .. } if *found == id
    ));
    assert_eq!(
        by_path.last().unwrap().state,
        Presence::Gone,
        "a path-keyed lineage sees the move as the document disappearing"
    );
}

#[test]
fn a_lineage_records_a_deletion_and_a_return() {
    let dir = seed("log-gone");
    let mut w = ws(&dir);
    let id = Id("b7k2m".into());
    w.index_mut().register(&id, Path::new("notes/a.md"));
    let take = |w: &mut Workspace<StdFs, Minter, FileIndex>, now: &str| {
        block_on(w.history_capture(Path::new("index.md"), now, None)).unwrap()
    };
    take(&mut w, "2026-07-31T09:00:00Z");

    // Out of the reachable graph and off disk.
    std::fs::remove_file(dir.join("notes/a.md")).unwrap();
    relink(&dir, &["notes/photo.jpg.yaml"]);
    take(&mut w, "2026-07-31T10:00:00Z");

    // Back again — which is what a restore looks like from the lineage's side.
    write(
        &dir,
        "notes/a.md",
        "---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n",
    );
    relink(&dir, &["notes/a.md", "notes/photo.jpg.yaml"]);
    take(&mut w, "2026-07-31T11:00:00Z");

    let log = block_on(w.history_log(Path::new("index.md"), &Subject::Id(id))).unwrap();
    assert_eq!(log.len(), 3);
    assert!(matches!(log[0].state, Presence::At { .. }));
    // Omission *is* deletion: there is no removal list to have consulted.
    assert_eq!(log[1].state, Presence::Gone);
    assert!(matches!(log[2].state, Presence::At { .. }));
    assert_eq!(log[2].created, "2026-07-31T11:00:00Z");
}

#[test]
fn an_id_less_document_still_has_a_lineage_by_path() {
    // The documents with no id — the config document, the registry, the bin
    // index, an attachment payload — are disproportionately what a sync
    // transport damages, so the weaker key has to work.
    let dir = seed("log-path");
    capture(&dir, "2026-07-31T09:00:00Z", None);
    write(&dir, "notes/photo.jpg", "OTHERBYTES");
    capture(&dir, "2026-07-31T10:00:00Z", None);

    let log = block_on(ws(&dir).history_log(
        Path::new("index.md"),
        &Subject::Path(PathBuf::from("notes/photo.jpg")),
    ))
    .unwrap();
    assert_eq!(log.len(), 2, "the payload's bytes changed once");
    let Presence::At { hash, .. } = &log[1].state else {
        panic!("the payload should be present in the second event");
    };
    assert_eq!(*hash, crate::fixity::digest(b"OTHERBYTES"));

    // A subject no event ever captured has an empty lineage, not an error.
    assert!(
        block_on(ws(&dir).history_log(
            Path::new("index.md"),
            &Subject::Path(PathBuf::from("notes/never.md")),
        ))
        .unwrap()
        .is_empty()
    );
}
