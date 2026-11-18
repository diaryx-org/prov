//! FHIR resources → the documents a medical workspace keeps.
//!
//! Pure: a [`Record`] and a [`Vocab`] in, a [`Plan`] out, with no workspace in
//! sight. A planned [`Doc`] knows its section, title, file stem, the fields the
//! importer owns, and a body whose links name other documents by their
//! [key](Doc::key) — the writer turns keys into paths once every document has
//! one.
//!
//! What becomes a document, and what does not:
//!
//! | FHIR | Document |
//! |---|---|
//! | Encounter | one per encounter, `kind: encounter`, filed by year under Encounters |
//! | Condition | one per condition, `kind: problem`, under Problems |
//! | MedicationRequest / MedicationStatement | one per *medicine*, `kind: medication`, under Medications; each prescription a row |
//! | AllergyIntolerance | `kind: allergy`, under Allergies |
//! | Immunization | `kind: immunization`, under Immunizations |
//! | Procedure | `kind: procedure`, under Procedures |
//! | DiagnosticReport (laboratory) | one per report, `kind: lab`, filed by year under Labs; its results a table |
//! | Observation (laboratory, in no report) | one per result, `kind: lab`, as above |
//! | Observation (anything else) | a row in its encounter's table, not a document |
//! | Practitioner / Organization | one per one the record names, under "Care team" |

use std::collections::{BTreeMap, BTreeSet, HashMap};

use prov::{Mapping, Value};

use crate::fhir::{Record, Resource, at, person_name, reference_of, str_at};
use crate::vocab::Vocab;

/// Where a document goes: the index it is made under, by title.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Section {
    CareTeam,
    Encounters,
    Problems,
    Medications,
    Allergies,
    Immunizations,
    Procedures,
    Labs,
    Imports,
}

impl Section {
    /// Every section, in the order the indexes are made under the root.
    pub const ALL: &[Section] = &[
        Section::Encounters,
        Section::Problems,
        Section::Medications,
        Section::Allergies,
        Section::Immunizations,
        Section::Procedures,
        Section::Labs,
        Section::CareTeam,
        Section::Imports,
    ];

    /// The index's title — what `under: '[[…]]'` names.
    pub fn title(self) -> &'static str {
        match self {
            Section::CareTeam => "Care team",
            Section::Encounters => "Encounters",
            Section::Problems => "Problems",
            Section::Medications => "Medications",
            Section::Allergies => "Allergies",
            Section::Immunizations => "Immunizations",
            Section::Procedures => "Procedures",
            Section::Labs => "Labs",
            Section::Imports => "Imports",
        }
    }

    /// The filing entry a document of this section is filed through, when
    /// the medical preset declares one.
    pub fn filing(self) -> Option<&'static str> {
        match self {
            Section::Encounters => Some("encounters"),
            Section::Labs => Some("labs"),
            _ => None,
        }
    }
}

/// A run of body text, or a link to another planned document by its key.
#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    Text(String),
    Link { key: String, label: String },
}

/// A document body under construction: text with links by key.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Body(pub Vec<Inline>);

impl Body {
    fn text(&mut self, s: impl Into<String>) -> &mut Self {
        self.0.push(Inline::Text(s.into()));
        self
    }
    fn link(&mut self, key: impl Into<String>, label: impl Into<String>) -> &mut Self {
        self.0.push(Inline::Link {
            key: key.into(),
            label: label.into(),
        });
        self
    }
    /// The keys this body links to.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.iter().filter_map(|i| match i {
            Inline::Link { key, .. } => Some(key.as_str()),
            Inline::Text(_) => None,
        })
    }
    /// Render, asking `link` for the markdown of each link it can make,
    /// given the key and the label already escaped; a link it cannot make is
    /// written as its label.
    pub fn render(&self, mut link: impl FnMut(&str, &str) -> Option<String>) -> String {
        let mut out = String::new();
        for inline in &self.0 {
            match inline {
                Inline::Text(t) => out.push_str(t),
                Inline::Link { key, label } => {
                    let label = cell(label);
                    match link(key, &label) {
                        Some(md) => out.push_str(&md),
                        None => out.push_str(&label),
                    }
                }
            }
        }
        out
    }
}

/// One document the import will make, or bring up to date.
#[derive(Debug, Clone)]
pub struct Doc {
    /// What identifies it across imports: `Encounter/<id>`, …, or
    /// `Medication|<system>|<code>` for a medicine.
    pub key: String,
    pub section: Section,
    pub title: String,
    /// The file name it is made under, without extension.
    pub stem: String,
    /// The fields the importer owns, in the order they are written:
    /// `kind`, `date_of_service`, `status`, `codes`, `other_codes`, `fhir`.
    pub fields: Mapping,
    /// The care-team document its `clinician` names, by key.
    pub clinician: Option<String>,
    pub body: Body,
}

impl Doc {
    /// The value of the field the filing entry cuts — `date_of_service`.
    pub fn date_of_service(&self) -> Option<&str> {
        self.fields.get("date_of_service").and_then(Value::as_str)
    }
}

/// The patient, as the root document carries them.
#[derive(Debug, Clone, PartialEq)]
pub struct Patient {
    pub id: String,
    pub name: String,
    pub birth_date: Option<String>,
    pub gender: Option<String>,
    pub deceased: Option<String>,
}

/// What [`plan`] made of a record.
#[derive(Debug, Clone)]
pub struct Plan {
    pub patient: Patient,
    /// Care-team documents first, then the rest in export order.
    pub docs: Vec<Doc>,
    /// Resources not made into documents, by why.
    pub skipped: BTreeMap<String, usize>,
    /// Codings whose system the vocabulary has no term for, by system URI:
    /// kept under `other_codes`, left out of `codes`.
    pub unknown_systems: BTreeMap<String, usize>,
    /// Observations summarized into an encounter's table.
    pub summarized: usize,
}

/// The most rows a medicine's prescription table lists; the rest are counted.
pub const PRESCRIPTION_ROWS: usize = 50;

/// Map a patient's record to the documents it becomes.
pub fn plan(record: &Record, vocab: &Vocab) -> Plan {
    Mapper::new(record, vocab).run()
}

struct Mapper<'a> {
    record: &'a Record,
    vocab: &'a Vocab,
    /// `Type/id` of every mapped resource, by bare id (a Bundle's
    /// `urn:uuid:` references carry only the id).
    keys_by_id: HashMap<&'a str, String>,
    resources: HashMap<String, &'a Resource>,
    skipped: BTreeMap<String, usize>,
    unknown_systems: BTreeMap<String, usize>,
    /// Care-team documents some document names, by key.
    care: BTreeSet<String>,
    /// What each encounter produced: section, key, label, note.
    produced: BTreeMap<String, Vec<(Section, String, String, String)>>,
    /// Each encounter's summarized observations: category, name, value.
    observed: BTreeMap<String, Vec<(String, String, String)>>,
    summarized: usize,
}

impl<'a> Mapper<'a> {
    fn new(record: &'a Record, vocab: &'a Vocab) -> Self {
        let mut keys_by_id = HashMap::new();
        let mut resources = HashMap::new();
        for r in &record.resources {
            keys_by_id.insert(r.id(), r.key());
            resources.insert(r.key(), r);
        }
        Self {
            record,
            vocab,
            keys_by_id,
            resources,
            skipped: BTreeMap::new(),
            unknown_systems: BTreeMap::new(),
            care: BTreeSet::new(),
            produced: BTreeMap::new(),
            observed: BTreeMap::new(),
            summarized: 0,
        }
    }

    fn skip(&mut self, why: impl Into<String>) {
        *self.skipped.entry(why.into()).or_default() += 1;
    }

    /// The `Type/id` a reference names among the record's own resources.
    fn key_of(&self, reference: &str) -> Option<String> {
        if let Some(id) = reference.strip_prefix("urn:uuid:") {
            return self.keys_by_id.get(id).cloned();
        }
        self.resources
            .contains_key(reference)
            .then(|| reference.to_string())
    }

    /// The encounter a resource was recorded at, as a key.
    fn encounter_of(&self, r: &Resource) -> Option<String> {
        r.str(&["encounter", "reference"])
            .or_else(|| r.str(&["context", "reference"]))
            .and_then(|e| self.key_of(e))
            .filter(|k| k.starts_with("Encounter/"))
    }

    /// The care-team key a practitioner or organization reference names,
    /// noted as needed.
    fn care_key(&mut self, reference: Option<&str>) -> Option<String> {
        let shared = self.record.shared(reference?)?;
        if !matches!(shared.resource_type(), "Practitioner" | "Organization") {
            return None;
        }
        let key = shared.key();
        self.care.insert(key.clone());
        Some(key)
    }

    /// `codes` and `other_codes` from a CodeableConcept.
    fn codes(&mut self, concept: Option<&Value>, codes: &mut Vec<Value>, other: &mut Vec<Value>) {
        let Some(concept) = concept else { return };
        // `Encounter.type` and the like are lists of concepts.
        let concepts = match concept.as_sequence() {
            Some(items) => items.iter().collect(),
            None => vec![concept],
        };
        let codings = concepts
            .iter()
            .flat_map(|c| c.get("coding").and_then(Value::as_sequence).unwrap_or(&[]));
        for coding in codings {
            let Some(code) = coding.get("code").and_then(scalar) else {
                continue;
            };
            let display = str_at(coding, &["display"]);
            let uri = str_at(coding, &["system"]).unwrap_or_default();
            let mut entry = Mapping::new();
            match self.vocab.system(uri) {
                Some(term) => {
                    entry.insert("system".into(), Value::String(term.into()));
                    entry.insert("code".into(), Value::String(code));
                    if let Some(d) = display {
                        entry.insert("display".into(), Value::String(d.into()));
                    }
                    let entry = Value::Mapping(entry);
                    if !codes.contains(&entry) {
                        codes.push(entry);
                    }
                }
                None => {
                    *self
                        .unknown_systems
                        .entry(if uri.is_empty() {
                            "(none)".into()
                        } else {
                            uri.into()
                        })
                        .or_default() += 1;
                    entry.insert("system".into(), Value::String(uri.into()));
                    entry.insert("code".into(), Value::String(code));
                    if let Some(d) = display {
                        entry.insert("display".into(), Value::String(d.into()));
                    }
                    let entry = Value::Mapping(entry);
                    if !other.contains(&entry) {
                        other.push(entry);
                    }
                }
            }
        }
    }

    /// The fields every document opens with: `kind`, `date_of_service`,
    /// `status`, `codes`, `other_codes`, `fhir`.
    fn fields(
        &mut self,
        kind: Option<&str>,
        date: Option<String>,
        status: Option<&str>,
        concepts: &[Option<&Value>],
        fhir: Mapping,
    ) -> Mapping {
        let mut f = Mapping::new();
        if let Some(kind) = kind {
            f.insert("kind".into(), Value::String(kind.into()));
        }
        if let Some(date) = date {
            f.insert("date_of_service".into(), Value::String(date));
        }
        if let Some(status) = status {
            f.insert("status".into(), Value::String(status.into()));
        }
        let (mut codes, mut other) = (Vec::new(), Vec::new());
        for concept in concepts {
            self.codes(*concept, &mut codes, &mut other);
        }
        if !codes.is_empty() {
            f.insert("codes".into(), Value::Sequence(codes));
        }
        if !other.is_empty() {
            f.insert("other_codes".into(), Value::Sequence(other));
        }
        f.insert("fhir".into(), Value::Mapping(fhir));
        f
    }

    fn produced(
        &mut self,
        encounter: Option<String>,
        section: Section,
        key: &str,
        label: &str,
        note: String,
    ) {
        if let Some(enc) = encounter {
            self.produced.entry(enc).or_default().push((
                section,
                key.to_string(),
                label.to_string(),
                note,
            ));
        }
    }

    fn run(mut self) -> Plan {
        let record = self.record;
        let mut docs = Vec::new();

        // Laboratory reports, and the results they hold.
        let mut in_report: BTreeSet<String> = BTreeSet::new();
        for r in &record.resources {
            if r.resource_type() == "DiagnosticReport" && is_lab_report(r) {
                for result in r.list(&["result"]) {
                    if let Some(k) = str_at(result, &["reference"]).and_then(|x| self.key_of(x)) {
                        in_report.insert(k);
                    }
                }
            }
        }

        let mut medications: BTreeMap<String, Vec<&Resource>> = BTreeMap::new();
        let mut medication_order: Vec<String> = Vec::new();

        for r in &record.resources {
            let status = r.str(&["status"]).unwrap_or_default();
            if status == "entered-in-error" {
                self.skip(format!("{} entered in error", r.resource_type()));
                continue;
            }
            match r.resource_type() {
                "Encounter" => {} // below, once everything it produced is known
                "Condition" => {
                    let verification = r
                        .str(&["verificationStatus", "coding", "code"])
                        .unwrap_or_default();
                    if matches!(verification, "refuted" | "entered-in-error") {
                        self.skip(format!("Condition {verification}"));
                        continue;
                    }
                    docs.push(self.condition(r));
                }
                "MedicationRequest" | "MedicationStatement" => {
                    let Some((key, _, _)) = self.medication_identity(r) else {
                        self.skip(format!("{} naming no medication", r.resource_type()));
                        continue;
                    };
                    if !medications.contains_key(&key) {
                        medication_order.push(key.clone());
                    }
                    medications.entry(key).or_default().push(r);
                }
                "AllergyIntolerance" => docs.push(self.allergy(r)),
                "Immunization" => {
                    if status == "not-done" {
                        self.skip("Immunization not done");
                        continue;
                    }
                    docs.push(self.immunization(r));
                }
                "Procedure" => {
                    if matches!(status, "not-done" | "preparation") {
                        self.skip(format!("Procedure {status}"));
                        continue;
                    }
                    docs.push(self.procedure(r));
                }
                "DiagnosticReport" => {
                    if is_lab_report(r) {
                        docs.push(self.lab_report(r));
                    } else {
                        self.skip("DiagnosticReport (not laboratory)");
                    }
                }
                "Observation" => {
                    if is_lab(r) {
                        if !in_report.contains(&r.key()) {
                            docs.push(self.lab_result(r));
                        }
                    } else if let Some(enc) = self.encounter_of(r) {
                        let category = categories(r).first().cloned().unwrap_or_default();
                        let name =
                            concept_text(r.at(&["code"])).unwrap_or_else(|| "Observation".into());
                        let rows = self.observed.entry(enc).or_default();
                        let components = r.list(&["component"]);
                        if components.len() > 2 && r.at(&["valueQuantity"]).is_none() {
                            // A questionnaire panel: a row for it, and one per answer.
                            rows.push((category.clone(), name, String::new()));
                            for c in components {
                                let q = concept_text(c.get("code")).unwrap_or_default();
                                rows.push((
                                    category.clone(),
                                    format!("· {q}"),
                                    observation_value(c),
                                ));
                            }
                        } else {
                            rows.push((category, name, observation_value(&r.value)));
                        }
                        self.summarized += 1;
                    } else {
                        self.skip("Observation (not laboratory) at no encounter");
                    }
                }
                other => self.skip(format!("{other} (not mapped)")),
            }
        }
        for key in medication_order {
            let requests = medications.remove(&key).unwrap_or_default();
            docs.push(self.medication(&key, &requests));
        }

        let mut encounters = Vec::new();
        for r in &record.resources {
            if r.resource_type() == "Encounter" {
                match r.str(&["status"]).unwrap_or_default() {
                    "entered-in-error" => {}
                    "cancelled" => self.skip("Encounter cancelled"),
                    _ => encounters.push(self.encounter(r)),
                }
            }
        }

        // The care team: everyone a document names, and the organizations
        // their roles name.
        let mut care_docs = Vec::new();
        let mut done = BTreeSet::new();
        while let Some(key) = self.care.iter().find(|k| !done.contains(*k)).cloned() {
            done.insert(key.clone());
            if let Some(doc) = self.care_member(&key) {
                care_docs.push(doc);
            }
        }
        care_docs.sort_by(|a, b| a.key.cmp(&b.key));

        let mut all = care_docs;
        all.extend(encounters);
        all.extend(docs);
        Plan {
            patient: patient(&record.patient),
            docs: all,
            skipped: self.skipped,
            unknown_systems: self.unknown_systems,
            summarized: self.summarized,
        }
    }

    fn encounter(&mut self, r: &Resource) -> Doc {
        let key = r.key();
        let concept = r.at(&["type"]);
        let class = r.str(&["class", "code"]).map(encounter_class);
        let title = concept_text(concept)
            .map(|t| clean_title(&t))
            .or_else(|| class.map(|c| format!("{c} visit")))
            .unwrap_or_else(|| "Encounter".into());
        let start = r.str(&["period", "start"]);
        let date = start.and_then(date_of);
        let clinician = {
            let parts = r.list(&["participant"]);
            let primary = parts
                .iter()
                .find(|p| str_at(p, &["type", "coding", "code"]) == Some("PPRF"))
                .or_else(|| parts.first());
            let reference = primary.and_then(|p| str_at(p, &["individual", "reference"]));
            self.care_key(reference)
        };
        let provider = self.care_key(r.str(&["serviceProvider", "reference"]));
        let mut fhir = fhir_id(r);
        if let Some(class) = r.str(&["class", "code"]) {
            fhir.insert("class".into(), Value::String(class.into()));
        }
        let fields = self.fields(Some("encounter"), date.clone(), None, &[concept], fhir);

        let mut body = Body::default();
        body.text(format!("\n# {}\n\n", escape_md(&title)));
        let mut lead = String::from(class.unwrap_or("Visit"));
        lead.push_str(" visit");
        if let Some(start) = start {
            lead.push_str(&format!(", {}", when(start, r.str(&["period", "end"]))));
        }
        body.text(escape_md(&lead));
        if let Some(c) = &clinician {
            body.text(" with ");
            let label = self.care_label(c);
            body.link(c.clone(), label);
        }
        if let Some(p) = &provider {
            body.text(" at ");
            let label = self.care_label(p);
            body.link(p.clone(), label);
        } else if let Some(loc) = r.str(&["location", "location", "display"]) {
            body.text(format!(" at {}", escape_md(loc)));
        }
        body.text(".\n");

        let mut reasons = Vec::new();
        for reason in r.list(&["reasonReference"]) {
            if let Some(k) = str_at(reason, &["reference"]).and_then(|x| self.key_of(x)) {
                let label = self
                    .resources
                    .get(&k)
                    .and_then(|c| concept_text(c.at(&["code"])))
                    .map(|t| clean_title(&t))
                    .unwrap_or_else(|| "a recorded problem".into());
                reasons.push(Inline::Link { key: k, label });
            }
        }
        if reasons.is_empty() {
            for reason in r.list(&["reasonCode"]) {
                if let Some(t) = concept_text(Some(reason)) {
                    reasons.push(Inline::Text(escape_md(&clean_title(&t))));
                }
            }
        }
        if !reasons.is_empty() {
            body.text("\nReason: ");
            for (i, reason) in reasons.into_iter().enumerate() {
                if i > 0 {
                    body.text("; ");
                }
                body.0.push(reason);
            }
            body.text(".\n");
        }

        if let Some(rows) = self.observed.get(&key)
            && !rows.is_empty()
        {
            body.text("\n## Observations\n\n| Observation | Value |\n| --- | --- |\n");
            for (_, name, value) in rows {
                body.text(format!("| {} | {} |\n", cell(name), cell(value)));
            }
        }

        let mut produced = self.produced.remove(&key).unwrap_or_default();
        produced.sort_by_key(|(section, ..)| *section);
        let mut last = None;
        for (section, k, label, note) in produced {
            if last != Some(section) {
                let heading = match section {
                    Section::Problems => "Problems",
                    Section::Medications => "Medications",
                    Section::Allergies => "Allergies",
                    Section::Immunizations => "Immunizations",
                    Section::Procedures => "Procedures",
                    Section::Labs => "Lab results",
                    _ => section.title(),
                };
                body.text(format!("\n## {heading}\n\n"));
                last = Some(section);
            }
            body.text("- ");
            body.link(k, label);
            if !note.is_empty() {
                body.text(format!(" — {}", escape_md(&note)));
            }
            body.text("\n");
        }

        Doc {
            stem: stem(date.as_deref(), &title),
            key,
            section: Section::Encounters,
            title,
            fields,
            clinician,
            body,
        }
    }

    fn condition(&mut self, r: &Resource) -> Doc {
        let concept = r.at(&["code"]);
        let title = concept_text(concept)
            .map(|t| clean_title(&t))
            .unwrap_or_else(|| "Condition".into());
        let clinical = r.str(&["clinicalStatus", "coding", "code"]);
        let status = clinical.and_then(|c| self.vocab.problem_status(c));
        if clinical.is_some() && status.is_none() {
            self.skip_status("Condition", clinical);
        }
        let onset = r
            .str(&["onsetDateTime"])
            .or_else(|| r.str(&["onsetPeriod", "start"]))
            .or_else(|| r.str(&["recordedDate"]));
        let date = onset.and_then(date_of);
        let clinician = self.care_key(
            r.str(&["asserter", "reference"])
                .or_else(|| r.str(&["recorder", "reference"])),
        );
        let encounter = self.encounter_of(r);
        let fields = self.fields(
            Some("problem"),
            date.clone(),
            status,
            &[concept],
            fhir_id(r),
        );

        let mut body = Body::default();
        body.text(format!("\n# {}\n\n", escape_md(&title)));
        body.text(code_line(concept));
        let mut facts = Vec::new();
        if let Some(c) = clinical {
            facts.push(format!("Clinical status: {c}"));
        }
        if let Some(v) = r.str(&["verificationStatus", "coding", "code"]) {
            facts.push(format!("Verification: {v}"));
        }
        if let Some(o) = onset {
            facts.push(format!("Onset: {}", when(o, None)));
        }
        if let Some(a) = r.str(&["abatementDateTime"]) {
            facts.push(format!("Abated: {}", when(a, None)));
        }
        for fact in facts {
            body.text(format!("- {}\n", escape_md(&fact)));
        }
        self.recorded_at(&mut body, &encounter);
        let key = r.key();
        let note = match (&date, status) {
            (Some(d), Some(s)) => format!("{s}, onset {d}"),
            (None, Some(s)) => s.to_string(),
            (Some(d), None) => format!("onset {d}"),
            (None, None) => String::new(),
        };
        self.produced(encounter, Section::Problems, &key, &title, note);
        Doc {
            stem: stem(date.as_deref(), &title),
            key,
            section: Section::Problems,
            title,
            fields,
            clinician,
            body,
        }
    }

    fn skip_status(&mut self, what: &str, status: Option<&str>) {
        *self
            .skipped
            .entry(format!(
                "{what} status `{}` (no term; written without `status`)",
                status.unwrap_or_default()
            ))
            .or_default() += 1;
    }

    /// The medicine a request or statement names: its key, its concept and
    /// its display text.
    fn medication_identity(&self, r: &'a Resource) -> Option<(String, &'a Value, String)> {
        let concept = match r.at(&["medicationCodeableConcept"]) {
            Some(c) => c,
            None => {
                let reference = r.str(&["medicationReference", "reference"])?;
                let medication = self.record.shared(reference)?;
                // The record outlives the mapper, so this borrow is of the record.
                let medication: &'a Resource = medication;
                medication.at(&["code"])?
            }
        };
        let text = concept_text(Some(concept))?;
        let coding = concept
            .get("coding")
            .and_then(Value::as_sequence)
            .and_then(|c| c.first());
        let key = match coding
            .and_then(|c| Some((str_at(c, &["system"])?, c.get("code").and_then(scalar)?)))
        {
            Some((system, code)) => format!("Medication|{system}|{code}"),
            None => format!("Medication|text|{text}"),
        };
        Some((key, concept, text))
    }

    fn medication(&mut self, key: &str, requests: &[&'a Resource]) -> Doc {
        let (_, concept, text) = self
            .medication_identity(requests[0])
            .expect("grouped by a medication it names");
        let title = clean_title(&text);
        let date_of = |r: &Resource| -> Option<String> {
            r.str(&["authoredOn"])
                .or_else(|| r.str(&["effectiveDateTime"]))
                .or_else(|| r.str(&["effectivePeriod", "start"]))
                .or_else(|| r.str(&["dateAsserted"]))
                .map(str::to_string)
        };
        let mut ordered: Vec<&Resource> = requests.to_vec();
        ordered.sort_by_key(|r| date_of(r));
        let latest = *ordered.last().expect("a group is never empty");
        let first_date = ordered
            .iter()
            .find_map(|r| date_of(r))
            .and_then(|d| date_of_str(&d));
        let fhir_status = latest.str(&["status"]);
        let status = fhir_status.and_then(|s| self.vocab.medication_status(s));
        if fhir_status.is_some() && status.is_none() {
            self.skip_status("Medication", fhir_status);
        }
        let clinician = self.care_key(
            latest
                .str(&["requester", "reference"])
                .or_else(|| latest.str(&["informationSource", "reference"])),
        );
        let mut fhir = fhir_id(latest);
        fhir.insert(
            "medication".into(),
            Value::String(key.trim_start_matches("Medication|").into()),
        );
        let fields = self.fields(
            Some("medication"),
            first_date.clone(),
            status,
            &[Some(concept)],
            fhir,
        );

        let mut body = Body::default();
        body.text(format!("\n# {}\n\n", escape_md(&title)));
        body.text(code_line(Some(concept)));
        let n = ordered.len();
        let span = match (ordered.first().and_then(|r| date_of(r)), date_of(latest)) {
            (Some(a), Some(b)) if date_of_str(&a) != date_of_str(&b) => format!(
                ", {} to {}",
                date_of_str(&a).unwrap_or(a),
                date_of_str(&b).unwrap_or(b)
            ),
            (_, Some(b)) => format!(", {}", date_of_str(&b).unwrap_or(b)),
            _ => String::new(),
        };
        let what = if latest.resource_type() == "MedicationStatement" {
            "Recorded"
        } else {
            "Prescribed"
        };
        body.text(format!(
            "{what} {}{span}. ",
            if n == 1 {
                "once".to_string()
            } else {
                format!("{n} times")
            }
        ));
        match (fhir_status, status) {
            (Some(f), Some(s)) => {
                body.text(format!("The most recent is {f}, so this reads as {s}.\n"))
            }
            (Some(f), None) => body.text(format!("The most recent is {f}.\n")),
            _ => body.text("\n"),
        };
        body.text(
            "\n| Date | Status | By | At | For | Dosage |\n| --- | --- | --- | --- | --- | --- |\n",
        );
        for r in ordered.iter().rev().take(PRESCRIPTION_ROWS) {
            let date = date_of(r).and_then(|d| date_of_str(&d)).unwrap_or_default();
            body.text(format!(
                "| {} | {} | ",
                date,
                cell(r.str(&["status"]).unwrap_or_default())
            ));
            match self.care_key(r.str(&["requester", "reference"])) {
                Some(k) => {
                    let label = self.care_label(&k);
                    body.link(k, label);
                }
                None => {
                    body.text(cell(r.str(&["requester", "display"]).unwrap_or_default()));
                }
            }
            body.text(" | ");
            let encounter = self.encounter_of(r);
            if let Some(e) = &encounter {
                let label = self.encounter_label(e);
                body.link(e.clone(), label);
            }
            body.text(" | ");
            let mut first = true;
            for reason in r.list(&["reasonReference"]) {
                if let Some(k) = str_at(reason, &["reference"]).and_then(|x| self.key_of(x)) {
                    let label = self
                        .resources
                        .get(&k)
                        .and_then(|c| concept_text(c.at(&["code"])))
                        .map(|t| clean_title(&t))
                        .unwrap_or_else(|| "problem".into());
                    if !first {
                        body.text(", ");
                    }
                    first = false;
                    body.link(k, label);
                }
            }
            if first {
                for reason in r.list(&["reasonCode"]) {
                    if let Some(t) = concept_text(Some(reason)) {
                        body.text(cell(&clean_title(&t)));
                        break;
                    }
                }
            }
            body.text(format!(" | {} |\n", cell(&dosage(r))));
            self.produced(
                encounter,
                Section::Medications,
                key,
                &title,
                format!(
                    "{} ({})",
                    what.to_lowercase(),
                    r.str(&["status"]).unwrap_or("no status")
                ),
            );
        }
        if n > PRESCRIPTION_ROWS {
            body.text(format!(
                "\n{} earlier rows are in the import this was read from.\n",
                n - PRESCRIPTION_ROWS
            ));
        }
        // Every request names its encounter, the ones past the table too.
        for r in ordered.iter().rev().skip(PRESCRIPTION_ROWS) {
            let encounter = self.encounter_of(r);
            self.produced(
                encounter,
                Section::Medications,
                key,
                &title,
                format!(
                    "{} ({})",
                    what.to_lowercase(),
                    r.str(&["status"]).unwrap_or("no status")
                ),
            );
        }
        Doc {
            stem: link_slug(&title),
            key: key.to_string(),
            section: Section::Medications,
            title,
            fields,
            clinician,
            body,
        }
    }

    fn allergy(&mut self, r: &Resource) -> Doc {
        let concept = r.at(&["code"]);
        let substance = concept_text(concept)
            .map(|t| clean_title(&t))
            .unwrap_or_else(|| "Unknown substance".into());
        let title = if substance.to_lowercase().contains("allerg") {
            substance
        } else {
            format!("Allergy to {}", lower_first(&substance))
        };
        let recorded = r
            .str(&["onsetDateTime"])
            .or_else(|| r.str(&["recordedDate"]));
        let date = recorded.and_then(date_of);
        let clinical = r.str(&["clinicalStatus", "coding", "code"]);
        let mut fhir = fhir_id(r);
        if let Some(c) = clinical {
            fhir.insert("clinicalStatus".into(), Value::String(c.into()));
        }
        let clinician = self.care_key(
            r.str(&["asserter", "reference"])
                .or_else(|| r.str(&["recorder", "reference"])),
        );
        let encounter = self.encounter_of(r);
        let fields = self.fields(Some("allergy"), date.clone(), None, &[concept], fhir);
        let mut body = Body::default();
        body.text(format!("\n# {}\n\n", escape_md(&title)));
        body.text(code_line(concept));
        let mut facts = Vec::new();
        if let Some(c) = clinical {
            facts.push(format!("Clinical status: {c}"));
        }
        for (label, path) in [("Type", "type"), ("Criticality", "criticality")] {
            if let Some(v) = r.str(&[path]) {
                facts.push(format!("{label}: {v}"));
            }
        }
        let categories: Vec<&str> = r
            .list(&["category"])
            .iter()
            .filter_map(Value::as_str)
            .collect();
        if !categories.is_empty() {
            facts.push(format!("Category: {}", categories.join(", ")));
        }
        if let Some(d) = recorded {
            facts.push(format!("Recorded: {}", when(d, None)));
        }
        for reaction in r.list(&["reaction"]) {
            let what: Vec<String> = reaction
                .get("manifestation")
                .and_then(Value::as_sequence)
                .unwrap_or(&[])
                .iter()
                .filter_map(|m| concept_text(Some(m)))
                .map(|t| clean_title(&t))
                .collect();
            let severity = str_at(reaction, &["severity"]);
            if !what.is_empty() {
                facts.push(match severity {
                    Some(s) => format!("Reaction: {} ({s})", what.join(", ")),
                    None => format!("Reaction: {}", what.join(", ")),
                });
            }
        }
        for fact in facts {
            body.text(format!("- {}\n", escape_md(&fact)));
        }
        self.recorded_at(&mut body, &encounter);
        let key = r.key();
        self.produced(encounter, Section::Allergies, &key, &title, String::new());
        Doc {
            stem: link_slug(&title),
            key,
            section: Section::Allergies,
            title,
            fields,
            clinician,
            body,
        }
    }

    fn immunization(&mut self, r: &Resource) -> Doc {
        let concept = r.at(&["vaccineCode"]);
        let title = concept_text(concept)
            .map(|t| clean_title(&t))
            .unwrap_or_else(|| "Immunization".into());
        let given = r
            .str(&["occurrenceDateTime"])
            .or_else(|| r.str(&["recorded"]));
        let date = given.and_then(date_of);
        let clinician = self.care_key(r.str(&["performer", "actor", "reference"]));
        let encounter = self.encounter_of(r);
        let fields = self.fields(
            Some("immunization"),
            date.clone(),
            None,
            &[concept],
            fhir_id(r),
        );
        let mut body = Body::default();
        body.text(format!("\n# {}\n\n", escape_md(&title)));
        body.text(code_line(concept));
        if let Some(g) = given {
            body.text(format!("- Given: {}\n", escape_md(&when(g, None))));
        }
        if let Some(s) = r.str(&["status"]) {
            body.text(format!("- Status: {}\n", escape_md(s)));
        }
        if r.at(&["primarySource"]).and_then(Value::as_bool) == Some(false) {
            body.text("- Reported, not administered by the recorder\n");
        }
        self.recorded_at(&mut body, &encounter);
        let key = r.key();
        self.produced(
            encounter,
            Section::Immunizations,
            &key,
            &title,
            String::new(),
        );
        Doc {
            stem: stem(date.as_deref(), &title),
            key,
            section: Section::Immunizations,
            title,
            fields,
            clinician,
            body,
        }
    }

    fn procedure(&mut self, r: &Resource) -> Doc {
        let concept = r.at(&["code"]);
        let title = concept_text(concept)
            .map(|t| clean_title(&t))
            .unwrap_or_else(|| "Procedure".into());
        let start = r
            .str(&["performedPeriod", "start"])
            .or_else(|| r.str(&["performedDateTime"]));
        let date = start.and_then(date_of);
        let clinician = {
            let reference = r
                .list(&["performer"])
                .iter()
                .filter_map(|p| str_at(p, &["actor", "reference"]))
                .find(|reference| {
                    self.record
                        .shared(reference)
                        .is_some_and(|s| s.resource_type() == "Practitioner")
                })
                .map(str::to_string);
            self.care_key(reference.as_deref())
        };
        let encounter = self.encounter_of(r);
        let fields = self.fields(
            Some("procedure"),
            date.clone(),
            None,
            &[concept],
            fhir_id(r),
        );
        let mut body = Body::default();
        body.text(format!("\n# {}\n\n", escape_md(&title)));
        body.text(code_line(concept));
        if let Some(s) = start {
            body.text(format!(
                "- Performed: {}\n",
                escape_md(&when(s, r.str(&["performedPeriod", "end"])))
            ));
        }
        if let Some(s) = r.str(&["status"]) {
            body.text(format!("- Status: {}\n", escape_md(s)));
        }
        for reason in r.list(&["reasonReference"]) {
            if let Some(k) = str_at(reason, &["reference"]).and_then(|x| self.key_of(x)) {
                let label = self
                    .resources
                    .get(&k)
                    .and_then(|c| concept_text(c.at(&["code"])))
                    .map(|t| clean_title(&t))
                    .unwrap_or_else(|| "a recorded problem".into());
                body.text("- For: ");
                body.link(k, label);
                body.text("\n");
            }
        }
        for reason in r.list(&["reasonCode"]) {
            if let Some(t) = concept_text(Some(reason)) {
                body.text(format!("- Reason: {}\n", escape_md(&clean_title(&t))));
            }
        }
        if let Some(l) = r.str(&["location", "display"]) {
            body.text(format!("- Where: {}\n", escape_md(l)));
        }
        self.recorded_at(&mut body, &encounter);
        let key = r.key();
        self.produced(encounter, Section::Procedures, &key, &title, String::new());
        Doc {
            stem: stem(date.as_deref(), &title),
            key,
            section: Section::Procedures,
            title,
            fields,
            clinician,
            body,
        }
    }

    fn lab_report(&mut self, r: &Resource) -> Doc {
        let concept = r.at(&["code"]);
        let title = concept_text(concept)
            .map(|t| clean_title(&t))
            .unwrap_or_else(|| "Laboratory report".into());
        let effective = r
            .str(&["effectiveDateTime"])
            .or_else(|| r.str(&["effectivePeriod", "start"]))
            .or_else(|| r.str(&["issued"]));
        let date = effective.and_then(date_of);
        let results: Vec<&Resource> = r
            .list(&["result"])
            .iter()
            .filter_map(|x| str_at(x, &["reference"]))
            .filter_map(|x| self.key_of(x))
            .filter_map(|k| self.resources.get(&k).copied())
            .collect();
        let mut concepts = vec![concept];
        concepts.extend(results.iter().map(|o| o.at(&["code"])));
        let (clinician, performer) = self.lab_performer(r);
        let encounter = self.encounter_of(r);
        let fields = self.fields(Some("lab"), date.clone(), None, &concepts, fhir_id(r));
        let mut body = Body::default();
        body.text(format!("\n# {}\n\n", escape_md(&title)));
        body.text(code_line(concept));
        self.lab_lead(&mut body, r, effective, performer);
        lab_table(&mut body, &results);
        self.recorded_at(&mut body, &encounter);
        let key = r.key();
        self.produced(encounter, Section::Labs, &key, &title, String::new());
        Doc {
            stem: stem(date.as_deref(), &title),
            key,
            section: Section::Labs,
            title,
            fields,
            clinician,
            body,
        }
    }

    fn lab_result(&mut self, r: &Resource) -> Doc {
        let concept = r.at(&["code"]);
        let title = concept_text(concept)
            .map(|t| clean_title(&t))
            .unwrap_or_else(|| "Laboratory result".into());
        let effective = r
            .str(&["effectiveDateTime"])
            .or_else(|| r.str(&["effectivePeriod", "start"]))
            .or_else(|| r.str(&["issued"]));
        let date = effective.and_then(date_of);
        let (clinician, performer) = self.lab_performer(r);
        let encounter = self.encounter_of(r);
        let fields = self.fields(Some("lab"), date.clone(), None, &[concept], fhir_id(r));
        let mut body = Body::default();
        body.text(format!("\n# {}\n\n", escape_md(&title)));
        body.text(code_line(concept));
        self.lab_lead(&mut body, r, effective, performer);
        lab_table(&mut body, &[r]);
        self.recorded_at(&mut body, &encounter);
        let key = r.key();
        self.produced(encounter, Section::Labs, &key, &title, String::new());
        Doc {
            stem: stem(date.as_deref(), &title),
            key,
            section: Section::Labs,
            title,
            fields,
            clinician,
            body,
        }
    }

    /// A lab's performers: the practitioner, as `clinician`, and whoever
    /// performed it (a laboratory, often), for the body.
    fn lab_performer(&mut self, r: &Resource) -> (Option<String>, Option<String>) {
        let mut clinician = None;
        let mut performer = None;
        for p in r.list(&["performer"]) {
            let reference = str_at(p, &["reference"]);
            let kind = reference
                .and_then(|x| self.record.shared(x))
                .map(|s| s.resource_type().to_string());
            match kind.as_deref() {
                Some("Practitioner") if clinician.is_none() => clinician = self.care_key(reference),
                Some(_) if performer.is_none() => performer = self.care_key(reference),
                _ => {}
            }
        }
        (clinician, performer)
    }

    fn lab_lead(
        &mut self,
        body: &mut Body,
        r: &Resource,
        effective: Option<&str>,
        performer: Option<String>,
    ) {
        let mut lead = String::new();
        if let Some(s) = r.str(&["status"]) {
            lead.push_str(&upper_first(s));
            lead.push_str(" result");
        } else {
            lead.push_str("Result");
        }
        if let Some(e) = effective {
            lead.push_str(&format!(", {}", when(e, None)));
        }
        body.text(escape_md(&lead));
        if let Some(p) = performer {
            body.text(", performed by ");
            let label = self.care_label(&p);
            body.link(p, label);
        }
        body.text(".\n\n");
    }

    fn recorded_at(&self, body: &mut Body, encounter: &Option<String>) {
        if let Some(e) = encounter {
            body.text("\nRecorded at ");
            let label = self.encounter_label(e);
            body.link(e.clone(), label);
            body.text(".\n");
        }
    }

    /// How a link to an encounter reads: its title and date.
    fn encounter_label(&self, key: &str) -> String {
        let Some(r) = self.resources.get(key) else {
            return "an encounter".into();
        };
        let title = concept_text(r.at(&["type"]))
            .map(|t| clean_title(&t))
            .unwrap_or_else(|| "Encounter".into());
        match r.str(&["period", "start"]).and_then(date_of) {
            Some(d) => format!("{title}, {d}"),
            None => title,
        }
    }

    /// How a link to a care-team member reads: their name.
    fn care_label(&self, key: &str) -> String {
        self.record
            .shared(key)
            .map(care_title)
            .unwrap_or_else(|| key.to_string())
    }

    fn care_member(&mut self, key: &str) -> Option<Doc> {
        let r = self.record.shared(key)?;
        let title = care_title(r);
        let mut body = Body::default();
        body.text(format!("\n# {}\n\n", escape_md(&title)));
        let mut facts = Vec::new();
        for identifier in r.list(&["identifier"]) {
            if str_at(identifier, &["system"]) == Some("http://hl7.org/fhir/sid/us-npi")
                && let Some(v) = str_at(identifier, &["value"])
            {
                facts.push(format!("NPI: {v}"));
            }
        }
        if r.resource_type() == "Organization" {
            for t in r.list(&["type"]) {
                if let Some(t) = concept_text(Some(t)) {
                    facts.push(format!("Type: {t}"));
                }
            }
        }
        if let Some(address) = r.list(&["address"]).first() {
            facts.push(format!("Address: {}", address_text(address)));
        }
        for telecom in r.list(&["telecom"]) {
            if let (Some(system), Some(value)) =
                (str_at(telecom, &["system"]), str_at(telecom, &["value"]))
            {
                facts.push(format!("{}: {value}", upper_first(system)));
            }
        }
        for fact in facts {
            body.text(format!("- {}\n", escape_md(&fact)));
        }
        if r.resource_type() == "Practitioner" {
            let roles: Vec<&Resource> = self
                .record
                .shared
                .iter()
                .filter(|role| {
                    role.resource_type() == "PractitionerRole"
                        && reference_of(role.at(&["practitioner"]), "Practitioner")
                            .and_then(|x| self.record.shared(&x))
                            .is_some_and(|p| p.key() == key)
                })
                .collect();
            for role in roles {
                let specialty = concept_text(role.at(&["specialty"]))
                    .or_else(|| concept_text(role.at(&["code"])));
                body.text(format!(
                    "- {}",
                    escape_md(specialty.as_deref().unwrap_or("Works"))
                ));
                let org = reference_of(role.at(&["organization"]), "Organization");
                if let Some(org) = self.care_key(org.as_deref()) {
                    body.text(" at ");
                    let label = self.care_label(&org);
                    body.link(org, label);
                }
                body.text("\n");
            }
        }
        Some(Doc {
            key: key.to_string(),
            section: Section::CareTeam,
            stem: link_slug(&title),
            title,
            fields: {
                let mut f = Mapping::new();
                f.insert("fhir".into(), Value::Mapping(fhir_id(r)));
                f
            },
            clinician: None,
            body,
        })
    }
}

/// `fhir: {resourceType, id}`.
fn fhir_id(r: &Resource) -> Mapping {
    let mut m = Mapping::new();
    m.insert(
        "resourceType".into(),
        Value::String(r.resource_type().into()),
    );
    m.insert("id".into(), Value::String(r.id().into()));
    m
}

fn patient(r: &Resource) -> Patient {
    Patient {
        id: r.id().to_string(),
        name: crate::fhir::patient_name(r),
        birth_date: r.str(&["birthDate"]).map(str::to_string),
        gender: r.str(&["gender"]).map(str::to_string),
        deceased: r.str(&["deceasedDateTime"]).and_then(date_of),
    }
}

/// A care-team member's title: a practitioner's name, an organization's.
pub fn care_title(r: &Resource) -> String {
    match r.resource_type() {
        "Practitioner" => person_name(r).unwrap_or_else(|| format!("Practitioner {}", r.id())),
        _ => r
            .str(&["name"])
            .map(str::to_string)
            .unwrap_or_else(|| format!("{} {}", r.resource_type(), r.id())),
    }
}

/// The categories an Observation or report carries, as codes.
fn categories(r: &Resource) -> Vec<String> {
    r.list(&["category"])
        .iter()
        .flat_map(|c| c.get("coding").and_then(Value::as_sequence).unwrap_or(&[]))
        .filter_map(|c| str_at(c, &["code"]).map(str::to_string))
        .collect()
}

/// A laboratory Observation.
pub fn is_lab(r: &Resource) -> bool {
    categories(r).iter().any(|c| c == "laboratory")
}

/// A laboratory DiagnosticReport: category `LAB` (HL7 v2-0074) or `laboratory`.
pub fn is_lab_report(r: &Resource) -> bool {
    categories(r)
        .iter()
        .any(|c| c == "LAB" || c == "laboratory")
}

/// A scalar coding value as a string (`code` is a string in FHIR, but be
/// lenient about a number).
fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Int(i) => Some(i.to_string()),
        Value::Float(f) => Some(f.to_string()),
        _ => None,
    }
}

/// A CodeableConcept's text: `text`, else its first coding's `display`.
pub fn concept_text(concept: Option<&Value>) -> Option<String> {
    let concept = concept?;
    let concept = match concept.as_sequence() {
        Some(items) => items.first()?,
        None => concept,
    };
    if let Some(t) = concept.get("text").and_then(Value::as_str) {
        return Some(t.to_string());
    }
    concept
        .get("coding")
        .and_then(Value::as_sequence)?
        .iter()
        .find_map(|c| str_at(c, &["display"]).or_else(|| str_at(c, &["code"])))
        .map(str::to_string)
}

/// A concept's codes as a body line: `SNOMED CT 10509002 · ICD-10 J20.9`.
fn code_line(concept: Option<&Value>) -> String {
    let Some(concept) = concept else {
        return String::new();
    };
    let concept = match concept.as_sequence() {
        Some(items) => match items.first() {
            Some(c) => c,
            None => return String::new(),
        },
        None => concept,
    };
    let parts: Vec<String> = concept
        .get("coding")
        .and_then(Value::as_sequence)
        .unwrap_or(&[])
        .iter()
        .filter_map(|c| {
            let code = c.get("code").and_then(scalar)?;
            let system = str_at(c, &["system"]).unwrap_or_default();
            Some(format!("{} {code}", system_name(system)))
        })
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!("{}.\n\n", escape_md(&parts.join(" · ")))
    }
}

/// A code system's name for a reader.
fn system_name(uri: &str) -> &str {
    match uri.trim_end_matches('/') {
        "http://snomed.info/sct" => "SNOMED CT",
        "http://loinc.org" => "LOINC",
        "http://www.nlm.nih.gov/research/umls/rxnorm" => "RxNorm",
        "http://hl7.org/fhir/sid/cvx" => "CVX",
        "http://hl7.org/fhir/sid/icd-10-cm" => "ICD-10-CM",
        "http://www.ama-assn.org/go/cpt" => "CPT",
        "" => "code",
        other => other,
    }
}

/// An Observation's value, read for a table: a quantity with its unit, a
/// concept's text, a string, or its components joined.
pub fn observation_value(obs: &Value) -> String {
    if let Some(q) = obs.get("valueQuantity") {
        return quantity(q);
    }
    if let Some(c) = obs.get("valueCodeableConcept") {
        return concept_text(Some(c))
            .map(|t| clean_title(&t))
            .unwrap_or_default();
    }
    for key in ["valueString", "valueDateTime", "valueTime"] {
        if let Some(s) = obs.get(key).and_then(Value::as_str) {
            return s.to_string();
        }
    }
    if let Some(b) = obs.get("valueBoolean").and_then(Value::as_bool) {
        return if b { "yes".into() } else { "no".into() };
    }
    if let Some(i) = obs.get("valueInteger").and_then(scalar) {
        return i;
    }
    let components = obs
        .get("component")
        .and_then(Value::as_sequence)
        .unwrap_or(&[]);
    if let Some(bp) = blood_pressure(components) {
        return bp;
    }
    if !components.is_empty() {
        return components
            .iter()
            .map(|c| {
                let name = concept_text(c.get("code")).unwrap_or_default();
                format!("{name}: {}", observation_value(c))
            })
            .collect::<Vec<_>>()
            .join("; ");
    }
    obs.get("dataAbsentReason")
        .and_then(|d| concept_text(Some(d)))
        .map(|t| format!("absent ({t})"))
        .unwrap_or_default()
}

/// Systolic and diastolic components as `120/80 mm[Hg]`.
fn blood_pressure(components: &[Value]) -> Option<String> {
    let find = |word: &str| {
        components
            .iter()
            .find(|c| concept_text(c.get("code")).is_some_and(|t| t.to_lowercase().contains(word)))
    };
    let (sys, dia) = (find("systolic")?, find("diastolic")?);
    let value = |c: &Value| match c.get("valueQuantity").and_then(|q| q.get("value")) {
        Some(Value::Float(f)) => Some(format_number(*f)),
        Some(v) => scalar(v),
        None => None,
    };
    let unit = str_at(sys, &["valueQuantity", "unit"]).unwrap_or_default();
    Some(
        format!("{}/{} {unit}", value(sys)?, value(dia)?)
            .trim_end()
            .to_string(),
    )
}

fn quantity(q: &Value) -> String {
    let value = match q.get("value") {
        Some(Value::Float(f)) => format_number(*f),
        Some(v) => scalar(v).unwrap_or_default(),
        None => String::new(),
    };
    let comparator = str_at(q, &["comparator"]).unwrap_or_default();
    let unit = str_at(q, &["unit"])
        .or_else(|| str_at(q, &["code"]))
        // A UCUM annotation alone (`{score}`, `{#}`) is a note, not a unit.
        .filter(|u| !(u.starts_with('{') && u.ends_with('}')));
    match unit {
        Some(u) if !u.is_empty() => format!("{comparator}{value} {u}"),
        _ => format!("{comparator}{value}"),
    }
}

/// A float as written, without a float's noise: at most four decimals,
/// trailing zeros dropped.
fn format_number(f: f64) -> String {
    let s = format!("{f:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" {
        "0".into()
    } else {
        s.to_string()
    }
}

/// The results table of a lab document.
fn lab_table(body: &mut Body, results: &[&Resource]) {
    if results.is_empty() {
        return;
    }
    let ranges = results.iter().any(|r| r.at(&["referenceRange"]).is_some());
    let flags = results.iter().any(|r| r.at(&["interpretation"]).is_some());
    body.text("| Test | Result |");
    if ranges {
        body.text(" Reference range |");
    }
    if flags {
        body.text(" Flag |");
    }
    body.text(" LOINC |\n| --- | --- |");
    if ranges {
        body.text(" --- |");
    }
    if flags {
        body.text(" --- |");
    }
    body.text(" --- |\n");
    for r in results {
        let name = concept_text(r.at(&["code"])).unwrap_or_default();
        let loinc = r
            .list(&["code", "coding"])
            .iter()
            .find(|c| str_at(c, &["system"]) == Some("http://loinc.org"))
            .and_then(|c| str_at(c, &["code"]))
            .unwrap_or_default();
        body.text(format!(
            "| {} | {} |",
            cell(&name),
            cell(&observation_value(&r.value))
        ));
        if ranges {
            let range = r
                .at(&["referenceRange"])
                .map(|rr| {
                    let low = rr.get("low").map(quantity);
                    let high = rr.get("high").map(quantity);
                    match (low, high) {
                        (Some(l), Some(h)) => format!("{l} – {h}"),
                        (Some(l), None) => format!("≥ {l}"),
                        (None, Some(h)) => format!("≤ {h}"),
                        (None, None) => str_at(rr, &["text"]).unwrap_or_default().to_string(),
                    }
                })
                .unwrap_or_default();
            body.text(format!(" {} |", cell(&range)));
        }
        if flags {
            let flag = concept_text(r.at(&["interpretation"])).unwrap_or_default();
            body.text(format!(" {} |", cell(&flag)));
        }
        body.text(format!(" {} |\n", cell(loinc)));
    }
}

/// A dosage instruction as text.
fn dosage(r: &Resource) -> String {
    let Some(d) = r.list(&["dosageInstruction"]).first() else {
        return String::new();
    };
    if let Some(t) = str_at(d, &["text"]) {
        return clean_title(t);
    }
    let mut parts = Vec::new();
    if let Some(q) = at(d, &["doseAndRate", "doseQuantity"]) {
        parts.push(quantity(q));
    }
    let repeat = at(d, &["timing", "repeat"]);
    if let Some(repeat) = repeat {
        let frequency = repeat.get("frequency").and_then(scalar);
        let period = repeat.get("period").map(|p| match p {
            Value::Float(f) => format_number(*f),
            v => scalar(v).unwrap_or_default(),
        });
        let unit = str_at(repeat, &["periodUnit"]);
        if let (Some(f), Some(p), Some(u)) = (frequency, period, unit) {
            parts.push(format!("{f} every {p} {u}"));
        }
    }
    if at(d, &["asNeededBoolean"]).and_then(Value::as_bool) == Some(true) {
        parts.push("as needed".into());
    }
    parts.join(", ")
}

fn address_text(address: &Value) -> String {
    let mut parts: Vec<String> = address
        .get("line")
        .and_then(Value::as_sequence)
        .unwrap_or(&[])
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    for key in ["city", "state", "postalCode"] {
        if let Some(v) = str_at(address, &[key]) {
            parts.push(v.to_string());
        }
    }
    parts.join(", ")
}

/// HL7 v3 ActCode encounter classes, as a reader says them.
fn encounter_class(code: &str) -> &str {
    match code {
        "AMB" => "Ambulatory",
        "EMER" => "Emergency",
        "IMP" | "ACUTE" | "NONAC" => "Inpatient",
        "OBSENC" => "Observation",
        "PRENC" => "Pre-admission",
        "SS" => "Short stay",
        "VR" => "Virtual",
        "HH" => "Home health",
        "FLD" => "Field",
        other => other,
    }
}

/// A display without SNOMED CT's semantic tag: `Acute bronchitis (disorder)`
/// reads `Acute bronchitis`. Only a trailing, lower-case parenthetical is a
/// tag; `levonorgestrel … [Mirena]` and `HIV (Human immunodeficiency virus)`
/// are left as they are.
pub fn clean_title(display: &str) -> String {
    let trimmed = display.trim();
    if let Some(open) = trimmed.rfind(" (")
        && trimmed.ends_with(')')
    {
        let tag = &trimmed[open + 2..trimmed.len() - 1];
        if !tag.is_empty()
            && tag
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == ' ' || c == '/' || c == '-')
        {
            return trimmed[..open].trim_end().to_string();
        }
    }
    trimmed.to_string()
}

/// The date of a FHIR date or dateTime, as an EDTF `type: date` value takes
/// it: `2026-02-04T01:52:31-07:00` → `2026-02-04`, the date where it
/// happened; a `2026-02` or `2026` stays as precise as it was.
pub fn date_of(s: &str) -> Option<String> {
    date_of_str(s)
}

fn date_of_str(s: &str) -> Option<String> {
    let date = s.split('T').next()?;
    let ok = match date.len() {
        4 | 7 | 10 => date.chars().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                c == '-'
            } else {
                c.is_ascii_digit()
            }
        }),
        _ => false,
    };
    ok.then(|| date.to_string())
}

/// A time span for a reader: `2026-02-04 01:52–02:07`.
fn when(start: &str, end: Option<&str>) -> String {
    let fmt = |s: &str| -> (String, Option<String>) {
        let mut parts = s.splitn(2, 'T');
        let date = parts.next().unwrap_or_default().to_string();
        let time = parts.next().map(|t| t.chars().take(5).collect::<String>());
        (date, time)
    };
    let (d1, t1) = fmt(start);
    let first = match &t1 {
        Some(t) => format!("{d1} {t}"),
        None => d1.clone(),
    };
    let Some(end) = end else { return first };
    let (d2, t2) = fmt(end);
    match t2 {
        Some(t) if d2 == d1 => {
            if t1.as_deref() == Some(t.as_str()) {
                first
            } else {
                format!("{first}–{t}")
            }
        }
        Some(t) => format!("{first} to {d2} {t}"),
        None if d2 == d1 => first,
        None => format!("{first} to {d2}"),
    }
}

/// A file stem: the date, then the title's slug.
fn stem(date: Option<&str>, title: &str) -> String {
    match date {
        Some(d) => format!("{d}-{}", link_slug(title)),
        None => link_slug(title),
    }
}

/// [`prov::link::slug`], cut to a length a file name bears.
fn link_slug(title: &str) -> String {
    let slug = prov::link::slug(title);
    let slug = if slug.is_empty() {
        "record".to_string()
    } else {
        slug
    };
    if slug.chars().count() <= 60 {
        return slug;
    }
    let cut: String = slug.chars().take(60).collect();
    match cut.rfind('-') {
        Some(i) if i > 20 => cut[..i].to_string(),
        _ => cut,
    }
}

fn upper_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) if !s.chars().nth(1).is_some_and(char::is_uppercase) => {
            f.to_lowercase().collect::<String>() + c.as_str()
        }
        _ => s.to_string(),
    }
}

/// Markdown-escape text from a record: the characters that would start a
/// link, emphasis, or HTML.
pub fn escape_md(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '[' | ']' | '*' | '_' | '<' | '>' | '`') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A table cell: escaped, with no pipe or line break.
fn cell(s: &str) -> String {
    escape_md(s).replace('|', "\\|").replace(['\n', '\r'], " ")
}

/// Whether a planned or written document belongs in the emergency summary,
/// read from its fields alone so that the same answer comes from a
/// [`Doc`]'s fields and from a document already on disk:
///
/// - an **allergy** whose FHIR clinical status is active (or unstated);
/// - a **medication** that is `active` or `on-hold`;
/// - a **problem** that is `active` and a *disorder* — where SNOMED CT names
///   it, its semantic tag is `(disorder)`. Synthea records employment,
///   education and stress as active `(finding)`s, which a stranger treating
///   the patient does not need.
pub fn is_emergency(meta: &Value) -> bool {
    let kind = meta.get("kind").and_then(Value::as_str);
    let status = meta.get("status").and_then(Value::as_str);
    match kind {
        Some("allergy") => matches!(
            meta.get("fhir")
                .and_then(|f| f.get("clinicalStatus"))
                .and_then(Value::as_str),
            None | Some("active")
        ),
        Some("medication") => matches!(status, Some("active" | "on-hold")),
        Some("problem") if status == Some("active") => {
            let codes = meta
                .get("codes")
                .and_then(Value::as_sequence)
                .unwrap_or(&[]);
            let snomed: Vec<&Value> = codes
                .iter()
                .filter(|c| str_at(c, &["system"]) == Some("snomed-ct"))
                .collect();
            snomed.is_empty()
                || snomed.iter().any(|c| {
                    str_at(c, &["display"]).is_some_and(|d| d.trim_end().ends_with("(disorder)"))
                })
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_lose_the_semantic_tag_only() {
        assert_eq!(
            clean_title("Acute bronchitis (disorder)"),
            "Acute bronchitis"
        );
        assert_eq!(
            clean_title("Body mass index 30+ - obesity (finding)"),
            "Body mass index 30+ - obesity"
        );
        assert_eq!(clean_title("Chemotherapy (regime/therapy)"), "Chemotherapy");
        assert_eq!(
            clean_title("HIV (Human immunodeficiency virus)"),
            "HIV (Human immunodeficiency virus)"
        );
        assert_eq!(
            clean_title("levonorgestrel 0.000833 MG/HR Intrauterine System [Mirena]"),
            "levonorgestrel 0.000833 MG/HR Intrauterine System [Mirena]"
        );
    }

    #[test]
    fn dates_keep_the_local_day_and_their_precision() {
        assert_eq!(
            date_of("2026-02-04T01:52:31-07:00").as_deref(),
            Some("2026-02-04")
        );
        assert_eq!(date_of("2026-02").as_deref(), Some("2026-02"));
        assert_eq!(date_of("1999").as_deref(), Some("1999"));
        assert_eq!(date_of("yesterday"), None);
    }

    #[test]
    fn spans_read_naturally() {
        assert_eq!(
            when(
                "2026-02-04T01:52:31-07:00",
                Some("2026-02-04T02:07:31-07:00")
            ),
            "2026-02-04 01:52–02:07"
        );
        assert_eq!(
            when("2026-02-04T23:00:00Z", Some("2026-02-05T01:00:00Z")),
            "2026-02-04 23:00 to 2026-02-05 01:00"
        );
        assert_eq!(when("2026-02-04", None), "2026-02-04");
    }

    #[test]
    fn values_read_with_their_units() {
        let obs = |json: &str| prov::meta::parse_value(json, prov::MetaFormat::Json).unwrap();
        assert_eq!(
            observation_value(&obs(
                r#"{"valueQuantity":{"value":7.0057,"unit":"10*3/uL"}}"#
            )),
            "7.0057 10*3/uL"
        );
        assert_eq!(
            observation_value(&obs(
                r#"{"valueQuantity":{"value":51.90000001,"unit":"cm"}}"#
            )),
            "51.9 cm"
        );
        assert_eq!(
            observation_value(&obs(
                r#"{"component":[{"code":{"text":"Systolic"},"valueQuantity":{"value":120,"unit":"mm[Hg]"}},{"code":{"text":"Diastolic"},"valueQuantity":{"value":80,"unit":"mm[Hg]"}}]}"#
            )),
            "120/80 mm[Hg]"
        );
        assert_eq!(
            observation_value(&obs(
                r#"{"valueCodeableConcept":{"text":"Never smoked tobacco (finding)"}}"#
            )),
            "Never smoked tobacco"
        );
    }

    #[test]
    fn markdown_in_a_record_stays_text() {
        assert_eq!(
            escape_md("Hemoglobin [Mass/volume]"),
            "Hemoglobin \\[Mass/volume\\]"
        );
        assert_eq!(cell("a | b"), "a \\| b");
    }

    #[test]
    fn the_emergency_list_is_disorders_medicines_and_allergies() {
        let meta = |yaml: &str| prov::meta::parse_value(yaml, prov::MetaFormat::Yaml).unwrap();
        assert!(is_emergency(&meta(
            "kind: problem\nstatus: active\ncodes:\n- {system: snomed-ct, code: '1', display: Hypertension (disorder)}\n"
        )));
        assert!(!is_emergency(&meta(
            "kind: problem\nstatus: active\ncodes:\n- {system: snomed-ct, code: '1', display: Full-time employment (finding)}\n"
        )));
        assert!(!is_emergency(&meta(
            "kind: problem\nstatus: resolved\ncodes:\n- {system: snomed-ct, code: '1', display: Hypertension (disorder)}\n"
        )));
        assert!(is_emergency(&meta(
            "kind: problem\nstatus: active\ncodes:\n- {system: icd-10-cm, code: I10}\n"
        )));
        assert!(is_emergency(&meta("kind: medication\nstatus: on-hold\n")));
        assert!(!is_emergency(&meta("kind: medication\nstatus: stopped\n")));
        assert!(is_emergency(&meta(
            "kind: allergy\nfhir: {clinicalStatus: active}\n"
        )));
        assert!(!is_emergency(&meta(
            "kind: allergy\nfhir: {clinicalStatus: resolved}\n"
        )));
        assert!(!is_emergency(&meta("kind: encounter\n")));
    }
}
