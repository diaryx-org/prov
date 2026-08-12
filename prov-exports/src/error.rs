//! What can go wrong planning an export.

use std::fmt;

/// The result of planning an export.
pub type Result<T> = std::result::Result<T, Error>;

/// An export could not be planned.
#[derive(Debug)]
pub enum Error {
    /// Reading the workspace failed.
    Graph(prov_graph::error::Error),
    /// The export names a view and that view could not be executed — its
    /// anchor resolves to nothing, or the read under it failed.
    ///
    /// Passed through rather than softened to "no view", because the two mean
    /// opposite things: an export whose view is broken must not fall back to
    /// exporting the gate's whole set. That would be the valve failing *open*.
    View(prov_views::Error),
    /// The export names a view this workspace does not declare.
    ///
    /// An error, not an unarranged export, for the same reason: the absent
    /// view was written down as a bound on what leaves, and the fail-closed
    /// reading of a bound nobody can find is to export nothing.
    ViewUnknown {
        /// The export that named it.
        export: String,
        /// The view name exactly as written.
        view: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Graph(e) => write!(f, "{e}"),
            Error::View(e) => write!(f, "{e}"),
            Error::ViewUnknown { export, view } => write!(
                f,
                "the export `{export}` is arranged by the view `{view}`, \
                 but this workspace declares no view by that name"
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Graph(e) => Some(e),
            Error::View(e) => Some(e),
            Error::ViewUnknown { .. } => None,
        }
    }
}

impl From<prov_graph::error::Error> for Error {
    fn from(error: prov_graph::error::Error) -> Self {
        Error::Graph(error)
    }
}

impl From<prov_views::Error> for Error {
    fn from(error: prov_views::Error) -> Self {
        Error::View(error)
    }
}
