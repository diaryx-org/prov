//! The computation held to its claims: what gets a rule, what is withheld,
//! and what the diff against a standing region says.

use std::collections::BTreeSet;

use historica::working::Rule;

use super::support::*;
use crate::{Reason, Standing};

#[test]
fn unreached_gets_a_rule_and_reachable_does_not() {
    let dir = tempdir("unreached");
    write(&dir, "index.md", "root");
    write(&dir, "notes/a.md", "linked");
    write(&dir, "notes/loose.md", "unlinked");
    let host = TestHost::new(&dir).reaches(&["index.md", "notes/a.md"]);

    let plan = plan(&host);

    assert_eq!(lines(&plan), ["skip notes/loose.md"]);
    assert_eq!(plan.rules[0].reason, Reason::Unreached);
}

#[test]
fn a_folder_nothing_reaches_collapses_to_one_rule() {
    let dir = tempdir("collapse");
    write(&dir, "index.md", "root");
    write(&dir, "drafts/one.md", "x");
    write(&dir, "drafts/deep/two.md", "y");
    let host = TestHost::new(&dir).reaches(&["index.md"]);

    let plan = plan(&host);

    assert_eq!(lines(&plan), ["skip drafts/"]);
}

#[test]
fn a_folder_the_graph_reaches_into_is_ruled_file_by_file() {
    let dir = tempdir("mixed");
    write(&dir, "index.md", "root");
    write(&dir, "notes/a.md", "linked");
    write(&dir, "notes/loose.md", "unlinked");
    write(&dir, "notes/spare.md", "unlinked");
    let host = TestHost::new(&dir).reaches(&["index.md", "notes/a.md"]);

    let plan = plan(&host);

    assert_eq!(lines(&plan), ["skip notes/loose.md", "skip notes/spare.md"]);
}

#[test]
fn a_hidden_directory_is_ruled_without_being_walked() {
    let dir = tempdir("hidden");
    write(&dir, "index.md", "root");
    write(&dir, ".git/objects/aa/bb", "loose object");
    write(&dir, ".git/HEAD", "ref");
    let host = TestHost::new(&dir).reaches(&["index.md"]);

    let plan = plan(&host);

    assert_eq!(lines(&plan), ["skip .git/"]);
    assert_eq!(plan.rules[0].reason, Reason::Hidden);
}

#[test]
fn a_hidden_directory_the_graph_reaches_into_is_walked() {
    let dir = tempdir("hidden-reached");
    write(&dir, "index.md", "root");
    write(&dir, ".notes/kept.md", "linked");
    write(&dir, ".notes/loose.md", "unlinked");
    let host = TestHost::new(&dir).reaches(&["index.md", ".notes/kept.md"]);

    let plan = plan(&host);

    assert_eq!(lines(&plan), ["skip .notes/loose.md"]);
}

#[test]
fn a_hidden_file_is_ruled_with_its_own_reason() {
    let dir = tempdir("hidden-file");
    write(&dir, "index.md", "root");
    write(&dir, ".envrc", "use flake");
    let host = TestHost::new(&dir).reaches(&["index.md"]);

    let plan = plan(&host);

    assert_eq!(lines(&plan), ["skip .envrc"]);
    assert_eq!(plan.rules[0].reason, Reason::Hidden);
}

#[test]
fn bookkeeping_is_ruled_where_it_stands() {
    let dir = tempdir("bookkeeping");
    write(&dir, "index.md", "root");
    write(&dir, "about.md", "derived");
    write(&dir, "recyclebin/items/gone.md", "consigned");
    let host = TestHost::new(&dir)
        .reaches(&["index.md"])
        .parks(&["recyclebin/items", "about.md"]);

    let plan = plan(&host);

    assert_eq!(lines(&plan), ["skip about.md", "skip recyclebin/items/"]);
    assert!(plan.rules.iter().all(|s| s.reason == Reason::Bookkeeping));
}

#[test]
fn a_derived_page_is_ruled_even_though_the_graph_reaches_it() {
    // The about page is *deliberately* reachable — its pointer is what keeps
    // it from lying loose — and excluded even so. Bookkeeping beats
    // reachability, the way the old capture set subtracted its exclusions
    // from the reachable walk.
    let dir = tempdir("derived-reachable");
    write(&dir, "index.md", "root");
    write(&dir, "about.md", "derived");
    let host = TestHost::new(&dir)
        .reaches(&["index.md", "about.md"])
        .parks(&["about.md"]);

    let plan = plan(&host);

    assert_eq!(lines(&plan), ["skip about.md"]);
    assert_eq!(plan.rules[0].reason, Reason::Bookkeeping);
}

#[test]
fn a_claimed_archive_is_one_rule_and_never_walked() {
    let dir = tempdir("claimed");
    write(&dir, "index.md", "root");
    write(&dir, "photos.md", "manifest");
    write(&dir, "photos/2024/a.jpg", "JPEG");
    write(&dir, "photos/2024/b.jpg", "JPEG");
    let host = TestHost::new(&dir)
        .reaches(&["index.md", "photos.md"])
        .claims(&["photos"]);

    let plan = plan(&host);

    assert_eq!(lines(&plan), ["skip photos/"]);
    assert_eq!(plan.rules[0].reason, Reason::Claimed);
}

#[test]
fn a_rule_never_covers_a_tracked_path() {
    let dir = tempdir("withheld");
    write(&dir, "index.md", "root");
    write(&dir, "notes/old.md", "recorded once, unlinked since");
    let host = TestHost::new(&dir).reaches(&["index.md"]);
    let standing = Standing {
        tracked: BTreeSet::from(["index.md".to_owned(), "notes/old.md".to_owned()]),
        ..Standing::default()
    };

    let plan = plan_against(&host, &standing);

    assert!(plan.rules.is_empty(), "rules: {:?}", lines(&plan));
    assert_eq!(
        plan.withheld,
        [(
            Rule::Path("notes/old.md".to_owned()),
            "notes/old.md".to_owned()
        )]
    );
}

#[test]
fn a_directory_rule_is_withheld_when_something_tracked_hides_beneath() {
    let dir = tempdir("withheld-dir");
    write(&dir, "index.md", "root");
    write(&dir, "drafts/old.md", "recorded once");
    write(&dir, "drafts/new.md", "never recorded");
    let host = TestHost::new(&dir).reaches(&["index.md"]);
    let standing = Standing {
        tracked: BTreeSet::from(["index.md".to_owned(), "drafts/old.md".to_owned()]),
        ..Standing::default()
    };

    let plan = plan_against(&host, &standing);

    // The collapse is forbidden, and so is the per-file rule on the tracked
    // path — but the file beside it is still fair to rule.
    assert_eq!(lines(&plan), ["skip drafts/new.md"]);
    assert_eq!(
        plan.withheld,
        [(
            Rule::Path("drafts/old.md".to_owned()),
            "drafts/old.md".to_owned()
        )]
    );
}

#[test]
fn a_hand_rule_covering_a_reachable_file_is_reported_not_repaired() {
    let dir = tempdir("shadowed");
    write(&dir, "index.md", "root");
    write(&dir, "notes/a.md", "linked and skipped");
    let host = TestHost::new(&dir).reaches(&["index.md", "notes/a.md"]);
    let standing = Standing {
        hand: vec![rule("skip notes/a.md")],
        ..Standing::default()
    };

    let plan = plan_against(&host, &standing);

    assert!(plan.rules.is_empty());
    assert_eq!(
        plan.shadowed,
        [(rule("skip notes/a.md"), "notes/a.md".to_owned())]
    );
}

#[test]
fn a_hand_rule_already_covering_an_unreached_file_is_not_duplicated() {
    let dir = tempdir("hand-covered");
    write(&dir, "index.md", "root");
    write(&dir, "scratch.md", "unlinked, hand-skipped");
    write(&dir, "junk/a.tmp", "under a hand-skipped folder");
    let host = TestHost::new(&dir).reaches(&["index.md"]);
    let standing = Standing {
        hand: vec![rule("skip scratch.md"), rule("skip junk/")],
        ..Standing::default()
    };

    let plan = plan_against(&host, &standing);

    assert!(plan.rules.is_empty(), "rules: {:?}", lines(&plan));
    assert!(plan.shadowed.is_empty());
}

#[test]
fn a_hand_suffix_rule_covers_what_it_covers() {
    let dir = tempdir("hand-suffix");
    write(&dir, "index.md", "root");
    write(&dir, "notes/.DS_Store", "junk the default already skips");
    let host = TestHost::new(&dir).reaches(&["index.md"]);
    let standing = Standing {
        hand: vec![rule("skip-suffix .DS_Store")],
        ..Standing::default()
    };

    let plan = plan_against(&host, &standing);

    assert!(plan.rules.is_empty(), "rules: {:?}", lines(&plan));
}

#[test]
fn the_store_itself_needs_no_rule() {
    let dir = tempdir("store-silent");
    write(&dir, "index.md", "root");
    write(&dir, "history/historica.txt", "historica-v1\n");
    write(&dir, "history/revisions/keep.rev.txt", "not content");
    let host = TestHost::new(&dir).reaches(&["index.md"]);

    let plan = plan(&host);

    assert!(plan.rules.is_empty(), "rules: {:?}", lines(&plan));
}

#[test]
fn a_folder_merely_called_history_is_content_like_any_other() {
    let dir = tempdir("history-name");
    write(&dir, "index.md", "root");
    write(&dir, "history/essay.md", "an essay about history");
    let host = TestHost::new(&dir).reaches(&["index.md", "history/essay.md"]);

    let plan = plan(&host);

    assert!(plan.rules.is_empty(), "rules: {:?}", lines(&plan));
}

#[test]
fn fresh_and_stale_diff_the_standing_region() {
    let dir = tempdir("diff");
    write(&dir, "index.md", "root");
    write(&dir, "loose.md", "unlinked");
    let host = TestHost::new(&dir).reaches(&["index.md"]);
    // The region says something obsolete — a file since linked or deleted —
    // and does not yet say what the walk found.
    let standing = Standing {
        region: vec![rule("skip vanished.md")],
        ..Standing::default()
    };

    let plan = plan_against(&host, &standing);

    assert_eq!(lines(&plan), ["skip loose.md"]);
    assert_eq!(plan.fresh, plan.rules);
    assert_eq!(plan.stale, [rule("skip vanished.md")]);
    assert!(!plan.settled());
}

#[test]
fn a_settled_region_computes_a_settled_plan() {
    let dir = tempdir("settled");
    write(&dir, "index.md", "root");
    write(&dir, "loose.md", "unlinked");
    let host = TestHost::new(&dir).reaches(&["index.md"]);
    let standing = Standing {
        region: vec![rule("skip loose.md")],
        ..Standing::default()
    };

    let plan = plan_against(&host, &standing);

    assert!(plan.settled());
    assert!(plan.fresh.is_empty());
    assert!(plan.stale.is_empty());
}

#[test]
fn rules_come_out_in_path_order_whatever_the_walk_met_first() {
    let dir = tempdir("order");
    write(&dir, "index.md", "root");
    write(&dir, "zebra.md", "unlinked");
    write(&dir, "alpha.md", "unlinked");
    write(&dir, "midway/loose.md", "unlinked");
    let host = TestHost::new(&dir).reaches(&["index.md"]);

    let plan = plan(&host);

    assert_eq!(
        lines(&plan),
        ["skip alpha.md", "skip midway/", "skip zebra.md"]
    );
}
