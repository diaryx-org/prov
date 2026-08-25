//! What a transaction can fail with.
//!
//! Five variants, and the split between them is the crate's whole safety story:
//! [`Io`](Error::Io) and [`Escape`](Error::Escape) are ordinary refusals that
//! leave the target untouched, [`Corrupt`](Error::Corrupt) and
//! [`StaleJournal`](Error::StaleJournal) are recovery refusing to guess, and
//! [`Torn`](Error::Torn) is the one case where the state on disk cannot be
//! named.
//!
//! There is no `thiserror` here on purpose — the crate has no dependencies, and
//! five variants do not need a derive to spell out.

use std::fmt;
use std::io;
use std::path::PathBuf;

/// A transaction result.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything applying or recovering a [`ChangeSet`](crate::ChangeSet) can fail
/// with.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The backend refused an operation. The staged set is unwound before this
    /// surfaces, so the tree is as it was.
    Io(io::Error),

    /// A staged path resolved outside the root it was applied against — either
    /// absolute, or climbing past the root with `..`. Refused before anything is
    /// written or journaled, because a set assembled from untrusted data must
    /// not be able to reach out of the tree it was pointed at.
    Escape(PathBuf),

    /// A [`Journal`](crate::journal::Journal) was asked for under a name that
    /// is not a single path component. Refused at construction, because the
    /// name is joined onto a caller-supplied root and one containing `..` or a
    /// separator would write outside the very tree an apply clamps into.
    InvalidJournalName(String),

    /// A staged path could not be encoded into the journal because it is not
    /// UTF-8. The journal stores paths as UTF-8 so that a set written on one
    /// platform replays identically on another; a path that cannot round-trip
    /// is refused at the commit point rather than silently mangled.
    NonUtf8Path(PathBuf),

    /// A journal was found that could not be trusted: a bad magic, a checksum
    /// mismatch, a truncated record, an unknown op tag. Refused rather than
    /// partially replayed — a journal exists to prevent invented states, so one
    /// that cannot be read is never guessed at.
    Corrupt(String),

    /// A journal was read successfully but could not be replayed to completion:
    /// a [`CopyFrom`](crate::FileOp::CopyFrom) whose source has gone, or a
    /// rename with neither side present. Distinct from
    /// [`Corrupt`](Error::Corrupt) — the intent was legible, the tree just
    /// could not be brought to it. The journal is left in place so a later
    /// recovery can finish once the missing piece is back.
    Recovery(String),

    /// A set was applied while a *previous* set's journal was still on disk: an
    /// earlier change was interrupted and never recovered. Landing this set
    /// would overwrite the record needed to finish that one, so the apply
    /// refuses. Call [`recover`](crate::recover) first, then retry.
    StaleJournal(PathBuf),

    /// A staged op failed *and* the rollback that should have undone it failed
    /// too. The one case where the crate cannot say what is on disk — so it says
    /// exactly that, rather than reporting the original failure as if the tree
    /// were untouched.
    ///
    /// The journal is deliberately left in place when this is returned, so the
    /// next [`recover`](crate::recover) rolls the set *forward* to the applied
    /// state. Either way the tree lands somewhere nameable.
    Torn {
        /// The failure that triggered the rollback.
        cause: String,
        /// The failure the rollback itself hit.
        rollback: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io error: {e}"),
            Error::Escape(p) => write!(f, "path escapes the root: {}", p.display()),
            Error::InvalidJournalName(name) => write!(
                f,
                "journal name must be a single path component, got {name:?}"
            ),
            Error::NonUtf8Path(p) => {
                write!(f, "journal cannot encode non-UTF-8 path: {}", p.display())
            }
            Error::Corrupt(what) => write!(f, "journal is corrupt: {what}"),
            Error::Recovery(what) => write!(f, "journal replay: {what}"),
            Error::StaleJournal(p) => write!(
                f,
                "a previous change was interrupted and not yet recovered (found {}); \
                 recover it first, then retry",
                p.display()
            ),
            Error::Torn { cause, rollback } => write!(
                f,
                "{cause}; and rolling back failed too: {rollback}. \
                 The tree may be partially written — run recovery."
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}
