//! What can go wrong executing a view.

use std::fmt;

/// The result of executing a view.
pub type Result<T> = std::result::Result<T, Error>;

/// A view could not be executed.
///
/// Only reading the workspace can fail a whole view. An expression that fails
/// on one document is a [`Failure`](crate::Failure) beside the result, not an
/// error instead of it.
#[derive(Debug)]
pub enum Error {
    /// Reading the workspace failed.
    Graph(prov_graph::error::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Graph(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Graph(e) => Some(e),
        }
    }
}

impl From<prov_graph::error::Error> for Error {
    fn from(error: prov_graph::error::Error) -> Self {
        Error::Graph(error)
    }
}
