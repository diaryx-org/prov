# prov, in Bend

The structural half of `prov check`, written in [Bend](https://bend-lang.com)
as an experiment: can prov's read core — the census of every link a document
declares, the spanning walk that orders it, the findings it raises — run as
proven code, in parallel, and still say exactly what `prov` says?

Everything here is held to `prov check` itself. `check.py` writes a corpus of
workspaces, runs both, and requires the same finding lines in the same order.

```console
cd spike/bend
python3 check.py              # the laws, the build, and the port held to prov
bend PROOF.bend               # the laws alone: prints "All terms check."
build/prov-bend <dir> <root>  # `prov check`, from the root document <root> of <dir>
build/prov-bend <dir> <root> --threads 1
```

Building needs Bend 2.0.27 (`curl -fsSL https://bend-lang.com/install.sh | sh`),
clang (Bend's C needs `musttail`, which gcc refuses), a Rust toolchain, and
Zig 0.16 for prov's own parsers, as the rest of the repository does.

## What is here

| file | what it is |
|---|---|
| `text.bend`, `path.bend` | strings and workspace paths: fs-transaction's `normalize` (a leading `..` stays), `link::resolve`, `split_locator`, and `PathBuf`'s component-wise order |
| `link.bend` | `Link::parse` — `[[target\|label]]`, `[label](target)` with `<…>` and balanced parentheses, a bare target — and what a target names: off the workspace, the same document, an id here or in another workspace, an alias, or a path |
| `id.bend` | `identity::verify`: the NOID extended-digit alphabet and moid's check character |
| `canon.bend` | the reader for what the shim says a document says |
| `census.bend` | the per-document census — every relation field in the vocabulary's order, then every body link — resolved against directory listings, as two parallel fork-join trees |
| `titles.bend` | the title index an alias resolves through: `title_scope`'s walk, `out_of_scope` parking, the full-tree fallback, `ShadowProbe`'s attachment sidecars, stems and titles |
| `walk.bend` | the spanning walk replayed in prov's order — a stack, second parents and cycles, the inverse check — and the findings, reachability and orphans |
| `main.bend` | the rounds that read the workspace, and the command |
| `ws.bend`, `ffi/` | two effects, their C adapter, and the Rust static library behind them — `RUNTIME.md` |
| `LAWS.bend`, `PROOF.bend` | sixteen claims about the code above, each proven |
| `check.py` | the gate |

About 3,300 lines of Bend, and 365 of Rust in the shim. The Rust files this
covers — `validate.rs`, `census.rs`, `scan.rs` and `link.rs` — are about
8,000, much of it the kinds this port leaves out.

## What the corpus says

The port models six kinds of finding: `unreadable`, `duplicate_containment`,
`missing_inverse`, `broken_link`, `case_mismatch` and `orphan`. Each corpus
workspace is checked by `prov check --json` and by the port; the modelled
findings must be the same lines in the same order. Other kinds prov reports
(`about_stale`, `malformed_id`, `dangling_id`, `ambiguous_alias`, …) are
named in the report and not compared.

| workspace | exercises | findings |
|---|---|---|
| `clean` | relative, absolute and body links, a nested tree | 0 |
| `broken` | missing children, relation and body links, a missing image, an `about` pointer at an unwritten page (not a finding), locators | 6 |
| `case` | a case-only match on a spanning link (descended into as written, so unreadable on a case-sensitive disk) and on overlay and body links | 4 |
| `containment` | a second parent, a repeated child, a cycle through the root | 4 |
| `inverse` | `part_of` elsewhere, absent, in a document without metadata, as a list, with a locator | 4 |
| `unreadable` | malformed YAML, a path above the root, a file that is not UTF-8 | 3 |
| `orphans` | Markdown, Djot and HTML beside reached documents; hidden files, non-documents and unreached directories left alone; `sub-z.md` after `sub/` | 4 |
| `ids` | registry descent, a qualified id naming the workspace itself, a foreign id, a dangling one, malformed ones, one registered to a missing file | 2 |
| `aliases` | `[[Title]]`, `[[stem]]`, an ambiguous title, an unknown one falling through to a path, a spanning alias forcing the full-tree scan, an alias in `part_of` | 2 |
| `attachments` | a whole-file sidecar that shadows its payload in the title index, a separated body under another case | 1 |
| `paths` | spaces, `<…>`, nested parentheses, `.` and `..`, trailing words after a link, an unterminated one | 3 |
| `scoped` | `out_of_scope` keeping a title out of the index | 1 |
| `deep` | the order findings arrive in, three levels down | 6 |
| prov itself | this repository | 0 |
| synthetic | 2,449 documents, broken links, strays and wrong parents sprinkled through | 561 |

All fifteen agree.

## What the laws say

`bend PROOF.bend` checks sixteen claims in about a third of a second. They
are about the defs `main.bend` runs, not about a model of them:

- **`census_total`** — resolving a document's links against the disk keeps
  every one: the census has an entry for each link declared.
- **`findings_exact`** — the census reports exactly what its entries owe: a
  broken link that is not an `about` pointer and a case mismatch, each once,
  and nothing else.
- **`plan_par_total`** and **`fix_par_total`** — the two fork-join trees plan
  and resolve every document they are handed exactly once, at any depth and
  whatever count they are told. The lanes neither drop a document nor process
  one twice, so the walk is replayed over all of them and no other.

The rest are the lemmas those need: list lengths under `append`, splitting a
list into owned halves, and `Nat.add`. What is *not* proven is as plain:
the walk's fuel suffices (it is checked at run time, and an exhausted walk
is an error, never a truncated answer), the path and link grammar agree with
prov's (the corpus is the evidence), and nothing about the shim, the C
adapter, the Rust parsers or Bend's runtime — Bend says so itself, naming
the 21 defs that reach foreign code.

## Performance

`check` over this repository takes about a tenth of a second in both.
Over the synthetic workspace, 2,449 documents:

| | `prov` | port, 1 thread | port, 4 threads |
|---|---|---|---|
| no alias links | 0.20 s | 0.76 s | 0.66 s |
| with alias links (a title index) | 0.32 s | 1.05 s | 0.98 s |

The port started at 2.2 s. Two things took it to here, and neither was the
parallel census:

- **Each directory is indexed once.** Resolving a link asked its directory's
  listing a linear question, lowercasing every name each time — prov's
  `DirNames` builds a set and a folded map once, and so does `Listing` now.
  1.7 s to 0.66 s.
- **The host answers a batch, as a list.** Every effect is a round trip
  through Bend's event loop to a helper thread; the rounds ask for all of a
  round's documents in one call, and the C adapter hands them back as one
  string per document, so each is read on the lane that plans it rather
  than all of them walked on the event loop's.

Four threads buy about a tenth. What remains is sequential by nature or by
this port's choice: the walk (a stack, ordered), the maps and sets the rounds
and the title index build, and, beneath all of it, strings that are lists of
characters — every character a heap cell, at about a hundred nanoseconds
each, as historica's spike measured too. The census's per-document work, the
part a fork-join tree can take, is now a small share of the whole.

## What the port found

**`check`'s answer depends on files outside the workspace.** A `contents:
../outside.md` is a broken link when nothing is at `outside.md` beside the
workspace, and an unreadable document when something is: `exact_name` lists
the parent of the root before `load` refuses the path. The port reproduces
it byte for byte, which is how it came up. It is filed as
[`docs/tasks/check-reads-above-the-root.md`](../../docs/tasks/check-reads-above-the-root.md).

Porting also made some of prov's behaviour explicit that its docs leave to
the code:

- A case-only match on a spanning link is descended into *as written*, so on
  a case-sensitive disk the child is reported unreadable as well as
  mismatched.
- The title index's scope is a second walk, and not the census's: it follows
  spanning links that resolve *lexically*, reads the directory of every path
  and id target (body images aside), and gives up and scans the whole tree
  the moment a spanning link is itself an alias.
- An image's alias-shaped target is a path, never a title.
- An `about` pointer at a page that is not there is not a broken link; the
  page is derived.

## What is not here

- **The other kinds.** Ids reconciled against frontmatter, `dangling_id` and
  `malformed_id` (the resolution is ported; the findings are not printed),
  `ambiguous_alias`, stale labels, islands (`missing_containment`), fixity,
  manifests, configuration, stores, vocabulary, dates, confirmations,
  `about_stale`. `check.py` compares the modelled kinds and names the rest;
  where prov reports an island, orphans are not compared, since islands
  subtract from them.
- **A vocabulary of the workspace's own.** `relations`, `spanning`,
  path-valued fields (`type: ref`), configuration in the root's `prov:`
  block, and history or recycle-bin stores are refused (exit 3) rather than
  answered differently.
- **Discovery.** The root document is given on the command line.
- **Writing.** No repair, no `mv`. The rename planner — the pure half of `mv`,
  with laws that every inbound reference is retargeted and no other changes
  — is the next piece this spike would take.
- **A library.** Bend compiles to a program, not a library Rust can call
  (`RUNTIME.md`), so this cannot stand in for the crate plates and diaryx
  link against. What it can do is what historica's spike did: find bugs in
  the Rust by being held to it.

## What Bend asked for

- **No mutual recursion, and only defs above.** A helper that classifies and
  then recurses is mutual recursion; so the recursion computes its answer for
  the tail first and a non-recursive helper combines it with the head
  (`Census.named` before it was an index, `Path.cmp.parts`). Base declares a
  law and fills it below to get round this; user code may not.
- **A `match` only on a parameter or a pattern-bound variable, in binder
  order.** A destructuring `let` of a computed value is refused too;
  `Pair.fst` and `Pair.snd` read one half.
- **Eager branches.** Both arms of a choice are computed, so a search that
  recursed through `pick` walks the whole string; the fixes carry the
  decision in, or keep the recursion to one call per element.
- **Quantities are types.** `+` wherever a value is read twice, including in
  proofs; a pair is affine, so `Maybe` of a pair is not `Data` and a
  constructor of its own replaces it; `List.map` takes affine lists and
  `String.join` reusable ones, so the two do not compose.
- **Constructor names are global.** `Done` is Base's; the census's is
  `Fixed`.
- **A list every lane can see is a list every lane counts.** Handing both
  halves of a fork the same `+` list keeps them from scaling; the census
  splits its jobs into owned halves, and the split and the fork must be one
  def.
- **`cc` is not clang.** The emitted C uses `musttail`, which gcc refuses.
