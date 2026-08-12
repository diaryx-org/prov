//! # prov-exports
//!
//! Named, closed-by-default document sets that may leave a
//! [prov](https://docs.rs/prov) workspace: the format a workspace declares
//! them in, and the plan that composes a gate with a view.
//!
//! ## What an export is
//!
//! Everything else prov reads is open by default — a view with no `under:`
//! covers the whole workspace, the spanning walk reaches everything. An
//! export is the boundary where that flips: a document is in an export only
//! if the document *itself* declares the export's gate value, and a document
//! that declares nothing leaves in nothing. The declaration lives in the
//! document, so it travels with the file and still means what it meant.
//!
//! ```yaml
//! exports:
//!   letters:
//!     label: Letters home
//!     gate: { field: audience, value: family }
//!     view: daily
//! ```
//!
//! What consumes a plan is deliberately out of scope: a publish step, an
//! OCFL/copy-out export, a partial sync all start from the same
//! [`ExportPlan`]. This crate only ever *plans* — and its dependencies are
//! `prov-graph` and `prov-views`, neither of which can write, so the layer
//! that decides what may leave is structurally unable to alter what it
//! judges.
//!
//! ## The invariant, and why this is not a view
//!
//! `prov-views` is a crate beside prov because a view has no invariant — a
//! wrong view shows the wrong rows and you edit the file. An export is the
//! opposite case: a wrong export is a file in hands it was never meant for,
//! and the crate exists to keep one sentence true:
//!
//! > **An export's document set is a subset of its gate's admitted set —
//! > whatever the view says.**
//!
//! [`plan`](fn@plan) enforces it structurally (the view's selection is only
//! ever `retain`ed against, never added from) and fails closed everywhere
//! else: an unreadable declaration is not an export, an unknown or broken
//! view is an error rather than a fall-back to the whole gate set, and
//! matching is exact so the written config never says less than the gate
//! does. The reasoning lives in [`spec`] and [`plan`](mod@plan).
//!
//! ## Two halves: plan, then compose
//!
//! [`plan`](fn@plan) is the half that touches the workspace — one spanning
//! walk, one metadata read per document, one view selection. [`compose`] is
//! the pure valve over what it gathered, testable with nothing to mock. The
//! same split as `prov-views`' `select`/`group`, for the same reason.

pub mod error;
pub mod lint;
pub mod plan;
pub mod spec;

pub use error::{Error, Result};
pub use lint::{ExportIssue, ExportIssueKind, diagnose_export, diagnose_exports};
pub use plan::{ExportDoc, ExportPlan, Withheld, compose, plan};
pub use spec::{EXPORT_KEYS, EXPORTS_KEY, ExportSpec, GATE_KEYS, Gate, exports_from};
