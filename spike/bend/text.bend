import Base

# Strings, as the rest of the port needs them. Bend has no `if`, a `match`
# takes only a parameter or a pattern-bound variable, and a def may call only
# the defs above it — so every branch is a helper that matches a `Bool` it is
# handed, and a search that recurses computes its answer from the head it
# has just looked at, never from a copy of the tail. (A `+` tail walked by
# `String.starts_with` at every character is the tail made whole each time:
# historica's port measured that as a line's length squared.)

# `a` followed by `b`, when `c` says so; else `b`.

def Text.keep(c: Bool, a: String, b: List<&2, String>) -> List<&2, String>:
  match c:
    case True{}:
      a <> b
    case False{}:
      b

def Text.pick(c: Bool, a: String, b: String) -> String:
  match c:
    case True{}:
      a
    case False{}:
      b

# --- splitting at one character -----------------------------------------

# The text before the first `c`, and after it — or the whole, and `None`.
def Text.cut.put(h: Char, r: String & Maybe<&2, String>) -> String & Maybe<&2, String>:
  (a, b) = r
  (SCon{h, a}, b)

def Text.cut.at(+h: Char, t: String, hit: Bool, r: String & Maybe<&2, String>) ->
  String & Maybe<&2, String>:
  match hit:
    case True{}:
      ("", Some{t})
    case False{}:
      Text.cut.put(h, r)

def Text.cut(s: String, +c: Char) -> String & Maybe<&2, String>:
  match s:
    case SNil{}:
      ("", None{})
    case SCon{+h, +t}:
      Text.cut.at(h, t, Char.is_eq(h, c), Text.cut(t, c))

def Text.before(s: String, +c: Char) -> String:
  Pair.fst(String, Maybe<&2, String>, Text.cut(s, c))

def Text.after(s: String, +c: Char) -> Maybe<&2, String>:
  Pair.snd(String, Maybe<&2, String>, Text.cut(s, c))

# The text after the last `c`, or the whole when there is none.
def Text.after_last(s: String, +c: Char) -> String:
  String.reverse(Text.before(String.reverse(s), c))

def Text.has(s: String, +c: Char) -> Bool:
  match s:
    case SNil{}:
      False{}
    case SCon{h, t}:
      Char.is_eq(h, c) || Text.has(t, c)

def Text.unesc.code.e(+x: U32, rs: Bool) -> U32:
  match rs:
    case True{}:
      30
    case False{}:
      x

def Text.unesc.code.n(+x: U32, nl: Bool) -> U32:
  match nl:
    case True{}:
      10
    case False{}:
      Text.unesc.code.e(x, U32.is_eq(x, 101))

def Text.unesc.code.t(+x: U32, tab: Bool) -> U32:
  match tab:
    case True{}:
      9
    case False{}:
      Text.unesc.code.n(x, U32.is_eq(x, 110))

def Text.unesc.code(+x: U32) -> U32:
  Text.unesc.code.t(x, U32.is_eq(x, 116))

# --- escapes -------------------------------------------------------------

# The shim's escapes: `\\`, `\t`, `\n`, and `\e` for the batch separator.
# `esc` says the character before was an unpaired backslash.
def Text.unesc.one(c: Char) -> Char:
  Char.from_u32(Text.unesc.code(Char.to_u32(c)))

def Text.unesc.plain(h: Char, rest: String, slash: Bool) -> String:
  match slash:
    case True{}:
      rest
    case False{}:
      SCon{h, rest}

def Text.unesc.step(+h: Char, rest: String, esc: Bool, slash: Bool) -> String:
  match esc:
    case True{}:
      SCon{Text.unesc.one(h), rest}
    case False{}:
      Text.unesc.plain(h, rest, slash)

# Whether the character at each position is escaped is decided left to right,
# so the walk carries it in; the recursion is on the tail alone.
def Text.unesc.go(s: String, +esc: Bool) -> String:
  match s:
    case SNil{}:
      ""
    case SCon{+h, t}:
      +slash = Bool.and(Bool.not(esc), Char.is_eq(h, '\\'))
      Text.unesc.step(h, Text.unesc.go(t, slash), esc, slash)

def Text.unescape(s: String) -> String:
  Text.unesc.go(s, False{})

# --- numbers ---------------------------------------------------------------

def Text.nat(s: String) -> Nat:
  Maybe.default(&2, Nat, Nat.read(s), 0n)

def Text.u32(s: String) -> U32:
  Maybe.default(&2, U32, U32.read(s), 0)
