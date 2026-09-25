import Base
import ./text.bend as T

# Workspace-relative paths, as prov-graph's `link::resolve` and
# fs-transaction's `normalize` read them: a path is its components, `.` and
# empty components drop out, and `..` cancels the component before it only
# when that one is a name — a leading `..` stays, which is what lets
# `escapes` see a climb above the root.

# --- components ------------------------------------------------------------

def Path.up.go(h: String, t: List<&2, String>, dots: Bool) -> List<&2, String>:
  match dots:
    case True{}:
      ".." <> (h <> t)
    case False{}:
      t

def Path.up(stack: List<&2, String>) -> List<&2, String>:
  match stack:
    case Nil{}:
      [".."]
    case +h <> t:
      Path.up.go(h, t, String.eq(h, ".."))

def Path.push.name(seg: String, stack: List<&2, String>, up: Bool) -> List<&2, String>:
  match up:
    case True{}:
      Path.up(stack)
    case False{}:
      seg <> stack

def Path.push.go(+seg: String, stack: List<&2, String>, skip: Bool) -> List<&2, String>:
  match skip:
    case True{}:
      stack
    case False{}:
      Path.push.name(seg, stack, String.eq(seg, ".."))

def Path.push(+seg: String, stack: List<&2, String>) -> List<&2, String>:
  Path.push.go(seg, stack, String.is_empty(seg) || String.eq(seg, "."))

# The components of `segs` folded onto `stack`, which is kept reversed.
def Path.fold(segs: List<&2, String>, stack: List<&2, String>) -> List<&2, String>:
  match segs:
    case Nil{}:
      stack
    case h <> t:
      Path.fold(t, Path.push(h, stack))

def Path.parts(p: String) -> List<&2, String>:
  List.reverse(&2, String, Path.fold(String.split(p, '/'), []))

def Path.join(parts: List<&2, String>) -> String:
  String.join(parts, "/")

def Path.normalize(p: String) -> String:
  Path.join(Path.parts(p))

# --- a path's two halves ---------------------------------------------------

# Everything but the last component; empty for a name at the root.
def Path.parent(p: String) -> String:
  Path.join(List.reverse(&2, String, List.tail(&2, String, List.reverse(&2, String, Path.parts(p)))))

# The last component.
def Path.name(p: String) -> String:
  Maybe.default(&2, String, List.last(&2, String, Path.parts(p)), "")

def Path.child.go(dir: String, name: String, root: Bool) -> String:
  match root:
    case True{}:
      name
    case False{}:
      dir ++ "/" ++ name

def Path.child(+dir: String, +name: String) -> String:
  Path.child.go(dir, name, String.is_empty(dir))

# The extension of a name: the text after its last `.`, when the `.` is not
# the name's first character (Rust's `Path::extension`).
def Path.ext.go(+name: String, dotted: Bool) -> String:
  match dotted:
    case True{}:
      T.Text.after_last(name, '.')
    case False{}:
      ""

def Path.ext(+name: String) -> String:
  Path.ext.go(name, T.Text.has(String.drop(name, 1n), '.'))

# --- resolving a target ------------------------------------------------------

# A target written in the document at `doc`: a locator (`#…`, after a
# non-empty document part) is dropped; a leading `/` is from the workspace
# root; anything else is from `doc`'s directory.
def Path.resolve.go(doc: String, +t: String, abs: Bool) -> String:
  match abs:
    case True{}:
      Path.normalize(String.drop(t, 1n))
    case False{}:
      Path.normalize(Path.parent(doc) ++ "/" ++ t)

# `split_locator`'s document half: the text before the first `#`, unless that
# is empty — a target that is only a locator stays whole.
def Path.locatorless.go(+t: String, +doc: String) -> String:
  T.Text.pick(String.is_empty(doc), t, doc)

def Path.locatorless(+t: String) -> String:
  Path.locatorless.go(t, T.Text.before(t, '#'))

def Path.resolve(doc: String, target: String) -> String:
  +t = Path.locatorless(target)
  Path.resolve.go(doc, t, String.starts_with(t, "/"))

# Whether a normalized path climbs above the root.
def Path.escapes(+p: String) -> Bool:
  String.eq(p, "..") || String.starts_with(p, "../")

# The first components decide, unless they are equal. The rest is compared
# either way: a helper that recursed only on `EQ` would be mutual recursion.
def Path.cmp.then(c: Cmp, rest: Cmp) -> Cmp:
  match c:
    case LT{}:
      LT{}
    case GT{}:
      GT{}
    case EQ{}:
      rest

# --- order -----------------------------------------------------------------

# `PathBuf`'s order: component by component, each by its bytes — so
# `a/b.md` sorts before `a-c.md`, which comparing the strings would not.
def Path.cmp.parts(a: List<&2, String>, b: List<&2, String>) -> Cmp:
  match a b:
    case Nil{} Nil{}:
      EQ{}
    case Nil{} Con{_, _}:
      LT{}
    case Con{_, _} Nil{}:
      GT{}
    case Con{x, xs} Con{y, ys}:
      Path.cmp.then(String.order(x, y), Path.cmp.parts(xs, ys))

def Path.le(a: String, b: String) -> Bool:
  Cmp.is_le(Path.cmp.parts(Path.parts(a), Path.parts(b)))
