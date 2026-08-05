use std::path::{Path, PathBuf};

use super::event_id::*;
use super::layout::*;
use super::paths::*;
use super::*;
use crate::error::{Error, Result};
use crate::exec::block_on;
use crate::fs::StdFs;
use crate::identity::Id;
use crate::identity::Minter;
use crate::index::FileIndex;
use crate::index::{Collision, IndexStore};
use crate::validate::Finding;
use crate::workspace::Workspace;

#[test]
fn an_event_id_is_reversible_to_its_shard_path() {
    let id = "2026-07-31-0915-pre-sync-4f2a9c1e";
    assert_eq!(shard_of(id).unwrap(), Path::new("2026").join("07"));
    assert_eq!(
        event_path(Path::new("history/index.md"), id, "md").unwrap(),
        Path::new("history/events/2026/07/2026-07-31-0915-pre-sync-4f2a9c1e.md")
    );
    // The point of repeating the date in the id: it resolves with every
    // index file destroyed.
    assert!(shard_of("not-an-event-id").is_err());
}

#[test]
fn a_blob_path_is_bare_hex_never_the_scheme_prefix() {
    let hash = crate::fixity::digest(b"hello");
    let path = blob_path(Path::new("history/index.md"), &hash).unwrap();
    let spelled = path.to_string_lossy();
    assert!(
        !spelled.contains(':'),
        "a colon in a blob filename is hostile to Windows and to sync clients: {spelled}"
    );
    let hex = hash.strip_prefix("sha256:").unwrap();
    assert_eq!(
        path,
        Path::new("history/blobs").join(&hex[..2]).join(&hex[2..])
    );
    assert!(blob_path(Path::new("history/index.md"), "blake3:beef").is_err());
}

#[test]
fn the_id_stamp_reads_the_timestamp_and_survives_a_round_trip() {
    assert_eq!(id_stamp("2026-07-31T09:15:22Z").unwrap(), "2026-07-31-0915");
    assert!(id_stamp("yesterday").is_err());
    assert_eq!(
        display_stamp("2026-07-31-0915-pre-sync-4f2a9c1e"),
        "2026-07-31 09:15"
    );
    // Two captures in the same minute must not read identically in an index,
    // so the entry carries the label slug the id already encodes.
    assert_eq!(
        display_entry("2026-07-31-0915-pre-sync-4f2a9c1e"),
        "2026-07-31 09:15 (pre-sync)"
    );
    assert_eq!(
        label_slug("2026-07-31-0915-pre-sync-4f2a9c1e"),
        Some("pre-sync".into())
    );
    assert_eq!(label_slug("2026-07-31-0915-4f2a9c1e"), None);
    assert_eq!(
        display_entry("2026-07-31-0915-4f2a9c1e"),
        "2026-07-31 09:15"
    );
}

fn entry(path: &str, hash_of: &[u8]) -> FileEntry {
    FileEntry {
        path: PathBuf::from(path),
        id: None,
        hash: crate::fixity::digest(hash_of),
    }
}

#[test]
fn the_canonical_form_ignores_the_serialization_format() {
    // Two devices, same state, same timestamp — the id must converge, which
    // is what makes a collision benign rather than a conflict.
    let files = vec![entry("a.md", b"a"), entry("b.md", b"b")];
    let one = mint_id("2026-07-31T09:15:22Z", TRIGGER_MANUAL, None, None, &files).unwrap();
    let two = mint_id("2026-07-31T09:15:22Z", TRIGGER_MANUAL, None, None, &files).unwrap();
    assert_eq!(one, two);

    // A different capture set is a different event.
    let changed = vec![entry("a.md", b"a"), entry("b.md", b"CHANGED")];
    let three = mint_id("2026-07-31T09:15:22Z", TRIGGER_MANUAL, None, None, &changed).unwrap();
    assert_ne!(one, three);
    // …and so is a different parent, so two devices forking from different
    // points do not collide.
    let forked = mint_id(
        "2026-07-31T09:15:22Z",
        TRIGGER_MANUAL,
        None,
        Some("2026-07-30-1804-nightly-8c1d55aa"),
        &files,
    )
    .unwrap();
    assert_ne!(one, forked);
}

#[test]
fn a_label_is_slugged_into_the_id_and_omitted_when_absent() {
    let files = vec![entry("a.md", b"a")];
    let labeled = mint_id(
        "2026-07-31T09:15:22Z",
        TRIGGER_MANUAL,
        Some("Pre Sync!"),
        None,
        &files,
    )
    .unwrap();
    assert!(
        labeled.starts_with("2026-07-31-0915-pre-sync-"),
        "{labeled}"
    );
    let bare = mint_id("2026-07-31T09:15:22Z", TRIGGER_MANUAL, None, None, &files).unwrap();
    assert!(bare.starts_with("2026-07-31-0915-"), "{bare}");
    // Both still parse back to the same shard.
    assert_eq!(shard_of(&labeled).unwrap(), shard_of(&bare).unwrap());
}

#[test]
fn timestamps_of_two_precisions_still_order_against_each_other() {
    // The migration hazard, stated as an assertion. A store keeps every
    // precision it was ever written at, because events are immutable and sync
    // interleaves devices — so the comparison, not the clock, is what has to
    // make them one order.
    let coarse = "2026-07-31T09:15:10Z";
    let fine = "2026-07-31T09:15:10.500000Z";
    assert!(
        coarse > fine,
        "the raw strings really are backwards — `Z` sorts after `.`"
    );
    assert!(
        comparable(coarse) < comparable(fine),
        "normalized, 09:15:10.000000 precedes 09:15:10.500000"
    );

    // Padding is to a fixed width, from either side, so a stamp written by
    // some other tool at millisecond or nanosecond precision still lands in
    // the right place.
    assert_eq!(
        comparable("2026-07-31T09:15:10Z"),
        "2026-07-31T09:15:10.000000Z"
    );
    assert_eq!(
        comparable("2026-07-31T09:15:10.5Z"),
        "2026-07-31T09:15:10.500000Z"
    );
    assert_eq!(
        comparable("2026-07-31T09:15:10.123456789Z"),
        "2026-07-31T09:15:10.123456Z"
    );
    // Already canonical: borrowed, not rebuilt.
    assert!(matches!(
        comparable("2026-07-31T09:15:10.123456Z"),
        std::borrow::Cow::Borrowed(_)
    ));
    // Not a `Z` stamp: left exactly as found rather than quietly mangled.
    assert_eq!(
        comparable("2026-07-31T09:15:10+01:00"),
        "2026-07-31T09:15:10+01:00"
    );

    // And the id is unaffected: it reads the calendar head only, so the
    // fraction changes nothing about where an event lives or what it is called.
    assert_eq!(id_stamp(coarse).unwrap(), id_stamp(fine).unwrap());
}

#[test]
fn diff_counts_changed_and_removed_against_the_previous_manifest() {
    let previous = Event {
        id: "p".into(),
        path: PathBuf::new(),
        created: "2026-07-30T00:00:00Z".into(),
        trigger: TRIGGER_MANUAL.into(),
        label: None,
        parent: None,
        files: vec![entry("a.md", b"a"), entry("gone.md", b"g")],
    };
    let current = Event {
        files: vec![entry("a.md", b"CHANGED"), entry("new.md", b"n")],
        ..previous.clone()
    };
    // `a.md` changed, `new.md` is new → 2; `gone.md` is removed → 1.
    assert_eq!(current.diff(&previous), (2, 1));
}

#[test]
fn manifest_order_is_byte_wise_on_the_joined_string_not_path_component_order() {
    // The bug, stated as directly as possible: `.` (0x2E) sorts before `/`
    // (0x2F) in the joined string, so `notes.md` belongs before
    // `notes/x.md`. `Path::cmp` disagrees — it compares the bare `notes`
    // component (a prefix of `notes.md`) against the whole `notes.md`
    // component and calls `notes` smaller, putting `notes/x.md` first.
    let notes_file = Path::new("notes.md");
    let notes_dir_file = Path::new("notes/x.md");
    assert!(
        notes_dir_file < notes_file,
        "Path::cmp really does get this backwards"
    );
    assert!(
        path_sort_key(notes_file) < path_sort_key(notes_dir_file),
        "the manifest's own key must get it the other way round"
    );

    // The same collision one directory deeper, to be sure the key is not
    // secretly depth-limited.
    let mut paths = [
        Path::new("deep/notes/x.md"),
        Path::new("deep/notes.md"),
        Path::new("index.md"),
        Path::new("notes/x.md"),
        Path::new("notes.md"),
    ];
    paths.sort_by_key(|p| path_sort_key(p));
    assert_eq!(
        paths,
        [
            Path::new("deep/notes.md"),
            Path::new("deep/notes/x.md"),
            Path::new("index.md"),
            Path::new("notes.md"),
            Path::new("notes/x.md"),
        ]
    );
}

#[test]
fn manifest_equality_for_the_unchanged_check_ignores_row_order() {
    // §6's "computed manifest is identical" is same-state, not
    // same-serialization: a manifest a pre-fix writer left in `Path`
    // component order must still compare equal to the correctly-ordered
    // one this fix computes for the same state, or a habitual `history-
    // capture` against an old store would start filling the log with
    // duplicates the moment it hit a collision like `notes.md` /
    // `notes/x.md`.
    let sorted = vec![entry("notes.md", b"n"), entry("notes/x.md", b"x")];
    let mut component_order = sorted.clone();
    component_order.reverse();
    assert_ne!(
        sorted, component_order,
        "the derived Vec equality this replaces really is row-order-sensitive"
    );
    assert_eq!(manifest_of(&sorted), manifest_of(&component_order));
}

#[test]
fn the_capture_set_exclusion_is_by_directory_prefix() {
    let store = Path::new("history");
    assert!(under(Path::new("history"), store));
    assert!(under(Path::new("history/index.md"), store));
    assert!(under(Path::new("history/events/2026/07/x.md"), store));
    // A sibling that merely shares a prefix is not inside it.
    assert!(!under(Path::new("historybook.md"), store));
    assert!(!under(Path::new("notes/a.md"), store));
}

// ── Store-level tests, over a real filesystem ────────────────────────────

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("prov-history-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).unwrap()
}

fn ws(dir: &Path) -> Workspace<StdFs, Minter, FileIndex> {
    Workspace::builder(StdFs)
        .root(dir)
        .identity(Minter::lazy(42))
        .index(FileIndex::new(fig::Format::Yaml))
        .build()
}

/// A small workspace: a root, two notes, and an attachment (payload plus
/// sidecar) — so the capture set covers the shapes that actually matter.
fn seed(tag: &str) -> PathBuf {
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

fn capture(dir: &Path, now: &str, label: Option<&str>) -> Captured {
    block_on(ws(dir).history_capture(Path::new("index.md"), now, label)).unwrap()
}

fn event_ids(dir: &Path) -> Vec<String> {
    block_on(ws(dir).history_list(Path::new("index.md")))
        .unwrap()
        .into_iter()
        .map(|e| e.id)
        .collect()
}

#[test]
fn a_capture_bootstraps_the_store_and_captures_attachment_payloads() {
    let dir = seed("capture-basic");
    let Captured::Written { id, files, .. } =
        capture(&dir, "2026-07-31T09:15:22Z", Some("pre-sync"))
    else {
        panic!("the first capture must write an event");
    };

    // The root now points at the store, so it is reachable — the whole
    // anti-`.obsidian/` move.
    assert!(
        read(&dir, "index.md").contains("history:"),
        "the root must declare the store: {}",
        read(&dir, "index.md")
    );
    // The id resolves to its path with no index consulted.
    let event = event_path(Path::new("history/index.md"), &id, "md").unwrap();
    assert!(dir.join(&event).exists(), "{} missing", event.display());

    // The capture set is the reachable file set: root, note, sidecar, and —
    // the one that is easy to get wrong — the attachment *payload*, which is
    // reached through the sidecar's `content` pointer rather than a relation.
    let manifest = read(&dir, event.to_str().unwrap());
    for expected in [
        "index.md",
        "notes/a.md",
        "notes/photo.jpg",
        "notes/photo.jpg.yaml",
    ] {
        assert!(
            manifest.contains(expected),
            "{expected} should be captured:\n{manifest}"
        );
    }
    assert_eq!(files, 4);

    // Every captured file's bytes are parked, addressed by content, with no
    // colon anywhere in the path.
    let payload_hash = crate::fixity::digest(b"JPEGBYTES");
    let blob = blob_path(Path::new("history/index.md"), &payload_hash).unwrap();
    assert_eq!(read(&dir, blob.to_str().unwrap()), "JPEGBYTES");
}

#[test]
fn capture_sorts_the_manifest_byte_wise_not_by_path_components() {
    // `notes.md` beside `notes/x.md` — a file and a same-stem directory as
    // siblings — plus the identical collision one directory deeper, so a
    // depth-limited fix would still fail this. `docs/history-format.md`
    // §3.1 requires byte-wise ascending order on the joined path string;
    // `BTreeSet<PathBuf>`/`Path::cmp` order component-wise and get exactly
    // this shape backwards (see `path_sort_key`).
    let dir = tempdir("capture-manifest-order");
    write(
        &dir,
        "index.md",
        "---\ntitle: Home\ncontents:\n- notes.md\n- notes/x.md\n- deep/notes.md\n\
         - deep/notes/x.md\n---\nroot\n",
    );
    write(
        &dir,
        "notes.md",
        "---\ntitle: Notes\npart_of: 'index.md'\n---\nnotes\n",
    );
    write(
        &dir,
        "notes/x.md",
        "---\ntitle: X\npart_of: '../index.md'\n---\nx\n",
    );
    write(
        &dir,
        "deep/notes.md",
        "---\ntitle: Deep notes\npart_of: '../index.md'\n---\ndeep notes\n",
    );
    write(
        &dir,
        "deep/notes/x.md",
        "---\ntitle: Deep X\npart_of: '../../index.md'\n---\ndeep x\n",
    );

    let Captured::Written { id, files, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
        panic!("the first capture must write an event");
    };
    assert_eq!(files, 5, "the root plus the four collision files");

    // Read the `path:` rows back off the document itself, in the order
    // they were written — the manifest is what two implementations have
    // to agree on, not `Event.files`' in-memory order.
    let event_rel = event_path(Path::new("history/index.md"), &id, "md").unwrap();
    let manifest_text = read(&dir, event_rel.to_str().unwrap());
    let order: Vec<&str> = manifest_text
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("- path: "))
        .collect();
    assert_eq!(
        order,
        vec![
            "deep/notes.md",
            "deep/notes/x.md",
            "index.md",
            "notes.md",
            "notes/x.md",
        ],
        "byte-wise ascending — `.` (0x2E) sorts before `/` (0x2F):\n{manifest_text}"
    );

    // And the id: read the event back and independently recompute the
    // digest suffix from its own recorded fields via `canonical_bytes`,
    // the same function `mint_id` used to mint it — proof the id names
    // exactly the manifest that landed on disk, in the order it landed.
    let event = block_on(ws(&dir).history_event(Path::new("index.md"), &id))
        .unwrap()
        .expect("the just-written event must read back");
    let digest = crate::fixity::digest(&canonical_bytes(
        &event.created,
        &event.trigger,
        event.label.as_deref(),
        event.parent.as_deref(),
        &event.files,
    ));
    assert_eq!(
        &id[id.len() - 8..],
        &digest["sha256:".len().."sha256:".len() + 8]
    );
}

#[test]
fn the_store_is_never_captured_into_itself() {
    // The recursion the whole design turns on: capturing the store inside the
    // store would mean no capture could ever be empty, and an exact restore
    // would delete the recovery points themselves.
    let dir = seed("capture-recursion");
    capture(&dir, "2026-07-31T09:15:22Z", None);
    let set = block_on(ws(&dir).history_capture_set(Path::new("index.md"))).unwrap();
    assert!(
        set.iter().all(|p| !p.starts_with("history")),
        "the store must be invisible to the mechanism: {set:?}"
    );
    // And that is exactly what makes the no-op capture reachable.
    let second = capture(&dir, "2026-07-31T10:00:00Z", None);
    assert!(
        matches!(second, Captured::Unchanged { .. }),
        "an unchanged workspace must write nothing, got {second:?}"
    );
}

#[test]
fn an_unchanged_workspace_writes_no_second_event() {
    let dir = seed("capture-empty");
    let first = capture(&dir, "2026-07-31T09:15:22Z", None);
    let Captured::Written { id, .. } = first else {
        panic!("expected a first event")
    };
    // A different clock and a different label — still the same *state*, so
    // still nothing to record. Otherwise a git hook fills the log.
    let again = capture(&dir, "2026-07-31T11:00:00Z", Some("nightly"));
    assert_eq!(again, Captured::Unchanged { id: id.clone() });
    assert_eq!(event_ids(&dir), vec![id.clone()]);

    // Change one byte and it captures again.
    write(
        &dir,
        "notes/a.md",
        "---\ntitle: A\npart_of: '../index.md'\n---\nalpha edited\n",
    );
    let third = capture(&dir, "2026-07-31T12:00:00Z", None);
    let Captured::Written {
        diff: Some((changed, removed)),
        blobs,
        ..
    } = third
    else {
        panic!("a changed workspace must capture")
    };
    assert_eq!((changed, removed), (1, 0));
    // Only the changed file's bytes are new — the rest deduplicate for free.
    assert_eq!(blobs, 1);
    assert_eq!(event_ids(&dir).len(), 2);
}

#[test]
fn the_first_event_records_the_root_that_already_declares_the_store() {
    // The bootstrap capture edits the root (it gains the `history` pointer),
    // so the manifest must hash the *post-edit* bytes. Otherwise event #1
    // describes a root predating its own store, and restoring it exactly
    // would strand the store unreachable — the one thing a restore must never
    // do. It is also what lets the very next capture be a no-op.
    let dir = seed("capture-pointer");
    let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
        panic!("expected an event")
    };
    let events = block_on(ws(&dir).history_list(Path::new("index.md"))).unwrap();
    let root_row = events[0]
        .files
        .iter()
        .find(|f| f.path == Path::new("index.md"))
        .expect("the root is in the capture set");
    let on_disk = crate::fixity::digest(read(&dir, "index.md").as_bytes());
    assert_eq!(
        root_row.hash, on_disk,
        "event {id} must record the root as the capture left it"
    );
    // And the parked blob is those same bytes, so a restore is byte-exact.
    let blob = blob_path(Path::new("history/index.md"), &root_row.hash).unwrap();
    assert_eq!(read(&dir, blob.to_str().unwrap()), read(&dir, "index.md"));
}

#[test]
fn same_second_captures_chain_in_the_order_they_happened() {
    // The bug microsecond precision exists to close: with `created` pinned to
    // the second, two captures in one second tied, the sort fell through to
    // the id — whose *middle* is the label slug — and every later event
    // recorded the alphabetically-last label as its `parent`, so
    // `history-list` reported forks that never happened.
    let dir = seed("ordering");
    let stamps = [
        ("2026-07-31T09:15:10.000000Z", "zulu"),
        ("2026-07-31T09:15:10.200000Z", "alpha"),
        ("2026-07-31T09:15:10.900000Z", "mike"),
    ];
    for (i, (now, label)) in stamps.iter().enumerate() {
        // Each capture must change something, or the second one writes nothing.
        write(
            &dir,
            "notes/a.md",
            &format!("---\ntitle: A\npart_of: '../index.md'\n---\nrevision {i}\n"),
        );
        capture(&dir, now, Some(label));
    }

    let events = block_on(ws(&dir).history_list(Path::new("index.md"))).unwrap();
    assert_eq!(
        events
            .iter()
            .map(|e| e.label.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("zulu"), Some("alpha"), Some("mike")],
        "capture order, not alphabetical order by label"
    );
    // A chain, not a fan: each event's parent is the one actually before it,
    // which is what makes a real fork mean something in `history-list`.
    assert_eq!(events[0].parent, None);
    assert_eq!(events[1].parent.as_deref(), Some(events[0].id.as_str()));
    assert_eq!(events[2].parent.as_deref(), Some(events[1].id.as_str()));
}

#[test]
fn an_event_written_before_sub_second_precision_keeps_its_place() {
    // The mixed store, end to end: an event carrying a second-granularity
    // `created` (every event written before this precision existed) against
    // ones that carry a fraction. Compared raw, the old event would sort last
    // in its second and the newest-event lookup would pick it — so a later
    // capture would record a *superseded* event as its parent.
    let dir = seed("ordering-mixed");
    write(
        &dir,
        "notes/a.md",
        "---\ntitle: A\npart_of: '../index.md'\n---\nfirst\n",
    );
    capture(&dir, "2026-07-31T09:15:10Z", Some("legacy"));
    write(
        &dir,
        "notes/a.md",
        "---\ntitle: A\npart_of: '../index.md'\n---\nsecond\n",
    );
    capture(&dir, "2026-07-31T09:15:10.500000Z", Some("current"));

    let events = block_on(ws(&dir).history_list(Path::new("index.md"))).unwrap();
    assert_eq!(
        events
            .iter()
            .map(|e| e.label.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("legacy"), Some("current")]
    );
    assert_eq!(events[1].parent.as_deref(), Some(events[0].id.as_str()));
}

#[test]
fn a_transport_conflict_copy_is_not_mistaken_for_an_event() {
    // Litter beside the store must not become a phantom event — an index
    // rebuilt to *include* a conflict copy would enshrine the damage.
    assert!(is_event_id("2026-07-31-0915-pre-sync-4f2a9c1e"));
    assert!(is_event_id("2026-07-31-0915-4f2a9c1e"));
    assert!(!is_event_id(
        "2026-07-31-0915-one-1d1beacc.sync-conflict-20260731-091600"
    ));
    assert!(!is_event_id("index.sync-conflict-20260731-091600"));
    assert!(!is_event_id("index"));
    assert!(!is_event_id("notes"));
}

#[test]
fn a_capture_leaves_check_clean() {
    let dir = seed("capture-check");
    capture(&dir, "2026-07-31T09:15:22Z", Some("pre-sync"));
    let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
    assert!(
        findings.is_empty(),
        "a capture must leave the workspace valid: {findings:?}"
    );
}

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

    let payload = crate::fixity::digest(b"JPEGBYTES");
    std::fs::remove_file(dir.join(blob_path(Path::new("history/index.md"), &payload).unwrap()))
        .unwrap();

    let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
    assert_eq!(
        findings,
        vec![Finding::HistoryBlobMissing {
            store: PathBuf::from("history/index.md"),
            hash: payload.clone(),
            paths: vec![PathBuf::from("notes/photo.jpg")],
        }],
        "one lost blob is one thing to put back, not one report per event"
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

    // Deleting the blob left nothing behind, so there is no orphan to pair
    // with it: the two findings answer opposite questions.
    assert!(
        !findings
            .iter()
            .any(|f| matches!(f, Finding::HistoryBlobOrphaned { .. }))
    );
}

#[test]
fn a_manifest_row_prov_could_never_have_parked_reports_rather_than_failing() {
    // A foreign event has to stay legible: `check` reads what arrived from
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
        &text.replace(
            &crate::fixity::digest(b"JPEGBYTES"),
            "blake3:beefbeefbeefbeef",
        ),
    );

    let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
    let missing: Vec<&Finding> = findings
        .iter()
        .filter(|f| matches!(f, Finding::HistoryBlobMissing { .. }))
        .collect();
    assert_eq!(missing.len(), 1, "{findings:?}");
    assert!(
        missing[0].to_string().contains("blake3:"),
        "{:?}",
        missing[0]
    );
    // …and the blob it no longer names is now unreferenced, which is the
    // other half of the same sweep.
    assert!(
        findings
            .iter()
            .any(|f| matches!(f, Finding::HistoryBlobOrphaned { .. })),
        "{findings:?}"
    );
}

#[test]
fn bytes_no_manifest_claims_are_reported_as_orphaned() {
    let dir = seed("blob-orphan");
    capture(&dir, "2026-07-31T09:00:00.000000Z", None);
    assert!(
        block_on(ws(&dir).check(Path::new("index.md")))
            .unwrap()
            .is_empty(),
        "a fresh capture claims every blob it parked"
    );

    // Cruft of the two shapes a transport actually leaves: a conflict copy
    // beside a real blob, and a stray at the top of the store. Neither could
    // ever match a hash, which is the point — this is not a digest check.
    write(&dir, "history/blobs/ab/sync-conflict-20260731", "junk");
    write(&dir, "history/blobs/stray.txt", "junk");
    // A hidden file is transport bookkeeping, not cruft prov should name.
    write(&dir, "history/blobs/.DS_Store", "junk");

    let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
    assert_eq!(
        findings,
        vec![Finding::HistoryBlobOrphaned {
            store: PathBuf::from("history/index.md"),
            blobs: vec![
                PathBuf::from("history/blobs/ab/sync-conflict-20260731"),
                PathBuf::from("history/blobs/stray.txt"),
            ],
        }],
        "one sweep, one finding, sorted — and the dotfile left alone"
    );
    assert!(
        findings[0].to_string().contains("history-prune"),
        "the report names the verb that collects them: {}",
        findings[0]
    );
}

#[test]
fn a_new_month_grows_the_shard_tree_without_rewriting_old_shards() {
    let dir = seed("capture-shard");
    capture(&dir, "2026-07-31T09:15:22Z", None);
    let july = read(&dir, "history/events/2026/07/index.md");

    write(&dir, "notes/b.md", "---\ntitle: B\n---\nbeta\n");
    write(
        &dir,
        "index.md",
        "---\ntitle: Home\ncontents:\n- notes/a.md\n- notes/b.md\n- notes/photo.jpg.yaml\n\
         history: history/index.md\n---\nroot\n",
    );
    write(
        &dir,
        "notes/b.md",
        "---\ntitle: B\npart_of: '../index.md'\n---\nbeta\n",
    );
    capture(&dir, "2026-08-01T09:00:00Z", None);

    // The new month is its own shard, linked from the year index; July's
    // shard index is untouched — the mutable surface is "this month", not
    // "forever".
    assert!(dir.join("history/events/2026/08/index.md").exists());
    assert_eq!(read(&dir, "history/events/2026/07/index.md"), july);
    assert!(read(&dir, "history/events/2026/index.md").contains("08/index.md"));
    assert!(
        block_on(ws(&dir).check(Path::new("index.md")))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn binned_bytes_are_not_newly_retained_by_a_routine_capture() {
    // The exclusion is narrow and worth pinning: a capture must not park
    // bytes the user has consigned to the bin. (It emphatically does *not*
    // make a purge final for content captured while it was live — that is
    // documented, not tested here, because it is a non-guarantee.)
    let dir = seed("capture-bin");
    write(
        &dir,
        "recyclebin/index.yaml",
        "title: Recycle Bin\ndeleted: []\n",
    );
    write(&dir, "recyclebin/items/notes/old.md", "binned bytes\n");
    write(
        &dir,
        "index.md",
        "---\ntitle: Home\ncontents:\n- notes/a.md\n- notes/photo.jpg.yaml\n\
         recycle_bin: recyclebin/index.yaml\n---\nroot\n",
    );
    let set = block_on(ws(&dir).history_capture_set(Path::new("index.md"))).unwrap();
    assert!(
        set.iter().all(|p| !p.starts_with("recyclebin/items")),
        "binned bytes must not be captured: {set:?}"
    );
    // The bin *index* is captured, though — that is what makes a restore put
    // a live document back as live.
    assert!(
        set.contains(&PathBuf::from("recyclebin/index.yaml")),
        "the bin index is ordinary structural state: {set:?}"
    );
}

// ── Transport simulation ─────────────────────────────────────────────────
//
// The feature's entire claim is surviving an external sync transport, so
// these simulate one: two workspace copies, concurrent captures, and a
// directory merge that unions added files, drops in a `.sync-conflict-…`
// file, and clobbers a shard index.

/// Copy every file under `from` into `to`, adding what is missing and leaving
/// what is already there — the union-of-added-files merge that git, Dropbox,
/// Syncthing and iCloud all perform without conflict.
fn merge_into(from: &Path, to: &Path) {
    fn walk(dir: &Path, base: &Path, to: &Path) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path.strip_prefix(base).unwrap().to_path_buf();
            if path.is_dir() {
                walk(&path, base, to);
            } else if !to.join(&rel).exists() {
                let dest = to.join(&rel);
                std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
                std::fs::copy(&path, &dest).unwrap();
            }
        }
    }
    walk(from, from, to);
}

#[test]
fn concurrent_captures_on_two_devices_merge_without_conflict() {
    // Two devices, same starting state, each captures locally. Because a
    // capture only *adds* files, the transport's union merge produces both
    // events side by side — the whole point of the append-only design.
    let one = seed("transport-one");
    let two = tempdir("transport-two");
    merge_into(&one, &two);

    // Device one edits and captures.
    write(
        &one,
        "notes/a.md",
        "---\ntitle: A\npart_of: '../index.md'\n---\nfrom device one\n",
    );
    let Captured::Written { id: id_one, .. } = capture(&one, "2026-07-31T09:15:22Z", Some("one"))
    else {
        panic!("device one must capture")
    };
    // Device two edits differently and captures — same minute, no coordination.
    write(
        &two,
        "notes/a.md",
        "---\ntitle: A\npart_of: '../index.md'\n---\nfrom device two\n",
    );
    let Captured::Written { id: id_two, .. } = capture(&two, "2026-07-31T09:15:22Z", Some("two"))
    else {
        panic!("device two must capture")
    };
    assert_ne!(id_one, id_two, "different content must mint different ids");

    // The transport reconciles: every added file lands in device one's copy.
    merge_into(&two, &one);

    // Both events survive, and both devices' pre-images are present.
    let ids = event_ids(&one);
    assert!(
        ids.contains(&id_one) && ids.contains(&id_two),
        "a merge must not lose either device's event: {ids:?}"
    );
    for bytes in [b"from device one".as_slice(), b"from device two".as_slice()] {
        let hash = crate::fixity::digest(
            format!(
                "---\ntitle: A\npart_of: '../index.md'\n---\n{}\n",
                String::from_utf8_lossy(bytes)
            )
            .as_bytes(),
        );
        let blob = blob_path(Path::new("history/index.md"), &hash).unwrap();
        assert!(
            one.join(&blob).exists(),
            "both devices' pre-images must survive the merge: {}",
            blob.display()
        );
    }
}

#[test]
fn a_merged_shard_index_is_reported_stale_and_rebuilt_from_its_directory() {
    // The one mutable file in the store is the shard index, so it is the one
    // a transport can mangle. That must be a finding with a mechanical fix,
    // never data loss — which is exactly what "the index is a cache" buys.
    let one = seed("transport-index");
    let two = tempdir("transport-index-two");
    merge_into(&one, &two);

    write(
        &one,
        "notes/a.md",
        "---\ntitle: A\npart_of: '../index.md'\n---\none\n",
    );
    capture(&one, "2026-07-31T09:15:22Z", Some("one"));
    write(
        &two,
        "notes/a.md",
        "---\ntitle: A\npart_of: '../index.md'\n---\ntwo\n",
    );
    capture(&two, "2026-07-31T09:16:00Z", Some("two"));

    // Merge device two's *event* across but let the transport clobber the
    // shard index with device two's copy — which knows nothing of device
    // one's event. This is the realistic damage: last-writer-wins on the
    // only file both devices rewrote.
    merge_into(&two, &one);
    std::fs::copy(
        two.join("history/events/2026/07/index.md"),
        one.join("history/events/2026/07/index.md"),
    )
    .unwrap();
    // …and drop in the conflict copy such a transport leaves behind.
    write(
        &one,
        "history/events/2026/07/index.sync-conflict-20260731-091600.md",
        "---\ntitle: July 2026\n---\nconflicted copy\n",
    );

    // Both events are still listed: `history-list` reads the directories, so
    // a mangled index cannot hide an event that is sitting right there.
    assert_eq!(
        event_ids(&one).len(),
        2,
        "the events are the authority, not the index"
    );

    // `check` names it, and the fix rebuilds that one shard.
    let findings = block_on(ws(&one).check(Path::new("index.md"))).unwrap();
    let stale: Vec<_> = findings
        .iter()
        .filter(|f| matches!(f, Finding::HistoryIndexStale { .. }))
        .collect();
    assert_eq!(stale.len(), 1, "expected one stale shard: {findings:?}");

    let mut w = ws(&one);
    let fix = block_on(w.suggest_fix(stale[0])).unwrap().expect("a fix");
    block_on(w.apply_fix(&fix)).unwrap();

    let after = block_on(ws(&one).check(Path::new("index.md"))).unwrap();
    assert!(
        !after
            .iter()
            .any(|f| matches!(f, Finding::HistoryIndexStale { .. })),
        "the rebuild should have settled the index: {after:?}"
    );
    let rebuilt = read(&one, "history/events/2026/07/index.md");
    for id in event_ids(&one) {
        assert!(
            rebuilt.contains(&id),
            "the rebuilt index must list every event in its directory: {rebuilt}"
        );
    }
}

#[test]
fn a_capture_after_a_merge_records_the_merged_state() {
    // The end-to-end claim: after a transport has done its worst, a capture
    // still runs and still records a consistent cut.
    let one = seed("transport-after");
    let two = tempdir("transport-after-two");
    merge_into(&one, &two);
    capture(&one, "2026-07-31T09:00:00Z", None);
    capture(&two, "2026-07-31T09:00:00Z", None);
    merge_into(&two, &one);

    write(
        &one,
        "notes/a.md",
        "---\ntitle: A\npart_of: '../index.md'\n---\npost-merge\n",
    );
    let outcome = capture(&one, "2026-07-31T10:00:00Z", Some("post-merge"));
    let Captured::Written { id, .. } = outcome else {
        panic!("a post-merge capture must write: {outcome:?}")
    };
    // Its parent is the newest event that existed locally — display metadata,
    // but it should still be recorded.
    let events = block_on(ws(&one).history_list(Path::new("index.md"))).unwrap();
    let latest = events.iter().find(|e| e.id == id).unwrap();
    assert!(latest.parent.is_some(), "a parent should be recorded");
    assert!(
        latest
            .files
            .iter()
            .any(|f| f.path == Path::new("notes/a.md")),
        "the merged state must be in the manifest"
    );
}

// ── Reading one event: `history-show` ────────────────────────────────────

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
fn a_captured_workspace_goes_back_from_its_blobs_without_a_journal_its_size() {
    // What restore will rest on, proved against what Phase 0 actually writes:
    // a manifest plus `blob_path` is enough to stage the whole capture set as
    // copies, and the journal that makes that set crash-atomic is bounded by
    // the file *count*, not by the size of the workspace. Staged as `write`s,
    // this same set would put every byte below into `.prov-journal` first.
    let dir = seed("restore-primitive");
    let payload = "J".repeat(256 * 1024);
    write(&dir, "notes/photo.jpg", &payload);
    let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
        panic!("the first capture must write an event");
    };

    // Damage of the shape a bad merge leaves: bytes clobbered at several paths
    // at once, which is why an event is a consistent cut rather than a file.
    write(&dir, "notes/a.md", "clobbered by a sync conflict");
    write(&dir, "notes/photo.jpg", "truncated");

    let mut w = ws(&dir);
    let event = block_on(w.history_event(Path::new("index.md"), &id))
        .unwrap()
        .unwrap();
    let store_index = Path::new("history/index.md");
    let mut cs = w.change();
    for file in &event.files {
        cs.copy_from(&file.path, blob_path(store_index, &file.hash).unwrap());
    }
    let journal = crate::journal::encode(cs.ops()).unwrap();
    assert!(
        journal.len() < 2048,
        "the journal for a {payload_len}-byte workspace should be paths only, \
         got {journal_len} bytes",
        payload_len = payload.len(),
        journal_len = journal.len()
    );
    block_on(w.commit(cs)).unwrap();

    // Byte-exact at every captured path — checked against the manifest's own
    // hashes, which is the only claim a restore actually owes.
    for file in &event.files {
        let bytes = std::fs::read(dir.join(&file.path)).unwrap();
        assert_eq!(
            crate::fixity::digest(&bytes),
            file.hash,
            "{} did not come back byte-exact",
            file.path.display()
        );
    }
    assert_eq!(read(&dir, "notes/photo.jpg").len(), payload.len());
}

// ── Prune ────────────────────────────────────────────────────────────────

/// Plan and run a prune, the sequence the CLI performs.
fn prune(dir: &Path, retention: &Retention) -> Pruned {
    let mut w = ws(dir);
    let root = Path::new("index.md");
    let plan = block_on(w.history_prune_plan(root, retention)).unwrap();
    block_on(w.history_prune(root, &plan)).unwrap();
    plan
}

/// Capture with `notes/a.md` rewritten first, so each capture has something to
/// record — and so the untouched files keep sharing the blob they already
/// parked.
fn capture_edited(dir: &Path, now: &str, label: &str, body: &str) -> String {
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

#[test]
fn a_prune_drops_the_oldest_and_collects_only_what_nothing_still_references() {
    let dir = seed("prune-basic");
    let first = capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
    let second = capture_edited(&dir, "2026-07-31T10:00:00.000000Z", "two", "beta");
    let third = capture_edited(&dir, "2026-07-31T11:00:00.000000Z", "three", "gamma");

    // The blob only the dropped events name, and one every event names — the
    // whole correctness question a GC has to get right.
    let dropped_bytes = blob_path(
        Path::new("history/index.md"),
        &crate::fixity::digest(b"---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n"),
    )
    .unwrap();
    let shared_bytes = blob_path(
        Path::new("history/index.md"),
        &crate::fixity::digest(b"JPEGBYTES"),
    )
    .unwrap();
    assert!(dir.join(&dropped_bytes).exists() && dir.join(&shared_bytes).exists());

    let plan = prune(&dir, &Retention::Keep(1));
    assert_eq!(plan.events, vec![first, second]);
    assert_eq!(plan.keeping, 1);
    assert!(plan.bytes > 0, "the report has to name what it freed");

    assert!(
        !dir.join(&dropped_bytes).exists(),
        "bytes only the dropped events named must go"
    );
    assert!(
        dir.join(&shared_bytes).exists(),
        "bytes a surviving manifest still names must not"
    );

    // The store is valid, and the surviving event is still a complete
    // recovery point — which is the property that makes prune safe at all.
    assert_eq!(
        block_on(ws(&dir).check(Path::new("index.md"))).unwrap(),
        vec![]
    );
    let events = block_on(ws(&dir).history_list(Path::new("index.md"))).unwrap();
    assert_eq!(
        events.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        vec![third.as_str()]
    );
    let survivor = &events[0];
    assert!(
        block_on(ws(&dir).history_missing_blobs(Path::new("index.md"), survivor))
            .unwrap()
            .is_empty(),
        "every row of a surviving event must still have its bytes"
    );
}

#[test]
fn a_prune_also_collects_the_orphans_that_were_already_there() {
    // `HistoryBlobOrphaned` points at this verb, so the two have to agree on
    // what an orphan is. They share the sweep, and this is the assertion that
    // says so.
    let dir = seed("prune-orphans");
    capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
    write(&dir, "history/blobs/ab/sync-conflict-20260731", "junk");

    let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
    assert!(matches!(
        findings.as_slice(),
        [Finding::HistoryBlobOrphaned { blobs, .. }]
            if blobs == &[PathBuf::from("history/blobs/ab/sync-conflict-20260731")]
    ));

    // Keeping every event still collects it: the sweep is "what nothing
    // references", not "what this drop orphaned".
    let plan = prune(&dir, &Retention::Keep(10));
    assert!(plan.events.is_empty());
    assert_eq!(
        plan.blobs,
        vec![PathBuf::from("history/blobs/ab/sync-conflict-20260731")]
    );
    assert_eq!(
        block_on(ws(&dir).check(Path::new("index.md"))).unwrap(),
        vec![]
    );
}

#[test]
fn an_emptied_shard_leaves_no_index_and_no_finding() {
    let dir = seed("prune-shards");
    capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "july", "alpha");
    capture_edited(&dir, "2026-08-01T09:00:00.000000Z", "august", "beta");
    assert!(dir.join("history/events/2026/07/index.md").exists());

    // Drop July: its shard index goes with it, but the year survives because
    // August is still there.
    prune(&dir, &Retention::Before("2026-08-01".into()));
    assert!(!dir.join("history/events/2026/07/index.md").exists());
    assert!(dir.join("history/events/2026/index.md").exists());
    assert!(dir.join("history/events/2026/08/index.md").exists());
    assert_eq!(
        block_on(ws(&dir).check(Path::new("index.md"))).unwrap(),
        vec![]
    );

    // Now the year, too. A change set removes files rather than directories,
    // so `2026/07/` is still sitting there — and must be invisible, not a
    // permanent finding about an index that should not exist.
    prune(&dir, &Retention::Keep(0));
    assert!(!dir.join("history/events/2026/index.md").exists());
    assert!(
        dir.join("history/events/2026/07").is_dir(),
        "the empty directory is expected to linger"
    );
    assert_eq!(
        block_on(ws(&dir).check(Path::new("index.md"))).unwrap(),
        vec![],
        "an event-less directory is not a shard"
    );

    // …and the store still works: a later capture rebuilds the tree around it.
    capture_edited(&dir, "2026-09-01T09:00:00.000000Z", "after", "delta");
    assert_eq!(
        block_on(ws(&dir).check(Path::new("index.md"))).unwrap(),
        vec![]
    );
}

#[test]
fn a_date_cutoff_keeps_the_day_it_names_and_a_typo_drops_nothing() {
    let dir = seed("prune-before");
    capture_edited(&dir, "2026-07-31T23:59:59.999999Z", "eve", "alpha");
    let boundary = capture_edited(&dir, "2026-08-01T00:00:00.000000Z", "dawn", "beta");
    let later = capture_edited(&dir, "2026-08-02T09:00:00.000000Z", "later", "gamma");

    // "before 2026-08-01" means before that day *started*: a bare date is a
    // prefix of every timestamp in its day, which is what makes the boundary
    // read the way a person means it without parsing a calendar.
    let w = ws(&dir);
    let plan = block_on(w.history_prune_plan(
        Path::new("index.md"),
        &Retention::Before("2026-08-01".into()),
    ))
    .unwrap();
    assert_eq!(plan.keeping, 2);
    assert!(!plan.events.contains(&boundary) && !plan.events.contains(&later));

    // A cutoff that is not a date deletes nothing rather than everything.
    let typo = block_on(w.history_prune_plan(
        Path::new("index.md"),
        &Retention::Before("yesterday".into()),
    ));
    assert!(typo.is_err(), "a typo must not be a silent full sweep");
}

#[test]
fn a_prune_with_nothing_to_drop_touches_no_file() {
    let dir = seed("prune-noop");
    capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
    let index = read(&dir, "history/events/2026/07/index.md");
    let before = std::fs::metadata(dir.join("history/index.md"))
        .unwrap()
        .modified()
        .unwrap();

    let plan = prune(&dir, &Retention::Keep(5));
    assert!(plan.is_empty());
    // Every index a prune touches is a file some transport has to carry, so
    // one with nothing to do must not churn them.
    assert_eq!(read(&dir, "history/events/2026/07/index.md"), index);
    assert_eq!(
        std::fs::metadata(dir.join("history/index.md"))
            .unwrap()
            .modified()
            .unwrap(),
        before
    );
}

// ── Forget ───────────────────────────────────────────────────────────────

fn forget(dir: &Path, subject: &Subject, now: &str, force: bool) -> Result<Forgotten> {
    block_on(ws(dir).history_forget(Path::new("index.md"), subject, now, force))
}

fn blob_of(bytes: &[u8]) -> PathBuf {
    blob_path(Path::new("history/index.md"), &crate::fixity::digest(bytes)).unwrap()
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
    let events = block_on(ws(&dir).history_list(Path::new("index.md"))).unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.files.iter().any(|f| f.path == Path::new("notes/twin.md"))),
        "events are immutable: the manifest still names it"
    );

    // Tombstoned, reachable, and clean — the record must not itself be an
    // orphan, and a deliberate destruction must not leave `check` failing.
    let tombstone = read(&dir, "history/forgotten.yaml");
    assert!(tombstone.contains("notes/twin.md") && tombstone.contains("2026-08-01T12:00:00"));
    assert!(read(&dir, "history/index.md").contains("forgotten.yaml"));
    assert_eq!(
        block_on(ws(&dir).check(Path::new("index.md"))).unwrap(),
        vec![]
    );
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
    assert!(
        block_on(ws(&dir).check(Path::new("index.md")))
            .unwrap()
            .is_empty(),
        "a recorded destruction is not a finding — a `check` that never came \
         back to clean would teach the user to stop reading it"
    );
    assert_eq!(
        block_on(ws(&dir).history_forgotten(Path::new("index.md")))
            .unwrap()
            .len(),
        1
    );

    // …and the suppression is precise, not blanket: bytes that went missing
    // without a record still say so.
    std::fs::remove_file(dir.join(blob_of(b"JPEGBYTES"))).unwrap();
    let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
    assert!(
        matches!(findings.as_slice(), [Finding::HistoryBlobMissing { paths, .. }]
            if paths == &[PathBuf::from("notes/photo.jpg")]),
        "{findings:?}"
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
    let mut w = ws(&dir);
    let id = Id("b7k2m".into());
    w.index_mut().register(&id, Path::new("notes/a.md"));
    block_on(w.history_capture(Path::new("index.md"), "2026-07-31T09:00:00.000000Z", None))
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
    w.index_mut().set_path(&id, Path::new("notes/b.md"));
    block_on(w.history_capture(Path::new("index.md"), "2026-07-31T10:00:00.000000Z", None))
        .unwrap();

    // Out of the workspace, so the guard is not what is under test.
    std::fs::remove_file(dir.join("notes/b.md")).unwrap();
    relink_live(&dir, &["notes/photo.jpg.yaml"]);
    w.index_mut().unregister(&id);

    let out = block_on(w.history_forget(
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

// ── An unreadable event must refuse destruction, not silently drop it ────

/// Corrupt an event document the way a sync transport actually does: a
/// conflict lands **inside** the frontmatter fence rather than beside it —
/// `a_transport_conflict_copy_is_not_mistaken_for_an_event` covers the
/// filename shape; this is the one no filename check can catch. The result
/// still has an event-shaped filename, so [`shard_event_ids`] finds it, but
/// nothing in its content parses any more.
fn tear(dir: &Path, rel: &str) {
    let text = read(dir, rel);
    let mangled = text.replacen("---\n", "---\n<<<<<<< ours\n=======\n>>>>>>> theirs\n", 1);
    write(dir, rel, &mangled);
}

#[test]
fn a_prune_refuses_while_any_event_is_unreadable() {
    // The bug this guards: a `referenced` set built only from the events
    // that parsed treats the torn event's blobs as unclaimed, and a prune
    // would collect and delete them — permanent loss from a bound that
    // silently dropped a whole event's worth of references.
    let dir = seed("prune-torn");
    let first = capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
    capture_edited(&dir, "2026-07-31T10:00:00.000000Z", "two", "beta");
    let torn = event_path(Path::new("history/index.md"), &first, "md").unwrap();
    tear(&dir, torn.to_str().unwrap());

    let w = ws(&dir);
    let err =
        block_on(w.history_prune_plan(Path::new("index.md"), &Retention::Keep(1))).unwrap_err();
    assert!(
        err.to_string().contains(torn.to_str().unwrap()),
        "the refusal has to name the file that could not be read: {err}"
    );

    // Refused before a plan even exists — nothing on disk moved.
    assert!(dir.join(&torn).exists());
    assert!(
        dir.join(blob_of(
            b"---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n"
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

#[test]
fn check_reports_an_unreadable_event_and_never_recommends_pruning_its_blobs() {
    // The promise docs/history-format.md §7 makes and the codebase did not
    // keep: an event document that fails to parse is a plain `Unreadable`.
    // And the other half of the bug: while it is unreadable, its blobs must
    // not be reported `HistoryBlobOrphaned` — that finding's own message
    // points straight at `history-prune`, so a false orphan here is a
    // diagnostic recommending the destructive verb the two tests above
    // refuse to run.
    let dir = seed("check-torn");
    let first = capture_edited(&dir, "2026-07-31T09:00:00.000000Z", "one", "alpha");
    let torn = event_path(Path::new("history/index.md"), &first, "md").unwrap();
    tear(&dir, torn.to_str().unwrap());

    let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
    assert!(
        findings
            .iter()
            .any(|f| matches!(f, Finding::Unreadable { doc, .. } if doc == &torn)),
        "missing the promised finding for {}: {findings:?}",
        torn.display()
    );
    assert!(
        !findings
            .iter()
            .any(|f| matches!(f, Finding::HistoryBlobOrphaned { .. })),
        "a torn event's blobs must not be reported as orphans: {findings:?}"
    );

    // Reading `check` must not be what destroys the bytes: the blob only
    // this (now unreadable) event named is still exactly where it was.
    assert!(
        dir.join(blob_of(
            b"---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n"
        ))
        .exists()
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

// ── Lineage: `history-log` ───────────────────────────────────────────────

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

// ── Restore ──────────────────────────────────────────────────────────────

/// [`relink`], keeping the `history` pointer.
///
/// `relink` writes the root a workspace had *before* its store existed, which
/// is what the lineage tests want (a capture follows, and re-bootstraps the
/// pointer). A restore has no such capture behind it: strip the pointer and
/// the store is simply not there to restore from.
fn relink_live(dir: &Path, contents: &[&str]) {
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

/// Plan and run a restore in one go, on a workspace of the caller's choosing —
/// the sequence the CLI performs, so a test exercises the shipped path rather
/// than a convenient shortcut past it.
fn restore(
    w: &mut Workspace<StdFs, Minter, FileIndex>,
    id: &str,
    scope: &Scope,
    exact: bool,
    force: bool,
) -> Result<RestorePlan> {
    let root = Path::new("index.md");
    let event = block_on(w.history_event(root, id))?.expect("the event should be in the store");
    let plan = block_on(w.history_restore_plan(root, &event, scope, exact))?;
    block_on(w.history_restore(root, &plan, force))?;
    Ok(plan)
}

fn dispositions(plan: &RestorePlan, want: Disposition) -> Vec<&Path> {
    plan.ops
        .iter()
        .filter(|op| op.disposition == want)
        .map(|op| op.path.as_path())
        .collect()
}

#[test]
fn a_restore_puts_the_whole_consistent_cut_back_byte_exact() {
    let dir = seed("restore-cut");
    let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
        panic!("the first capture must write an event");
    };

    // Damage of the shape a bad merge leaves: several files at once, which is
    // why an event is a consistent cut rather than a file. One of them is the
    // parent's child list — the structural half a per-file undo would miss.
    write(&dir, "notes/a.md", "clobbered by a sync conflict");
    write(&dir, "notes/photo.jpg", "truncated");
    relink_live(&dir, &["notes/photo.jpg.yaml"]);

    let mut w = ws(&dir);
    let plan = restore(&mut w, &id, &Scope::Whole, false, false).unwrap();
    assert_eq!(
        dispositions(&plan, Disposition::Overwrite),
        vec![
            Path::new("index.md"),
            Path::new("notes/a.md"),
            Path::new("notes/photo.jpg")
        ]
    );
    // The sidecar was never touched, so the restore has nothing to say about
    // it — and says so, rather than rewriting bytes that already match.
    assert_eq!(
        dispositions(&plan, Disposition::Unchanged),
        vec![Path::new("notes/photo.jpg.yaml")]
    );

    let event = block_on(w.history_event(Path::new("index.md"), &id))
        .unwrap()
        .unwrap();
    for file in &event.files {
        let bytes = std::fs::read(dir.join(&file.path)).unwrap();
        assert_eq!(
            crate::fixity::digest(&bytes),
            file.hash,
            "{} did not come back byte-exact",
            file.path.display()
        );
    }
    assert!(read(&dir, "index.md").contains("notes/a.md"));
    assert!(
        block_on(ws(&dir).check(Path::new("index.md")))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_default_restore_deletes_nothing_and_exact_makes_the_tree_match() {
    let dir = seed("restore-exact");
    let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
        panic!("the first capture must write an event");
    };

    // What a sync transport actually does: leaves a second file behind, linked
    // into the graph. Writing captured bytes over the top does not remove it —
    // which is the gap `--exact` exists to close, and why the default leaving
    // it is a deliberate decision rather than an oversight.
    write(
        &dir,
        "notes/a.sync-conflict-20260731.md",
        "---\ntitle: A (conflicted copy)\npart_of: '../index.md'\n---\nalpha\n",
    );
    relink_live(
        &dir,
        &[
            "notes/a.md",
            "notes/a.sync-conflict-20260731.md",
            "notes/photo.jpg.yaml",
        ],
    );

    // Both plans off the same damaged tree, so what differs between them is the
    // flag and nothing else. Taken before either runs, because the delete set is
    // drawn from the *reachable* files: the restored root stops linking the
    // conflict copy, and a plan computed afterwards would no longer see it.
    let mut w = ws(&dir);
    let root = Path::new("index.md");
    let event = block_on(w.history_event(root, &id)).unwrap().unwrap();
    let additive = block_on(w.history_restore_plan(root, &event, &Scope::Whole, false)).unwrap();
    let exact = block_on(w.history_restore_plan(root, &event, &Scope::Whole, true)).unwrap();

    assert_eq!(additive.count(Disposition::Remove), 0);
    block_on(w.history_restore(root, &additive, false)).unwrap();
    assert!(
        dir.join("notes/a.sync-conflict-20260731.md").exists(),
        "the default restore must delete nothing"
    );

    assert_eq!(
        exact.removals().collect::<Vec<_>>(),
        vec![Path::new("notes/a.sync-conflict-20260731.md")]
    );
    block_on(w.history_restore(root, &exact, false)).unwrap();
    assert!(!dir.join("notes/a.sync-conflict-20260731.md").exists());

    // The one subtree the mechanism is blind to survives its own exact
    // restore: an event that deleted every event newer than it would destroy
    // the recovery points themselves.
    let event = event_path(Path::new("history/index.md"), &id, "md").unwrap();
    assert!(dir.join(event).exists(), "the store must survive --exact");
    assert!(dir.join("history/blobs").exists());
}

#[test]
fn restoring_the_state_the_workspace_already_holds_writes_nothing() {
    let dir = seed("restore-noop");
    let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
        panic!("the first capture must write an event");
    };
    let before = std::fs::metadata(dir.join("notes/a.md"))
        .unwrap()
        .modified()
        .unwrap();

    let mut w = ws(&dir);
    let plan = restore(&mut w, &id, &Scope::Whole, false, false).unwrap();
    assert!(plan.is_noop(), "every row already matches the capture");
    assert_eq!(plan.count(Disposition::Unchanged), plan.ops.len());
    assert_eq!(
        std::fs::metadata(dir.join("notes/a.md"))
            .unwrap()
            .modified()
            .unwrap(),
        before,
        "an unchanged row must not be rewritten"
    );
}

#[test]
fn a_row_whose_blob_never_arrived_is_skipped_by_name_not_fatal() {
    // A manifest and the blobs it names travel over a transport separately, so
    // a half-synced event is ordinary rather than broken. The rows prov *can*
    // supply still come back; the one it cannot is reported.
    let dir = seed("restore-halfsynced");
    let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
        panic!("the first capture must write an event");
    };
    let payload = crate::fixity::digest(b"JPEGBYTES");
    std::fs::remove_file(dir.join(blob_path(Path::new("history/index.md"), &payload).unwrap()))
        .unwrap();
    write(&dir, "notes/a.md", "clobbered");
    write(&dir, "notes/photo.jpg", "truncated");

    let mut w = ws(&dir);
    let plan = restore(&mut w, &id, &Scope::Whole, false, false).unwrap();
    assert_eq!(
        dispositions(&plan, Disposition::NoBytes),
        vec![Path::new("notes/photo.jpg")]
    );
    assert_eq!(
        read(&dir, "notes/a.md"),
        "---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n"
    );
    assert_eq!(
        read(&dir, "notes/photo.jpg"),
        "truncated",
        "a row with no bytes is left alone, not emptied"
    );

    // Under `--exact` the same event is refused nothing: the delete pass is
    // drawn from the manifest's paths, and a row it cannot supply is still a
    // path the manifest holds — so nothing is removed on the strength of bytes
    // that merely have not arrived.
    let mut w = ws(&dir);
    let exact = restore(&mut w, &id, &Scope::Whole, true, false).unwrap();
    assert_eq!(exact.count(Disposition::Remove), 0);
    assert!(dir.join("notes/photo.jpg").exists());
}

#[test]
fn a_restore_refuses_to_displace_a_registration_unless_it_resolves_it_itself() {
    let dir = seed("restore-collision");
    let mut w = ws(&dir);
    let id = Id("b7k2m".into());
    w.index_mut().register(&id, Path::new("notes/a.md"));
    let Captured::Written { id: event, .. } =
        block_on(w.history_capture(Path::new("index.md"), "2026-07-31T09:15:22Z", None)).unwrap()
    else {
        panic!("the first capture must write an event");
    };

    // The document moved after the capture. Restoring additively would put the
    // old path back and leave the new one there — two documents spelling one
    // id, which only their author can arbitrate.
    std::fs::rename(dir.join("notes/a.md"), dir.join("notes/b.md")).unwrap();
    relink_live(&dir, &["notes/b.md", "notes/photo.jpg.yaml"]);
    w.index_mut().set_path(&id, Path::new("notes/b.md"));

    let ev = block_on(w.history_event(Path::new("index.md"), &event))
        .unwrap()
        .unwrap();
    let plan =
        block_on(w.history_restore_plan(Path::new("index.md"), &ev, &Scope::Whole, false)).unwrap();
    assert_eq!(plan.conflicts.len(), 1);
    assert_eq!(plan.conflicts[0].path, Path::new("notes/a.md"));
    assert!(matches!(
        plan.conflicts[0].collision,
        Collision::Id { ref held_by, .. } if held_by == Path::new("notes/b.md")
    ));
    let err = block_on(w.history_restore(Path::new("index.md"), &plan, false)).unwrap_err();
    assert!(matches!(err, Error::Collision(Collision::Id { .. })));
    assert!(
        !dir.join("notes/a.md").exists(),
        "a refused restore must move nothing"
    );

    // `--exact` removes the document currently holding the id, so nothing is
    // displaced and the same restore is no longer a collision at all. This is
    // the difference between "put these bytes back too" and "make the tree
    // match this capture".
    let exact =
        block_on(w.history_restore_plan(Path::new("index.md"), &ev, &Scope::Whole, true)).unwrap();
    assert!(
        exact.conflicts.is_empty(),
        "a collision the restore itself resolves is not a collision: {:?}",
        exact.conflicts
    );
    assert_eq!(
        exact.removals().collect::<Vec<_>>(),
        vec![Path::new("notes/b.md")]
    );
    block_on(w.history_restore(Path::new("index.md"), &exact, false)).unwrap();
    assert!(dir.join("notes/a.md").exists());
    assert!(!dir.join("notes/b.md").exists());
}

#[test]
fn a_scope_restores_a_slice_and_refuses_what_the_capture_never_held() {
    let dir = seed("restore-scope");
    let mut w = ws(&dir);
    let id = Id("b7k2m".into());
    w.index_mut().register(&id, Path::new("notes/a.md"));
    let Captured::Written { id: event, .. } =
        block_on(w.history_capture(Path::new("index.md"), "2026-07-31T09:15:22Z", None)).unwrap()
    else {
        panic!("the first capture must write an event");
    };
    write(&dir, "notes/a.md", "clobbered");
    write(&dir, "notes/photo.jpg", "truncated");

    // A directory scope takes everything the capture held beneath it; the root
    // above it is left alone.
    let ev = block_on(w.history_event(Path::new("index.md"), &event))
        .unwrap()
        .unwrap();
    let plan = block_on(w.history_restore_plan(
        Path::new("index.md"),
        &ev,
        &Scope::Paths(vec![PathBuf::from("notes")]),
        false,
    ))
    .unwrap();
    assert_eq!(plan.ops.len(), 3, "the three files under notes/");
    assert!(!plan.ops.iter().any(|op| op.path == Path::new("index.md")));

    // An id scope reaches the one document, wherever the capture found it.
    let by_id =
        block_on(w.history_restore_plan(Path::new("index.md"), &ev, &Scope::Id(id.clone()), false))
            .unwrap();
    assert_eq!(
        by_id
            .ops
            .iter()
            .map(|op| op.path.as_path())
            .collect::<Vec<_>>(),
        vec![Path::new("notes/a.md")]
    );
    block_on(w.history_restore(Path::new("index.md"), &by_id, false)).unwrap();
    assert!(read(&dir, "notes/a.md").contains("alpha"));
    assert_eq!(
        read(&dir, "notes/photo.jpg"),
        "truncated",
        "a scope restores only what it names"
    );

    // A scope that selects nothing is a typo, not an empty restore.
    for scope in [
        Scope::Paths(vec![PathBuf::from("notes/never.md")]),
        Scope::Id(Id("nosuch".into())),
    ] {
        assert!(
            block_on(w.history_restore_plan(Path::new("index.md"), &ev, &scope, false)).is_err()
        );
    }

    // And `exact` is a statement about the whole tree, which a slice of the
    // capture cannot make.
    assert!(
        block_on(w.history_restore_plan(
            Path::new("index.md"),
            &ev,
            &Scope::Paths(vec![PathBuf::from("notes")]),
            true,
        ))
        .is_err()
    );
}

#[test]
fn a_restored_root_never_strands_the_store_unreachable() {
    // A capture always records a root that already declares the store, so this
    // is the hand-edited (or foreign) case: a manifest whose root predates the
    // pointer. Restoring it verbatim would leave `history/` unreachable —
    // invisible to `check`, and unfindable by the next restore.
    let dir = seed("restore-pointer");
    let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
        panic!("the first capture must write an event");
    };
    let rootless = "---\ntitle: Home\ncontents:\n- notes/a.md\n- notes/photo.jpg.yaml\n---\nroot\n";
    let hash = crate::fixity::digest(rootless.as_bytes());
    let blob = blob_path(Path::new("history/index.md"), &hash).unwrap();
    std::fs::create_dir_all(dir.join(&blob).parent().unwrap()).unwrap();
    std::fs::write(dir.join(&blob), rootless).unwrap();

    let mut w = ws(&dir);
    let mut event = block_on(w.history_event(Path::new("index.md"), &id))
        .unwrap()
        .unwrap();
    for file in &mut event.files {
        if file.path == Path::new("index.md") {
            file.hash = hash.clone();
        }
    }
    let plan =
        block_on(w.history_restore_plan(Path::new("index.md"), &event, &Scope::Whole, false))
            .unwrap();
    block_on(w.history_restore(Path::new("index.md"), &plan, false)).unwrap();

    let root = read(&dir, "index.md");
    assert!(
        root.contains("history:"),
        "a restored root must still declare the store: {root}"
    );
    assert!(
        block_on(ws(&dir).history_path(Path::new("index.md")))
            .unwrap()
            .is_some()
    );
}

// ── Case-fold identity: the probe and the `exact` removal set agreeing ────

/// Whether `dir` sits on a filesystem that folds ASCII case for path
/// lookups — probed empirically (this suite runs on APFS in development
/// and ext4 in CI, and the two disagree) rather than assumed from
/// `cfg(target_os)`, mirroring the production probe this exercises
/// ([`Workspace::filesystem_case_folds`]). Every test below that depends on
/// case-folding actually happening skips its case-insensitive-only
/// assertions when this is `false`, so the suite stays green on Linux CI.
fn case_insensitive_fs(dir: &Path) -> bool {
    let probe = dir.join(".case-probe.tmp");
    std::fs::write(&probe, b"x").unwrap();
    let collides = dir.join(".CASE-PROBE.tmp").exists();
    let _ = std::fs::remove_file(&probe);
    collides
}

/// The literal on-disk spelling of `rel`'s final component, read straight
/// from its parent directory's listing — the same thing a restore's own
/// [`Workspace::on_disk_identity`] reads, so a test can assert *which*
/// casing survived rather than merely that *a* casing did.
fn literal_name(dir: &Path, rel: &str) -> String {
    let rel = Path::new(rel);
    let entries = std::fs::read_dir(dir.join(rel.parent().unwrap())).unwrap();
    let want = rel.file_name().unwrap().to_string_lossy();
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|name| name.eq_ignore_ascii_case(&want))
        .unwrap_or_else(|| panic!("no entry named {want} (any case) in {}", rel.display()))
}

#[test]
fn an_exact_restore_spares_and_recases_a_row_that_only_differs_from_disk_by_case() {
    let dir = seed("restore-case-exact");
    let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
        panic!("the first capture must write an event");
    };
    if !case_insensitive_fs(&dir) {
        return;
    }

    // A sync client — or a user in Finder — renamed the file to a
    // different case after the capture. The manifest still spells it
    // `notes/a.md`. Restoring that old event with `--exact` is the exact
    // shape of the data-loss bug: the disposition probe used to find this
    // row `Unchanged` through the filesystem's own folding, while the
    // removal pass compared paths as literal strings and queued the very
    // same file for `Remove` — so the run deleted it and neither spelling
    // survived.
    std::fs::rename(dir.join("notes/a.md"), dir.join("notes/A.md")).unwrap();

    let mut w = ws(&dir);
    let plan = restore(&mut w, &id, &Scope::Whole, true, false).unwrap();

    assert_eq!(
        plan.removals().collect::<Vec<_>>(),
        Vec::<&Path>::new(),
        "a case-only rename must never be planned for removal under --exact"
    );
    assert_eq!(
        dispositions(&plan, Disposition::CaseOnly),
        vec![Path::new("notes/a.md")],
        "the bytes already matched; only the on-disk name's case did not"
    );
    assert_eq!(
        literal_name(&dir, "notes/a.md"),
        "a.md",
        "restore renames the file to the manifest's own spelling"
    );
    assert_eq!(
        read(&dir, "notes/a.md"),
        "---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n"
    );
}

#[test]
fn an_additive_restore_recases_the_old_spelling_instead_of_silently_doing_nothing() {
    let dir = seed("restore-case-additive");
    let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
        panic!("the first capture must write an event");
    };
    if !case_insensitive_fs(&dir) {
        return;
    }
    std::fs::rename(dir.join("notes/a.md"), dir.join("notes/A.md")).unwrap();

    // The other edge the same bug left behind: without `--exact`, the old
    // probe found this row `Unchanged` and wrote nothing at all, so the
    // manifest's own spelling never came back — a restore that silently
    // no-ops on a row it was actually asked to restore.
    let mut w = ws(&dir);
    let plan = restore(&mut w, &id, &Scope::Whole, false, false).unwrap();

    assert!(
        !plan.is_noop(),
        "a case-only rename is a real change the plan must report, not silence"
    );
    assert_eq!(
        dispositions(&plan, Disposition::CaseOnly),
        vec![Path::new("notes/a.md")]
    );
    assert_eq!(literal_name(&dir, "notes/a.md"), "a.md");
}

#[test]
fn an_overwrite_recases_too_when_the_on_disk_content_also_changed() {
    let dir = seed("restore-case-overwrite");
    let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
        panic!("the first capture must write an event");
    };
    if !case_insensitive_fs(&dir) {
        return;
    }
    std::fs::rename(dir.join("notes/a.md"), dir.join("notes/A.md")).unwrap();
    write(&dir, "notes/A.md", "clobbered by a sync conflict");

    let mut w = ws(&dir);
    let plan = restore(&mut w, &id, &Scope::Whole, false, false).unwrap();

    assert_eq!(
        dispositions(&plan, Disposition::Overwrite),
        vec![Path::new("notes/a.md")]
    );
    assert_eq!(
        literal_name(&dir, "notes/a.md"),
        "a.md",
        "an overwrite must fix the casing too, not just the content"
    );
    assert_eq!(
        read(&dir, "notes/a.md"),
        "---\ntitle: A\npart_of: '../index.md'\n---\nalpha\n"
    );
}

#[test]
fn a_foreign_event_naming_two_paths_that_differ_only_by_case_is_refused_exactly_where_the_filesystem_would_self_clobber()
 {
    // A manifest naming both spellings is a state only a case-sensitive
    // filesystem can capture — a normal capture here could never observe
    // both paths reachable at once. Simulated directly on the event rather
    // than on disk, since this filesystem could not produce it either.
    let dir = seed("restore-case-foreign");
    let Captured::Written { id, .. } = capture(&dir, "2026-07-31T09:15:22Z", None) else {
        panic!("the first capture must write an event");
    };
    let w = ws(&dir);
    let mut event = block_on(w.history_event(Path::new("index.md"), &id))
        .unwrap()
        .unwrap();
    event.files.push(entry("notes/A.md", b"a different alpha"));

    let result =
        block_on(w.history_restore_plan(Path::new("index.md"), &event, &Scope::Whole, false));
    if case_insensitive_fs(&dir) {
        assert!(
            result.is_err(),
            "a case-colliding manifest must be refused on a filesystem that \
             folds case — writing the second row would silently clobber the first"
        );
    } else {
        // The whole point: this fix must change nothing on a filesystem
        // that does not fold case, where the two paths are simply two
        // ordinary, unrelated files.
        assert!(
            result.is_ok(),
            "must not refuse on a filesystem that does not fold case: {result:?}"
        );
    }
}
