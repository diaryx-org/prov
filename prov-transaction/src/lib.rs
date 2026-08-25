//! Crash-atomic filesystem transactions.
//!
//! [`ChangeSet`] stages root-relative writes, renames, removals, and copies as
//! one ordered unit. [`ChangeSet::apply`] lands the whole set or none of it: an
//! error unwinds every op already applied, and a write-ahead journal makes a
//! *committed* set recoverable after a process crash or power loss, via
//! [`recover`].
//!
//! The journal's file name is [configurable](Journal::named) and defaults to
//! [`.fstx-journal`](Journal::DEFAULT_NAME). Apply and recovery must agree
//! about it, which is why both operations hang off [`Journal`].
//!
//! Files stay ordinary files. This is not a virtual filesystem and not a
//! database — nothing here changes how the tree is read, only how it is
//! written.
//!
//! ```no_run
//! use prov_transaction::{ChangeSet, StdFs, exec::block_on, recover};
//! use std::path::Path;
//!
//! let root = Path::new("/tmp/example");
//! # std::fs::create_dir_all(root).unwrap();
//! // Finish anything a previous crash left journaled, before reading the tree.
//! block_on(recover(&StdFs, root))?;
//!
//! let mut change = ChangeSet::new();
//! change.write("notes/a.md", "hello");
//! change.rename("old.md", "notes/b.md");
//! change.remove("stale.md");
//! block_on(change.apply(&StdFs, root))?;
//! # Ok::<(), prov_transaction::Error>(())
//! ```
//!
//! ## Backends
//!
//! Everything is generic over the [`fs`] port, so the same transaction runs
//! over [`StdFs`], the bundled [`InMemoryFs`], or an adapter you write for
//! OPFS, IndexedDB, or a network store. A backend *declares* the durability it
//! can keep through [`Capabilities`](fs::Capabilities), and the apply path
//! picks the strongest protocol that backend actually supports rather than
//! assuming one and lying on the backends that cannot keep it.
//!
//! ## Scope
//!
//! - **Single writer.** There is no locking; concurrent appliers against one
//!   root will race. See [`change`] for the details.
//! - **A set is bounded by memory.** Staged bytes and the undo buffer are both
//!   held in memory for the length of the apply; [`FileOp::CopyFrom`] is the
//!   escape hatch for a large payload already on disk.
//! - **Futures are not required to be `Send`.** The port uses native
//!   `async fn`, so a backend keeps its own future types — which means an
//!   apply over a non-`Send` backend cannot be `tokio::spawn`ed.

pub mod change;
pub mod error;
pub mod exec;
pub mod fs;
pub mod journal;
pub mod path;

#[cfg(test)]
mod fs_faults;

pub use change::{ChangeSet, FileOp};
pub use error::{Error, Result};
pub use fs::{InMemoryFs, ReadStorage, StdFs, Storage};
pub use journal::{Journal, Recovered, recover};
