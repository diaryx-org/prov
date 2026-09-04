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

use std::collections::HashMap;
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

/// A directory listing turned around: which of its files are conventionally
/// named sidecars, and for what.
///
/// The lookup used to run the other way — build all six `<payload>.<ext>`
/// candidates for a file and ask an ordered set of the listing whether it holds
/// any of them. That is six `PathBuf` allocations and six ordered-set probes
/// *per file scanned*, to answer "no" in every workspace with no attachments in
/// it; over twenty thousand documents it was 16% of a `check`, most of it
/// comparing paths component by component inside the set.
///
/// Inverting it costs one pass over the listing — the same
/// `<payload>.<ext>` convention read backwards, so the two are exact inverses —
/// and leaves the per-file question a single hash lookup that usually misses.
/// A workspace with no attachments builds an empty map and every probe is that
/// miss.
///
/// The `content` pointer is still what confirms a hit
/// ([`Graph::sidecar_claims`]); this only says which files are worth asking
/// about.
#[derive(Debug, Default, Clone)]
pub struct ShadowProbe {
    /// Payload path → the conventionally named sidecars actually present, in
    /// [`sidecar_candidates`]' preference order.
    sidecars: HashMap<PathBuf, Vec<PathBuf>>,
}

impl ShadowProbe {
    /// Index the workspace-relative files a scan enumerated.
    pub fn over<'a>(listing: impl IntoIterator<Item = &'a PathBuf>) -> Self {
        let mut ranked: HashMap<PathBuf, Vec<(usize, PathBuf)>> = HashMap::new();
        for entry in listing {
            let Some(rank) = entry
                .extension()
                .and_then(|e| e.to_str())
                .and_then(|ext| SIDECAR_EXTENSIONS.iter().position(|known| *known == ext))
            else {
                continue;
            };
            // `photo.jpg.yaml` names `photo.jpg` — `sidecar_candidates` run
            // backwards, which is what makes this the same question.
            ranked
                .entry(entry.with_extension(""))
                .or_default()
                .push((rank, entry.clone()));
        }
        let sidecars = ranked
            .into_iter()
            .map(|(payload, mut found)| {
                found.sort_by_key(|(rank, _)| *rank);
                (payload, found.into_iter().map(|(_, path)| path).collect())
            })
            .collect();
        Self { sidecars }
    }

    /// The sidecars the listing holds for `payload`, in preference order —
    /// empty for a file nothing beside it could be claiming.
    fn sidecars_for(&self, payload: &Path) -> &[PathBuf] {
        self.sidecars.get(payload).map_or(&[], Vec::as_slice)
    }
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
    /// `probe` is the [`ShadowProbe`] over the workspace-relative files the
    /// calling scan already enumerated (its directory read), so a shadow check
    /// costs one hash lookup rather than a stat per metadata extension — this
    /// runs per file in the flat title and id scans, and per reachable path in
    /// the vocabulary and fixity passes
    /// (`validate::Workspace::reachable_documents`). A sidecar outside the
    /// listing the probe was built over therefore does not shadow, which is the
    /// same bound the scans themselves observe.
    pub async fn is_shadowed_payload(&self, path: &Path, probe: &ShadowProbe) -> bool {
        for candidate in probe.sidecars_for(path) {
            if self.sidecar_claims(candidate, path).await {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    /// The probe is `sidecar_candidates` read backwards, so the two must agree
    /// about every file in a listing — that equivalence is the whole argument
    /// for inverting the lookup.
    #[test]
    fn the_probe_answers_what_probing_every_candidate_answered() {
        let listing = paths(&[
            "photo.jpg",
            "photo.jpg.yaml",
            "notes/scan.pdf",
            "notes/scan.pdf.json",
            "notes/a.md",
            "loose.toml",
        ]);
        let probe = ShadowProbe::over(listing.iter());
        for path in &listing {
            let by_candidate: Vec<PathBuf> = sidecar_candidates(path)
                .filter(|c| listing.contains(c))
                .collect();
            assert_eq!(
                probe.sidecars_for(path),
                by_candidate.as_slice(),
                "{}",
                path.display()
            );
        }
    }

    /// A payload with more than one conventional sidecar is reported in
    /// `SIDECAR_EXTENSIONS` order however the directory listed them, because the
    /// caller confirms them in that order and stops at the first that claims.
    #[test]
    fn several_sidecars_come_back_in_preference_order() {
        let listing = paths(&["photo.jpg.figl", "photo.jpg.yaml", "photo.jpg.json"]);
        let probe = ShadowProbe::over(listing.iter());
        assert_eq!(
            probe.sidecars_for(Path::new("photo.jpg")),
            paths(&["photo.jpg.yaml", "photo.jpg.json", "photo.jpg.figl"]).as_slice()
        );
    }

    /// The common case: nothing in the listing could be a sidecar, so every
    /// file's probe is a miss and no candidate path is ever built.
    #[test]
    fn a_listing_with_no_sidecars_claims_nothing() {
        let listing = paths(&["index.md", "a.md", "b.md"]);
        let probe = ShadowProbe::over(listing.iter());
        assert!(probe.sidecars.is_empty());
        assert!(probe.sidecars_for(Path::new("a.md")).is_empty());
    }
}
