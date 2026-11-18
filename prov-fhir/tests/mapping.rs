//! The mapping, on the seeded Synthea fixture: what a patient's share of the
//! export is, and what it becomes — before any workspace is involved.
//!
//! The fixture (`tests/fixtures/synthea`) is two patients from
//! `java -jar synthea-with-dependencies.jar -s 42 -p 25 Texas
//! --exporter.fhir.bulk_data=true`, cut down to the resource types the
//! importer maps and the practitioners, organizations and medications those
//! refer to. Synthea's data is synthetic throughout.

use std::path::{Path, PathBuf};

use prov::Value;
use prov_fhir::fhir::{Export, Record};
use prov_fhir::map::{self, Doc, Section, is_emergency};
use prov_fhir::vocab::Vocab;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/synthea")
}

const SHARYN: &str = "c053e996-a4c4-6c02-e2b6-284227156c67";
const WILFREDO: &str = "aee7bbe1-0c45-c028-1e62-1f4cdb30c273";

fn record(id: &str) -> Record {
    Export::read(&fixture())
        .unwrap()
        .record(id)
        .unwrap()
        .unwrap()
}

fn plan(id: &str) -> map::Plan {
    map::plan(&record(id), &Vocab::medical())
}

fn field<'d>(doc: &'d Doc, key: &str) -> Option<&'d str> {
    doc.fields.get(key).and_then(Value::as_str)
}

fn systems(doc: &Doc, key: &str) -> Vec<String> {
    doc.fields
        .get(key)
        .and_then(Value::as_sequence)
        .unwrap_or(&[])
        .iter()
        .filter_map(|c| c.get("system").and_then(Value::as_str).map(str::to_string))
        .collect()
}

#[test]
fn an_export_lists_its_patients() {
    let export = Export::read(&fixture()).unwrap();
    let ids: Vec<&str> = export.patients().map(|p| p.id()).collect();
    assert_eq!(ids, [WILFREDO, SHARYN]);
}

#[test]
fn a_record_is_one_patients_share_and_what_it_refers_to() {
    let r = record(SHARYN);
    assert_eq!(r.patient_id(), SHARYN);
    for resource in &r.resources {
        let subject = resource
            .str(&["subject", "reference"])
            .or_else(|| resource.str(&["patient", "reference"]));
        assert_eq!(subject, Some(format!("Patient/{SHARYN}").as_str()));
    }
    // The shared resources are the ones this patient's resources name: a
    // practitioner, their role, and the clinic.
    let types: Vec<&str> = r.shared.iter().map(|s| s.resource_type()).collect();
    for t in [
        "Practitioner",
        "PractitionerRole",
        "Organization",
        "Medication",
    ] {
        assert!(types.contains(&t), "no {t} in the share: {types:?}");
    }

    // The raw files are the export's own lines, byte for byte.
    let names: Vec<&str> = r.raw.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"Patient.ndjson") && names.contains(&"Encounter.ndjson"));
    let source = std::fs::read_to_string(fixture().join("Encounter.ndjson")).unwrap();
    let (_, mine) = r.raw.iter().find(|(n, _)| n == "Encounter.ndjson").unwrap();
    let mine = String::from_utf8(mine.clone()).unwrap();
    for line in mine.lines() {
        assert!(source.lines().any(|l| l == line));
        assert!(line.contains(SHARYN));
    }
    let patient = r.raw.iter().find(|(n, _)| n == "Patient.ndjson").unwrap();
    assert!(!String::from_utf8_lossy(&patient.1).contains(WILFREDO));
}

#[test]
fn the_digest_names_the_share_and_nothing_else() {
    assert_eq!(record(SHARYN).digest(), record(SHARYN).digest());
    assert_ne!(record(SHARYN).digest(), record(WILFREDO).digest());
}

#[test]
fn each_resource_becomes_the_kind_it_is() {
    let p = plan(SHARYN);
    let count = |s: Section| p.docs.iter().filter(|d| d.section == s).count();
    let r = record(SHARYN);
    let of = |t: &str| {
        r.resources
            .iter()
            .filter(|x| x.resource_type() == t)
            .count()
    };
    assert_eq!(count(Section::Encounters), of("Encounter"));
    assert_eq!(count(Section::Problems), of("Condition"));
    assert_eq!(count(Section::Allergies), 2);
    assert_eq!(count(Section::Immunizations), of("Immunization"));
    assert_eq!(count(Section::Procedures), of("Procedure"));
    for doc in &p.docs {
        let kind = field(doc, "kind");
        let expected = match doc.section {
            Section::Encounters => Some("encounter"),
            Section::Problems => Some("problem"),
            Section::Medications => Some("medication"),
            Section::Allergies => Some("allergy"),
            Section::Immunizations => Some("immunization"),
            Section::Procedures => Some("procedure"),
            Section::Labs => Some("lab"),
            Section::CareTeam | Section::Imports => None,
        };
        assert_eq!(kind, expected, "{}", doc.key);
        // Every document says which FHIR resource it is.
        let fhir = doc.fields.get("fhir").unwrap();
        assert!(fhir.get("resourceType").and_then(Value::as_str).is_some());
        assert!(fhir.get("id").and_then(Value::as_str).is_some());
    }
    // The care team comes first, so a clinician link has a target.
    let first_other = p
        .docs
        .iter()
        .position(|d| d.section != Section::CareTeam)
        .unwrap();
    assert!(
        p.docs[..first_other]
            .iter()
            .all(|d| d.section == Section::CareTeam)
    );
    assert!(first_other > 0);
}

#[test]
fn vital_signs_and_surveys_are_rows_not_documents() {
    let p = plan(SHARYN);
    let r = record(SHARYN);
    let labs = r
        .resources
        .iter()
        .filter(|o| o.resource_type() == "Observation" && map::is_lab(o))
        .count();
    let others = r
        .resources
        .iter()
        .filter(|o| o.resource_type() == "Observation" && !map::is_lab(o))
        .count();
    assert!(others > 0 && labs > 0);
    assert_eq!(p.summarized, others);
    // A lab document is a report (a panel) or a result in no report; fewer
    // than one per result.
    let lab_docs = p.docs.iter().filter(|d| d.section == Section::Labs).count();
    assert!(
        lab_docs > 0 && lab_docs < labs,
        "{lab_docs} lab documents for {labs} results"
    );
    // The vitals are in the encounter that took them.
    let with_table = p
        .docs
        .iter()
        .filter(|d| d.section == Section::Encounters)
        .filter(|d| {
            d.body
                .render(|_, l| Some(l.to_string()))
                .contains("| Body Weight |")
        })
        .count();
    assert!(with_table > 0);
}

#[test]
fn codes_map_to_vocabulary_terms() {
    let p = plan(SHARYN);
    for doc in &p.docs {
        let expected: &[&str] = match doc.section {
            Section::Encounters | Section::Problems | Section::Allergies | Section::Procedures => {
                &["snomed-ct"]
            }
            Section::Medications => &["rxnorm"],
            Section::Immunizations => &["cvx"],
            Section::Labs => &["loinc"],
            _ => continue,
        };
        let got = systems(doc, "codes");
        assert!(!got.is_empty(), "{} has no codes", doc.key);
        assert!(
            got.iter().all(|s| expected.contains(&s.as_str())),
            "{}: {got:?}",
            doc.key
        );
        assert!(systems(doc, "other_codes").is_empty());
    }
    assert!(p.unknown_systems.is_empty(), "{:?}", p.unknown_systems);
}

#[test]
fn medicines_group_their_prescriptions() {
    let p = plan(SHARYN);
    let meds: Vec<&Doc> = p
        .docs
        .iter()
        .filter(|d| d.section == Section::Medications)
        .collect();
    let r = record(SHARYN);
    let requests = r
        .resources
        .iter()
        .filter(|x| x.resource_type() == "MedicationRequest")
        .count();
    assert!(meds.len() <= requests);
    // Keyed by the medicine, whether the request names it inline or by
    // reference to a Medication resource.
    for doc in &meds {
        assert!(
            doc.key
                .starts_with("Medication|http://www.nlm.nih.gov/research/umls/rxnorm|")
        );
        let named = doc.fields["fhir"].get("medication").and_then(Value::as_str);
        assert_eq!(Some(doc.key.trim_start_matches("Medication|")), named);
    }
    let status = |title: &str| {
        meds.iter()
            .find(|d| d.title.starts_with(title))
            .and_then(|d| field(d, "status"))
    };
    assert_eq!(status("Fexofenadine"), Some("active"));
    assert_eq!(status("NDA020800 0.3 ML Epinephrine"), Some("active"));
    assert_eq!(status("Ibuprofen"), Some("stopped"));
}

#[test]
fn problem_statuses_come_from_the_clinical_status() {
    let p = plan(SHARYN);
    let r = record(SHARYN);
    for doc in p.docs.iter().filter(|d| d.section == Section::Problems) {
        let id = doc.fields["fhir"]
            .get("id")
            .and_then(Value::as_str)
            .unwrap();
        let condition = r.resources.iter().find(|c| c.id() == id).unwrap();
        let fhir = condition
            .str(&["clinicalStatus", "coding", "code"])
            .unwrap();
        let expected = match fhir {
            "active" | "recurrence" | "relapse" => "active",
            "inactive" | "remission" => "inactive",
            "resolved" => "resolved",
            other => panic!("unexpected clinical status {other}"),
        };
        assert_eq!(field(doc, "status"), Some(expected));
    }
}

#[test]
fn the_emergency_list_is_the_active_allergies_and_current_medicines() {
    let p = plan(SHARYN);
    let emergency: Vec<&str> = p
        .docs
        .iter()
        .filter(|d| is_emergency(&Value::Mapping(d.fields.clone())))
        .map(|d| d.title.as_str())
        .collect();
    assert_eq!(
        emergency,
        [
            "Allergic disposition",
            "Allergy to peanut",
            "Fexofenadine hydrochloride 30 MG Oral Tablet",
            "NDA020800 0.3 ML Epinephrine 1 MG/ML Auto-Injector",
        ],
        "an active `(finding)` such as `Risk activity involvement` is not on it"
    );
}

#[test]
fn an_encounter_lists_what_it_produced() {
    let p = plan(SHARYN);
    let keys: std::collections::BTreeSet<&str> = p.docs.iter().map(|d| d.key.as_str()).collect();
    let mut linked = 0;
    for doc in p.docs.iter().filter(|d| d.section == Section::Encounters) {
        for key in doc.body.keys() {
            // Every link names a planned document, so none renders as text.
            assert!(
                keys.contains(key),
                "{} links to {key}, which is not planned",
                doc.key
            );
            linked += 1;
        }
        assert!(doc.date_of_service().is_some());
        assert!(systems(doc, "codes").contains(&"snomed-ct".to_string()));
    }
    assert!(linked > 0);
}

/// A Bundle, with codings from a system the vocabulary has no term for.
fn bundle(dir: &Path) -> PathBuf {
    let path = dir.join("bundle.json");
    std::fs::write(
        &path,
        r#"{
  "resourceType": "Bundle",
  "type": "collection",
  "entry": [
    { "fullUrl": "urn:uuid:p1",
      "resource": { "resourceType": "Patient", "id": "p1",
        "name": [{ "given": ["Ada"], "family": "Example" }], "birthDate": "1980-05-01" } },
    { "fullUrl": "urn:uuid:e1",
      "resource": { "resourceType": "Encounter", "id": "e1", "status": "finished",
        "class": { "code": "AMB" },
        "type": [{ "coding": [{ "system": "http://www.ama-assn.org/go/cpt", "code": "99213",
          "display": "Office visit" }] }],
        "subject": { "reference": "urn:uuid:p1" },
        "period": { "start": "2024-03-01T09:00:00-05:00", "end": "2024-03-01T09:20:00-05:00" } } },
    { "fullUrl": "urn:uuid:c1",
      "resource": { "resourceType": "Condition", "id": "c1",
        "clinicalStatus": { "coding": [{ "code": "relapse" }] },
        "code": { "coding": [
          { "system": "http://hl7.org/fhir/sid/icd-10-cm", "code": "I10", "display": "Essential hypertension" },
          { "system": "http://example.org/local-codes", "code": "HTN-7", "display": "High blood pressure" }
        ] },
        "subject": { "reference": "urn:uuid:p1" },
        "encounter": { "reference": "urn:uuid:e1" },
        "onsetDateTime": "2024-03-01" } },
    { "fullUrl": "urn:uuid:c2",
      "resource": { "resourceType": "Condition", "id": "c2",
        "verificationStatus": { "coding": [{ "code": "entered-in-error" }] },
        "code": { "text": "Mistake" },
        "subject": { "reference": "urn:uuid:p1" } } }
  ]
}"#,
    )
    .unwrap();
    path
}

#[test]
fn an_unknown_code_system_is_kept_beside_the_codes_and_counted() {
    let dir = prov_testkit::scratch("prov-fhir-map", "unknown-system");
    let export = Export::read(&bundle(&dir)).unwrap();
    let r = export.record("p1").unwrap().unwrap();
    let p = map::plan(&r, &Vocab::medical());
    assert_eq!(p.patient.name, "Ada Example");
    let problem = p
        .docs
        .iter()
        .find(|d| d.section == Section::Problems)
        .unwrap();
    assert_eq!(systems(problem, "codes"), ["icd-10-cm"]);
    assert_eq!(
        systems(problem, "other_codes"),
        ["http://example.org/local-codes"]
    );
    assert_eq!(
        field(problem, "status"),
        Some("active"),
        "relapse is an active problem"
    );
    assert_eq!(
        p.unknown_systems.get("http://example.org/local-codes"),
        Some(&1)
    );
    // CPT is in the vocabulary.
    let visit = p
        .docs
        .iter()
        .find(|d| d.section == Section::Encounters)
        .unwrap();
    assert_eq!(systems(visit, "codes"), ["cpt"]);
    // A `urn:uuid:` reference from the condition reaches the encounter.
    assert!(problem.body.keys().any(|k| k == "Encounter/e1"));
    // The condition entered in error is not a problem.
    assert_eq!(
        p.docs
            .iter()
            .filter(|d| d.section == Section::Problems)
            .count(),
        1
    );
    assert_eq!(p.skipped.get("Condition entered-in-error"), Some(&1));
    // A Bundle is kept whole.
    assert_eq!(r.raw.len(), 1);
    assert_eq!(r.raw[0].0, "bundle.json");
}
