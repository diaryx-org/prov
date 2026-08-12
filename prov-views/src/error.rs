//! What can go wrong executing a view.

use std::fmt;

/// The result of executing a view.
pub type Result<T> = std::result::Result<T, Error>;

/// A view could not be executed.
#[derive(Debug)]
pub enum Error {
    /// Reading the workspace failed.
    Graph(prov_graph::error::Error),
    /// The view's `under:` names nothing this workspace can resolve.
    ///
    /// Deliberately not folded into an empty result — see
    /// [`select`](fn@crate::select).
    AnchorUnresolved {
        /// The view that declared it.
        view: String,
        /// The anchor exactly as written.
        under: String,
        /// Why it did not resolve, in a sentence a user can act on.
        why: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Graph(e) => write!(f, "{e}"),
            Error::AnchorUnresolved { view, under, why } => write!(
                f,
                "the view `{view}` is anchored under `{under}`, but {why}"
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Graph(e) => Some(e),
            Error::AnchorUnresolved { .. } => None,
        }
    }
}

impl From<prov_graph::error::Error> for Error {
    fn from(error: prov_graph::error::Error) -> Self {
        Error::Graph(error)
    }
}
