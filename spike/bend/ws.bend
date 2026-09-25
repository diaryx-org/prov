import Base

# What the workspace holds, asked of the host through the adapter in `ffi/`.
# `RUNTIME.md` says why the boundary sits here. Neither effect decides
# anything: which directories are listed, which documents are read, and what
# every answer means are the Bend side's.
#
# Each takes a batch — a round's worth of questions — because every crossing
# is a round trip through the event loop to a helper thread, and thousands
# of them one at a time were most of what a check cost.

# What directories hold: the root, then a directory a line (empty for the
# root). Back, two strings for each: a head — the directory, `1` when it was
# listed, tab-separated — and its entries a line each: kind (`f`, `d`, `l`
# for a link never followed, `o`), a tab, name.
def Ws.dirs(query: String) -> IO(Result<&1, &1, U32 & String, List<&2, String>>):
  import "./ffi/ws_dirs.c"
  import "./ffi/ws_dirs.js"

# What documents say: the root, then a document a line. Back, two strings for
# each: a head — the path, then `1`, or `0` and the words prov's own
# `Unreadable` finding would carry — and the records `canon.bend` reads.
# Each document's records are their own string, so the census can read each
# on its own lane.
def Ws.loads(query: String) -> IO(Result<&1, &1, U32 & String, List<&2, String>>):
  import "./ffi/ws_loads.c"
  import "./ffi/ws_loads.js"
