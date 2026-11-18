//! The import, end to end: the seeded Synthea fixture into a scratch
//! workspace, checked by the same `check` the `prov` CLI runs, imported again
//! to show nothing moves, and imported from a changed export to show what does.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use prov::{Discovery, StdFs, Value, Workspace, block_on};
use prov_fhir::fhir::{Export, Record};
use prov_fhir::import::{self, Options, Report};
use prov_fhir::map::Section;

const SHARYN: &str = "c053e996-a4c4-6c02-e2b6-284227156c67";
const WILFREDO: &str = "aee7bbe1-0c45-c028-1e62-1f4cdb30c273";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/synthea")
}

fn record_from(source: &Path, id: &str) -> Record {
    Export::read(source).unwrap().record(id).unwrap().unwrap()
}

fn opts(now: &str) -> Options {
    Options {
        emergency: true,
        now: now.to_string(),
    }
}

/// A fresh medical workspace at `dir/ws`, as `--each` makes one.
fn workspace(dir: &Path, title: &str) -> PathBuf {
    let root = dir.join("ws");
    block_on(prov_fhir::preset::init(
        &root,
        title,
        &prov_fhir::preset::medical().unwrap(),
    ))
    .unwrap();
    root
}

fn run(root: &Path, record: &Record, now: &str) -> Report {
    block_on(import::import(root, record, &opts(now))).unwrap()
}

/// What `prov check` reports for the workspace at `root`: every finding of
/// the library's check, and the about page's.
fn check(root: &Path) -> Vec<String> {
    let Discovery::Found(found) = block_on(prov::discover(&StdFs, root)).unwrap() else {
        panic!("no workspace at {}", root.display());
    };
    let ws = block_on(Workspace::open_for_writing(StdFs, found.as_root(), 1)).unwrap();
    let mut findings: Vec<String> = block_on(ws.check(&found.root_doc))
        .unwrap()
        .iter()
        .map(|f| format!("{f:?}"))
        .collect();
    let probe: Workspace<StdFs> = Workspace::builder(StdFs)
        .root(root)
        .relations(found.config.relation_set())
        .build();
    let about = prov::AboutContext {
        root_doc: found.root_doc.clone(),
        config_doc: block_on(probe.config_path(&found.root_doc)).unwrap(),
        registry_doc: block_on(probe.registry_path(&found.root_doc)).unwrap(),
        deletions_doc: block_on(probe.deletions_path(&found.root_doc)).unwrap(),
        history_doc: block_on(probe.history_path(&found.root_doc)).unwrap(),
        reified: block_on(probe.reified_vocabularies(&found.root_doc, &found.config)).unwrap(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    if let Some(f) = block_on(ws.check_about(&found.root_doc, &found.config, &about)).unwrap() {
        findings.push(format!("{f:?}"));
    }
    findings
}

/// Every file under `root`, by path, with its bytes.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap().to_path_buf();
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn meta(root: &Path, rel: &Path) -> Value {
    let text = std::fs::read_to_string(root.join(rel)).unwrap();
    prov::Document::parse(rel, &text).unwrap().meta
}

/// The document whose `fhir` names `key` (`Encounter/<id>`) or the medicine
/// `Medication|<system>|<code>`.
fn find(root: &Path, title_starts: &str) -> PathBuf {
    snapshot(root)
        .into_keys()
        .find(|p| {
            p.extension().is_some_and(|e| e == "md")
                && meta(root, p)
                    .get("title")
                    .and_then(Value::as_str)
                    .is_some_and(|t| t.starts_with(title_starts))
        })
        .unwrap_or_else(|| panic!("no document titled {title_starts}…"))
}

fn str_field(root: &Path, rel: &Path, key: &str) -> Option<String> {
    meta(root, rel)
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[test]
fn an_import_checks_clean() {
    let dir = prov_testkit::scratch("prov-fhir-import", "clean");
    let root = workspace(&dir, "Scratch");
    let record = record_from(&fixture(), SHARYN);
    let report = run(&root, &record, "2026-11-18T12:00:00.000000Z");

    assert!(!report.import_reused);
    assert!(report.retitled, "the root takes the patient's name");
    assert!(Report::total(&report.created) > 100, "{report:?}");
    assert_eq!(report.created.get(&Section::Allergies), Some(&2));
    assert!(report.updated.is_empty());
    assert!(report.unknown_systems.is_empty());
    assert_eq!(check(&root), Vec::<String>::new());

    // The root is the patient.
    let root_meta = meta(&root, Path::new("index.md"));
    assert_eq!(
        root_meta.get("title").and_then(Value::as_str),
        Some("Sharyn437 Anette189 Hagenes547")
    );
    assert_eq!(
        root_meta
            .get("fhir")
            .and_then(|f| f.get("id"))
            .and_then(Value::as_str),
        Some(SHARYN)
    );

    // The export is kept whole under Imports, and every document says it
    // came from there.
    let node = &report.import;
    assert!(node.starts_with("imports"));
    assert_eq!(str_field(&root, node, "kind").as_deref(), Some("import"));
    let kept = root.join(node.with_extension("")).join("Encounter.ndjson");
    let (_, raw) = record
        .raw
        .iter()
        .find(|(n, _)| n == "Encounter.ndjson")
        .unwrap();
    assert_eq!(&std::fs::read(kept).unwrap(), raw);
    let encounter = snapshot(&root)
        .into_keys()
        .find(|p| {
            p.starts_with("encounters") && !p.ends_with("index.md") && p.components().count() == 3
        })
        .expect("an encounter filed under its year");
    let m = meta(&root, &encounter);
    assert_eq!(m.get("kind").and_then(Value::as_str), Some("encounter"));
    let derived = m.get("derived_from").and_then(Value::as_str).unwrap();
    assert!(
        derived.contains(&node.to_string_lossy().to_string()),
        "{derived}"
    );
    let generated = m.get("generated").unwrap();
    assert_eq!(
        generated.get("by").and_then(Value::as_str),
        Some("process:prov-fhir")
    );
    assert_eq!(
        generated.get("how").and_then(Value::as_str),
        Some("imported")
    );
    assert_eq!(
        generated.get("at").and_then(Value::as_str),
        Some("2026-11-18T12:00:00.000000Z")
    );
    let year = &m.get("date_of_service").and_then(Value::as_str).unwrap()[..4];
    assert_eq!(
        encounter.parent().unwrap(),
        Path::new("encounters").join(year)
    );

    // Audiences: the emergency list, and everything else the patient's.
    let peanut = find(&root, "Allergy to peanut");
    assert_eq!(
        str_field(&root, &peanut, "audience").as_deref(),
        Some("emergency")
    );
    let ibuprofen = find(&root, "Ibuprofen");
    assert_eq!(
        str_field(&root, &ibuprofen, "audience").as_deref(),
        Some("self")
    );
    assert_eq!(
        str_field(&root, &ibuprofen, "status").as_deref(),
        Some("stopped")
    );
}

#[test]
fn importing_again_changes_nothing() {
    let dir = prov_testkit::scratch("prov-fhir-import", "again");
    let root = workspace(&dir, "Scratch");
    let record = record_from(&fixture(), SHARYN);
    let first = run(&root, &record, "2026-11-18T12:00:00.000000Z");
    let before = snapshot(&root);

    let again = run(&root, &record, "2026-11-19T08:00:00.000000Z");
    assert!(again.import_reused);
    assert!(again.created.is_empty(), "{again:?}");
    assert!(again.updated.is_empty(), "{again:?}");
    assert_eq!(
        Report::total(&again.unchanged),
        Report::total(&first.created)
    );
    assert_eq!(
        snapshot(&root),
        before,
        "a second import of the same export wrote something"
    );
}

#[test]
fn a_changed_export_updates_in_place() {
    let dir = prov_testkit::scratch("prov-fhir-import", "update");
    let root = workspace(&dir, "Scratch");
    run(
        &root,
        &record_from(&fixture(), SHARYN),
        "2026-11-18T12:00:00.000000Z",
    );
    let fexofenadine = find(&root, "Fexofenadine");
    let epinephrine = find(&root, "NDA020800 0.3 ML Epinephrine");
    assert_eq!(
        str_field(&root, &fexofenadine, "audience").as_deref(),
        Some("emergency")
    );
    // The patient decides the epinephrine is for the care team, not the
    // emergency summary.
    let text = std::fs::read_to_string(root.join(&epinephrine)).unwrap();
    std::fs::write(
        root.join(&epinephrine),
        text.replace("audience: emergency", "audience: care-team"),
    )
    .unwrap();
    let docs_before = snapshot(&root).len();

    // A later export: both prescriptions have since been completed.
    let later = dir.join("later");
    std::fs::create_dir_all(&later).unwrap();
    for entry in std::fs::read_dir(fixture()).unwrap() {
        let path = entry.unwrap().path();
        let mut text = std::fs::read_to_string(&path).unwrap();
        if path.file_name().unwrap() == "MedicationRequest.ndjson" {
            text = text.replace(r#""status":"active""#, r#""status":"completed""#);
        }
        std::fs::write(later.join(path.file_name().unwrap()), text).unwrap();
    }
    let report = run(
        &root,
        &record_from(&later, SHARYN),
        "2026-12-01T12:00:00.000000Z",
    );
    assert!(!report.import_reused, "a different export is a new import");
    assert!(report.created.is_empty(), "{report:?}");
    assert_eq!(
        report.updated.get(&Section::Medications),
        Some(&2),
        "{report:?}"
    );
    assert_eq!(check(&root), Vec::<String>::new());

    for med in [&fexofenadine, &epinephrine] {
        assert_eq!(str_field(&root, med, "status").as_deref(), Some("stopped"));
        // The new version came from the new import, and `generated` is
        // still the first one's.
        let derived = str_field(&root, med, "derived_from").unwrap();
        assert!(derived.contains(&report.import.to_string_lossy().to_string()));
        let m = meta(&root, med);
        assert_eq!(
            m.get("generated")
                .and_then(|g| g.get("at"))
                .and_then(Value::as_str),
            Some("2026-11-18T12:00:00.000000Z")
        );
    }
    // The audience the importer set follows the status; the one the patient
    // set stays theirs.
    assert_eq!(
        str_field(&root, &fexofenadine, "audience").as_deref(),
        Some("self")
    );
    assert_eq!(
        str_field(&root, &epinephrine, "audience").as_deref(),
        Some("care-team")
    );
    // Only the new import's files are new: no document was duplicated.
    let added = snapshot(&root).len() - docs_before;
    let raw_files = record_from(&later, SHARYN).raw.len();
    assert_eq!(
        added,
        raw_files + 2,
        "the export's files, its node and its manifest"
    );
}

#[test]
fn a_workspace_keeps_one_patient() {
    let dir = prov_testkit::scratch("prov-fhir-import", "one-patient");
    let root = workspace(&dir, "Scratch");
    run(
        &root,
        &record_from(&fixture(), SHARYN),
        "2026-11-18T12:00:00.000000Z",
    );
    let err = block_on(import::import(
        &root,
        &record_from(&fixture(), WILFREDO),
        &opts("2026-11-18T12:00:00.000000Z"),
    ))
    .unwrap_err();
    assert!(err.to_string().contains("someone else"), "{err}");
}

#[test]
fn a_workspace_without_the_preset_is_refused() {
    let dir = prov_testkit::scratch("prov-fhir-import", "no-preset");
    let root = dir.join("ws");
    let builtin = prov::preset::Preset::builtin();
    block_on(prov_fhir::preset::init(&root, "Plain", &builtin)).unwrap();
    let err = block_on(import::import(
        &root,
        &record_from(&fixture(), SHARYN),
        &opts("2026-11-18T12:00:00.000000Z"),
    ))
    .unwrap_err();
    assert!(err.to_string().contains("medical preset"), "{err}");
}

#[test]
fn a_bundle_imports_and_is_kept_whole() {
    let dir = prov_testkit::scratch("prov-fhir-import", "bundle");
    // One patient's lines from the fixture, as the entries of a Bundle.
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(fixture()).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        for line in text.lines() {
            let mine = line.contains(WILFREDO) || !line.contains("\"Patient/");
            let patient_line = line.starts_with(r#"{"resourceType":"Patient""#);
            if (mine && !patient_line) || (patient_line && line.contains(WILFREDO)) {
                entries.push(format!(r#"{{"resource":{line}}}"#));
            }
        }
    }
    let bundle = dir.join("wilfredo.json");
    std::fs::write(
        &bundle,
        format!(
            r#"{{"resourceType":"Bundle","type":"collection","entry":[{}]}}"#,
            entries.join(",")
        ),
    )
    .unwrap();
    let root = workspace(&dir, "Scratch");
    let report = run(
        &root,
        &record_from(&bundle, WILFREDO),
        "2026-11-18T12:00:00.000000Z",
    );
    assert!(Report::total(&report.created) > 10, "{report:?}");
    assert_eq!(check(&root), Vec::<String>::new());
    let kept = root
        .join(report.import.with_extension(""))
        .join("wilfredo.json");
    assert_eq!(
        std::fs::read(kept).unwrap(),
        std::fs::read(&bundle).unwrap()
    );
}
