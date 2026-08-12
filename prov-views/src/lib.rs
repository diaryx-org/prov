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
//!     group: [date_of_document, created, updated]
//!     by: month
//!     under: '[Daily](/Daily/daily_index.md)'
//!     where:
//!       not: { has: draft }
//!     nest: month
//! ```
//!
//! ## Nothing here knows what a date is
//!
//! This crate has no `date` grouping, no built-in field chain, and no calendar.
//! `group:` is an ordered list of field keys and `by:` is a **coarsening** —
//! `year`/`month`/`day` cut ISO-8601 text, `initial` cuts the first letters for
//! an A–Z index, and both are the same kind of thing. So the three field names
//! in the example above are a *declaration the workspace makes*, not a
//! convention this crate blesses. A workspace that files by `taken_on` writes
//! that instead, and every prov tool reading the same `views:` block agrees,
//! rather than each one hardcoding a chain and hoping.
//!
//! The reasoning, and the MoReq2010 classification/aggregation split the format
//! follows, are in [`spec`].
//!
//! ## What this crate does not do
//!
//! **It cannot write.** Its one dependency is `prov-graph`, the read core,
//! whose filesystem port has no method that writes a byte — so a view engine is
//! structurally unable to modify the workspace it reads, rather than merely
//! intending not to.
//!
//! **It has no invariant.** prov's job is what must stay true — inverses
//! paired, ids registered, links resolvable, fixity honest — and a view is not
//! that: a wrong view shows the wrong rows and you edit the file. That is why
//! this is a crate beside prov rather than a feature inside it, and why
//! [`ViewSpec::nest`] is a *description* of where a frontend should file a new
//! record rather than something this crate goes and does.
//!
//! **It does not render.** A [`RowSet`] is data. Which glyph `icon: calendar`
//! draws, and what the [ungrouped](RowSet::ungrouped) bucket is called, are
//! decisions for the frontend that has a screen.
//!
//! ## Two halves: select, then group
//!
//! [`select`](fn@select) answers *which documents does this view cover?* — scope, then
//! conditions — and returns a flat, deduplicated [`Selection`] in path order.
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
//! let rows = prov_views::group(&selection, &spec.group);
//! for group in &rows.groups {
//!     println!("{} ({})", group.key, group.rows.len());
//! }
//! # Ok(())
//! # }
//! ```

pub mod error;
pub mod filter;
pub mod group;
pub mod lint;
pub mod select;
pub mod spec;

pub use error::{Error, Result};
pub use filter::{CONDITION_KEYS, Condition};
pub use group::{Group, RowSet, group};
pub use lint::{ViewIssue, ViewIssueKind, diagnose_view, diagnose_views};
pub use select::{Row, Selection, select};
pub use spec::{GRAINS, Grain, Grouping, VIEW_KEYS, VIEWS_KEY, ViewSpec, humanize, views_from};
