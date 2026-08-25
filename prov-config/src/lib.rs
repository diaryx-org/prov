//! Pure workspace policy and codec types.
//!
//! This crate deliberately contains no filesystem discovery or pointer
//! resolution. It describes, parses, serializes, and diagnoses workspace
//! policy values; the `prov` crate applies that policy to a mutable workspace.

mod textdist;

pub mod config;
pub mod vocabulary;

pub use config::{
    About, ConfigIssue, ConfigIssueKind, FIELD_TYPES, FieldSpec, Fixity, IdStorage, OpenClosed,
    ROOT_CONFIG_KEY, RelationDef, RelationStyleConfig, SPEC_VERSION, WorkspaceConfig, diagnose,
    field_type_as_config_str, field_type_from_config_str, is_valid_scope_path,
    is_valid_workspace_id, metadata_format_from_str, metadata_format_str, spec_ahead,
};
pub use vocabulary::{Term, Vocabulary};

/// The closest live vocabulary term, if it is within the policy's typo
/// threshold. Kept as a policy-level operation so callers do not need access
/// to the crate-private edit-distance implementation.
pub fn nearest_vocabulary_term(key: &str, candidates: &[String]) -> Option<String> {
    textdist::nearest_owned(key, candidates)
}

/// Whether two strings are within the typo threshold used by diagnostics.
pub fn vocabulary_terms_near(a: &str, b: &str) -> bool {
    (1..=2).contains(&textdist::levenshtein(a, b))
}

/// The edit distance when two vocabulary-like strings are near enough to be
/// considered a typo.
pub fn vocabulary_distance(a: &str, b: &str) -> Option<usize> {
    let distance = textdist::levenshtein(a, b);
    (1..=2).contains(&distance).then_some(distance)
}
