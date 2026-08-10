//! Laws over the verbs, rather than examples of them.
//!
//! Each verb's own `mod tests` drives it once, from a fixture chosen to show
//! what it does. That answers "does `reparent` reparent?" and cannot answer the
//! question this module asks, which is about the verbs *together*:
//!
//! > A workspace that was whole stays whole, whatever you do to it.
//!
//! That is the crate's central promise — link maintenance spans documents, so
//! the failure mode that matters is not one verb misbehaving but one verb
//! leaving a workspace the *next* verb then reasons about wrongly. An example
//! test cannot reach it, because reaching it means running a sequence nobody
//! thought to write down.
//!
//! So: generate sequences. The oracle is prov's own
//! [`CheckDiff`](crate::validate::CheckDiff) — already built for exactly this
//! judgment, since a bare post-operation finding list cannot tell damage an
//! operation *caused* from damage it *inherited*. Only `introduced` is a bug.
//!
//! Two laws are checked over every generated sequence, and they cover the two
//! ways an op can end:
//!
//! - **It succeeded** — then it introduced no finding it did not itself report
//!   (only `delete` ever reports one, and only because it is designed to leave
//!   inbound references dangling rather than rewrite intent it cannot re-aim).
//! - **It refused** — then it changed nothing at all (`change.rs`'s error
//!   atomicity, quantified: an in-memory unwind is only worth having if it
//!   really does put every byte back).
//!
//! The backend is [`InMemoryFs`] rather than a temp directory: a few hundred
//! `check` runs over a live workspace is the whole point, and that is only cheap
//! without a real disk under it.

use std::path::{Path, PathBuf};

use proptest::prelude::*;

use crate::exec::block_on;
use crate::fs::InMemoryFs;
use crate::relation::RelationSet;
use crate::validate::{CheckDiff, Finding};
use crate::workspace::Workspace;

/// The workspace root, and the start `check` is asked from. No generated op
/// ever takes it as a subject: renaming or deleting the root is a real thing to
/// do, but it moves the ground `check` stands on, and a law about *the rest of
/// the verbs* should not be entangled with that. It stays a parent, though —
/// every op that takes a container may name it.
const ROOT: &str = "index.md";

/// Filenames a generated op may author. Deliberately disjoint from the seed's
/// names, so a collision is always the op's own doing and never the fixture's.
const NAMES: [&str; 3] = ["p", "q", "r"];
/// Directories a generated op may author into — including the root itself, and
/// one the seed already uses, so "moved into an occupied directory" is reachable.
const DIRS: [&str; 3] = ["", "n", "d"];
const TITLES: [&str; 2] = ["Renamed", "Retitled"];

/// A verb applied to *positions* rather than paths. The paths cannot be
/// generated up front — half of them do not exist until an earlier op in the
/// same sequence creates them — so an op names its operands by index and is
/// resolved against the live workspace when its turn comes (see [`pick`]).
///
/// Indices shrink toward zero, which is what makes a shrunk counterexample
/// readable: proptest walks the failure back toward "the first document, under
/// the root", not toward some arbitrary survivor.
#[derive(Debug, Clone)]
enum Op {
    Create {
        parent: usize,
        name: usize,
        dir: usize,
    },
    Rename {
        subject: usize,
        name: usize,
        dir: usize,
    },
    Reparent {
        child: usize,
        parent: usize,
    },
    Adopt {
        child: usize,
        parent: usize,
    },
    Duplicate {
        subject: usize,
    },
    Retitle {
        subject: usize,
        title: usize,
    },
    Separate {
        subject: usize,
    },
    Combine {
        subject: usize,
    },
    Delete {
        subject: usize,
    },
}

fn op() -> impl Strategy<Value = Op> {
    let ix = 0..6usize;
    prop_oneof![
        (ix.clone(), 0..NAMES.len(), 0..DIRS.len()).prop_map(|(parent, name, dir)| Op::Create {
            parent,
            name,
            dir
        }),
        (ix.clone(), 0..NAMES.len(), 0..DIRS.len()).prop_map(|(subject, name, dir)| Op::Rename {
            subject,
            name,
            dir
        }),
        (ix.clone(), ix.clone()).prop_map(|(child, parent)| Op::Reparent { child, parent }),
        (ix.clone(), ix.clone()).prop_map(|(child, parent)| Op::Adopt { child, parent }),
        ix.clone().prop_map(|subject| Op::Duplicate { subject }),
        (ix.clone(), 0..TITLES.len()).prop_map(|(subject, title)| Op::Retitle { subject, title }),
        ix.clone().prop_map(|subject| Op::Separate { subject }),
        ix.clone().prop_map(|subject| Op::Combine { subject }),
        ix.prop_map(|subject| Op::Delete { subject }),
    ]
}

/// A small, `check`-clean workspace: a root, two children, and a grandchild in
/// a subdirectory (so re-relativization has something to get wrong).
fn seeded() -> InMemoryFs {
    InMemoryFs::with_files(
        [
            (
                ROOT,
                "---\ntitle: Home\ncontents:\n- '[A](/a.md)'\n- '[B](/b.md)'\n---\n# Home\n",
            ),
            (
                "a.md",
                "---\ntitle: A\npart_of: '[Home](/index.md)'\ncontents:\n- '[C](/n/c.md)'\n---\nA body.\n",
            ),
            (
                "n/c.md",
                "---\ntitle: C\npart_of: '[A](/a.md)'\n---\nC body.\n",
            ),
            (
                "b.md",
                "---\ntitle: B\npart_of: '[Home](/index.md)'\n---\nB body.\n",
            ),
        ]
        .into_iter()
        .map(|(p, t)| (PathBuf::from(p), t.to_string()))
        .collect(),
    )
}

/// How many sequences to run. A case is up to eight ops and two full `check`
/// passes per op, so this is the one knob worth keeping cheap by default and
/// easy to turn up: a hunt is
/// `PROPTEST_CASES=4000 cargo test --release mutate::properties`, which is how
/// the laws below were last taken seriously (4000 sequences of up to 13 ops,
/// both clean).
fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64)
}

fn build(fs: &InMemoryFs) -> Workspace<&InMemoryFs> {
    Workspace::builder(fs)
        .root(Path::new(""))
        .relations(RelationSet::diaryx())
        .build()
}

/// The workspace's reachable **nodes**, sorted — the spanning tree flattened.
///
/// This, rather than "every file on disk", is the law's domain, and the
/// narrowing was earned rather than assumed: drawing subjects from the raw file
/// listing let the sequences name a *separated body* (the prose half of a
/// `content` pair — a file, but not a node), and that turned up two behaviours
/// this law has no business ruling on together.
///
/// One was a real defect and is now fixed: `delete` took a node's body with it
/// but deleting the *body* stranded the node's `content` pointer in silence.
/// Both `delete` and `recycle` refuse a body subject now. The other is not a bug
/// at all — `duplicate` says outright that "a `source` with no parent (the
/// spanning root, or an orphan) is copied without attaching", so the `Orphan`
/// `check` then raises is the documented outcome, not damage.
///
/// A law that had to enumerate both would be a sieve of exceptions. Stated over
/// the nodes instead it is a clean claim — *operations on the workspace's nodes
/// keep the workspace whole*. Widening it back to every file is worth trying now
/// that the body case is guarded; what it would test is what `rename` and
/// `retitle` owe a file that is content but not a node.
///
/// Sorting is load-bearing, not tidiness: the backend stores files in a hash
/// map, and a property test that cannot replay its own counterexample is worth
/// very little.
fn nodes(ws: &Workspace<&InMemoryFs>) -> Vec<PathBuf> {
    fn walk(node: &crate::graph::Node, out: &mut Vec<PathBuf>) {
        out.push(node.path.clone());
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    if let Ok(root) = block_on(ws.tree(ROOT)) {
        walk(&root, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

/// The whole workspace as bytes, for the did-a-refusal-change-anything half.
fn snapshot(fs: &InMemoryFs) -> Vec<(String, String)> {
    let mut entries = fs.export_entries();
    entries.sort();
    entries
}

/// Resolve an operand index against a candidate list. `None` when there is
/// nothing to pick — a sequence that deletes everything reachable still has to
/// run its remaining ops without panicking.
fn pick(ix: usize, candidates: &[PathBuf]) -> Option<PathBuf> {
    if candidates.is_empty() {
        return None;
    }
    Some(candidates[ix % candidates.len()].clone())
}

/// Run one op. `None` when it could not be addressed at all (nothing to pick);
/// otherwise the verb's verdict, carrying the findings it **reported**.
///
/// That list is empty for every verb but [`Workspace::delete`], which is the
/// one op designed to leave a workspace with something wrong in it: it removes
/// the parent's spanning entry, and every *other* inbound reference it returns
/// rather than rewrites, "because a link records intent and there is no new
/// target to send it to" (`delete.rs`). Threading it back here is what lets the
/// law below stay strict — a destructive verb is allowed to break exactly what
/// it says it broke, and not one finding more.
fn apply(ws: &mut Workspace<&InMemoryFs>, op: &Op) -> Option<crate::Result<Vec<Finding>>> {
    let all = nodes(ws);
    // A parent may be any document, including the root; a subject may be
    // anything but the root (see `ROOT`).
    let subjects: Vec<PathBuf> = all
        .iter()
        .filter(|p| p.as_path() != Path::new(ROOT))
        .cloned()
        .collect();
    let authored = |name: usize, dir: usize| -> PathBuf {
        Path::new(DIRS[dir]).join(format!("{}.md", NAMES[name]))
    };
    Some(match op {
        Op::Create { parent, name, dir } => {
            let parent = pick(*parent, &all)?;
            block_on(ws.create(&authored(*name, *dir), &parent)).map(|_| Vec::new())
        }
        Op::Rename { subject, name, dir } => {
            let subject = pick(*subject, &subjects)?;
            block_on(ws.rename(&subject, &authored(*name, *dir))).map(|_| Vec::new())
        }
        Op::Reparent { child, parent } => {
            let (child, parent) = (pick(*child, &subjects)?, pick(*parent, &all)?);
            block_on(ws.reparent(&child, &parent)).map(|_| Vec::new())
        }
        Op::Adopt { child, parent } => {
            let (child, parent) = (pick(*child, &subjects)?, pick(*parent, &all)?);
            block_on(ws.adopt(&child, &parent)).map(|_| Vec::new())
        }
        Op::Duplicate { subject } => {
            let subject = pick(*subject, &subjects)?;
            block_on(ws.duplicate(&subject)).map(|_| Vec::new())
        }
        Op::Retitle { subject, title } => {
            let subject = pick(*subject, &subjects)?;
            block_on(ws.retitle(&subject, TITLES[*title])).map(|_| Vec::new())
        }
        Op::Separate { subject } => {
            let subject = pick(*subject, &subjects)?;
            block_on(ws.separate(&subject)).map(|_| Vec::new())
        }
        Op::Combine { subject } => {
            let subject = pick(*subject, &subjects)?;
            block_on(ws.combine(&subject)).map(|_| Vec::new())
        }
        Op::Delete { subject } => {
            let subject = pick(*subject, &subjects)?;
            block_on(ws.delete(&subject, false))
        }
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(), ..ProptestConfig::default() })]

    /// **A workspace that was whole stays whole.** Every op that succeeds must
    /// leave `check` with nothing new to say — findings it *inherited* are not
    /// its verdict, which is exactly the distinction `CheckDiff` draws.
    ///
    /// The sequence matters more than any single op: this is where "verb A
    /// leaves a state verb B then reasons about wrongly" lives, and there is no
    /// fixture anyone would have thought to write for it.
    #[test]
    fn a_workspace_that_was_whole_stays_whole(ops in prop::collection::vec(op(), 1..9)) {
        let fs = seeded();
        let mut ws = build(&fs);

        let seed_findings = block_on(ws.check(ROOT)).expect("the seed is readable");
        prop_assert!(
            seed_findings.is_empty(),
            "the fixture must start clean, or every law below is vacuous: {seed_findings:?}"
        );

        for (n, op) in ops.iter().enumerate() {
            let before = block_on(ws.check(ROOT)).expect("check before");
            let Some(outcome) = apply(&mut ws, op) else { continue };
            // A refusal is the next law's business; here we need only that the
            // workspace is still checkable afterwards.
            let Ok(reported) = outcome else { continue };
            let after = block_on(ws.check(ROOT)).expect("check after");
            let unreported: Vec<_> = CheckDiff::between(&before, &after)
                .introduced
                .into_iter()
                .filter(|f| !reported.contains(f))
                .collect();
            prop_assert!(
                unreported.is_empty(),
                "op {n} of {ops:?} — {op:?} — introduced {unreported:?} without reporting it"
            );
        }
    }

    /// **A refused op changes nothing.** `change.rs` promises error atomicity:
    /// an op that fails part-way unwinds every write it had already made. Each
    /// verb's own tests prove that for one hand-placed failure; this proves it
    /// for whatever refusals a generated sequence happens to provoke — and the
    /// comparison is byte-for-byte over the whole tree, not "still check-clean",
    /// which a torn-but-undetectable state would also satisfy.
    #[test]
    fn a_refused_op_leaves_the_workspace_byte_for_byte(
        ops in prop::collection::vec(op(), 1..9),
    ) {
        let fs = seeded();
        let mut ws = build(&fs);

        for (n, op) in ops.iter().enumerate() {
            let before = snapshot(&fs);
            let Some(outcome) = apply(&mut ws, op) else { continue };
            if let Err(refusal) = outcome {
                prop_assert_eq!(
                    &snapshot(&fs),
                    &before,
                    "op {} of {:?} — {:?} — refused ({}) but still wrote",
                    n,
                    ops,
                    op,
                    refusal
                );
            }
        }
    }
}
