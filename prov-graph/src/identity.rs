//! Identity — the id *type*, and what makes one well-formed.
//!
//! An id is a stable, opaque name for a document. This module is the read half
//! of prov's identity layer: the [`Id`] newtype, the alphabet and length it is
//! spelled in, and [`verify`] — the check-character arithmetic that catches a
//! typo'd `id:` link before it dangles silently.
//!
//! *Minting* an id is a write, and lives in `prov-identity`.
//! alongside the trigger set that decides when a document earns one. The split
//! matters because this crate never issues an id; it only recognizes ids
//! something else issued, which is exactly what link resolution needs.
//!
//! ## The ID scheme
//!
//! Prov's internal IDs share their lineage with diaryx's ARK blades but
//! carry no NAAN or shoulder — they are workspace-internal, not published
//! permalinks (DESIGN §4's two identity layers). The primitives come from the
//! [`moid`] crate (*minimal opaque ID*): an ID is [`BLADE_RANDOM_LEN`]
//! random characters from the 29-character NOID extended-digit alphabet
//! ([`moid::Alphabet::noid_xdigit`] — digits plus consonants: no vowels, so no
//! accidental words; no `l`, so no ambiguity with `1`) plus one NOID check
//! character, so a typo'd ID is *detected* rather than silently resolving to
//! nothing. The alphabet is the canonical NOID one, so the check character
//! agrees with a real NOID minter and not merely with our own arithmetic. An ID
//! may therefore contain — and begin with — a digit; anything stamping one into
//! metadata must keep it a *string*.

use moid::Alphabet;

/// A stable, opaque document identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Id(pub String);

impl Id {
    /// The id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Random characters per ID (excluding the check character). 29^6 ≈ 595M —
/// collision-free in practice for a workspace, enforced absolutely by
/// mint-with-rejection.
pub const BLADE_RANDOM_LEN: usize = 6;

/// Total ID length: the random body plus one check character.
pub const BLADE_LEN: usize = BLADE_RANDOM_LEN + 1;

/// Prov IDs use [`BLADE_RANDOM_LEN`] random NOID extended-digit characters plus
/// a NOID check character. Minting lives in `prov-identity`; this crate only
/// verifies IDs.
/// Whether `id` is a well-formed prov ID: correct length, alphabet-only,
/// and a matching trailing check character. This is what catches a typo'd
/// `prov:` link before it dangles silently.
pub fn verify(id: &str) -> bool {
    moid::Minter::new(Alphabet::noid_xdigit(), BLADE_RANDOM_LEN)
        .validate(id)
        .is_ok()
}

/// Where a document's stable ID is persisted — the identity-storage axis
/// (DESIGN §5). Orthogonal to *when* an ID is minted (`prov`'s `Registration`) and to
/// how references are spelled; this is purely the ID's *home*.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IdStorage {
    /// **Registry only** (`registry`): IDs live solely in the registry document —
    /// authoritative, non-derivable, resolved by direct lookup. The cleanest
    /// documents (no `id` clutter), but identity does not travel with a file.
    Registry,
    /// **Frontmatter + registry** (`both`, the default): each document also
    /// carries its own ID in an `id` frontmatter field (a portable, self-describing
    /// shadow), and the registry is retained as a rebuildable cache + tombstone
    /// ledger. The ID travels with the file across copies and out-of-band moves.
    #[default]
    Frontmatter,
    /// **Frontmatter only** (`frontmatter`): the `id` field is the sole home; no
    /// registry document is written and resolution rebuilds the id→path map by
    /// scanning frontmatter. Maximally self-describing, but it forfeits tombstones
    /// (a deleted file takes its ID with it), so an ID can in principle be reminted.
    FrontmatterOnly,
}

impl IdStorage {
    /// Whether this mode writes the ID into each document's `id` frontmatter.
    pub fn stamps_frontmatter(self) -> bool {
        matches!(self, IdStorage::Frontmatter | IdStorage::FrontmatterOnly)
    }

    /// Whether this mode keeps a registry document (the authoritative store, or —
    /// under [`Frontmatter`](IdStorage::Frontmatter) — a rebuildable cache).
    pub fn keeps_registry(self) -> bool {
        matches!(self, IdStorage::Registry | IdStorage::Frontmatter)
    }

    /// Parse the `id_storage` config spelling; unknown → `None`. `both` is the
    /// frontmatter+registry default; `frontmatter` is the registry-less mode.
    pub fn from_config_str(value: &str) -> Option<Self> {
        match value {
            "registry" => Some(Self::Registry),
            "both" => Some(Self::Frontmatter),
            "frontmatter" => Some(Self::FrontmatterOnly),
            _ => None,
        }
    }

    /// The `id_storage` config spelling.
    pub fn as_config_str(self) -> &'static str {
        match self {
            Self::Registry => "registry",
            Self::Frontmatter => "both",
            Self::FrontmatterOnly => "frontmatter",
        }
    }
}
