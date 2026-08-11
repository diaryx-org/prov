//! History's tests — **integration** tests, which is why they are here rather
//! than in `prov-history`.
//!
//! Almost none of them can be written against the store alone. They seed a real
//! [`Workspace`](crate::Workspace) over a real filesystem and then ask what the
//! rest of prov makes of what history did: whether [`check`](crate::Workspace::check)
//! comes back clean, whether the [`Fix`](crate::Fix) it suggests actually retires
//! the finding, whether a warmed [`FixityCache`](crate::FixityCache) removes the
//! reads, whether a recycled document stops answering to its title. That
//! composition *is* the thing under test, and it only exists at this layer.
//!
//! The pure-logic tests — manifest ordering, id minting, blob paths, index
//! rendering — travelled with their code and live in `prov-history` beside it.
//!
//! One file per verb, mirroring the modules in `prov-history`. The fixtures
//! they share — a seeded workspace, a capture, a torn event — are in
//! [`support`], which no sibling could own without every other one reaching
//! across for it.

mod capture;
mod check;
mod forget;
mod prune;
mod read;
mod restore;
mod support;
