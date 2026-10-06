//! `prov views` — the workspace's declared lenses, listed and executed.
//!
//! These drive the whole chain the library tests only see in pieces: a `views:`
//! block written into a real root document, read back through `WorkspaceConfig`,
//! executed against a real spanning tree on a real filesystem. Scope by
//! ancestry excluding an out-of-subtree document that *would* have grouped, and
//! a failing expression reported beside the answer, are only observable from
//! outside.

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

/// A journal declaring two views: a date chain scoped to `Daily` by ancestry,
/// and an unscoped field. `readme.md` carries a `created` stamp and is *not*
/// under `Daily` — it is what makes the scoped view's scope observable.
/// `extra` is spliced into the `daily` entry.
fn vault(tag: &str, extra: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("prov-views-cli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    write(
        &dir,
        "index.md",
        &format!(
            "---\ntitle: Home\nprov:\n  views:\n    daily:\n      label: Daily\n      \
             where: doc.ancestors.exists(a, a.title == 'Daily')\n      \
             key: month(first(date_of_document, created))\n{extra}    \
             who:\n      key: people\ncontents:\n- daily.md\n- readme.md\n---\n"
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

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("prov-views-cli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn bare_views_lists_what_the_workspace_declares() {
    let dir = vault("list", "");
    let (ok, out) = run(&dir, &["views"]);
    assert!(ok, "{out}");
    assert!(
        out.contains(
            "daily  Daily — key: month(first(date_of_document, created))  \
             where: doc.ancestors.exists(a, a.title == 'Daily')"
        ),
        "{out}"
    );
    assert!(out.contains("who  Who — key: people\n"), "{out}");
}

/// Scope is ancestry. `readme.md` carries `created: 2026-01-02` and would group
/// happily — it is excluded because `Daily` is not above it, which is what a
/// lens over a whole vault cannot express.
#[test]
fn a_view_scoped_by_ancestry_groups_only_its_subtree() {
    let dir = vault("scope", "");
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
    let dir = vault("who", "");
    let (ok, out) = run(&dir, &["views", "who"]);
    assert!(ok, "{out}");
    assert!(out.contains("Ada (2)"), "{out}");
    assert!(out.contains("Grace (1)"), "{out}");
    assert!(
        out.contains("readme.md — Readme"),
        "the README is in scope: {out}"
    );
}

/// The count a reader is given is **documents**, not rows. `07-24.md` is under
/// both `Ada` and `Grace`; a total that counted it twice would have the view
/// claiming more entries than the workspace holds.
#[test]
fn the_summary_counts_documents_separately_from_rows() {
    let dir = vault("counts", "");
    let (ok, out) = run(&dir, &["views", "who"]);
    assert!(ok, "{out}");
    assert!(out.contains("6 document(s), 7 row(s)"), "{out}");
}

/// The union the old `group:` chain could not say: a document under every day
/// it has a date for.
#[test]
fn a_key_may_put_a_document_under_several_of_its_own_dates() {
    let dir = scratch("union");
    write(
        &dir,
        "index.md",
        "---\ntitle: Home\nprov:\n  views:\n    activity:\n      \
         key: '[day(created), day(updated)]'\ncontents:\n- a.md\n---\n",
    );
    write(
        &dir,
        "a.md",
        "---\ntitle: A\npart_of: index.md\ncreated: 2026-09-01\nupdated: 2026-09-20T10:00:00Z\n---\n",
    );
    let (ok, out) = run(&dir, &["views", "activity"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("2026-09-01 (1)\n  a.md — A") && out.contains("2026-09-20 (1)\n  a.md — A"),
        "{out}"
    );
}

/// A condition narrows the census, and one matching nothing is an ordinary
/// empty answer.
#[test]
fn a_where_condition_narrows_a_view() {
    let dir = scratch("filter");
    write(
        &dir,
        "index.md",
        "---\ntitle: Home\nprov:\n  views:\n    finished:\n      \
         where: '!present(draft)'\n      key: title\ncontents:\n- a.md\n- b.md\n---\n",
    );
    write(
        &dir,
        "a.md",
        "---\ntitle: A\npart_of: index.md\ndraft: true\n---\n",
    );
    write(&dir, "b.md", "---\ntitle: B\npart_of: index.md\n---\n");
    let (ok, out) = run(&dir, &["views", "finished"]);
    assert!(ok, "{out}");
    assert!(!out.contains("a.md"), "the draft is filtered out: {out}");
    assert!(out.contains("B (1)"), "{out}");
}

/// An expression that cannot run is a config finding — and the view is not
/// read at all, rather than read as a view of everything.
#[test]
fn a_bad_expression_is_reported_by_check_and_the_view_is_not_read() {
    let dir = vault("badwhere", "      icon: calendar\n");
    let text = std::fs::read_to_string(dir.join("index.md")).unwrap();
    std::fs::write(
        dir.join("index.md"),
        text.replace("key: month(first(", "key: mnth(first("),
    )
    .unwrap();

    let (_, out) = run(&dir, &["check"]);
    assert!(
        out.contains("views.daily.key") && out.contains("there is no function `mnth`"),
        "{out}"
    );
    let (ok, out) = run(&dir, &["views", "daily"]);
    assert!(!ok && out.contains("no view named `daily`"), "{out}");
}

/// A document the condition cannot be evaluated on is named, beside the
/// answer — never silently shown or dropped.
#[test]
fn a_failing_expression_is_reported_per_document() {
    let dir = scratch("failure");
    write(
        &dir,
        "index.md",
        "---\ntitle: Home\nprov:\n  views:\n    long:\n      \
         where: size(nickname) > 3\n      key: title\ncontents:\n- a.md\n- b.md\n---\n",
    );
    write(
        &dir,
        "a.md",
        "---\ntitle: A\npart_of: index.md\nnickname: Addie\n---\n",
    );
    write(&dir, "b.md", "---\ntitle: B\npart_of: index.md\n---\n");
    let (ok, out, err) = run_split(&dir, &["views", "long"]);
    assert!(ok, "{out}{err}");
    assert!(out.contains("A (1)"), "{out}");
    assert!(
        err.contains("could not be evaluated") && err.contains("b.md — where:"),
        "{err}"
    );

    let (ok, out, err) = run_split(&dir, &["views", "long", "--json"]);
    assert!(ok && err.is_empty(), "{err}");
    assert!(
        out.contains(
            "\"failures\": [\n    {\n      \"path\": \"b.md\",\n      \"clause\": \"where\","
        ),
        "{out}"
    );
}

/// The A–Z index end to end: `hopper` lower-cased still lands under `H`, and
/// `Ålesund` cuts by character rather than byte.
#[test]
fn an_initial_key_builds_an_a_to_z_index() {
    let dir = scratch("az");
    write(
        &dir,
        "index.md",
        "---\ntitle: Home\nprov:\n  views:\n    surnames:\n      key: initial(surname)\n\
         contents:\n- a.md\n- b.md\n- c.md\n---\n",
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
}

/// Filing by a multi-valued field is a finding, not a runtime surprise — the
/// spine is single-parent, so a document with two values has two homes. A
/// view grouping by the same field is fine.
#[test]
fn filing_by_a_multi_valued_field_is_reported() {
    let dir = scratch("nest");
    write(
        &dir,
        "index.md",
        "---\ntitle: Home\nprov:\n  fields:\n    people:\n      type: seq\n  views:\n    who:\n      \
         key: people\n  filing:\n    who:\n      field: people\n      nest: initial\n\
         contents:\n- a.md\n---\n",
    );
    write(
        &dir,
        "a.md",
        "---\ntitle: A letter\npart_of: index.md\npeople:\n- Ada\n- Grace\n---\n",
    );

    let (_, out) = run(&dir, &["check"]);
    assert!(
        out.contains("filing.who.nest") && out.contains("several homes"),
        "{out}"
    );
    let (ok, out) = run(&dir, &["views", "who"]);
    assert!(ok, "the view still groups: {out}");
    assert!(
        out.contains("Ada (1)") && out.contains("Grace (1)"),
        "{out}"
    );
}

/// Filing by reference wants the field declared a ref — the filing would
/// otherwise break the day the shelf moved. A view grouping by the same path
/// reads it through `field()`.
#[test]
fn filing_by_reference_is_checked_and_a_view_reads_the_path() {
    let dir = scratch("ref");
    let index = |declared: &str| {
        format!(
            "---\ntitle: Home\nprov:\n  fields:\n    written.on:\n      type: {declared}\n  views:\n    \
             journal:\n      key: field('written.on')\n  filing:\n    journal:\n      \
             field: written.on\n      nest: ref\ncontents:\n- calendar.md\n- a.md\n---\n"
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
    // test's. What matters is that the filing entry itself is not one.
    let (_, out) = run(&dir, &["check"]);
    assert!(
        !out.contains("filing.journal.nest"),
        "declared a ref: clean\n{out}"
    );

    let (ok, out) = run(&dir, &["views", "journal"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("17 (1)") && !out.contains("d17.md (1)"),
        "groups by the document the link names, titled by it: {out}"
    );

    write(&dir, "index.md", &index("string"));
    let (ok, out) = run(&dir, &["check"]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("filing.journal.nest") && out.contains("not declared `type: ref`"),
        "{out}"
    );
}

/// A People lens over a `type: ref` field: one person linked under two labels
/// is one group, titled by the person's own page, and relabelling a link does
/// not move a document out of it. The key is the page; the label is its title.
#[test]
fn a_reference_field_groups_by_the_record_it_names() {
    let dir = scratch("people-ref");
    write(
        &dir,
        "index.md",
        "---\ntitle: Home\nprov:\n  fields:\n    people:\n      type: ref\n  views:\n    \
         people:\n      key: people\ncontents:\n- ruth.md\n- letter.md\n- recipe.md\n---\n",
    );
    write(
        &dir,
        "ruth.md",
        "---\ntitle: Ruth Harris\npart_of: index.md\n---\n",
    );
    let entries = |id: &str| {
        write(
            &dir,
            "letter.md",
            &format!(
                "---\ntitle: Letter\npart_of: index.md\npeople:\n- '[Ruth Harris]({id})'\n---\n"
            ),
        );
        write(
            &dir,
            "recipe.md",
            &format!("---\ntitle: Recipe\npart_of: index.md\npeople:\n- '[Grandma]({id})'\n---\n"),
        );
    };
    // The workspace is whole before an id is minted in it, then the two
    // entries link the person by that id under their two labels.
    entries("ruth.md");
    let (ok, id, err) = run_split(&dir, &["id", "ruth.md"]);
    assert!(ok, "{err}");
    entries(id.trim());

    let (ok, out) = run(&dir, &["views", "people"]);
    assert!(ok, "{out}");
    assert!(out.contains("Ruth Harris (2)"), "one group: {out}");
    assert!(!out.contains("Grandma"), "not a group of its own: {out}");

    let (ok, json, _) = run_split(&dir, &["views", "people", "--json"]);
    assert!(ok, "{json}");
    assert!(
        json.contains("\"key\": \"ruth.md\",\n      \"label\": \"Ruth Harris\","),
        "{json}"
    );
}

/// A view in the retired form is not read, and `check` prints what replaces
/// it — the exact YAML, with the anchor as ancestry and `nest:` as filing.
#[test]
fn a_retired_view_is_reported_with_its_replacement() {
    let dir = scratch("retired");
    write(
        &dir,
        "index.md",
        "---\ntitle: Home\nprov:\n  views:\n    daily:\n      group: [date_of_document, created]\n      \
         by: month\n      under: '[[Daily]]'\n      nest: year\ncontents:\n- daily.md\n---\n",
    );
    write(
        &dir,
        "daily.md",
        "---\ntitle: Daily\npart_of: index.md\n---\n",
    );
    let (_, out) = run(&dir, &["check"]);
    assert!(
        out.contains("config `views.daily` is written with the retired view keys"),
        "{out}"
    );
    assert!(
        out.contains("key: \"month(first(date_of_document, created))\"")
            && out.contains("where: \"under('[[Daily]]')\"")
            && out.contains("filing:\n      daily:\n        under: \"[[Daily]]\""),
        "{out}"
    );
    let (_, out) = run(&dir, &["views"]);
    assert!(out.contains("declares no views"), "{out}");

    // The replacement is exact, so an unattended sweep writes it — the view
    // and its filing entry in one go, inside the root's own `prov:` block.
    let (_, out) = run(&dir, &["check", "--fix", "mechanical"]);
    assert!(out.contains("→ rewrite it as the view prov reads"), "{out}");
    let (_, out) = run(&dir, &["check"]);
    assert!(!out.contains("config `views.daily`"), "{out}");
    let (_, out) = run(&dir, &["views"]);
    assert!(out.contains("daily"), "{out}");
    let root = std::fs::read_to_string(dir.join("index.md")).unwrap();
    assert!(
        root.contains("  filing:\n    daily:\n") && root.contains("contents:\n- daily.md"),
        "{root}"
    );
}

#[test]
fn an_unknown_view_name_lists_the_ones_that_exist() {
    let dir = vault("unknown", "");
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
    let dir = scratch("none");
    assert!(run(&dir, &["init", "--yes"]).0, "init");

    let (ok, out) = run(&dir, &["views"]);
    assert!(ok, "{out}");
    assert!(out.contains("declares no views"), "{out}");
}

/// A misspelled key inside a view is caught by the config linter, with the
/// near miss offered.
#[test]
fn a_misspelled_view_key_is_reported_by_check() {
    let dir = vault("lint", "      labl: Oops\n");
    let (_, out) = run(&dir, &["check"]);
    assert!(
        out.contains("views.daily.labl") && out.contains("views.daily.label"),
        "{out}"
    );
    // …and the view still runs: an unknown key is not a broken expression.
    let (ok, out) = run(&dir, &["views", "daily"]);
    assert!(ok, "{out}");
    assert!(out.contains("2026-07 (1)"), "{out}");
}

/// The whole reason for the flag: a consumer that was reading every file's
/// frontmatter one `prov meta` at a time gets the selection *and* the metadata
/// in one call, so the loop it was carrying can go.
#[test]
fn an_executed_view_carries_each_row_s_whole_metadata() {
    let dir = vault("json-rows", "");
    let (ok, out, err) = run_split(&dir, &["views", "daily", "--json"]);
    assert!(ok, "{out}{err}");
    assert!(err.is_empty(), "nothing goes to stderr: {err}");

    assert!(out.starts_with("{\n  \"view\": \"daily\",\n"), "{out}");
    assert!(
        out.contains(
            "\"key\": \"2026-07\",\n      \"label\": \"2026-07\",\n      \"rows\": [\n        {\n          \
             \"path\": \"daily/07-24.md\",\n          \"title\": \"July 24\",\n          \
             \"id\": null,\n          \"ancestors\": [\n"
        ),
        "{out}"
    );
    // Each row knows what is above it, root first.
    assert!(
        out.contains("\"path\": \"daily.md\",\n              \"title\": \"Daily\","),
        "{out}"
    );
    // The metadata is the document's own block, entire — not the fields the
    // view happened to group on.
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
    let dir = vault("json-counts", "");
    let (ok, out, _) = run_split(&dir, &["views", "daily", "--json"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("\"ungrouped\": [\n    {\n      \"path\": \"daily/2026.md\""),
        "{out}"
    );

    let (ok, out, _) = run_split(&dir, &["views", "who", "--json"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("\"documents\": 6,\n  \"rows\": 7,\n  \"failures\": []\n}"),
        "{out}"
    );
}

/// A document with no `title` gets `null` rather than a missing key: a parser
/// reading a fixed set of keys should not have to branch on which arrived.
#[test]
fn a_row_without_a_title_says_null() {
    let dir = vault("json-untitled", "");
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
/// key, with an absent one spelled `null`.
#[test]
fn bare_views_json_lists_the_declarations_as_records() {
    let dir = vault("json-list", "");
    let (ok, out, err) = run_split(&dir, &["views", "--json"]);
    assert!(ok, "{out}{err}");
    assert!(err.is_empty(), "nothing goes to stderr: {err}");
    assert!(
        out.contains(
            "\"name\": \"daily\",\n    \"label\": \"Daily\",\n    \"icon\": null,\n    \
             \"where\": \"doc.ancestors.exists(a, a.title == 'Daily')\",\n    \
             \"key\": \"month(first(date_of_document, created))\""
        ),
        "{out}"
    );
    // `who` declares neither a label nor a condition: the label is the
    // humanized name, and the condition is null rather than absent.
    assert!(
        out.contains(
            "\"name\": \"who\",\n    \"label\": \"Who\",\n    \"icon\": null,\n    \
             \"where\": null,\n    \"key\": \"people\""
        ),
        "{out}"
    );
}

/// A workspace that declares none prints `[]`. The text form says so in a
/// sentence, which is narration for a person; an empty array says the same
/// thing to a program.
#[test]
fn a_workspace_with_no_views_prints_an_empty_json_array() {
    let dir = scratch("json-none");
    assert!(run(&dir, &["init", "--yes"]).0, "init");

    let (ok, out, _) = run_split(&dir, &["views", "--json"]);
    assert!(ok, "{out}");
    assert_eq!(out, "[]\n");
}

/// `query` is a view nobody declared: the same census, the same expressions.
#[test]
fn query_answers_without_a_declared_view() {
    let dir = vault("query", "");
    // A flat list without `--key`.
    let (ok, out) = run(&dir, &["query", "'Ada' in people"]);
    assert!(ok, "{out}");
    assert_eq!(out, "daily/07-24.md — July 24\nreadme.md — Readme\n");

    // Grouped with one, exactly as the declared view groups.
    let (ok, out) = run(
        &dir,
        &[
            "query",
            "doc.ancestors.exists(a, a.title == 'Daily')",
            "--key",
            "month(first(date_of_document, created))",
        ],
    );
    let (_, declared) = run(&dir, &["views", "daily"]);
    assert!(ok, "{out}");
    assert_eq!(out, declared);

    // A bad expression is refused before anything is read.
    let (ok, out) = run(&dir, &["query", "status =="]);
    assert!(!ok && out.contains("where:"), "{out}");

    // JSON: the rows, and the failures beside them.
    let (ok, out, err) = run_split(&dir, &["query", "present(people)", "--json"]);
    assert!(ok && err.is_empty(), "{err}");
    assert!(
        out.starts_with("{\n  \"documents\": [\n    {\n      \"path\": \"daily/07-24.md\""),
        "{out}"
    );
    assert!(out.ends_with("\"failures\": []\n}\n"), "{out}");
}

/// A view is judged whole, so `prov config` can build one a setting at a time
/// once it has a key — and refuses a condition on a view that has none, naming
/// the key it lacks.
#[test]
fn config_builds_a_view_one_setting_at_a_time() {
    let dir = scratch("config");
    assert!(run(&dir, &["init", "--yes"]).0, "init");

    let (ok, out) = run(&dir, &["config", "views.open.where", "present(status)"]);
    assert!(!ok && out.contains("views.open.key"), "{out}");

    for (key, value) in [
        ("views.open.key", "status"),
        ("views.open.where", "present(status) && status != 'done'"),
        ("views.open.label", "Open"),
    ] {
        let (ok, out) = run(&dir, &["config", key, value]);
        assert!(ok, "{key}: {out}");
    }
    let (ok, out) = run(&dir, &["views"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("open  Open — key: status  where: present(status) && status != 'done'"),
        "{out}"
    );

    let (ok, out) = run(&dir, &["config", "views.open.key", "mnth(created)"]);
    assert!(!ok && out.contains("no function `mnth`"), "{out}");
}
