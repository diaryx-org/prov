//! The write half of the ID index.
//!
//! [`prov_graph::index`] declares [`IdIndex`] — the lookups link resolution
//! needs, and no way to change what is stored. This module declares everything
//! that does change it: [`IndexStore`], the [`Rebase`] seam a pending change
//! set answers through, and the two concrete registries whose state only a
//! writer has any use for — [`InMemoryIndex`] and the registry-document-backed
//! [`FileIndex`].
//!
//! ## Tombstones — IDs are forever
//!
//! Deleting a document leaves a *tombstone*: the ID stops resolving but is
//! never forgotten, so it can never be reminted to mean something else. A
//! dangling `prov:` reference then stays *diagnosable* ("that document was
//! deleted") instead of becoming a silent re-resolution hazard.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use prov_graph::Result;
use prov_graph::document::{Document, MetaCarrier, require_whole_file, whole_file_format};
use prov_graph::identity::Id;
use prov_graph::index::{IdIndex, NoIndex};
use prov_graph::meta::{Mapping, Value};

use crate::edit::MetaEditor;

/// What a pending change set can tell a store about its own persisted home.
///
/// [`IndexStore::rebase`] needs exactly two facts before it renders: where the
/// document hosting its records will *end up* once the change lands, and what
/// text will be in it. That is the whole of the dependency, so it is the whole
/// of this trait — the alternative, handing `rebase` the change set itself,
/// would make every store implementor depend on the mutation engine to answer
/// two questions about a path.
pub trait Rebase {
    /// Where `path` will be after the change lands, if the change moves it.
    fn renamed_to(&self, path: &Path) -> Option<PathBuf>;

    /// The bytes the change will leave at `path`, if it writes there.
    fn staged(&self, path: &Path) -> Option<&[u8]>;
}

/// A pending change set can answer the two questions an [`IndexStore`] has
/// before it renders: where its host document will end up, and what will be in
/// it.
///
/// The impl lives here rather than beside [`ChangeSet`](prov_transaction::ChangeSet)
/// because [`Rebase`] is prov's question, not the transaction crate's — a
/// generic change set has no idea that anything wants to read it back
/// mid-build. Implementing the narrow trait rather than passing the set whole
/// is what keeps a store implementor free of the mutation engine.
impl Rebase for prov_transaction::ChangeSet {
    fn renamed_to(&self, path: &Path) -> Option<PathBuf> {
        prov_transaction::ChangeSet::renamed_to(self, path)
    }

    fn staged(&self, path: &Path) -> Option<&[u8]> {
        prov_transaction::ChangeSet::staged(self, path)
    }
}

/// Somewhere IDs (and eventually derived graph data) are persisted and queried —
/// [`IdIndex`]'s lookups plus everything that changes what is stored.
pub trait IndexStore: IdIndex {
    /// Record that `id` names the document at `path`.
    fn register(&mut self, id: &Id, path: &Path);

    /// Update the path an ID points at (e.g. after a move/rename).
    ///
    /// Bijection-safe like [`register`](IndexStore::register): if `new_path`
    /// already carries a *different* id, that id's forward entry must not
    /// survive pointing at a path it no longer owns. A caller that moves an id
    /// it did not just mint should ask
    /// `prov`'s `Workspace::move_conflict` first and
    /// refuse the collision up front — the document being displaced still
    /// spells the id in its own frontmatter. This eviction is the same last
    /// line of defence [`register`](IndexStore::register) keeps, for when
    /// something slips through anyway. A store with tombstones should also
    /// *retire* what it displaces, so an evicted id stays
    /// [`is_known`](IdIndex::is_known) and can never be reissued;
    /// [`FileIndex`] does.
    fn set_path(&mut self, id: &Id, new_path: &Path);

    /// Retire an ID (e.g. after a delete). A store with tombstones keeps the
    /// ID on record so it is never reissued; a plain store may forget it.
    fn unregister(&mut self, id: &Id);

    // ---- staging ----
    //
    // A mutation's registry update has to land in the *same* unit as its
    // document edits (§ the module docs): a rename that repoints three links but
    // loses its `id → path` update leaves every `prov:<id>` reference to the
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
    /// op's `prov`'s `change`(`prov`'s `Workspace::change`) mistake it for one
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
    fn rebase(&mut self, cs: &dyn Rebase) -> Result<()> {
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

impl IndexStore for NoIndex {
    fn register(&mut self, _id: &Id, _path: &Path) {}
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

impl IdIndex for InMemoryIndex {
    fn resolve(&self, id: &Id) -> Option<PathBuf> {
        self.forward.get(id).cloned()
    }

    fn id_for_path(&self, path: &Path) -> Option<Id> {
        self.reverse.get(path).cloned()
    }
}

impl IndexStore for InMemoryIndex {
    /// Displacing an existing registration must not leave the *other* map
    /// pointing at the old counterpart — [`set_path`](IndexStore::set_path)
    /// delegates here for exactly this care. Without it a collision leaves the
    /// registry claiming two paths for one id (or two ids for one path), which
    /// nothing downstream can make sense of.
    ///
    /// Eviction is the last line of defence, not the intended path: callers that
    /// register (or move, via `set_path`) an id they did not just mint should
    /// ask `prov`'s `Workspace::registration_conflict` or
    /// `prov`'s `Workspace::move_conflict` first and refuse,
    /// because the document being displaced still spells the id in its own
    /// frontmatter. What this guarantees is only that the index stays
    /// *consistent* when something slips through.
    fn register(&mut self, id: &Id, path: &Path) {
        if let Some(old_path) = self.forward.insert(id.clone(), path.to_path_buf()) {
            self.reverse.remove(&old_path);
        }
        if let Some(old_id) = self.reverse.insert(path.to_path_buf(), id.clone())
            && old_id != *id
        {
            self.forward.remove(&old_id);
        }
    }

    /// Moving an id onto a path is the same bijection-safe upsert as
    /// registering it there fresh — displacing in either direction must not
    /// leave the other map pointing at the old counterpart — so this *is*
    /// [`register`](IndexStore::register), not a near-duplicate of it. Before
    /// this delegated, a displacement in the new-path direction went
    /// unevicted: `new_path`'s previous id kept a forward entry pointing at a
    /// path it no longer owned, the exact two-ids-one-path break `register`
    /// was fixed against in 11abd38.
    fn set_path(&mut self, id: &Id, new_path: &Path) {
        self.register(id, new_path);
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
        // A registry is a record store, so it must be a whole-file config
        // document — a markdown carrier is refused (DESIGN §5, whole-file rule).
        require_whole_file(path, carrier)?;
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
                        return Err(prov_graph::error::Error::Structure(format!(
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
                    let mut top = prov_graph::meta::parse_mapping(&self.host_text, format)?;
                    top.insert("registry".into(), Value::Mapping(registry));
                    prov_graph::meta::serialize_mapping(&top, format)?
                }
                // A registry is always whole-file (enforced in `parse`/`new`), so
                // a fenced carrier cannot reach here; refuse defensively rather
                // than silently write a store the load path would then reject.
                MetaCarrier::Fenced(_) => {
                    return Err(prov_graph::error::Error::MarkdownStore(
                        self.host.clone().unwrap_or_default(),
                    ));
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
            let fig_value = value
                .clone()
                .map(fig::Value::Str)
                .unwrap_or(fig::Value::Null);
            editor.set_value(
                &[
                    fig::Segment::Key("registry"),
                    fig::Segment::Key(id.as_str()),
                ],
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

impl IdIndex for FileIndex {
    fn resolve(&self, id: &Id) -> Option<PathBuf> {
        self.live.resolve(id)
    }

    fn id_for_path(&self, path: &Path) -> Option<Id> {
        self.live.id_for_path(path)
    }

    /// A tombstoned id no longer resolves but stays known forever, so it can
    /// never be reminted to mean something else.
    fn is_known(&self, id: &Id) -> bool {
        self.live.resolve(id).is_some() || self.tombstones.contains(id)
    }
}

impl IndexStore for FileIndex {
    /// Registering an id **retires its tombstone**, because the id is live
    /// again and a record cannot be both. This is not a hypothetical pairing:
    /// `restore` from the recycle bin re-registers the very id `recycle`
    /// tombstoned, so the sequence runs whenever a delete is undone.
    ///
    /// [`render`](Self::render) has always resolved the two in this direction —
    /// it lays the live records down *over* the tombstones — so without this the
    /// store disagrees with its own serialization until the process restarts,
    /// and a round trip through the registry document silently "changes" it.
    /// Nothing is lost by forgetting the tombstone: `is_known` stays true
    /// through `resolve` while the id is live, so mint-by-rejection cannot
    /// reissue it, and retiring it again tombstones it again.
    /// Registering maintains the tombstone set in **both** directions, which is
    /// what makes "an ID is never reissued" (DESIGN §10) true of this store
    /// rather than merely intended.
    ///
    /// *Retires whatever it displaces.* Taking a path out from under the id
    /// currently carrying it evicts that id from the live map — the bijection
    /// repair [`InMemoryIndex::register`] performs and documents. Eviction alone
    /// would forget the id *entirely*, so [`is_known`](IdIndex::is_known)
    /// would go from true to false and a later mint could reissue it while the
    /// displaced document still spells it in its own frontmatter. Reaching that
    /// needs a displacement to slip past `registration_conflict` /
    /// `move_conflict`, which is precisely the case this store is the last line
    /// of defence for, so the displaced id earns a tombstone on the way out.
    ///
    /// *Un-retires what it registers.* An id being registered is live, and a
    /// record cannot be both live and retired. This runs whenever a delete is
    /// undone: `restore` re-registers the very id `recycle` tombstoned.
    /// [`render`](Self::render) has always resolved the pair this way — it lays
    /// the live records over the tombstones — so without this the store
    /// disagrees with its own serialization until the process restarts. Nothing
    /// is lost by forgetting the tombstone, because the id is `is_known` through
    /// `resolve` while it is live, and the clause above tombstones it again if it
    /// is ever displaced.
    ///
    /// The two clauses only work together. Un-retiring without retiring the
    /// displaced would make the forgetting *easier* to reach: an id restored
    /// from the bin and then displaced would have no tombstone left to fall back
    /// on.
    fn register(&mut self, id: &Id, path: &Path) {
        if let Some(displaced) = self.live.id_for_path(path)
            && displaced != *id
        {
            self.tombstones.insert(displaced);
        }
        self.live.register(id, path);
        self.tombstones.remove(id);
        self.dirty = true;
    }

    /// Moving an id onto a path is registering it there — the same
    /// bijection-safe eviction in both directions — so this delegates rather
    /// than restating it, exactly as [`InMemoryIndex::set_path`] delegates to
    /// its own `register`.
    fn set_path(&mut self, id: &Id, new_path: &Path) {
        self.register(id, new_path);
    }

    /// Retire to a tombstone: the ID stops resolving but stays known forever.
    fn unregister(&mut self, id: &Id) {
        self.live.unregister(id);
        self.tombstones.insert(id.clone());
        self.dirty = true;
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
        let Some(saved) = self.saved.take() else {
            return;
        };
        let FileIndexState {
            live,
            tombstones,
            host_text,
            persisted,
            has_registry_key,
            dirty,
        } = *saved;
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

    fn rebase(&mut self, cs: &dyn Rebase) -> Result<()> {
        let Some(host) = self.host.clone() else {
            return Ok(());
        };
        // Follow a move of the host document to its final path.
        let dest = cs.renamed_to(&host).unwrap_or(host);
        // Whatever the set will leave in that document is the text the records
        // must be spliced into — the op's edit, not the copy read at startup.
        if let Some(bytes) = cs.staged(&dest) {
            let text = String::from_utf8(bytes.to_vec()).map_err(|e| {
                prov_graph::error::Error::Structure(format!(
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

        let (path, rendered) = ix
            .pending_write()
            .unwrap()
            .expect("dirty, and now has a home");
        assert_eq!(path, PathBuf::from("registry.yaml"));
        assert!(
            rendered.contains("fixed.md"),
            "this store's record must land: {rendered}"
        );
        assert!(
            rendered.contains("other.md"),
            "the host's record must survive: {rendered}"
        );
        assert!(
            rendered.contains("part_of"),
            "the host's self-description survives: {rendered}"
        );

        // The host's record was not adopted as live in memory — but the next
        // parse of what we just wrote reads both, which is what makes that safe.
        assert_eq!(ix.resolve(&Id("theirss".into())), None);
        let reread = FileIndex::parse(Path::new("registry.yaml"), &rendered).unwrap();
        assert_eq!(reread.resolve(&mine), Some(PathBuf::from("fixed.md")));
        assert_eq!(
            reread.resolve(&Id("theirss".into())),
            Some(PathBuf::from("other.md"))
        );
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
    fn a_displacing_register_leaves_no_stale_entry_in_either_map() {
        // The index is a bijection, and `register` is the one mutator that used to
        // be able to break it: displacing in one direction left the other map
        // pointing at the old counterpart, so the registry claimed two paths for
        // one id. A caller should refuse the collision up front
        // (`registration_conflict`); this is what keeps the store coherent when
        // one slips through anyway.
        let (a, b) = (Id("aaaaaaa".into()), Id("bbbbbbb".into()));

        // Same id, new path: the path it left must stop claiming it.
        let mut ix = InMemoryIndex::new();
        ix.register(&a, Path::new("one.md"));
        ix.register(&a, Path::new("two.md"));
        assert_eq!(ix.resolve(&a), Some(PathBuf::from("two.md")));
        assert_eq!(ix.id_for_path(Path::new("one.md")), None);
        assert_eq!(ix.len(), 1);

        // Same path, new id: the id it displaced must stop resolving to it.
        let mut ix = InMemoryIndex::new();
        ix.register(&a, Path::new("one.md"));
        ix.register(&b, Path::new("one.md"));
        assert_eq!(ix.id_for_path(Path::new("one.md")), Some(b.clone()));
        assert_eq!(ix.resolve(&a), None);
        assert_eq!(ix.len(), 1);

        // Re-registering the pair already held changes nothing.
        ix.register(&b, Path::new("one.md"));
        assert_eq!(ix.resolve(&b), Some(PathBuf::from("one.md")));
        assert_eq!(ix.id_for_path(Path::new("one.md")), Some(b));
        assert_eq!(ix.len(), 1);
    }

    #[test]
    fn a_displacing_set_path_leaves_no_stale_entry_in_either_map() {
        // `set_path` used to take only half of `register`'s care: it evicted the
        // *moving* id's old reverse entry but ignored what `reverse.insert` at the
        // new path returned, so a displaced id's forward entry survived pointing
        // at a path it no longer owned — two ids resolving to one path. A caller
        // should refuse the collision up front (`Workspace::move_conflict`); this
        // is what keeps the store coherent when one slips through anyway, exactly
        // as `a_displacing_register_leaves_no_stale_entry_in_either_map` covers
        // for `register`.
        let (a, b) = (Id("aaaaaaa".into()), Id("bbbbbbb".into()));
        let mut ix = InMemoryIndex::new();
        ix.register(&a, Path::new("one.md"));
        ix.register(&b, Path::new("two.md"));

        // Move `a` onto `two.md`, which `b` already holds.
        ix.set_path(&a, Path::new("two.md"));

        assert_eq!(ix.resolve(&a), Some(PathBuf::from("two.md")));
        assert_eq!(ix.id_for_path(Path::new("two.md")), Some(a));
        assert_eq!(ix.id_for_path(Path::new("one.md")), None);
        // `b`'s forward entry must not survive pointing at a path it no longer
        // owns — the exact break that made `id:b` links resolve to the wrong
        // document.
        assert_eq!(ix.resolve(&b), None);
        assert_eq!(ix.len(), 1);
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
        assert_eq!(
            back.resolve(&Id("bcdfghj".into())),
            Some(PathBuf::from("notes/a.md"))
        );
        assert_eq!(back.resolve(&Id("mmmmmmm".into())), None);
        assert!(
            back.is_known(&Id("mmmmmmm".into())),
            "tombstone survives the round-trip"
        );
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
    fn a_markdown_carrier_registry_is_refused() {
        // A registry is a record store (DESIGN §5, whole-file rule): a markdown
        // (fenced) carrier has no stable home for prov's sorted records, so it is
        // rejected at load rather than read.
        let host = "---
title: Registry
part_of: index.md
registry:
  bcdfghj: a.md
---
# About this file

Prose does not belong in a record store.
";
        let err = FileIndex::parse(Path::new("registry.md"), host).unwrap_err();
        assert!(
            matches!(err, prov_graph::error::Error::MarkdownStore(_)),
            "expected MarkdownStore, got {err:?}"
        );
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

    #[test]
    fn a_displaced_id_is_retired_rather_than_forgotten() {
        // `register` keeps the bijection by evicting whatever it displaced. What
        // must *not* go with the eviction is the id's existence: `is_known` is
        // the mint-by-rejection predicate, so forgetting the id would make it
        // available to be minted again for a different document while the
        // displaced one still spells it in its own frontmatter — the case
        // DESIGN §10 answers with "IDs are never reissued".
        //
        // Reaching this needs a displacement to slip past `registration_conflict`
        // / `move_conflict`. That is what those guards are for, and this store is
        // the last line of defence when one gets through.
        let mut ix = FileIndex::new(fig::Format::Yaml);
        let (first, second) = (Id("aaa111a".into()), Id("bbb222b".into()));

        ix.register(&first, Path::new("a.md"));
        ix.register(&second, Path::new("a.md")); // displaces `first`

        assert_eq!(ix.resolve(&first), None, "evicted from the live map");
        assert!(ix.is_tombstoned(&first), "and retired on the way out");
        assert!(ix.is_known(&first), "so it can never be reissued");
        assert_eq!(ix.resolve(&second), Some(PathBuf::from("a.md")));
    }

    #[test]
    fn re_registering_an_id_retires_its_tombstone() {
        // The `restore`-from-bin path: `recycle` tombstones the id, `restore`
        // registers it again. A record cannot be live and retired at once, and
        // `render` has always resolved that in favour of live — so the in-memory
        // store must agree, or it disagrees with its own serialization until the
        // process restarts.
        let mut ix = FileIndex::new(fig::Format::Yaml);
        let id = Id("ccc333c".into());
        ix.set_host("registry.yaml", "title: ID registry\n")
            .unwrap();

        ix.register(&id, Path::new("a.md"));
        ix.unregister(&id);
        assert!(ix.is_tombstoned(&id), "retired by the delete");

        ix.register(&id, Path::new("a.md")); // the restore
        assert_eq!(ix.resolve(&id), Some(PathBuf::from("a.md")), "live again");
        assert!(!ix.is_tombstoned(&id), "and no longer retired");

        // Which is what the file said all along, so the round trip is lossless.
        let text = ix.render().unwrap();
        let reloaded = FileIndex::parse(Path::new("registry.yaml"), &text).unwrap();
        assert_eq!(reloaded.resolve(&id), Some(PathBuf::from("a.md")));
        assert!(!reloaded.is_tombstoned(&id));
    }

    #[test]
    fn a_restored_id_that_is_later_displaced_is_still_never_reissued() {
        // The two clauses of `register` in one sequence — and the reason they
        // had to land together. Un-retiring on registration, without retiring
        // what a registration displaces, would make this the *easiest* way to
        // forget an id rather than an impossible one.
        let mut ix = FileIndex::new(fig::Format::Yaml);
        let (restored, other) = (Id("aaa111a".into()), Id("bbb222b".into()));

        ix.register(&restored, Path::new("a.md"));
        ix.unregister(&restored); // recycled
        ix.register(&restored, Path::new("a.md")); // restored — tombstone cleared
        ix.register(&other, Path::new("a.md")); // displaced again

        assert!(ix.is_known(&restored), "retired again on the way out");
    }

    /// Laws over the registry, rather than examples of it.
    ///
    /// DESIGN §5 singles this store out: the graph and resolution parts of the
    /// index are a derived cache, harmless when stale, but `id → path` is
    /// *authoritative, non-derivable state* — lose it and no amount of reading
    /// the documents puts it back. So the invariant it keeps deserves to be
    /// asserted universally rather than witnessed:
    ///
    /// > **`forward` and `reverse` are two views of one bijection.**
    ///
    /// [`InMemoryIndex::register`] says as much in its own doc comment, and the
    /// history is instructive — 11abd38 fixed a displacement that went unevicted
    /// in one direction, leaving an id with a forward entry to a path it no
    /// longer owned. That is a two-line slip in a four-line function, invisible
    /// to any single example, and it is precisely what a sequence of colliding
    /// registrations finds.
    ///
    /// The generators use **three ids and three paths**. That is the whole
    /// design: a small universe makes collision and displacement the common
    /// case rather than a rare one, which is where every bug in a bijection
    /// lives. A generator drawing fresh ids would exercise the easy path
    /// forever.
    mod properties {
        use super::*;
        use proptest::prelude::*;

        const IDS: [&str; 3] = ["aaa111a", "bbb222b", "ccc333c"];
        const PATHS: [&str; 3] = ["a.md", "b.md", "n/c.md"];

        #[derive(Debug, Clone)]
        enum Op {
            Register { id: usize, path: usize },
            SetPath { id: usize, path: usize },
            Unregister { id: usize },
        }

        fn op() -> impl Strategy<Value = Op> {
            prop_oneof![
                (0..IDS.len(), 0..PATHS.len()).prop_map(|(id, path)| Op::Register { id, path }),
                (0..IDS.len(), 0..PATHS.len()).prop_map(|(id, path)| Op::SetPath { id, path }),
                (0..IDS.len()).prop_map(|id| Op::Unregister { id }),
            ]
        }

        fn run(ix: &mut impl IndexStore, op: &Op) {
            match op {
                Op::Register { id, path } => {
                    ix.register(&Id(IDS[*id].into()), Path::new(PATHS[*path]))
                }
                Op::SetPath { id, path } => {
                    ix.set_path(&Id(IDS[*id].into()), Path::new(PATHS[*path]))
                }
                Op::Unregister { id } => ix.unregister(&Id(IDS[*id].into())),
            }
        }

        /// Both directions of the claim. Only ids and paths the sequence names
        /// can be in the maps, so checking those is checking all of them.
        fn assert_bijection(ix: &impl IndexStore) -> std::result::Result<(), TestCaseError> {
            for id in IDS.map(|i| Id(i.into())) {
                if let Some(path) = ix.resolve(&id) {
                    let back = ix.id_for_path(&path);
                    prop_assert_eq!(
                        back.as_ref(),
                        Some(&id),
                        "`{}` resolves to `{}`, which does not point back",
                        id,
                        path.display()
                    );
                }
            }
            for path in PATHS.map(Path::new) {
                if let Some(id) = ix.id_for_path(path) {
                    let back = ix.resolve(&id);
                    prop_assert_eq!(
                        back.as_deref(),
                        Some(path),
                        "`{}` carries `{}`, which does not point back",
                        path.display(),
                        id
                    );
                }
            }
            Ok(())
        }

        proptest! {
            /// The registry never names two paths for one id, or two ids for one
            /// path — after *any* sequence of registrations, moves and
            /// retirements, however much they displace each other.
            #[test]
            fn the_id_map_stays_a_bijection(ops in prop::collection::vec(op(), 1..12)) {
                let mut ix = InMemoryIndex::new();
                for (n, op) in ops.iter().enumerate() {
                    run(&mut ix, op);
                    assert_bijection(&ix).map_err(|e| {
                        TestCaseError::fail(format!("after op {n} ({op:?}) of {ops:?}: {e}"))
                    })?;
                }
            }

            /// The same law for the persistent store, which delegates but wraps
            /// the delegation in dirty-tracking and tombstones — and inherits
            /// nothing automatically just because it forwards today.
            #[test]
            fn the_persistent_id_map_stays_a_bijection(
                ops in prop::collection::vec(op(), 1..12),
            ) {
                let mut ix = FileIndex::new(fig::Format::Yaml);
                for op in &ops {
                    run(&mut ix, op);
                    assert_bijection(&ix)?;
                }
            }

            /// **A retired id is never forgotten.** Mint-by-rejection depends on
            /// it: an id that stops resolving must stay *known*, or a later mint
            /// could reissue it and a dangling `id:` reference would quietly
            /// change meaning — the difference between "that document was
            /// deleted" and "that was never issued here" (DESIGN §10).
            #[test]
            fn a_retired_id_stays_known_forever(ops in prop::collection::vec(op(), 1..12)) {
                let mut ix = FileIndex::new(fig::Format::Yaml);
                let mut retired: Vec<Id> = Vec::new();
                for op in &ops {
                    run(&mut ix, op);
                    if let Op::Unregister { id } = op {
                        retired.push(Id(IDS[*id].into()));
                    }
                    for id in &retired {
                        prop_assert!(ix.is_known(id), "`{id}` was retired and then forgotten");
                    }
                }
            }

            /// **An id that has ever been issued stays known.** DESIGN §10
            /// settles the tombstone question with "IDs are never reissued", and
            /// `is_known` is the predicate mint-by-rejection asks, so it must be
            /// monotonic: once true for an id, true forever.
            ///
            /// Displacements included: this property found that an evicted id
            /// was forgotten rather than tombstoned, and `FileIndex::register`
            /// now retires what it displaces, so the law holds unscoped.
            #[test]
            fn is_known_is_monotonic(ops in prop::collection::vec(op(), 1..12)) {
                let mut ix = FileIndex::new(fig::Format::Yaml);
                let mut ever = Vec::new();
                for (n, op) in ops.iter().enumerate() {
                    run(&mut ix, op);
                    for id in IDS.map(|i| Id(i.into())) {
                        if ix.is_known(&id) && !ever.contains(&id) {
                            ever.push(id);
                        }
                    }
                    for id in &ever {
                        prop_assert!(
                            ix.is_known(id),
                            "`{id}` was known and then forgotten at op {n} ({op:?}) of {ops:?}"
                        );
                    }
                }
            }

            /// **Rollback restores exactly.** `checkpoint`/`rollback` is what
            /// lets a failed change set unwind the registry alongside the
            /// documents (DESIGN §5: the registry's write rides the same unit).
            /// A partial restore would leave the one artifact that cannot be
            /// rebuilt disagreeing with the files it describes.
            #[test]
            fn rollback_restores_the_map_it_checkpointed(
                before in prop::collection::vec(op(), 0..6),
                after in prop::collection::vec(op(), 1..8),
            ) {
                let mut ix = InMemoryIndex::new();
                for op in &before {
                    run(&mut ix, op);
                }
                let snapshot: Vec<Option<PathBuf>> =
                    IDS.map(|i| ix.resolve(&Id(i.into()))).into();

                ix.checkpoint();
                for op in &after {
                    run(&mut ix, op);
                }
                ix.rollback();

                let restored: Vec<Option<PathBuf>> =
                    IDS.map(|i| ix.resolve(&Id(i.into()))).into();
                prop_assert_eq!(restored, snapshot, "rolled back to a different map");
                assert_bijection(&ix)?;
            }

            /// **A rendered registry reads back as itself.** The store is a real
            /// document a user (or a merge) can open, so the text is the durable
            /// form — and its records must survive the round trip, tombstones
            /// included, since a tombstone that failed to persist would let the
            /// id be reissued by the next run.
            #[test]
            fn a_rendered_registry_parses_back_to_the_same_records(
                ops in prop::collection::vec(op(), 1..12),
            ) {
                let mut ix = FileIndex::new(fig::Format::Yaml);
                ix.set_host("registry.yaml", "title: ID registry\n").unwrap();
                for op in &ops {
                    run(&mut ix, op);
                }

                let text = ix.render().expect("render");
                let reloaded = FileIndex::parse(Path::new("registry.yaml"), &text)
                    .expect("prov's own registry must parse");

                for id in IDS.map(|i| Id(i.into())) {
                    prop_assert_eq!(
                        reloaded.resolve(&id),
                        ix.resolve(&id),
                        "`{}` did not survive the round trip through:\n{}",
                        id,
                        text
                    );
                    prop_assert_eq!(
                        reloaded.is_tombstoned(&id),
                        ix.is_tombstoned(&id),
                        "`{}`'s tombstone did not survive:\n{}",
                        id,
                        text
                    );
                }
                assert_bijection(&reloaded)?;
            }
        }
    }
}
