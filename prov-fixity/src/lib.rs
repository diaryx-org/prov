//! Fixity — content checksums that let prov detect *bit-rot*, not just
//! broken links.
//!
//! Link validation in the higher-level `prov` crate answers "does the graph
//! still hold together?"; fixity answers the other archival question: "are the
//! bytes still the bytes?" A stored hash, recomputed on read and compared, catches the
//! silent corruption an archive most fears — a flipped bit in a decade-old
//! attachment that no link check would ever notice.
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
//! here. This crate did once carry its own, on the reasoning that guards
//! [`prov_graph::exec::block_on`] and the journal's FNV checksum — keep the
//! dependency surface tiny and WASM-clean. It is the one place that reasoning
//! loses: `sha2` is pure Rust and `no_std`-capable, so it costs no build
//! toolchain and compiles on `wasm32-unknown-unknown` like everything else
//! here, while a hand-written loop cannot reach the hardware path — `sha2`
//! dispatches to SHA-NI on x86-64 and to the ARMv8 crypto extensions on
//! aarch64, and `stamp --all` hashes every covered file in the workspace.
//!
//! What does not change is that correctness here is *checked*, not trusted:
//! SHA-256 is a fully specified, deterministic function with published test
//! vectors, and the tests below pin this crate's output to the NIST vectors and
//! to what `sha256sum` produces — now testing the binding rather than a local
//! compression loop, which is exactly what they are for.

use sha2::{Digest, Sha256};

/// How far content checksums cover a workspace.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Fixity {
    /// No content checksums are recorded or verified.
    Off,
    /// Attachment payloads only.
    #[default]
    Payloads,
    /// Attachment payloads and document bodies.
    Full,
}

impl Fixity {
    /// Whether attachment payloads are checksummed.
    pub fn covers_payloads(self) -> bool {
        matches!(self, Self::Payloads | Self::Full)
    }

    /// Whether document bodies are checksummed.
    pub fn covers_bodies(self) -> bool {
        matches!(self, Self::Full)
    }

    /// Parse the configuration spelling; unknown values return `None`.
    pub fn from_config_str(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "attachments" => Some(Self::Payloads),
            "all" => Some(Self::Full),
            _ => None,
        }
    }

    /// Return the configuration spelling.
    pub fn as_config_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Payloads => "attachments",
            Self::Full => "all",
        }
    }
}

/// The fixity digest of `bytes`, spelled `sha256:<lowercase-hex>` — the form
/// recorded in a sidecar, a frontmatter field, or a recycle-bin tombstone, and
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
