//! # prov-grain
//!
//! Grains for a [prov](https://docs.rs/prov) workspace: how finely a value is
//! cut into groups.
//!
//! A grain is a **coarsening** — a many-to-one function from a value to group
//! keys. `2026-07-24` at `year` is `2026`, at `month` is `2026-07`; `Lovelace`
//! at `initial` is `L`. The calendar grains read their value as EDTF
//! ([`date`]), so an archive's `1913~` and `1918/1922` are cut rather than
//! dropped.
//!
//! A grain has two halves, and the two sides of a workspace use one each:
//!
//! - **Reading** — a view's `year(…)`, `month(…)`, `day(…)` and `initial(…)`
//!   functions (`prov-views`) — needs only [`Grain::cuts`]: value → keys,
//!   possibly several, because an interval is under every year it spans.
//! - **Writing** — a filing entry's `nest:` (`prov-filing`) — also needs
//!   [`Grain::chain`], the coarser grains a nest builds indexes through, and
//!   [`Grain::cut`], the single key a record files under.
//!
//! That is why this is a crate of its own: a grain belongs to neither side,
//! and neither side should depend on the other to reach it.

pub mod date;
pub mod grain;
mod scalar;

pub use grain::{GRAINS, Grain};
pub use scalar::{scalar_text, scalar_texts};
