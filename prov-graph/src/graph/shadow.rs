//! Shadowed payloads — the files prov can read but must not.
//!
//! `attach --opaque` makes a promise: prov will link, move and fixity-check this
//! file, but never read *it* as a document. When the payload happens to be
//! something prov can parse — a `.md`, a `.yaml` — keeping that promise means
//! every scan has to *notice* the sidecar beside it and skip the file. That
//! check lives here rather than beside the `attach` verb because the check is a
//! read: the census, the title scan and the id scan all owe it, and none of them
//! is attaching anything.
//!
//! The convention is the fast path and the `content` pointer is authoritative. A
//! sidecar under a non-conventional name still claims its payload; it just is
//! not found by probing, which is why `attach`'s own sweep confirms the pointer
//! rather than trusting the name.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::Graph;
use crate::fs::ReadStorage;
use crate::index::IdIndex;
use crate::link;

const SIDECAR_EXTENSIONS: &[&str] = &["yaml", "yml", "json", "toml", "fig", "figl"];

/// Every path that could be `payload`'s sidecar under the `<payload>.<ext>`
/// convention, in reverse-lookup preference order. The probe half of the lookup;
/// the `content` pointer confirms a hit ([`Graph::sidecar_claims`]).
///
/// Note the convention cannot collide with a *separated* document's metadata
/// half, which replaces the extension (`note.md` → `note.yaml`) rather than
/// appending to it (`note.md.yaml`).
pub fn sidecar_candidates(payload: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    let name = payload
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    SIDECAR_EXTENSIONS
        .iter()
        .map(move |ext| payload.with_file_name(format!("{name}.{ext}")))
}

impl<FS: ReadStorage, Ix: IdIndex> Graph<FS, Ix> {
    /// Whether the document at `candidate` is an attachment sidecar whose
    /// `content` resolves to `payload` — the authoritative half of the reverse
    /// lookup, the `<payload>.<ext>` convention above being only the probe.
    ///
    /// Requires [`is_attachment`](crate::Document::is_attachment), so a separated
    /// *prose* node never reads as one: its body is a document in its own right,
    /// and prov must keep scanning it. Unreadable or unparsable candidates simply
    /// do not claim (this runs inside best-effort scans).
    pub async fn sidecar_claims(&self, candidate: &Path, payload: &Path) -> bool {
        let Ok((_, doc)) = self.load(candidate).await else {
            return false;
        };
        let Some(content) = doc.content_attr() else {
            return false;
        };
        let dir = candidate.parent().unwrap_or(Path::new(""));
        doc.is_attachment() && link::normalize(dir.join(content)) == payload
    }

    /// Whether `path` — a file prov *can* read — has been deliberately shadowed:
    /// claimed as an opaque payload by an attachment sidecar beside it. The
    /// promise `attach --opaque` makes, enforced: prov links, moves and fixity-
    /// checks the file (through its sidecar's own `content_hash`) but never
    /// reads *it* as a document, so its title stays out of the title index, any
    /// `id` it shows stays out of the registry, any `fields` value it carries is
    /// never checked against a vocabulary, and any `content_hash` it shows is
    /// never treated as its own.
    ///
    /// `listing` is the set of workspace-relative files the calling scan already
    /// enumerated (its directory read), so a shadow check costs a set lookup
    /// rather than a stat per metadata extension — this runs per file in the flat
    /// title and id scans, and per reachable path in the vocabulary and fixity
    /// passes (`validate::Workspace::reachable_documents`). A sidecar outside
    /// the listing therefore does not shadow, which is the same bound the scans
    /// themselves observe.
    pub async fn is_shadowed_payload(&self, path: &Path, listing: &BTreeSet<PathBuf>) -> bool {
        for candidate in sidecar_candidates(path) {
            if listing.contains(&candidate) && self.sidecar_claims(&candidate, path).await {
                return true;
            }
        }
        false
    }
}
