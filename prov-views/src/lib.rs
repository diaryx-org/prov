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
//!     nest: month
//! ```
//!
//! ## Nothing here knows what a date is
//!
//! This crate has no `date` grouping, no built-in field chain, and no calendar.
//! `group:` is an ordered list of field keys and `by:` is a prefix cut over
//! ISO-8601 text — so the three field names in the example above are a
//! *declaration the workspace makes*, not a convention this crate blesses. A
//! workspace that files by `taken_on` writes that instead, and every prov tool
//! reading the same `views:` block agrees, rather than each one hardcoding a
//! chain and hoping.
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
//! ## Executing one
//!
//! ```no_run
//! use prov_graph::exec::block_on;
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let graph: prov_graph::Graph<prov_graph::fs::StdFs, prov_graph::index::NoIndex> = todo!();
//! # let spec: prov_views::ViewSpec = todo!();
//! let rows = block_on(prov_views::execute(&graph, &spec, "index.md"))?;
//! for group in &rows.groups {
//!     println!("{} ({})", group.key, group.rows.len());
//! }
//! # Ok(())
//! # }
//! ```

pub mod error;
pub mod exec;
pub mod lint;
pub mod spec;

pub use error::{Error, Result};
pub use exec::{Group, Row, RowSet, execute};
pub use lint::{ViewIssue, ViewIssueKind, diagnose_view, diagnose_views};
pub use spec::{GRAINS, Grain, Grouping, VIEW_KEYS, VIEWS_KEY, ViewSpec, humanize, views_from};
