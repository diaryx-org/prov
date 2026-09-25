# The runtime boundary

What the Bend side asks of the host, why the boundary sits where it does,
and how the program is built. historica's spike (`historica/spike/bend`,
branch `verus-spike`) found this shape first; this follows it.

## Bend hosts, Rust serves

Checked against Bend **2.0.27**, `bend guide` and `bend guide effects`.

A compiled Bend program is a program, not a library: there is no target that
exports pure definitions to a Rust caller (Bend's issue #813 asks for one;
the maintainer's answer is planned, not scheduled). JavaScript can call pure
definitions through Bend's loader, and nothing else can. So the host
relationship is the other way round: `main.bend` runs, its effects are C
functions spliced into the generated program, and those call a Rust static
library, `ffi/`, which owns every buffer and every read.

## Where the boundary is, and why there

An effect answers a question and decides nothing. There are two:

- **`Ws.dirs`** — what directories hold: each entry's name and kind, a
  symbolic link reported and never followed, as prov's `StdFs` reports it.
- **`Ws.loads`** — what documents say: each document's metadata as a flat,
  pre-order value tree (`V` records: depth, key, tag, text), and the links
  prov's body scanner finds in its prose (`L` records: span, image, label,
  target); or, when it cannot be read, the words prov's `Unreadable` finding
  would carry.

The parse is the boundary because prov's parsers are fig and twig — YAML,
JSON, TOML and fig for metadata, Markdown, Djot and HTML for prose — written
in Zig, and a pure Bend definition cannot call a host: a foreign call is an
effect, and an effect's answer is never evidence for a law. Porting the
parsers is the alternative, and it would be most of the work and most of the
risk. So the shim calls `prov_graph::document::Document::parse` and
`prov_graph::link::scan_body_links` — the same code `prov` runs — and
everything after that is decided in Bend: which directories are listed and
which documents read (and so which are not); which fields are links, in what
order; how a target parses and what it names; whether an id verifies; how
a path resolves and whether its name is on disk; the title index; the walk;
every finding and its words.

One decision prov makes before reading is made here before asking: a path
that climbs above the root is refused as `Graph::load` refuses it, and the
host is never asked for it. (The host *is* asked to list a directory above
the root when a link names one, because prov's `exact_name` does that too —
see `docs/tasks/check-reads-above-the-root.md`.)

## Batches, and a list back

Every effect is a round trip: the event loop parks the effect, a helper
thread runs the Rust, and the event loop rebuilds the answer. At a few
hundred microseconds each, thousands of them one at a time were most of
what a check cost. So each effect takes a batch — the root and a path a
line — and the port asks once per round of the walk.

The answer comes back as pieces, a head and a body for each question,
separated by the byte `0x1e`, which the shim escapes inside any field. The C
adapter splits there and hands Bend a `List<String>` rather than one string,
so each document's records are a string of their own and are read on the
lane that plans that document, not walked on the event loop's thread.

## The adapter

`ffi/ws_dirs.c` holds the adapter both effects share — copy the argument out
of Bend's heap with `io_cstr`, run the Rust function on a helper thread with
`io_work`, rebuild the answer on the event loop — and registers `Ws.dirs`;
`ffi/ws_loads.c` declares it and registers `Ws.loads`. `ffi/src/lib.rs` is
the ABI: byte strings as `(pointer, length)`, `0` and a buffer or an errno
and a message, every buffer returned through `pb_free`, no panic across the
boundary.

The JS twins refuse, in words: the parsers are native, and a JS build that
answered differently from the native one would be worse than one that says
it cannot.

The effect symbols and the term representation are Bend's runtime
internals, not a stable ABI. The adapter is rebuilt with every Bend upgrade.

## Building

`bend main.bend -o x` compiles the C itself with nothing of ours linked, so
the program is emitted and linked by hand:

```console
cargo build --release --manifest-path ffi/Cargo.toml
bend main.bend -o build/prov-bend.c          # about 1 MB of C, five seconds
clang -O3 -w -o build/prov-bend build/prov-bend.c \
  ffi/target/release/libprov_bend_ffi.a -lm -lpthread -ldl
```

`check.py` does this. The compiler must be clang: the C uses `musttail`,
which gcc refuses.

## What the proofs cover

`PROOF.bend` checks source terms and termination. It does not verify the
compiler, the runtime, the C adapter, the Rust shim, prov's parsers, or the
operating system; `check.py`'s corpus is the evidence for those, and Bend
names every def that reaches foreign code when it checks `main.bend`.
