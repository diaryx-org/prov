import Base

# prov's opaque ids: six symbols of the NOID extended-digit alphabet and a
# check character — the alphabet's symbol at the sum of each symbol's
# ordinal times its position, modulo the alphabet's size. moid's `validate`,
# which `identity::verify` is: an id of the wrong length, with a symbol
# outside the alphabet, or with the wrong check character is malformed, and
# is never looked up.

def Id.alphabet() -> String:
  "0123456789bcdfghjkmnpqrstvwxz"

def Id.radix() -> U32:
  29

def Id.ord.at(hit: Bool, +i: U32, rest: Maybe<&2, U32>) -> Maybe<&2, U32>:
  match hit:
    case True{}:
      Some{i}
    case False{}:
      rest

def Id.ord.go(cs: String, +c: Char, +i: U32) -> Maybe<&2, U32>:
  match cs:
    case SNil{}:
      None{}
    case SCon{h, t}:
      Id.ord.at(Char.is_eq(h, c), i, Id.ord.go(t, c, (i + 1 : U32)))

# A symbol's place in the alphabet.
def Id.ord(+c: Char) -> Maybe<&2, U32>:
  Id.ord.go(Id.alphabet(), c, 0)

def Id.symbol(n: U32) -> Char:
  Maybe.default(&2, Char, String.get(Id.alphabet(), U32.to_nat(n)), '0')

# The weighted sum of a body's symbols, the first weighing one.
def Id.sum(s: String, +i: U32) -> U32:
  match s:
    case SNil{}:
      0
    case SCon{h, t}:
      ((Maybe.default(&2, U32, Id.ord(h), 0) * i) + Id.sum(t, (i + 1 : U32)) : U32)

# The check character a body earns.
def Id.check(s: String) -> Char:
  Id.symbol(U32.mod(Id.sum(s, 1), Id.radix()))

def Id.known(s: String) -> Bool:
  match s:
    case SNil{}:
      True{}
    case SCon{h, t}:
      Maybe.is_some(&2, U32, Id.ord(h)) && Id.known(t)

def Id.last(+s: String) -> Char:
  Maybe.default(&2, Char, String.get(s, 6n), ' ')

def Id.verify(+s: String) -> Bool:
  Nat.is_eq(String.length(s), 7n) && Id.known(s) && Char.is_eq(Id.check(String.take(s, 6n)), Id.last(s))
