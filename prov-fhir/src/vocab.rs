//! The medical preset's vocabularies, as the importer reads them.
//!
//! Three closed fields decide what the importer may write: `codes[].system`
//! (which code systems a coding may be said to come from), and `status` under
//! Problems and under Medications. The terms are read from the workspace's
//! own vocabulary files, wherever its config points, so a workspace that adds
//! a code system to its vocabulary — with the system's `uri:` — gets its
//! codings imported without a new build of the importer.
//!
//! A term's `uri:` (code systems) and `fhir:` (statuses) say which FHIR value
//! it stands for. FHIR statuses the vocabulary does not name are read through
//! [`PROBLEM_FALLBACK`] and [`MEDICATION_FALLBACK`]: FHIR has more of them than
//! a patient's list needs (`recurrence` and `relapse` are both, to the
//! patient, a problem that is active).

use std::collections::BTreeMap;
use std::path::Path;

use prov::{MetaFormat, StdFs, Value, Workspace, WorkspaceConfig};

use crate::Error;

/// FHIR `Condition.clinicalStatus` codes the problem-status vocabulary does
/// not carry as a `fhir:` hint, and the term each one is.
pub const PROBLEM_FALLBACK: &[(&str, &str)] = &[
    ("recurrence", "active"),
    ("relapse", "active"),
    ("remission", "inactive"),
];

/// FHIR `MedicationRequest.status` / `MedicationStatement.status` codes the
/// medication-status vocabulary does not carry as a `fhir:` hint, and the term
/// each one is. `draft`, `intended` and `unknown` are deliberately absent: they
/// say nothing about whether the medicine is being taken, so the importer
/// writes no status rather than a guess.
pub const MEDICATION_FALLBACK: &[(&str, &str)] = &[
    ("completed", "stopped"),
    ("cancelled", "stopped"),
    ("not-taken", "stopped"),
];

/// The terms the importer maps FHIR values onto.
#[derive(Debug, Clone, Default)]
pub struct Vocab {
    /// Code-system URI → term (`http://loinc.org` → `loinc`).
    pub systems: BTreeMap<String, String>,
    /// FHIR clinical status → problem-status term.
    pub problem_statuses: BTreeMap<String, String>,
    /// FHIR medication status → medication-status term.
    pub medication_statuses: BTreeMap<String, String>,
}

impl Vocab {
    /// The vocabularies `presets/medical` ships, as built into this binary —
    /// for tests, and for the summary of a workspace not yet made.
    pub fn medical() -> Self {
        let parse = |text: &str| {
            prov::meta::parse_value(text, MetaFormat::Yaml).expect("the shipped vocabulary parses")
        };
        Self::from_documents(
            Some(&parse(crate::preset::CODE_SYSTEMS)),
            Some(&parse(crate::preset::PROBLEM_STATUSES)),
            Some(&parse(crate::preset::MEDICATION_STATUSES)),
        )
    }

    /// Build from the three vocabulary documents, any of which may be absent.
    pub fn from_documents(
        code_systems: Option<&Value>,
        problem_statuses: Option<&Value>,
        medication_statuses: Option<&Value>,
    ) -> Self {
        let mut vocab = Self::default();
        for (term, spec) in terms(code_systems) {
            if let Some(uri) = spec.get("uri").and_then(Value::as_str) {
                vocab
                    .systems
                    .insert(uri.trim_end_matches('/').to_string(), term.clone());
            }
        }
        let statuses = |doc: Option<&Value>, fallback: &[(&str, &str)]| {
            let mut out = BTreeMap::new();
            let known: Vec<String> = terms(doc).iter().map(|(t, _)| t.clone()).collect();
            for (term, spec) in terms(doc) {
                let fhir = spec.get("fhir").and_then(Value::as_str).unwrap_or(&term);
                out.insert(fhir.to_string(), term.clone());
            }
            for (fhir, term) in fallback {
                if known.iter().any(|t| t == term) {
                    out.entry((*fhir).to_string())
                        .or_insert_with(|| (*term).to_string());
                }
            }
            out
        };
        vocab.problem_statuses = statuses(problem_statuses, PROBLEM_FALLBACK);
        vocab.medication_statuses = statuses(medication_statuses, MEDICATION_FALLBACK);
        vocab
    }

    /// Read the vocabularies a workspace's config points at.
    pub async fn of_workspace(
        ws: &Workspace<StdFs, impl prov::IdentityPolicy, impl prov::IndexStore>,
        root_doc: &Path,
        config: &WorkspaceConfig,
    ) -> Result<Self, Error> {
        let read = |pointer: Option<&String>| -> Result<Option<Value>, Error> {
            let Some(pointer) = pointer else {
                return Ok(None);
            };
            let Some(path) = ws.vocabulary_path(root_doc, pointer) else {
                return Ok(None);
            };
            let full = ws.fs_path(&path);
            let text =
                std::fs::read_to_string(&full).map_err(|e| format!("{}: {e}", full.display()))?;
            Ok(Some(prov::Document::parse(&path, &text)?.meta))
        };
        let pointer = |field: &str, under: Option<&str>| {
            config.fields.get(field).and_then(|specs| {
                specs
                    .iter()
                    .find(|s| s.under.as_deref() == under)
                    .and_then(|s| s.vocabulary.as_ref())
            })
        };
        Ok(Self::from_documents(
            read(pointer("codes[].system", None))?.as_ref(),
            read(pointer("status", Some("[[Problems]]")))?.as_ref(),
            read(pointer("status", Some("[[Medications]]")))?.as_ref(),
        ))
    }

    /// The term for a coding's `system` URI.
    pub fn system(&self, uri: &str) -> Option<&str> {
        self.systems
            .get(uri.trim_end_matches('/'))
            .map(String::as_str)
    }

    /// The problem-status term for a FHIR clinical status.
    pub fn problem_status(&self, fhir: &str) -> Option<&str> {
        self.problem_statuses.get(fhir).map(String::as_str)
    }

    /// The medication-status term for a FHIR medication status.
    pub fn medication_status(&self, fhir: &str) -> Option<&str> {
        self.medication_statuses.get(fhir).map(String::as_str)
    }
}

/// A vocabulary document's `terms`, as `(term, spec)` pairs.
fn terms(doc: Option<&Value>) -> Vec<(String, Value)> {
    doc.and_then(|d| d.get("terms"))
        .and_then(Value::as_mapping)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_systems_map_by_uri() {
        let v = Vocab::medical();
        assert_eq!(v.system("http://loinc.org"), Some("loinc"));
        assert_eq!(v.system("http://snomed.info/sct"), Some("snomed-ct"));
        assert_eq!(
            v.system("http://www.nlm.nih.gov/research/umls/rxnorm"),
            Some("rxnorm")
        );
        assert_eq!(v.system("http://hl7.org/fhir/sid/cvx"), Some("cvx"));
        assert_eq!(
            v.system("http://hl7.org/fhir/sid/icd-10-cm"),
            Some("icd-10-cm")
        );
        // A trailing slash is the same system.
        assert_eq!(v.system("http://loinc.org/"), Some("loinc"));
        assert_eq!(v.system("urn:oid:2.16.840.1.113883.6.238"), None);
    }

    #[test]
    fn statuses_map_through_hints_then_fallbacks() {
        let v = Vocab::medical();
        assert_eq!(v.problem_status("active"), Some("active"));
        assert_eq!(v.problem_status("recurrence"), Some("active"));
        assert_eq!(v.problem_status("remission"), Some("inactive"));
        assert_eq!(v.problem_status("resolved"), Some("resolved"));
        assert_eq!(v.medication_status("completed"), Some("stopped"));
        assert_eq!(v.medication_status("on-hold"), Some("on-hold"));
        assert_eq!(v.medication_status("unknown"), None);
        assert_eq!(v.medication_status("draft"), None);
    }

    #[test]
    fn a_fallback_to_a_term_the_vocabulary_lacks_is_dropped() {
        let doc = prov::meta::parse_value("terms:\n  active: { fhir: active }\n", MetaFormat::Yaml)
            .unwrap();
        let v = Vocab::from_documents(None, Some(&doc), None);
        assert_eq!(v.problem_status("recurrence"), Some("active"));
        assert_eq!(v.problem_status("remission"), None);
    }
}
