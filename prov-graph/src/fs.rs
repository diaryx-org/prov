//! prov's filesystem read port.
//!
//! prov is generic over *where* documents live: rather than depend on any one
//! concrete backend — `std::fs`, `tokio::fs`, or a browser filesystem like
//! OPFS/IndexedDB — the library asks only for a small async trait that mirrors
//! the slice of [`std::fs`] its scan/traverse engine needs.
//!
//! The port itself lives in [`prov_transaction::fs`], one layer below this
//! crate, because it is the seam a *transaction* lands through and has nothing
//! to do with documents. It is re-exported here so `prov_graph::fs` remains the
//! name prov's read core is written against.
//!
//! Only the read half is re-exported here. The write half — [`Storage`], the
//! durability vocabulary, and the writable adapters — is `prov-store`'s `fs`
//! module, so that depending on this crate cannot get you the ability to change
//! a workspace.
//!
//! [`Storage`]: prov_transaction::fs::Storage

pub use prov_transaction::fs::{DirEntry, FileType, Metadata, ReadStorage, StdFs};
