//! Crash-atomic filesystem transactions.
//!
//! [`ChangeSet`] stages workspace-relative writes, renames, removals, and
//! content-addressed copies as one ordered unit. A write-ahead journal makes a
//! committed set recoverable after a process crash or power loss; ordinary
//! errors are unwound in memory.
//!
//! This crate deliberately knows nothing about a workspace, its graph, or its
//! mutation policy. It only needs the [`prov_store::fs::Storage`] port and the
//! [`prov_store::index::Rebase`] seam used by persistent indexes to inspect a
//! pending set.

pub mod change;
pub mod journal;

#[cfg(test)]
mod fs_faults;

pub use change::{ChangeSet, FileOp, discard_file, write_blob_atomic, write_probe};
pub use journal::{JOURNAL_NAME, Recovered, recover};
