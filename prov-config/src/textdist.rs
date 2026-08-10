//! Small string edit-distance helpers, shared by the config linter
//! ([`crate::config::diagnose`]) and the vocabulary term-consistency pass
//! ([`crate::validate`]). Both need the same "is this a likely typo of a known
//! spelling?" judgment — a misspelled config key, a drifted tag — so the metric
//! lives in one place rather than being copied per call site.

/// The candidate in `candidates` that most resembles `key`, when one is within a
/// small edit distance (a likely typo) — else `None`. Distance is measured
/// case-sensitively so a case-only slip surfaces its canonical spelling. The
/// threshold (2) is deliberately tight: recognized spellings are distinctive
/// enough that structural fields (`title`, `part_of`, `id`) and ordinary user
/// values fall outside it, so they are never mistaken for typos.
pub(crate) fn nearest(key: &str, candidates: &[&str]) -> Option<String> {
    candidates
        .iter()
        .map(|cand| (levenshtein(key, cand), *cand))
        .filter(|(d, _)| (1..=2).contains(d))
        .min_by_key(|(d, _)| *d)
        .map(|(_, cand)| cand.to_string())
}

/// The candidate string (owned) in `candidates` nearest to `key` within the
/// typo threshold — the `String`-slice form of [`nearest`], for callers whose
/// candidate set is built at runtime (vocabulary term names) rather than a
/// static `&[&str]` (config axis names).
pub(crate) fn nearest_owned(key: &str, candidates: &[String]) -> Option<String> {
    candidates
        .iter()
        .map(|cand| (levenshtein(key, cand), cand))
        .filter(|(d, _)| (1..=2).contains(d))
        .min_by_key(|(d, _)| *d)
        .map(|(_, cand)| cand.clone())
}

/// Levenshtein edit distance — the classic two-row dynamic program.
pub(crate) fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == *cb { 0 } else { 1 };
            curr[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(curr[j] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_finds_a_close_typo_and_ignores_distant_words() {
        assert_eq!(
            nearest("recyle_bin", &["recycle_bin", "fixity"]),
            Some("recycle_bin".to_string())
        );
        // A word within threshold but not identical is a hit; a far word is not.
        assert_eq!(nearest("author", &["recycle_bin", "fixity"]), None);
        // An exact match is distance 0 — deliberately not a "typo".
        assert_eq!(nearest("fixity", &["fixity"]), None);
    }

    #[test]
    fn nearest_owned_matches_the_slice_form() {
        let cands = vec!["public".to_string(), "friends".to_string()];
        assert_eq!(
            nearest_owned("freinds", &cands),
            Some("friends".to_string())
        );
        assert_eq!(nearest_owned("colleagues", &cands), None);
    }

    /// [`levenshtein`] claims to be a *distance*, and the callers lean on that
    /// harder than the examples above show: `nearest` takes a `min_by_key` over
    /// it and a threshold of 1..=2 — reasoning that only holds if the number is
    /// a metric rather than merely a plausible-looking score. So the metric
    /// axioms are worth asserting directly. A hand-rolled two-row dynamic
    /// program is exactly the kind of code where an index slip yields something
    /// that is right on the examples someone tried and asymmetric elsewhere.
    mod properties {
        use super::*;
        use proptest::prelude::*;

        /// Short ASCII words. ASCII keeps `chars()` and byte length in step, so
        /// a failing case reads the way it looks; short keeps the DP small and
        /// the shrunk output legible.
        fn word() -> impl Strategy<Value = String> {
            "[a-z_]{0,6}"
        }

        proptest! {
            /// Identity of indiscernibles, in the direction that matters here:
            /// a string is distance 0 from itself, and nothing else is 0 from it.
            #[test]
            fn only_a_string_itself_is_distance_zero(a in word(), b in word()) {
                prop_assert_eq!(levenshtein(&a, &a), 0);
                prop_assert_eq!(levenshtein(&a, &b) == 0, a == b);
            }

            /// Symmetry. `nearest` compares `key` against candidates in one
            /// fixed order; if the metric were asymmetric, the suggestion a user
            /// gets would depend on which side of the call their typo landed.
            #[test]
            fn distance_does_not_depend_on_the_order_of_its_arguments(
                a in word(),
                b in word(),
            ) {
                prop_assert_eq!(levenshtein(&a, &b), levenshtein(&b, &a));
            }

            /// The triangle inequality — the axiom that makes "within 2 edits"
            /// mean something transitive rather than an isolated score.
            #[test]
            fn distance_never_exceeds_going_the_long_way(
                a in word(),
                b in word(),
                c in word(),
            ) {
                prop_assert!(
                    levenshtein(&a, &c) <= levenshtein(&a, &b) + levenshtein(&b, &c),
                    "d({a},{c}) > d({a},{b}) + d({b},{c})"
                );
            }

            /// Bounds: no more edits than the longer string has characters, and
            /// no fewer than the difference in their lengths (every surplus
            /// character costs at least one edit to account for).
            #[test]
            fn distance_is_bounded_by_the_lengths(a in word(), b in word()) {
                let (la, lb) = (a.chars().count(), b.chars().count());
                let d = levenshtein(&a, &b);
                prop_assert!(d <= la.max(lb), "d={d} exceeds the longer string");
                prop_assert!(d >= la.abs_diff(lb), "d={d} is below the length gap");
            }

            /// The threshold `nearest` is built on, stated as a law: an exact
            /// match is never offered as a typo (distance 0 is excluded), and a
            /// candidate is offered only when it really is within two edits.
            #[test]
            fn nearest_offers_only_genuine_near_misses(
                key in word(),
                candidates in prop::collection::vec(word(), 1..4),
            ) {
                let Some(hit) = nearest_owned(&key, &candidates) else { return Ok(()) };
                let d = levenshtein(&key, &hit);
                prop_assert!((1..=2).contains(&d), "offered `{hit}` at distance {d}");
                // And it is the nearest such candidate, not merely a near one.
                let best = candidates
                    .iter()
                    .map(|c| levenshtein(&key, c))
                    .filter(|d| (1..=2).contains(d))
                    .min();
                prop_assert_eq!(Some(d), best);
            }
        }
    }
}
