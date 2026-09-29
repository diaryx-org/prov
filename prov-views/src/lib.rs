//! # prov-views
//!
//! Declarative views over a [prov](https://docs.rs/prov) workspace: the format
//! a workspace declares them in, and the traversal that executes one into a
//! grouped set of rows.
//!
//! ## What a view is
//!
//! A prov workspace has a **spine** — the single-parent spanning relation that
//! makes a directory of plain files discoverable by following its own links. A
//! view is a *second* way through the same documents: "the entries under
//! `Daily`, by month", "everything tagged, by tag". The same document can
//! appear under several groups, which is exactly what the spine cannot do and
//! why a view is worth having.
//!
//! ```yaml
//! views:
//!   daily:
//!     label: Daily
//!     icon: calendar
//!     where: "under('Daily') && !present(draft)"
//!     key: month(first(date_of_document, created, updated))
//! ```
//!
//! ## A view is a query
//!
//! `where:` and `key:` are [CEL](expr) expressions over each document's
//! fields and the document itself (`doc`). prov adds a handful of functions
//! that carry its own decisions — `year`/`month`/`day` cut a value read as
//! EDTF (so an archive's `1913~` and `1918/1922` file, see
//! `prov_grain::date`; the grains are `prov-grain`'s, shared with filing),
//! `initial` cuts an A–Z index, `first` is a fallback chain, `present` asks
//! whether a field is filled in — and nothing in this crate knows which field
//! is the date: the three names in the example above are a *declaration the
//! workspace makes*.
//!
//! A view does not know the spine. The census ([`documents`]) records each
//! document's ancestors, and a view scopes itself by reading them. Where a
//! *new* record goes is not a view's either: that is `prov-filing`, a
//! declaration of its own, because filing writes into the single-parent spine
//! and needs guarantees reading does not.
//!
//! ## What this crate does not do
//!
//! **It cannot write.** It reads through `prov-graph`, the read core,
//! whose filesystem port has no method that writes a byte — so a view engine is
//! structurally unable to modify the workspace it reads, rather than merely
//! intending not to.
//!
//! **It has no invariant.** prov's job is what must stay true — inverses
//! paired, ids registered, links resolvable, fixity honest — and a view is not
//! that: a wrong view shows the wrong rows and you edit the file. That is why
//! this is a crate beside prov rather than a feature inside it, and why
//! `prov_filing::FilingSpec::route` is a *description* of where a frontend should file a
//! new record rather than something this crate goes and does. Its expression
//! evaluator, CEL, cannot write either: an expression has no side effects and
//! always finishes.
//!
//! **It does not render.** A [`RowSet`] is data. Which glyph `icon: calendar`
//! draws, and what the [ungrouped](RowSet::ungrouped) bucket is called, are
//! decisions for the frontend that has a screen.
//!
//! ## Two halves: select, then group
//!
//! [`select`](fn@select) answers *which documents does this view cover?* — the
//! census, then the condition — and returns a flat, deduplicated
//! [`Selection`] in path order.
//! [`group`](fn@group) projects that into a [`RowSet`], and is a **pure function**: no
//! I/O, no workspace, nothing to mock.
//!
//! The split is not tidiness. A [`Selection`] is the honest answer to "how many
//! documents is this view about", which a grouped result cannot give — a
//! document under two of a multi-valued field's groups is one document in two
//! places. It also means one selection can be grouped several ways at once,
//! which is what a frontend's view switcher does, and that every grouping
//! question is testable without a filesystem.
//!
//! ```no_run
//! use prov_graph::exec::block_on;
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let graph: prov_graph::Graph<prov_graph::fs::StdFs, prov_graph::index::NoIndex> = todo!();
//! # let spec: prov_views::ViewSpec = todo!();
//! let selection = block_on(prov_views::select(&graph, &spec, "index.md"))?;
//! println!("{} documents", selection.len());
//!
//! let rows = prov_views::group(&selection, &spec.key);
//! for group in &rows.groups {
//!     println!("{} ({})", group.key, group.rows.len());
//! }
//! # Ok(())
//! # }
//! ```

pub mod error;
pub mod expr;
pub mod group;
pub mod legacy;
pub mod lint;
pub mod search;
pub mod select;
pub mod spec;

pub use error::{Error, Result};
pub use expr::{Evaluator, Expression, ExpressionError, FUNCTIONS, KeyShape};
pub use group::{Group, RowSet, group};
pub use legacy::{Translation, translate};
pub use lint::{ViewIssue, ViewIssueKind, diagnose_view, diagnose_views};
pub use search::{Corpus, Excluded, Hit, IndexedDoc, Passage, Query, Site, corpus, fold, search};
pub use select::{Ancestor, Clause, Failure, Row, Selection, documents, narrow, select};
pub use spec::{RETIRED_VIEW_KEYS, VIEW_KEYS, VIEWS_KEY, ViewSpec, humanize, views_from};
