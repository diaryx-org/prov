//! Fixity — content checksums that let prov detect *bit-rot*, not just
//! broken links.
//!
//! Link validation in the higher-level `prov` crate answers "does the graph
//! still hold together?"; fixity answers the other archival question: "are the
//! bytes still the bytes?" A stored hash, recomputed on read and compared,
//! catches the silent corruption an archive most fears — a flipped bit in a
//! decade-old attachment that no link check would ever notice.
//!
//! ## Why this sits in the read core
//!
//! Everything here is a pure function of its inputs: a policy enum, a digest,
//! two predicates over a recorded string, and one over a parsed document's
//! shape. None of it opens a file, and none of it can change one — the same
//! reason [`identity`](crate::identity) sits here rather than above the read
//! boundary. The *writes* that record a digest (`attach`, `save`, the manifest
//! verbs) live in `prov`, and the pass that reads bytes back to compare them is
//! `prov`'s `check`.
//!
//! ## Why SHA-256
//!
//! The algorithm is **SHA-256**, and a hash is recorded as `sha256:<hex>` — the
//! prefix names the algorithm, so the field is self-describing and a future one
//! can be added without ambiguity. SHA-256 is the archival lingua franca: a
//! prov workspace's fixity is verifiable by *anyone*, with standard tools
//! (`sha256sum`, BagIt validators), not only by prov — the same
//! tool-agnostic, self-describing ethos the whole crate is built on.
//!
//! The compression function comes from `sha2` rather than being written out
//! here. This module did once carry its own, on the reasoning that guards
//! [`exec::block_on`](crate::exec::block_on) and the journal's FNV checksum —
//! keep the dependency surface tiny and WASM-clean. It is the one place that
//! reasoning loses: `sha2` is pure Rust and `no_std`-capable, so it costs no
//! build toolchain and compiles on `wasm32-unknown-unknown` like everything
//! else here, while a hand-written loop cannot reach the hardware path — `sha2`
//! dispatches to SHA-NI on x86-64 and to the ARMv8 crypto extensions on
//! aarch64, and `stamp --all` hashes every covered file in the workspace.
//!
//! What does not change is that correctness here is *checked*, not trusted:
//! SHA-256 is a fully specified, deterministic function with published test
//! vectors, and the tests below pin this module's output to the NIST vectors
//! and to what `sha256sum` produces — now testing the binding rather than a
//! local compression loop, which is exactly what they are for.

use sha2::{Digest, Sha256};

/// Whether a workspace records content checksums.
///
/// Not a coverage scale, though it was one — `off | attachments | all`, where
/// `all` additionally checksummed a combined document's *body*. What a checksum
/// covers is now read off the document's shape instead, and the shape answers
/// better than a setting could, so all that is left to configure is whether
/// checksums are written at all. [`covers`](Fixity::covers) is the rule and why.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Fixity {
    /// No content checksums are recorded.
    Off,
    /// Every node whose content is a file of its own records one.
    #[default]
    On,
}

impl Fixity {
    /// Whether a `content_hash` is written for `doc`: fixity is on, **and** the
    /// hash would cover a file other than the one recording it — an attachment's
    /// payload, or a separated document's prose body.
    ///
    /// That second half is the whole rule, and it is a claim about what a
    /// checksum is *worth*, not about how much work to do. A hash covering a
    /// sibling file is one artifact vouching for another, whole-file:
    /// `sha256sum body.md` reproduces it by hand, which is the tool-agnostic
    /// verifiability this module's choice of SHA-256 exists to buy. A hash of a
    /// combined document's own body buys none of it. It covers
    /// [`Document::body`](crate::document::Document::body), a parsed substring —
    /// and not reliably a contiguous one, since a metadata block need not sit at
    /// a file's edge and the body is then the prose from both sides of it,
    /// concatenated. There is no file to hand `sha256sum`, and no rule to state
    /// to an outside verifier short of reimplementing prov's parser. A guarantee
    /// that silently changed strength with the carrier is what retired the tier.
    ///
    /// A **manifest node** is not covered here, and is not thereby exempt: its
    /// checksum pins the manifest document it declares, so it is recorded and
    /// refreshed by the manifest verbs — the only ones that know what rebuilding
    /// it costs. See [`manifest`](crate::manifest).
    pub fn covers(self, doc: &crate::document::Document) -> bool {
        self == Self::On && doc.content_attr().is_some()
    }

    /// Whether checksums are recorded at all — the axis on its own, for a caller
    /// with no parsed document to ask about: a sidecar being minted, whose shape
    /// is not in question because the verb is what is giving it one.
    pub fn is_on(self) -> bool {
        self == Self::On
    }

    /// Parse the configuration spelling; unknown values return `None`.
    ///
    /// `attachments` is still read. It was this axis's default spelling while
    /// coverage was tiered, so it is written into every `prov.yaml` predating
    /// this, and it names a subset of what `on` now covers — nothing an author
    /// asked for is lost by taking it at its word. `all` is deliberately *not*
    /// read, though it was the other live spelling: what it asked for was body
    /// checksums, which is precisely the thing that went away, and a workspace
    /// that asked for them is owed the news rather than something quietly
    /// narrower. It lands as an invalid value on a recognized axis — the default
    /// is kept and `check` reports it, listing the spellings that remain.
    pub fn from_config_str(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "on" | "attachments" => Some(Self::On),
            _ => None,
        }
    }

    /// Return the configuration spelling.
    pub fn as_config_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
        }
    }
}

/// The fixity digest of `bytes`, spelled `sha256:<lowercase-hex>` — the form
/// recorded in an attachment sidecar, a manifest row, or a node's frontmatter, and
/// the form [`verify`] checks against. The `sha256:` prefix names the algorithm,
/// so the record is self-describing and a future digest can be distinguished.
pub fn digest(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(7 + 64);
    s.push_str("sha256:");
    for byte in Sha256::digest(bytes) {
        s.push(char::from_digit((byte >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((byte & 0xf) as u32, 16).unwrap());
    }
    s
}

/// Whether `bytes` still hash to the `recorded` digest. `true` when the recorded
/// value is empty — nothing was ever recorded, so there is nothing to contradict
/// (a document predating fixity is not "corrupt"). A recorded value prov
/// cannot recognize (a future algorithm) is treated as *unverifiable*, which is
/// also `true`: fixity never raises a false alarm over a hash it does not
/// understand, it simply cannot vouch for it.
pub fn verify(bytes: &[u8], recorded: &str) -> bool {
    match recorded.strip_prefix("sha256:") {
        Some(_) => digest(bytes) == recorded,
        None if recorded.is_empty() => true,
        None => true,
    }
}

/// Whether `recorded` is a fixity digest prov can actually check — the
/// predicate that separates "verified" from "unverifiable" so a caller can tell
/// a matching hash from one it had to take on faith.
pub fn is_recognized(recorded: &str) -> bool {
    recorded.starts_with("sha256:")
}

#[cfg(test)]
mod tests {
    use super::*;

    // The NIST / FIPS 180-4 known-answer vectors. If these pass, the
    // implementation is SHA-256 — correctness is checked, not trusted.
    #[test]
    fn matches_the_published_sha256_vectors() {
        assert_eq!(
            digest(b""),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            digest(b"abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            digest(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "sha256:248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn crosses_a_block_boundary_correctly() {
        // 1,000,000 'a's — the classic long vector that exercises multi-block
        // compression and the length padding.
        let million_a = vec![b'a'; 1_000_000];
        assert_eq!(
            digest(&million_a),
            "sha256:cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn verify_accepts_the_matching_digest_and_rejects_a_changed_byte() {
        let recorded = digest(b"the original bytes");
        assert!(verify(b"the original bytes", &recorded));
        assert!(!verify(b"the corrupted bytes", &recorded));
    }

    #[cfg(feature = "yaml")]
    #[test]
    fn covers_exactly_the_documents_whose_hash_would_name_another_file() {
        use crate::document::Document;
        let parse = |p: &str, t: &str| Document::parse(p, t).unwrap();

        // A separated node and an attachment sidecar both point `content` at a
        // sibling — one file vouching for another, which is the covered shape.
        let separated = parse("notes/a.yaml", "title: A\ncontent: a.md\n");
        let sidecar = parse("photo.jpg.yaml", "title: Photo\ncontent: photo.jpg\n");
        assert!(Fixity::On.covers(&separated));
        assert!(Fixity::On.covers(&sidecar));

        // A combined document's hash could only cover its own parsed body. That
        // is the coverage this axis dropped, so it is not covered at any setting.
        let combined = parse("note.md", "---\ntitle: Note\n---\nhello\n");
        assert!(!Fixity::On.covers(&combined));

        // A manifest node pins its manifest, but through the manifest verbs.
        let node = parse(
            "photos.yaml",
            "title: Photos\nmanifest: photos.manifest.yaml\n",
        );
        assert!(!Fixity::On.covers(&node));

        // `off` covers nothing, whatever the shape.
        assert!(!Fixity::Off.covers(&separated));
        assert!(!Fixity::Off.covers(&sidecar));
    }

    #[test]
    fn reads_the_retired_default_spelling_but_not_the_retired_tier() {
        // `attachments` named a subset of what `on` covers, so it is honored.
        assert_eq!(Fixity::from_config_str("attachments"), Some(Fixity::On));
        assert_eq!(Fixity::from_config_str("on"), Some(Fixity::On));
        assert_eq!(Fixity::from_config_str("off"), Some(Fixity::Off));
        // `all` asked for body checksums, which is the thing that went away —
        // it is reported, not silently reinterpreted.
        assert_eq!(Fixity::from_config_str("all"), None);
        // What is written back is always the current spelling.
        assert_eq!(Fixity::On.as_config_str(), "on");
        assert_eq!(Fixity::Off.as_config_str(), "off");
    }

    #[test]
    fn verify_never_cries_wolf_over_an_unrecorded_or_unknown_digest() {
        // Nothing recorded → nothing to contradict.
        assert!(verify(b"anything", ""));
        // A digest from an algorithm prov does not know → unverifiable, not
        // corrupt. `is_recognized` is how a caller tells the two apart.
        assert!(verify(b"anything", "blake3:deadbeef"));
        assert!(!is_recognized("blake3:deadbeef"));
        assert!(is_recognized("sha256:e3b0c442"));
    }
}
