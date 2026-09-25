import Base
import ./link.bend as L
import ./census.bend as N
import ./walk.bend as W

# The claims, stated here and proven in PROOF.bend. Each is about the code
# the port runs, not a model of it: the defs named are the ones `main.bend`
# calls.

# --- arithmetic and lists ---------------------------------------------------------

law add_succ_r:
  for a: Nat
  for b: Nat
  {Nat.add(a, 1n+b) == 1n+Nat.add(a, b) : Nat}

law len_append:
  for xs: List<&2, N.Named>
  for ys: List<&2, N.Named>
  {List.length(&2, N.Named, List.append(&2, N.Named, xs, ys)) == Nat.add(List.length(&2, N.Named, xs), List.length(&2, N.Named, ys)) : Nat}

# --- the census ----------------------------------------------------------------------

# Resolving a document's links against the disk keeps every one of them: the
# census has an entry for each link the document declares, never fewer.
law census_total:
  for +dirs: Map<&2, N.Listing>
  for +ctx: L.Ctx
  for ps: List<&2, N.Plan>
  {List.length(&2, N.CLink, N.Census.links(dirs, ctx, ps)) == List.length(&2, N.Plan, ps) : Nat}

law broken_len:
  for b: Bool
  for s: String
  for site: N.Site
  for t: String
  for fs: List<&2, W.Fact>
  {List.length(&2, W.Fact, W.Walk.entry.broken(b, s, site, t, fs)) == W.Walk.faults.broken(b, List.length(&2, W.Fact, fs)) : Nat}

# The census's findings are exactly what its entries owe: a broken link that
# is not an `about` pointer, and a case mismatch, each reported once, and
# nothing else reported.
law findings_exact:
  for es: List<&2, W.Entry>
  {List.length(&2, W.Fact, W.Walk.findings(es)) == W.Walk.faults(es) : Nat}

# --- the parallel census ---------------------------------------------------------

# Splitting a list in two loses nothing and invents nothing.
law split_total:
  for n: Nat
  for xs: List<&2, N.Job>
  {N.Split.total(N.Split.go(n, xs)) == List.length(&2, N.Job, xs) : Nat}

law cons_total:
  for h: N.Job
  for r: List<&2, N.Job> & List<&2, N.Job>
  {N.Split.total(N.Split.cons(h, r)) == 1n+N.Split.total(r) : Nat}

law seq_total:
  for +ctx: L.Ctx
  for js: List<&2, N.Job>
  {List.length(&2, N.Named, N.Census.plan.seq(ctx, js)) == List.length(&2, N.Job, js) : Nat}

law tree_total:
  for +depth: Nat
  for +ctx: L.Ctx
  for +n: Nat
  for r: List<&2, N.Job> & List<&2, N.Job>
  {List.length(&2, N.Named, N.Census.plan.tree(depth, ctx, n, r)) == N.Split.total(r) : Nat}

# The fork-join census plans every document it is handed exactly once, at
# any depth, whatever `n` says: the lanes neither drop a document nor plan
# one twice.
law plan_par_total:
  for +depth: Nat
  for +ctx: L.Ctx
  for +n: Nat
  for +js: List<&2, N.Job>
  {List.length(&2, N.Named, N.Census.plan.par(depth, ctx, n, js)) == List.length(&2, N.Job, js) : Nat}

law len_append_fixed:
  for xs: List<&2, N.Fixed>
  for ys: List<&2, N.Fixed>
  {List.length(&2, N.Fixed, List.append(&2, N.Fixed, xs, ys)) == Nat.add(List.length(&2, N.Fixed, xs), List.length(&2, N.Fixed, ys)) : Nat}

law nsplit_total:
  for n: Nat
  for xs: List<&2, N.Named>
  {N.Split.ntotal(N.Split.ngo(n, xs)) == List.length(&2, N.Named, xs) : Nat}

law ncons_total:
  for h: N.Named
  for r: List<&2, N.Named> & List<&2, N.Named>
  {N.Split.ntotal(N.Split.ncons(h, r)) == 1n+N.Split.ntotal(r) : Nat}

law fix_seq_total:
  for +dirs: Map<&2, N.Listing>
  for +ctx: L.Ctx
  for ns: List<&2, N.Named>
  {List.length(&2, N.Fixed, N.Census.fix.seq(dirs, ctx, ns)) == List.length(&2, N.Named, ns) : Nat}

law fix_tree_total:
  for +depth: Nat
  for +dirs: Map<&2, N.Listing>
  for +ctx: L.Ctx
  for +n: Nat
  for r: List<&2, N.Named> & List<&2, N.Named>
  {List.length(&2, N.Fixed, N.Census.fix.tree(depth, dirs, ctx, n, r)) == N.Split.ntotal(r) : Nat}

# And the fork-join resolution resolves every planned document exactly once:
# the walk is replayed over all of them and no other.
law fix_par_total:
  for +depth: Nat
  for +dirs: Map<&2, N.Listing>
  for +ctx: L.Ctx
  for +n: Nat
  for +ns: List<&2, N.Named>
  {List.length(&2, N.Fixed, N.Census.fix.par(depth, dirs, ctx, n, ns)) == List.length(&2, N.Named, ns) : Nat}
