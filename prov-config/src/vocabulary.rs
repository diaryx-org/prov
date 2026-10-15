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
//! reasons about the term *keys*, each term's stable `id`, whether it is
//! `retired`, whether it `holds` a document back from an export that
//! holds on its field, and which other terms it `also_reads`; everything else
//! in a term entry is tier-3 payload prov carries but never reads (a diaryx
//! audience's gate/theme).
//!
//! # A term can read what another reads
//!
//! A term may name other terms it `also_reads`: an export gated on `family`
//! whose term reads `friends` admits a document declaring `friends` too, so a
//! page is marked once, with the readership it is for, and every readership
//! that reads that one reaches it. The relation is transitive (family reads
//! friends, friends reads public, so family reads public) and is read off the
//! *terms*, never off the documents: whether a document leaves an export is
//! still a question about one field on that one document, answered against a
//! set the vocabulary resolved once ([`Vocabulary::reads`]).
//!
//! A cycle is not an error to fail on: every term on it reads every other,
//! which is a set like any other, and the resolution stops where it has been.
//! It is almost always a mistake, though, so `check` reports it
//! ([`Vocabulary::reads_cycles`]). A name that is no term reads nothing.

use std::collections::BTreeMap;

use crate::OpenClosed;
use prov_graph::identity::Id;
use prov_graph::meta::Value;

/// A single term in a vocabulary. prov reads only the fields here; any other
/// keys on the term entry are carried, not interpreted.
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
    /// Whether a document carrying this term waits: an export whose `hold`
    /// names this vocabulary's field holds back a document the gate admits
    /// when its value is a term that `holds: true`, as it does one declaring
    /// the literal `true`. So a `status: draft` proposal stays home while
    /// `status: accepted` leaves, and which states are unfinished is said once,
    /// on the terms, rather than in every export.
    pub holds: bool,
    /// The terms this one also reads, by key, as written: an export gated on
    /// this term admits what each of them admits, transitively. See the module
    /// docs. A flat store writes term names (`also_reads: [friends]`); a
    /// reified one writes links to the term nodes, which the loader resolves
    /// to their keys. A name that is no term reads nothing.
    pub also_reads: Vec<String>,
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
                        holds: entry.get("holds").and_then(Value::as_bool).unwrap_or(false),
                        also_reads: entry.get(ALSO_READS).map(scalar_names).unwrap_or_default(),
                    },
                    // A bare `term:` (null/scalar value) is a live term with no metadata.
                    None => Term {
                        id: None,
                        means: None,
                        retired: false,
                        holds: false,
                        also_reads: Vec::new(),
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

    /// Whether `value` names a term that holds a document back — see
    /// [`Term::holds`]. A retired term still holds: retirement says a value is
    /// no longer for new content, not that the documents already carrying it
    /// are ready to leave.
    pub fn holds(&self, value: &str) -> bool {
        self.terms.get(value).is_some_and(|t| t.holds)
    }

    /// Every term `term` reads through [`Term::also_reads`], transitively and
    /// without `term` itself: nearest first, then in the order each term
    /// declares them, each once.
    ///
    /// Empty for a term that reads nothing and for a name that is no term. A
    /// name in an `also_reads` list that is no term is skipped, so a typo
    /// widens nothing. A cycle stops where it has already been; a term on one
    /// is not in its own answer, since it is what was asked about.
    pub fn reads(&self, term: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut queue: std::collections::VecDeque<&str> = std::collections::VecDeque::new();
        queue.push_back(term);
        while let Some(at) = queue.pop_front() {
            let Some(entry) = self.terms.get(at) else {
                continue;
            };
            for next in &entry.also_reads {
                let next = next.trim();
                if next == term || !self.terms.contains_key(next) || out.iter().any(|t| t == next) {
                    continue;
                }
                out.push(next.to_string());
                queue.push_back(next);
            }
        }
        out
    }

    /// The cycles among the terms' [`also_reads`](Term::also_reads), each as
    /// the sorted names of the terms on it, each cycle once, in name order.
    ///
    /// Every term on a cycle reads every other, which resolves well enough
    /// ([`reads`](Self::reads)) and is almost never meant: a family that reads
    /// friends who read the family is one readership written as two. `check`
    /// reports each.
    pub fn reads_cycles(&self) -> Vec<Vec<String>> {
        let mut cycles: Vec<Vec<String>> = Vec::new();
        for name in self.terms.keys() {
            if cycles.iter().any(|c| c.contains(name)) {
                continue;
            }
            let on_cycle = self.terms[name]
                .also_reads
                .iter()
                .any(|next| next.trim() == name)
                || self
                    .reads(name)
                    .iter()
                    .any(|other| self.reads(other).iter().any(|back| back == name));
            if !on_cycle {
                continue;
            }
            let mut members: Vec<String> = self
                .reads(name)
                .into_iter()
                .filter(|other| self.reads(other).iter().any(|back| back == name))
                .collect();
            members.push(name.clone());
            members.sort();
            cycles.push(members);
        }
        cycles
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

/// The key a term declares the terms it also reads under — on a flat store's
/// term entry, and on a reified store's term node.
pub const ALSO_READS: &str = "also_reads";

/// The trimmed, non-empty names in a scalar or a sequence of scalars — how a
/// flat store's `also_reads` is read. Anything else names nothing.
fn scalar_names(value: &Value) -> Vec<String> {
    let one = |v: &Value| {
        v.as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    match value {
        Value::Sequence(items) => items.iter().filter_map(one).collect(),
        other => one(other).into_iter().collect(),
    }
}

#[cfg(test)]
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
    fn a_term_holds_only_when_it_says_so() {
        let v = vocab(
            "vocabulary:\n  field: status\n  values: closed\n\
             terms:\n  draft:\n    holds: true\n  accepted: {}\n  old:\n    holds: true\n    retired: true\n",
        );
        assert!(v.holds("draft"));
        assert!(!v.holds("accepted"));
        assert!(
            v.holds("old"),
            "retiring a term does not release what carries it"
        );
        assert!(!v.holds("unknown"));
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

    /// Reading is transitive and nearest first, and a name that is no term
    /// widens nothing.
    #[test]
    fn a_term_reads_what_the_terms_it_reads_read() {
        let v = vocab(
            "vocabulary:\n  field: audience\n  values: closed\n\
             terms:\n  closest:\n    also_reads: [family]\n  family:\n    also_reads: friends\n  \
             friends:\n    also_reads: [public, nobody]\n  public: {}\n  work: {}\n",
        );
        assert_eq!(v.reads("closest"), ["family", "friends", "public"]);
        assert_eq!(v.reads("family"), ["friends", "public"]);
        assert_eq!(v.reads("public"), Vec::<String>::new());
        assert_eq!(v.reads("work"), Vec::<String>::new());
        assert_eq!(v.reads("unknown"), Vec::<String>::new());
        assert!(v.reads_cycles().is_empty());
    }

    /// A cycle resolves, stopping where it has been, and is reported once.
    #[test]
    fn a_cycle_resolves_and_is_reported() {
        let v = vocab(
            "vocabulary:\n  field: audience\n  values: closed\n\
             terms:\n  family:\n    also_reads: [friends]\n  friends:\n    also_reads: [family, public]\n  \
             public: {}\n  me:\n    also_reads: [me]\n",
        );
        assert_eq!(v.reads("family"), ["friends", "public"]);
        assert_eq!(v.reads("friends"), ["family", "public"]);
        assert_eq!(v.reads("me"), Vec::<String>::new());
        assert_eq!(
            v.reads_cycles(),
            vec![
                vec!["family".to_string(), "friends".to_string()],
                vec!["me".to_string()],
            ]
        );
    }

    #[test]
    fn a_document_without_the_marker_is_not_a_vocabulary() {
        let doc = Document::parse("notes.md", "---\ntitle: Notes\n---\nbody\n").unwrap();
        assert!(Vocabulary::from_meta(&doc.meta).is_none());
    }
}
