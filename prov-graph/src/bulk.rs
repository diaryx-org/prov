//! A backend's notice that a whole-tree read is starting — the hook a storage
//! layer priced per *call* uses to pay once for a pass instead of once per
//! document.
//!
//! prov reads a workspace one document at a time, through [`ReadStorage`],
//! and most backends charge nothing for the asking: `std::fs` opens a file
//! in microseconds, and the census's cost is the bytes. A backend that
//! serialises access against something else — a coordinated filesystem under
//! a sync daemon, a file provider, a remote store — charges for each *call*:
//! every read is a round trip before the first byte arrives, and a walk over
//! a few thousand documents is a few thousand round trips, which is the
//! difference between a rename that returns at once and one that returns in
//! half a minute. Such a backend can usually pay once for the whole tree
//! instead — one coordination scope over the root that covers everything
//! beneath it — but only if it is told that a whole-tree read is about to
//! happen, and told again when it is over, so the scope never spans a write.
//!
//! [`BulkReads`] is that notice. The read core calls
//! [`begin`](BulkReads::begin) before the census walk reads its first
//! document and [`end`](BulkReads::end) after it reads its last — and before
//! it returns, so a verb that walks and then writes (`rename`, `retitle`,
//! every mutation that asks who links here) has already closed the scope by
//! the time its change set lands. The walk is the one place the notice is
//! given: it is the read that visits everything, every other pass either
//! composes it or reads a handful of documents, and a backend that needs to
//! batch those too can nest — a `begin` inside an open scope is the
//! backend's to treat as already covered.
//!
//! Nothing is memoised here. The read memo ([`crate::memo`]) already keeps
//! what the walk read for the rest of the operation, so a mutation that
//! loads the documents it rewrites after the scope has closed finds them
//! remembered and reads nothing through the backend again. The two are
//! complementary: the memo saves the second read, this saves the price of
//! the first.
//!
//! A graph built without a hook behaves exactly as before; the default is
//! no notice, and `std::fs` has no use for one.
//!
//! [`ReadStorage`]: crate::fs::ReadStorage

use std::fmt;
use std::path::Path;
use std::sync::Arc;

/// A storage backend's hook for a whole-tree read pass. See the module docs.
///
/// Both calls are synchronous and infallible on purpose: a backend that
/// cannot batch simply does nothing, because batching is a performance
/// outcome, and the pass must run either way. `begin` is handed the graph's
/// root — the directory the pass will stay inside — and every `begin` is
/// balanced by exactly one `end`, including when the pass unwinds on an
/// error, because the read core holds it as a guard.
pub trait BulkReads: Send + Sync {
    /// A whole-tree read under `root` is about to start.
    fn begin(&self, root: &Path);

    /// The read that the last `begin` announced has finished. Nothing the
    /// pass read will be read again through the backend before the
    /// operation's next `begin`, unless the operation writes first.
    fn end(&self);
}

/// The hook as the graph holds it — shared, because a graph is cloned
/// wholesale and every copy is a view of the same backend. A newtype so that
/// the graph and its builders stay `Debug` without asking the backend to be.
#[derive(Clone)]
pub struct Hook(Arc<dyn BulkReads>);

impl Hook {
    /// Wrap a backend's hook.
    pub fn new(hook: Arc<dyn BulkReads>) -> Self {
        Hook(hook)
    }

    /// The backend's hook, shared.
    pub fn get(&self) -> Arc<dyn BulkReads> {
        Arc::clone(&self.0)
    }

    pub(crate) fn as_dyn(&self) -> &dyn BulkReads {
        self.0.as_ref()
    }
}

impl fmt::Debug for Hook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BulkReads")
    }
}

/// An open pass. Dropping it is the `end` call, which is what makes the
/// balance something a backend can rely on rather than hope for.
#[must_use]
pub(crate) struct Pass<'a>(&'a dyn BulkReads);

impl<'a> Pass<'a> {
    pub(crate) fn open(hook: &'a dyn BulkReads, root: &Path) -> Self {
        hook.begin(root);
        Pass(hook)
    }
}

impl Drop for Pass<'_> {
    fn drop(&mut self) {
        self.0.end();
    }
}
