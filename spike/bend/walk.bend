import Base
import ./path.bend as P
import ./link.bend as L
import ./census.bend as N

# The ordered half of `check`: prov-graph's spanning walk, replayed over the
# documents the census has already read, and the findings it and the census
# make. The walk is a stack — the last child pushed is the next visited — and
# a spanning target already pushed is a second parent or a cycle. Each child
# it descends into is asked whether its `part_of` points back.

# A finding, in the words `prov check` prints.

type Fact is Data:
  XUnread{doc: String, error: String}
  XDup{doc: String, target: String}
  XInverse{doc: String, child: String, inverse: String}
  XBroken{doc: String, site: N.Site, target: String}
  XCase{doc: String, site: N.Site, target: String, actual: String}
  XOrphan{doc: String}

# One census entry: a link, where it is written, and how it resolved.
type Entry is Data:
  Entry{source: String, site: N.Site, target: String, hit: N.Hit}

# The walk's state: the stack (its head is the top), every path ever pushed,
# and what has been found so far, newest first.
type St is Data:
  St{stack: List<&2, String>, seen: Set(), facts: List<&2, Fact>, census: List<&2, Entry>, bodies: List<&2, String>}

# --- lookups --------------------------------------------------------------------

def Walk.has(s: Set(), key: String) -> Bool:
  Pair.snd(Set(), Bool, Set.has(s, key))

def Walk.node(+nodes: Map<&2, N.Node>, key: String) -> N.Node:
  Pair.snd(Map<&2, N.Node>, N.Node, Map.get(N.Node, N.NUnread{"not loaded"}, nodes, key))

def Walk.is_span(s: N.Site) -> Bool:
  match s:
    case N.SRel{f, _}:
      String.eq(f, N.Census.spanning())
    case N.SBody{}:
      False{}

# --- the inverse check --------------------------------------------------------------

def Walk.same(m: Maybe<&2, String>, +p: String) -> Bool:
  match m:
    case Some{q}:
      String.eq(q, p)
    case None{}:
      False{}

# Whether any of a child's `part_of` targets, read from the child, lands on
# `p` — `resolve_link_with(..) == Target::Path(p)`.
def Walk.points(+ctx: L.Ctx, +p: String, +child: String, ups: List<&2, String>) -> Bool:
  match ups:
    case Nil{}:
      False{}
    case h <> t:
      Walk.same(L.Link.lands(ctx, child, L.Link.target(L.Link.parse(h))), p) || Walk.points(ctx, p, child, t)

def Walk.back(back: Bool, +p: String, +child: String, facts: List<&2, Fact>) -> List<&2, Fact>:
  match back:
    case True{}:
      facts
    case False{}:
      XInverse{p, child, N.Census.inverse()} <> facts

def Walk.meta(meta: Bool, +ctx: L.Ctx, +p: String, +child: String, ups: List<&2, String>, facts: List<&2, Fact>) -> List<&2, Fact>:
  match meta:
    case True{}:
      Walk.back(Walk.points(ctx, p, child, ups), p, child, facts)
    case False{}:
      facts

# Only a child that loads and has metadata is asked.
def Walk.inverse(n: N.Node, +ctx: L.Ctx, +p: String, +child: String, facts: List<&2, Fact>) -> List<&2, Fact>:
  match n:
    case N.NUnread{_}:
      facts
    case N.NDoc{m, _, ups, _}:
      Walk.meta(m, ctx, p, child, ups, facts)

# --- one link --------------------------------------------------------------------------

def Walk.descend.go(
  dup: Bool, +nodes: Map<&2, N.Node>, +ctx: L.Ctx, +p: String, +target: String, +r: String,
  stack: List<&2, String>, seen: Set(), facts: List<&2, Fact>, census: List<&2, Entry>, bodies: List<&2, String>
) -> St:
  match dup:
    case True{}:
      St{stack, seen, XDup{p, target} <> facts, census, bodies}
    case False{}:
      St{r <> stack, Set.add(seen, r), Walk.inverse(Walk.node(nodes, r), ctx, p, r, facts), census, bodies}

def Walk.descend(+nodes: Map<&2, N.Node>, +ctx: L.Ctx, +p: String, +target: String, +r: String, st: St) -> St:
  match st:
    case St{stack, +seen, facts, census, bodies}:
      Walk.descend.go(Walk.has(seen, r), nodes, ctx, p, target, r, stack, seen, facts, census, bodies)

def Walk.span.to(m: Maybe<&2, String>, +nodes: Map<&2, N.Node>, +ctx: L.Ctx, +p: String, +target: String, st: St) -> St:
  match m:
    case Some{r}:
      Walk.descend(nodes, ctx, p, target, r, st)
    case None{}:
      st

def Walk.span(span: Bool, m: Maybe<&2, String>, +nodes: Map<&2, N.Node>, +ctx: L.Ctx, +p: String, +target: String, st: St) -> St:
  match span:
    case True{}:
      Walk.span.to(m, nodes, ctx, p, target, st)
    case False{}:
      st

def Walk.record(e: Entry, st: St) -> St:
  match st:
    case St{stack, seen, facts, census, bodies}:
      St{stack, seen, facts, e <> census, bodies}

# A frontmatter or body link: the spanning ones are descended into, and every
# one goes into the census.
def Walk.link(+nodes: Map<&2, N.Node>, +ctx: L.Ctx, +p: String, l: N.CLink, st: St) -> St:
  match l:
    case N.CLink{+site, +target, +hit}:
      Walk.record(Entry{p, site, target, hit},
        Walk.span(Walk.is_span(site), N.Census.reached(hit), nodes, ctx, p, target, st))

def Walk.links(+nodes: Map<&2, N.Node>, +ctx: L.Ctx, +p: String, ls: List<&2, N.CLink>, st: St) -> St:
  match ls:
    case Nil{}:
      st
    case h <> t:
      Walk.links(nodes, ctx, p, t, Walk.link(nodes, ctx, p, h, st))

def Walk.body.put(
  h: N.Hit, +p: String, site: N.Site, target: String,
  stack: List<&2, String>, seen: Set(), facts: List<&2, Fact>, census: List<&2, Entry>, bodies: List<&2, String>
) -> St:
  match h:
    case N.HPath{q}:
      St{stack, seen, facts, census, q <> bodies}
    case N.HCase{g, +a}:
      St{stack, seen, XCase{p, site, target, a} <> facts, census, P.Path.child(P.Path.parent(g), a) <> bodies}
    case N.HBroken{}:
      St{stack, seen, XBroken{p, site, target} <> facts, census, bodies}
    case N.HId{_}:
      St{stack, seen, facts, census, bodies}
    case N.HNone{}:
      St{stack, seen, facts, census, bodies}

# A separated document's `content`: not a graph edge, but its body file is
# reached, and a body that is not there is a finding the walk raises.
def Walk.body.hit(h: N.Hit, +p: String, site: N.Site, target: String, st: St) -> St:
  match st:
    case St{stack, seen, facts, census, bodies}:
      Walk.body.put(h, p, site, target, stack, seen, facts, census, bodies)

def Walk.content(m: Maybe<&2, N.CLink>, +p: String, st: St) -> St:
  match m:
    case Some{N.CLink{s, t, h}}:
      Walk.body.hit(h, p, s, t, st)
    case None{}:
      st

def Walk.fact(f: Fact, st: St) -> St:
  match st:
    case St{stack, seen, facts, census, bodies}:
      St{stack, seen, f <> facts, census, bodies}

def Walk.visit.node(n: N.Node, +nodes: Map<&2, N.Node>, +ctx: L.Ctx, +p: String, st: St) -> St:
  match n:
    case N.NUnread{e}:
      Walk.fact(XUnread{p, e}, st)
    case N.NDoc{_, ls, _, c}:
      Walk.content(c, p, Walk.links(nodes, ctx, p, ls, st))

def Walk.visit(+nodes: Map<&2, N.Node>, +ctx: L.Ctx, +p: String, st: St) -> St:
  Walk.visit.node(Walk.node(nodes, p), nodes, ctx, p, st)

# One visit per unit of fuel. A path is pushed only once (`seen`), and only
# for a spanning link of a document visited for the first time, so one more
# than the number of spanning links suffices; `main.bend` passes that, and a
# walk that stops with the stack not empty is reported, never truncated.
def Walk.run(fuel: Nat, +nodes: Map<&2, N.Node>, +ctx: L.Ctx, st: St) -> St:
  match fuel:
    case 0n:
      st
    case 1n+f:
      match st:
        case St{stack, seen, facts, census, bodies}:
          match stack:
            case Nil{}:
              St{[], seen, facts, census, bodies}
            case +p <> rest:
              Walk.run(f, nodes, ctx, Walk.visit(nodes, ctx, p, St{rest, seen, facts, census, bodies}))

def Walk.spans.add(span: Bool, n: Nat) -> Nat:
  match span:
    case True{}:
      1n+n
    case False{}:
      n

# Every spanning link any document holds: the fuel the walk is given.
def Walk.spans(ls: List<&2, N.CLink>) -> Nat:
  match ls:
    case Nil{}:
      0n
    case N.CLink{s, _, _} <> t:
      Walk.spans.add(Walk.is_span(s), Walk.spans(t))

def Walk.node.spans(n: N.Node) -> Nat:
  match n:
    case N.NUnread{_}:
      0n
    case N.NDoc{_, ls, _, _}:
      Walk.spans(ls)

def Walk.fuel(ns: List<&2, N.Node>) -> Nat:
  match ns:
    case Nil{}:
      1n
    case h <> t:
      Nat.add(Walk.node.spans(h), Walk.fuel(t))

def Walk.start(+root: String) -> St:
  St{[root], Set.add(Set.new(), root), [], [], []}

# --- the census's findings ------------------------------------------------------------

def Walk.is_about(s: N.Site) -> Bool:
  match s:
    case N.SRel{f, _}:
      String.eq(f, N.Census.about())
    case N.SBody{}:
      False{}

def Walk.entry.broken(about: Bool, source: String, site: N.Site, target: String, rest: List<&2, Fact>) -> List<&2, Fact>:
  match about:
    case True{}:
      rest
    case False{}:
      XBroken{source, site, target} <> rest

# `finding_for`, over the kinds this port models: a broken link (but not an
# `about` pointer at a page not yet written — that page is derived) and a
# case mismatch.
def Walk.entry.hit(h: N.Hit, +source: String, +site: N.Site, target: String, rest: List<&2, Fact>) -> List<&2, Fact>:
  match h:
    case N.HBroken{}:
      Walk.entry.broken(Walk.is_about(site), source, site, target, rest)
    case N.HCase{_, a}:
      XCase{source, site, target, a} <> rest
    case N.HPath{_}:
      rest
    case N.HId{_}:
      rest
    case N.HNone{}:
      rest

def Walk.findings(es: List<&2, Entry>) -> List<&2, Fact>:
  match es:
    case Nil{}:
      Nil{}
    case Entry{+s, +site, t, h} <> rest:
      Walk.entry.hit(h, s, site, t, Walk.findings(rest))

# --- reachability and orphans ------------------------------------------------------

def Walk.reach.hit(h: N.Hit, rest: List<&2, String>) -> List<&2, String>:
  match h:
    case N.HPath{p}:
      p <> rest
    case N.HId{to}:
      to <> rest
    case N.HCase{g, +a}:
      P.Path.child(P.Path.parent(g), a) <> rest
    case N.HBroken{}:
      rest
    case N.HNone{}:
      rest

def Walk.reach.entries(es: List<&2, Entry>) -> List<&2, String>:
  match es:
    case Nil{}:
      Nil{}
    case Entry{_, _, _, h} <> rest:
      Walk.reach.hit(h, Walk.reach.entries(rest))

# `reachable_set`: the root, every census link's resolved path, and every
# separated document's body.
def Walk.reachable(+root: String, es: List<&2, Entry>, bodies: List<&2, String>) -> List<&2, String>:
  root <> List.append(&2, String, bodies, Walk.reach.entries(es))

def Walk.dirs(ps: List<&2, String>) -> List<&2, String>:
  match ps:
    case Nil{}:
      Nil{}
    case h <> t:
      P.Path.parent(h) <> Walk.dirs(t)

def Walk.content_ext(+e: String) -> Bool:
  String.eq(e, "md") || String.eq(e, "markdown") || String.eq(e, "dj") || String.eq(e, "djot")
    || String.eq(e, "html") || String.eq(e, "htm")

def Walk.orphan.keep(+path: String, rest: List<&2, String>, keep: Bool) -> List<&2, String>:
  match keep:
    case True{}:
      path <> rest
    case False{}:
      rest

def Walk.orphan.ents(+dir: String, +reached: Set(), es: List<&2, N.DirEnt>) -> List<&2, String>:
  match es:
    case Nil{}:
      Nil{}
    case N.DirEnt{+k, +name} <> t:
      Walk.orphan.keep(P.Path.child(dir, name), Walk.orphan.ents(dir, reached, t),
        String.eq(k, "f") && Bool.not(String.starts_with(name, "."))
          && Walk.content_ext(P.Path.ext(name))
          && Bool.not(Walk.has(reached, P.Path.child(dir, name))))

def Walk.orphan.dir(+dir: String, +reached: Set(), l: N.Listing) -> List<&2, String>:
  match l:
    case N.Listed{es, _, _}:
      Walk.orphan.ents(dir, reached, es)
    case N.Unlisted{}:
      Nil{}

def Walk.orphan.dirs(ds: List<&2, String>, +reached: Set(), +listings: Map<&2, N.Listing>) -> List<&2, String>:
  match ds:
    case Nil{}:
      Nil{}
    case +d <> t:
      List.append(&2, String,
        Walk.orphan.dir(d, reached,
          Pair.snd(Map<&2, N.Listing>, N.Listing, Map.get(N.Listing, N.Unlisted{}, listings, d))),
        Walk.orphan.dirs(t, reached, listings))

def Walk.le(a: String, b: String) -> Bool:
  P.Path.le(a, b)

def Walk.orphan.facts(ps: List<&2, String>) -> List<&2, Fact>:
  match ps:
    case Nil{}:
      Nil{}
    case h <> t:
      XOrphan{h} <> Walk.orphan.facts(t)

# The documents directly inside a directory the reachable set occupies that
# nothing reaches, in path order. `dirs` holds each such directory once.
def Walk.orphans(dirs: List<&2, String>, +reached: Set(), +listings: Map<&2, N.Listing>) -> List<&2, Fact>:
  Walk.orphan.facts(List.sort(~String, ~Walk.le, Walk.orphan.dirs(dirs, reached, listings)))

def Walk.site.index(f: String, i: Maybe<&2, Nat>) -> String:
  match i:
    case Some{n}:
      f ++ "[" ++ Nat.show(n) ++ "]"
    case None{}:
      f

# --- the words ------------------------------------------------------------------------

def Walk.site(s: N.Site) -> String:
  match s:
    case N.SRel{f, i}:
      Walk.site.index(f, i)
    case N.SBody{}:
      "body"

def Walk.show(f: Fact) -> String:
  match f:
    case XUnread{d, e}:
      d ++ ": unreadable: " ++ e
    case XDup{d, t}:
      d ++ ": " ++ t ++ " is already contained elsewhere (cycle or second parent)"
    case XInverse{d, c, i}:
      d ++ ": child " ++ c ++ " does not declare " ++ i ++ " back to it"
    case XBroken{d, s, t}:
      d ++ ": broken " ++ Walk.site(s) ++ " link: " ++ t
    case XCase{d, s, t, a}:
      d ++ ": case mismatch in " ++ Walk.site(s) ++ " link: " ++ t ++ " is " ++ a ++ " on disk"
    case XOrphan{d}:
      d ++ ": orphan — on disk but not linked into the workspace"

def Walk.warning(f: Fact) -> Bool:
  match f:
    case XCase{_, _, _, _}:
      True{}
    case XUnread{_, _}:
      False{}
    case XDup{_, _}:
      False{}
    case XInverse{_, _, _}:
      False{}
    case XBroken{_, _, _}:
      False{}
    case XOrphan{_}:
      False{}

# How many findings the census's entries owe: one for each broken link that
# is not an `about` pointer, one for each case mismatch, none for the rest.
# `findings_exact` in LAWS.bend says `findings` pays exactly that.
def Walk.faults.broken(about: Bool, n: Nat) -> Nat:
  match about:
    case True{}:
      n
    case False{}:
      1n+n

def Walk.faults.hit(h: N.Hit, site: N.Site, n: Nat) -> Nat:
  match h:
    case N.HBroken{}:
      Walk.faults.broken(Walk.is_about(site), n)
    case N.HCase{_, _}:
      1n+n
    case N.HPath{_}:
      n
    case N.HId{_}:
      n
    case N.HNone{}:
      n

def Walk.faults(es: List<&2, Entry>) -> Nat:
  match es:
    case Nil{}:
      0n
    case Entry{_, site, _, h} <> rest:
      Walk.faults.hit(h, site, Walk.faults(rest))
