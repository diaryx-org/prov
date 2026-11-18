//! Reading a FHIR export: a Bulk Data directory of ndjson files, one ndjson
//! file, or a Bundle.
//!
//! A bulk export holds every patient a server exported, and a workspace holds
//! one patient's record, so the reader's job is to cut one patient's share
//! out of it: the resources that name the patient, and the practitioners,
//! organizations, locations and medications those resources refer to. That
//! share is a [`Record`]. Its [`Record::raw`] files are the export's own lines,
//! byte for byte and in the export's order, which is what the import keeps as
//! evidence; [`Record::resources`] is the same share parsed.
//!
//! JSON is read with fig, through [`prov::meta::parse_value`], into prov's
//! own [`Value`]: the parser prov already reads `---json` frontmatter with.
//!
//! References are resolved in the three spellings an export uses: `Type/id`,
//! `urn:uuid:<id>` (a Bundle's), and the conditional `Type?identifier=sys|val`
//! that Synthea writes for practitioners and organizations.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use prov::{MetaFormat, Value};

use crate::Error;

/// Resource types a bulk export shares between patients. They are parsed
/// whole, and a patient's share carries the ones its resources refer to.
pub const SHARED_TYPES: &[&str] = &[
    "Practitioner",
    "PractitionerRole",
    "Organization",
    "Location",
    "Medication",
];

/// The resource types the importer maps to documents or reads while mapping.
/// Every other type a patient's share holds (claims, care plans, devices…) is
/// kept in the raw export and not parsed.
pub const MAPPED_TYPES: &[&str] = &[
    "Encounter",
    "Condition",
    "MedicationRequest",
    "MedicationStatement",
    "AllergyIntolerance",
    "Immunization",
    "Procedure",
    "Observation",
    "DiagnosticReport",
];

/// One parsed resource.
#[derive(Debug, Clone)]
pub struct Resource {
    /// The resource, as prov's value tree.
    pub value: Value,
}

impl Resource {
    /// Parse one resource from its JSON text.
    pub fn parse(json: &str) -> Result<Self, Error> {
        let value = prov::meta::parse_value(json, MetaFormat::Json)?;
        if value.get("resourceType").and_then(Value::as_str).is_none() {
            return Err("not a FHIR resource: no `resourceType`".into());
        }
        Ok(Self { value })
    }

    /// `resourceType`.
    pub fn resource_type(&self) -> &str {
        self.str(&["resourceType"]).unwrap_or_default()
    }

    /// `id`, or the empty string for a resource without one.
    pub fn id(&self) -> &str {
        self.str(&["id"]).unwrap_or_default()
    }

    /// `Type/id`: how the importer names a resource everywhere.
    pub fn key(&self) -> String {
        format!("{}/{}", self.resource_type(), self.id())
    }

    /// The value at `path`, each step a mapping key; a sequence on the way
    /// is entered at its first item.
    pub fn at(&self, path: &[&str]) -> Option<&Value> {
        at(&self.value, path)
    }

    /// The string at `path`; see [`at`](Self::at).
    pub fn str(&self, path: &[&str]) -> Option<&str> {
        self.at(path).and_then(Value::as_str)
    }

    /// The sequence at `path`, or an empty one.
    pub fn list(&self, path: &[&str]) -> &[Value] {
        self.at(path).and_then(Value::as_sequence).unwrap_or(&[])
    }
}

/// The value at `path` below `value`; a sequence on the way is entered at its
/// first item.
pub fn at<'v>(value: &'v Value, path: &[&str]) -> Option<&'v Value> {
    let mut here = value;
    for step in path {
        if let Some(items) = here.as_sequence() {
            here = items.first()?;
        }
        here = here.get(step)?;
    }
    Some(here)
}

/// The string at `path` below `value`.
pub fn str_at<'v>(value: &'v Value, path: &[&str]) -> Option<&'v str> {
    at(value, path).and_then(Value::as_str)
}

/// A Reference as a string the export's resources answer to: its
/// `reference`, or — for a logical reference, which names its target by
/// identifier alone, as Synthea's PractitionerRole does — the conditional
/// `Type?identifier=system|value` it amounts to.
pub fn reference_of(reference: Option<&Value>, target_type: &str) -> Option<String> {
    let reference = reference?;
    if let Some(r) = reference.get("reference").and_then(Value::as_str) {
        return Some(r.to_string());
    }
    let identifier = reference.get("identifier")?;
    Some(format!(
        "{target_type}?identifier={}|{}",
        str_at(identifier, &["system"])?,
        str_at(identifier, &["value"])?
    ))
}

/// The resource type an export line names, read from its text without
/// parsing it: the value after the first `"resourceType"`.
fn sniff_type(line: &str) -> Option<&str> {
    let after = &line[line.find("\"resourceType\"")? + "\"resourceType\"".len()..];
    let after = after.trim_start().strip_prefix(':')?.trim_start();
    let after = after.strip_prefix('"')?;
    Some(&after[..after.find('"')?])
}

/// Every `"reference": "…"` string in a resource's text, without parsing it.
fn sniff_references(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("\"reference\"") {
        rest = &rest[at + "\"reference\"".len()..];
        let Some(value) = rest.trim_start().strip_prefix(':') else {
            continue;
        };
        let Some(value) = value.trim_start().strip_prefix('"') else {
            continue;
        };
        if let Some(end) = value.find('"') {
            out.push(&value[..end]);
        }
    }
    out
}

/// One line of an ndjson file, kept as the export wrote it.
#[derive(Debug, Clone)]
struct Line {
    resource_type: String,
    text: String,
}

/// What an export was read from.
#[derive(Debug, Clone)]
enum Origin {
    /// ndjson files: every line is kept as written.
    Ndjson,
    /// A Bundle: kept whole, as the file it was.
    Bundle { path: PathBuf, bytes: Vec<u8> },
}

/// A FHIR export, read: its patients, the resources they share, and each
/// other line, waiting to be cut into one patient's [`Record`].
#[derive(Debug)]
pub struct Export {
    origin: Origin,
    patients: Vec<(Resource, String)>,
    /// Shared resources, parsed, with the line each was read from.
    shared: Vec<(Resource, Line)>,
    /// Every reference spelling that names a shared resource, to its index.
    shared_by_ref: HashMap<String, usize>,
    /// Every other line, in export order.
    lines: Vec<Line>,
    /// Resources already parsed (a Bundle's), by line index.
    parsed: HashMap<usize, Resource>,
}

impl Export {
    /// Read an export: a directory (every `*.ndjson` in it, by name), one
    /// ndjson file, or a JSON Bundle.
    pub fn read(source: &Path) -> Result<Self, Error> {
        if source.is_dir() {
            let mut files: Vec<PathBuf> = std::fs::read_dir(source)?
                .filter_map(|entry| entry.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|e| e == "ndjson"))
                .collect();
            files.sort();
            if files.is_empty() {
                return Err(format!("{}: no .ndjson files in it", source.display()).into());
            }
            Self::from_ndjson(&files)
        } else if source.extension().is_some_and(|e| e == "ndjson") {
            Self::from_ndjson(&[source.to_path_buf()])
        } else {
            Self::from_bundle(source)
        }
    }

    fn from_ndjson(files: &[PathBuf]) -> Result<Self, Error> {
        let mut export = Self::empty(Origin::Ndjson);
        for file in files {
            let text =
                std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
            for (n, line) in text.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                let Some(resource_type) = sniff_type(line) else {
                    return Err(format!(
                        "{}:{}: not a FHIR resource (no resourceType)",
                        file.display(),
                        n + 1
                    )
                    .into());
                };
                export.push(Line {
                    resource_type: resource_type.to_string(),
                    text: line.to_string(),
                })?;
            }
        }
        Ok(export)
    }

    fn from_bundle(path: &Path) -> Result<Self, Error> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let text = String::from_utf8(bytes.clone())
            .map_err(|_| format!("{}: not UTF-8", path.display()))?;
        let bundle = Resource::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if bundle.resource_type() != "Bundle" {
            return Err(format!(
                "{}: a {}, not a Bundle or a directory of ndjson",
                path.display(),
                bundle.resource_type()
            )
            .into());
        }
        let mut export = Self::empty(Origin::Bundle {
            path: path.to_path_buf(),
            bytes,
        });
        for entry in bundle.list(&["entry"]) {
            let Some(resource) = entry.get("resource") else {
                continue;
            };
            let resource = Resource {
                value: resource.clone(),
            };
            let line = Line {
                resource_type: resource.resource_type().to_string(),
                text: String::new(),
            };
            // A Bundle's references are `urn:uuid:<id>`, which the entry's
            // `fullUrl` spells; `Type/id` reaches the same resource.
            let full_url = entry.get("fullUrl").and_then(Value::as_str);
            export.push_parsed(line, resource, full_url);
        }
        Ok(export)
    }

    fn empty(origin: Origin) -> Self {
        Self {
            origin,
            patients: Vec::new(),
            shared: Vec::new(),
            shared_by_ref: HashMap::new(),
            lines: Vec::new(),
            parsed: HashMap::new(),
        }
    }

    fn push(&mut self, line: Line) -> Result<(), Error> {
        if line.resource_type == "Patient" || SHARED_TYPES.contains(&line.resource_type.as_str()) {
            let resource = Resource::parse(&line.text)?;
            self.push_parsed(line, resource, None);
        } else {
            self.lines.push(line);
        }
        Ok(())
    }

    fn push_parsed(&mut self, line: Line, resource: Resource, full_url: Option<&str>) {
        if line.resource_type == "Patient" {
            self.patients.push((resource, line.text));
        } else if SHARED_TYPES.contains(&line.resource_type.as_str()) {
            let index = self.shared.len();
            for spelling in reference_spellings(&resource) {
                self.shared_by_ref.entry(spelling).or_insert(index);
            }
            if let Some(full_url) = full_url {
                self.shared_by_ref
                    .entry(full_url.to_string())
                    .or_insert(index);
            }
            self.shared.push((resource, line));
        } else {
            self.parsed.insert(self.lines.len(), resource);
            self.lines.push(line);
        }
    }

    /// The patients the export holds, in export order.
    pub fn patients(&self) -> impl Iterator<Item = &Resource> {
        self.patients.iter().map(|(p, _)| p)
    }

    /// The shared resource a reference names, in any of its spellings.
    pub fn shared(&self, reference: &str) -> Option<&Resource> {
        self.shared_by_ref
            .get(reference)
            .map(|&i| &self.shared[i].0)
    }

    /// One patient's share of the export. `None` when no patient has `id`.
    pub fn record(&self, id: &str) -> Result<Option<Record>, Error> {
        let Some((patient, patient_line)) = self.patients.iter().find(|(p, _)| p.id() == id) else {
            return Ok(None);
        };
        let names = [format!("Patient/{id}"), format!("urn:uuid:{id}")];
        let mut resources = Vec::new();
        let mut mine: Vec<&Line> = Vec::new();
        let mut referenced: BTreeSet<usize> = BTreeSet::new();
        let mut kept: BTreeMap<String, usize> = BTreeMap::new();
        for (index, line) in self.lines.iter().enumerate() {
            let belongs = match self.parsed.get(&index) {
                Some(resource) => names_patient(resource, &names),
                None => names.iter().any(|n| line.text.contains(n.as_str())),
            };
            if !belongs {
                continue;
            }
            if let Some(resource) = self.parsed.get(&index) {
                if MAPPED_TYPES.contains(&line.resource_type.as_str()) {
                    resources.push(resource.clone());
                }
            } else {
                for reference in sniff_references(&line.text) {
                    if let Some(&i) = self.shared_by_ref.get(reference) {
                        referenced.insert(i);
                    }
                }
                if MAPPED_TYPES.contains(&line.resource_type.as_str()) {
                    resources.push(Resource::parse(&line.text)?);
                }
            }
            if !MAPPED_TYPES.contains(&line.resource_type.as_str()) {
                *kept.entry(line.resource_type.clone()).or_default() += 1;
            }
            mine.push(line);
        }
        // A Bundle's resources were parsed, so their references are read off
        // the tree rather than the text.
        for resource in &resources {
            let mut refs = Vec::new();
            collect_references(&resource.value, &mut refs);
            for reference in refs {
                if let Some(&i) = self.shared_by_ref.get(reference.as_str()) {
                    referenced.insert(i);
                }
            }
        }
        // A practitioner's roles say where they work: carry each role of a
        // practitioner the patient saw, and the organizations those name.
        let practitioners: BTreeSet<String> = referenced
            .iter()
            .map(|&i| &self.shared[i].0)
            .filter(|r| r.resource_type() == "Practitioner")
            .map(Resource::key)
            .collect();
        for (i, (role, _)) in self.shared.iter().enumerate() {
            if role.resource_type() != "PractitionerRole" {
                continue;
            }
            let Some(of) = reference_of(role.at(&["practitioner"]), "Practitioner")
                .and_then(|r| self.shared(&r))
            else {
                continue;
            };
            if practitioners.contains(&of.key()) {
                referenced.insert(i);
                for (field, target) in [("organization", "Organization"), ("location", "Location")]
                {
                    if let Some(&j) = reference_of(role.at(&[field]), target)
                        .and_then(|r| self.shared_by_ref.get(&r))
                    {
                        referenced.insert(j);
                    }
                }
            }
        }

        let raw = match &self.origin {
            Origin::Bundle { path, bytes } => {
                if self.patients.len() > 1 {
                    return Err(format!(
                        "{}: a Bundle of {} patients — a workspace keeps one patient's record, \
                         and a Bundle is kept whole; export one Bundle per patient, or Bulk ndjson",
                        path.display(),
                        self.patients.len()
                    )
                    .into());
                }
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "bundle.json".into());
                vec![(name, bytes.clone())]
            }
            Origin::Ndjson => {
                let mut by_type: BTreeMap<String, String> = BTreeMap::new();
                let mut add = |resource_type: &str, text: &str| {
                    let file = by_type.entry(resource_type.to_string()).or_default();
                    file.push_str(text);
                    file.push('\n');
                };
                add("Patient", patient_line);
                for &i in &referenced {
                    let line = &self.shared[i].1;
                    add(&line.resource_type, &line.text);
                }
                for line in &mine {
                    add(&line.resource_type, &line.text);
                }
                by_type
                    .into_iter()
                    .map(|(t, text)| (format!("{t}.ndjson"), text.into_bytes()))
                    .collect()
            }
        };

        Ok(Some(Record {
            patient: patient.clone(),
            resources,
            shared: referenced
                .iter()
                .map(|&i| self.shared[i].0.clone())
                .collect(),
            raw,
            kept,
        }))
    }
}

/// Whether a parsed resource names the patient as its subject.
fn names_patient(resource: &Resource, names: &[String]) -> bool {
    ["subject", "patient", "beneficiary"].iter().any(|field| {
        resource
            .str(&[field, "reference"])
            .is_some_and(|r| names.iter().any(|n| n == r))
    })
}

/// Every `reference` string below `value`.
fn collect_references(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Mapping(map) => {
            for (key, inner) in map {
                if key == "reference"
                    && let Some(s) = inner.as_str()
                {
                    out.push(s.to_string());
                } else {
                    collect_references(inner, out);
                }
            }
        }
        Value::Sequence(items) => items.iter().for_each(|v| collect_references(v, out)),
        _ => {}
    }
}

/// Every spelling a reference to `resource` may take: `Type/id`,
/// `urn:uuid:id`, and `Type?identifier=system|value` for each identifier.
fn reference_spellings(resource: &Resource) -> Vec<String> {
    let t = resource.resource_type();
    let mut out = vec![
        format!("{t}/{}", resource.id()),
        format!("urn:uuid:{}", resource.id()),
    ];
    for identifier in resource.list(&["identifier"]) {
        if let (Some(system), Some(value)) = (
            str_at(identifier, &["system"]),
            str_at(identifier, &["value"]),
        ) {
            out.push(format!("{t}?identifier={system}|{value}"));
        }
    }
    out
}

/// One patient's share of an export.
#[derive(Debug, Clone)]
pub struct Record {
    /// The Patient resource.
    pub patient: Resource,
    /// The resources the importer maps, in export order.
    pub resources: Vec<Resource>,
    /// The shared resources this patient's resources refer to.
    pub shared: Vec<Resource>,
    /// The share as files — `Encounter.ndjson`, … — each line as the export
    /// wrote it; or a Bundle, whole. Sorted by name.
    pub raw: Vec<(String, Vec<u8>)>,
    /// The patient's resources of types the importer does not map — claims,
    /// care plans, clinical notes… — by type: kept in [`raw`](Self::raw)
    /// and in no document.
    pub kept: BTreeMap<String, usize>,
}

impl Record {
    /// The patient's FHIR id.
    pub fn patient_id(&self) -> &str {
        self.patient.id()
    }

    /// The shared resource a reference names: `Type/id`, `urn:uuid:id`, or a
    /// conditional `Type?identifier=sys|val`.
    pub fn shared(&self, reference: &str) -> Option<&Resource> {
        self.shared.iter().find(|r| {
            r.key() == reference
                || reference.strip_prefix("urn:uuid:") == Some(r.id())
                || reference_spellings(r).iter().any(|s| s == reference)
        })
    }

    /// `sha256:<hex>` over the raw files, names and bytes: what makes two
    /// imports of the same export the same import.
    pub fn digest(&self) -> String {
        let mut all = Vec::new();
        for (name, bytes) in &self.raw {
            all.extend_from_slice(name.as_bytes());
            all.push(0);
            all.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            all.extend_from_slice(bytes);
        }
        prov::fixity::digest(&all)
    }
}

/// The patient's display name: `given… family`, from the official name.
pub fn patient_name(patient: &Resource) -> String {
    let names = patient.list(&["name"]);
    let name = names
        .iter()
        .find(|n| str_at(n, &["use"]) == Some("official"))
        .or_else(|| names.first());
    let Some(name) = name else {
        return format!("Patient {}", patient.id());
    };
    if let Some(text) = str_at(name, &["text"]) {
        return text.to_string();
    }
    let mut parts: Vec<&str> = name
        .get("given")
        .and_then(Value::as_sequence)
        .unwrap_or(&[])
        .iter()
        .filter_map(Value::as_str)
        .collect();
    if let Some(family) = str_at(name, &["family"]) {
        parts.push(family);
    }
    if parts.is_empty() {
        format!("Patient {}", patient.id())
    } else {
        parts.join(" ")
    }
}

/// A human name (Practitioner) as `prefix given family`.
pub fn person_name(resource: &Resource) -> Option<String> {
    let name = resource.list(&["name"]).first()?;
    if let Some(text) = str_at(name, &["text"]) {
        return Some(text.to_string());
    }
    let mut parts = Vec::new();
    for key in ["prefix", "given"] {
        for part in name
            .get(key)
            .and_then(Value::as_sequence)
            .unwrap_or(&[])
            .iter()
            .filter_map(Value::as_str)
        {
            parts.push(part.to_string());
        }
    }
    if let Some(family) = str_at(name, &["family"]) {
        parts.push(family.to_string());
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}
