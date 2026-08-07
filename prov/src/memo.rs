//! A read memo with the lifetime of one operation — the cheap half of not
//! reading the same file twice.
//!
//! prov's passes are composed rather than fused: [`check`] runs a walk, then an
//! orphan sweep, then a fixity pass, then five more, and each of them starts
//! from a path and calls [`load`] on it. That composition is what makes the
//! passes independently testable and independently correct, and it is worth
//! keeping — but taken literally it means `check` reads and parses the same
//! document once per pass that cares about it. The walk loads every reachable
//! document to build the census; `fixity_findings` then loads every reachable
//! document again to hash its body. Two full reads and two full parses, for a
//! question the first read already answered.
//!
//! So remember the answer for as long as the operation lasts, and no longer.
//!
//! ## Why the scope, and why it is explicit
//!
//! A memo with no end is a cache, and a cache has to be invalidated. A memo
//! bounded by an *operation* barely has to be: nothing outside prov can write
//! to the workspace between two of `check`'s sub-passes in any sense prov could
//! have detected anyway, and everything inside prov that writes goes through
//! [`ChangeSet`](crate::change::ChangeSet), which forgets what it touched
//! ([`Workspace::commit`]). There is no staleness window left that a stat could
//! have closed and this cannot.
//!
//! The scope is explicit — a caller opens one with
//! [`Workspace::read_scope`] and holds the guard — because a memo that switched
//! itself on would be a cache again, and because the caller is the only one who
//! knows where its operation begins. Scopes nest: an operation that opens one
//! and then calls another that opens its own gets a single memo lasting the
//! outer one, which is exactly the composition case (`history_capture` opens a
//! scope and then calls `reachable_files`, which is welcome to open its own).
//!
//! ## What is not memoized
//!
//! Failures. A read that errored is not remembered, so a missing file is
//! re-checked rather than pinned to its first answer — the cost is one syscall
//! on a path prov already knows is trouble, and the alternative is a memo that
//! can report a file absent after it appeared.
//!
//! [`check`]: crate::Workspace::check
//! [`load`]: crate::Workspace::load
//! [`Workspace::commit`]: crate::Workspace

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use crate::document::Document;

/// Take a lock, recovering from a panic that poisoned it.
///
/// A memo and a cache are optimizations. Turning a panic that happened
/// elsewhere into a second panic here would be a bug of prov's own making, and
/// the worst that can actually be wrong behind a poisoned lock is a stale entry
/// — which every caller already tolerates by construction.
///
/// `Mutex` rather than `RefCell` for the whole family: `&Workspace` must stay
/// `Send` (`prov/tests/public_api.rs` pins that, so an embedder can drive
/// `apply` and `discover` from a multi-threaded runtime), which needs the
/// workspace itself to be `Sync`, which a `RefCell` is not. No guard is ever
/// held across an `.await`.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// How much document text one memo may hold. A workspace of a few thousand
/// markdown documents lands well under this; the cap is here so that a
/// workspace of unusually large documents degrades to "re-reads some of them"
/// rather than to holding the whole tree in memory at once.
const BUDGET: usize = 32 * 1024 * 1024;

/// The largest single document worth remembering. A file this big is rare
/// enough that keeping it would evict nothing useful and buy nothing.
const MAX_DOCUMENT: usize = 1024 * 1024;

/// What one operation has already read.
#[derive(Debug, Default)]
pub(crate) struct ReadMemo {
    /// Scope nesting depth. Nothing is remembered outside a scope, and the
    /// outermost exit is what clears it.
    depth: usize,
    docs: HashMap<PathBuf, (String, Document)>,
    /// Text held, against [`BUDGET`].
    bytes: usize,
}

impl ReadMemo {
    pub(crate) fn enter(&mut self) {
        self.depth += 1;
    }

    /// Leave one scope. Only the outermost exit drops what was remembered — an
    /// inner scope is covered by the one that encloses it.
    pub(crate) fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
        if self.depth == 0 {
            self.clear();
        }
    }

    /// What was read for `path` this operation, if anything. `None` outside a
    /// scope, always: a memo nobody opened holds nothing.
    pub(crate) fn get(&self, path: &Path) -> Option<(String, Document)> {
        if self.depth == 0 {
            return None;
        }
        self.docs.get(path).cloned()
    }

    /// Remember what `path` read as. A no-op outside a scope, over the
    /// per-document ceiling, or once the budget is spent.
    pub(crate) fn remember(&mut self, path: &Path, text: &str, doc: &Document) {
        if self.depth == 0 || text.len() > MAX_DOCUMENT || self.bytes + text.len() > BUDGET {
            return;
        }
        self.bytes += text.len();
        self.docs
            .insert(path.to_path_buf(), (text.to_string(), doc.clone()));
    }

    /// Forget `path` — what a write to it means.
    pub(crate) fn forget(&mut self, path: &Path) {
        if let Some((text, _)) = self.docs.remove(path) {
            self.bytes -= text.len();
        }
    }

    pub(crate) fn clear(&mut self) {
        self.docs.clear();
        self.bytes = 0;
    }

    /// How many documents are remembered — the observable the tests assert on.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.docs.len()
    }
}

/// An open read scope. Hold it for the operation; dropping it leaves the scope,
/// and dropping the outermost one drops everything the operation remembered.
///
/// Obtained from [`Workspace::read_scope`](crate::Workspace::read_scope).
#[must_use = "a read scope ends the moment its guard is dropped"]
pub struct ReadScope<'a>(pub(crate) &'a Mutex<ReadMemo>);

impl Drop for ReadScope<'_> {
    fn drop(&mut self) {
        lock(self.0).leave();
    }
}

impl std::fmt::Debug for ReadScope<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadScope")
            .field("depth", &lock(self.0).depth)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> Document {
        Document::parse("a.md", text).unwrap()
    }

    #[test]
    fn nothing_is_remembered_outside_a_scope() {
        let mut memo = ReadMemo::default();
        memo.remember(Path::new("a.md"), "hello", &doc("hello"));
        assert_eq!(memo.len(), 0);
        assert!(memo.get(Path::new("a.md")).is_none());
    }

    #[test]
    fn a_scope_remembers_and_its_exit_forgets() {
        let mut memo = ReadMemo::default();
        memo.enter();
        memo.remember(Path::new("a.md"), "hello", &doc("hello"));
        assert_eq!(
            memo.get(Path::new("a.md")).map(|(text, _)| text),
            Some("hello".to_string())
        );
        memo.leave();
        assert_eq!(memo.len(), 0);
    }

    /// The composition case: an operation that opens a scope and calls one that
    /// opens its own must not lose the memo when the inner one returns.
    #[test]
    fn an_inner_scope_does_not_end_the_outer_one() {
        let mut memo = ReadMemo::default();
        memo.enter();
        memo.remember(Path::new("a.md"), "hello", &doc("hello"));
        memo.enter();
        memo.leave();
        assert!(
            memo.get(Path::new("a.md")).is_some(),
            "an inner scope's exit dropped the outer scope's memo"
        );
        memo.leave();
        assert_eq!(memo.len(), 0);
    }

    #[test]
    fn a_write_forgets_the_document_it_wrote() {
        let mut memo = ReadMemo::default();
        memo.enter();
        memo.remember(Path::new("a.md"), "hello", &doc("hello"));
        memo.forget(Path::new("a.md"));
        assert!(memo.get(Path::new("a.md")).is_none());
        assert_eq!(memo.len(), 0);
    }

    /// A memo is an optimization, so exceeding its budget must cost speed and
    /// nothing else — the reads simply stop being remembered.
    #[test]
    fn an_oversized_document_is_not_remembered() {
        let mut memo = ReadMemo::default();
        memo.enter();
        let huge = "x".repeat(MAX_DOCUMENT + 1);
        memo.remember(Path::new("big.md"), &huge, &doc("body"));
        assert_eq!(memo.len(), 0);
    }
}
