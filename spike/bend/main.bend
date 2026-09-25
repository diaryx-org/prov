import Base
import ./text.bend as T
import ./path.bend as P
import ./link.bend as L
import ./canon.bend as C
import ./census.bend as N
import ./walk.bend as W
import ./titles.bend as Ti
import ./ws.bend as Ws

# `prov-bend <dir> <root>`: `prov check`, from the root document `root` of the
# workspace in `dir`, over the kinds of finding this port models — a
# document that cannot be read, a second parent or cycle, a child whose
# `part_of` does not point back, a broken link, a case mismatch, and an
# orphan. Findings go to standard output a line each, as prov prints them;
# the count goes to standard error; findings exit 1.
#
# Reading happens in rounds. Each round loads the documents the spanning
# relation has newly reached, plans every link they hold in parallel, lists
# the directories those links point into, and finds what the next round
# must load. Then every document is resolved against the listings in
# parallel, the walk is replayed over them in prov's order, and the
# directories the reachable set occupies are listed for orphans.

# --- the host ----------------------------------------------------------------------

type Rounds is Data:
  Rounds{named: List<&2, N.Named>, dirs: Map<&2, N.Listing>, done: Bool}

type Tree is Data:
  Tree{dirs: Map<&2, N.Listing>, all: List<&2, String>}

# One answer of a batch: its head (what was asked, whether it was answered,
# why not) and its body.
def Io.head.ok(+fs: List<&2, String>) -> Bool:
  String.eq(C.Canon.field(fs, 1n), "1")

def Io.job.doc(ok: Bool, why: String, body: String) -> N.Loaded:
  match ok:
    case True{}:
      N.LText{body}
    case False{}:
      N.LUnread{why}

def Io.job(head: String, body: String) -> N.Job:
  +fs = String.split(head, '\t')
  N.Job{C.Canon.field(fs, 0n), Io.job.doc(Io.head.ok(fs), C.Canon.field(fs, 2n), body)}

# Heads and bodies alternate.
def Io.jobs.pairs(ps: List<&2, String>) -> List<&2, N.Job>:
  match ps:
    case Nil{}:
      Nil{}
    case h <> Nil{}:
      Nil{}
    case h <> Con{b, t}:
      Io.job(h, b) <> Io.jobs.pairs(t)

def Io.jobs.done(r: Result<&1, &1, U32 & String, List<&2, String>>) -> IO(List<&2, N.Job>):
  match r:
    case Done{ps}:
      IO.pure(List<&2, N.Job>, Io.jobs.pairs(ps))
    case Fail{e}:
      (_, msg) = e
      IO.die(List<&2, N.Job>, 5, "prov-bend: the host could not read documents: " ++ msg)

def Io.escaping.keep(yes: Bool, +h: String, rest: List<&2, N.Job>) -> List<&2, N.Job>:
  match yes:
    case True{}:
      N.Job{h, N.LUnread{"path escapes the workspace root: " ++ h}} <> rest
    case False{}:
      rest

def Io.escaping(ps: List<&2, String>) -> List<&2, N.Job>:
  match ps:
    case Nil{}:
      Nil{}
    case +h <> t:
      Io.escaping.keep(P.Path.escapes(h), h, Io.escaping(t))

def Io.inside(ps: List<&2, String>) -> List<&2, String>:
  match ps:
    case Nil{}:
      Nil{}
    case +h <> t:
      T.Text.keep(Bool.not(P.Path.escapes(h)), h, Io.inside(t))

def Io.asked(+root: String, ps: List<&2, String>) -> IO(List<&2, N.Job>):
  match ps:
    case Nil{}:
      IO.pure(List<&2, N.Job>, [])
    case +h <> +t:
      IO.bind(Result<&1, &1, U32 & String, List<&2, String>>, List<&2, N.Job>,
        Ws.Ws.loads(root ++ "\n" ++ String.join(h <> t, "\n")), Io.jobs.done)

# Every document in `ps` read, in one crossing of the boundary. `Graph::load`
# refuses a path that climbs above the root before the host is asked for it,
# and so does this.
def Io.jobs(+root: String, +ps: List<&2, String>) -> IO(List<&2, N.Job>):
  do IO<List<&2, N.Job>>:
    asked : List<&2, N.Job> <- Io.asked(root, Io.inside(ps))
    return List.append(&2, N.Job, Io.escaping(ps), asked)

def Io.ent(line: String) -> N.DirEnt:
  +fs = String.split(line, '\t')
  N.DirEnt{C.Canon.field(fs, 0n), C.Canon.field(fs, 1n)}

def Io.has(m: Map<&2, N.Listing>, key: String) -> Bool:
  Pair.snd(Map<&2, N.Listing>, Bool, Map.has(&2, N.Listing, m, key))

def Io.first(js: List<&2, N.Job>) -> N.Loaded:
  match js:
    case Nil{}:
      N.LUnread{"not read"}
    case N.Job{_, l} <> _:
      l

def Io.ents.put(empty: Bool, h: String, rest: List<&2, N.DirEnt>) -> List<&2, N.DirEnt>:
  match empty:
    case True{}:
      rest
    case False{}:
      Io.ent(h) <> rest

def Io.ents(ls: List<&2, String>) -> List<&2, N.DirEnt>:
  match ls:
    case Nil{}:
      Nil{}
    case +h <> t:
      Io.ents.put(String.is_empty(h), h, Io.ents(t))

def Io.listing(ok: Bool, body: String) -> N.Listing:
  match ok:
    case True{}:
      N.Census.listed(Io.ents(String.lines(body)))
    case False{}:
      N.Unlisted{}

def Io.dirs.pairs(ps: List<&2, String>, dirs: Map<&2, N.Listing>) -> Map<&2, N.Listing>:
  match ps:
    case Nil{}:
      dirs
    case h <> Nil{}:
      dirs
    case h <> Con{b, t}:
      +fs = String.split(h, '\t')
      Io.dirs.pairs(t, Map.set(&2, N.Listing, dirs, C.Canon.field(fs, 0n), Io.listing(Io.head.ok(fs), b)))

def Io.dirs.done(dirs: Map<&2, N.Listing>, r: Result<&1, &1, U32 & String, List<&2, String>>) -> IO(Map<&2, N.Listing>):
  match r:
    case Done{ps}:
      IO.pure(Map<&2, N.Listing>, Io.dirs.pairs(ps, dirs))
    case Fail{e}:
      (_, msg) = e
      IO.die(Map<&2, N.Listing>, 5, "prov-bend: the host could not list directories: " ++ msg)

def Io.list_all(+root: String, ds: List<&2, String>, dirs: Map<&2, N.Listing>) -> IO(Map<&2, N.Listing>):
  match ds:
    case Nil{}:
      IO.pure(Map<&2, N.Listing>, dirs)
    case +h <> +t:
      do IO<Map<&2, N.Listing>>:
        r : Result<&1, &1, U32 & String, List<&2, String>> <- Ws.Ws.dirs(root ++ "\n" ++ String.join(h <> t, "\n"))
        Io.dirs.done(dirs, r)

def Io.unknown(ds: List<&2, String>, +dirs: Map<&2, N.Listing>) -> List<&2, String>:
  match ds:
    case Nil{}:
      Nil{}
    case +h <> t:
      T.Text.keep(Bool.not(Io.has(dirs, h)), h, Io.unknown(t, dirs))

# Every directory in `ds` not yet listed, listed, in one crossing.
def Io.lists(+root: String, ds: List<&2, String>, +dirs: Map<&2, N.Listing>) -> IO(Map<&2, N.Listing>):
  Io.list_all(root, Io.unknown(ds, dirs), dirs)

def Io.sub.keep(+d: String, +parked: List<&2, String>, +k: String, +n: String, rest: List<&2, String>) -> List<&2, String>:
  T.Text.keep(String.eq(k, "d") && Bool.not(String.starts_with(n, "."))
    && Bool.not(Ti.Titles.parked(P.Path.child(d, n), parked)), P.Path.child(d, n), rest)

def Io.subs(+d: String, +parked: List<&2, String>, es: List<&2, N.DirEnt>) -> List<&2, String>:
  match es:
    case Nil{}:
      Nil{}
    case N.DirEnt{+k, +n} <> t:
      Io.sub.keep(d, parked, k, n, Io.subs(d, parked, t))

def Io.subdirs(+d: String, +parked: List<&2, String>, l: N.Listing) -> List<&2, String>:
  match l:
    case N.Listed{es, _, _}:
      Io.subs(d, parked, es)
    case N.Unlisted{}:
      Nil{}

# Every directory under the root, but hidden and parked ones, each listed:
# one listing per unit of fuel.
def Io.tree(fuel: Nat, +root: String, +parked: List<&2, String>, todo: List<&2, String>, +dirs: Map<&2, N.Listing>, acc: List<&2, String>) -> IO(Tree):
  match fuel:
    case 0n:
      IO.pure(Tree, Tree{dirs, acc})
    case 1n+f:
      match todo:
        case Nil{}:
          IO.pure(Tree, Tree{dirs, acc})
        case +d <> rest:
          do IO<Tree>:
            +dirs2 : Map<&2, N.Listing> <- Io.lists(root, [d], dirs)
            Io.tree(f, root, parked, List.append(&2, String, rest, Io.subdirs(d, parked,
              Pair.snd(Map<&2, N.Listing>, N.Listing, Map.get(N.Listing, N.Unlisted{}, dirs2, d)))),
              dirs2, d <> acc)

# --- the configuration -------------------------------------------------------------

def Cfg.vals(l: N.Loaded) -> List<&2, C.Val>:
  match l:
    case N.LText{t}:
      C.Canon.vals(C.Canon.parse(t))
    case N.LUnread{_}:
      Nil{}

def Cfg.pointer.go(+root: String, m: Maybe<&2, String>) -> Maybe<&2, String>:
  match m:
    case Some{raw}:
      Some{P.Path.resolve(root, L.Link.target(L.Link.parse(raw)))}
    case None{}:
      None{}

# A pointer the root holds — `config`, `registry` — as a path, if it is one.
def Cfg.pointer(+root: String, +vals: List<&2, C.Val>, +key: String) -> Maybe<&2, String>:
  Cfg.pointer.go(root, C.Canon.str(vals, key))

def Cfg.load(+dir: String, m: Maybe<&2, String>) -> IO(List<&2, C.Val>):
  match m:
    case Some{+p}:
      do IO<List<&2, C.Val>>:
        js : List<&2, N.Job> <- Io.jobs(dir, [p])
        return Cfg.vals(Io.first(js))
    case None{}:
      IO.pure(List<&2, C.Val>, [])

def Cfg.ws_ok(+w: String) -> Bool:
  Bool.not(String.is_empty(w)) && Bool.not(T.Text.has(w, '/')) && Bool.not(T.Text.has(w, ':'))
    && Bool.not(T.Text.has(w, ' ')) && Bool.not(T.Text.has(w, '\t'))

def Cfg.ws.go(+w: String, ok: Bool) -> String:
  T.Text.pick(ok, w, "")

def Cfg.ws.m(m: Maybe<&2, String>) -> String:
  match m:
    case Some{+w}:
      Cfg.ws.go(w, Cfg.ws_ok(w))
    case None{}:
      ""

# `workspace_id`, ignored when it is not a usable name.
def Cfg.ws(m: Maybe<&2, String>) -> String:
  Cfg.ws.m(m)

def Cfg.registry(kvs: List<&2, C.KV>, m: Map<&2, String>) -> Map<&2, String>:
  match kvs:
    case Nil{}:
      m
    case C.KV{k, v} <> t:
      Cfg.registry(t, Map.set(&2, String, m, k, v))

def Cfg.refusal.refs(refs: Bool, block: Bool) -> String:
  match refs:
    case True{}:
      "path-valued fields (`type: ref`)"
    case False{}:
      T.Text.pick(block, "configuration in the root's `prov:` block, or a history store or recycle bin", "")

def Cfg.parked.one(+s: String, rest: List<&2, String>) -> List<&2, String>:
  T.Text.keep(Bool.not(String.is_empty(s)), P.Path.normalize(s), rest)

def Cfg.parked.items(items: List<&2, C.Item>) -> List<&2, String>:
  match items:
    case Nil{}:
      Nil{}
    case C.IStr{_, s} <> t:
      Cfg.parked.one(String.trim(s), Cfg.parked.items(t))
    case C.INon{_} <> t:
      Cfg.parked.items(t)

# `out_of_scope`: directories, trimmed, a trailing `/` dropped.
def Cfg.parked(f: C.Field) -> List<&2, String>:
  match f:
    case C.FSeq{items}:
      Cfg.parked.items(items)
    case C.FStr{_}:
      Nil{}
    case C.FNone{}:
      Nil{}
    case C.FOther{}:
      Nil{}

def Cfg.refusal.go(vocab: Bool, refs: Bool, block: Bool) -> String:
  match vocab:
    case True{}:
      "a vocabulary of the workspace's own (`relations`, `spanning`)"
    case False{}:
      Cfg.refusal.refs(refs, block)

# What this port does not model, and so refuses rather than answer
# differently: a vocabulary of the workspace's own, path-valued fields, and
# configuration kept in the root's own `prov:` block.
def Cfg.refusal(+root_vals: List<&2, C.Val>, +cfg: List<&2, C.Val>) -> String:
  Cfg.refusal.go(
    C.Canon.has_top(cfg, "relations") || C.Canon.has_top(cfg, "spanning"),
    C.Canon.anywhere(cfg, "type", "ref"),
    C.Canon.has_top(root_vals, "prov") || C.Canon.has_top(root_vals, "history") || C.Canon.has_top(root_vals, "recycle_bin"))

# --- the rounds ----------------------------------------------------------------------

def Round.names(ns: List<&2, N.Named>) -> List<&2, String>:
  match ns:
    case Nil{}:
      Nil{}
    case N.Named{p, _} <> t:
      p <> Round.names(t)

def Round.dirs(ns: List<&2, N.Named>) -> List<&2, String>:
  match ns:
    case Nil{}:
      Nil{}
    case N.Named{_, d} <> t:
      List.append(&2, String, N.Census.draft.dirs(d), Round.dirs(t))

def Round.span.some(m: Maybe<&2, String>, rest: List<&2, String>) -> List<&2, String>:
  match m:
    case Some{p}:
      p <> rest
    case None{}:
      rest

def Round.span.keep(span: Bool, m: Maybe<&2, String>, rest: List<&2, String>) -> List<&2, String>:
  match span:
    case True{}:
      Round.span.some(m, rest)
    case False{}:
      rest

def Round.span(+dirs: Map<&2, N.Listing>, +ctx: L.Ctx, ps: List<&2, N.Plan>) -> List<&2, String>:
  match ps:
    case Nil{}:
      Nil{}
    case N.Plan{s, _, r, _} <> t:
      Round.span.keep(W.Walk.is_span(s), N.Census.reached(N.Census.hit(dirs, ctx, r)), Round.span(dirs, ctx, t))

def Round.draft.span(+dirs: Map<&2, N.Listing>, +ctx: L.Ctx, d: N.Draft) -> List<&2, String>:
  match d:
    case N.DUnread{_}:
      Nil{}
    case N.DDoc{_, ps, _, _, _, _}:
      Round.span(dirs, ctx, ps)

# Where the spanning relation leads from this round's documents.
def Round.next(+dirs: Map<&2, N.Listing>, +ctx: L.Ctx, ns: List<&2, N.Named>) -> List<&2, String>:
  match ns:
    case Nil{}:
      Nil{}
    case N.Named{_, d} <> t:
      List.append(&2, String, Round.draft.span(dirs, ctx, d), Round.next(dirs, ctx, t))

def Round.fresh(ps: List<&2, String>, +loaded: Set()) -> List<&2, String>:
  match ps:
    case Nil{}:
      Nil{}
    case +h <> t:
      T.Text.keep(Bool.not(W.Walk.has(loaded, h)), h, Round.fresh(t, loaded))

def Round.dedupe(ps: List<&2, String>) -> List<&2, String>:
  Set.to_list(Set.from_list(ps))

def Round.add(ps: List<&2, String>, s: Set()) -> Set():
  match ps:
    case Nil{}:
      s
    case h <> t:
      Round.add(t, Set.add(s, h))

# One round per unit of fuel: load what is new, plan it, list what it points
# into, and go on with where its spanning links lead. `fresh` is what this
# round loads — nothing loaded already, each path once.
def Round.run(
  fuel: Nat, +depth: Nat, +ctx: L.Ctx, +root: String, fresh: List<&2, String>, loaded: Set(),
  acc: List<&2, N.Named>, dirs: Map<&2, N.Listing>
) -> IO(Rounds):
  match fuel:
    case 0n:
      IO.pure(Rounds, Rounds{acc, dirs, List.is_empty(&2, String, fresh)})
    case 1n+f:
      match fresh:
        case Nil{}:
          IO.pure(Rounds, Rounds{acc, dirs, True{}})
        case +h <> +t:
          do IO<Rounds>:
            jobs : List<&2, N.Job> <- Io.jobs(root, h <> t)
            +named : List<&2, N.Named> = N.Census.plan.par(depth, ctx, List.length(&2, String, h <> t), jobs)
            +dirs2 : Map<&2, N.Listing> <- Io.lists(root, Round.dedupe(Round.dirs(named)), dirs)
            +loaded2 : Set() = Round.add(h <> t, loaded)
            Round.run(f, depth, ctx, root, Round.dedupe(Round.fresh(Round.next(dirs2, ctx, named), loaded2)), loaded2,
              List.append(&2, N.Named, acc, named), dirs2)

# --- after the rounds --------------------------------------------------------------

def Main.nodes(fs: List<&2, N.Fixed>, m: Map<&2, N.Node>) -> Map<&2, N.Node>:
  match fs:
    case Nil{}:
      m
    case N.Fixed{p, n} <> t:
      Main.nodes(t, Map.set(&2, N.Node, m, p, n))

def Main.bare(fs: List<&2, N.Fixed>) -> List<&2, N.Node>:
  match fs:
    case Nil{}:
      Nil{}
    case N.Fixed{_, n} <> t:
      n <> Main.bare(t)

def Main.st.stack(st: W.St) -> List<&2, String>:
  match st:
    case W.St{s, _, _, _, _}:
      s

def Main.st.facts(st: W.St) -> List<&2, W.Fact>:
  match st:
    case W.St{_, _, f, _, _}:
      List.reverse(&2, W.Fact, f)

def Main.st.census(st: W.St) -> List<&2, W.Entry>:
  match st:
    case W.St{_, _, _, c, _}:
      List.reverse(&2, W.Entry, c)

def Main.st.bodies(st: W.St) -> List<&2, String>:
  match st:
    case W.St{_, _, _, _, b}:
      b

def Main.lines(fs: List<&2, W.Fact>) -> List<&2, String>:
  match fs:
    case Nil{}:
      Nil{}
    case h <> t:
      W.Walk.show(h) <> Main.lines(t)

def Main.warnings.add(w: Bool, n: Nat) -> Nat:
  match w:
    case True{}:
      1n+n
    case False{}:
      n

def Main.warnings(fs: List<&2, W.Fact>) -> Nat:
  match fs:
    case Nil{}:
      0n
    case h <> t:
      Main.warnings.add(W.Walk.warning(h), Main.warnings(t))

def Main.count.go(+n: Nat, w: Nat) -> String:
  match w:
    case 0n:
      Nat.show(n) ++ " finding(s)"
    case 1n+k:
      Nat.show(n) ++ " finding(s), " ++ Nat.show(1n+k) ++ " warning(s)"

# `count_line`.
def Main.count(+n: Nat, w: Nat) -> String:
  Main.count.go(n, w)

def Main.print(ls: List<&2, String>) -> IO(Unit):
  match ls:
    case Nil{}:
      IO.pure(Unit, Unit{})
    case h <> t:
      do IO<Unit>:
        IO.print(h)
        Main.print(t)

def Main.finish.go(+fs: List<&2, W.Fact>, clean: Bool) -> IO(Unit):
  match clean:
    case True{}:
      IO.print_err("ok: no findings")
    case False{}:
      do IO<Unit>:
        Main.print(Main.lines(fs))
        IO.die(Unit, 1, Main.count(List.length(&2, W.Fact, fs), Main.warnings(fs)))

def Main.finish(+fs: List<&2, W.Fact>) -> IO(Unit):
  Main.finish.go(fs, List.is_empty(&2, W.Fact, fs))

def Main.refuse(why: String) -> IO(Unit):
  IO.die(Unit, 3, "prov-bend: not modelled: " ++ why)

def Main.arg(args: List<String>, n: Nat, d: String) -> String:
  Maybe.default(&1, String, List.get(&1, String, args, n), d)

# The lanes the per-document work is dealt across: `2^(depth+1)`.
def Main.depth() -> Nat:
  4n

def Main.check.census(
  +dir: String, +root: String, facts: List<&2, W.Fact>, +census: List<&2, W.Entry>,
  bodies: List<&2, String>, +listings: Map<&2, N.Listing>
) -> IO(Unit):
  do IO<Unit>:
    +reached : List<&2, String> = W.Walk.reachable(root, census, bodies)
    +rdirs : List<&2, String> = Round.dedupe(W.Walk.dirs(reached))
    more : Map<&2, N.Listing> <- Io.lists(dir, rdirs, listings)
    Main.finish(List.concat(&2, W.Fact, [facts, W.Walk.findings(census),
      W.Walk.orphans(rdirs, Set.from_list(reached), more)]))

def Main.check.walked(
  +dir: String, +root: String, stack: List<&2, String>, facts: List<&2, W.Fact>, +census: List<&2, W.Entry>,
  bodies: List<&2, String>, +listings: Map<&2, N.Listing>
) -> IO(Unit):
  match stack:
    case _ <> _:
      IO.die(Unit, 4, "prov-bend: the walk ran out of fuel")
    case Nil{}:
      Main.check.census(dir, root, facts, census, bodies, listings)

def Main.check.after(
  +dir: String, +root: String, +ctx: L.Ctx, +st: W.St, +listings: Map<&2, N.Listing>
) -> IO(Unit):
  Main.check.walked(dir, root, Main.st.stack(st), Main.st.facts(st), Main.st.census(st), Main.st.bodies(st), listings)

def Main.check.fixed(done: Bool, +dir: String, +root: String, +ctx: L.Ctx, +fixed: List<&2, N.Fixed>, +dirs: Map<&2, N.Listing>) -> IO(Unit):
  match done:
    case False{}:
      IO.die(Unit, 4, "prov-bend: the rounds ran out of fuel")
    case True{}:
      Main.check.after(dir, root, ctx,
        W.Walk.run(W.Walk.fuel(Main.bare(fixed)), Main.nodes(fixed, Map.new(&2, N.Node)), ctx, W.Walk.start(root)),
        dirs)

def Main.ctx(ctx: L.Ctx, titles: Map<&2, List<&2, String>>) -> L.Ctx:
  match ctx:
    case L.Ctx{ws, reg, _}:
      L.Ctx{ws, reg, titles}

def Main.check.resolved(done: Bool, +dir: String, +root: String, +ctx: L.Ctx, +named: List<&2, N.Named>, +dirs: Map<&2, N.Listing>) -> IO(Unit):
  Main.check.fixed(done, dir, root, ctx, N.Census.fix.par(Main.depth(), dirs, ctx, List.length(&2, N.Named, named), named), dirs)

def Main.unknown(ps: List<&2, String>, +known: Map<&2, N.Draft>) -> List<&2, String>:
  match ps:
    case Nil{}:
      Nil{}
    case +h <> t:
      T.Text.keep(Bool.not(Pair.snd(Map<&2, N.Draft>, Bool, Map.has(&2, N.Draft, known, h))), h, Main.unknown(t, known))

def Main.check.indexed(
  done: Bool, +dir: String, +root: String, +ctx: L.Ctx, +named: List<&2, N.Named>,
  +dirs2: Map<&2, N.Listing>, +sdirs: List<&2, String>
) -> IO(Unit):
  do IO<Unit>:
    +cands : List<&2, Ti.Cand> = Ti.Titles.cands(sdirs, dirs2)
    +known : Map<&2, N.Draft> = Ti.Titles.map(named, Map.new(&2, N.Draft))
    +more : List<&2, String> = Round.dedupe(Main.unknown(Ti.Titles.reads(cands), known))
    jobs : List<&2, N.Job> <- Io.jobs(dir, more)
    +extra : List<&2, N.Named> = N.Census.plan.par(Main.depth(), ctx, List.length(&2, String, more), jobs)
    Main.check.resolved(done, dir, root,
      Main.ctx(ctx, Ti.Titles.index(cands, Ti.Titles.map(extra, known), Map.new(&2, List<&2, String>))),
      named, dirs2)

def Main.check.treed(done: Bool, +dir: String, +root: String, +ctx: L.Ctx, +named: List<&2, N.Named>, t: Tree) -> IO(Unit):
  match t:
    case Tree{+ds, +all}:
      Main.check.indexed(done, dir, root, ctx, named, ds, all)

# A spanning alias defeats the bound, and the whole tree is scanned instead:
# every directory under the root, but a hidden or parked one.
def Main.check.scoped.go(
  full: Bool, done: Bool, +dir: String, +root: String, +ctx: L.Ctx, +parked: List<&2, String>,
  +named: List<&2, N.Named>, +dirs: Map<&2, N.Listing>, +sdirs: List<&2, String>
) -> IO(Unit):
  match full:
    case True{}:
      do IO<Unit>:
        t : Tree <- Io.tree(U32.to_nat(65536), dir, parked, [""], dirs, [])
        Main.check.treed(done, dir, root, ctx, named, t)
    case False{}:
      do IO<Unit>:
        +dirs2 : Map<&2, N.Listing> <- Io.lists(dir, sdirs, dirs)
        Main.check.indexed(done, dir, root, ctx, named, dirs2, sdirs)

def Main.check.scoped(
  done: Bool, +dir: String, +root: String, +ctx: L.Ctx, +parked: List<&2, String>, +named: List<&2, N.Named>,
  +dirs: Map<&2, N.Listing>, +sc: Ti.Scope
) -> IO(Unit):
  Main.check.scoped.go(Ti.Titles.full(sc), done, dir, root, ctx, parked, named, dirs, Round.dedupe(Ti.Titles.dirs(sc)))

# An alias somewhere: build the index first. The scope's directories are
# listed, every file the index may name and every sidecar that may shadow one
# is read, and the census is resolved against it.
def Main.check.titles(
  done: Bool, needed: Bool, +dir: String, +root: String, +ctx: L.Ctx, +parked: List<&2, String>,
  +named: List<&2, N.Named>, +dirs: Map<&2, N.Listing>
) -> IO(Unit):
  match needed:
    case False{}:
      Main.check.resolved(done, dir, root, ctx, named, dirs)
    case True{}:
      Main.check.scoped(done, dir, root, ctx, parked, named, dirs,
        Ti.Titles.walk(Ti.Titles.fuel(named), Ti.Titles.map(named, Map.new(&2, N.Draft)), parked, Ti.Titles.start(root)))

def Main.check.rounds(+dir: String, +root: String, +ctx: L.Ctx, +parked: List<&2, String>, r: Rounds) -> IO(Unit):
  match r:
    case Rounds{+named, +dirs, done}:
      Main.check.titles(done, Ti.Titles.needed(named), dir, root, ctx, parked, named, dirs)

def Main.check.why(ok: Bool, why: String, +dir: String, +root: String, +ctx: L.Ctx, +parked: List<&2, String>) -> IO(Unit):
  match ok:
    case False{}:
      Main.refuse(why)
    case True{}:
      do IO<Unit>:
        r : Rounds <- Round.run(U32.to_nat(65536), Main.depth(), ctx, dir, [root], Set.new(), [], Map.new(&2, N.Listing))
        Main.check.rounds(dir, root, ctx, parked, r)

def Main.check.go(+dir: String, +root: String, +ctx: L.Ctx, +parked: List<&2, String>, +why: String) -> IO(Unit):
  Main.check.why(String.is_empty(why), why, dir, root, ctx, parked)

def main() -> IO(Unit):
  do IO<Unit>:
    args : List<String> <- IO.args()
    +dir : String = Main.arg(args, 0n, ".")
    args2 : List<String> <- IO.args()
    +root : String = P.Path.normalize(Main.arg(args2, 1n, "README.md"))
    rj : List<&2, N.Job> <- Io.jobs(dir, [root])
    +rvals : List<&2, C.Val> = Cfg.vals(Io.first(rj))
    +cfg : List<&2, C.Val> <- Cfg.load(dir, Cfg.pointer(root, rvals, "config"))
    reg : List<&2, C.Val> <- Cfg.load(dir, Cfg.pointer(root, rvals, "registry"))
    +ctx : L.Ctx = L.Ctx{Cfg.ws(C.Canon.str(cfg, "workspace_id")), Cfg.registry(C.Canon.entries(reg, "registry"), Map.new(&2, String)), Map.new(&2, List<&2, String>)}
    Main.check.go(dir, root, ctx, Cfg.parked(C.Canon.get(cfg, "out_of_scope")), Cfg.refusal(rvals, cfg))
