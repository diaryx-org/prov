//! A device-local memory of what each file in a workspace hashed to last time.
//!
//! [`digest`](super::digest) is the cheapest thing in prov to describe and one
//! of the most expensive to run: it reads a whole file and pushes every byte
//! through SHA-256. A [`history_capture`] does that for *every* file in the
//! capture set, on every capture, whether or not anything changed — so a
//! workspace where one document was edited pays to read and hash the other
//! nine hundred to find that out.
//!
//! Almost none of them changed. So remember what they hashed to, and check
//! rather than read. Validating an entry costs one stat; a capture over a
//! workspace where nothing changed does no reads and no hashing at all.
//!
//! ## Why this may serve a capture and may never serve `check`
//!
//! An entry is served only when the file's modification time *and* its length
//! both still match what was recorded — the test every build system trusts. But
//! prov has a pass whose entire purpose is to detect changes that test cannot
//! see: [`fixity_findings`], the bit-rot check. Silent corruption is by
//! construction a change to the bytes that does not touch the inode's mtime or
//! length — a disk flipping a bit does not restat the file. A cache keyed on
//! mtime would confidently vouch for precisely the file that rotted.
//!
//! So the line is drawn in the callers, not here:
//!
//! > A remembered digest may **decide what to do**, and it may land somewhere
//! > content-addressed. It may never **establish or verify a fixity baseline**.
//!
//! [`history_capture`] is on the safe side of that line, and its own use is
//! narrower still: a remembered digest is used only when the blob it names is
//! *already parked*, so the bytes at that address are on disk and were hashed
//! from the real file when they got there. Whenever a capture actually reads a
//! file, it hashes the bytes it read. A stale entry can therefore cost an event
//! that misdescribes an instant; it can never park bytes under an address that
//! is not their digest, and it can never make `check` miss corruption — because
//! `check` does not ask.
//!
//! ## Why device-local, and not in the workspace
//!
//! A prov workspace is an archive of plain files that explains itself. A binary
//! cache is not part of that explanation, and two devices writing one would
//! produce sync conflicts over a file whose only job is to describe *this*
//! device's disk. It is also *derived state* in the sense DESIGN §5 means:
//! disposable, rebuildable, and load-bearing for nothing. Deleting it costs one
//! slow capture.
//!
//! Which is why this type does no I/O. It decodes from bytes and encodes to
//! bytes; where those bytes live is the host's business (`prov-cli` keeps them
//! under the user's cache directory), and prov itself stays free of any notion
//! of a location outside the workspace.
//!
//! ## Staleness
//!
//! Every failure mode — a missing file, a truncated one, the wrong magic, a
//! version this build does not know, a cache written for a different workspace —
//! decodes to the same answer: nothing is remembered. There is nothing in here
//! worth recovering, only re-deriving. And because the validator is the file's
//! own stat, *any* write by anyone — prov, an editor, a sync daemon — retires
//! the entry on its own. [`forget`](FixityCache::forget) is prov being tidy
//! about its own writes, not the mechanism that keeps this honest.
//!
//! [`history_capture`]: crate::Workspace::history_capture
//! [`fixity_findings`]: crate::Workspace::check

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use prov_graph::fs::Metadata;

/// Identifies the format, so a file written by another program is refused
/// rather than misread.
const MAGIC: &[u8; 8] = b"PROVFIXC";

/// Bumped whenever the layout below changes. A file at a version this build does
/// not know is discarded, not migrated — it is a cache.
const VERSION: u32 = 1;

/// The most files one cache may remember. Beyond this it stops growing rather
/// than becoming an index of the disk; the excess is re-hashed next time.
const MAX_ENTRIES: usize = 200_000;

/// One remembered file: the stat it was hashed at, and what it hashed to.
#[derive(Debug, Clone)]
struct Entry {
    /// Modification time in nanoseconds either side of the Unix epoch.
    ///
    /// Nanoseconds rather than the milliseconds a cache like this usually keeps,
    /// because the whole risk here is two different contents sharing one
    /// timestamp *and* one length, and the width of that window is the width of
    /// the clock's resolution. Signed, so a file stamped before 1970 — which a
    /// restored archive genuinely can be — records its real time instead of
    /// saturating at the epoch and colliding with everything else that did.
    mtime_ns: i128,
    len: u64,
    /// The digest, in [`digest`](super::digest)'s self-describing
    /// `sha256:<hex>` spelling. Stored as written rather than as raw bytes, so a
    /// future algorithm needs no format change and an entry this build cannot
    /// interpret is still legible to one that can.
    hash: String,
}

/// What this device remembers of a workspace's file digests.
///
/// Keyed by **workspace-relative path**, so a workspace that moves keeps its
/// cache; `root` is recorded only to refuse a cache that was written for a
/// different workspace entirely.
#[derive(Debug, Clone)]
pub struct FixityCache {
    root: PathBuf,
    entries: BTreeMap<PathBuf, Entry>,
    /// Whether anything has changed since it was decoded — a capture that
    /// learned nothing should not rewrite the file to say so.
    dirty: bool,
}

impl FixityCache {
    /// An empty cache for the workspace rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            entries: BTreeMap::new(),
            dirty: false,
        }
    }

    /// Decode what was persisted for the workspace at `root`.
    ///
    /// `None` for anything that is not exactly what some build of prov wrote
    /// for *this* workspace — the caller's move is to start
    /// [`new`](Self::new), never to investigate.
    pub fn decode(bytes: &[u8], root: &Path) -> Option<Self> {
        let mut r = Reader { bytes, at: 0 };
        if r.take(MAGIC.len())? != MAGIC {
            return None;
        }
        if r.u32()? != VERSION {
            return None;
        }
        let stored_root = r.string()?;
        if Path::new(&stored_root) != root {
            // Written for a workspace at a different path. Its entries may well
            // still describe real files, but nothing here proves it, and a wrong
            // guess would serve one workspace's digests as another's.
            return None;
        }
        let count = r.u32()? as usize;
        let mut entries = BTreeMap::new();
        for _ in 0..count {
            let rel = r.string()?;
            let mtime_ns = r.i128()?;
            let len = r.u64()?;
            let hash = r.string()?;
            entries.insert(
                PathBuf::from(rel),
                Entry {
                    mtime_ns,
                    len,
                    hash,
                },
            );
        }
        Some(Self {
            root: root.to_path_buf(),
            entries,
            dirty: false,
        })
    }

    /// The bytes to persist. Pair with [`is_dirty`](Self::is_dirty): a cache
    /// that learned nothing this run is worth writing to nobody.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.entries.len() * 128 + 64);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        push_str(&mut out, &self.root.to_string_lossy());
        out.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        // `BTreeMap`, so the bytes are a function of the contents and not of the
        // order they were learned in — two runs that saw the same workspace
        // write the same file.
        for (rel, entry) in &self.entries {
            push_str(&mut out, &rel.to_string_lossy());
            out.extend_from_slice(&entry.mtime_ns.to_le_bytes());
            out.extend_from_slice(&entry.len.to_le_bytes());
            push_str(&mut out, &entry.hash);
        }
        out
    }

    /// The remembered digest for the workspace-relative `path`, if the file
    /// `meta` describes is still the one it was recorded against.
    ///
    /// Both halves of the stat must agree: a length alone misses an edit that
    /// preserved the size, and a timestamp alone trusts a clock the file may
    /// have arrived with.
    pub fn get(&self, path: &Path, meta: &Metadata) -> Option<&str> {
        let stamp = stamp(meta)?;
        let entry = self.entries.get(path)?;
        (entry.mtime_ns == stamp && entry.len == meta.len()).then_some(entry.hash.as_str())
    }

    /// Remember that `path` hashed to `hash` at the stat `meta` describes.
    ///
    /// Three things are declined rather than stored wrong: a file whose backend
    /// reports no modification time (nothing could ever validate it, so keeping
    /// it would only cost space), a path that is not valid UTF-8 (it has no
    /// stable key, and a lossy one could collide with a different file), and
    /// anything at all once [`MAX_ENTRIES`] is reached.
    pub fn put(&mut self, path: &Path, meta: &Metadata, hash: &str) {
        let Some(mtime_ns) = stamp(meta) else { return };
        if path.to_str().is_none() || hash.is_empty() {
            return;
        }
        let entry = Entry {
            mtime_ns,
            len: meta.len(),
            hash: hash.to_string(),
        };
        match self.entries.get_mut(path) {
            Some(slot) => {
                if slot.mtime_ns == entry.mtime_ns
                    && slot.len == entry.len
                    && slot.hash == entry.hash
                {
                    return;
                }
                *slot = entry;
            }
            None => {
                if self.entries.len() >= MAX_ENTRIES {
                    return;
                }
                self.entries.insert(path.to_path_buf(), entry);
            }
        }
        self.dirty = true;
    }

    /// Forget `path` — what a write to it means.
    pub fn forget(&mut self, path: &Path) {
        if self.entries.remove(path).is_some() {
            self.dirty = true;
        }
    }

    /// Forget everything. For a write prov cannot attribute to one path.
    pub fn clear(&mut self) {
        if !self.entries.is_empty() {
            self.entries.clear();
            self.dirty = true;
        }
    }

    /// Whether anything has changed since this was decoded.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// The workspace this cache was built for.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// How many files are remembered.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is remembered.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A file's modification time as nanoseconds either side of the Unix epoch, or
/// `None` when the backend does not report one.
fn stamp(meta: &Metadata) -> Option<i128> {
    let modified = meta.modified().ok()?;
    Some(match modified.duration_since(UNIX_EPOCH) {
        Ok(since) => since.as_nanos() as i128,
        // Before the epoch — a real state for a restored archive, and one that
        // must stay distinguishable rather than clamping to zero.
        Err(before) => -(before.duration().as_nanos() as i128),
    })
}

fn push_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// A bounds-checked cursor. Every read returns `None` past the end, so a
/// truncated file falls out as "no cache" rather than a panic.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn i128(&mut self) -> Option<i128> {
        Some(i128::from_le_bytes(self.take(16)?.try_into().ok()?))
    }
    fn string(&mut self) -> Option<String> {
        let len = self.u32()? as usize;
        String::from_utf8(self.take(len)?.to_vec()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prov_graph::fs::FileType;
    use std::time::Duration;

    fn meta(secs: u64, len: u64) -> Metadata {
        Metadata::new(
            FileType::FILE,
            len,
            Some(UNIX_EPOCH + Duration::from_secs(secs)),
        )
    }

    /// A backend that reports no modification time — `InMemoryFs` is one.
    fn timeless(len: u64) -> Metadata {
        Metadata::new(FileType::FILE, len, None)
    }

    const HASH: &str = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn what_was_remembered_survives_a_round_trip() {
        let root = Path::new("/vault");
        let mut cache = FixityCache::new(root);
        cache.put(Path::new("index.md"), &meta(1_700_000_000, 5), HASH);
        cache.put(
            Path::new("notes/a.md"),
            &meta(1_700_000_001, 9),
            "sha256:beef",
        );

        let bytes = cache.encode();
        let reloaded = FixityCache::decode(&bytes, root).unwrap();
        assert_eq!(reloaded.len(), 2);
        assert_eq!(
            reloaded.get(Path::new("index.md"), &meta(1_700_000_000, 5)),
            Some(HASH)
        );
        assert_eq!(
            reloaded.get(Path::new("notes/a.md"), &meta(1_700_000_001, 9)),
            Some("sha256:beef")
        );
        assert!(!reloaded.is_dirty(), "a freshly decoded cache is not dirty");
    }

    /// The whole safety argument in one test: an entry is served only while both
    /// halves of the stat still agree.
    #[test]
    fn a_changed_file_is_not_served_from_the_cache() {
        let mut cache = FixityCache::new("/vault");
        let path = Path::new("index.md");
        cache.put(path, &meta(1_700_000_000, 5), HASH);

        assert!(cache.get(path, &meta(1_700_000_000, 5)).is_some());
        assert!(
            cache.get(path, &meta(1_700_000_001, 5)).is_none(),
            "a newer modification time is a different file"
        );
        assert!(
            cache.get(path, &meta(1_700_000_000, 6)).is_none(),
            "a different length is a different file"
        );
        assert!(
            cache.get(path, &timeless(5)).is_none(),
            "a backend that cannot say when is never trusted"
        );
    }

    #[test]
    fn a_file_with_no_modification_time_is_never_remembered() {
        let mut cache = FixityCache::new("/vault");
        cache.put(Path::new("index.md"), &timeless(5), HASH);
        assert_eq!(cache.len(), 0);
        assert!(!cache.is_dirty());
    }

    #[test]
    fn a_write_forgets_the_file_it_wrote() {
        let mut cache = FixityCache::new("/vault");
        let path = Path::new("index.md");
        cache.put(path, &meta(1, 5), HASH);
        cache.forget(path);
        assert!(cache.get(path, &meta(1, 5)).is_none());
        assert_eq!(cache.len(), 0);
    }

    /// A cache is not a database. Every way it can be wrong reads as nothing
    /// remembered.
    #[test]
    fn a_damaged_or_foreign_cache_decodes_to_nothing() {
        let root = Path::new("/vault");
        let mut cache = FixityCache::new(root);
        cache.put(Path::new("index.md"), &meta(1, 5), HASH);
        let good = cache.encode();

        assert!(
            FixityCache::decode(&good[..good.len() - 3], root).is_none(),
            "a truncated cache was read anyway"
        );

        let mut wrong_magic = good.clone();
        wrong_magic[0] = b'X';
        assert!(FixityCache::decode(&wrong_magic, root).is_none());

        let mut wrong_version = good.clone();
        wrong_version[MAGIC.len()] = 0xff;
        assert!(FixityCache::decode(&wrong_version, root).is_none());

        assert!(
            FixityCache::decode(&good, Path::new("/elsewhere")).is_none(),
            "a cache written for another workspace was accepted"
        );

        assert!(FixityCache::decode(b"", root).is_none());
    }

    /// A capture that learned nothing must not rewrite the file to say so.
    #[test]
    fn re_recording_the_same_answer_leaves_the_cache_clean() {
        let root = Path::new("/vault");
        let mut cache = FixityCache::new(root);
        cache.put(Path::new("index.md"), &meta(1, 5), HASH);
        let mut reloaded = FixityCache::decode(&cache.encode(), root).unwrap();

        reloaded.put(Path::new("index.md"), &meta(1, 5), HASH);
        assert!(
            !reloaded.is_dirty(),
            "recording an answer already held marked the cache dirty"
        );

        reloaded.put(Path::new("index.md"), &meta(2, 5), "sha256:beef");
        assert!(
            reloaded.is_dirty(),
            "a genuinely new answer was not recorded"
        );
    }

    /// The encoding is a function of the contents, not of the order they arrived
    /// in — so an unchanged workspace produces an unchanged file.
    #[test]
    fn the_encoding_is_order_independent() {
        let mut one = FixityCache::new("/vault");
        one.put(Path::new("b.md"), &meta(2, 2), "sha256:bb");
        one.put(Path::new("a.md"), &meta(1, 1), "sha256:aa");

        let mut two = FixityCache::new("/vault");
        two.put(Path::new("a.md"), &meta(1, 1), "sha256:aa");
        two.put(Path::new("b.md"), &meta(2, 2), "sha256:bb");

        assert_eq!(one.encode(), two.encode());
    }

    /// A pre-epoch timestamp is a real state for a restored archive, and two of
    /// them must stay distinguishable from each other and from the epoch.
    #[test]
    fn a_pre_epoch_timestamp_round_trips() {
        let root = Path::new("/vault");
        let old = Metadata::new(
            FileType::FILE,
            5,
            Some(UNIX_EPOCH - Duration::from_secs(86_400)),
        );
        let older = Metadata::new(
            FileType::FILE,
            5,
            Some(UNIX_EPOCH - Duration::from_secs(172_800)),
        );

        let mut cache = FixityCache::new(root);
        cache.put(Path::new("relic.md"), &old, HASH);
        let reloaded = FixityCache::decode(&cache.encode(), root).unwrap();

        assert_eq!(reloaded.get(Path::new("relic.md"), &old), Some(HASH));
        assert!(
            reloaded.get(Path::new("relic.md"), &older).is_none(),
            "two pre-epoch timestamps collapsed onto one another"
        );
        assert!(
            reloaded.get(Path::new("relic.md"), &meta(0, 5)).is_none(),
            "a pre-epoch timestamp was clamped to the epoch"
        );
    }

    /// Laws over the frame, rather than examples of it.
    ///
    /// This is the crate's one hand-rolled binary parser, and it reads a file
    /// prov did not necessarily write: it lives outside the workspace, in a
    /// user cache directory, where a half-finished write, a truncating backup,
    /// or an unrelated file of the same name are all ordinary. The module's
    /// promise is absolute — "`None` for anything that is not exactly what some
    /// build of prov wrote for *this* workspace", with **every failure decoding
    /// to nothing remembered** — and a length prefix read straight out of
    /// untrusted bytes is exactly where that sort of promise usually has a hole.
    ///
    /// A parser is also the one place property testing most resembles fuzzing,
    /// so both are here, and the difference between them is the lesson:
    /// uniformly random bytes almost never get past `MAGIC`, so they prove only
    /// that the front door is locked. *Corrupting a valid encoding* keeps the
    /// header intact and lands the damage in a length prefix or a UTF-8
    /// boundary — the code that never runs otherwise.
    mod properties {
        use super::*;
        use proptest::prelude::*;

        const ROOT: &str = "/vault";

        /// A cache built the only way one ever is: through `put`.
        fn cache() -> impl Strategy<Value = FixityCache> {
            prop::collection::vec(
                (
                    "[a-z/]{1,8}",
                    0..4_000_000_000u64,
                    0..64u64,
                    "[a-f0-9]{0,8}",
                ),
                0..5usize,
            )
            .prop_map(|puts| {
                let mut cache = FixityCache::new(ROOT);
                for (path, secs, len, hash) in puts {
                    cache.put(
                        Path::new(&path),
                        &meta(secs, len),
                        &format!("sha256:{hash}"),
                    );
                }
                cache
            })
        }

        proptest! {
            /// `decode ∘ encode = id`. The entries survive, the root survives,
            /// and the reloaded cache is **not dirty** — the last clause is the
            /// one with consequences, since a cache that decoded itself dirty
            /// would rewrite the file on every run that learned nothing.
            #[test]
            fn what_was_encoded_decodes_back_to_the_same_cache(cache in cache()) {
                let bytes = cache.encode();
                let reloaded = FixityCache::decode(&bytes, Path::new(ROOT))
                    .expect("prov's own bytes must decode");
                prop_assert_eq!(reloaded.len(), cache.len());
                prop_assert_eq!(reloaded.root(), cache.root());
                prop_assert!(!reloaded.is_dirty());
                // Encoding is a function of the contents, not of the order they
                // were learned in — which is what makes the file diffable and
                // two runs over one workspace agree byte for byte.
                prop_assert_eq!(reloaded.encode(), bytes);
            }

            /// Arbitrary bytes: never a panic, and never a cache claiming a
            /// root other than the one asked for. This is the front-door test —
            /// it rarely gets past `MAGIC`, which is exactly why the next one
            /// exists.
            #[test]
            fn arbitrary_bytes_decode_to_nothing_or_to_this_workspace(
                bytes in prop::collection::vec(any::<u8>(), 0..96),
            ) {
                if let Some(cache) = FixityCache::decode(&bytes, Path::new(ROOT)) {
                    prop_assert_eq!(cache.root(), Path::new(ROOT));
                    prop_assert!(!cache.is_dirty());
                }
            }

            /// **Corrupt one byte of a real encoding.** The header still passes,
            /// so the damage lands in a length prefix, a UTF-8 sequence, or an
            /// entry count — the paths random bytes never reach.
            ///
            /// What is *not* claimed: that corruption is detected. There is no
            /// per-entry checksum, deliberately, because a wrong digest here can
            /// only ever be served past the mtime-and-length gate the entry also
            /// carries. The claim is the weaker, sufficient one: no panic, no
            /// hang, and no cache attributed to the wrong workspace.
            #[test]
            fn a_corrupted_encoding_never_panics_and_never_changes_workspace(
                cache in cache(),
                at in any::<prop::sample::Index>(),
                xor in 1..=255u8,
            ) {
                let mut bytes = cache.encode();
                let at = at.index(bytes.len());
                bytes[at] ^= xor;
                if let Some(decoded) = FixityCache::decode(&bytes, Path::new(ROOT)) {
                    prop_assert_eq!(decoded.root(), Path::new(ROOT));
                    prop_assert!(!decoded.is_dirty());
                }
            }

            /// **Truncation is never partial acceptance.** A short read — an
            /// interrupted write, a copy that stopped — must decode to nothing,
            /// not to the entries that happened to arrive. Anything less would
            /// let a half-written cache answer questions about files whose
            /// records never landed.
            #[test]
            fn a_truncated_encoding_decodes_to_nothing(
                cache in cache(),
                at in any::<prop::sample::Index>(),
            ) {
                let bytes = cache.encode();
                let cut = at.index(bytes.len());
                prop_assert!(
                    FixityCache::decode(&bytes[..cut], Path::new(ROOT)).is_none(),
                    "{cut} of {} bytes still decoded",
                    bytes.len()
                );
            }

            /// A cache written for another workspace is refused outright, however
            /// well-formed it is — the entries may describe real files, but
            /// nothing in them proves which workspace's.
            #[test]
            fn a_cache_from_another_workspace_is_refused(cache in cache()) {
                prop_assert!(
                    FixityCache::decode(&cache.encode(), Path::new("/elsewhere")).is_none()
                );
            }
        }
    }
}
