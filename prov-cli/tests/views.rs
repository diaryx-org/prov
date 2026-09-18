//! `prov views` — the workspace's declared lenses, listed and executed.
//!
//! These drive the whole chain the library tests only see in pieces: a `views:`
//! block written into a real root document, read back through `WorkspaceConfig`,
//! executed against a real spanning tree on a real filesystem. Two of the three
//! properties asserted here (scope excluding an out-of-subtree document that
//! *would* have grouped, and a dead anchor exiting non-zero) are only observable
//! from outside.

use std::path::Path;
use std::process::Command;

fn run(dir: &Path, args: &[&str]) -> (bool, String) {
    let (ok, stdout, stderr) = run_split(dir, args);
    (ok, stdout + &stderr)
}

/// The two streams apart, which the `--json` assertions need: the flag promises
/// one value on stdout and nothing beside it, and a test that concatenates
/// cannot tell a warning from a key.
fn run_split(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_prov"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run prov");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// A journal declaring two views: a scoped date chain and an unscoped field.
/// `readme.md` carries a `created` stamp and is *not* under `Daily` — it is
/// what makes the scoped view's scope observable.
fn vault(tag: &str, under: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("prov-views-cli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    write(
        &dir,
        "index.md",
        &format!(
            "---\ntitle: Home\nprov:\n  views:\n    daily:\n      label: Daily\n      \
             group: [date_of_document, created]\n      by: month\n      under: '{under}'\n    \
             who:\n      group: people\ncontents:\n- daily.md\n- readme.md\n---\n"
        ),
    );
    write(
        &dir,
        "readme.md",
        "---\ntitle: Readme\npart_of: index.md\ncreated: 2026-01-02\npeople:\n- Ada\n---\n",
    );
    write(
        &dir,
        "daily.md",
        "---\ntitle: Daily\npart_of: index.md\ncontents:\n- daily/2026.md\n---\n",
    );
    write(
        &dir,
        "daily/2026.md",
        "---\ntitle: '2026'\npart_of: ../daily.md\ncontents:\n- 07-24.md\n- 08-01.md\n---\n",
    );
    write(
        &dir,
        "daily/07-24.md",
        "---\ntitle: July 24\npart_of: 2026.md\ndate_of_document: 2026-07-24\npeople:\n- Ada\n- Grace\n---\n",
    );
    write(
        &dir,
        "daily/08-01.md",
        "---\ntitle: August 1\npart_of: 2026.md\ncreated: 2026-08-01T09:00:00Z\n---\n",
    );
    dir
}

#[test]
fn bare_views_lists_what_the_workspace_declares() {
    let dir = vault("list", "[Daily](daily.md)");
    let (ok, out) = run(&dir, &["views"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("daily  Daily — group: date_of_document → created by month"),
        "{out}"
    );
    assert!(
        out.contains("who  Who — group: people (whole workspace)"),
        "{out}"
    );
}

/// The point of `under:`. `readme.md` carries `created: 2026-01-02` and would
/// group happily — it is excluded because it is not in the subtree, which is
/// what a lens over a whole vault cannot express.
#[test]
fn a_scoped_view_groups_only_its_subtree() {
    let dir = vault("scope", "[Daily](daily.md)");
    let (ok, out) = run(&dir, &["views", "daily"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("2026-07 (1)") && out.contains("daily/07-24.md — July 24"),
        "{out}"
    );
    assert!(out.contains("2026-08 (1)"), "{out}");
    assert!(
        !out.contains("readme.md"),
        "the README is out of scope: {out}"
    );
    // The year index has no date of its own, and is named rather than dropped.
    assert!(
        out.contains("(ungrouped) (1)") && out.contains("daily/2026.md"),
        "{out}"
    );
    assert!(!out.contains("2026-01"), "{out}");
}

/// The same corpus, unscoped and grouped by a multi-valued field: the README
/// joins, and one document files under both of its values.
#[test]
fn an_unscoped_view_covers_everything_and_repeats_multi_valued_rows() {
    let dir = vault("who", "[Daily](daily.md)");
    let (ok, out) = run(&dir, &["views", "who"]);
    assert!(ok, "{out}");
    assert!(out.contains("Ada (2)"), "{out}");
    assert!(out.contains("Grace (1)"), "{out}");
    assert!(
        out.contains("readme.md — Readme"),
        "the README is in scope: {out}"
    );
}

/// The count a reader is given is **documents**, not rows. `letter.md` is under
/// both `Ada` and `Grace`; a total that counted it twice would have the view
/// claiming more entries than the workspace holds.
#[test]
fn the_summary_counts_documents_separately_from_rows() {
    let dir = vault("counts", "[Daily](daily.md)");
    let (ok, out) = run(&dir, &["views", "who"]);
    assert!(ok, "{out}");
    assert!(out.contains("6 document(s), 7 row(s)"), "{out}");
}

/// `where:` narrows what scope reached — and a condition matching nothing is an
/// ordinary empty answer, not the error a broken anchor is.
#[test]
fn a_where_condition_narrows_a_view() {
    let dir = vault("filter", "[Daily](daily.md)");
    let text = std::fs::read_to_string(dir.join("index.md")).unwrap();
    std::fs::write(
        dir.join("index.md"),
        text.replace(
            "      by: month\n",
            "      by: month\n      where:\n        not:\n          has: draft\n",
        ),
    )
    .unwrap();
    // Mark one entry a draft; it should leave the view without leaving the vault.
    let entry = dir.join("daily/07-24.md");
    let marked = std::fs::read_to_string(&entry)
        .unwrap()
        .replace("date_of_document:", "draft: true\ndate_of_document:");
    std::fs::write(&entry, marked).unwrap();

    let (ok, out) = run(&dir, &["views", "daily"]);
    assert!(ok, "{out}");
    assert!(
        !out.contains("07-24.md"),
        "the draft is filtered out: {out}"
    );
    assert!(out.contains("2026-08 (1)"), "{out}");

    // The listing flags that this view no longer shows everything it reaches.
    let (_, out) = run(&dir, &["views"]);
    assert!(out.contains("[filtered]"), "{out}");
}

/// A `where:` nobody can read is a config finding, not a silent "select
/// everything" or "select nothing".
#[test]
fn an_unreadable_where_is_reported_by_check() {
    let dir = vault("badwhere", "[Daily](daily.md)");
    let text = std::fs::read_to_string(dir.join("index.md")).unwrap();
    std::fs::write(
        dir.join("index.md"),
        text.replace(
            "      by: month\n",
            "      by: month\n      where: audience == public\n",
        ),
    )
    .unwrap();

    let (_, out) = run(&dir, &["check"]);
    assert!(
        out.contains("views.daily.where") && out.contains("has, equals"),
        "{out}"
    );
}

/// The generalization end to end: the same `by:` key, no calendar involved.
/// `hopper` lower-cased still lands under `H`, and `Ålesund` cuts by character
/// rather than byte.
#[test]
fn an_initial_grain_builds_an_a_to_z_index() {
    let dir = std::env::temp_dir().join(format!("prov-views-cli-az-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    write(
        &dir,
        "index.md",
        "---\ntitle: Home\nprov:\n  views:\n    surnames:\n      group: surname\n      \
         by: initial\n      nest: { initial: 2 }\ncontents:\n- a.md\n- b.md\n- c.md\n---\n",
    );
    write(
        &dir,
        "a.md",
        "---\ntitle: Ada\npart_of: index.md\nsurname: Lovelace\n---\n",
    );
    write(
        &dir,
        "b.md",
        "---\ntitle: Grace\npart_of: index.md\nsurname: hopper\n---\n",
    );
    write(
        &dir,
        "c.md",
        "---\ntitle: Someone\npart_of: index.md\nsurname: Ålesund\n---\n",
    );

    let (ok, out) = run(&dir, &["views", "surnames"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("H (1)"),
        "lower-case still files under H: {out}"
    );
    assert!(out.contains("L (1)"), "{out}");
    assert!(out.contains("Å (1)"), "cut by character, not byte: {out}");

    // The listing reports the filing grain, since `nest` is the half that
    // writes.
    let (_, out) = run(&dir, &["views"]);
    assert!(out.contains("by initial"), "{out}");
    assert!(out.contains("files by initial 2"), "{out}");
}

/// `nest` on a multi-valued field is a finding, not a runtime surprise — the
/// spine is single-parent, so a document with two values has two homes. The
/// view still groups.
#[test]
fn nesting_by_a_multi_valued_field_is_reported() {
    let dir = std::env::temp_dir().join(format!("prov-views-cli-nest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    write(
        &dir,
        "index.md",
        "---\ntitle: Home\nprov:\n  fields:\n    people:\n      type: seq\n  views:\n    who:\n      \
         group: people\n      nest: initial\ncontents:\n- a.md\n---\n",
    );
    write(
        &dir,
        "a.md",
        "---\ntitle: A letter\npart_of: index.md\npeople:\n- Ada\n- Grace\n---\n",
    );

    let (_, out) = run(&dir, &["check"]);
    assert!(
        out.contains("views.who.nest") && out.contains("several homes"),
        "{out}"
    );

    let (ok, out) = run(&dir, &["views", "who"]);
    assert!(ok, "the view still groups: {out}");
    assert!(
        out.contains("Ada (1)") && out.contains("Grace (1)"),
        "{out}"
    );
}

/// `nest: ref` files a record under the document its own field links to.
/// The listing says so, the view groups by the link, and the field not being
/// declared `type: ref` is a finding — the filing would break the day the
/// shelf moved.
#[test]
fn nesting_by_reference_is_listed_grouped_and_checked() {
    let dir = std::env::temp_dir().join(format!("prov-views-cli-ref-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let index = |declared: &str| {
        format!(
            "---\ntitle: Home\nprov:\n  fields:\n    written.on:\n      type: {declared}\n  views:\n    journal:\n      \
             group: written.on\n      nest: ref\ncontents:\n- calendar.md\n- a.md\n---\n"
        )
    };
    write(&dir, "index.md", &index("ref"));
    write(
        &dir,
        "calendar.md",
        "---\ntitle: Calendar\npart_of: index.md\ncontents:\n- d17.md\n---\n",
    );
    write(
        &dir,
        "d17.md",
        "---\ntitle: '17'\npart_of: calendar.md\n---\n",
    );
    write(
        &dir,
        "a.md",
        "---\ntitle: Morning\npart_of: index.md\nwritten:\n  on: d17.md\n  at: '09:12'\n---\n",
    );

    // Not `ok`: a fresh vault has no `about.md`, and that finding is not this
    // test's. What matters is that the view itself is not one.
    let (_, out) = run(&dir, &["check"]);
    assert!(
        !out.contains("views.journal.nest"),
        "declared a ref: clean\n{out}"
    );

    let (ok, out) = run(&dir, &["views"]);
    assert!(ok, "{out}");
    assert!(out.contains("files by ref"), "{out}");

    let (ok, out) = run(&dir, &["views", "journal"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("d17.md (1)"),
        "groups by the link as written: {out}"
    );

    write(&dir, "index.md", &index("string"));
    let (ok, out) = run(&dir, &["check"]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("views.journal.nest") && out.contains("not declared `type: ref`"),
        "{out}"
    );
}

/// An anchor that names nothing must not read as an archive with nothing in it.
#[test]
fn a_dead_anchor_fails_loudly_rather_than_printing_an_empty_view() {
    let dir = vault("dead", "[Daily](gone.md)");
    let (ok, out) = run(&dir, &["views", "daily"]);
    assert!(!ok, "a broken view exits non-zero: {out}");
    assert!(out.contains("no document exists there"), "{out}");
}

#[test]
fn an_unknown_view_name_lists_the_ones_that_exist() {
    let dir = vault("unknown", "[Daily](daily.md)");
    let (ok, out) = run(&dir, &["views", "nope"]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("no view named `nope`") && out.contains("daily, who"),
        "{out}"
    );
}

/// A workspace that declares none says so, rather than printing nothing and
/// leaving the user unsure whether the command ran.
#[test]
fn a_workspace_with_no_views_says_so() {
    let dir = std::env::temp_dir().join(format!("prov-views-cli-none-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert!(run(&dir, &["init", "--yes"]).0, "init");

    let (ok, out) = run(&dir, &["views"]);
    assert!(ok, "{out}");
    assert!(out.contains("declares no views"), "{out}");
}

/// A misspelled key inside a view is caught by the config linter — the whole
/// reason the block was promoted out of an app's private namespace, where
/// nothing would have looked at it.
#[test]
fn a_misspelled_view_key_is_reported_by_check() {
    let dir = vault("lint", "[Daily](daily.md)");
    let text = std::fs::read_to_string(dir.join("index.md")).unwrap();
    std::fs::write(
        dir.join("index.md"),
        text.replace("      by: month", "      by: monthh\n      labl: Oops"),
    )
    .unwrap();

    let (_, out) = run(&dir, &["check"]);
    assert!(
        out.contains("views.daily.by") && out.contains("expected: year, month, day"),
        "{out}"
    );
    assert!(
        out.contains("views.daily.labl") && out.contains("views.daily.label"),
        "{out}"
    );

    // …and the view still runs, grouping on the uncut values rather than on a
    // grain nobody asked for.
    let (ok, out) = run(&dir, &["views", "daily"]);
    assert!(ok, "{out}");
    assert!(out.contains("2026-07-24 (1)"), "{out}");
}

/// The whole reason for the flag: a consumer that was reading every file's
/// frontmatter one `prov meta` at a time gets the selection *and* the metadata
/// in one call, so the loop it was carrying can go.
#[test]
fn an_executed_view_carries_each_row_s_whole_metadata() {
    let dir = vault("json-rows", "[Daily](daily.md)");
    let (ok, out, err) = run_split(&dir, &["views", "daily", "--json"]);
    assert!(ok, "{out}{err}");
    assert!(err.is_empty(), "nothing goes to stderr: {err}");

    assert!(out.starts_with("{\n  \"view\": \"daily\",\n"), "{out}");
    assert!(
        out.contains(
            "\"key\": \"2026-07\",\n      \"rows\": [\n        {\n          \
             \"path\": \"daily/07-24.md\",\n          \"title\": \"July 24\",\n          \
             \"meta\": {\n"
        ),
        "{out}"
    );
    // The metadata is the document's own block, entire and in its order — not
    // the fields the view happened to group on.
    assert!(
        out.contains("\"date_of_document\": \"2026-07-24\"")
            && out.contains("\"people\": [\n              \"Ada\",\n              \"Grace\"\n"),
        "{out}"
    );
    // Structure survives: the year index's `contents` crosses as a list, not as
    // the text of one.
    assert!(
        out.contains("\"contents\": [\n          \"07-24.md\",\n          \"08-01.md\"\n        ]"),
        "{out}"
    );
}

/// The ungrouped bucket is a key, not an omission, for the reason the text
/// output names it rather than dropping it: a view whose entries have all
/// stopped grouping is otherwise indistinguishable from an empty archive.
#[test]
fn the_ungrouped_bucket_and_both_counts_survive_the_crossing() {
    let dir = vault("json-counts", "[Daily](daily.md)");
    let (ok, out, _) = run_split(&dir, &["views", "daily", "--json"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("\"ungrouped\": [\n    {\n      \"path\": \"daily/2026.md\""),
        "{out}"
    );

    // The same two numbers the text summary keeps apart, kept apart here: the
    // multi-valued view files one document under two people.
    let (ok, out, _) = run_split(&dir, &["views", "who", "--json"]);
    assert!(ok, "{out}");
    assert!(out.contains("\"documents\": 6,\n  \"rows\": 7\n}"), "{out}");
}

/// A document with no `title` gets `null` rather than a missing key: a parser
/// reading a fixed set of keys should not have to branch on which arrived.
#[test]
fn a_row_without_a_title_says_null() {
    let dir = vault("json-untitled", "[Daily](daily.md)");
    write(
        &dir,
        "daily/09-09.md",
        "---\npart_of: 2026.md\ndate_of_document: 2026-09-09\n---\n",
    );
    let index = dir.join("daily/2026.md");
    let text = std::fs::read_to_string(&index).unwrap();
    std::fs::write(&index, text.replace("- 08-01.md", "- 08-01.md\n- 09-09.md")).unwrap();

    let (ok, out, _) = run_split(&dir, &["views", "daily", "--json"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("\"path\": \"daily/09-09.md\",\n          \"title\": null,\n"),
        "{out}"
    );
}

/// The listing, as records: the same facts the line prints, each under its own
/// key, with an absent axis spelled `null`.
#[test]
fn bare_views_json_lists_the_declarations_as_records() {
    let dir = vault("json-list", "[Daily](daily.md)");
    let (ok, out, err) = run_split(&dir, &["views", "--json"]);
    assert!(ok, "{out}{err}");
    assert!(err.is_empty(), "nothing goes to stderr: {err}");
    assert!(
        out.contains(
            "\"name\": \"daily\",\n    \"label\": \"Daily\",\n    \"group\": [\n      \
             \"date_of_document\",\n      \"created\"\n    ],\n    \"by\": \"month\",\n    \
             \"under\": \"[Daily](daily.md)\",\n    \"filtered\": false,\n    \"nest\": null"
        ),
        "{out}"
    );
    // `who` declares neither a label nor a grain: the label is the humanized
    // name, and the rest are null rather than absent.
    assert!(
        out.contains(
            "\"name\": \"who\",\n    \"label\": \"Who\",\n    \"group\": [\n      \
             \"people\"\n    ],\n    \"by\": null,\n    \"under\": null,"
        ),
        "{out}"
    );
}

/// A workspace that declares none prints `[]`. The text form says so in a
/// sentence, which is narration for a person; an empty array says the same
/// thing to a program, so "declares nothing" and "printed nothing" stay
/// distinguishable.
#[test]
fn a_workspace_with_no_views_prints_an_empty_json_array() {
    let dir = std::env::temp_dir().join(format!("prov-views-cli-json-none-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert!(run(&dir, &["init", "--yes"]).0, "init");

    let (ok, out, _) = run_split(&dir, &["views", "--json"]);
    assert!(ok, "{out}");
    assert_eq!(out, "[]\n");
}

/// `--json` reports a view's shape, not its execution, so a `where:` shows up
/// as the flag the listing line makes it — and the executed view is an ordinary
/// empty-ish answer, not an error.
#[test]
fn the_json_listing_flags_a_filtered_view() {
    let dir = vault("json-filter", "[Daily](daily.md)");
    let text = std::fs::read_to_string(dir.join("index.md")).unwrap();
    std::fs::write(
        dir.join("index.md"),
        text.replace(
            "      by: month\n",
            "      by: month\n      where:\n        has: date_of_document\n",
        ),
    )
    .unwrap();

    let (ok, out, _) = run_split(&dir, &["views", "--json"]);
    assert!(ok, "{out}");
    assert!(out.contains("\"filtered\": true"), "{out}");
}

/// A broken view is still an error under `--json`: the failure modes do not
/// change with the output format, or a script would read a dead anchor as an
/// archive with nothing in it — which is the exact confusion the error exists
/// to prevent.
#[test]
fn a_dead_anchor_still_fails_under_json() {
    let dir = vault("json-dead", "[Daily](gone.md)");
    let (ok, out, err) = run_split(&dir, &["views", "daily", "--json"]);
    assert!(!ok, "{out}{err}");
    assert!(out.is_empty(), "no half-written object on stdout: {out}");
    assert!(err.contains("no document exists there"), "{err}");

    let (ok, out, err) = run_split(&dir, &["views", "nope", "--json"]);
    assert!(!ok, "{out}{err}");
    assert!(err.contains("no view named `nope`"), "{err}");
}
