//! Identity, re-exported from the read core.
//!
//! The id type, its check-character verification, and the registration/minting
//! policy all live in [`prov_graph::identity`] — none of it touches storage, so
//! none of it needs to sit above the read boundary. The *write* that consumes a
//! mint, `Workspace::register`, is in [`crate::workspace`].

pub use prov_graph::identity::*;
