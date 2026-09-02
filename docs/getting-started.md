---
title: Getting Started with prov
part_of: '[prov](/README.md)'
---

# Getting Started with prov

A beginner's guide to the `prov` command line. By the end you'll have a small
workspace, understand how its structure is stored, and know every command you
need for day-to-day use.

> **What prov is, in one sentence.** A *self-describing plaintext
> workspace*: a set of documents whose structure lives in the documents' own
> frontmatter, not in the folder layout or an app-private sidecar. Follow the
> links from a root document and the whole workspace unfolds. See
> [DESIGN.md](DESIGN.md) for the reasoning behind that idea.

> **The transcripts below are executable.** Every command block marked as part of
> the walkthrough is run in CI, in order, against a real workspace
> (`ci/check-getting-started.sh`). If the CLI changes so a command in this guide
> stops working, the build fails — so what you read here is what the current
> binary does. (Random IDs and absolute paths in the output will differ on your
> machine.)

---

## 1. The mental model

Three ideas carry everything else.

- **Documents** are plaintext files (`.md`, usually) with an embedded metadata
  block — YAML frontmatter between `---` fences:

  ```markdown
  ---
  title: Rust
  part_of: '[My Vault](/index.md)'
  ---

  # Rust

  Body prose goes here.
  ```

- **Relations** are the named links in that metadata. prov ships with the
  *diaryx* vocabulary:

  | Relation   | Direction        | Meaning                                    |
  | ---------- | ---------------- | ------------------------------------------ |
  | `contents` | parent → child   | "this document contains these"             |
  | `part_of`  | child → parent   | the inverse — "this belongs to that"       |
  | `links`    | any → any        | a loose cross-reference (an *overlay* link) |
  | `registry` | root → registry  | where stable IDs are recorded              |
  | `config`   | root → config    | where workspace settings live              |

- **The spanning tree.** Exactly one relation is the *spanning* relation —
  `contents`/`part_of` here. It is single-parent, and it is the workspace's
  discovery spine: every document has one path back to one **root**. Every other
  relation (like `links`) may be many-to-many, laid over the tree as a graph.

The root is just a document that nothing contains — it has no `part_of`.
prov finds it by walking up from your current directory until it sees a
document with metadata and no `part_of` (an `index.md` or `README.md` wins
ties).

---

## 2. Install

prov builds from source:

- **Rust** (`cargo`, 1.85 or newer) — to build prov itself.

If you want to set custom (non-default) features for the build, you will also need a Zig 0.16.0 toolchain.

```sh
$ git clone https://github.com/diaryx-org/prov
$ cd prov
$ cargo build --release
```

The binary lands at `target/release/prov`. Put it on your `PATH`, or invoke
it by full path. Every example below uses the command name `prov`.

---

## 3. Create a workspace

`init` sets up a workspace: a self-describing root document plus a config
document that records your preferences. On a terminal it walks you through a
series of choices:

```sh
$ prov init my-vault
┌  prov init
│
◇  Title ················ My Vault
◇  Author ··············· (blank)
◇  Content format ······· Markdown
◇  Embed type ··········· Character delimiters
◇  Config language ······ YAML
◇  Wrapper ·············· Markdown
◇  Identity ············· On demand
◇  References between documents ··· By path
◇  Path format ·········· Workspace-absolute
◇  Where IDs are stored ·· In each file (+ registry)
◇  Content checksums ····· Attachments
│
└  initialized /home/you/my-vault
```

Each prompt has a flag, so you can skip the interview entirely. Pass `--yes`
(`-y`) to take every default:

<!-- exec -->
```sh
$ prov init my-vault --yes
initialized /home/you/my-vault
  root: index.md — My Vault
  config: prov.yaml — content markdown, embed delimited (character delimiters), language yaml, identity lazy, references path, markdown notation, root paths, id storage both, recycle bin, fixity attachments
next: prov new <title> --in index.md
```

The prompts, in the order they're asked:

| Prompt                        | Flag           | Default                       | Options                                                       |
| ----------------------------- | -------------- | ----------------------------- | ------------------------------------------------------------- |
| **Title**                     | `--title`      | the directory's name          | any text                                                      |
| **Author**                    | `--author`     | omitted                       | any text                                                      |
| **Content format**            | `--content`    | `markdown`                    | `markdown` (`.md`), `djot` (`.dj`), `html` (`.html`)          |
| **Embed type**                | `--embed`      | the content's first style     | `delimited`, `code-block`, `html-script`, `html-code`, `separate` — narrowed by content format |
| **Config language**           | `--meta`       | `yaml`                        | `yaml`, `json`, `toml`, `fig` — narrowed by embed type        |
| **Wrapper**                   | `--wrapper`    | `markdown`                    | `markdown` (`[Title](target)`), `wikilink` (`[[target]]`)     |
| **Identity**                  | `--identity`   | `lazy`                        | `off` (a.k.a. `none`), `lazy`, `eager` — see [§9](#9-stable-ids-optional) |
| **References between docs**   | `--reference`  | `path`                        | `path`, `id`, `alias`, `split` — `id`/`split` need identity   |
| **Path format**               | `--link-style` | `markdown-root`               | `markdown-root`, `markdown-relative`, `plain-relative`, `plain-canonical` (only when references are by path) |
| **Where IDs are stored**      | `--id-storage` | `frontmatter`                 | `registry`, `frontmatter` — only when identity is on          |
| **Content checksums**         | `--fixity`     | `payloads`                    | `off`, `payloads` (attachments), `full` (also bodies)         |

The root-shaping choices come first; the rest are **workspace preferences**, all
written into a config document (`prov.yaml`, linked from the root) so the
workspace records how it wants to be authored — see [§10](#10-workspace-config).
The **content format** sets the root file's extension and body grammar. The
**embed type** picks the carrier the config language is written in — frontmatter
delimiters, a fenced code block, an HTML data island, or a separate sidecar — and
gates which config languages fit (bare delimiters don't suit `fig`).

Setting some flags and being prompted for the rest works too. `--reference id`
needs identity, so it's rejected with `--identity off`:

```sh
$ prov init my-vault --content djot --reference id --yes
initialized /home/you/my-vault
  root: index.dj — My Vault
  config: prov.yaml — content djot, embed code_block (typed code block), language yaml, identity lazy, references id, id storage both, recycle bin, fixity attachments
next: prov new <title> --in index.dj
```

With no directory argument, `init` initializes the current directory. It refuses
to run where a workspace root already exists, so re-running it by mistake is
safe. Look at the root it wrote:

<!-- exec -->
```sh
$ cd my-vault
$ cat index.md
---
title: My Vault
config: prov.yaml
---

# My Vault
```

---

## 4. Grow the tree with `new`

`new` takes the new document's **title** as its positional argument and the
parent to hang it under as `--in` (`-i`). It derives a readable filename from the
title (a slug plus the content extension) and wires up *both* directions of the
spanning link — the parent gains a `contents` entry, the child gets a `part_of`
back.

<!-- exec -->
```sh
$ prov new "Rust" --in index.md
created rust.md (in index.md)
$ prov new "Zig" --in index.md
created zig.md (in index.md)
```

Override the derived filename with `--as <path>` (an exact path) or just its
extension with `--ext`. Look at what `new` wrote:

<!-- exec -->
```sh
$ cat index.md
---
title: My Vault
config: prov.yaml
contents:
- '[Rust](/rust.md)'
- '[Zig](/zig.md)'
---

# My Vault
$ cat rust.md
---
title: Rust
part_of: '[My Vault](/index.md)'
---
```

The links are ordinary Markdown links written into the metadata. Nothing about
the structure lives in the filesystem — move these files to another machine and
they still describe the same tree.

---

## 5. See the workspace

`tree` prints the containment tree, discovered by following `contents` from the
root:

<!-- exec -->
```sh
$ prov tree
index.md — My Vault
├── rust.md — Rust
└── zig.md — Zig
```

`show` summarizes one document — its title, spanning children, and overlay
links:

<!-- exec -->
```sh
$ prov show index.md
index.md
  title: My Vault
  contents (2 children):
    - [Rust](/rust.md)
    - [Zig](/zig.md)
  config:
    - prov.yaml
```

More single-document readers:

| Command                    | Prints                                             |
| -------------------------- | -------------------------------------------------- |
| `prov meta FILE`       | the raw metadata block (no fences)                 |
| `prov get FILE KEY`    | one field by dotted path (`title`, `contents.0`)   |
| `prov links FILE`      | every link as `relation⇥target`                    |
| `prov body FILE`       | everything *outside* the metadata block            |
| `prov backlinks FILE`  | who links *to* this document, across the workspace |

<!-- exec -->
```sh
$ prov backlinks index.md
rust.md	part_of	path
zig.md	part_of	path
```

---

## 6. Edit metadata

`set` and `unset` change a field while preserving the file's formatting,
comments, and metadata format. `set` even creates the block if a document has
none.

<!-- exec -->
```sh
$ prov set rust.md summary "Notes on the Rust language"
$ prov get rust.md summary
Notes on the Rust language
$ prov unset rust.md summary
```

Values are typed by inference: `true`/`false`, integers, floats, and `null`
become those types; everything else is a string. Dotted keys address nested
fields and sequence indices (`contents.0`).

### Body prose and `render`

The *body* is everything after the frontmatter. prov can render a
Markdown/Djot body to HTML, and it understands code — a `[[…]]` inside a code
span is treated as code, never as a link:

<!-- exec -->
```sh
$ printf '\n# Rust\n\nInline `let x = [[1,2],[3,4]];` is code, not a link.\n' >> rust.md
$ prov render rust.md
<h1>Rust</h1>
<p>Inline <code>let x = [[1,2],[3,4]];</code> is code, not a link.</p>
```

`render` picks the grammar from the extension: `.md`/`.markdown` → Markdown,
`.dj`/`.djot` → Djot, `.html`/`.htm` → HTML.

---

## 7. Restructure safely: `mv` and `rm`

This is prov's payoff. `mv` moves a file **and rewrites every link that
pointed at it** — the parent's `contents` entry, the moved file's own relative
links, overlay links, and body wikilinks across the whole workspace.

<!-- exec -->
```sh
$ prov mv rust.md rust-lang.md
moved rust.md -> rust-lang.md
$ prov tree
index.md — My Vault
├── rust-lang.md — Rust
└── zig.md — Zig
```

`rm` removes a document's parent entry and destroys the file. prov does not keep
a copy — getting the bytes back belongs to whatever tool this folder is
version-controlled or backed up with — but it **records what it deleted**, and
that record is the half no such tool has:

<!-- exec -->
```sh
$ prov rm zig.md
deleted zig.md (recorded; `prov restore` relinks it once the file is back)
```

Bring the file back yourself (`git checkout zig.md`, a copy out of a snapshot)
and `prov restore zig.md` does the rest: re-registers the id it held and re-adds
the parent's entry, which is what a plain file-level restore leaves broken.
`prov clear-deletions` forgets the records when you no longer want them.

`rm` refuses to orphan children unless you pass `--force`, and warns about any
links left dangling.

---

## 8. Check integrity

`check` walks from the root and reports problems: broken links, case mismatches,
duplicate containment, a child missing its `part_of` inverse, dangling IDs, and
documents on disk that nothing links to (orphans). It exits non-zero when it
finds anything, so it fits in CI. Right now the workspace is consistent:

<!-- exec -->
```sh
$ prov check
ok: no findings
```

Break the inverse on purpose to see a finding — and `--fix`, which walks the
findings and offers what can be done about each one. Most findings have more than
one defensible repair (a broken link can be pointed at a near-match *or* dropped;
an orphan can be adopted under any container above it), so `--fix` numbers them
and lets you choose; where there is only one, it just asks. Add `--fix mechanical`
to apply, without prompting, only the repairs that restate an authority rather
than choose between readings — the mode for scripts.

Repairs never delete a file, and they rewrite prose only where the parser itself
reported a link, so code that merely looks like one is never "repaired":

<!-- exec allow-fail -->
```sh
$ prov unset rust-lang.md part_of
$ prov check
index.md: child rust-lang.md does not declare part_of back to it
1 finding(s)
$ printf 'y\n' | prov check --fix
⚑  index.md: child rust-lang.md does not declare part_of back to it
   → declare part_of back to index.md  [derived]
   apply? [y]es / [n]o / [a]ll of this kind / [q]uit: applied 1 fix(es): 1 finding(s) resolved, 0 introduced, 0 still outstanding
```

The `[derived]` tag is why this one could have run under `--fix mechanical` too:
the child claims no other parent, so the back-link is the only reading of what
the parent already declares — nothing is being chosen. A repair tagged
`[judgment]` picks between rival answers and `[destructive]` removes something
you wrote, and neither ever runs unprompted.

A fix is a real mutation of the graph, so `--fix` re-runs `check` afterwards and
reports the difference in three buckets: what the fixes **resolved**, what they
**introduced**, and what was already there and still is. Only the middle one is
this run's fault, and it is the only one that makes `--fix` exit non-zero.

<!-- exec -->
```sh
$ prov check
ok: no findings
```

Broken *body* wikilinks are reported but not auto-fixed. Note that a body
wikilink like `[[index.md]]` resolves **relative to the file it's in** — from
`sub/rust.md` that means `sub/index.md`. Write `[[/index.md]]` (from the root) or
`[[../index.md]]` (relative) to point at the real root.

---

## 9. Stable IDs (optional)

Paths change; sometimes you want a link that *doesn't* break on a move. prov
can mint a stable ID for a document and resolve it back to a path — the "the app
owns your links" trick, except the identity data is a plain file in your own tree.

Two independent settings control this (§10):

- **`identity`** — *when* a document earns a stable ID: `none`/`off` (never),
  `lazy` (on a link-by-id or publish — the recommended default), or `eager`
  (every document at creation).
- **`references.target`** — *what a reference addresses*: `path`, `id`, or
  `alias`. Set it to `id` and prov authors structural links *by ID*, so a
  move rewrites no links at all (the registry tracks the new path). Only
  meaningful when `identity` isn't off. The `init` **References between
  documents** prompt sets this.

Even with `references.target: path`, `lazy` identity (the default) means you can
mint an ID on demand and paste a durable reference by hand:

<!-- exec -->
```sh
$ prov config identity lazy
set identity = lazy in prov.yaml
$ prov id rust-lang.md
initialized registry.yaml (linked from index.md)
id:s5jpwxz
```

The ID survives a move — the registry follows the file:

<!-- exec -->
```sh
$ id=$(prov id rust-lang.md)
$ prov mv rust-lang.md notes/rust.md
moved rust-lang.md -> notes/rust.md
$ prov resolve "$id"
notes/rust.md
```

The first `id` bootstraps a `registry` document (`registry.yaml`, or
`.json`/`.figl` matching your metadata format) beside the root and links it from
the root's metadata via the `registry` relation — so the identity state is
*reachable*, discovered by following links like everything else, not hidden in a
dotfolder. IDs are written `id:<id>`; deleting a document *tombstones* its ID (it
stops resolving but is never reissued), so a stale `id:` reference stays
diagnosable.

With `identity: off`, `prov id` politely refuses — there is nothing to mint.

---

## 10. Workspace config

Settings live in a config document linked from the root via the `config`
relation — same reachability move as the registry. `init` writes this document
(`prov.yaml`) with the preferences you chose; afterwards `prov config`
reads and writes it. Keys are grouped into a small nested vocabulary
(`docs/config-vocab.md`); a policy setting can also live in the root's
`prov:` frontmatter block. `prov check` flags any key prov would
silently ignore (a typo, or an unrecognized value).

<!-- exec -->
```sh
$ prov config
spec: 1
content_format: markdown
metadata:
  format: yaml
  embed: delimited
references:
  notation: markdown
  path_style: root
  target: path
  label: false
id_storage: both
updated: ''
identity: lazy
fixity: attachments
record_deletions: true
$ prov config references.target id
set references.target = id in prov.yaml
```

The knobs (dotted keys address nested axes):

| Key                       | Values                                                          | Meaning                                          |
| ------------------------- | -------------------------------------------------------------- | ------------------------------------------------ |
| `references.notation`     | `markdown`, `wikilink`, `bare`                                 | the syntactic form links are written in          |
| `references.path_style`   | `root`, `relative`, `canonical`                                | how a *path* target is resolved                  |
| `references.target`       | `path`, `id`, `alias`                                           | what a reference addresses                        |
| `references.label`        | `true`/`false`                                                 | whether an id/alias link carries a `\|Title`      |
| `identity`                | `none` (or `off`), `lazy`, `eager`                             | when a document earns a stable ID                |
| `id_storage`              | `registry`, `frontmatter`, `both`                              | where a stable ID lives                          |
| `metadata.format`         | `yaml`, `json`, `toml`, `fig`                                  | config language for newly created documents      |
| `metadata.embed`          | `delimited`, `code_block`, `html_script`, `html_code`, `separate` | how that config language is embedded          |
| `content_format`          | `markdown`, `djot`, `html`                                     | the body grammar the workspace is authored in    |
| `fixity`                  | `off`, `attachments`, `all`                                    | how far content-checksum coverage extends        |
| `record_deletions`        | `true`/`false`                                                 | a delete records what it destroyed               |
| `updated`                 | *a field name*                                                 | the machine-maintained "last updated" field      |

The two `init` identity prompts map onto these keys: **Identity** sets
`identity`, and **References between documents** sets `references.target`. With
`identity: lazy` + `references.target: id`, structural links are by ID and a move
rewrites nothing — the registry does the work.

**Making config explicit.** Every key has a default, so a workspace with a
minimal (or no) config document still runs — it just relies on those defaults. If
you would rather see and edit every setting, `prov config --setup` writes the
full effective config into `prov.yaml` (creating and linking it if needed),
filling in the keys you have not set while preserving the ones you have:

<!-- exec -->
```console
$ prov config --setup
wrote 9 explicit setting(s) to prov.yaml
```

**Config that won't take effect.** prov reads config back by exact key and
value, so a misspelled key or an unrecognized value is silently ignored (the
default stands). `prov check` reports each one; and any command that opens the
workspace prints a one-line reminder if your config has such a setting — or a
`spec` newer than your prov understands. Set `PROV_QUIET=1` to silence
these reminders.

---

## 11. When something changed outside prov

`edit` restamps the checksum and the `updated` field when it saves, because it
launched the editor and knows whether you changed anything. Nothing else does:
a sync client, another editor, a script, or `git checkout` all move bytes behind
prov's back, and the document's own bookkeeping is then out of date.

Turn on body checksums and name an `updated` field, so there is bookkeeping to
be out of date in the first place:

<!-- exec -->
```console
$ prov config fixity all
$ prov config updated updated
$ prov stamp --all
```

`stamp --all` gives every document a checksum for the bytes it currently has —
the baseline. It does *not* set `updated` on any of them: a checksum only
restates the bytes, but a timestamp claims an edit happened, and a sweep across
a workspace it merely read has no evidence for that.

Now change a file the way anything other than prov would, and `check` sees it:

<!-- exec allow-fail -->
```console
$ printf '\nA paragraph added by another editor.\n' >> notes/rust.md
$ prov check
notes/rust.md: fixity mismatch — content changed since its checksum was recorded (bit-rot, or an out-of-band edit)
1 finding(s)
```

`check` deliberately stops there. It cannot tell an edit you meant from bit-rot
you didn't, so it reports the question rather than answering it — which is why
`--fix mechanical` skips this one, and why even `--fix` would only correct the
checksum and never write `updated`: nothing on disk tells it when the edit
happened. **You** are the missing evidence, and `stamp` is how you supply it:

<!-- exec -->
```console
$ prov stamp notes/rust.md
notes/rust.md: stamped `updated` + checksum
$ prov check
ok: no findings
```

Run it again and it writes nothing — with a checksum on record, the stamps land
only when the bytes actually drifted. That makes it safe to put in a sync hook
or a cron job, and it makes `prov stamp --all` the standing repair for "my
folder was touched by something else":

<!-- exec -->
```console
$ prov stamp notes/rust.md
notes/rust.md: checksum still matches the bytes — nothing to stamp
```

Use `--no-timestamp` to correct the checksum without claiming an edit time, and
`--dry-run` to see what would move.

**Asking about one document.** `check --only` reports just the findings lodged
against one file — the ones whose repair rewrites it. It still walks the whole
workspace, because the findings that matter most about a single document are
the ones discovered from somewhere else: nothing links to it, its parent dropped
it, an inbound label went stale. Checking *from* the document (`prov check
notes/rust.md`, the positional argument) is a different question — that walks
its subtree — and it cannot see any of those.

<!-- exec allow-fail -->
```console
$ prov unset notes/rust.md part_of
$ prov check --only notes/rust.md
index.md: child notes/rust.md does not declare part_of back to it
1 finding(s) for notes/rust.md
$ prov check --only notes/rust.md --fix mechanical
```

Note whose finding that is: the missing back-link is reported by the *parent*,
but it is filed against the child, because the child is the file a repair
rewrites.

**For a script.** `--json` prints the same findings as a JSON array — each with
a `kind` to branch on, the `subject` it is filed against, the human `message`,
and that finding's own fields. A clean run prints `[]` rather than nothing, so
"no findings" and "no output" stay distinguishable:

<!-- exec -->
```console
$ prov check --json
[]
```

Stderr stays empty in this mode — the count line is for a person, and the array
already says how many it holds. The **exit code** does not change, though:
findings still exit non-zero, which is what lets `check` stand as a CI gate. If
your shell treats a non-zero exit as a failed pipeline (nushell does), capture
the status rather than piping through it:

```nu
# nushell: `prov check --json | from json` yields nothing when there are
# findings, because the pipeline aborts on the non-zero exit.
(prov check --json | complete).stdout | from json
```

---

## 12. Handing the folder to another tool

The workspace is what the graph reaches from the root. Everything else in the
directory — an editor's dotfiles, a scratch note nothing links, the page prov
generates for you — is on disk without
being *of* the workspace, and a tool that copies, syncs, backs up or
version-controls the folder has no way to know the difference. `prov ignore`
tells it:

<!-- exec -->
```console
$ echo "notes to self" > scratch.md
$ prov ignore
/about.md
/scratch.md
```

The lines are gitignore syntax, anchored to the workspace root. `--why` groups
them under the reason each rule is there:

<!-- exec -->
```console
$ prov ignore --why
# bookkeeping
/about.md

# unreached
/scratch.md
```

*Bookkeeping* is prov's own machinery, and *unreached* is the one worth
reading: nothing in the workspace links `scratch.md`. Link it — or delete it —
and the next run stops mentioning it, which makes this list a second way of
asking what `check`'s orphan findings ask.

So the whole list goes where such a list is expected:

<!-- exec -->
```console
$ prov ignore > .gitignore
```

Run it again afterwards and `/.gitignore` is on the list too, filed under
*hidden* — the file you just wrote is not part of the graph either, and prov
says so rather than making an exception for it.

prov writes nothing but the list. Merging it into a tool's own configuration —
what that tool already ignores, and what a rule should mean for a file it has
recorded before — is that tool's decision, not prov's. `--json` gives the same
rules as records for a program making it.

---

## Command reference

| Command                         | What it does                                             |
| ------------------------------- | -------------------------------------------------------- |
| `init [DIR] [flags]`            | create a workspace root (interactive; every prompt has a flag) |
| `new TITLE --in P`              | create a child document, linking both directions         |
| `mv FROM TO [--in P]`           | move/rename, maintaining every affected link             |
| `reparent PATH --in P`          | change a document's parent, leaving the file put         |
| `rm PATH [--force]`             | delete the file, removing the parent's entry and recording it |
| `restore PATH`                  | put a deleted document back in the graph, once its bytes are back |
| `clear-deletions`               | forget every deletion the log records                    |
| `attach FILE [--in P]`          | give a non-document file a metadata sidecar, linked in    |
| `attach FILE --opaque`          | the same for a file prov *could* read — a specimen it must not interpret |
| `attach DIR --manifest`         | cover a whole directory with one node and one file list, instead of a sidecar each |
| `manifest DIR [--update\|--verify]` | what the list says / rebuild it from the directory / re-check every file's checksum |
| `tree [ROOT]`                   | print the containment tree                               |
| `explore [FILE]`                | walk the graph interactively                             |
| `check [ROOT] [--fix]`          | report (and optionally repair) integrity problems        |
| `check --only F` / `--json`     | just one document's findings / the same as machine-readable JSON |
| `show FILE`                     | summarize a document                                     |
| `meta / get / links / body`     | read metadata or body                                    |
| `set FILE KEY VALUE` / `unset`  | edit a metadata field, format-preserving                 |
| `edit FILE`                     | open in `$EDITOR`, restamping fixity/`updated` on save    |
| `stamp FILE` / `stamp --all`    | the same bookkeeping for an edit prov didn't host (a sync, another editor) |
| `render FILE`                   | render the body to HTML                                  |
| `duplicate FILE`                | copy a document as a fresh sibling                       |
| `convert FILE AXIS VALUE`       | restate a document: links (`notation` / `path_style`), metadata (`metadata.format` / `metadata.embed`), or prose (`content_format`) |
| `id FILE` / `resolve ID`        | mint / look up a stable ID                               |
| `backlinks FILE`                | list inbound links                                       |
| `config [KEY [VALUE]]`          | read/write workspace settings                            |
| `ignore [--why\|--json]`        | what a tool copying this folder should leave alone, as gitignore lines |

Run `prov <command> --help` for the full options of any command.

---

## Known limitations

prov is young. Things a beginner will hit:

- **`mv` doesn't yet honor the reference style.** A move currently rewrites the
  parent's link as a *relative* path even when your `references.path_style` is
  `root`. The link still resolves; only its style changes. (`new` and
  `check --fix` do respect the style.)
- **The root must be unambiguous.** If a directory has two documents with
  metadata and no `part_of`, prov can't tell which is the root and reports
  an ambiguity. Keep a single root per workspace (name it `index.md`).
- **One vocabulary for now.** The CLI uses the built-in diaryx relation set
  (`contents`/`part_of`/`links`/…). Custom vocabularies exist in the library but
  aren't yet exposed as a CLI flag.

For where the project is headed, see [DESIGN.md](DESIGN.md) and
[next-steps.md](next-steps.md).
