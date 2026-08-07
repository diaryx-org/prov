//! Fixity — content checksums that let prov detect *bit-rot*, not just
//! broken links.
//!
//! Link validation ([`crate::validate`]) answers "does the graph still hold
//! together?"; fixity answers the other archival question: "are the bytes still
//! the bytes?" A stored hash, recomputed on read and compared, catches the
//! silent corruption an archive most fears — a flipped bit in a decade-old
//! attachment that no link check would ever notice.
//!
//! ## Why SHA-256, and why hand-rolled
//!
//! The algorithm is **SHA-256**, and a hash is recorded as `sha256:<hex>` — the
//! prefix names the algorithm, so the field is self-describing and a future one
//! can be added without ambiguity. SHA-256 is the archival lingua franca: a
//! prov workspace's fixity is verifiable by *anyone*, with standard tools
//! (`sha256sum`, BagIt validators), not only by prov — the same
//! tool-agnostic, self-describing ethos the whole crate is built on.
//!
//! It is implemented here rather than pulled from a crate for the same reason
//! [`crate::exec::block_on`] and the journal's FNV checksum are: prov keeps
//! its dependency surface tiny and WASM-clean (no build-toolchain cost, nothing
//! to audit). SHA-256 is a fully specified, deterministic function with published
//! test vectors, so correctness is *checked*, not trusted — the tests below pin
//! it to the NIST vectors and to what `sha256sum` produces.
//!
//! ## Not hashing what has not changed
//!
//! Hashing is cheap to describe and expensive to run, and a capture runs it over
//! every file in the workspace. [`FixityCache`] is the device-local memory that
//! lets a capture skip the files whose stat says they are untouched — and, just
//! as importantly, the argument for which passes may consult it and which may
//! never. See its module documentation; the short version is that the bit-rot
//! check must not, because bit-rot is exactly the change a stat cannot see.

mod cache;

pub use cache::FixityCache;

/// The SHA-256 round constants — the first 32 bits of the fractional parts of
/// the cube roots of the first 64 primes (FIPS 180-4 §4.2.2).
#[rustfmt::skip]
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// The initial hash state — the first 32 bits of the fractional parts of the
/// square roots of the first 8 primes (FIPS 180-4 §5.3.3).
const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// The raw SHA-256 digest of `bytes`, as 32 bytes.
fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut h = H0;

    // Pad: the message, a 0x80 byte, zeros, then the bit-length as a 64-bit
    // big-endian integer — to a multiple of 64 bytes (FIPS 180-4 §5.1.1).
    let bit_len = (bytes.len() as u64).wrapping_mul(8);
    let mut msg = bytes.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    // Compress each 512-bit block.
    for block in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes(word.try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(v);
        }
    }

    let mut out = [0u8; 32];
    for (chunk, word) in out.chunks_exact_mut(4).zip(h) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// The fixity digest of `bytes`, spelled `sha256:<lowercase-hex>` — the form
/// recorded in a sidecar, a frontmatter field, or a recycle-bin tombstone, and
/// the form [`verify`] checks against. The `sha256:` prefix names the algorithm,
/// so the record is self-describing and a future digest can be distinguished.
pub fn digest(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(7 + 64);
    s.push_str("sha256:");
    for byte in sha256(bytes) {
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
