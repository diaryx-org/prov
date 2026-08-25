//! The write half of prov's filesystem port.
//!
//! [`prov_graph::fs`] re-exports [`ReadStorage`] — everything the traversal
//! core needs, and nothing that can change a byte. This module re-exports the
//! other half: [`Storage`], the durability vocabulary a backend answers with
//! ([`Capabilities`], [`Durability`], [`SyncGuarantee`]), and the
//! write-temp-then-rename protocol that makes a replacement crash-atomic.
//!
//! Both halves are defined in [`prov_transaction::fs`], one layer below, where
//! the transaction that drives them lives. The split into a read module here
//! and a write module there is what it has always been: importing `prov-graph`
//! alone cannot get you the ability to change a workspace.

pub use prov_transaction::fs::{
    Capabilities, Durability, InMemoryFs, ReadStorage, StdFs, Storage, SyncGuarantee, memory,
};
