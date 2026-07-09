//! Error and result types.

use thiserror::Error;

/// Errors produced by colophon.
#[derive(Debug, Error)]
pub enum Error {
    /// The embedded-metadata backend (`fig`) failed to parse or serialize.
    #[error("metadata error: {0}")]
    Meta(#[from] fig::Error),

    /// A structural invariant was violated (e.g. malformed frontmatter fence).
    #[error("{0}")]
    Structure(String),

    /// The storage backend failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// The `twig` body parser failed — see `content.rs`.
    #[cfg(feature = "content")]
    #[error("content error: {0}")]
    Content(String),
}

/// Convenience alias for results in this crate.
pub type Result<T> = std::result::Result<T, Error>;
