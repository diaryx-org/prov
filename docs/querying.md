---
title: Querying your files
part_of: '[prov](/README.md)'
---

# Querying your files

A walkthrough of asking questions of a prov workspace — *which notes mention
Ada? what is still open? what did I write each month?* — and then saving the
answers you want to keep as **views**. It assumes you have `prov` installed
and have skimmed [Getting Started](getting-started.md); nothing else.

> **The transcripts below are executable.** CI runs every command in this
> guide, in order, against a real workspace, and checks that each query prints
> exactly what is shown (`ci/check-getting-started.sh docs/querying.md`). If
> prov changes so an answer here goes stale, the build fails.

---

## 1. Two questions, one language

Every question you ask has at most two parts:

- **which documents** — a *condition*, like "mentions Ada" or "is not done";
- **how to group them** — a *key*, like "by status" or "by month".

Both are written in [CEL](https://github.com/google/cel-spec), the Common
Expression Language: small, C-like expressions such as `status == 'open'`.
CEL is not something prov invented — it is what tools like Kubernetes use for
expressions inside configuration — so what you learn here is ordinary CEL,
plus a handful of functions prov adds. It cannot change your files and it
always finishes, so there is no query you can write here that does harm.

`prov query` asks a question once. A **view** is a question saved in the
workspace's settings, with a name, so every tool reading the workspace can ask
it the same way.

---

## 2. A workspace to ask questions of

Make a small one: a journal with four entries, and a task list with two tasks.

<!-- exec -->
```console
$ mkdir field-notes && cd field-notes
$ prov init --yes
$ prov new "Journal" --in index.md
$ prov new "Tasks" --in index.md
```

The entries are plain files, so write them the way you would write any
Markdown — each names its parent in `part_of`:

<!-- exec -->
```console
$ cat > river-walk.md <<'EOF'
---
title: Walk by the river
part_of: '[Journal](/journal.md)'
created: 2026-09-01
updated: 2026-09-20
people: [Ada, Grace]
tags: [walk, river]
mood: calm
---
EOF
$ cat > library.md <<'EOF'
---
title: At the library
part_of: '[Journal](/journal.md)'
created: 2026-09-14
people: [Ada]
tags: [reading]
draft: true
---
EOF
$ cat > letter.md <<'EOF'
---
title: Grandfather's letter
part_of: '[Journal](/journal.md)'
created: 2026-08-30
date_of_document: 1918/1920
people: [Walter]
tags: [family, archive]
---
EOF
$ cat > fair-photo.md <<'EOF'
---
title: Photo from the fair
part_of: '[Journal](/journal.md)'
created: 2026-08-30
date_of_document: 1913~
tags: [archive]
---
EOF
$ cat > fix-gate.md <<'EOF'
---
title: Fix the garden gate
part_of: '[Tasks](/tasks.md)'
created: 2026-09-02
status: open
---
EOF
$ cat > call-ada.md <<'EOF'
---
title: Call Ada back
part_of: '[Tasks](/tasks.md)'
created: 2026-09-10
status: done
people: [Ada]
---
EOF
```

Each file says which parent it belongs to, but the parents do not list them
yet, so nothing can reach them. `prov check` notices, and `--fix mechanical`
adds each one to the parent it names:

<!-- exec -->
```console
$ prov check --fix mechanical
```

Two of the dates are worth a second look. The letter's `1918/1920` means
*sometime between 1918 and 1920*, and the photo's `1913~` means *about 1913*.
Both are [EDTF](https://www.loc.gov/standards/datetime/), the Library of
Congress's standard spelling for uncertain dates, and prov reads them as what
they say rather than as broken dates.

`prov docs` lists every document the workspace reaches — the set every query
starts from:

<!-- exec expect -->
```console
$ prov docs
call-ada.md — Call Ada back
fair-photo.md — Photo from the fair
fix-gate.md — Fix the garden gate
index.md — Field Notes
journal.md — Journal
letter.md — Grandfather's letter
library.md — At the library
river-walk.md — Walk by the river
tasks.md — Tasks
```

---

## 3. Your first query

A condition names fields directly. Each field in a document's frontmatter is
a variable of the same name:

<!-- exec expect -->
```console
$ prov query "mood == 'calm'"
river-walk.md — Walk by the river
$ prov query "status == 'open'"
fix-gate.md — Fix the garden gate
```

Text goes in single quotes inside the expression, and the whole expression in
double quotes for the shell.

A field that holds a list — `people`, `tags` — is asked with `in`:

<!-- exec expect -->
```console
$ prov query "'Ada' in people"
call-ada.md — Call Ada back
library.md — At the library
river-walk.md — Walk by the river
```

`in` also works the other way round, for "is this one of these values":

<!-- exec expect -->
```console
$ prov query "status in ['open', 'in-progress']"
fix-gate.md — Fix the garden gate
```

---

## 4. Missing fields are `null`

Most documents do not have most fields. The letter has no `mood`, the tasks
have no `tags`, and the journal index has hardly anything. A field a document
does not have reads as `null`, so a comparison with it is simply false — which
is why `mood == 'calm'` above did not complain about the eight documents
without a mood. `'Ada' in people` is false on a document that lists nobody,
for the same reason.

To ask whether a field is there at all, use prov's `present()`. It is true
when the field is set to something — not missing, not blank, not an empty list:

<!-- exec expect -->
```console
$ prov query "present(draft)"
library.md — At the library
```

---

## 5. Combining conditions

`&&` is *and*, `||` is *or*, `!` is *not*, and parentheses group:

<!-- exec expect -->
```console
$ prov query "'archive' in tags && !present(draft)"
fair-photo.md — Photo from the fair
letter.md — Grandfather's letter
```

---

## 6. The document itself: `doc`

Besides its fields, every document is available as `doc`:

| | |
|---|---|
| `doc.title` | its title |
| `doc.path` | where it is, relative to the workspace root |
| `doc.id` | its stable id, if it has one |
| `doc.meta` | the whole frontmatter, for a field whose name is not a plain word: `doc.meta['date of birth']` |
| `doc.ancestors` | every document above it in the tree, from the root down, each with a `path`, `title` and `id` |

CEL's own text functions work on any of them:

<!-- exec expect -->
```console
$ prov query "doc.title.startsWith('Walk')"
river-walk.md — Walk by the river
```

`doc.ancestors` is how you ask about **one part of the workspace**. Everything
filed under the Journal, however deep:

<!-- exec expect -->
```console
$ prov query "doc.ancestors.exists(a, a.title == 'Journal')"
fair-photo.md — Photo from the fair
letter.md — Grandfather's letter
library.md — At the library
river-walk.md — Walk by the river
```

`exists(a, …)` is CEL for "at least one item of this list, called `a`,
satisfies …". This follows the tree, not the folders, so it keeps working if
you move the Journal's files somewhere else. Matching on `a.id` rather than
`a.title` keeps it working even if you rename the Journal.

---

## 7. Grouping with a key

Add `--key` to group the answer. Each distinct value becomes a group:

<!-- exec expect -->
```console
$ prov query "doc.ancestors.exists(a, a.title == 'Tasks')" --key status
done (1)
  call-ada.md — Call Ada back
open (1)
  fix-gate.md — Fix the garden gate

2 document(s), 2 row(s)
```

A list field puts a document under **each** of its values — the walk by the
river is under both Ada and Grace — and a document with no value lands in
`(ungrouped)`, shown rather than hidden:

<!-- exec expect -->
```console
$ prov query "doc.ancestors.exists(a, a.title == 'Journal')" --key people
Ada (2)
  library.md — At the library
  river-walk.md — Walk by the river
Grace (1)
  river-walk.md — Walk by the river
Walter (1)
  letter.md — Grandfather's letter
(ungrouped) (1)
  fair-photo.md — Photo from the fair

4 document(s), 5 row(s)
```

That is why the summary gives two numbers: four documents, drawn five times.

---

## 8. Grouping by date

A date is usually too fine to group by as it is. prov's `year()`, `month()`
and `day()` cut a date to that size:

<!-- exec expect -->
```console
$ prov query "doc.ancestors.exists(a, a.title == 'Journal')" --key "month(created)"
2026-08 (2)
  fair-photo.md — Photo from the fair
  letter.md — Grandfather's letter
2026-09 (2)
  library.md — At the library
  river-walk.md — Walk by the river

4 document(s), 4 row(s)
```

They read the uncertain dates too. *Sometime between 1918 and 1920* belongs
under each of those years, and *about 1913* under 1913:

<!-- exec expect -->
```console
$ prov query "present(date_of_document)" --key "year(date_of_document)"
1913 (1)
  fair-photo.md — Photo from the fair
1918 (1)
  letter.md — Grandfather's letter
1919 (1)
  letter.md — Grandfather's letter
1920 (1)
  letter.md — Grandfather's letter

2 document(s), 4 row(s)
```

Often the date you want is *the date the document is about, or failing that,
the date it was written*. `first()` takes the first of its arguments that is
present:

<!-- exec expect -->
```console
$ prov query "doc.ancestors.exists(a, a.title == 'Journal')" --key "year(first(date_of_document, created))"
1913 (1)
  fair-photo.md — Photo from the fair
1918 (1)
  letter.md — Grandfather's letter
1919 (1)
  letter.md — Grandfather's letter
1920 (1)
  letter.md — Grandfather's letter
2026 (2)
  library.md — At the library
  river-walk.md — Walk by the river

4 document(s), 6 row(s)
```

A key can also be a list, which puts a document under every group in it.
Here, every day a journal entry was written *or* changed — the walk by the
river shows up on both of its days:

<!-- exec expect -->
```console
$ prov query "doc.ancestors.exists(a, a.title == 'Journal')" --key "[day(created), day(updated)]"
2026-08-30 (2)
  fair-photo.md — Photo from the fair
  letter.md — Grandfather's letter
2026-09-01 (1)
  river-walk.md — Walk by the river
2026-09-14 (1)
  library.md — At the library
2026-09-20 (1)
  river-walk.md — Walk by the river

4 document(s), 5 row(s)
```

`initial()` does the same for names, cutting a value to its first letter for
an A–Z index:

<!-- exec expect -->
```console
$ prov query "present(people)" --key "initial(people)"
A (3)
  call-ada.md — Call Ada back
  library.md — At the library
  river-walk.md — Walk by the river
G (1)
  river-walk.md — Walk by the river
W (1)
  letter.md — Grandfather's letter

4 document(s), 5 row(s)
```

---

## 9. When a query cannot answer for a document

Some operations have no meaning on a missing field. `size()` counts the items
in a list, but a document with no `people` has nothing to count:

<!-- exec -->
```console
$ prov query "size(people) > 1"
river-walk.md — Walk by the river
prov: 5 document(s) could not be evaluated, and are not shown:
  fair-photo.md — where: `size` cannot take what it was given here — often a field this document does not have, which reads as null (`present()` checks for one)
  ...
```

prov still answers for the documents it can, and lists the ones it could not,
with the reason, so a query never silently shows or hides a document. The fix
is the one the message suggests — ask whether the field is there first. `&&`
stops as soon as its left side is false, so the `size()` is never reached for
a document without `people`:

<!-- exec expect -->
```console
$ prov query "present(people) && size(people) > 1"
river-walk.md — Walk by the river
```

A mistake in the expression itself — a typo, or a function that does not
exist — is refused before anything runs:

<!-- exec allow-fail -->
```console
$ prov query "present(people)" --key "mnth(created)"
prov: key: there is no function `mnth` — prov adds `present`, `first`, `field`, `year`, `month`, `day`, `initial` to CEL's own
```

---

## 10. Saving a question as a view

A question you ask often belongs in the workspace, where every tool reading it
can ask it too. A view is a `key:` and an optional `where:`, stored under
`views:` in the workspace's settings. `prov config` writes one a setting at a
time — the key first, since a view without one is not a view:

<!-- exec -->
```console
$ prov config views.open-tasks.key status
$ prov config views.open-tasks.where "doc.ancestors.exists(a, a.title == 'Tasks') && status != 'done'"
$ prov config views.open-tasks.label "Open tasks"
```

or you can write it into `prov.yaml` by hand:

```yaml
views:
  open-tasks:
    key: status
    where: doc.ancestors.exists(a, a.title == 'Tasks') && status != 'done'
    label: Open tasks
```

`prov views` lists what the workspace declares, and `prov views <name>` asks
the question:

<!-- exec expect -->
```console
$ prov views
open-tasks  Open tasks — key: status  where: doc.ancestors.exists(a, a.title == 'Tasks') && status != 'done'
$ prov views open-tasks
open (1)
  fix-gate.md — Fix the garden gate

1 document(s), 1 row(s)
```

`prov check` reads your views as it reads everything else, so a typo in one is
reported the next time you check rather than discovered the day the view comes
back empty.

For a script or another program, `--json` gives the same answer as data —
each document's path, title, id, ancestors and whole frontmatter — and lists
any document a query could not answer for under `failures`:

<!-- exec -->
```console
$ prov views open-tasks --json
$ prov query "present(draft)" --json
```

---

## Cheat sheet

| to ask | write |
|---|---|
| a field equals a value | `status == 'open'` |
| a field is one of several values | `status in ['open', 'in-progress']` |
| a list field contains a value | `'Ada' in people` |
| a field is filled in / is not | `present(draft)` / `!present(draft)` |
| any item of a list matches | `tags.exists(t, t.startsWith('arch'))` |
| under an index in the tree | `doc.ancestors.exists(a, a.title == 'Journal')` |
| …surviving a rename of that index | `doc.ancestors.exists(a, a.id == 'abc1234')` |
| a title or path pattern | `doc.title.startsWith('Walk')`, `doc.path.matches('^tasks/')` |
| group by a field | `--key status` |
| group by month / year / day | `--key "month(created)"` |
| the first date that is filled in | `--key "year(first(date_of_document, created))"` |
| every date a document has | `--key "[day(created), day(updated)]"` |
| an A–Z index | `--key "initial(people)"` |
| a nested field | `field('written.on')`, `field('confirmed[].by')` |

prov's functions are `present`, `first`, `field`, `year`, `month`, `day` and
`initial`; everything else — `==`, `in`, `&&`, `exists`, `startsWith`,
`matches`, `size` — is CEL's own, and CEL's
[language definition](https://github.com/google/cel-spec/blob/master/doc/langdef.md)
documents it.

## Where next

- [Views](config-vocab.md#views) in the configuration reference — the full
  rules, including how uncertain dates are grouped.
- [Filing](config-vocab.md#filing) — where a *new* document should go, which
  is a separate setting from how documents are read.
