import Base
import ./text.bend as T
import ./path.bend as P
import ./link.bend as L
import ./census.bend as N

# The title index an alias resolves through: every name a document in the
# workspace's own directories answers to — its file stem, and its `title` —
# and the documents answering to each. prov-graph's `title_index_scoped`: the
# directories are the ones a walk from the root occupies, following spanning
# path and id links and noting the directory of every path or id target
# (a body image's aside — a folder of pictures holds no titles). A spanning
# link that is itself an alias defeats the bound; prov scans the whole tree
# then, and this port refuses instead.

type Scope is Data:
  Scope{stack: List<&2, String>, visited: Set(), dirs: List<&2, String>, full: Bool}

# A file the index may name, and the sidecars beside it that could claim it
# as their attachment's payload, in `SIDECAR_EXTENSIONS` order.
type Cand is Data:
  Cand{path: String, sidecars: List<&2, String>}

# --- the scope ------------------------------------------------------------------

def Titles.prefix(a: List<&2, String>, b: List<&2, String>) -> Bool:
  match a b:
    case _ Nil{}:
      True{}
    case Nil{} Con{_, _}:
      False{}
    case Con{x, xs} Con{y, ys}:
      String.eq(x, y) && Titles.prefix(xs, ys)

# `Path::starts_with`, component-wise.
def Titles.under(+dir: String, p: String) -> Bool:
  Titles.prefix(P.Path.parts(dir), P.Path.parts(p))

def Titles.parked(+dir: String, parked: List<&2, String>) -> Bool:
  match parked:
    case Nil{}:
      False{}
    case h <> t:
      Titles.under(dir, h) || Titles.parked(dir, t)

def Titles.draft(+drafts: Map<&2, N.Draft>, key: String) -> N.Draft:
  Pair.snd(Map<&2, N.Draft>, N.Draft, Map.get(N.Draft, N.DUnread{""}, drafts, key))

def Titles.has(s: Set(), key: String) -> Bool:
  Pair.snd(Set(), Bool, Set.has(s, key))

def Titles.note(+q: String, +parked: List<&2, String>, dirs: List<&2, String>) -> List<&2, String>:
  T.Text.keep(Bool.not(Titles.parked(P.Path.parent(q), parked)), P.Path.parent(q), dirs)

def Titles.target(r: L.Res) -> Maybe<&2, String>:
  match r:
    case L.RPath{p}:
      Some{p}
    case L.RId{to}:
      Some{to}
    case L.RAlias{_, _}:
      None{}
    case L.ROff{}:
      None{}
    case L.RSame{}:
      None{}
    case L.RForeign{}:
      None{}
    case L.RMalformed{}:
      None{}
    case L.RDangling{}:
      None{}

def Titles.is_alias(r: L.Res) -> Bool:
  match r:
    case L.RAlias{_, _}:
      True{}
    case L.RPath{_}:
      False{}
    case L.RId{_}:
      False{}
    case L.ROff{}:
      False{}
    case L.RSame{}:
      False{}
    case L.RForeign{}:
      False{}
    case L.RMalformed{}:
      False{}
    case L.RDangling{}:
      False{}

def Titles.push(span: Bool, +q: String, stack: List<&2, String>) -> List<&2, String>:
  match span:
    case True{}:
      q <> stack
    case False{}:
      stack

def Titles.found(m: Maybe<&2, String>, span: Bool, +parked: List<&2, String>, sc: Scope) -> Scope:
  match m:
    case Some{+q}:
      match sc:
        case Scope{stack, visited, dirs, full}:
          Scope{Titles.push(span, q, stack), visited, Titles.note(q, parked, dirs), full}
    case None{}:
      sc

def Titles.alias(yes: Bool, sc: Scope) -> Scope:
  match yes:
    case True{}:
      match sc:
        case Scope{stack, visited, dirs, _}:
          Scope{stack, visited, dirs, True{}}
    case False{}:
      sc

def Titles.site.body(image: Bool) -> Maybe<&2, Bool>:
  match image:
    case True{}:
      None{}
    case False{}:
      Some{False{}}

def Titles.site(s: N.Site, image: Bool) -> Maybe<&2, Bool>:
  match s:
    case N.SRel{f, _}:
      Some{String.eq(f, N.Census.spanning())}
    case N.SBody{}:
      Titles.site.body(image)

# One link of a visited document: `span` says it is spanning (`None` for a
# body image, which the scope ignores).
def Titles.plan.go(kind: Maybe<&2, Bool>, +r: L.Res, +parked: List<&2, String>, sc: Scope) -> Scope:
  match kind:
    case None{}:
      sc
    case Some{+span}:
      Titles.alias(span && Titles.is_alias(r), Titles.found(Titles.target(r), span, parked, sc))

def Titles.plans(ps: List<&2, N.Plan>, +parked: List<&2, String>, sc: Scope) -> Scope:
  match ps:
    case Nil{}:
      sc
    case N.Plan{s, _, +r, image} <> t:
      Titles.plans(t, parked, Titles.plan.go(Titles.site(s, image), r, parked, sc))

def Titles.doc(d: N.Draft, +parked: List<&2, String>, sc: Scope) -> Scope:
  match d:
    case N.DUnread{_}:
      sc
    case N.DDoc{_, ps, _, _, _, _}:
      Titles.plans(ps, parked, sc)

def Titles.occupy(parked_dir: Bool, +drafts: Map<&2, N.Draft>, +parked: List<&2, String>, +p: String, sc: Scope) -> Scope:
  match parked_dir:
    case True{}:
      sc
    case False{}:
      match sc:
        case Scope{stack, visited, dirs, full}:
          Titles.doc(Titles.draft(drafts, p), parked, Scope{stack, visited, P.Path.parent(p) <> dirs, full})

def Titles.visit.go(seen: Bool, +drafts: Map<&2, N.Draft>, +parked: List<&2, String>, +p: String, sc: Scope) -> Scope:
  match seen:
    case True{}:
      sc
    case False{}:
      match sc:
        case Scope{stack, visited, dirs, full}:
          Titles.occupy(Titles.parked(P.Path.parent(p), parked), drafts, parked, p,
            Scope{stack, Set.add(visited, p), dirs, full})

def Titles.visit(+drafts: Map<&2, N.Draft>, +parked: List<&2, String>, +p: String, sc: Scope) -> Scope:
  match sc:
    case Scope{stack, +visited, dirs, full}:
      Titles.visit.go(Titles.has(visited, p), drafts, parked, p, Scope{stack, visited, dirs, full})

# `title_scope`: one pop per unit of fuel. Every push is a spanning link of a
# document visited once, so one more than their number suffices.
def Titles.walk(fuel: Nat, +drafts: Map<&2, N.Draft>, +parked: List<&2, String>, sc: Scope) -> Scope:
  match fuel:
    case 0n:
      sc
    case 1n+f:
      match sc:
        case Scope{stack, visited, dirs, full}:
          match stack:
            case Nil{}:
              Scope{[], visited, dirs, full}
            case +p <> rest:
              Titles.walk(f, drafts, parked, Titles.visit(drafts, parked, p, Scope{rest, visited, dirs, full}))

def Titles.start(+root: String) -> Scope:
  Scope{[root], Set.new(), [], False{}}

def Titles.dirs(sc: Scope) -> List<&2, String>:
  match sc:
    case Scope{_, _, d, _}:
      d

def Titles.full(sc: Scope) -> Bool:
  match sc:
    case Scope{_, _, _, f}:
      f

# --- the files -----------------------------------------------------------------------

def Titles.sidecar_exts() -> List<&2, String>:
  ["yaml", "yml", "json", "toml", "fig", "figl"]

def Titles.files.add(yes: Bool, n: String, s: Set()) -> Set():
  match yes:
    case True{}:
      Set.add(s, n)
    case False{}:
      s

# The files a directory holds — not its directories, not its hidden
# entries: what `ShadowProbe` is built over.
def Titles.files(es: List<&2, N.DirEnt>, s: Set()) -> Set():
  match es:
    case Nil{}:
      s
    case N.DirEnt{+k, +n} <> t:
      Titles.files(t, Titles.files.add(String.eq(k, "f") && Bool.not(String.starts_with(n, ".")), n, s))

def Titles.sidecars(+files: Set(), +dir: String, +name: String, exts: List<&2, String>) -> List<&2, String>:
  match exts:
    case Nil{}:
      Nil{}
    case e <> t:
      +side = name ++ "." ++ e
      T.Text.keep(Titles.has(files, side), P.Path.child(dir, side), Titles.sidecars(files, dir, name, t))

# `is_document_path`: a content document or a whole-file one.
def Titles.document(+name: String) -> Bool:
  N.Census.content_ext(P.Path.ext(name)) || N.Census.whole_ext(P.Path.ext(name))

def Titles.cand.keep(keep: Bool, +files: Set(), +dir: String, +name: String, rest: List<&2, Cand>) -> List<&2, Cand>:
  match keep:
    case True{}:
      Cand{P.Path.child(dir, name), Titles.sidecars(files, dir, name, Titles.sidecar_exts())} <> rest
    case False{}:
      rest

def Titles.ents(+files: Set(), +dir: String, es: List<&2, N.DirEnt>) -> List<&2, Cand>:
  match es:
    case Nil{}:
      Nil{}
    case N.DirEnt{+k, +n} <> t:
      Titles.cand.keep(String.eq(k, "f") && Bool.not(String.starts_with(n, ".")) && Titles.document(n),
        files, dir, n, Titles.ents(files, dir, t))

def Titles.dir(+dir: String, l: N.Listing) -> List<&2, Cand>:
  match l:
    case N.Listed{+es, _, _}:
      Titles.ents(Titles.files(es, Set.new()), dir, es)
    case N.Unlisted{}:
      Nil{}

def Titles.cands(ds: List<&2, String>, +listings: Map<&2, N.Listing>) -> List<&2, Cand>:
  match ds:
    case Nil{}:
      Nil{}
    case +d <> t:
      List.append(&2, Cand,
        Titles.dir(d, Pair.snd(Map<&2, N.Listing>, N.Listing, Map.get(N.Listing, N.Unlisted{}, listings, d))),
        Titles.cands(t, listings))

# Every file the index needs read: each candidate and its sidecars.
def Titles.reads(cs: List<&2, Cand>) -> List<&2, String>:
  match cs:
    case Nil{}:
      Nil{}
    case Cand{p, sides} <> t:
      p <> List.append(&2, String, sides, Titles.reads(t))

def Titles.claimed.go(c: Maybe<&2, String>, +p: String) -> Bool:
  match c:
    case Some{q}:
      String.eq(q, p)
    case None{}:
      False{}

# --- the index ----------------------------------------------------------------------

def Titles.claimed(d: N.Draft, +p: String) -> Bool:
  match d:
    case N.DUnread{_}:
      False{}
    case N.DDoc{_, _, _, _, _, c}:
      Titles.claimed.go(c, p)

# `is_shadowed_payload`: a sidecar beside the file is an attachment whose
# `content` names it.
def Titles.shadowed(+drafts: Map<&2, N.Draft>, +p: String, sides: List<&2, String>) -> Bool:
  match sides:
    case Nil{}:
      False{}
    case s <> t:
      Titles.claimed(Titles.draft(drafts, s), p) || Titles.shadowed(drafts, p, t)

def Titles.contains(ps: List<&2, String>, +p: String) -> Bool:
  match ps:
    case Nil{}:
      False{}
    case h <> t:
      String.eq(h, p) || Titles.contains(t, p)

def Titles.insert.put(ix: Map<&2, List<&2, String>>, name: String, p: String, have: List<&2, String>, dup: Bool) -> Map<&2, List<&2, String>>:
  match dup:
    case True{}:
      ix
    case False{}:
      Map.set(&2, List<&2, String>, ix, name, List.append(&2, String, have, [p]))

def Titles.insert.add(ix: Map<&2, List<&2, String>>, +name: String, +p: String, +have: List<&2, String>) -> Map<&2, List<&2, String>>:
  Titles.insert.put(ix, name, p, have, Titles.contains(have, p))

def Titles.insert.go(+ix: Map<&2, List<&2, String>>, +name: String, +p: String, blank: Bool) -> Map<&2, List<&2, String>>:
  match blank:
    case True{}:
      ix
    case False{}:
      Titles.insert.add(ix, name, p, L.Link.titled(ix, name))

# `TitleIndex::insert`: a blank name is no name, and a path is listed once.
def Titles.insert(+ix: Map<&2, List<&2, String>>, +name: String, +p: String) -> Map<&2, List<&2, String>>:
  Titles.insert.go(ix, name, p, String.is_empty(String.trim(name)))

# `file_stem`: the name before its last `.`, unless that `.` begins it.
def Titles.stem.go(+name: String, dotted: Bool) -> String:
  match dotted:
    case True{}:
      String.reverse(Maybe.default(&2, String, T.Text.after(String.reverse(name), '.'), ""))
    case False{}:
      name

def Titles.stem(+name: String) -> String:
  Titles.stem.go(name, T.Text.has(String.drop(name, 1n), '.'))

def Titles.title(d: N.Draft) -> Maybe<&2, String>:
  match d:
    case N.DUnread{_}:
      None{}
    case N.DDoc{_, _, _, _, t, _}:
      t

def Titles.titled(+ix: Map<&2, List<&2, String>>, +p: String, t: Maybe<&2, String>) -> Map<&2, List<&2, String>>:
  match t:
    case Some{+s}:
      Titles.insert(ix, s, p)
    case None{}:
      ix

def Titles.file(shadow: Bool, +drafts: Map<&2, N.Draft>, +p: String, +ix: Map<&2, List<&2, String>>) -> Map<&2, List<&2, String>>:
  match shadow:
    case True{}:
      ix
    case False{}:
      Titles.titled(Titles.insert(ix, Titles.stem(P.Path.name(p)), p), p, Titles.title(Titles.draft(drafts, p)))

def Titles.index(cs: List<&2, Cand>, +drafts: Map<&2, N.Draft>, +ix: Map<&2, List<&2, String>>) -> Map<&2, List<&2, String>>:
  match cs:
    case Nil{}:
      ix
    case Cand{+p, sides} <> t:
      Titles.index(t, drafts, Titles.file(Titles.shadowed(drafts, p, sides), drafts, p, ix))

# Whether any link is an alias — what makes prov build the index at all.
def Titles.needed.plans(ps: List<&2, N.Plan>) -> Bool:
  match ps:
    case Nil{}:
      False{}
    case N.Plan{_, _, r, _} <> t:
      Titles.is_alias(r) || Titles.needed.plans(t)

def Titles.needed.draft(d: N.Draft) -> Bool:
  match d:
    case N.DUnread{_}:
      False{}
    case N.DDoc{_, ps, _, _, _, _}:
      Titles.needed.plans(ps)

def Titles.needed(ns: List<&2, N.Named>) -> Bool:
  match ns:
    case Nil{}:
      False{}
    case N.Named{_, d} <> t:
      Titles.needed.draft(d) || Titles.needed(t)

def Titles.spans.inc(yes: Bool, n: Nat) -> Nat:
  match yes:
    case True{}:
      1n+n
    case False{}:
      n

def Titles.spans.add(s: N.Site, n: Nat) -> Nat:
  match s:
    case N.SRel{f, _}:
      Titles.spans.inc(String.eq(f, N.Census.spanning()), n)
    case N.SBody{}:
      n

def Titles.spans.plans(ps: List<&2, N.Plan>) -> Nat:
  match ps:
    case Nil{}:
      0n
    case N.Plan{s, _, _, _} <> t:
      Titles.spans.add(s, Titles.spans.plans(t))

def Titles.spans.draft(d: N.Draft) -> Nat:
  match d:
    case N.DUnread{_}:
      0n
    case N.DDoc{_, ps, _, _, _, _}:
      Titles.spans.plans(ps)

# The scope walk's fuel: one more than every spanning link there is.
def Titles.fuel(ns: List<&2, N.Named>) -> Nat:
  match ns:
    case Nil{}:
      1n
    case N.Named{_, d} <> t:
      Nat.add(Titles.spans.draft(d), Titles.fuel(t))

def Titles.map(ns: List<&2, N.Named>, m: Map<&2, N.Draft>) -> Map<&2, N.Draft>:
  match ns:
    case Nil{}:
      m
    case N.Named{p, d} <> t:
      Titles.map(t, Map.set(&2, N.Draft, m, p, d))
