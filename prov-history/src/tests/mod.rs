//! The skiplist held to its own claims.
//!
//! [`SkipHost`](crate::SkipHost) is the whole of what this crate may assume
//! about the workspace, so the host here supplies exactly its four facts as
//! fixtures and nothing more — a test that needs prov's real reachability is
//! testing `prov`, and lives there. The store side is the opposite: those
//! tests run against a **real** historica store built with the historica
//! library itself, because the store's formats are the other party to this
//! crate's one contract, and a mock of them would test agreement with the
//! mock.

mod host;
mod plan;
mod store;
mod support;
