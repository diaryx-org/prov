import Base
import ./text.bend as T
import ./path.bend as P
import ./id.bend as I

# A reference as prov-graph's `link.rs` reads one: the three spellings —
# `[[target|label]]`, `[label](target)` (with `<…>` around a target that
# holds spaces, and a target's own parentheses balanced), and a bare target
# — and what the target names: somewhere off the workspace, a place in the
# same document, a document by id (here or in another workspace), a
# document by alias, or a path.

type Link is Data:
  Link{label: Maybe<&2, String>, target: String, wiki: Bool}

# A `[label](target)` split in two.
type Md is Data:
  Md{label: String, target: String}

# What a target names before anything on disk is asked.
type Res is Data:
  ROff{}
  RSame{}
  RForeign{}
  RMalformed{}
  RDangling{}
  RAlias{name: String, path: String}
  RId{to: String}
  RPath{path: String}

# What resolution needs to know about the workspace: its own name, the
# registry's ids, and the title index — every name a document answers to (its
# file stem, its `title`) and the documents answering to it. The index is
# empty until `titles.bend` has built it; a plan made without it keeps an
# alias's fallback path, which is what an unknown name falls through to.
type Ctx is Data:
  Ctx{ws: String, registry: Map<&2, String>, titles: Map<&2, List<&2, String>>}

# --- the three spellings ----------------------------------------------------

def Link.mcons(h: Char, r: Maybe<&2, String>) -> Maybe<&2, String>:
  match r:
    case Some{t}:
      Some{SCon{h, t}}
    case None{}:
      None{}

def Link.mpick(stop: Bool, now: Maybe<&2, String>, later: Maybe<&2, String>) -> Maybe<&2, String>:
  match stop:
    case True{}:
      now
    case False{}:
      later

def Link.depth.close(+depth: Nat, close: Bool) -> Nat:
  match close:
    case True{}:
      Nat.sub(depth, 1n)
    case False{}:
      depth

def Link.depth.next(+h: Char, +depth: Nat, open: Bool) -> Nat:
  match open:
    case True{}:
      1n+depth
    case False{}:
      Link.depth.close(depth, Char.is_eq(h, ')'))

# `find_closing_paren`: the text before the `)` that closes at depth zero,
# every `(` opening one more level; `None` when nothing closes.
def Link.paren(s: String, +depth: Nat) -> Maybe<&2, String>:
  match s:
    case SNil{}:
      None{}
    case SCon{+h, t}:
      Link.mpick(
        Char.is_eq(h, ')') && Nat.is_eq(depth, 0n),
        Some{""},
        Link.mcons(h, Link.paren(t, Link.depth.next(h, depth, Char.is_eq(h, '(')))))

# `](<target>)`: the `>` must be followed at once by `)`.
def Link.angle.go(inner: Maybe<&2, String>, rest: Maybe<&2, String>) -> Maybe<&2, String>:
  match rest:
    case Some{+r}:
      Link.mpick(String.starts_with(r, ")"), inner, None{})
    case None{}:
      None{}

def Link.angle(+s: String) -> Maybe<&2, String>:
  Link.angle.go(Some{T.Text.before(s, '>')}, T.Text.after(s, '>'))

def Link.url(+after: String, angled: Bool) -> Maybe<&2, String>:
  match angled:
    case True{}:
      Link.angle(String.drop(after, 1n))
    case False{}:
      Link.paren(after, 0n)

def Link.md.url(label: String, url: Maybe<&2, String>) -> Maybe<&2, Md>:
  match url:
    case Some{t}:
      Some{Md{label, t}}
    case None{}:
      None{}

def Link.md.opened(label: String, +a: String, opened: Bool) -> Maybe<&2, Md>:
  match opened:
    case True{}:
      Link.md.url(label, Link.url(String.drop(a, 1n), String.starts_with(a, "(<")))
    case False{}:
      None{}

def Link.md.after(label: String, after: Maybe<&2, String>) -> Maybe<&2, Md>:
  match after:
    case Some{+a}:
      Link.md.opened(label, a, String.starts_with(a, "("))
    case None{}:
      None{}

# `split_markdown_link`: the label runs from the `[` to the first `]`, which
# must be followed at once by `(`.
def Link.md.go(+raw: String, bracket: Bool) -> Maybe<&2, Md>:
  match bracket:
    case True{}:
      Link.md.after(T.Text.before(String.drop(raw, 1n), ']'), T.Text.after(String.drop(raw, 1n), ']'))
    case False{}:
      None{}

def Link.md(+raw: String) -> Maybe<&2, Md>:
  Link.md.go(raw, String.starts_with(raw, "["))

def Link.bare.or(+raw: String, md: Maybe<&2, Md>) -> Link:
  match md:
    case Some{Md{l, t}}:
      Link{Some{l}, t, False{}}
    case None{}:
      Link{None{}, raw, False{}}

def Link.wiki.label(+inner: String, label: Maybe<&2, String>) -> Link:
  match label:
    case Some{l}:
      Link{Some{String.trim(l)}, String.trim(T.Text.before(inner, '|')), True{}}
    case None{}:
      Link{None{}, String.trim(inner), True{}}

def Link.wiki(+raw: String) -> Link:
  +inner = String.take(String.drop(raw, 2n), Nat.sub(String.length(raw), 4n))
  Link.wiki.label(inner, T.Text.after(inner, '|'))

def Link.parse.go(+raw: String, wiki: Bool) -> Link:
  match wiki:
    case True{}:
      Link.wiki(raw)
    case False{}:
      Link.bare.or(raw, Link.md(raw))

# `Link::parse`.
def Link.parse(raw: String) -> Link:
  +r = String.trim(raw)
  Link.parse.go(r, String.starts_with(r, "[[") && String.ends_with(r, "]]") && Nat.is_ge(String.length(r), 4n))

def Link.target(l: Link) -> String:
  match l:
    case Link{_, t, _}:
      t

def Link.label(l: Link) -> Maybe<&2, String>:
  match l:
    case Link{m, _, _}:
      m

# --- what a target names -----------------------------------------------------

def Link.is_external(+t: String) -> Bool:
  String.contains(t, "://") || String.starts_with(t, "mailto:")

def Link.addressed.go(+t: String, ext: Bool) -> String:
  match ext:
    case True{}:
      t
    case False{}:
      P.Path.locatorless(t)

def Link.addressed(+t: String) -> String:
  Link.addressed.go(t, Link.is_external(t))

# `is_alias_shaped`: a bare name — nothing empty, no separator, no extension.
def Link.alias_shaped(+t: String) -> Bool:
  Bool.not(String.is_empty(t)) && Bool.not(T.Text.has(t, '/')) && Bool.not(T.Text.has(t, '\\'))
    && Bool.not(T.Text.has(String.drop(t, 1n), '.'))

def Link.scheme.legacy(+t: String, old: Bool) -> Maybe<&2, String>:
  match old:
    case True{}:
      Some{String.drop(t, 9n)}
    case False{}:
      None{}

# The `id:` (or legacy `colophon:`) body of an addressed target.
def Link.scheme.go(+t: String, id: Bool) -> Maybe<&2, String>:
  match id:
    case True{}:
      Some{String.drop(t, 3n)}
    case False{}:
      Link.scheme.legacy(t, String.starts_with(t, "colophon:"))

def Link.scheme(+t: String) -> Maybe<&2, String>:
  Link.scheme.go(t, String.starts_with(t, "id:"))

def Link.lookup.hit(p: String, none: Bool) -> Res:
  match none:
    case True{}:
      RDangling{}
    case False{}:
      RId{P.Path.normalize(p)}

def Link.lookup.go(+p: String) -> Res:
  Link.lookup.hit(p, String.is_empty(p))

def Link.lookup.ok(ok: Bool, +ctx_reg: Map<&2, String>, id: String) -> Res:
  match ok:
    case True{}:
      Link.lookup.go(Pair.snd(Map<&2, String>, String, Map.get(String, "", ctx_reg, id)))
    case False{}:
      RMalformed{}

# A local id: malformed unless it verifies, and then the registry's.
def Link.lookup(+ctx_reg: Map<&2, String>, +id: String) -> Res:
  Link.lookup.ok(I.Id.verify(id), ctx_reg, id)

def Link.foreign.go(+reg: Map<&2, String>, id: String, local: Bool) -> Res:
  match local:
    case True{}:
      Link.lookup(reg, id)
    case False{}:
      RForeign{}

# `id:<ws>/<id>` names this workspace only when `ws` is its own name.
def Link.foreign(+reg: Map<&2, String>, +ws: String, +qual: String, id: String) -> Res:
  Link.foreign.go(reg, id, Bool.not(String.is_empty(ws)) && String.eq(qual, ws))

def Link.id.local(+reg: Map<&2, String>, body: String, empty: Bool) -> Res:
  match empty:
    case True{}:
      RMalformed{}
    case False{}:
      Link.lookup(reg, body)

def Link.id.qualified(+reg: Map<&2, String>, +ws: String, +qual: String, id: String, bad: Bool) -> Res:
  match bad:
    case True{}:
      RMalformed{}
    case False{}:
      Link.foreign(reg, ws, qual, id)

def Link.id.split(+reg: Map<&2, String>, +ws: String, +body: String, rest: Maybe<&2, String>) -> Res:
  match rest:
    case None{}:
      Link.id.local(reg, body, String.is_empty(body))
    case Some{+id}:
      Link.id.qualified(reg, ws, T.Text.before(body, '/'), id,
        String.is_empty(T.Text.before(body, '/')) || String.is_empty(id) || T.Text.has(id, '/'))

def Link.id(+reg: Map<&2, String>, +ws: String, +body: String) -> Res:
  Link.id.split(reg, ws, body, T.Text.after(body, '/'))

def Link.path(+doc: String, +t: String, alias: Bool) -> Res:
  match alias:
    case True{}:
      RAlias{Link.addressed(t), P.Path.resolve(doc, t)}
    case False{}:
      RPath{P.Path.resolve(doc, t)}

def Link.named(+reg: Map<&2, String>, +ws: String, +doc: String, +t: String, id: Maybe<&2, String>) -> Res:
  match id:
    case Some{body}:
      Link.id(reg, ws, body)
    case None{}:
      Link.path(doc, t, Link.alias_shaped(Link.addressed(t)))

def Link.local(+reg: Map<&2, String>, +ws: String, +doc: String, +t: String, same: Bool) -> Res:
  match same:
    case True{}:
      RSame{}
    case False{}:
      Link.named(reg, ws, doc, t, Link.scheme(Link.addressed(t)))

def Link.resolve.go(+reg: Map<&2, String>, +ws: String, +doc: String, +t: String, off: Bool) -> Res:
  match off:
    case True{}:
      ROff{}
    case False{}:
      Link.local(reg, ws, doc, t, String.starts_with(t, "#"))

# `resolve_forward`, up to the disk: what the target `t`, written in `doc`,
# names. A path is only lexical here; whether it is on disk is `Census`'s.
def Link.resolve(ctx: Ctx, +doc: String, +t: String) -> Res:
  match ctx:
    case Ctx{+ws, +reg, _}:
      Link.resolve.go(reg, ws, doc, t, Link.is_external(t))

# The documents answering to a name.
def Link.titled(+titles: Map<&2, List<&2, String>>, name: String) -> List<&2, String>:
  Pair.snd(Map<&2, List<&2, String>>, List<&2, String>, Map.get(List<&2, String>, [], titles, name))

def Link.named.one(h: String, one: Bool) -> Maybe<&2, String>:
  match one:
    case True{}:
      Some{P.Path.normalize(h)}
    case False{}:
      None{}

# `TitleIndex::resolve` as `resolve_link_with` takes it: one document is that
# document; several are ambiguous, and land nowhere; none falls through to
# the path.
def Link.named.lands(ps: List<&2, String>, path: String) -> Maybe<&2, String>:
  match ps:
    case Nil{}:
      Some{path}
    case h <> t:
      Link.named.one(h, List.is_empty(&2, String, t))

# `resolve_link_with`'s `Target::Path`: where a target lands without asking
# the disk — what the inverse check compares against.
def Link.lands.res(r: Res, +titles: Map<&2, List<&2, String>>) -> Maybe<&2, String>:
  match r:
    case RId{to}:
      Some{to}
    case RPath{p}:
      Some{p}
    case RAlias{name, path}:
      Link.named.lands(Link.titled(titles, name), path)
    case ROff{}:
      None{}
    case RSame{}:
      None{}
    case RForeign{}:
      None{}
    case RMalformed{}:
      None{}
    case RDangling{}:
      None{}

def Link.lands.go(r: Res, ctx: Ctx) -> Maybe<&2, String>:
  match ctx:
    case Ctx{_, _, +titles}:
      Link.lands.res(r, titles)

def Link.lands(+ctx: Ctx, +doc: String, +t: String) -> Maybe<&2, String>:
  Link.lands.go(Link.resolve(ctx, doc, t), ctx)
