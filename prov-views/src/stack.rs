//! Room on the stack for CEL, whichever thread asks.
//!
//! The `cel` crate parses with an ANTLR-generated recursive-descent parser and
//! evaluates by recursing over the tree, and both spend far more stack per
//! level than a host would guess. Measured in a debug build: about 220 KB to
//! parse `x` at all, then about 165 KB for every level of nesting — cel's own
//! limit is 96 levels, so 16 MB at the limit — and about 40 KB per level of
//! the tree to evaluate, with no limit, since `a + b + c …` nests a level per
//! operand without counting against the parser's. A release build spends
//! about a twentieth of that, which still overflows a 512 KB thread at around
//! 64 levels.
//!
//! A host cannot know which prov calls reach the parser — discovery does,
//! through the views a config declares — and a Swift cooperative-pool thread
//! has 512 KB. So every deep walk over CEL in this crate runs through
//! [`with_stack`], which carries on in place when the thread has room and
//! moves onto a fresh stack of the size asked for when it does not. The
//! proper fix is a parser that does not recurse this hard, and that is the
//! `cel` crate's (see `docs/tasks/cel-parses-on-a-small-stack.md`).

/// What a parse may need beyond its length: cel's nesting limit at the debug
/// cost above, with room to spare.
pub(crate) const PARSE: usize = 32 * 1024 * 1024;

/// What a parse may need per byte of source, on top of [`PARSE`]: an operand
/// chain is not bounded by cel's limit, costs the parser about 1.3 KB a term
/// in debug, and prov's own walks over the tree it builds run inside the same
/// parse. A term is at least two bytes (`x+`).
pub(crate) const PARSE_PER_BYTE: usize = 2 * 1024;

/// What any walk over a parsed tree needs before its first level.
pub(crate) const BASE: usize = 256 * 1024;

/// What any walk over a parsed tree may need per level of its depth: the
/// interpreter's frames, the largest, at the debug cost above with margin.
pub(crate) const PER_LEVEL: usize = 64 * 1024;

/// What a walk over a tree `depth` levels deep may need.
pub(crate) fn for_depth(depth: usize) -> usize {
    BASE.saturating_add(depth.saturating_mul(PER_LEVEL))
}

/// Run `f` with at least `needed` bytes of stack: here when the thread has
/// that much left, else on a fresh stack of that size.
///
/// On wasm there is one stack, sized when the module is linked, and nothing to
/// switch to; `f` runs where it is.
pub(crate) fn with_stack<R>(needed: usize, f: impl FnOnce() -> R) -> R {
    #[cfg(not(target_family = "wasm"))]
    {
        stacker::maybe_grow(needed, needed, f)
    }
    #[cfg(target_family = "wasm")]
    {
        let _ = needed;
        f()
    }
}
