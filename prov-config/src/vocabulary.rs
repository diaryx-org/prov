//! Controlled vocabularies — the term sets that make `fields` references (tags,
//! audiences, statuses) *resolvable* and therefore consistent.
//!
//! DESIGN §2's principle: prov keeps consistent only what it can resolve. A bare
//! `tags:` string is tier-3 content prov merely carries; the moment a `fields`
//! entry ([`crate::config::FieldSpec`]) points that field at a vocabulary, its
//! values become references prov checks — a closed vocabulary rejects unknown
//! values, an open one flags likely typos (`crate::validate`).
//!
//! A vocabulary lives in a **whole-file config document** (the whole-file store
//! rule, DESIGN §5): a self-describing node — `title`, `part_of` back toward the
//! root — declaring `vocabulary: { field, values }` and a `terms:` mapping. prov
//! reasons about the term *keys*, each term's stable `id`, and whether it is
//! `retired`; everything else in a term entry is tier-3 payload prov carries but
//! never reads (a diaryx audience's gate/theme).

use std::collections::BTreeMap;

use crate::OpenClosed;
use prov_graph::identity::Id;
use prov_graph::meta::Value;

/// A single term in a vocabulary. prov reads only the three fields here; any
/// other keys on the term entry are carried, not interpreted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    /// The term's stable opaque id, if it has been minted — what lets a term's
    /// display label change without breaking references (DESIGN §4).
    pub id: Option<Id>,
    /// A free-form human gloss of the term. Carried, never read (§2).
    pub means: Option<String>,
    /// Whether the term is retired: still *known* (so a reference to it stays
    /// diagnosable and its id is never reissued, the tombstone idea of §10) but no
    /// longer a valid value for new content.
    pub retired: bool,
}

/// A parsed controlled vocabulary — the term set for one field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vocabulary {
    /// The frontmatter field this vocabulary governs (`audience`, `tags`).
    pub field: String,
    /// Whether the value set is open (folksonomy) or closed (must be known).
    pub values: OpenClosed,
    /// The legal terms, keyed by term name.
    pub terms: BTreeMap<String, Term>,
}

impl Vocabulary {
    /// Parse a vocabulary from a store document's top-level metadata mapping. The
    /// store declares a `vocabulary: { field, values }` marker and a `terms:`
    /// mapping (each term either a bare key or a `{ id?, means?, retired? }`
    /// entry). Returns `None` when the document carries no `vocabulary` marker —
    /// i.e. it is not a vocabulary store.
    pub fn from_meta(meta: &Value) -> Option<Self> {
        let marker = meta.get("vocabulary")?;
        let field = marker.get("field").and_then(Value::as_str)?.to_string();
        let values = marker
            .get("values")
            .and_then(Value::as_str)
            .and_then(OpenClosed::from_config_str)
            .unwrap_or_default();
        let mut terms = BTreeMap::new();
        if let Some(map) = meta.get("terms").and_then(Value::as_mapping) {
            for (name, spec) in map {
                let term = match spec.as_mapping() {
                    Some(entry) => Term {
                        id: entry
                            .get("id")
                            .and_then(Value::as_str)
                            .map(|s| Id(s.to_string())),
                        means: entry
                            .get("means")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        retired: entry
                            .get("retired")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    },
                    // A bare `term:` (null/scalar value) is a live term with no metadata.
                    None => Term {
                        id: None,
                        means: None,
                        retired: false,
                    },
                };
                terms.insert(name.clone(), term);
            }
        }
        Some(Self {
            field,
            values,
            terms,
        })
    }

    /// Whether `value` is a known, non-retired term — i.e. a valid value.
    pub fn accepts(&self, value: &str) -> bool {
        self.terms.get(value).is_some_and(|t| !t.retired)
    }

    /// Whether `value` names a *retired* term — known but no longer valid.
    pub fn is_retired(&self, value: &str) -> bool {
        self.terms.get(value).is_some_and(|t| t.retired)
    }

    /// The live (non-retired) term names — the candidate set for near-miss
    /// suggestions when an open-vocabulary value does not match.
    pub fn live_term_names(&self) -> Vec<String> {
        self.terms
            .iter()
            .filter(|(_, t)| !t.retired)
            .map(|(name, _)| name.clone())
            .collect()
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::*;
    use prov_graph::document::Document;

    fn vocab(text: &str) -> Vocabulary {
        let doc = Document::parse("vocab/audiences.yaml", text).unwrap();
        Vocabulary::from_meta(&doc.meta).expect("a vocabulary store")
    }

    #[test]
    fn parses_a_closed_vocabulary_with_terms() {
        let v = vocab(
            "title: Audiences\n\
             part_of: /README.md\n\
             vocabulary:\n  field: audience\n  values: closed\n\
             terms:\n  public:\n    means: Anyone\n  friends:\n    id: aud_k9fp\n",
        );
        assert_eq!(v.field, "audience");
        assert_eq!(v.values, OpenClosed::Closed);
        assert!(v.accepts("public"));
        assert!(v.accepts("friends"));
        assert!(!v.accepts("colleagues"));
        assert_eq!(v.terms["friends"].id, Some(Id("aud_k9fp".into())));
    }

    #[test]
    fn a_retired_term_is_known_but_not_accepted() {
        let v = vocab(
            "vocabulary:\n  field: status\n  values: closed\n\
             terms:\n  active: {}\n  archived_2024:\n    retired: true\n",
        );
        assert!(v.accepts("active"));
        assert!(!v.accepts("archived_2024"), "retired is not a valid value");
        assert!(v.is_retired("archived_2024"), "but it is still known");
        assert_eq!(v.live_term_names(), vec!["active".to_string()]);
    }

    #[test]
    fn a_document_without_the_marker_is_not_a_vocabulary() {
        let doc = Document::parse("notes.md", "---\ntitle: Notes\n---\nbody\n").unwrap();
        assert!(Vocabulary::from_meta(&doc.meta).is_none());
    }
}
