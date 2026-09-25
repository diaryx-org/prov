import Base
import ./text.bend as T

# The document the shim describes — `ffi/src/lib.rs`'s `render` is the
# writer — read back: whether it has metadata, its metadata tree as flat
# pre-order records, and the links prov's body scanner found in its prose.
# The parse of YAML, JSON, TOML, fig and the prose grammars is the host's;
# everything a document is then taken to say is decided from these records.

type Val is Data:
  Val{depth: Nat, key: String, tag: String, text: String}

type BodyLink is Data:
  BodyLink{start: U32, end: U32, image: Bool, label: Maybe<&2, String>, target: String}

type Doc is Data:
  Doc{meta: Bool, vals: List<&2, Val>, links: List<&2, BodyLink>}

# One item of a relation's list: its place as written, and its text when it
# is a string. A non-string item yields no link but still holds its place.
type Item is Data:
  IStr{index: Nat, text: String}
  INon{index: Nat}

# A top-level metadata field, as far as links care.
type Field is Data:
  FNone{}
  FStr{text: String}
  FSeq{items: List<&2, Item>}
  FOther{}

type KV is Data:
  KV{key: String, val: String}

type Acc is Data:
  Acc{meta: Bool, vals: List<&2, Val>, links: List<&2, BodyLink>}

# --- reading the records ------------------------------------------------------

def Canon.field(+fs: List<&2, String>, n: Nat) -> String:
  T.Text.unescape(Maybe.default(&2, String, List.get(&2, String, fs, n), ""))

def Canon.label.go(label: String, yes: Bool) -> Maybe<&2, String>:
  match yes:
    case True{}:
      Some{label}
    case False{}:
      None{}

def Canon.label(+present: String, label: String) -> Maybe<&2, String>:
  Canon.label.go(label, String.eq(present, "1"))

def Canon.val(+fs: List<&2, String>) -> Val:
  Val{T.Text.nat(Canon.field(fs, 1n)), Canon.field(fs, 2n), Canon.field(fs, 3n), Canon.field(fs, 4n)}

def Canon.link(+fs: List<&2, String>) -> BodyLink:
  BodyLink{
    T.Text.u32(Canon.field(fs, 1n)), T.Text.u32(Canon.field(fs, 2n)),
    String.eq(Canon.field(fs, 3n), "1"),
    Canon.label(Canon.field(fs, 4n), Canon.field(fs, 5n)),
    Canon.field(fs, 6n)}

def Canon.add.link(yes: Bool, acc: Acc, +fs: List<&2, String>) -> Acc:
  match yes:
    case True{}:
      match acc:
        case Acc{m, vs, ls}:
          Acc{m, vs, Canon.link(fs) <> ls}
    case False{}:
      acc

def Canon.add.val(yes: Bool, acc: Acc, +fs: List<&2, String>, +tag: String) -> Acc:
  match yes:
    case True{}:
      match acc:
        case Acc{m, vs, ls}:
          Acc{m, Canon.val(fs) <> vs, ls}
    case False{}:
      Canon.add.link(String.eq(tag, "L"), acc, fs)

def Canon.add.head(yes: Bool, acc: Acc, +fs: List<&2, String>, +tag: String) -> Acc:
  match yes:
    case True{}:
      match acc:
        case Acc{_, vs, ls}:
          Acc{String.eq(Canon.field(fs, 1n), "1"), vs, ls}
    case False{}:
      Canon.add.val(String.eq(tag, "V"), acc, fs, tag)

def Canon.line(acc: Acc, line: String) -> Acc:
  +fs = String.split(line, '\t')
  +tag = Canon.field(fs, 0n)
  Canon.add.head(String.eq(tag, "H"), acc, fs, tag)

def Canon.lines(ls: List<&2, String>, acc: Acc) -> Acc:
  match ls:
    case Nil{}:
      acc
    case h <> t:
      Canon.lines(t, Canon.line(acc, h))

def Canon.fin(acc: Acc) -> Doc:
  match acc:
    case Acc{m, vs, ls}:
      Doc{m, List.reverse(&2, Val, vs), List.reverse(&2, BodyLink, ls)}

# The shim's answer for one document.
def Canon.parse(text: String) -> Doc:
  Canon.fin(Canon.lines(String.lines(text), Acc{False{}, [], []}))

def Canon.has_meta(d: Doc) -> Bool:
  match d:
    case Doc{m, _, _}:
      m

def Canon.vals(d: Doc) -> List<&2, Val>:
  match d:
    case Doc{_, vs, _}:
      vs

def Canon.links(d: Doc) -> List<&2, BodyLink>:
  match d:
    case Doc{_, _, ls}:
      ls

# --- the fields -----------------------------------------------------------------

def Canon.is_top(+key: String, v: Val) -> Bool:
  match v:
    case Val{d, k, _, _}:
      Nat.is_eq(d, 0n) && String.eq(k, key)

def Canon.vpick(here: Bool, now: List<&2, Val>, later: List<&2, Val>) -> List<&2, Val>:
  match here:
    case True{}:
      now
    case False{}:
      later

# The records from the top-level `key` on, or none.
def Canon.from(vals: List<&2, Val>, +key: String) -> List<&2, Val>:
  match vals:
    case Nil{}:
      Nil{}
    case +h <> +t:
      Canon.vpick(Canon.is_top(key, h), h <> t, Canon.from(t, key))

def Canon.item.go(i: Nat, text: String, str: Bool) -> Item:
  match str:
    case True{}:
      IStr{i, text}
    case False{}:
      INon{i}

def Canon.item(v: Val) -> Item:
  match v:
    case Val{_, k, +tag, text}:
      Canon.item.go(T.Text.nat(k), text, String.eq(tag, "s"))

def Canon.depth(v: Val) -> Nat:
  match v:
    case Val{d, _, _, _}:
      d

def Canon.kid.go(v: Val, rest: List<&2, Item>, one: Bool) -> List<&2, Item>:
  match one:
    case True{}:
      Canon.item(v) <> rest
    case False{}:
      rest

def Canon.kid(v: Val, rest: List<&2, Item>, +d: Nat) -> List<&2, Item>:
  Canon.kid.go(v, rest, Nat.is_eq(d, 1n))

def Canon.ipick(stop: Bool, rest: List<&2, Item>) -> List<&2, Item>:
  match stop:
    case True{}:
      Nil{}
    case False{}:
      rest

# The direct children of a field: the depth-one records until the next
# top-level one.
def Canon.kids(vals: List<&2, Val>) -> List<&2, Item>:
  match vals:
    case Nil{}:
      Nil{}
    case +h <> t:
      Canon.ipick(Nat.is_eq(Canon.depth(h), 0n), Canon.kid(h, Canon.kids(t), Canon.depth(h)))

def Canon.shape.q(rest: List<&2, Val>, q: Bool) -> Field:
  match q:
    case True{}:
      FSeq{Canon.kids(rest)}
    case False{}:
      FOther{}

def Canon.shape.s(+tag: String, text: String, rest: List<&2, Val>, s: Bool) -> Field:
  match s:
    case True{}:
      FStr{text}
    case False{}:
      Canon.shape.q(rest, String.eq(tag, "q"))

def Canon.shape(+tag: String, text: String, rest: List<&2, Val>) -> Field:
  Canon.shape.s(tag, text, rest, String.eq(tag, "s"))

def Canon.get.go(found: List<&2, Val>) -> Field:
  match found:
    case Nil{}:
      FNone{}
    case Val{_, _, tag, text} <> rest:
      Canon.shape(tag, text, rest)

# The top-level field `key`.
def Canon.get(vals: List<&2, Val>, +key: String) -> Field:
  Canon.get.go(Canon.from(vals, key))

def Canon.str.go(f: Field) -> Maybe<&2, String>:
  match f:
    case FStr{s}:
      Some{s}
    case FNone{}:
      None{}
    case FSeq{_}:
      None{}
    case FOther{}:
      None{}

# A top-level string field.
def Canon.str(vals: List<&2, Val>, +key: String) -> Maybe<&2, String>:
  Canon.str.go(Canon.get(vals, key))

def Canon.kv.go(k: String, text: String, rest: List<&2, KV>, keep: Bool) -> List<&2, KV>:
  match keep:
    case True{}:
      KV{k, text} <> rest
    case False{}:
      rest

def Canon.kv.put(v: Val, rest: List<&2, KV>) -> List<&2, KV>:
  match v:
    case Val{+d, k, +tag, text}:
      Canon.kv.go(k, text, rest, Nat.is_eq(d, 1n) && String.eq(tag, "s"))

def Canon.kpick(stop: Bool, rest: List<&2, KV>) -> List<&2, KV>:
  match stop:
    case True{}:
      Nil{}
    case False{}:
      rest

def Canon.kvs(vals: List<&2, Val>) -> List<&2, KV>:
  match vals:
    case Nil{}:
      Nil{}
    case +h <> t:
      Canon.kpick(Nat.is_eq(Canon.depth(h), 0n), Canon.kv.put(h, Canon.kvs(t)))

def Canon.entries.go(found: List<&2, Val>) -> List<&2, KV>:
  match found:
    case Nil{}:
      Nil{}
    case _ <> rest:
      Canon.kvs(rest)

# The string entries of a top-level mapping: the registry's ids and paths.
def Canon.entries(vals: List<&2, Val>, +key: String) -> List<&2, KV>:
  Canon.entries.go(Canon.from(vals, key))

# Whether any record anywhere is `key: text` — how the port notices config
# it does not model and refuses rather than answer differently.
def Canon.anywhere(vals: List<&2, Val>, +key: String, +text: String) -> Bool:
  match vals:
    case Nil{}:
      False{}
    case Val{_, k, _, x} <> t:
      (String.eq(k, key) && String.eq(x, text)) || Canon.anywhere(t, key, text)

# Whether the top-level `key` is a scalar tagged `tag` reading `text` — a
# boolean `attachment: true`, say.
def Canon.anywhere_top(vals: List<&2, Val>, +key: String, +tag: String, +text: String) -> Bool:
  match vals:
    case Nil{}:
      False{}
    case Val{d, k, g, x} <> t:
      (Nat.is_eq(d, 0n) && String.eq(k, key) && String.eq(g, tag) && String.eq(x, text))
        || Canon.anywhere_top(t, key, tag, text)

def Canon.has_top(vals: List<&2, Val>, +key: String) -> Bool:
  Bool.not(List.is_empty(&2, Val, Canon.from(vals, key)))
