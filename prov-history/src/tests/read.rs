use std::path::{Path, PathBuf};

use super::support::*;
use crate::*;
use prov_graph::exec::block_on;
use prov_store::index::IndexStore;

/// The summary's whole contract: the same answer `list` gives, for the price of
/// a listing. A store with no events at all is the boundary case a cadence
/// check meets first, on the vault where history was just switched on.
#[test]
fn a_summary_names_the_event_history_list_would_have_named() {
    let dir = seed("summary-agrees");

    // Before any capture: no store, and nothing to be newest.
    let empty = block_on(store(&dir).summary(Path::new("index.md"))).unwrap();
    assert_eq!(empty, Summary::default());
    assert!(!empty.store_exists);

    capture_edited(&dir, "2026-07-29T09:15:22.000000Z", "one", "alpha");
    capture_edited(&dir, "2026-08-02T11:04:07.000000Z", "two", "beta");
    let newest = capture_edited(&dir, "2026-08-02T11:59:00.000000Z", "three", "gamma");

    let summary = block_on(store(&dir).summary(Path::new("index.md"))).unwrap();
    let listed = block_on(store(&dir).list(Path::new("index.md"))).unwrap();
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

    let latest = block_on(store(&dir).summary(Path::new("index.md")))
        .unwrap()
        .latest
        .expect("two events have a newest");
    let listed = block_on(store(&dir).list(Path::new("index.md"))).unwrap();

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

    let summary = block_on(store(&dir).summary(Path::new("index.md"))).unwrap();
    let latest = summary.latest.expect("the intact event is still there");

    assert_eq!(
        summary.events, 2,
        "the torn file is a slot: something captured, even if its bytes are now unreadable"
    );
    assert_eq!(latest.id, readable);
    assert_eq!(
        latest.id,
        block_on(store(&dir).list(Path::new("index.md")))
            .unwrap()
            .last()
            .unwrap()
            .id,
        "`list` skips the torn document too, so the two still agree"
    );
}

/// Size is the number a settings screen shows, and it is deliberately not in
/// the summary — one `metadata` call per file is the per-file cost the
/// summary exists to avoid.
#[test]
fn store_bytes_totals_the_store_and_answers_zero_when_there_is_none() {
    let dir = seed("summary-bytes");
    assert_eq!(
        block_on(store(&dir).store_bytes(Path::new("index.md"))).unwrap(),
        0,
        "no store is zero bytes, not an error"
    );

    capture_edited(&dir, "2026-07-31T09:15:22.000000Z", "one", "alpha");
    let first = block_on(store(&dir).store_bytes(Path::new("index.md"))).unwrap();
    assert!(first > 0);

    // A second capture parks the changed document's new bytes and writes
    // another event, so the store grows — while the untouched files go on
    // sharing the blobs they already parked.
    capture_edited(&dir, "2026-08-01T10:00:00.000000Z", "two", "beta");
    assert!(
        block_on(store(&dir).store_bytes(Path::new("index.md"))).unwrap() > first,
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
    let (index, found) = block_on(store(&dir).store_index(Path::new("index.md"))).unwrap();
    assert_eq!(found, StoreLocation::Conventional);
    assert_eq!(index, PathBuf::from("history/index.md"));
    // And the event is restorable, which is the whole point of still finding it.
    assert!(
        block_on(store(&dir).event(Path::new("index.md"), &before[0]))
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

    let (_, found) = block_on(store(&dir).store_index(Path::new("index.md"))).unwrap();
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
    let event = block_on(store(&dir).event(Path::new("index.md"), &id))
        .unwrap()
        .expect("the event must resolve without any index");
    assert_eq!(event.id, id);
    assert_eq!(event.label.as_deref(), Some("pre-sync"));
    assert_eq!(event.files.len(), 4);

    // An id that names nothing is absence, not an error; a string that is not
    // an event id at all is an error.
    assert!(
        block_on(store(&dir).event(Path::new("index.md"), "2026-07-31-0000-deadbeef"))
            .unwrap()
            .is_none()
    );
    assert!(block_on(store(&dir).event(Path::new("index.md"), "yesterday")).is_err());
}

#[test]
fn missing_blobs_name_the_paths_a_restore_could_not_recover() {
    let dir = seed("show-blobs");
    let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
        panic!("the first capture must write an event");
    };
    let event = block_on(store(&dir).event(Path::new("index.md"), &id))
        .unwrap()
        .unwrap();
    assert!(
        block_on(store(&dir).missing_blobs(Path::new("index.md"), &event))
            .unwrap()
            .is_empty(),
        "a capture parks every file's bytes"
    );

    // The half-synced case: the event document arrived, one blob did not.
    let payload = digest(b"JPEGBYTES");
    let blob = blob_path(Path::new("history/index.md"), &payload).unwrap();
    std::fs::remove_file(dir.join(&blob)).unwrap();
    let missing = block_on(store(&dir).missing_blobs(Path::new("index.md"), &event)).unwrap();
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
        block_on(store(&dir).missing_blobs(Path::new("index.md"), &foreign))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn read_only_verbs_keep_degrading_gracefully_around_an_unreadable_event() {
    // §7's flip side, restated as a test: the destructive verbs and the
    // store's own `findings` must refuse or report, but `list` (and anything
    // built on it) has always been allowed to skip what it cannot read — that
    // is graceful degradation, not the destruction this fix guards against.
    let dir = seed("list-torn");
    let first = capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
    let second = capture_edited(&dir, "2026-07-31T10:00:00.000000Z", "two", "beta");
    let torn = event_path(Path::new("history/index.md"), &first, "md").unwrap();
    tear(&dir, torn.to_str().unwrap());

    let events = block_on(store(&dir).list(Path::new("index.md"))).unwrap();
    assert_eq!(
        events.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        vec![second.as_str()],
        "a read still answers with whatever it could parse"
    );
}

#[test]
fn a_lineage_follows_an_id_through_a_rename_no_path_key_could() {
    let dir = seed("log-rename");
    let mut w = store(&dir);
    let id = Id("b7k2m".into());
    w.host_mut()
        .index_mut()
        .register(&id, Path::new("notes/a.md"));
    let take = |w: &mut HistoryStore<TestHost>, now: &str| {
        block_on(w.capture(Path::new("index.md"), now, None)).unwrap()
    };
    take(&mut w, "2026-07-31T09:00:00Z");

    // The move: same bytes, new path. A path-keyed store shows two unrelated
    // lineages here; the id column shows one document that moved.
    std::fs::rename(dir.join("notes/a.md"), dir.join("notes/b.md")).unwrap();
    relink(&dir, &["notes/b.md", "notes/photo.jpg.yaml"]);
    w.host_mut()
        .index_mut()
        .set_path(&id, Path::new("notes/b.md"));
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

    let log = block_on(w.log(Path::new("index.md"), &Subject::Id(id.clone()))).unwrap();
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

    // The same document asked for by its old *path*. The id column is not what
    // carries it across the move here — the manifests are: one path left with
    // those exact bytes and one arrived with them, so the rename is inferred and
    // the lineage stays whole. The row it finds still remembers the id, which is
    // what lets the weaker query hand the caller the stronger one.
    let by_path = block_on(w.log(
        Path::new("index.md"),
        &Subject::Path(PathBuf::from("notes/a.md")),
    ))
    .unwrap();
    assert!(matches!(
        &by_path[0].state,
        Presence::At { id: Some(found), .. } if *found == id
    ));
    assert_ne!(
        by_path.last().unwrap().state,
        Presence::Gone,
        "the inferred rename must keep a path-keyed lineage from ending at the move"
    );
    assert_eq!(
        by_path.iter().map(|v| v.inferred).collect::<Vec<_>>(),
        vec![false, true, false],
        "only the point that crossed the rename is an inference"
    );
    // Inferred or recorded, the two keys agree on the answer.
    assert_eq!(
        by_path.iter().map(|v| &v.state).collect::<Vec<_>>(),
        log.iter().map(|v| &v.state).collect::<Vec<_>>(),
    );
}

#[test]
fn a_lineage_records_a_deletion_and_a_return() {
    let dir = seed("log-gone");
    let mut w = store(&dir);
    let id = Id("b7k2m".into());
    w.host_mut()
        .index_mut()
        .register(&id, Path::new("notes/a.md"));
    let take = |w: &mut HistoryStore<TestHost>, now: &str| {
        block_on(w.capture(Path::new("index.md"), now, None)).unwrap()
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

    let log = block_on(w.log(Path::new("index.md"), &Subject::Id(id))).unwrap();
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

    let log = block_on(store(&dir).log(
        Path::new("index.md"),
        &Subject::Path(PathBuf::from("notes/photo.jpg")),
    ))
    .unwrap();
    assert_eq!(log.len(), 2, "the payload's bytes changed once");
    let Presence::At { hash, .. } = &log[1].state else {
        panic!("the payload should be present in the second event");
    };
    assert_eq!(*hash, digest(b"OTHERBYTES"));

    // A subject no event ever captured has an empty lineage, not an error.
    assert!(
        block_on(store(&dir).log(
            Path::new("index.md"),
            &Subject::Path(PathBuf::from("notes/never.md")),
        ))
        .unwrap()
        .is_empty()
    );
}

/// `cat`'s whole contract: the bytes a capture *held*, not the bytes on disk
/// now. The distinction is the point of the verb — a workspace whose file has
/// since changed is exactly when anyone asks.
#[test]
fn a_cat_returns_the_pre_image_not_the_current_file() {
    let dir = seed("cat-preimage");
    capture(&dir, "2026-07-31T09:00:00Z", None);
    let first = event_ids(&dir).pop().unwrap();

    // The workspace moves on: prose edited, payload replaced.
    write(
        &dir,
        "notes/a.md",
        "---\ntitle: A\npart_of: '../index.md'\n---\nrevised\n",
    );
    write(&dir, "notes/photo.jpg", "OTHERBYTES");
    capture(&dir, "2026-07-31T10:00:00Z", None);

    let w = store(&dir);
    let event = block_on(w.event(Path::new("index.md"), &first))
        .unwrap()
        .unwrap();
    let get = |target: &str| {
        block_on(w.cat(
            Path::new("index.md"),
            &event,
            &Subject::Path(PathBuf::from(target)),
        ))
        .unwrap()
    };

    let Retrieved::Bytes { path, hash, bytes } = get("notes/a.md") else {
        panic!("the first event captured notes/a.md");
    };
    assert_eq!(
        String::from_utf8(bytes.clone()).unwrap(),
        "---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n",
        "the bytes must be the ones that were captured, not the ones on disk"
    );
    assert_eq!(path, PathBuf::from("notes/a.md"));
    // The returned bytes hash to the digest the manifest recorded — which is the
    // only thing that makes them evidence rather than a copy of something.
    assert_eq!(hash, digest(&bytes));

    // Not text, and not treated as text: a capture set holds whatever the
    // workspace holds, and the attachment is why `cat` yields bytes.
    let Retrieved::Bytes { bytes, .. } = get("notes/photo.jpg") else {
        panic!("the first event captured the payload");
    };
    assert_eq!(bytes, b"JPEGBYTES");
}

/// Following an id reaches a document across a rename — the same property
/// `log` has, and for the same reason: a path-keyed lookup would report the
/// document as never captured, which is the one wrong answer available here.
#[test]
fn a_cat_follows_an_id_to_the_path_the_capture_recorded() {
    let dir = seed("cat-rename");
    let mut w = store(&dir);
    let id = Id("b7k2m".into());
    w.host_mut()
        .index_mut()
        .register(&id, Path::new("notes/a.md"));
    block_on(w.capture(Path::new("index.md"), "2026-07-31T09:00:00Z", None)).unwrap();
    let first = event_ids(&dir).pop().unwrap();

    std::fs::rename(dir.join("notes/a.md"), dir.join("notes/b.md")).unwrap();
    relink(&dir, &["notes/b.md", "notes/photo.jpg.yaml"]);
    w.host_mut()
        .index_mut()
        .set_path(&id, Path::new("notes/b.md"));
    block_on(w.capture(Path::new("index.md"), "2026-07-31T10:00:00Z", None)).unwrap();

    let event = block_on(w.event(Path::new("index.md"), &first))
        .unwrap()
        .unwrap();
    let Retrieved::Bytes { path, .. } =
        block_on(w.cat(Path::new("index.md"), &event, &Subject::Id(id.clone()))).unwrap()
    else {
        panic!("the id was captured in the first event, under its old path");
    };
    assert_eq!(
        path,
        PathBuf::from("notes/a.md"),
        "the reported path is the one that capture recorded, not the current one"
    );

    // The path the document has *now* was not in that capture at all, which is
    // precisely the miss the id key exists to avoid.
    assert_eq!(
        block_on(w.cat(
            Path::new("index.md"),
            &event,
            &Subject::Path(PathBuf::from("notes/b.md")),
        ))
        .unwrap(),
        Retrieved::Unrecorded
    );
}

/// The three absences, kept apart. A row that never existed, bytes still in
/// flight, and bytes destroyed on purpose are three different facts, and a
/// caller piping this into `diff` has to be able to tell them apart.
#[test]
fn a_cat_tells_the_three_absences_apart() {
    let dir = seed("cat-absent");
    capture(&dir, "2026-07-31T09:00:00Z", None);
    let id = event_ids(&dir).pop().unwrap();
    let w = store(&dir);
    let event = block_on(w.event(Path::new("index.md"), &id))
        .unwrap()
        .unwrap();
    let get = |target: &str| {
        block_on(w.cat(
            Path::new("index.md"),
            &event,
            &Subject::Path(PathBuf::from(target)),
        ))
        .unwrap()
    };

    // Never captured — the document did not exist when the event was taken.
    assert_eq!(get("notes/never.md"), Retrieved::Unrecorded);

    // Captured, bytes not here: the ordinary half-synced event. Reported as its
    // own kind rather than as loss, because it resolves itself.
    let payload = blob_of(b"JPEGBYTES");
    std::fs::remove_file(dir.join(&payload)).unwrap();
    assert_eq!(
        get("notes/photo.jpg"),
        Retrieved::NoBytes {
            path: PathBuf::from("notes/photo.jpg"),
            hash: digest(b"JPEGBYTES"),
        }
    );
}

/// Deliberate destruction reads as deliberate. `forget` states its bargain —
/// the record outlives the bytes — and a read verb that called the result
/// "missing" would report a completed operation as damage.
#[test]
fn a_cat_names_forgotten_bytes_as_forgotten_rather_than_missing() {
    let dir = seed("cat-forgotten");
    capture(&dir, "2026-07-31T09:00:00Z", None);
    let id = event_ids(&dir).pop().unwrap();

    // Forgetting a document still in the capture set needs `force`: the next
    // capture would only park it again.
    block_on(store(&dir).forget(
        Path::new("index.md"),
        &Subject::Path(PathBuf::from("notes/photo.jpg")),
        "2026-07-31T10:00:00Z",
        true,
    ))
    .unwrap();

    let w = store(&dir);
    let event = block_on(w.event(Path::new("index.md"), &id))
        .unwrap()
        .unwrap();
    assert_eq!(
        block_on(w.cat(
            Path::new("index.md"),
            &event,
            &Subject::Path(PathBuf::from("notes/photo.jpg")),
        ))
        .unwrap(),
        Retrieved::Forgotten {
            path: PathBuf::from("notes/photo.jpg"),
            hash: digest(b"JPEGBYTES"),
        },
        "the tombstone is what separates destroyed-on-purpose from not-here-yet"
    );

    // The event still records that the file existed, at that path, with that
    // hash — the record the bargain promises to keep.
    assert!(
        event
            .files
            .iter()
            .any(|f| f.path == Path::new("notes/photo.jpg"))
    );
}

/// The payoff: a document with **no id at all** keeps its lineage across a
/// rename. That is nearly every document in an archive of any age — and exactly
/// the population a sync transport damages, so the weaker key needed to stop
/// being quite this weak.
#[test]
fn a_lineage_infers_a_rename_for_a_document_with_no_id() {
    let dir = seed("log-infer");
    capture(&dir, "2026-07-31T09:00:00Z", None);

    // A move and nothing else: the bytes are byte-identical either side, which
    // is the whole basis of the inference.
    std::fs::rename(dir.join("notes/a.md"), dir.join("notes/moved.md")).unwrap();
    relink(&dir, &["notes/moved.md", "notes/photo.jpg.yaml"]);
    capture(&dir, "2026-07-31T10:00:00Z", None);

    // …then an edit at the new path, to prove the lineage kept following rather
    // than merely surviving the one event.
    write(
        &dir,
        "notes/moved.md",
        "---\ntitle: A\npart_of: '../index.md'\n---\nrevised\n",
    );
    capture(&dir, "2026-07-31T11:00:00Z", None);

    let log = block_on(store(&dir).log(
        Path::new("index.md"),
        &Subject::Path(PathBuf::from("notes/a.md")),
    ))
    .unwrap();

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
            Path::new("notes/moved.md"),
            Path::new("notes/moved.md"),
        ],
        "the lineage must cross the rename and keep going"
    );
    assert_eq!(
        log.iter().map(|v| v.inferred).collect::<Vec<_>>(),
        vec![false, true, false],
        "the inference is marked at the point that used it, and nowhere else"
    );
    // No id was ever recorded, so nothing but the hashes could have carried it.
    assert!(
        log.iter()
            .all(|v| matches!(&v.state, Presence::At { id: None, .. } | Presence::Gone))
    );
}

/// The guard that keeps the inference honest, tested against the case a real
/// workspace produces immediately: **identical bytes are not a unique name.**
/// Two documents sharing a digest — boilerplate, or the empty file every vault
/// has several of — move in one capture, and no pairing between them is
/// evidence. The lineage breaks rather than guessing.
#[test]
fn an_ambiguous_move_is_not_inferred_as_a_rename() {
    let dir = seed("log-ambiguous");
    let twin = "---\ntitle: Twin\npart_of: '../index.md'\n---\nsame\n";
    write(&dir, "notes/x.md", twin);
    write(&dir, "notes/y.md", twin);
    relink(
        &dir,
        &[
            "notes/a.md",
            "notes/photo.jpg.yaml",
            "notes/x.md",
            "notes/y.md",
        ],
    );
    capture(&dir, "2026-07-31T09:00:00Z", None);

    // Both twins move in the same capture. Either could be either.
    std::fs::rename(dir.join("notes/x.md"), dir.join("notes/p.md")).unwrap();
    std::fs::rename(dir.join("notes/y.md"), dir.join("notes/q.md")).unwrap();
    relink(
        &dir,
        &[
            "notes/a.md",
            "notes/photo.jpg.yaml",
            "notes/p.md",
            "notes/q.md",
        ],
    );
    capture(&dir, "2026-07-31T10:00:00Z", None);

    let log = block_on(store(&dir).log(
        Path::new("index.md"),
        &Subject::Path(PathBuf::from("notes/x.md")),
    ))
    .unwrap();
    assert_eq!(
        log.last().unwrap().state,
        Presence::Gone,
        "two candidates is not one candidate; an ambiguous pairing must not be claimed"
    );
    assert!(
        log.iter().all(|v| !v.inferred),
        "nothing here was inferred, so nothing may be marked as inferred"
    );

    // The unambiguous half of the same store still works: `notes/a.md` never
    // moved, so the guard costs the ordinary case nothing.
    let steady = block_on(store(&dir).log(
        Path::new("index.md"),
        &Subject::Path(PathBuf::from("notes/a.md")),
    ))
    .unwrap();
    assert!(matches!(steady.last().unwrap().state, Presence::At { .. }));
}

/// A document that moves twice is followed twice: the tracked path is *where the
/// lineage is now*, not where it started, so each rename is inferred against the
/// capture immediately before it.
#[test]
fn a_lineage_follows_more_than_one_inferred_rename() {
    let dir = seed("log-infer-twice");
    capture(&dir, "2026-07-31T09:00:00Z", None);

    // Distinct timestamps, because these two events *must* order: same-minute
    // ids tie-break on a content digest, which is arbitrary by design.
    for (from, to, now) in [
        ("notes/a.md", "notes/b.md", "2026-07-31T10:00:00Z"),
        ("notes/b.md", "notes/c.md", "2026-07-31T11:00:00Z"),
    ] {
        std::fs::rename(dir.join(from), dir.join(to)).unwrap();
        relink(&dir, &[to, "notes/photo.jpg.yaml"]);
        capture(&dir, now, None);
    }

    let log = block_on(store(&dir).log(
        Path::new("index.md"),
        &Subject::Path(PathBuf::from("notes/a.md")),
    ))
    .unwrap();
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
            Path::new("notes/c.md"),
        ],
    );
    assert_eq!(
        log.iter().map(|v| v.inferred).collect::<Vec<_>>(),
        vec![false, true, true],
    );
}

/// A diff's four kinds, over two real captures. The one worth checking hardest
/// is the move: it is the difference between a diff a person can read and a
/// wall of paired deletions and creations.
#[test]
fn a_diff_names_what_changed_moved_arrived_and_went() {
    let dir = seed("diff-kinds");
    let note = |title: &str, body: &str| {
        format!("---\ntitle: {title}\npart_of: '../index.md'\n---\n{body}\n")
    };
    write(&dir, "notes/keep.md", &note("Keep", "kept"));
    write(&dir, "notes/mover.md", &note("Mover", "travelling"));
    relink(
        &dir,
        &[
            "notes/a.md",
            "notes/photo.jpg.yaml",
            "notes/keep.md",
            "notes/mover.md",
        ],
    );
    capture(&dir, "2026-07-31T09:00:00Z", None);

    write(&dir, "notes/a.md", &note("A", "revised"));
    std::fs::rename(dir.join("notes/mover.md"), dir.join("notes/moved.md")).unwrap();
    std::fs::remove_file(dir.join("notes/keep.md")).unwrap();
    write(&dir, "notes/new.md", &note("New", "arrived"));
    relink(
        &dir,
        &[
            "notes/a.md",
            "notes/photo.jpg.yaml",
            "notes/moved.md",
            "notes/new.md",
        ],
    );
    capture(&dir, "2026-07-31T10:00:00Z", None);

    let events = block_on(store(&dir).list(Path::new("index.md"))).unwrap();
    let diff = manifest_diff(&events[0], &events[1]);
    assert_eq!(diff.from, events[0].id);
    assert_eq!(diff.to, events[1].id);

    let row = |p: &str| {
        diff.rows
            .iter()
            .find(|r| r.path == Path::new(p))
            .unwrap_or_else(|| panic!("no diff row for {p}"))
            .change
            .clone()
    };
    assert!(matches!(row("notes/a.md"), Change::Changed { .. }));
    assert_eq!(
        row("notes/moved.md"),
        Change::Moved {
            from: PathBuf::from("notes/mover.md"),
            hash: digest(note("Mover", "travelling").as_bytes()),
        },
        "identical bytes at a new path is a move, not a deletion beside a creation"
    );
    assert!(matches!(row("notes/new.md"), Change::Added { .. }));
    assert!(matches!(row("notes/keep.md"), Change::Removed { .. }));

    // A moved path appears once, under its new name only — the old name must
    // not also be reported as removed.
    assert!(
        !diff
            .rows
            .iter()
            .any(|r| r.path == Path::new("notes/mover.md")),
        "the move already accounts for the old path"
    );
    // Unchanged files are absent entirely; a diff lists differences.
    assert!(
        !diff
            .rows
            .iter()
            .any(|r| r.path == Path::new("notes/photo.jpg"))
    );

    // Rows read changed, moved, added, removed — the order the question is asked.
    let ranks: Vec<u8> = diff.rows.iter().map(|r| r.change.rank()).collect();
    assert!(
        ranks.windows(2).all(|w| w[0] <= w[1]),
        "{ranks:?} is not sorted"
    );

    // An event against itself differs in nothing.
    assert!(manifest_diff(&events[1], &events[1]).is_empty());
}

/// The pairing rule is shared with `history-log`'s rename inference, so it fails
/// the same way: identical bytes are not a unique name, and an ambiguous move is
/// reported as what can actually be seen — a removal and an arrival.
#[test]
fn an_ambiguous_move_is_not_paired_in_a_diff() {
    let dir = seed("diff-ambiguous");
    let twin = "---\ntitle: Twin\npart_of: '../index.md'\n---\nsame\n";
    write(&dir, "notes/x.md", twin);
    write(&dir, "notes/y.md", twin);
    fn all<'a>(a: &'a str, b: &'a str) -> Vec<&'a str> {
        vec!["notes/a.md", "notes/photo.jpg.yaml", a, b]
    }
    relink(&dir, &all("notes/x.md", "notes/y.md"));
    capture(&dir, "2026-07-31T09:00:00Z", None);

    std::fs::rename(dir.join("notes/x.md"), dir.join("notes/p.md")).unwrap();
    std::fs::rename(dir.join("notes/y.md"), dir.join("notes/q.md")).unwrap();
    relink(&dir, &all("notes/p.md", "notes/q.md"));
    capture(&dir, "2026-07-31T10:00:00Z", None);

    let events = block_on(store(&dir).list(Path::new("index.md"))).unwrap();
    let diff = manifest_diff(&events[0], &events[1]);
    let kind = |p: &str| {
        diff.rows
            .iter()
            .find(|r| r.path == Path::new(p))
            .map(|r| r.change.clone())
    };
    for gone in ["notes/x.md", "notes/y.md"] {
        assert!(
            matches!(kind(gone), Some(Change::Removed { .. })),
            "{gone} should read as removed, since which arrival it became is a guess"
        );
    }
    for arrived in ["notes/p.md", "notes/q.md"] {
        assert!(matches!(kind(arrived), Some(Change::Added { .. })));
    }
    assert!(
        !diff
            .rows
            .iter()
            .any(|r| matches!(r.change, Change::Moved { .. })),
        "no pairing here is unambiguous, so none may be claimed"
    );
}
