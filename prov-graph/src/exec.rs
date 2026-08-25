//! A dependency-free executor for backends whose futures are already ready.
//!
//! Re-exported from [`prov_transaction::exec`], which owns it because the
//! filesystem port it drives lives there too. See that module for what
//! [`block_on`] does and does not promise.

pub use prov_transaction::exec::block_on;
