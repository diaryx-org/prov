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
    #[error("content error: {0}")]
    Content(String),

    /// A staged write failed *and* the rollback that should have undone it also
    /// failed — see [`crate::change::ChangeSet::apply`]. The one case where
    /// colophon cannot say what is on disk, so it says exactly that instead of
    /// reporting the original failure as if the workspace were untouched.
    #[error(
        "{cause}; and rolling back failed too: {rollback}. \
         The workspace may be partially written — run `colophon check`."
    )]
    Torn {
        /// The failure that triggered the rollback.
        cause: String,
        /// The failure the rollback itself hit.
        rollback: String,
    },
}

/// Convenience alias for results in this crate.
pub type Result<T> = std::result::Result<T, Error>;
