import Base
import ./text.bend as T
import ./path.bend as P
import ./link.bend as L
import ./canon.bend as C

# The per-document half of prov's census: every link a document declares —
# its relation fields, in the vocabulary's order, then its body links — with
# where it is written and what it names. A pure function of one document, the
# registry, and the directory listings, so a batch of documents is a parallel
# tree (`Census.plan.par`, `Census.fix.par`); the walk that orders the census
# and raises the structural findings is `walk.bend`'s.

# Where in a document a link is written: a relation field (and its place in
# the list), or the body.

type Site is Data:
  SRel{field: String, index: Maybe<&2, Nat>}
  SBody{}

# A link before the disk is asked. `image` marks a body `![alt](target)`,
# which names a payload and not a document.
type Plan is Data:
  Plan{site: Site, target: String, res: L.Res, image: Bool}

# How a link resolves, the disk asked: `Resolution` in prov-graph.
type Hit is Data:
  HPath{path: String}
  HCase{got: String, actual: String}
  HBroken{}
  HId{to: String}
  HNone{}

type CLink is Data:
  CLink{site: Site, target: String, hit: Hit}

# A document as the walk needs it. `ups` are the raw targets of the spanning
# relation's inverse (`part_of`), which the parent's inverse check reads;
# `content` is a separated document's body pointer.
type Node is Data:
  NUnread{error: String}
  NDoc{meta: Bool, links: List<&2, CLink>, ups: List<&2, String>, content: Maybe<&2, CLink>}

# A document as `plan` leaves it, before the disk is asked. `title` is what
# the title index files it under besides its stem; `claims` is the payload it
# is an attachment sidecar for, if it is one.
type Draft is Data:
  DUnread{error: String}
  DDoc{meta: Bool, plans: List<&2, Plan>, ups: List<&2, String>, content: Maybe<&2, Plan>, title: Maybe<&2, String>, claims: Maybe<&2, String>}

# A document as the host answered for it: why it could not be read, or its
# records, not yet read here — reading them is the plan's, on its own lane.
type Loaded is Data:
  LUnread{error: String}
  LText{text: String}

type DirEnt is Data:
  DirEnt{kind: String, name: String}

# A directory as listed: its entries in the order the directory gave them,
# and — `DirNames` — the set of its names and the map from each name folded
# to ASCII lower case to the name it folds from (the last such), built once
# so each link resolved into it is a lookup rather than a walk.
type Listing is Data:
  Listed{entries: List<&2, DirEnt>, names: Set(), folded: Map<&2, String>}
  Unlisted{}

type Named is Data:
  Named{path: String, draft: Draft}

type Fixed is Data:
  Fixed{path: String, node: Node}

# --- in parallel -------------------------------------------------------------------

type Job is Data:
  Job{path: String, loaded: Loaded}

# --- the vocabulary -------------------------------------------------------------

# prov's `RelationSet::diaryx()`, in its order — the order `edges` reads a
# document's fields in, and so the census's. A workspace that declares its own
# vocabulary is refused by `main.bend`, not answered differently.
def Census.relations() -> List<&2, String>:
  ["contents", "part_of", "links", "link_of", "replaces", "replaced_by", "derived_from",
   "derivations", "registry", "config", "deletions", "recycle_bin", "history", "about"]

def Census.spanning() -> String:
  "contents"

def Census.inverse() -> String:
  "part_of"

def Census.about() -> String:
  "about"

def Census.one(+ctx: L.Ctx, +doc: String, site: Site, raw: String) -> Plan:
  +t = L.Link.target(L.Link.parse(raw))
  Plan{site, t, L.Link.resolve(ctx, doc, t), False{}}

# --- reading a document's links ---------------------------------------------------

def Census.items(+ctx: L.Ctx, +doc: String, +field: String, items: List<&2, C.Item>) -> List<&2, Plan>:
  match items:
    case Nil{}:
      Nil{}
    case C.IStr{i, s} <> t:
      Census.one(ctx, doc, SRel{field, Some{i}}, s) <> Census.items(ctx, doc, field, t)
    case C.INon{_} <> t:
      Census.items(ctx, doc, field, t)

# `indexed_link_strings`: a string is one link with no place; a list is a
# link per string item, each at its place as written.
def Census.field(+ctx: L.Ctx, +doc: String, +field: String, f: C.Field) -> List<&2, Plan>:
  match f:
    case C.FStr{s}:
      [Census.one(ctx, doc, SRel{field, None{}}, s)]
    case C.FSeq{items}:
      Census.items(ctx, doc, field, items)
    case C.FNone{}:
      Nil{}
    case C.FOther{}:
      Nil{}

def Census.fields(+ctx: L.Ctx, +doc: String, +vals: List<&2, C.Val>, names: List<&2, String>) -> List<&2, Plan>:
  match names:
    case Nil{}:
      Nil{}
    case +n <> t:
      List.append(&2, Plan, Census.field(ctx, doc, n, C.Canon.get(vals, n)), Census.fields(ctx, doc, vals, t))

def Census.imaged(r: L.Res, +doc: String, +t: String) -> L.Res:
  match r:
    case L.RAlias{_, p}:
      L.RPath{p}
    case L.ROff{}:
      L.ROff{}
    case L.RSame{}:
      L.RSame{}
    case L.RForeign{}:
      L.RForeign{}
    case L.RMalformed{}:
      L.RMalformed{}
    case L.RDangling{}:
      L.RDangling{}
    case L.RId{to}:
      L.RId{to}
    case L.RPath{p}:
      L.RPath{p}

def Census.body.alias(r: L.Res, +doc: String, +t: String, image: Bool) -> L.Res:
  match image:
    case True{}:
      Census.imaged(r, doc, t)
    case False{}:
      r

# An image names a payload, never a document by title: resolved by path only.
def Census.body.res(+ctx: L.Ctx, +doc: String, +t: String, image: Bool) -> L.Res:
  Census.body.alias(L.Link.resolve(ctx, doc, t), doc, t, image)

def Census.body(+ctx: L.Ctx, +doc: String, ls: List<&2, C.BodyLink>) -> List<&2, Plan>:
  match ls:
    case Nil{}:
      Nil{}
    case C.BodyLink{_, _, +image, _, +t} <> rest:
      Plan{SBody{}, t, Census.body.res(ctx, doc, t, image), image} <> Census.body(ctx, doc, rest)

def Census.strings.items(items: List<&2, C.Item>) -> List<&2, String>:
  match items:
    case Nil{}:
      Nil{}
    case C.IStr{_, s} <> t:
      s <> Census.strings.items(t)
    case C.INon{_} <> t:
      Census.strings.items(t)

def Census.strings(f: C.Field) -> List<&2, String>:
  match f:
    case C.FStr{s}:
      [s]
    case C.FSeq{items}:
      Census.strings.items(items)
    case C.FNone{}:
      Nil{}
    case C.FOther{}:
      Nil{}

# A separated document's `content`: resolved from the document, by path.
def Census.content(+doc: String, m: Maybe<&2, String>) -> Maybe<&2, Plan>:
  match m:
    case Some{+c}:
      Some{Plan{SRel{"content", None{}}, c, L.RPath{P.Path.resolve(doc, c)}, False{}}}
    case None{}:
      None{}

def Census.content_ext(+e: String) -> Bool:
  String.eq(e, "md") || String.eq(e, "markdown") || String.eq(e, "dj") || String.eq(e, "djot")
    || String.eq(e, "html") || String.eq(e, "htm")

def Census.whole_ext(+e: String) -> Bool:
  String.eq(e, "yaml") || String.eq(e, "yml") || String.eq(e, "json") || String.eq(e, "toml")
    || String.eq(e, "fig") || String.eq(e, "figl")

# `is_opaque_payload`: neither a content document nor a whole-file one.
def Census.opaque(+p: String) -> Bool:
  Bool.not(Census.content_ext(P.Path.ext(P.Path.name(p)))) && Bool.not(Census.whole_ext(P.Path.ext(P.Path.name(p))))

def Census.claims.go(yes: Bool, p: String) -> Maybe<&2, String>:
  match yes:
    case True{}:
      Some{p}
    case False{}:
      None{}

# `sidecar_claims`, the half a document can answer alone: it is an
# attachment (`attachment: true`, or a `content` that is an opaque payload),
# and so claims the file its `content` names.
def Census.claims(+doc: String, m: Maybe<&2, String>, flagged: Bool) -> Maybe<&2, String>:
  match m:
    case Some{+c}:
      Census.claims.go(flagged || Census.opaque(c), P.Path.normalize(P.Path.parent(doc) ++ "/" ++ c))
    case None{}:
      None{}

def Census.draft(+ctx: L.Ctx, +doc: String, d: C.Doc) -> Draft:
  match d:
    case C.Doc{meta, +vals, links}:
      DDoc{
        meta,
        List.append(&2, Plan, Census.fields(ctx, doc, vals, Census.relations()), Census.body(ctx, doc, links)),
        Census.strings(C.Canon.get(vals, Census.inverse())),
        Census.content(doc, C.Canon.str(vals, "content")),
        C.Canon.str(vals, "title"),
        Census.claims(doc, C.Canon.str(vals, "content"), C.Canon.anywhere_top(vals, "attachment", "b", "true"))}

# One document, read: what it links to, before the disk is asked.
def Census.plan(+ctx: L.Ctx, +doc: String, l: Loaded) -> Draft:
  match l:
    case LUnread{e}:
      DUnread{e}
    case LText{t}:
      Census.draft(ctx, doc, C.Canon.parse(t))

# --- the disk --------------------------------------------------------------------

def Census.lower(s: String) -> String:
  String.to_lower(s)

def Census.exact.variant(p: String, actual: String, none: Bool) -> Hit:
  match none:
    case True{}:
      HBroken{}
    case False{}:
      HCase{p, actual}

def Census.exact.folded(p: String, +actual: String) -> Hit:
  Census.exact.variant(p, actual, String.is_empty(actual))

def Census.exact.hit(held: Bool, +p: String, +name: String, folded: Map<&2, String>) -> Hit:
  match held:
    case True{}:
      HPath{p}
    case False{}:
      Census.exact.folded(p, Pair.snd(Map<&2, String>, String, Map.get(String, "", folded, Census.lower(name))))

def Census.exact.in(+p: String, +name: String, l: Listing) -> Hit:
  match l:
    case Listed{_, names, folded}:
      Census.exact.hit(Pair.snd(Set(), Bool, Set.has(names, name)), p, name, folded)
    case Unlisted{}:
      HBroken{}

def Census.index.names(es: List<&2, DirEnt>, s: Set()) -> Set():
  match es:
    case Nil{}:
      s
    case DirEnt{_, n} <> t:
      Census.index.names(t, Set.add(s, n))

def Census.index.folded(es: List<&2, DirEnt>, m: Map<&2, String>) -> Map<&2, String>:
  match es:
    case Nil{}:
      m
    case DirEnt{_, +n} <> t:
      Census.index.folded(t, Map.set(&2, String, m, Census.lower(n), n))

# A directory's entries, indexed.
def Census.listed(+es: List<&2, DirEnt>) -> Listing:
  Listed{es, Census.index.names(es, Set.new()), Census.index.folded(es, Map.new(&2, String))}

# `exact_name`: how a path's last component matches its directory's listing.
def Census.exact(+dirs: Map<&2, Listing>, +p: String) -> Hit:
  Census.exact.in(p, P.Path.name(p),
    Pair.snd(Map<&2, Listing>, Listing, Map.get(Listing, Unlisted{}, dirs, P.Path.parent(p))))

def Census.alias.one(+dirs: Map<&2, Listing>, h: String, one: Bool) -> Hit:
  match one:
    case True{}:
      Census.exact(dirs, P.Path.normalize(h))
    case False{}:
      HNone{}

# An alias, resolved through the title index: one document is that
# document, on disk or not; several are ambiguous; none falls through to the
# path the target spells.
def Census.alias(+dirs: Map<&2, Listing>, ps: List<&2, String>, path: String) -> Hit:
  match ps:
    case Nil{}:
      Census.exact(dirs, path)
    case h <> t:
      Census.alias.one(dirs, h, List.is_empty(&2, String, t))

def Census.hit.alias(+dirs: Map<&2, Listing>, ctx: L.Ctx, name: String, path: String) -> Hit:
  match ctx:
    case L.Ctx{_, _, +titles}:
      Census.alias(dirs, L.Link.titled(titles, name), path)

def Census.hit(+dirs: Map<&2, Listing>, +ctx: L.Ctx, r: L.Res) -> Hit:
  match r:
    case L.RPath{p}:
      Census.exact(dirs, p)
    case L.RId{to}:
      HId{to}
    case L.RAlias{name, path}:
      Census.hit.alias(dirs, ctx, name, path)
    case L.ROff{}:
      HNone{}
    case L.RSame{}:
      HNone{}
    case L.RForeign{}:
      HNone{}
    case L.RMalformed{}:
      HNone{}
    case L.RDangling{}:
      HNone{}

def Census.link(+dirs: Map<&2, Listing>, +ctx: L.Ctx, p: Plan) -> CLink:
  match p:
    case Plan{s, t, r, _}:
      CLink{s, t, Census.hit(dirs, ctx, r)}

def Census.links(+dirs: Map<&2, Listing>, +ctx: L.Ctx, ps: List<&2, Plan>) -> List<&2, CLink>:
  match ps:
    case Nil{}:
      Nil{}
    case h <> t:
      Census.link(dirs, ctx, h) <> Census.links(dirs, ctx, t)

def Census.mlink(+dirs: Map<&2, Listing>, +ctx: L.Ctx, m: Maybe<&2, Plan>) -> Maybe<&2, CLink>:
  match m:
    case Some{p}:
      Some{Census.link(dirs, ctx, p)}
    case None{}:
      None{}

# One document, the disk asked.
def Census.fix(+dirs: Map<&2, Listing>, +ctx: L.Ctx, d: Draft) -> Node:
  match d:
    case DUnread{e}:
      NUnread{e}
    case DDoc{m, ps, ups, c, _, _}:
      NDoc{m, Census.links(dirs, ctx, ps), ups, Census.mlink(dirs, ctx, c)}

# The path a hit reaches, if it reaches one: what the spanning walk descends
# into. A case mismatch descends into the path as written.
def Census.reached(h: Hit) -> Maybe<&2, String>:
  match h:
    case HPath{p}:
      Some{p}
    case HCase{g, _}:
      Some{g}
    case HId{to}:
      Some{to}
    case HBroken{}:
      None{}
    case HNone{}:
      None{}

def Census.res.dir(r: L.Res, rest: List<&2, String>) -> List<&2, String>:
  match r:
    case L.RPath{p}:
      P.Path.parent(p) <> rest
    case L.RId{_}:
      rest
    case L.RAlias{_, p}:
      P.Path.parent(p) <> rest
    case L.ROff{}:
      rest
    case L.RSame{}:
      rest
    case L.RForeign{}:
      rest
    case L.RMalformed{}:
      rest
    case L.RDangling{}:
      rest

# The directories a plan's paths sit in: what must be listed before it is
# fixed.
def Census.plan.dirs(ps: List<&2, Plan>) -> List<&2, String>:
  match ps:
    case Nil{}:
      Nil{}
    case Plan{_, _, r, _} <> t:
      Census.res.dir(r, Census.plan.dirs(t))

def Census.mplan.dirs(m: Maybe<&2, Plan>) -> List<&2, String>:
  match m:
    case Some{p}:
      Census.plan.dirs([p])
    case None{}:
      Nil{}

def Census.draft.dirs(d: Draft) -> List<&2, String>:
  match d:
    case DUnread{_}:
      Nil{}
    case DDoc{_, ps, _, c, _, _}:
      List.append(&2, String, Census.plan.dirs(ps), Census.mplan.dirs(c))

def Split.cons(h: Job, r: List<&2, Job> & List<&2, Job>) -> List<&2, Job> & List<&2, Job>:
  (a, b) = r
  (h <> a, b)

# The first `n` of a list and the rest, the list consumed: each half is then
# owned by one lane, rather than one list every lane counts references on.
def Split.go(n: Nat, xs: List<&2, Job>) -> List<&2, Job> & List<&2, Job>:
  match n:
    case 0n:
      ([], xs)
    case 1n+k:
      match xs:
        case Nil{}:
          ([], [])
        case h <> t:
          Split.cons(h, Split.go(k, t))

def Census.plan.seq(+ctx: L.Ctx, js: List<&2, Job>) -> List<&2, Named>:
  match js:
    case Nil{}:
      Nil{}
    case Job{+p, l} <> t:
      Named{p, Census.plan(ctx, p, l)} <> Census.plan.seq(ctx, t)

# `depth` levels of fork-join over a list already split in two. Splitting and
# forking are one def because two would be mutual recursion, which Bend
# refuses.
def Census.plan.tree(+depth: Nat, +ctx: L.Ctx, +n: Nat, r: List<&2, Job> & List<&2, Job>) -> List<&2, Named>:
  match depth:
    case 0n:
      (xs, ys) = r
      List.append(&2, Named, Census.plan.seq(ctx, xs), Census.plan.seq(ctx, ys))
    case 1n+d:
      (xs, ys) = r
      +q = Nat.div(n, 4n)
      a b = Census.plan.tree(d, ctx, Nat.div(n, 2n), Split.go(q, xs)) Census.plan.tree(d, ctx, Nat.div(n, 2n), Split.go(q, ys))
      List.append(&2, Named, a, b)

# Every job planned, `2^(depth+1)` lanes at most. `n` is how many there
# are: counting them here would consume the list the lanes are to own.
def Census.plan.par(+depth: Nat, +ctx: L.Ctx, +n: Nat, js: List<&2, Job>) -> List<&2, Named>:
  Census.plan.tree(depth, ctx, n, Split.go(Nat.div(n, 2n), js))

def Split.ncons(h: Named, r: List<&2, Named> & List<&2, Named>) -> List<&2, Named> & List<&2, Named>:
  (a, b) = r
  (h <> a, b)

def Split.ngo(n: Nat, xs: List<&2, Named>) -> List<&2, Named> & List<&2, Named>:
  match n:
    case 0n:
      ([], xs)
    case 1n+k:
      match xs:
        case Nil{}:
          ([], [])
        case h <> t:
          Split.ncons(h, Split.ngo(k, t))

def Census.fix.seq(+dirs: Map<&2, Listing>, +ctx: L.Ctx, ns: List<&2, Named>) -> List<&2, Fixed>:
  match ns:
    case Nil{}:
      Nil{}
    case Named{p, d} <> t:
      Fixed{p, Census.fix(dirs, ctx, d)} <> Census.fix.seq(dirs, ctx, t)

def Census.fix.tree(+depth: Nat, +dirs: Map<&2, Listing>, +ctx: L.Ctx, +n: Nat, r: List<&2, Named> & List<&2, Named>) -> List<&2, Fixed>:
  match depth:
    case 0n:
      (xs, ys) = r
      List.append(&2, Fixed, Census.fix.seq(dirs, ctx, xs), Census.fix.seq(dirs, ctx, ys))
    case 1n+d:
      (xs, ys) = r
      +q = Nat.div(n, 4n)
      a b = Census.fix.tree(d, dirs, ctx, Nat.div(n, 2n), Split.ngo(q, xs)) Census.fix.tree(d, dirs, ctx, Nat.div(n, 2n), Split.ngo(q, ys))
      List.append(&2, Fixed, a, b)

def Census.fix.par(+depth: Nat, +dirs: Map<&2, Listing>, +ctx: L.Ctx, +n: Nat, ns: List<&2, Named>) -> List<&2, Fixed>:
  Census.fix.tree(depth, dirs, ctx, n, Split.ngo(Nat.div(n, 2n), ns))

# How many jobs a split holds, both halves together.
def Split.total(r: List<&2, Job> & List<&2, Job>) -> Nat:
  (a, b) = r
  Nat.add(List.length(&2, Job, a), List.length(&2, Job, b))

def Split.ntotal(r: List<&2, Named> & List<&2, Named>) -> Nat:
  (a, b) = r
  Nat.add(List.length(&2, Named, a), List.length(&2, Named, b))
