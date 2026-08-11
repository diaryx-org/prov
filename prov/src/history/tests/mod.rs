//! History's **integration** tests — what `prov` adds around the store.
//!
//! The store's own behaviour is tested in `prov-history`, against a host built
//! to nothing but its two traits. Everything here needs the rest of this crate
//! to even be phrased, and that is the entry condition: a test belongs in this
//! module only if removing `prov` from it would remove its subject.
//!
//! Two of those, one file each:
//!
//! - [`findings`] — [`HistoryIssue`](prov_history::HistoryIssue) →
//!   [`Finding`](crate::Finding), its prose and its ordering, the
//!   [`Fix`](crate::Fix) that retires it, and the claim no store-level check
//!   can make: that a history verb leaves the whole *workspace*
//!   [`check`](crate::Workspace::check)-clean.
//! - [`composition`] — the answers this crate supplies to the host traits, and
//!   what else in the workspace depends on them: the recycle bin's items
//!   excluded from a capture set, the store's interior excluded from the title
//!   index, and the [`FixityCache`](crate::FixityCache) a capture reads through.
//!
//! If a test here stops needing `check`, a `Fix`, the bin, the title index or
//! the cache, it has become a test of the store and should move.

mod composition;
mod findings;
mod support;
