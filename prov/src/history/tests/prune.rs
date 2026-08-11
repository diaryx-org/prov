use std::path::{Path, PathBuf};

use super::support::*;
use crate::validate::Finding;
use prov_graph::exec::block_on;
use prov_history::*;

/// Plan and run a prune, the sequence the CLI performs.
fn prune(dir: &Path, retention: &Retention) -> Pruned {
    let mut w = ws(dir);
    let root = Path::new("index.md");
    let plan = block_on(w.history_prune_plan(root, retention)).unwrap();
    block_on(w.history_prune(root, &plan)).unwrap();
    plan
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
