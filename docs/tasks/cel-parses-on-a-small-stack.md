---
title: cel parses on a small stack
part_of: '[Tasks](/docs/tasks/tasks.md)'
created: 2026-10-14T16:47:13.324124Z
status: open
author: adammharris
description: "cel's ANTLR parser spends ~165 KB of stack per nesting level in a debug build, so prov runs every CEL walk on a grown stack; upstream's Pratt parser fits in 279 KB at the limit and is the way out once it is selectable"
---

# cel parses on a small stack

**Done when** `cel` parses and evaluates an expression at its own nesting
limit within a few hundred KB of stack in a debug build, and
`prov-views/src/stack.rs` is gone or sized by a measurement against that
release.

## The bug

The Diaryx Mac app crashed (SIGBUS in the stack guard page) following a link
into another library. `locatePeer` ran on a Swift cooperative-pool thread,
512 KB of stack, and reached the CEL parser through discovery:
`discover` → `build` → `WorkspaceConfig::apply` → `ViewSpec::parse` →
`Expression::parse` → `cel::Program::compile` → the ANTLR-generated
`CELParser`, recursing `expr` → `conditionalOr` → … → `primary` →
`exprList` → `expr`. Three levels of nesting were enough.

What `cel` 0.14.5 needs, measured by halving a thread's stack until it
overflows (0.15.0 has the same parser and the same limit):

| | debug | release |
|---|---|---|
| parse `x` | 223 KB | 31 KB |
| parse, per level of nesting | ~165 KB | ~7 KB |
| parse at the nesting limit (96) | ~16 MB | ~0.7 MB |
| parse, per operand of `x + x + …` | ~1.3 KB | — |
| evaluate, per level of the tree | ~40 KB | — |

So even a release build overflows 512 KB at about 64 levels, inside the
parser's own limit. An operand chain is not counted against that limit at
all, and the interpreter recurses once per operand.

## What prov does meanwhile

`prov-views/src/stack.rs` runs every deep walk over CEL — the parse, an
evaluation, prov's own walks over the tree, and the drop of it — through
`stacker::maybe_grow`, sized from the measurements above: a fixed 32 MB plus
2 KB per byte of source for a parse, 256 KB plus 64 KB per level of the
tree's depth for everything else. A thread with that much left runs in
place; one without moves onto a fresh stack. Not on wasm, which has one
stack.

The tests that hold it are
`discovery_survives_a_nested_view_on_a_small_stack` (prov) and
`an_expression_at_cels_nesting_limit_needs_no_stack_of_the_callers` and
`a_long_operand_chain_needs_no_stack_of_the_callers` (prov-views), each on
a 256–512 KB thread in a debug build.

## Upstream

Checked 2026-10-14, against cel-rust `main` and 0.15.0.

- **The stack cost is known and accepted for ANTLR.** #212 (closed) is this
  bug on Windows; its fix, #213 (0.11.6), is the `max_recursion_depth` of 96
  prov already gets, which turns a deep parse into an error but does not
  make a shallow one cheap. #273 found no low-hanging fruit: about 21 frames
  per nesting level, "all due to how deep the stack gets due to how ANTLR
  does this".
- **The way out is a Pratt parser ported from cel-go** (#320, #326, parity
  fixes in #333), about 20–40× faster than ANTLR by its benchmarks. Since
  0.14.5 it is `cel::parser::PrattParser`, behind the non-default
  `parser_pratt` feature and `#[doc(hidden)]`; `Program::compile` still uses
  ANTLR. The maintainer's remaining steps (on #320): a default
  `parser_antlr` feature that, when turned off, lets `parser_pratt` replace
  it, then a release. Neither is in 0.15.0, and CI's conformance job does
  not turn `parser_pratt` on.
- **Pratt fits a small stack.** Measured as above, 0.14.5:

  | | ANTLR debug | Pratt debug | ANTLR release | Pratt release |
  |---|---|---|---|---|
  | parse `x` | 215 KB | 23 KB | 23 KB | 23 KB |
  | nesting 16 | 2.9 MB | 39 KB | 135 KB | 23 KB |
  | nesting 95 | 16 MB | 279 KB | 727 KB | 151 KB |
  | `x + …` × 2000 | 2.6 MB | 423 KB | 775 KB | 119 KB |

  With `Expression::parse` calling `PrattParser` instead of
  `Program::compile`, prov's whole test suite passes unchanged (tried and
  reverted; prov keeps only the `IdedExpr`, so no `Program` is needed).
- **Evaluation is untouched by any of it.** The interpreter still recurses
  per level of the tree (~40 KB in debug); #355 (plan evaluation) is about
  speed, not depth. That half of `stack.rs` stays until it changes.

So the parse half of this task is done when Pratt is cel's default parser,
or selectable without a hidden API: switch to it, re-measure, and drop the
parse size in `stack.rs` to what Pratt needs. Taking `PrattParser` now
would mean leaning on a `#[doc(hidden)]` API that semver does not cover.
