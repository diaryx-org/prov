//! Identity policy — when a document earns an id, and how one is minted.
//!
//! Everything here is optional. The graph and mutation layers operate on paths
//! and never require an ID. This module decides **when** a document earns a
//! stable ID (the trigger set) and **what** that ID looks like (the mint).
//! *Where* IDs are stored is [`prov_graph::index`]; the [`Id`] type itself and the
//! well-formedness check over it are [`prov_graph::identity`], because
//! resolving a link needs to *recognize* an id without being able to issue one.
//!
//! The default is [`NoIdentity`] — identity off, no ID ever written. The
//! recommended lazy policy registers an ID only when something durably refers
//! to a document (a link-by-id or a publish), keeping the authoritative set as
//! small as possible.
//!
//! Minting is random (opaque for free), with uniqueness enforced by rejection
//! against the index — including its tombstones, so a deleted document's ID is
//! never reissued. An ID may contain, and begin with, a digit; anything
//! stamping one into metadata must keep it a *string* (see
//! `prov-store`'s `edit::infer_scalar`). The alphabet, check-character arithmetic,
//! and seeded PRNG all live in [`moid`].

use std::path::Path;

use moid::Alphabet;
use moid::SeededRng;

pub use prov_graph::identity::{BLADE_LEN, BLADE_RANDOM_LEN, Id, verify};

fn canonical_minter() -> moid::Minter {
    moid::Minter::new(Alphabet::noid_xdigit(), BLADE_RANDOM_LEN)
}

/// Random characters in a *minted* workspace name — twice a document blade's
/// [`BLADE_RANDOM_LEN`], for a different uniqueness problem.
///
/// A document ID is unique by *rejection*: the minter can see the registry, so a
/// collision is caught and re-rolled, and six characters (29⁶ ≈ 595M) is ample.
/// A workspace name has no such arbiter — nothing can see the other workspaces
/// in the world, which is exactly why `prov_config::is_valid_workspace_id`
/// refuses to promise uniqueness. So the only defense a minted name has is its
/// width: at 29¹² ≈ 3.5 × 10¹⁷, a million independently minted names collide
/// with probability ~10⁻⁶. That is what makes an unaudited mint honest to call
/// globally unique.
pub const WORKSPACE_NAME_RANDOM_LEN: usize = 12;

/// Total length of a minted workspace name: [`WORKSPACE_NAME_RANDOM_LEN`] plus
/// the check character every [`moid`] blade ends with.
pub const WORKSPACE_NAME_LEN: usize = WORKSPACE_NAME_RANDOM_LEN + 1;

/// Mint an opaque global name for a *workspace*, randomizing from `seed`.
///
/// The name a workspace calls itself is normally the user's to choose — it is
/// read by humans, in `id:<workspace>/<id>` references. This is the escape hatch
/// for when there is no good choice to make: a workspace that must be nameable
/// from anywhere, whose owner has no naming authority to lean on and would
/// rather not gamble that `notes` is theirs alone. So this is offered, never
/// applied: nothing in prov mints a workspace name on its own, because a name is
/// a *commitment* (every reference written elsewhere is spelled with it), and
/// prov does not make commitments on a user's behalf.
///
/// The result is a [`moid`] blade over the same NOID extended-digit alphabet as
/// a document ID, and so is always well-formed by
/// `prov_config::is_valid_workspace_id`: no vowels (nothing accidentally spells
/// a word), and no `/`, `:` or whitespace to break the qualifier position it
/// gets written in. It is deliberately *not* prefixed or otherwise marked as
/// minted — a reader of a reference has no business caring whether the name was
/// chosen or rolled.
pub fn mint_workspace_id(seed: u64) -> String {
    moid::Minter::new(Alphabet::noid_xdigit(), WORKSPACE_NAME_RANDOM_LEN)
        .mint_seeded(&mut SeededRng::new(seed))
}

/// Which events cause a document to be assigned (registered) an ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Registration {
    /// Register every document at creation time (eager).
    pub on_create: bool,
    /// Register when a document is first referenced by ID (e.g. a wikilink).
    pub on_link: bool,
    /// Register when a document is published.
    pub on_publish: bool,
}

impl Registration {
    /// Never register — identity is effectively off.
    pub const OFF: Self = Self {
        on_create: false,
        on_link: false,
        on_publish: false,
    };
    /// Register only on a durable reference (link-by-id or publish). Recommended.
    pub const LAZY: Self = Self {
        on_create: false,
        on_link: true,
        on_publish: true,
    };
    /// Register every document the moment it is created.
    pub const EAGER: Self = Self {
        on_create: true,
        on_link: true,
        on_publish: true,
    };

    /// Whether any trigger is active.
    pub fn is_active(&self) -> bool {
        self.on_create || self.on_link || self.on_publish
    }
}

/// The registration event a caller is asking about (for example, a
/// workspace's `register` operation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// A document was created.
    Create,
    /// Something is about to link to the document by ID.
    Link,
    /// The document is being published.
    Publish,
}

impl Registration {
    /// Whether this trigger set fires for `event`.
    pub fn fires_on(&self, event: Trigger) -> bool {
        match event {
            Trigger::Create => self.on_create,
            Trigger::Link => self.on_link,
            Trigger::Publish => self.on_publish,
        }
    }
}

/// A policy deciding when to register documents and how their IDs are minted.
pub trait IdentityPolicy {
    /// The registration trigger set for this policy.
    fn registration(&self) -> Registration;

    /// Mint a fresh ID for the document at `path`. Only called when a trigger
    /// fires, so a disabled policy need never produce a meaningful value.
    /// Uniqueness is the *caller's* job (mint-with-rejection against the
    /// index); a mint may repeat.
    fn mint(&mut self, path: &Path) -> Id;
}

/// Identity disabled — the default. Paths only; no ID is ever minted or written.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoIdentity;

impl IdentityPolicy for NoIdentity {
    fn registration(&self) -> Registration {
        Registration::OFF
    }

    fn mint(&mut self, _path: &Path) -> Id {
        // Unreachable in practice: `OFF` fires no triggers.
        Id(String::new())
    }
}

/// The bundled minting policy: NOID xdigit + check IDs from a seeded PRNG.
///
/// Minting is delegated to [`moid`]: a [`moid::Minter`] over the canonical
/// alphabet ([`canonical_minter`]) driven by a [`moid::SeededRng`]. The RNG is
/// xorshift64 — *not* cryptographic, and not claimed to be: these are opaque
/// internal handles whose uniqueness is enforced by rejection, not by entropy.
/// Both parts are `Clone`/`Debug`, which keeps this policy (and any workspace
/// carrying it) `Clone`/`Debug`, and a fixed seed makes tests deterministic. A
/// deployment wanting stronger opacity (or ARK permalinks, like diaryx)
/// implements [`IdentityPolicy`] itself.
#[derive(Debug, Clone)]
pub struct Minter {
    registration: Registration,
    minter: moid::Minter,
    rng: SeededRng,
}

impl Minter {
    /// Register only on a durable reference (the recommended default),
    /// randomizing from `seed`.
    pub fn lazy(seed: u64) -> Self {
        Self::with(Registration::LAZY, seed)
    }

    /// Register every document at creation, randomizing from `seed`.
    pub fn eager(seed: u64) -> Self {
        Self::with(Registration::EAGER, seed)
    }

    /// Register on a custom trigger set, randomizing from `seed`. A zero seed is
    /// nudged off xorshift64's fixed point by [`moid::SeededRng`].
    pub fn with(registration: Registration, seed: u64) -> Self {
        Self {
            registration,
            minter: canonical_minter(),
            rng: SeededRng::new(seed),
        }
    }
}

impl IdentityPolicy for Minter {
    fn registration(&self) -> Registration {
        self.registration
    }

    fn mint(&mut self, _path: &Path) -> Id {
        Id(self.minter.mint_seeded(&mut self.rng))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moid::Alphabet;

    #[test]
    fn no_identity_is_off() {
        assert!(!NoIdentity.registration().is_active());
    }

    #[test]
    fn lazy_registers_on_link_and_publish_only() {
        let r = Minter::lazy(1).registration();
        assert!(!r.fires_on(Trigger::Create));
        assert!(r.fires_on(Trigger::Link));
        assert!(r.fires_on(Trigger::Publish));
    }

    #[test]
    fn eager_registers_on_create() {
        assert!(Minter::eager(1).registration().fires_on(Trigger::Create));
    }

    #[test]
    fn mints_verified_distinct_opaque_ids() {
        let mut p = Minter::eager(42);
        let a = p.mint(Path::new("a.md"));
        let b = p.mint(Path::new("b.md"));
        assert_ne!(a, b);
        for id in [&a, &b] {
            assert_eq!(id.as_str().len(), BLADE_LEN);
            assert!(verify(id.as_str()), "{id}");
        }
    }

    #[test]
    fn same_seed_is_deterministic() {
        let a = Minter::lazy(7).mint(Path::new("x"));
        let b = Minter::lazy(7).mint(Path::new("y"));
        assert_eq!(a, b, "path does not participate in the mint");
    }

    #[test]
    fn mints_wide_opaque_workspace_names() {
        let a = mint_workspace_id(42);
        let b = mint_workspace_id(43);
        assert_ne!(a, b);
        for name in [&a, &b] {
            assert_eq!(name.chars().count(), WORKSPACE_NAME_LEN);
            // Every constraint the qualifier position imposes, checked here
            // rather than through `prov-config` (which this crate cannot see):
            // non-empty, and none of the three characters that would break
            // `id:<workspace>/<id>` apart.
            assert!(!name.is_empty());
            assert!(
                !name
                    .chars()
                    .any(|c| c == '/' || c == ':' || c.is_whitespace()),
                "{name} cannot be written as a reference qualifier"
            );
        }
    }

    /// A minted workspace name is *wider* than a document ID, and that width is
    /// the entire uniqueness argument — nothing rejects a colliding one, because
    /// nothing can see the other workspaces it might collide with. Asserted at
    /// compile time, since narrowing the constant is the way this would be lost.
    const _: () = assert!(WORKSPACE_NAME_LEN > BLADE_LEN);

    #[test]
    fn a_workspace_name_is_wider_than_a_document_id() {
        assert!(
            mint_workspace_id(1).chars().count() > Minter::lazy(1).mint(Path::new("x")).0.len()
        );
    }

    #[test]
    fn verify_rejects_typos() {
        let id = Minter::lazy(3).mint(Path::new("x")).0;
        assert!(verify(&id));
        // Flip one body character to another alphabet character.
        let mut chars: Vec<char> = id.chars().collect();
        chars[0] = if chars[0] == 'b' { 'c' } else { 'b' };
        let typo: String = chars.iter().collect();
        assert!(!verify(&typo), "{typo}");
        // Wrong length, wrong alphabet (vowels and `y` are both out).
        assert!(!verify("bcd"));
        assert!(!verify("aeiouAy"));
        assert!(!verify("bcdfghy"));
    }

    #[test]
    fn check_char_matches_the_noid_lineage() {
        // Independently computed: the xdigit alphabet leads with the digits, so
        // ordinals b=10,c=11,d=12,f=13,g=14,h=15 weighted by position 1..=6 →
        // 10+22+36+52+70+90 = 280; 280 % 29 = 19 → the 19th xdigit symbol is
        // 'n'. moid computes the same check character, so a full ID with that
        // body validates.
        assert_eq!(Alphabet::noid_xdigit().check_char("bcdfgh"), 'n');
        assert!(verify("bcdfghn"));
    }

    #[test]
    fn an_id_may_be_all_digits() {
        // The point of the xdigit alphabet: digits are in it, so an ID can look
        // like a number — which is why every stamp writes a string scalar.
        let check = Alphabet::noid_xdigit().check_char("012345");
        assert!(verify(&format!("012345{check}")));
    }

    /// The check character's whole reason to exist, stated as a law.
    ///
    /// `verify_rejects_typos` above flips one character of one ID and confirms
    /// the result is refused. That is a witness, and the claim a check character
    /// actually makes is universal: **no single-character substitution of a
    /// valid ID is ever itself valid.** A check digit that caught most typos and
    /// missed some would still pass every example anyone thought to write, and
    /// would silently let a mistyped `id:` reference resolve to nothing while
    /// looking well-formed — the failure `MalformedId` exists to prevent.
    mod properties {
        use super::*;
        use proptest::prelude::*;

        /// The NOID extended-digit alphabet: the ten digits plus the nineteen
        /// consonants that cannot combine into a word (no vowels, no `y`, no
        /// `l`). Twenty-nine symbols, which is where the crate's own "29^6 ≈
        /// 595M" comes from. Written out here so a substitution can be drawn
        /// from it; `every_minted_character_is_in_the_alphabet` keeps the
        /// transcription honest.
        const XDIGIT: &str = "0123456789bcdfghjkmnpqrstvwxz";

        fn minted() -> impl Strategy<Value = String> {
            any::<u64>().prop_map(|seed| Minter::lazy(seed).mint(Path::new("x")).0)
        }

        proptest! {
            #[test]
            fn every_minted_id_verifies_and_is_the_declared_length(id in minted()) {
                prop_assert_eq!(id.chars().count(), BLADE_LEN);
                prop_assert!(verify(&id), "{id}");
            }

            #[test]
            fn every_minted_character_is_in_the_alphabet(id in minted()) {
                for c in id.chars() {
                    prop_assert!(XDIGIT.contains(c), "`{c}` of `{id}` is not an xdigit");
                }
            }

            /// The law. Substitute any one character of a valid ID — body or
            /// check character — for any *other* alphabet character, and the
            /// result must be refused. Every position, every replacement.
            #[test]
            fn no_single_character_slip_survives_verification(
                id in minted(),
                position in 0..BLADE_LEN,
                replacement in 0..XDIGIT.chars().count(),
            ) {
                let alphabet: Vec<char> = XDIGIT.chars().collect();
                let mut chars: Vec<char> = id.chars().collect();
                let replacement = alphabet[replacement];
                prop_assume!(chars[position] != replacement);
                chars[position] = replacement;
                let typo: String = chars.into_iter().collect();
                prop_assert!(
                    !verify(&typo),
                    "`{typo}` is one character from `{id}` and still verified"
                );
            }

            /// A transposition of two *adjacent, different* characters is the
            /// other slip a check character is chosen to catch — the one a
            /// simple sum cannot see, since addition does not care about order.
            #[test]
            fn no_adjacent_transposition_survives_verification(
                id in minted(),
                position in 0..BLADE_LEN - 1,
            ) {
                let mut chars: Vec<char> = id.chars().collect();
                prop_assume!(chars[position] != chars[position + 1]);
                chars.swap(position, position + 1);
                let swapped: String = chars.into_iter().collect();
                prop_assert!(
                    !verify(&swapped),
                    "`{swapped}` transposes two characters of `{id}` and still verified"
                );
            }
        }
    }
}
