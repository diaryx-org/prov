//! The store side, against the real thing.
//!
//! Every test here builds an actual historica store with the historica
//! library — `init`, `record`, the store's own `skipped.txt` — because the
//! store's formats are the other party to this crate's contract. What is
//! asserted is the whole loop: the region lands between historica's defaults
//! and the person's own lines, recording takes exactly what the graph
//! reaches, and a settled workspace plans a settled plan.

use std::path::Path;

use historica::record::{Clock, Platform, Recording};
use historica::store::Store;
use historica::working::Working;

use super::support::*;
use crate::{Standing, apply};

fn init_store(dir: &Path) {
    Store::init(dir.join("history")).unwrap();
}

/// Record everything the store's `skipped.txt` currently allows, as the
/// historica command would.
fn record_all(dir: &Path) -> Vec<String> {
    let mut store = Store::open(dir.join("history")).unwrap();
    let skipped = store.skipped().clone();
    let working = Working::read(dir, &skipped).unwrap();
    let history = store.history();
    let superseded = history.superseded();
    let parents: Vec<_> = history
        .heads()
        .into_iter()
        .filter(|head| !superseded.contains(head))
        .collect();
    let mut platform = Platform;
    let recorded = historica::record::record(
        &mut store,
        &working,
        &Recording {
            parents,
            author: "A Test <test@example.com>".to_owned(),
            when: platform.now().unwrap(),
            message: "recorded by the skiplist tests".to_owned(),
            moves: Vec::new(),
            at: Vec::new(),
        },
        &mut platform,
    )
    .unwrap();
    let tree = store.tree(&recorded.revision).unwrap();
    let mut paths: Vec<String> = tree.files().map(|(_, path)| path.to_owned()).collect();
    paths.sort();
    paths
}

#[test]
fn reading_a_workspace_without_a_store_is_an_error_not_an_empty_answer() {
    let dir = tempdir("no-store");
    write(&dir, "index.md", "root");

    assert!(Standing::read(&dir).is_err());
}

#[test]
fn a_fresh_store_stands_with_defaults_in_hand_and_nothing_else() {
    let dir = tempdir("fresh-standing");
    write(&dir, "index.md", "root");
    init_store(&dir);

    let standing = Standing::read(&dir).unwrap();

    // historica's own defaults are hand rules — a person may delete them, and
    // this crate never rewrites them.
    assert_eq!(standing.hand.len(), 3);
    assert!(standing.region.is_empty());
    assert!(standing.tracked.is_empty());
}

#[test]
fn the_region_lands_after_the_defaults_and_leaves_them_untouched() {
    let dir = tempdir("region-lands");
    write(&dir, "index.md", "root");
    write(&dir, "loose.md", "unlinked");
    init_store(&dir);
    let before = read(&dir, "history/skipped.txt");

    let host = TestHost::new(&dir).reaches(&["index.md"]);
    let plan = plan_against(&host, &Standing::read(&dir).unwrap());
    apply(&dir, &plan).unwrap();

    let after = read(&dir, "history/skipped.txt");
    assert!(after.starts_with(&before), "the defaults moved:\n{after}");
    assert!(after.contains("# prov:begin"));
    assert!(after.contains("\nskip loose.md\n"));
    assert!(after.trim_end().ends_with("# prov:end"));
    // What was written is a file historica itself reads back.
    assert!(Store::open(dir.join("history")).unwrap().skipped().len() > 3);
}

#[test]
fn reapplying_rewrites_the_region_and_only_the_region() {
    let dir = tempdir("region-only");
    write(&dir, "index.md", "root");
    write(&dir, "loose.md", "unlinked");
    init_store(&dir);

    let host = TestHost::new(&dir).reaches(&["index.md"]);
    apply(&dir, &plan_against(&host, &Standing::read(&dir).unwrap())).unwrap();

    // The person adds a rule of their own — after the region, the way
    // `historica skip` appends.
    let mut text = read(&dir, "history/skipped.txt");
    text.push_str("skip by-hand.md\n");
    std::fs::write(dir.join("history/skipped.txt"), &text).unwrap();

    // The file changes shape: `loose.md` is linked now, so its rule goes.
    let host = TestHost::new(&dir).reaches(&["index.md", "loose.md"]);
    let standing = Standing::read(&dir).unwrap();
    assert!(standing.hand.contains(&rule("skip by-hand.md")));
    let plan = plan_against(&host, &standing);
    assert_eq!(plan.stale, [rule("skip loose.md")]);
    apply(&dir, &plan).unwrap();

    let after = read(&dir, "history/skipped.txt");
    assert!(!after.contains("skip loose.md"));
    assert!(after.contains("skip by-hand.md"), "the hand line was lost");
    assert!(after.contains("# prov:begin"));
}

#[test]
fn recording_takes_exactly_what_the_graph_reaches() {
    let dir = tempdir("end-to-end");
    write(&dir, "index.md", "root");
    write(&dir, "notes/a.md", "linked");
    write(&dir, "notes/loose.md", "unlinked");
    write(&dir, ".git/HEAD", "ref");
    write(&dir, "recyclebin/items/gone.md", "consigned");
    init_store(&dir);

    let host = TestHost::new(&dir)
        .reaches(&["index.md", "notes/a.md"])
        .parks(&["recyclebin/items"]);
    apply(&dir, &plan_against(&host, &Standing::read(&dir).unwrap())).unwrap();

    let recorded = record_all(&dir);
    assert_eq!(recorded, ["index.md", "notes/a.md"]);

    // The tracked set now stands in the store, and the same plan settles.
    let standing = Standing::read(&dir).unwrap();
    assert_eq!(
        standing.tracked,
        ["index.md".to_owned(), "notes/a.md".to_owned()].into()
    );
    let plan = plan_against(&host, &standing);
    assert!(plan.settled());
    assert!(plan.withheld.is_empty());
}

#[test]
fn a_file_linked_later_is_released_by_the_next_plan_and_recorded() {
    let dir = tempdir("linked-later");
    write(&dir, "index.md", "root");
    write(&dir, "notes/loose.md", "unlinked today, linked tomorrow");
    init_store(&dir);

    let host = TestHost::new(&dir).reaches(&["index.md"]);
    apply(&dir, &plan_against(&host, &Standing::read(&dir).unwrap())).unwrap();
    record_all(&dir);

    // Tomorrow: the graph reaches it. Yesterday's plan collapsed the wholly
    // unreached folder into one rule, and that rule is what goes stale.
    let host = TestHost::new(&dir).reaches(&["index.md", "notes/loose.md"]);
    let plan = plan_against(&host, &Standing::read(&dir).unwrap());
    assert_eq!(plan.stale, [rule("skip notes/")]);
    apply(&dir, &plan).unwrap();

    let recorded = record_all(&dir);
    assert_eq!(recorded, ["index.md", "notes/loose.md"]);
}

#[test]
fn a_tracked_file_that_left_the_graph_is_withheld_end_to_end() {
    let dir = tempdir("departed");
    write(&dir, "index.md", "root");
    write(&dir, "notes/a.md", "linked, recorded, then unlinked");
    init_store(&dir);

    let host = TestHost::new(&dir).reaches(&["index.md", "notes/a.md"]);
    apply(&dir, &plan_against(&host, &Standing::read(&dir).unwrap())).unwrap();
    record_all(&dir);

    // The link is removed; the file stays on disk and in the tree.
    let host = TestHost::new(&dir).reaches(&["index.md"]);
    let plan = plan_against(&host, &Standing::read(&dir).unwrap());

    assert!(plan.rules.is_empty());
    assert_eq!(plan.withheld.len(), 1);
    assert_eq!(plan.withheld[0].1, "notes/a.md");
    // And because nothing was written, recording still succeeds.
    apply(&dir, &plan).unwrap();
    write(&dir, "notes/a.md", "edited so there is something to record");
    let recorded = record_all(&dir);
    assert_eq!(recorded, ["index.md", "notes/a.md"]);
}

#[test]
fn an_empty_plan_over_a_store_with_no_region_writes_nothing() {
    let dir = tempdir("nothing-to-say");
    write(&dir, "index.md", "root");
    init_store(&dir);
    let before = read(&dir, "history/skipped.txt");

    let host = TestHost::new(&dir).reaches(&["index.md"]);
    apply(&dir, &plan_against(&host, &Standing::read(&dir).unwrap())).unwrap();

    assert_eq!(read(&dir, "history/skipped.txt"), before);
}

#[test]
fn an_emptied_plan_empties_the_region_it_wrote() {
    let dir = tempdir("emptied");
    write(&dir, "index.md", "root");
    write(&dir, "loose.md", "unlinked");
    init_store(&dir);

    let host = TestHost::new(&dir).reaches(&["index.md"]);
    apply(&dir, &plan_against(&host, &Standing::read(&dir).unwrap())).unwrap();

    // The loose file is deleted; nothing is left to skip.
    std::fs::remove_file(dir.join("loose.md")).unwrap();
    let plan = plan_against(&host, &Standing::read(&dir).unwrap());
    assert_eq!(plan.stale, [rule("skip loose.md")]);
    apply(&dir, &plan).unwrap();

    let after = read(&dir, "history/skipped.txt");
    assert!(after.contains("# prov:begin"));
    assert!(!after.contains("skip loose.md"));
    let standing = Standing::read(&dir).unwrap();
    assert!(standing.region.is_empty());
}

#[test]
fn markers_that_do_not_delimit_one_region_are_refused() {
    let dir = tempdir("bad-markers");
    write(&dir, "index.md", "root");
    init_store(&dir);
    let mut text = read(&dir, "history/skipped.txt");
    text.push_str("# prov:end\n");
    std::fs::write(dir.join("history/skipped.txt"), &text).unwrap();

    assert!(Standing::read(&dir).is_err());
}
