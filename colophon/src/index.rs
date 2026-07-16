//! Index — where stable IDs and (later) the materialized graph live.
//!
//! The [`IndexStore`] is the single artifact that fuses two natures (DESIGN
//! §5): the **authoritative** id↔path registry — not rebuildable from the
//! documents — and (to come) the **derived** resolution cache and adjacency
//! index, which are. Keeping the store behind a trait is deliberate: a sidecar
//! file, an in-memory map, or a sync-backed store are all valid homes.
//!
//! ## Tombstones — IDs are forever
//!
//! DESIGN's open question #1 ("does the registry ever need to survive without
//! its documents?") is answered **yes, minimally**: deleting a document leaves
//! a *tombstone* — the ID stops resolving but is never forgotten, so it can
//! never be reminted to mean something else. A dangling `colophon:` reference
//! then stays *diagnosable* (validation can say "that document was deleted")
//! instead of becoming a silent re-resolution hazard.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::document::{Document, MetaCarrier, whole_file_format};
use crate::edit::MetaEditor;
use crate::error::Result;
use crate::identity::Id;
use crate::meta::{Mapping, Value};

/// Somewhere IDs (and eventually derived graph data) are persisted and queried.
pub trait IndexStore {
    /// Record that `id` names the document at `path`.
    fn register(&mut self, id: &Id, path: &Path);

    /// Resolve an ID to its current path. `None` for unknown *and* tombstoned
    /// IDs — use [`is_known`](IndexStore::is_known) to tell them apart.
    fn resolve(&self, id: &Id) -> Option<PathBuf>;

    /// The ID currently assigned to `path`, if any.
    fn id_for_path(&self, path: &Path) -> Option<Id>;

    /// Update the path an ID points at (e.g. after a move/rename).
    fn set_path(&mut self, id: &Id, new_path: &Path);

    /// Retire an ID (e.g. after a delete). A store with tombstones keeps the
    /// ID on record so it is never reissued; a plain store may forget it.
    fn unregister(&mut self, id: &Id);

    /// Whether `id` has *ever* been issued — live or tombstoned. This is the
    /// mint-with-rejection predicate: a fresh ID must be `!is_known`.
    fn is_known(&self, id: &Id) -> bool {
        self.resolve(id).is_some()
    }

    // ---- staging ----
    //
    // A mutation's registry update has to land in the *same* unit as its
    // document edits (§ the module docs): a rename that repoints three links but
    // loses its `id → path` update leaves every `colophon:<id>` reference to the
    // moved document resolving to nothing — the exact failure IDs exist to
    // prevent, and the one the documents cannot self-heal from, because the
    // registry is authoritative rather than derived (DESIGN §5).
    //
    // So the op mutates the store in memory *first*, stages the resulting write
    // alongside the documents', and applies the lot. These four hooks are what
    // make that reversible. All default to nothing, which is exactly right for
    // [`NoIndex`] (nothing to persist) and for a store that persists itself.

    /// Snapshot the store, so a mutation that fails can put it back. Called
    /// before an op touches the index; paired with exactly one
    /// [`rollback`](IndexStore::rollback) or [`committed`](IndexStore::committed).
    fn checkpoint(&mut self) {}

    /// Restore the last [`checkpoint`](IndexStore::checkpoint) — the mutation
    /// failed and its writes were unwound, so the in-memory store must forget it
    /// too, or it would claim a move that never happened.
    fn rollback(&mut self) {}

    /// The mutation's writes landed: drop the checkpoint.
    ///
    /// `persisted` says whether this store's own [`pending_write`] was among
    /// them. These are two different facts and must not be conflated: the
    /// checkpoint is dropped **unconditionally**, because the op succeeded and
    /// there is nothing left to undo, while `dirty` clears only when the write
    /// actually went out. A store with no home stages nothing yet still commits
    /// successfully — leaving its checkpoint outstanding would make the *next*
    /// op's [`change`](crate::workspace::Workspace::change) mistake it for one
    /// abandoned mid-edit and unwind a mutation that fully happened.
    ///
    /// [`pending_write`]: IndexStore::pending_write
    fn committed(&mut self, persisted: bool) {
        let _ = persisted;
    }

    /// Follow the mutation's change set to wherever it leaves this store's home.
    ///
    /// Called just before [`pending_write`](IndexStore::pending_write), because a
    /// store that persists into a *document* has a problem the rest of the
    /// mutation does not: that document is itself part of the workspace, and the
    /// same op may be moving or rewriting it. The registry declares a `part_of`
    /// back at the root, so moving the root re-relativizes it; and the registry
    /// document can simply be renamed like any other node.
    ///
    /// Either way its write is staged *last*, so without this it would render
    /// against the text read at startup and land at the path read at startup —
    /// silently reverting the op's edit, or recreating the file the op just
    /// renamed away from. Rebasing first makes the last write build *on* the
    /// earlier one instead of erasing it.
    fn rebase(&mut self, cs: &crate::change::ChangeSet) -> Result<()> {
        let _ = cs;
        Ok(())
    }

    /// The write that would persist this store, as `(path, full new text)` —
    /// staged into the mutation's change set and applied with it.
    ///
    /// `None` when there is nothing to write: the store is unchanged, has no
    /// file home, or persists itself some other way. A store that returns `None`
    /// while dirty is left dirty, so a caller that knows a home this store does
    /// not can still write it (the CLI bootstrapping a registry document only
    /// once a fix has actually minted an ID).
    fn pending_write(&mut self) -> Result<Option<(PathBuf, String)>> {
        Ok(None)
    }
}

/// No index — identity-off workspaces. Registers nothing, resolves nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoIndex;

impl IndexStore for NoIndex {
    fn register(&mut self, _id: &Id, _path: &Path) {}
    fn resolve(&self, _id: &Id) -> Option<PathBuf> {
        None
    }
    fn id_for_path(&self, _path: &Path) -> Option<Id> {
        None
    }
    fn set_path(&mut self, _id: &Id, _new_path: &Path) {}
    fn unregister(&mut self, _id: &Id) {}
}

/// A simple in-memory registry — for tests and ephemeral workspaces. No
/// tombstones: an unregistered ID is forgotten entirely.
#[derive(Debug, Clone, Default)]
pub struct InMemoryIndex {
    forward: HashMap<Id, PathBuf>,
    reverse: HashMap<PathBuf, Id>,
    /// The last [`checkpoint`](IndexStore::checkpoint), restored by
    /// [`rollback`](IndexStore::rollback). Nothing is persisted from here, so
    /// the two maps are the whole of the state to save.
    saved: Option<Box<InMemoryState>>,
}

/// An [`InMemoryIndex`]'s saved state — see its `saved` field.
#[derive(Debug, Clone)]
struct InMemoryState {
    forward: HashMap<Id, PathBuf>,
    reverse: HashMap<PathBuf, Id>,
}

impl InMemoryIndex {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of registered IDs.
    pub fn len(&self) -> usize {
        self.forward.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.forward.is_empty()
    }
}

impl IndexStore for InMemoryIndex {
    fn register(&mut self, id: &Id, path: &Path) {
        self.forward.insert(id.clone(), path.to_path_buf());
        self.reverse.insert(path.to_path_buf(), id.clone());
    }

    fn resolve(&self, id: &Id) -> Option<PathBuf> {
        self.forward.get(id).cloned()
    }

    fn id_for_path(&self, path: &Path) -> Option<Id> {
        self.reverse.get(path).cloned()
    }

    fn set_path(&mut self, id: &Id, new_path: &Path) {
        if let Some(old) = self.forward.insert(id.clone(), new_path.to_path_buf()) {
            self.reverse.remove(&old);
        }
        self.reverse.insert(new_path.to_path_buf(), id.clone());
    }

    fn unregister(&mut self, id: &Id) {
        if let Some(path) = self.forward.remove(id) {
            self.reverse.remove(&path);
        }
    }

    fn checkpoint(&mut self) {
        self.saved = Some(Box::new(InMemoryState {
            forward: self.forward.clone(),
            reverse: self.reverse.clone(),
        }));
    }

    fn rollback(&mut self) {
        if let Some(saved) = self.saved.take() {
            self.forward = saved.forward;
            self.reverse = saved.reverse;
        }
    }

    /// Nothing here is ever persisted, so `persisted` is irrelevant — but the
    /// checkpoint must still be dropped on every success.
    fn committed(&mut self, _persisted: bool) {
        self.saved = None;
    }
}

/// The persistent registry: a snapshot with tombstones, living **under the
/// `registry` key of a workspace document** — the document the root's
/// registry-pointer relation targets.
///
/// The host document can be either shape (`MetaCarrier`): a bare config file
/// (`registry.yaml`, `registry.figl`, …) whose whole content is metadata, or a
/// prose document (`registry.md`) whose fenced frontmatter carries the records.
/// Writes splice only the `registry` value back through the carrier-aware
/// editor, so the host's other keys (`title`, `part_of` — the self-description
/// that makes the registry a first-class node of the tree), its comments
/// outside the records, its body, and its fence style all survive.
///
/// The rendered records are one per line (in YAML hosts), sorted by ID; a live
/// record is `id: path`, a tombstone is `id: null` (DESIGN §5's diff-friendly
/// shape). This type is pure — text in ([`FileIndex::parse`]), text out
/// ([`FileIndex::render`]) — so any storage backend can host it; the caller
/// owns the I/O and can consult [`is_dirty`](FileIndex::is_dirty) to skip
/// no-op writes.
#[derive(Debug, Clone)]
pub struct FileIndex {
    live: InMemoryIndex,
    tombstones: BTreeSet<Id>,
    /// The host document's workspace-relative path — where
    /// [`pending_write`](IndexStore::pending_write) stages its write. `None` for
    /// a registry with no document behind it yet: an
    /// [`InMemoryIndex`]-in-disguise built by [`new`](FileIndex::new), either
    /// because the workspace stores IDs in frontmatter only (nothing to persist)
    /// or because no registry document has been bootstrapped yet. Such a store
    /// stays dirty rather than silently dropping records, so a caller that knows
    /// a home can still write it.
    host: Option<PathBuf>,
    /// The host document's full current text and carrier — what `render`
    /// splices the records back into.
    host_text: String,
    carrier: MetaCarrier,
    /// The record state as currently written in `host_text` — `render` applies
    /// only the per-record diff against this, as scalar upserts (whole-mapping
    /// splices cannot round-trip through every carrier; scalars can).
    persisted: BTreeMap<Id, Option<String>>,
    /// Whether `host_text` already has a `registry` key. When it does not, the
    /// first render inserts the whole mapping at once — that is what gets the
    /// block (one-record-per-line) layout on bare hosts; per-record creation
    /// would make fig auto-create a flow map.
    has_registry_key: bool,
    dirty: bool,
    /// The last [`checkpoint`](IndexStore::checkpoint).
    saved: Option<Box<FileIndexState>>,
}

/// Every field of a [`FileIndex`] a mutation can move — saved by
/// [`checkpoint`](IndexStore::checkpoint) and put back by
/// [`rollback`](IndexStore::rollback). `render` advances `host_text`/`persisted`
/// as a side effect of staging, so those are as much part of the mutation as the
/// records themselves and have to unwind with them.
#[derive(Debug, Clone)]
struct FileIndexState {
    live: InMemoryIndex,
    tombstones: BTreeSet<Id>,
    host_text: String,
    persisted: BTreeMap<Id, Option<String>>,
    has_registry_key: bool,
    dirty: bool,
}

impl FileIndex {
    /// An empty registry with no host document — see the `host` field. Records
    /// resolve in memory; nothing is staged for writing.
    pub fn new(format: fig::Format) -> Self {
        Self {
            live: InMemoryIndex::new(),
            tombstones: BTreeSet::new(),
            host: None,
            host_text: String::new(),
            carrier: MetaCarrier::WholeFile(format),
            persisted: BTreeMap::new(),
            has_registry_key: false,
            dirty: false,
            saved: None,
        }
    }

    /// Give a registry a host document to persist into, adopting `text` as its
    /// current contents.
    ///
    /// The bootstrap seam: a workspace that only discovers it needs a registry
    /// *after* a mutation has minted an ID (`check --fix` declines to create one
    /// until a fix actually registers something) creates the document, then hands
    /// it here so the write renders against the real host — its title, its
    /// `part_of`, its fence style — rather than against nothing.
    ///
    /// **This store's records stay authoritative.** Only the write *target* is
    /// adopted: the host's text, carrier, and already-persisted record state, so
    /// [`render`](Self::render) splices a correct diff into it. Records the host
    /// happens to carry are not merged into memory — they were not part of what
    /// this store was built from, and adopting them here would resurrect, as live
    /// records, whatever a scan or a caller had deliberately left out. They are
    /// not *lost* either: `render` only ever touches the records it knows about,
    /// so their lines survive in the document and are read back normally by the
    /// next [`parse`](Self::parse).
    pub fn set_host(&mut self, path: impl Into<PathBuf>, text: &str) -> Result<()> {
        let path = path.into();
        let reparsed = Self::parse(&path, text)?;
        self.host = Some(path);
        self.carrier = reparsed.carrier;
        self.host_text = reparsed.host_text;
        self.persisted = reparsed.persisted;
        self.has_registry_key = reparsed.has_registry_key;
        Ok(())
    }

    /// The document this registry persists into, if it has one.
    pub fn host(&self) -> Option<&Path> {
        self.host.as_deref()
    }

    /// Parse the registry out of its host document. `path` picks the carrier
    /// (a config extension means the whole file is metadata; anything else is
    /// searched for a fenced block); the records are read from the metadata's
    /// `registry` key. A host with no `registry` key is an empty registry —
    /// the rest of its metadata is left alone.
    pub fn parse(path: &Path, text: &str) -> Result<Self> {
        let doc = Document::parse(path, text)?;
        let carrier = doc.carrier.unwrap_or_else(|| {
            // No metadata yet: default by extension, else fresh YAML frontmatter.
            whole_file_format(path)
                .map(MetaCarrier::WholeFile)
                .unwrap_or(MetaCarrier::Fenced(fig::EmbedType::FrontmatterYaml))
        });
        let mut index = Self {
            live: InMemoryIndex::new(),
            tombstones: BTreeSet::new(),
            host: Some(path.to_path_buf()),
            host_text: text.to_string(),
            carrier,
            persisted: BTreeMap::new(),
            has_registry_key: doc.meta.get("registry").is_some(),
            dirty: false,
            saved: None,
        };
        if let Some(registry) = doc.meta.get("registry").and_then(Value::as_mapping) {
            for (id, value) in registry {
                let id = Id(id.clone());
                match value {
                    Value::Null => {
                        index.persisted.insert(id.clone(), None);
                        index.tombstones.insert(id);
                    }
                    Value::String(path) => {
                        index.persisted.insert(id.clone(), Some(path.clone()));
                        index.live.register(&id, Path::new(path));
                    }
                    _ => {
                        return Err(crate::error::Error::Structure(format!(
                            "registry entry `{id}` must be a path or null (tombstone)"
                        )));
                    }
                }
            }
        }
        Ok(index)
    }

    /// Render the host document with the current records applied to its
    /// `registry` key. Each changed record is a *scalar* upsert
    /// (`registry.<id> = path` / `null`), so everything else in the host —
    /// title, part_of, comments, body, fences, existing record lines — is
    /// untouched, whatever the carrier. Records never reorder; new ones land
    /// in ID order.
    pub fn render(&mut self) -> Result<String> {
        let mut current: BTreeMap<Id, Option<String>> = BTreeMap::new();
        for id in &self.tombstones {
            current.insert(id.clone(), None);
        }
        for (id, path) in &self.live.forward {
            current.insert(id.clone(), Some(path.to_string_lossy().into_owned()));
        }
        if current == self.persisted {
            return Ok(self.host_text.clone());
        }

        // First materialization of the `registry` key.
        if !self.has_registry_key {
            let mut registry = Mapping::new();
            for (id, value) in &current {
                registry.insert(
                    id.0.clone(),
                    value.clone().map(Value::String).unwrap_or(Value::Null),
                );
            }
            let rendered = match self.carrier {
                // Bare host: rebuild the whole config document (its metadata
                // plus the new registry mapping) through `serialize_mapping`,
                // whose block layout gives one record per line. This is the
                // one write that does not go through the comment-preserving
                // editor — a fig value splice renders short maps in flow
                // style, which would freeze the registry inline forever.
                // Bootstrap hosts are machine-generated, so nothing of note
                // is lost; afterwards every write is a preserving upsert.
                MetaCarrier::WholeFile(format) => {
                    let mut top = crate::meta::parse_mapping(&self.host_text, format)?;
                    top.insert("registry".into(), Value::Mapping(registry));
                    crate::meta::serialize_mapping(&top, format)?
                }
                // Fenced host: a block map cannot be spliced into the fence
                // (fig embed limitation), so records land per-key — fig
                // auto-creates the chain as a flow map. Valid, just inline.
                MetaCarrier::Fenced(_) => {
                    let mut editor = MetaEditor::open_or_init(&self.host_text, Some(self.carrier))?;
                    for (id, value) in &current {
                        let fig_value =
                            value.clone().map(fig::Value::Str).unwrap_or(fig::Value::Null);
                        editor.set_value(
                            &[fig::Segment::Key("registry"), fig::Segment::Key(id.as_str())],
                            fig_value,
                        )?;
                    }
                    editor.render()?
                }
            };
            self.host_text = rendered.clone();
            self.persisted = current;
            self.has_registry_key = true;
            return Ok(rendered);
        }

        // Steady state: per-record comment-preserving upserts of the diff.
        let mut editor = MetaEditor::open_or_init(&self.host_text, Some(self.carrier))?;
        for (id, value) in &current {
            if self.persisted.get(id) == Some(value) {
                continue;
            }
            let fig_value = value.clone().map(fig::Value::Str).unwrap_or(fig::Value::Null);
            editor.set_value(
                &[fig::Segment::Key("registry"), fig::Segment::Key(id.as_str())],
                fig_value,
            )?;
        }
        let rendered = editor.render()?;
        self.host_text = rendered.clone();
        self.persisted = current;
        Ok(rendered)
    }

    /// Whether the registry changed since it was parsed/created (i.e. needs a
    /// write). Cleared by [`mark_clean`](FileIndex::mark_clean).
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Mark the registry as persisted.
    pub fn mark_clean(&mut self) {
        self.dirty = false;
    }

    /// The number of live (resolving) IDs.
    pub fn len(&self) -> usize {
        self.live.len()
    }

    /// Whether the registry has no live IDs.
    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    /// Whether `id` is retired: known but no longer resolving.
    pub fn is_tombstoned(&self, id: &Id) -> bool {
        self.tombstones.contains(id)
    }

    /// Iterate live records as `(id, path)`, sorted by ID.
    pub fn iter(&self) -> impl Iterator<Item = (&Id, &PathBuf)> {
        let mut live: Vec<_> = self.live.forward.iter().collect();
        live.sort_by(|a, b| a.0.cmp(b.0));
        live.into_iter()
    }
}

impl IndexStore for FileIndex {
    fn register(&mut self, id: &Id, path: &Path) {
        self.live.register(id, path);
        self.dirty = true;
    }

    fn resolve(&self, id: &Id) -> Option<PathBuf> {
        self.live.resolve(id)
    }

    fn id_for_path(&self, path: &Path) -> Option<Id> {
        self.live.id_for_path(path)
    }

    fn set_path(&mut self, id: &Id, new_path: &Path) {
        self.live.set_path(id, new_path);
        self.dirty = true;
    }

    /// Retire to a tombstone: the ID stops resolving but stays known forever.
    fn unregister(&mut self, id: &Id) {
        self.live.unregister(id);
        self.tombstones.insert(id.clone());
        self.dirty = true;
    }

    fn is_known(&self, id: &Id) -> bool {
        self.live.resolve(id).is_some() || self.tombstones.contains(id)
    }

    fn checkpoint(&mut self) {
        self.saved = Some(Box::new(FileIndexState {
            live: self.live.clone(),
            tombstones: self.tombstones.clone(),
            host_text: self.host_text.clone(),
            persisted: self.persisted.clone(),
            has_registry_key: self.has_registry_key,
            dirty: self.dirty,
        }));
    }

    fn rollback(&mut self) {
        let Some(saved) = self.saved.take() else { return };
        let FileIndexState { live, tombstones, host_text, persisted, has_registry_key, dirty } =
            *saved;
        self.live = live;
        self.tombstones = tombstones;
        self.host_text = host_text;
        self.persisted = persisted;
        self.has_registry_key = has_registry_key;
        self.dirty = dirty;
    }

    fn committed(&mut self, persisted: bool) {
        self.saved = None;
        if persisted {
            self.dirty = false;
        }
    }

    fn rebase(&mut self, cs: &crate::change::ChangeSet) -> Result<()> {
        let Some(host) = self.host.clone() else {
            return Ok(());
        };
        // Follow a move of the host document to its final path.
        let dest = cs.renamed_to(&host).unwrap_or(host);
        // Whatever the set will leave in that document is the text the records
        // must be spliced into — the op's edit, not the copy read at startup.
        if let Some(bytes) = cs.staged(&dest) {
            let text = String::from_utf8(bytes.to_vec()).map_err(|e| {
                crate::error::Error::Structure(format!(
                    "{} is not valid UTF-8: {e}",
                    dest.display()
                ))
            })?;
            return self.set_host(dest, &text);
        }
        // Moved but not rewritten: the bytes travelled with the rename, so
        // `host_text` still describes it and only the path changes.
        self.host = Some(dest);
        Ok(())
    }

    /// The registry's write, rendered against its host document. `None` — and
    /// crucially *still dirty* — when there is no host to write to.
    fn pending_write(&mut self) -> Result<Option<(PathBuf, String)>> {
        if !self.dirty {
            return Ok(None);
        }
        let Some(host) = self.host.clone() else {
            return Ok(None);
        };
        Ok(Some((host, self.render()?)))
    }
}

// These engine tests use YAML fixtures throughout, so they run whenever the
// (default) `yaml` feature is on.
#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::*;

    #[test]
    fn set_host_keeps_this_stores_records_and_preserves_the_hosts() {
        // The bootstrap backstop: an index built with no home (records minted by
        // fixes) is given one after the fact. Its own records must survive into
        // the write, and any the host already carried must not be trampled by it.
        let mut ix = FileIndex::new(fig::Format::Yaml);
        let mine = Id("mineeee".into());
        ix.register(&mine, Path::new("fixed.md"));

        // A host that already has a record of its own, plus self-description.
        let host = "title: ID registry\npart_of: index.md\nregistry:\n  theirss: other.md\n";
        ix.set_host("registry.yaml", host).unwrap();

        let (path, rendered) = ix.pending_write().unwrap().expect("dirty, and now has a home");
        assert_eq!(path, PathBuf::from("registry.yaml"));
        assert!(rendered.contains("fixed.md"), "this store's record must land: {rendered}");
        assert!(rendered.contains("other.md"), "the host's record must survive: {rendered}");
        assert!(rendered.contains("part_of"), "the host's self-description survives: {rendered}");

        // The host's record was not adopted as live in memory — but the next
        // parse of what we just wrote reads both, which is what makes that safe.
        assert_eq!(ix.resolve(&Id("theirss".into())), None);
        let reread = FileIndex::parse(Path::new("registry.yaml"), &rendered).unwrap();
        assert_eq!(reread.resolve(&mine), Some(PathBuf::from("fixed.md")));
        assert_eq!(reread.resolve(&Id("theirss".into())), Some(PathBuf::from("other.md")));
    }

    #[test]
    fn a_store_with_no_host_stays_dirty_rather_than_dropping_records() {
        // Frontmatter-only workspaces, and the window before a registry is
        // bootstrapped: nothing to stage, so the caller must still be told there
        // is something to write.
        let mut ix = FileIndex::new(fig::Format::Yaml);
        ix.register(&Id("orphann".into()), Path::new("a.md"));
        assert_eq!(ix.pending_write().unwrap(), None, "nowhere to write");
        assert!(ix.is_dirty(), "and so it must not claim to be persisted");
    }

    #[test]
    fn registers_and_resolves_both_directions() {
        let mut ix = InMemoryIndex::new();
        let id = Id("ajp7eq".into());
        ix.register(&id, Path::new("notes/a.md"));
        assert_eq!(ix.resolve(&id), Some(PathBuf::from("notes/a.md")));
        assert_eq!(ix.id_for_path(Path::new("notes/a.md")), Some(id.clone()));
        assert_eq!(ix.len(), 1);
    }

    #[test]
    fn move_updates_path_and_clears_stale_reverse() {
        let mut ix = InMemoryIndex::new();
        let id = Id("ajp7eq".into());
        ix.register(&id, Path::new("a.md"));
        ix.set_path(&id, Path::new("moved/a.md"));
        assert_eq!(ix.resolve(&id), Some(PathBuf::from("moved/a.md")));
        assert_eq!(ix.id_for_path(Path::new("a.md")), None);
        assert_eq!(ix.id_for_path(Path::new("moved/a.md")), Some(id));
    }

    #[test]
    fn unregister_removes_both_directions() {
        let mut ix = InMemoryIndex::new();
        let id = Id("x".into());
        ix.register(&id, Path::new("a.md"));
        ix.unregister(&id);
        assert!(ix.is_empty());
        assert_eq!(ix.id_for_path(Path::new("a.md")), None);
    }

    #[test]
    fn file_index_round_trips_sorted_with_tombstones() {
        let mut ix = FileIndex::new(fig::Format::Yaml);
        ix.register(&Id("zzzzzzz".into()), Path::new("z.md"));
        ix.register(&Id("bcdfghj".into()), Path::new("notes/a.md"));
        ix.register(&Id("mmmmmmm".into()), Path::new("gone.md"));
        ix.unregister(&Id("mmmmmmm".into()));

        let text = ix.render().unwrap();
        // Sorted, one record per line, tombstone as null.
        let b = text.find("bcdfghj").unwrap();
        let m = text.find("mmmmmmm").unwrap();
        let z = text.find("zzzzzzz").unwrap();
        assert!(b < m && m < z, "{text}");
        assert!(text.contains("mmmmmmm: null"), "{text}");

        let back = FileIndex::parse(Path::new("registry.yaml"), &text).unwrap();
        assert_eq!(back.resolve(&Id("bcdfghj".into())), Some(PathBuf::from("notes/a.md")));
        assert_eq!(back.resolve(&Id("mmmmmmm".into())), None);
        assert!(back.is_known(&Id("mmmmmmm".into())), "tombstone survives the round-trip");
        assert!(back.is_tombstoned(&Id("mmmmmmm".into())));
        assert!(!back.is_dirty());
    }

    #[test]
    fn registry_host_keeps_its_self_description_and_comments() {
        // A bare config host with a title, a part_of back to the root, and a
        // comment: splicing records must leave all of that alone.
        let host = "# who am I? see title
title: ID registry
part_of: index.md
registry:
  bcdfghj: a.md
";
        let mut ix = FileIndex::parse(Path::new("registry.yaml"), host).unwrap();
        ix.register(&Id("zzzzzzz".into()), Path::new("z.md"));
        let out = ix.render().unwrap();
        assert!(out.contains("# who am I? see title"), "{out}");
        assert!(out.contains("title: ID registry"), "{out}");
        assert!(out.contains("part_of: index.md"), "{out}");
        assert!(out.contains("bcdfghj: a.md"), "{out}");
        assert!(out.contains("zzzzzzz: z.md"), "{out}");
    }

    #[test]
    fn registry_can_live_in_markdown_frontmatter() {
        // The registry embedded in a prose document: records in the fenced
        // block, body untouched.
        let host = "---
title: Registry
part_of: index.md
registry:
  bcdfghj: a.md
---
# About this file

The workspace's ID registry lives in my frontmatter.
";
        let mut ix = FileIndex::parse(Path::new("registry.md"), host).unwrap();
        assert_eq!(ix.resolve(&Id("bcdfghj".into())), Some(PathBuf::from("a.md")));
        ix.set_path(&Id("bcdfghj".into()), Path::new("moved/a.md"));
        let out = ix.render().unwrap();
        assert!(out.starts_with("---
title: Registry"), "fences kept: {out}");
        assert!(out.contains("bcdfghj: moved/a.md"), "{out}");
        assert!(out.ends_with("The workspace's ID registry lives in my frontmatter.\n"), "body kept: {out}");

        let back = FileIndex::parse(Path::new("registry.md"), &out).unwrap();
        assert_eq!(back.resolve(&Id("bcdfghj".into())), Some(PathBuf::from("moved/a.md")));
    }

    #[test]
    fn tombstoned_ids_are_never_free_for_reminting() {
        let mut ix = FileIndex::new(fig::Format::Yaml);
        let id = Id("bcdfghj".into());
        ix.register(&id, Path::new("a.md"));
        ix.unregister(&id);
        assert_eq!(ix.resolve(&id), None, "does not resolve");
        assert!(ix.is_known(&id), "but is still known — never reminted");
    }

    #[test]
    fn dirty_tracks_mutations() {
        let mut ix = FileIndex::new(fig::Format::Yaml);
        assert!(!ix.is_dirty());
        ix.register(&Id("x".into()), Path::new("a.md"));
        assert!(ix.is_dirty());
        ix.mark_clean();
        assert!(!ix.is_dirty());
    }

    #[test]
    fn empty_text_is_an_empty_registry() {
        let ix = FileIndex::parse(Path::new("registry.yaml"), "").unwrap();
        assert!(ix.is_empty());
    }
}
