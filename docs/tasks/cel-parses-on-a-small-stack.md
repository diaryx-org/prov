---
title: cel parses on a small stack
part_of: '[Tasks](/docs/tasks/tasks.md)'
created: 2026-10-14T16:47:13.324124Z
status: open
author: adammharris
description: cel's ANTLR parser spends ~165 KB of stack per nesting level in a debug build, so prov runs every CEL walk on a grown stack; the fix belongs upstream, and the workaround goes when it lands
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

The fix is the parser's: frames this large come from the generated
`CELParser`'s context objects and `antlr4rust`'s recursion, and the
interpreter could resolve an operand chain without a frame per operand. Worth
an issue on `cel-rust` with the table above. When a release fixes it,
re-measure and shrink or remove `stack.rs`.
