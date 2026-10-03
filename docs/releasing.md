# Releasing prov

Every crate in the workspace shares one version number, one tag, and one
changelog. A release is therefore one command:

```console
$ release release minor          # bump, changelog, commit, tag
$ release release minor --push   # …and push, which ships the binaries
```

prov is not on crates.io; 0.18.0 was the last version uploaded there. The
repositories that build on it — plates, provui, diaryx — name it as a git
dependency, every one of them the same way:

```toml
prov = { git = "https://github.com/diaryx-org/prov", branch = "main" }
```

and each one's `Cargo.lock` records the exact commit it builds, which `cargo
update -p prov` moves. So a change reaches them once it is pushed to `main`,
not once it is released: a tag is a name for a commit and the start of a
Homebrew build, not the step that makes the code available. The spelling has to
match everywhere, because cargo tells sources apart by it — one consumer on a
`rev` or a `tag` while another is on `branch = "main"` is two provs in one
graph, and a type from one is not a type from the other.

`release` is the shared tooling in [diaryx-org/devtools][devtools], which prov,
twig, leaf, flower, and the historica repos all cut releases with. What makes
prov prov is `.config/release.toml` and nothing else; the behaviour lives there.
It used to be `cargo xtask release`, one of five copies of the same program.

[devtools]: https://github.com/diaryx-org/devtools

Everything below is what that command does, and what it deliberately refuses to
do on its own.

## What a tag starts

Pushing `vX.Y.Z` starts one workflow, and it cannot be undone:

- **`homebrew.yml`** builds the release binaries, attaches a WASI build, cuts
  the GitHub release, writes the formula into `diaryx-org/homebrew-tap`, and
  then sets the release body to that version's section of the changelog —
  `release release-notes <tag>`, which reads `docs/CHANGELOG.md` rather than
  re-rendering it, so the release page and the repository say the same thing,
  intro and all. That job runs after the rest, because attaching the first
  binary is what creates the release to write notes on. If the tag carries no
  section for its own version the job fails and nothing else does: write the
  section, and re-run it.

That is why `release` stops at the local tag unless it is given `--push`: every
step before the push is a commit you can amend or throw away, and the push is
the step that writes the tap. Without `--push` the command prints the two `git
push` lines it did not run, and the two-line undo.

## What `release` checks first

`release release` refuses before it writes anything if the working tree is
dirty, the branch is not `main`, `main` is behind `origin/main`, the tag already
exists locally or on origin, or git-cliff is not installed.

Then it runs the whole of CI (`cargo xtask ci`), the same jobs the workflow
runs. `--no-verify` skips that, and is for a release you have just watched go
green.

## The pieces, on their own

| Command | What it does |
|---|---|
| `release version` | print the workspace version |
| `release bump <patch\|minor\|major\|x.y.z\|as-is>` | move `[workspace.package]`, every internal `path`+`version` dependency, and the lockfile |
| `release changelog` | print the generated region |
| `release changelog --write` | splice it into `docs/CHANGELOG.md` |
| `release changelog --check` | fail if that region is stale |
| `release release-notes [tag]` | that release's changelog section, as the GitHub release body |

## The changelog

`docs/CHANGELOG.md` is handwritten except for one region, between

```
<!-- git-cliff:begin — generated; edits here are overwritten -->
<!-- git-cliff:end -->
```

inside `## Unreleased`. git-cliff fills it from the commits since the last tag
through the shared `cliff.toml` in diaryx-org/devtools; `release` renders it one
last time, moves it into a `## vX.Y.Z — date` section, and empties the region.
Edits inside the markers are lost on the next write. A release **intro** — for a
release that wants a narrative rather than a list — goes in the released section
below the end marker, where regeneration cannot reach it.

`--write` also answers to the tag list, not just to the region: any `v*` tag
with no `## <tag> —` section of its own gets one, generated from its commit
range and folded in at its place in the order. That is what a tag cut *after*
the fact needs — v0.5.0 was tagged once its crates were already published,
and until the backfill existed, tagging it made those commits vanish from the
file entirely: no longer unreleased, and in no section either. `--check` reports
a missing section the same way it reports a stale region. Existing sections are
never rewritten, so a handwritten intro survives every write.

There is no CI job checking the region for staleness, unlike twig and fig: it is
regenerated as part of every release, and git-cliff is not on the runners.

## What the commits have to say

Two conventions carry straight into the changelog.

**Spell the colon.** `add(history): a verb for the bytes a capture is still
holding`, not `add(history) a verb …`. Most of prov's log drops it, and
git-conventional then cannot tell where the subject ends — the whole commit body
lands in the bullet and the trailers below it are never parsed as trailers. A
preprocessor in the shared `cliff.toml` in diaryx-org/devtools puts the colon
back for the known types so the existing history still reads, but it is a rescue,
not a licence.

`add` is the house spelling of `feat`; `polish` rides with `refactor`. `docs`,
`chore`, `test`, `ci`, `build`, and `style` are skipped entirely, and anything
the parsers do not recognise lands in an **Uncategorised — triage before
release** bucket rather than being dropped.

**Write a `Behavioural-change:` trailer** on any commit where a caller who
upgrades without editing a line of their own code would observe a difference — a
field that appears, an error that stops being returned, a walk that now skips
something. It is true of a bug fix as often as of a feature. The trailers are
collected, in commit order, into a **Behavioural changes** section at the end of
the release, which is the part a consumer reads first and often only.

```
add(history): a capture written somewhere it cannot do any harm

Behavioural-change: `HistoryStore::capture` takes `CaptureNote<'_>` where it
  took `label: Option<&str>`. A caller passing a label updates to
  `CaptureNote::labelled(label)`, and one passing `None` to
  `CaptureNote::default()`.
```

One trailer per observable difference; a commit may carry several. Continuation
lines are indented two spaces.
