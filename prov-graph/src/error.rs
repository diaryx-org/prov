//! Error and result types.

use std::path::PathBuf;

use thiserror::Error;

/// Errors produced by prov.
#[derive(Debug, Error)]
pub enum Error {
    /// The embedded-metadata backend (`fig`) failed to parse or serialize.
    #[error("metadata error: {0}")]
    Meta(#[from] fig::Error),

    /// A structural invariant was violated (e.g. malformed frontmatter fence).
    #[error("{0}")]
    Structure(String),

    /// A document a workspace operation names is not on disk — the typed form of
    /// the many "X does not exist" guards the mutation ops make before touching a
    /// document (`reparent`, `rename`, `duplicate`, `register`, …). A caller can
    /// tell a genuinely-missing target from a malformed one by matching the
    /// variant, rather than sniffing the message text.
    #[error("{0} does not exist")]
    NotFound(PathBuf),

    /// A workspace operation would create a document where one already exists, and
    /// refused rather than overwrite it — the typed form of the "X already exists"
    /// guards in `create`/`rename`/`attach`. A destination collision is a distinct
    /// outcome from a missing source, and now distinguishable as one.
    #[error("{0} already exists")]
    AlreadyExists(PathBuf),

    /// The storage backend failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// The `twig` body parser failed — see `content.rs`.
    #[error("content error: {0}")]
    Content(String),

    /// A record store — the id registry, the recycle-bin index, or a flat
    /// vocabulary — was found in a **markdown** carrier (fenced frontmatter)
    /// rather than a whole-file config document (`.yaml`/`.json`/`.figl`). prov
    /// imposes a sorted, one-record-per-line layout on these stores (DESIGN §5),
    /// so a prose carrier has no stable home for its records and is refused. Make
    /// it a bare config file. See [`crate::document::require_whole_file`].
    #[error(
        "record store must be a whole-file config document (.yaml/.json/.figl), \
         not markdown frontmatter: {0}"
    )]
    MarkdownStore(PathBuf),

    /// A path handed to a workspace read or write resolved *outside* the
    /// workspace root — an absolute path, or one that climbs above the root with
    /// `..`. prov clamps every I/O to the tree it was pointed at (a link
    /// target is data, and data must never be able to name `/etc/passwd` or a
    /// sibling repo), so such a path is refused rather than followed. See
    /// [`crate::link::escapes_root`], the guard at `prov`'s `Workspace`'s `load`
    /// and `prov`'s `ChangeSet::apply`.
    #[error("path escapes the workspace root: {0}")]
    Escape(PathBuf),

    /// A `prov`'s `ChangeSet` was applied while a previous change's
    /// write-ahead journal was still on disk — an earlier mutation was
    /// interrupted (a crash) and never recovered. Landing this set would
    /// overwrite that journal and lose the record needed to complete the
    /// interrupted change, so the apply refuses: run recovery
    /// (`prov`'s `journal::recover`, which `prov check` performs) first, then
    /// retry.
    #[error(
        "a previous change was interrupted and not yet recovered (found {0}); \
         recover it first (run `prov check`), then retry"
    )]
    StaleJournal(PathBuf),

    /// An apply could not deliver either of its durable answers — see
    /// `prov`'s `ChangeSet::apply`. The classic case: a staged write failed
    /// *and* the rollback that should have undone it also failed. Since
    /// fs-transaction 0.2 the same shape also covers a rollback or a
    /// completed apply whose certification (or journal retirement) could not
    /// be made durable. In every case prov cannot stand behind a clean
    /// endpoint, so it says exactly that instead of reporting the original
    /// failure as if the workspace were untouched — and `prov check`'s
    /// recovery is what resolves the workspace to a nameable state.
    #[error(
        "{cause}; and rolling back failed too: {rollback}. \
         The workspace may be partially written — run `prov check`."
    )]
    Torn {
        /// The failure that triggered the rollback.
        cause: String,
        /// The failure the rollback itself hit.
        rollback: String,
    },

    /// An operation would have registered an ID across a registration the index
    /// already holds — see [`Collision`](crate::index::Collision). Refused rather
    /// than resolved, because the displaced document still spells the ID in its
    /// own frontmatter and only its author can say which one should keep it.
    #[error("{0}; refusing to displace it")]
    Collision(crate::index::Collision),
}

impl From<crate::index::Collision> for Error {
    fn from(collision: crate::index::Collision) -> Self {
        Error::Collision(collision)
    }
}

/// Convenience alias for results in this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Carry a transaction failure into prov's own error vocabulary.
///
/// [`fs_transaction`] phrases its errors for a generic tree of files, since
/// it knows nothing about workspaces. The variants map one-to-one onto prov's,
/// which restate them in terms a prov user can act on — naming `prov check` as
/// the recovery step, and a workspace root as the boundary that was crossed.
impl From<fs_transaction::Error> for Error {
    fn from(e: fs_transaction::Error) -> Self {
        use fs_transaction::Error as Tx;
        match e {
            Tx::Io(e) => Error::Io(e),
            Tx::Escape(path) => Error::Escape(path),
            Tx::StaleJournal(path) => Error::StaleJournal(path),
            Tx::Torn { cause, rollback } => Error::Torn { cause, rollback },
            // The three that have no prov-level counterpart: a journal prov
            // cannot read, a replay it cannot finish, and a path it could not
            // encode. All are structural failures of the on-disk state, which
            // is what `Structure` names.
            Tx::NonUtf8Path(path) => {
                Error::Structure(format!("cannot journal non-UTF-8 path: {}", path.display()))
            }
            Tx::Corrupt(what) => Error::Structure(format!("journal is corrupt: {what}")),
            Tx::Recovery(what) => Error::Structure(format!("journal replay: {what}")),
            // `fs_transaction::Error` is `#[non_exhaustive]`: a variant added
            // upstream must not silently become a compile error here, but it
            // must not be mistaken for a prov-level failure either.
            other => Error::Structure(other.to_string()),
        }
    }
}
