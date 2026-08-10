//! Plain text → walkable graph — the crate's read core.
//!
//! Underneath everything else sits the **census**
//! ([`census`](crate::workspace::Workspace::census)): one traversal that
//! yields every forward link reachable from a root — frontmatter relation
//! edges *and* body `[[…]]` wikilinks alike — each tagged with where it is
//! written ([`LinkSite`]) and how it resolves ([`Resolution`]), plus the
//! [`StructuralFact`]s the same pass raises from traversal state (a document
//! that would not load, a broken single-parent invariant, and so on). Because
//! it is read straight from the documents, the census is *ground truth*:
//! [`validate`](crate::validate)'s findings, the
//! [`backlinks`](crate::workspace::Workspace::backlinks) map, and
//! reachability ([`reachable_files`](crate::workspace::Workspace::reachable_files),
//! [`reachable_documents`](crate::workspace::Workspace::reachable_documents))
//! are all views over it, and any stored index heals *toward* the census,
//! never the reverse.
//!
//! Alongside it sits [`tree`]'s materialized [`Node`] walk — the same edges,
//! but a spanning-only DFS that renders a `contents`/`part_of` outline rather
//! than a flat link census. See `tree`'s module doc for why it stays a
//! second walker instead of a view over the census.
//!
//! This is the plain-text-workspace promise (crate root docs) made concrete:
//! follow the links declared in a document's own metadata and body, and the
//! structure unfolds without a side channel — no cache to trust instead of the
//! documents themselves. `validate`'s findings and `mutate`'s inbound-rename
//! maintenance are both built on what is censused here; nothing above this
//! module re-derives an edge from anywhere but a document's own bytes.
//!
//! Housed here: the read primitive ([`load`]) every pass shares, link
//! resolution ([`resolve`], [`Target`]) built on top of it, the census types
//! with the spanning-tree walker that fills them in, and the [`tree`] walker.
//! They stay `impl`ed on [`Workspace`](crate::workspace::Workspace) rather
//! than a graph type of its own. That split is now unblocked —
//! [`validate`](crate::validate) has shed its repair half to
//! [`remedy`](crate::remedy) and is a findings view and nothing else — but it
//! is still not *warranted*: a `Graph` handle would have to be threaded
//! through every mutation verb, each of which reads and writes in the same
//! breath, and nothing yet asks for the seam that would buy.
//!
//! `graph` is also the crate's sole *surface* onto [`Storage`](crate::fs::Storage)
//! reads: every other module reaches the filesystem through a `Workspace`
//! method housed here rather than calling `self.fs()` directly. Most of that
//! surface is [`load`] — clamped against root escape and served from the
//! read-scope memo — but a handful of call sites (existence checks, a
//! directory listing, a raw byte read for something that is not a document)
//! never wanted the clamp or the memo; those go through [`probe`]'s raw
//! primitives instead.
//!
//! **What this module does not depend on.** `graph` imports the mechanism
//! layers below it — [`crate::document`], [`crate::link`], [`crate::title`],
//! [`crate::identity`], and the generic [`crate::index::IndexStore`] — but
//! never a *policy* module (`crate::config`, `crate::validate`,
//! `crate::about`). The census walk raises [`StructuralFact`]s rather than
//! [`Finding`](crate::validate::Finding)s for exactly this reason: `Finding`
//! is `validate`'s vocabulary, and a walker that constructed one directly
//! would pull that policy layer's whole enum (and its config-, fixity-, and
//! vocabulary-flavored variants) down into the read core. `validate::check`
//! derives each `Finding` from a `StructuralFact` or a [`CensusEntry`]'s
//! [`Resolution`] one for one — the walk already knows exactly what
//! happened; `validate` only names it.
//! [`resolve_link_with`](crate::workspace::Workspace::resolve_link_with)'s
//! only reach beyond a bare path/id resolver is [`crate::title::TitleIndex`], which
//! is itself a derived cache with no policy of its own (DESIGN §5) — the same
//! dependency [`census`] already carries. That is a stable seam, not a design
//! gap the coupling papers over, so no `Resolve` trait was introduced here: it
//! would exist only to abstract a single already-generic parameter
//! (`Ix: IndexStore`) and a self-contained cache type, and would cost a layer
//! of indirection for no dependency this module does not already own.

mod census;
mod load;
mod probe;
mod resolve;
mod tree;

pub use census::{Backlink, CensusEntry, LinkSite, Resolution};
pub(crate) use census::{StructuralFact, Walk, reachable_set};
pub use resolve::Target;
pub use tree::{Node, NodeKind, TreeOptions};
