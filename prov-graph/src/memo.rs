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
//! ## Directories, for the same reason and at a worse ratio
//!
//! The memo holds *listings* beside documents, because the same argument runs
//! harder there. Resolution asks whether a link's target exists and — when it
//! does not — whether some entry beside it differs only in case, which
//! [`exact_name`] answers by reading the target's parent directory. That is one
//! directory read **per link**, where the document memo saves one read per
//! *pass*: a flat workspace of N documents holding N links into one directory
//! read that directory N times, each read enumerating N entries, and `check`
//! went quadratic on exactly that. Listings are also indexed rather than
//! stored raw (`DirNames`), so the answer is a hash lookup and not a scan of
//! everything the directory holds.
//!
//! [`exact_name`]: crate::graph::Graph
//!
//! ## Why the scope, and why it is explicit
//!
//! A memo with no end is a cache, and a cache has to be invalidated. A memo
//! bounded by an *operation* barely has to be: nothing outside prov can write
//! to the workspace between two of `check`'s sub-passes in any sense prov could
//! have detected anyway, and everything inside prov that writes goes through
//! `prov`'s `ChangeSet`, which forgets what it touched
//! ([`Workspace::commit`]). There is no staleness window left that a stat could
//! have closed and this cannot.
//!
//! The scope is explicit — a caller opens one with
//! [`Graph::read_scope`](crate::graph::Graph::read_scope) and holds the guard — because a memo that switched
//! itself on would be a cache again, and because the caller is the only one who
//! knows where its operation begins. Scopes nest: an operation that opens one
//! and then calls another that opens its own gets a single memo lasting the
//! outer one, which is exactly the composition case (`prov`'s `check` opens a
//! scope and then calls `walk`, which is welcome to open its own).
//!
//! ## Links, beside the document they were read from
//!
//! A body's links are a pure function of its text, and finding them means
//! parsing the prose, which costs more than reading it. The census does it for
//! every document it reaches, and a mutation asking who links to a document
//! builds an inbound index that does it again. So a remembered document can
//! carry its links too, held only while the body they were scanned from is
//! the body remembered.
//!
//! ## What is not memoized
//!
//! Failures. A read that errored is not remembered, so a missing file is
//! re-checked rather than pinned to its first answer — the cost is one syscall
//! on a path prov already knows is trouble, and the alternative is a memo that
//! can report a file absent after it appeared.
//!
//! [`check`]: https://docs.rs/prov
//! [`load`]: crate::graph::Graph::load
//! [`Workspace::commit`]: https://docs.rs/prov

use crate::hash::{FastMap, FastSet};
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::document::Loaded;
use crate::link::BodyLink;

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
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
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

/// One directory's entry names, indexed for the two questions a name lookup
/// asks: is this name here, and is a name that differs from it only in ASCII
/// case here?
///
/// Two maps rather than one, because a directory on a case-sensitive
/// filesystem may hold both `Notes.md` and `notes.md`, and an exact match must
/// win however the listing happened to be ordered — a single folded map would
/// answer "the other one" whenever the wrong one was inserted last.
///
/// Keyed by [`OsString`], not `String`: the scan this replaced compared
/// [`OsStr`]s, so a name that is not UTF-8 still matched itself exactly, and
/// dropping such an entry here would turn a document prov can open into one it
/// reports as missing.
///
/// [`OsStr`]: std::ffi::OsStr
#[derive(Debug, Default)]
pub(crate) struct DirNames {
    /// The listing itself, for a scan that walks the directory rather than
    /// asking it about one name.
    entries: Vec<crate::fs::DirEntry>,
    /// Every entry name, as listed.
    exact: FastSet<OsString>,
    /// ASCII-lowercased name → the name as listed. Last writer wins, which only
    /// matters when a directory holds two names differing in case, and then only
    /// for the *inexact* answer — [`exact`](Self::exact) has already settled the
    /// other one.
    folded: FastMap<OsString, OsString>,
}

impl DirNames {
    /// Index a directory read.
    pub(crate) fn index(entries: Vec<crate::fs::DirEntry>) -> Self {
        let mut names = Self::default();
        for entry in &entries {
            let Some(name) = entry.file_name() else {
                continue;
            };
            names
                .folded
                .insert(name.to_ascii_lowercase(), name.to_os_string());
            names.exact.insert(name.to_os_string());
        }
        names.entries = entries;
        names
    }

    /// The directory's entries, as listed.
    pub(crate) fn entries(&self) -> &[crate::fs::DirEntry] {
        &self.entries
    }

    /// Whether the directory holds exactly this name.
    pub(crate) fn holds(&self, name: &OsStr) -> bool {
        self.exact.contains(name)
    }

    /// The name the directory holds that differs from `name` in ASCII case
    /// alone, if any. Ask [`holds`](Self::holds) first: this will happily
    /// return `name` itself.
    pub(crate) fn case_variant(&self, name: &OsStr) -> Option<&OsStr> {
        self.folded
            .get(&name.to_ascii_lowercase())
            .map(OsString::as_os_str)
    }

    /// Roughly the bytes this holds, for the memo's budget. Approximate on
    /// purpose — the budget is a ceiling that keeps an unusual workspace from
    /// eating memory, not an allocator.
    fn weight(&self) -> usize {
        let listed = self
            .entries
            .iter()
            .map(|e| e.path().as_os_str().len())
            .sum::<usize>();
        let names = listed + self.exact.iter().map(|n| n.len()).sum::<usize>();
        let folded = self
            .folded
            .iter()
            .map(|(k, v)| k.len() + v.len())
            .sum::<usize>();
        names + folded
    }
}

/// How many documents a memo must hold before freeing them is worth handing
/// to another thread. Below it, the hand-off costs more than it saves.
#[cfg(all(feature = "parallel", not(target_family = "wasm")))]
const DISCARD_ELSEWHERE: usize = 1024;

/// Free what an operation remembered, without making the operation wait.
///
/// Dropping ten thousand documents is a free per string and per value in
/// each, and it was a tenth of a `check`'s main thread, spent after the
/// answer was known. With the `parallel` feature, a large memo is dropped on
/// rayon's pool instead, so the caller returns at once; a process that exits
/// first simply never pays. Nothing observes the memory, so nothing waits on
/// it.
fn discard(
    remembered: (
        FastMap<OsString, Remembered>,
        FastMap<OsString, Arc<DirNames>>,
    ),
) {
    #[cfg(all(feature = "parallel", not(target_family = "wasm")))]
    if remembered.0.len() >= DISCARD_ELSEWHERE {
        rayon::spawn(move || drop(remembered));
        return;
    }
    drop(remembered);
}

/// One document as read, shared with whoever asked for it, and once someone
/// has asked, the links in its body.
#[derive(Debug)]
struct Remembered {
    read: Arc<Loaded>,
    links: Option<Arc<[BodyLink]>>,
}

impl Remembered {
    /// Whether `body` is the body this document was remembered with. Almost
    /// always asked of the very string the memo handed out, so the pointer
    /// settles it before any byte is compared.
    fn has_body(&self, body: &str) -> bool {
        let ours = self.read.doc.body.as_str();
        std::ptr::eq(ours, body) || ours == body
    }
}

/// What one operation has already read.
///
/// Keyed by a path's bytes rather than by [`Path`]: `Path`'s hash walks it a
/// component at a time, a hasher call per component, and the memo is asked
/// once per link per document. The paths prov asks about are normalized, so
/// the two agree; where a caller spells one path two ways (`a//b`), the worst
/// is a second read.
#[derive(Debug, Default)]
pub struct ReadMemo {
    /// Scope nesting depth. Nothing is remembered outside a scope, and the
    /// outermost exit is what clears it.
    depth: usize,
    docs: FastMap<OsString, Remembered>,
    /// Listings held, keyed workspace-relative like [`docs`](Self::docs) — which
    /// is what lets [`forget`](Self::forget) drop the one a write invalidates.
    dirs: FastMap<OsString, Arc<DirNames>>,
    /// Text and listing names held, against [`BUDGET`].
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
    pub(crate) fn get(&self, path: &Path) -> Option<Arc<Loaded>> {
        if self.depth == 0 {
            return None;
        }
        self.docs
            .get(path.as_os_str())
            .map(|hit| Arc::clone(&hit.read))
    }

    /// Whether a read would be answered here: a scope is open and `path` is
    /// remembered.
    #[cfg(all(feature = "parallel", not(target_family = "wasm")))]
    pub(crate) fn holds(&self, path: &Path) -> bool {
        self.depth > 0 && self.docs.contains_key(path.as_os_str())
    }

    /// Whether the budget is spent: another document of any size the memo
    /// keeps might not fit. What tells a prefetch that reading further ahead
    /// would read what cannot be kept.
    #[cfg(all(feature = "parallel", not(target_family = "wasm")))]
    pub(crate) fn spent(&self) -> bool {
        self.bytes + MAX_DOCUMENT > BUDGET
    }

    /// Whether a scope is open, so that something remembered now is still
    /// here for the rest of the operation.
    #[cfg(all(feature = "parallel", not(target_family = "wasm")))]
    pub(crate) fn in_scope(&self) -> bool {
        self.depth > 0
    }

    /// The links found in `path`'s body, when they were found in this
    /// operation and in a body that is still `body`.
    pub(crate) fn links(&self, path: &Path, body: &str) -> Option<Arc<[BodyLink]>> {
        if self.depth == 0 {
            return None;
        }
        let hit = self.docs.get(path.as_os_str())?;
        hit.has_body(body).then(|| hit.links.clone()).flatten()
    }

    /// Remember the links found in `body`, beside `path`'s document. A no-op
    /// unless that document is remembered with that body: links outlive
    /// nothing they were read from.
    pub(crate) fn remember_links(&mut self, path: &Path, body: &str, links: Arc<[BodyLink]>) {
        if self.depth == 0 {
            return;
        }
        if let Some(hit) = self.docs.get_mut(path.as_os_str())
            && hit.has_body(body)
            && hit.links.is_none()
        {
            hit.links = Some(links);
        }
    }

    /// Remember what `path` read as. A no-op outside a scope, over the
    /// per-document ceiling, or once the budget is spent.
    pub(crate) fn remember(&mut self, path: &Path, read: Arc<Loaded>) {
        let len = read.text.len();
        if self.depth == 0 || len > MAX_DOCUMENT || self.bytes + len > BUDGET {
            return;
        }
        self.bytes += len;
        let remembered = Remembered { read, links: None };
        if let Some(stale) = self.docs.insert(path.as_os_str().to_owned(), remembered) {
            self.bytes -= stale.read.text.len();
        }
    }

    /// The indexed listing of the workspace-relative directory `path`, if it
    /// was read this operation. `None` outside a scope, like
    /// [`get`](Self::get).
    pub(crate) fn dir(&self, path: &Path) -> Option<Arc<DirNames>> {
        if self.depth == 0 {
            return None;
        }
        self.dirs.get(path.as_os_str()).cloned()
    }

    /// Remember a directory read. A no-op outside a scope or once the budget is
    /// spent — a listing that is not remembered costs a re-read and nothing
    /// else.
    pub(crate) fn remember_dir(&mut self, path: &Path, names: Arc<DirNames>) {
        let weight = names.weight();
        if self.depth == 0 || self.bytes + weight > BUDGET {
            return;
        }
        self.bytes += weight;
        self.dirs.insert(path.as_os_str().to_owned(), names);
    }

    /// Forget `path` — what a write to it means.
    ///
    /// Every **ancestor's listing** goes too. A write may create the name or a
    /// remove may take it away, which only the parent's listing can be wrong
    /// about — but a write may also create the directories above it, and a
    /// scan that walks the tree lists those. So the operation that stages a
    /// change and then reads the workspace back never meets a directory as it
    /// stood beforehand.
    pub fn forget(&mut self, path: &Path) {
        if let Some(forgotten) = self.docs.remove(path.as_os_str()) {
            self.bytes -= forgotten.read.text.len();
        }
        for dir in path.ancestors().skip(1) {
            if let Some(names) = self.dirs.remove(dir.as_os_str()) {
                self.bytes -= names.weight();
            }
        }
    }

    pub(crate) fn clear(&mut self) {
        let docs = std::mem::take(&mut self.docs);
        let dirs = std::mem::take(&mut self.dirs);
        self.bytes = 0;
        discard((docs, dirs));
    }

    /// How many documents are remembered — the observable the tests assert on.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.docs.len()
    }

    /// How many directory listings are remembered, likewise.
    #[cfg(test)]
    pub(crate) fn dirs_len(&self) -> usize {
        self.dirs.len()
    }
}

/// An open read scope. Hold it for the operation; dropping it leaves the scope,
/// and dropping the outermost one drops everything the operation remembered.
///
/// Obtained from [`Graph::read_scope`](crate::graph::Graph::read_scope).
///
/// ## Why it holds the memo rather than borrowing it
///
/// A guard that borrowed its graph would be unusable from the operations that
/// need it most. A mutating verb reads (a census, a subtree walk), *then*
/// stages and commits — and `commit` takes `&mut self`, which an outstanding
/// `&self` borrow forbids. Every verb in `prov`'s `mutate` is that shape, so a
/// borrowing guard could only be held across the read half and dropped before
/// the writes, which in most of them is before the expensive pass has even
/// started. Sharing the memo instead is what lets one scope cover a whole verb.
///
/// The lifetime tie was also a (weak) argument that a scope cannot be stashed
/// and left open — "a memo with no end is a cache". That argument is now
/// discipline rather than a type: hold the guard in a local, for one operation.
/// What has not changed is that nothing outlives it — dropping the outermost
/// guard clears the memo, whether or not the graph is still around.
#[must_use = "a read scope ends the moment its guard is dropped"]
pub struct ReadScope(Arc<Mutex<ReadMemo>>);

impl ReadScope {
    /// Enter the scope guarded by `memo`.
    pub(crate) fn open(memo: &Arc<Mutex<ReadMemo>>) -> Self {
        lock(memo).enter();
        ReadScope(Arc::clone(memo))
    }
}

impl Drop for ReadScope {
    fn drop(&mut self) {
        lock(&self.0).leave();
    }
}

impl std::fmt::Debug for ReadScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadScope")
            .field("depth", &lock(&self.0).depth)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::document::Document;

    fn doc(text: &str) -> Document {
        Document::parse("a.md", text).unwrap()
    }

    fn loaded(text: &str) -> Arc<Loaded> {
        Arc::new(Loaded {
            text: text.into(),
            doc: doc(text),
        })
    }

    #[test]
    fn nothing_is_remembered_outside_a_scope() {
        let mut memo = ReadMemo::default();
        memo.remember(Path::new("a.md"), loaded("hello"));
        assert_eq!(memo.len(), 0);
        assert!(memo.get(Path::new("a.md")).is_none());
    }

    #[test]
    fn a_scope_remembers_and_its_exit_forgets() {
        let mut memo = ReadMemo::default();
        memo.enter();
        memo.remember(Path::new("a.md"), loaded("hello"));
        assert_eq!(
            memo.get(Path::new("a.md")).map(|read| read.text.clone()),
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
        memo.remember(Path::new("a.md"), loaded("hello"));
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
        memo.remember(Path::new("a.md"), loaded("hello"));
        memo.forget(Path::new("a.md"));
        assert!(memo.get(Path::new("a.md")).is_none());
        assert_eq!(memo.len(), 0);
    }

    fn links(body: &str) -> Arc<[BodyLink]> {
        crate::link::scan_body_links(Path::new("a.md"), body).into()
    }

    #[test]
    fn links_are_remembered_beside_the_body_they_were_found_in() {
        let mut memo = ReadMemo::default();
        memo.enter();
        let text = "---\ntitle: A\n---\nsee [b](b.md)\n";
        let read = doc(text);
        memo.remember(
            Path::new("a.md"),
            Arc::new(Loaded {
                text: text.into(),
                doc: read.clone(),
            }),
        );
        memo.remember_links(Path::new("a.md"), &read.body, links(&read.body));
        assert_eq!(
            memo.links(Path::new("a.md"), &read.body).map(|l| l.len()),
            Some(1)
        );
        assert!(
            memo.links(Path::new("a.md"), "see [c](c.md)\n").is_none(),
            "links found in one body answered for another"
        );
        memo.forget(Path::new("a.md"));
        assert!(memo.links(Path::new("a.md"), &read.body).is_none());
    }

    #[test]
    fn links_are_not_remembered_for_a_document_that_is_not() {
        let mut memo = ReadMemo::default();
        memo.enter();
        memo.remember_links(
            Path::new("a.md"),
            "see [b](b.md)\n",
            links("see [b](b.md)\n"),
        );
        assert!(memo.links(Path::new("a.md"), "see [b](b.md)\n").is_none());
    }

    fn names(entries: &[&str]) -> Arc<DirNames> {
        let entries: Vec<crate::fs::DirEntry> = entries
            .iter()
            .map(|n| crate::fs::DirEntry::new(*n, crate::fs::FileType::FILE))
            .collect();
        Arc::new(DirNames::index(entries))
    }

    #[test]
    fn a_listing_answers_an_exact_name_and_a_case_variant_apart() {
        let names = names(&["Notes.md", "photo.jpg"]);
        assert!(names.holds(OsStr::new("Notes.md")));
        assert!(!names.holds(OsStr::new("notes.md")));
        assert_eq!(
            names.case_variant(OsStr::new("notes.md")),
            Some(OsStr::new("Notes.md"))
        );
        assert_eq!(names.case_variant(OsStr::new("gone.md")), None);
    }

    /// Both spellings are present, so the exact one must win however the
    /// listing was ordered — the reason the index keeps two maps.
    #[test]
    fn an_exact_name_wins_over_a_case_variant_of_itself() {
        let names = names(&["notes.md", "Notes.md"]);
        assert!(names.holds(OsStr::new("notes.md")));
        assert!(names.holds(OsStr::new("Notes.md")));
    }

    #[test]
    fn a_scope_remembers_a_directory_and_its_exit_forgets() {
        let mut memo = ReadMemo::default();
        memo.remember_dir(Path::new("notes"), names(&["a.md"]));
        assert_eq!(memo.dirs_len(), 0, "nothing is remembered outside a scope");

        memo.enter();
        memo.remember_dir(Path::new("notes"), names(&["a.md"]));
        assert!(memo.dir(Path::new("notes")).is_some());
        memo.leave();
        assert_eq!(memo.dirs_len(), 0);
    }

    /// A write creates or removes a name, so the listing that enumerated the
    /// parent is stale — and it is the only listing that can be.
    #[test]
    fn forgetting_a_written_document_forgets_its_parent_listing() {
        let mut memo = ReadMemo::default();
        memo.enter();
        memo.remember_dir(Path::new("notes"), names(&["a.md"]));
        memo.remember_dir(Path::new("other"), names(&["b.md"]));

        memo.forget(Path::new("notes/new.md"));
        assert!(
            memo.dir(Path::new("notes")).is_none(),
            "the directory the write lands in still answers from before it"
        );
        assert!(
            memo.dir(Path::new("other")).is_some(),
            "an unrelated directory was dropped"
        );
    }

    /// A write may create every directory above it, and a scan that walks the
    /// tree lists them, so each listing above the write goes, and only those.
    #[test]
    fn forgetting_a_written_document_forgets_every_listing_above_it() {
        let mut memo = ReadMemo::default();
        memo.enter();
        for dir in ["", "a", "a/b", "a/b/c", "a/x"] {
            memo.remember_dir(Path::new(dir), names(&["n.md"]));
        }
        memo.forget(Path::new("a/b/c/new.md"));
        for dir in ["", "a", "a/b", "a/b/c"] {
            assert!(memo.dir(Path::new(dir)).is_none(), "{dir:?} kept");
        }
        assert!(
            memo.dir(Path::new("a/x")).is_some(),
            "a sibling was dropped"
        );
    }

    #[test]
    fn a_listing_keeps_its_entries() {
        let listing = names(&["b.md", "a.md"]);
        let listed: Vec<_> = listing
            .entries()
            .iter()
            .filter_map(|e| e.file_name())
            .collect();
        assert_eq!(listed, ["b.md", "a.md"]);
    }

    /// A write at the workspace root forgets the root listing — `parent()` of a
    /// bare name is the empty path, which is how the root is keyed.
    #[test]
    fn forgetting_a_root_document_forgets_the_root_listing() {
        let mut memo = ReadMemo::default();
        memo.enter();
        memo.remember_dir(Path::new(""), names(&["index.md"]));
        memo.forget(Path::new("new.md"));
        assert!(memo.dir(Path::new("")).is_none());
    }

    /// A memo is an optimization, so exceeding its budget must cost speed and
    /// nothing else — the reads simply stop being remembered.
    #[test]
    fn an_oversized_document_is_not_remembered() {
        let mut memo = ReadMemo::default();
        memo.enter();
        let huge = "x".repeat(MAX_DOCUMENT + 1);
        memo.remember(
            Path::new("big.md"),
            Arc::new(Loaded {
                text: huge.clone(),
                doc: doc("body"),
            }),
        );
        assert_eq!(memo.len(), 0);
    }
}
