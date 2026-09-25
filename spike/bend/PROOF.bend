import Base
import ./link.bend as L
import ./census.bend as N
import ./walk.bend as W
import ./LAWS.bend as Laws

def Laws.add_succ_r(a, b):
  match a:
    case 0n:
      {==}
    case 1n+p:
      %Laws.add_succ_r(p, b) : {1n+Nat.add(p, 1n+b) == 1n+_ : Nat}
      {==}

def Laws.len_append(xs, ys):
  match xs:
    case Nil{}:
      {==}
    case h <> t:
      %Laws.len_append(t, ys) : {1n+List.length(&2, N.Named, List.append(&2, N.Named, t, ys)) == 1n+_ : Nat}
      {==}

def Laws.census_total(dirs, ctx, ps):
  match ps:
    case Nil{}:
      {==}
    case h <> t:
      %Laws.census_total(dirs, ctx, t) : {1n+List.length(&2, N.CLink, N.Census.links(dirs, ctx, t)) == 1n+_ : Nat}
      {==}

def Laws.broken_len(b, s, site, t, fs):
  match b:
    case True{}:
      {==}
    case False{}:
      {==}

def Laws.findings_exact(es):
  match es:
    case Nil{}:
      {==}
    case W.Entry{+s, +site, +t, +h} <> +rest:
      match h:
        case N.HBroken{}:
          %Laws.findings_exact(rest) : {List.length(&2, W.Fact, W.Walk.entry.broken(W.Walk.is_about(site), s, site, t, W.Walk.findings(rest))) == W.Walk.faults.broken(W.Walk.is_about(site), _) : Nat}
          Laws.broken_len(W.Walk.is_about(site), s, site, t, W.Walk.findings(rest))
        case N.HCase{g, a}:
          %Laws.findings_exact(rest) : {1n+List.length(&2, W.Fact, W.Walk.findings(rest)) == 1n+_ : Nat}
          {==}
        case N.HPath{p}:
          Laws.findings_exact(rest)
        case N.HId{to}:
          Laws.findings_exact(rest)
        case N.HNone{}:
          Laws.findings_exact(rest)

def Laws.cons_total(h, r):
  match r:
    case (a, b):
      {==}

def Laws.split_total(n, xs):
  match n:
    case 0n:
      {==}
    case 1n+(+k):
      match xs:
        case Nil{}:
          {==}
        case h <> +t:
          %Laws.split_total(k, t) : {N.Split.total(N.Split.cons(h, N.Split.go(k, t))) == 1n+_ : Nat}
          Laws.cons_total(h, N.Split.go(k, t))

def Laws.seq_total(ctx, js):
  match js:
    case Nil{}:
      {==}
    case N.Job{p, l} <> t:
      %Laws.seq_total(ctx, t) : {1n+List.length(&2, N.Named, N.Census.plan.seq(ctx, t)) == 1n+_ : Nat}
      {==}

def Laws.tree_total(depth, ctx, n, r):
  match depth:
    case 0n:
      match r:
        case (+xs, +ys):
          %Laws.seq_total(ctx, xs) : {List.length(&2, N.Named, List.append(&2, N.Named, N.Census.plan.seq(ctx, xs), N.Census.plan.seq(ctx, ys))) == Nat.add(_, List.length(&2, N.Job, ys)) : Nat}
          %Laws.seq_total(ctx, ys) : {List.length(&2, N.Named, List.append(&2, N.Named, N.Census.plan.seq(ctx, xs), N.Census.plan.seq(ctx, ys))) == Nat.add(List.length(&2, N.Named, N.Census.plan.seq(ctx, xs)), _) : Nat}
          Laws.len_append(N.Census.plan.seq(ctx, xs), N.Census.plan.seq(ctx, ys))
    case 1n+d:
      match r:
        case (+xs, +ys):
          %Laws.split_total(Nat.div(n, 4n), xs) : {List.length(&2, N.Named, List.append(&2, N.Named, N.Census.plan.tree(d, ctx, Nat.div(n, 2n), N.Split.go(Nat.div(n, 4n), xs)), N.Census.plan.tree(d, ctx, Nat.div(n, 2n), N.Split.go(Nat.div(n, 4n), ys)))) == Nat.add(_, List.length(&2, N.Job, ys)) : Nat}
          %Laws.tree_total(d, ctx, Nat.div(n, 2n), N.Split.go(Nat.div(n, 4n), xs)) : {List.length(&2, N.Named, List.append(&2, N.Named, N.Census.plan.tree(d, ctx, Nat.div(n, 2n), N.Split.go(Nat.div(n, 4n), xs)), N.Census.plan.tree(d, ctx, Nat.div(n, 2n), N.Split.go(Nat.div(n, 4n), ys)))) == Nat.add(_, List.length(&2, N.Job, ys)) : Nat}
          %Laws.split_total(Nat.div(n, 4n), ys) : {List.length(&2, N.Named, List.append(&2, N.Named, N.Census.plan.tree(d, ctx, Nat.div(n, 2n), N.Split.go(Nat.div(n, 4n), xs)), N.Census.plan.tree(d, ctx, Nat.div(n, 2n), N.Split.go(Nat.div(n, 4n), ys)))) == Nat.add(List.length(&2, N.Named, N.Census.plan.tree(d, ctx, Nat.div(n, 2n), N.Split.go(Nat.div(n, 4n), xs))), _) : Nat}
          %Laws.tree_total(d, ctx, Nat.div(n, 2n), N.Split.go(Nat.div(n, 4n), ys)) : {List.length(&2, N.Named, List.append(&2, N.Named, N.Census.plan.tree(d, ctx, Nat.div(n, 2n), N.Split.go(Nat.div(n, 4n), xs)), N.Census.plan.tree(d, ctx, Nat.div(n, 2n), N.Split.go(Nat.div(n, 4n), ys)))) == Nat.add(List.length(&2, N.Named, N.Census.plan.tree(d, ctx, Nat.div(n, 2n), N.Split.go(Nat.div(n, 4n), xs))), _) : Nat}
          Laws.len_append(N.Census.plan.tree(d, ctx, Nat.div(n, 2n), N.Split.go(Nat.div(n, 4n), xs)), N.Census.plan.tree(d, ctx, Nat.div(n, 2n), N.Split.go(Nat.div(n, 4n), ys)))

def Laws.plan_par_total(depth, ctx, n, js):
  %Laws.split_total(Nat.div(n, 2n), js) : {List.length(&2, N.Named, N.Census.plan.tree(depth, ctx, n, N.Split.go(Nat.div(n, 2n), js))) == _ : Nat}
  Laws.tree_total(depth, ctx, n, N.Split.go(Nat.div(n, 2n), js))

def Laws.len_append_fixed(xs, ys):
  match xs:
    case Nil{}:
      {==}
    case h <> t:
      %Laws.len_append_fixed(t, ys) : {1n+List.length(&2, N.Fixed, List.append(&2, N.Fixed, t, ys)) == 1n+_ : Nat}
      {==}

def Laws.ncons_total(h, r):
  match r:
    case (a, b):
      {==}

def Laws.nsplit_total(n, xs):
  match n:
    case 0n:
      {==}
    case 1n+(+k):
      match xs:
        case Nil{}:
          {==}
        case h <> +t:
          %Laws.nsplit_total(k, t) : {N.Split.ntotal(N.Split.ncons(h, N.Split.ngo(k, t))) == 1n+_ : Nat}
          Laws.ncons_total(h, N.Split.ngo(k, t))

def Laws.fix_seq_total(dirs, ctx, ns):
  match ns:
    case Nil{}:
      {==}
    case N.Named{p, d} <> t:
      %Laws.fix_seq_total(dirs, ctx, t) : {1n+List.length(&2, N.Fixed, N.Census.fix.seq(dirs, ctx, t)) == 1n+_ : Nat}
      {==}

def Laws.fix_tree_total(depth, dirs, ctx, n, r):
  match depth:
    case 0n:
      match r:
        case (+xs, +ys):
          %Laws.fix_seq_total(dirs, ctx, xs) : {List.length(&2, N.Fixed, List.append(&2, N.Fixed, N.Census.fix.seq(dirs, ctx, xs), N.Census.fix.seq(dirs, ctx, ys))) == Nat.add(_, List.length(&2, N.Named, ys)) : Nat}
          %Laws.fix_seq_total(dirs, ctx, ys) : {List.length(&2, N.Fixed, List.append(&2, N.Fixed, N.Census.fix.seq(dirs, ctx, xs), N.Census.fix.seq(dirs, ctx, ys))) == Nat.add(List.length(&2, N.Fixed, N.Census.fix.seq(dirs, ctx, xs)), _) : Nat}
          Laws.len_append_fixed(N.Census.fix.seq(dirs, ctx, xs), N.Census.fix.seq(dirs, ctx, ys))
    case 1n+d:
      match r:
        case (+xs, +ys):
          %Laws.nsplit_total(Nat.div(n, 4n), xs) : {List.length(&2, N.Fixed, List.append(&2, N.Fixed, N.Census.fix.tree(d, dirs, ctx, Nat.div(n, 2n), N.Split.ngo(Nat.div(n, 4n), xs)), N.Census.fix.tree(d, dirs, ctx, Nat.div(n, 2n), N.Split.ngo(Nat.div(n, 4n), ys)))) == Nat.add(_, List.length(&2, N.Named, ys)) : Nat}
          %Laws.fix_tree_total(d, dirs, ctx, Nat.div(n, 2n), N.Split.ngo(Nat.div(n, 4n), xs)) : {List.length(&2, N.Fixed, List.append(&2, N.Fixed, N.Census.fix.tree(d, dirs, ctx, Nat.div(n, 2n), N.Split.ngo(Nat.div(n, 4n), xs)), N.Census.fix.tree(d, dirs, ctx, Nat.div(n, 2n), N.Split.ngo(Nat.div(n, 4n), ys)))) == Nat.add(_, List.length(&2, N.Named, ys)) : Nat}
          %Laws.nsplit_total(Nat.div(n, 4n), ys) : {List.length(&2, N.Fixed, List.append(&2, N.Fixed, N.Census.fix.tree(d, dirs, ctx, Nat.div(n, 2n), N.Split.ngo(Nat.div(n, 4n), xs)), N.Census.fix.tree(d, dirs, ctx, Nat.div(n, 2n), N.Split.ngo(Nat.div(n, 4n), ys)))) == Nat.add(List.length(&2, N.Fixed, N.Census.fix.tree(d, dirs, ctx, Nat.div(n, 2n), N.Split.ngo(Nat.div(n, 4n), xs))), _) : Nat}
          %Laws.fix_tree_total(d, dirs, ctx, Nat.div(n, 2n), N.Split.ngo(Nat.div(n, 4n), ys)) : {List.length(&2, N.Fixed, List.append(&2, N.Fixed, N.Census.fix.tree(d, dirs, ctx, Nat.div(n, 2n), N.Split.ngo(Nat.div(n, 4n), xs)), N.Census.fix.tree(d, dirs, ctx, Nat.div(n, 2n), N.Split.ngo(Nat.div(n, 4n), ys)))) == Nat.add(List.length(&2, N.Fixed, N.Census.fix.tree(d, dirs, ctx, Nat.div(n, 2n), N.Split.ngo(Nat.div(n, 4n), xs))), _) : Nat}
          Laws.len_append_fixed(N.Census.fix.tree(d, dirs, ctx, Nat.div(n, 2n), N.Split.ngo(Nat.div(n, 4n), xs)), N.Census.fix.tree(d, dirs, ctx, Nat.div(n, 2n), N.Split.ngo(Nat.div(n, 4n), ys)))

def Laws.fix_par_total(depth, dirs, ctx, n, ns):
  %Laws.nsplit_total(Nat.div(n, 2n), ns) : {List.length(&2, N.Fixed, N.Census.fix.tree(depth, dirs, ctx, n, N.Split.ngo(Nat.div(n, 2n), ns))) == _ : Nat}
  Laws.fix_tree_total(depth, dirs, ctx, n, N.Split.ngo(Nat.div(n, 2n), ns))
