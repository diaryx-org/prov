//! The workspace handle — where the filesystem, relation vocabulary, identity
//! policy, and index store are composed.
//!
//! The type parameters encode the "identity is a bolt-on" design: a
//! `Workspace<FS>` defaults to [`NoIdentity`] + [`NoIndex`] — paths only, with
//! the identity machinery compiled out. Opting in is one builder line that flips
//! a type parameter:
//!
//! ```no_run
//! use prov::workspace::Workspace;
//! use prov::relation::RelationSet;
//! # fn demo<FS>(fs: FS) {
//! // Paths only — no ID ever touches a document.
//! let ws = Workspace::builder(fs).root("vault").build();
//! # let _ = ws;
//! # }
//! ```
//!
//! The filesystem-driven `scan`/traverse/mutate engine ports from `diaryx_core`
//! next; the seams are in place so that port has somewhere to land.

use prov_graph::document::Document;
use prov_graph::fs::{DirEntry, Metadata};
use prov_graph::graph::{Backlink, CensusEntry, Graph, Node, ReadSettings, TreeOptions, Walk};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::change::{ChangeSet, FileOp};
use crate::config::{Fixity, History, IdStorage};
use crate::fixity::FixityCache;
use crate::identity::{IdentityPolicy, NoIdentity, Trigger};
use prov_graph::document::EmbedStyle;
use prov_graph::error::{Error, Result};
use prov_graph::fs::{ReadStorage};
use prov_store::fs::{Storage};
use prov_graph::graph::Target;
use prov_graph::index::{Collision, IdIndex, NoIndex};
use prov_store::index::{IndexStore};
use prov_graph::link::{self, Addressing, Link, LinkStyle, ReferenceStyle, Wrapper};
use prov_graph::memo::{ReadScope, lock};
use prov_graph::meta::Value;
use prov_graph::relation::RelationSet;
use prov_graph::title::TitleIndex;

/// The workspace's **policy knobs**, as one value.
///
/// Every field here answers "how does this workspace author and read
/// documents?" — the vocabulary, the reference style, where ids live, how far
/// checksums go. What is *not* here is as deliberate: the filesystem and root
/// are a location rather than a policy; the identity policy and index store are
/// type parameters, because [`Workspace`]'s whole "identity is a bolt-on"
/// design is that they can be compiled out; and the [`FixityCache`] is
/// device-local memory the workspace is handed, not something it declares.
///
/// It exists as a struct because it was previously ten loose fields, and every
/// one of them had to be hand-copied through
/// [`identity`](WorkspaceBuilder::identity), [`index`](WorkspaceBuilder::index),
/// [`build`](WorkspaceBuilder::build), and [`Workspace`]'s `Clone` — four lists
/// that had to agree, with nothing but review to make them. Carried whole,
/// those four sites stop mentioning the knobs at all, so adding one is a field
/// and its accessor rather than a field and four transcriptions.
///
/// [`Default`] is the paths-only workspace [`Workspace::builder`] starts from.
/// Note `id_storage` defaults to [`IdStorage::Registry`] rather than
/// [`WorkspaceConfig`](crate::config::WorkspaceConfig)'s `both`: a hand-built
/// workspace keeps writing id-free documents until it opts in, where one built
/// *from a config* gets what the config declares — that is what the
/// `From<&WorkspaceConfig>` impl below is for.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The relation vocabulary — see [`Workspace::relations`].
    pub relations: RelationSet,
    /// The path style links are authored in — see [`Workspace::link_style`].
    pub link_style: LinkStyle,
    /// The legacy "author links by id" axis, superseded by an explicit
    /// `reference_style` — see [`Workspace::reference_style`].
    pub id_links: bool,
    /// The workspace-default reference style, overriding the `link_style` /
    /// `id_links` pair when set — see [`Workspace::reference_style`].
    pub reference_style: Option<ReferenceStyle>,
    /// The metadata format a new document gets when it inherits no parent block
    /// — see [`Workspace::default_embed_format`].
    pub default_embed_format: fig::Format,
    /// How that metadata is embedded — see [`Workspace::embed_style`].
    pub embed_style: EmbedStyle,
    /// How far content checksums are recorded — see [`Workspace::fixity`].
    pub fixity: Fixity,
    /// Whether a history store is kept, and on what trigger — see
    /// [`Workspace::history`].
    pub history: History,
    /// Where a document's stable id is persisted — see
    /// [`Workspace::id_storage`].
    pub id_storage: IdStorage,
    /// What this workspace calls itself — the qualifier a cross-workspace
    /// reference names it by. Empty means anonymous, so no `id:<ws>/<id>`
    /// reference can ever be recognized as pointing back here. See
    /// [`WorkspaceConfig::workspace_id`](crate::config::WorkspaceConfig::workspace_id).
    pub workspace_id: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            relations: RelationSet::diaryx(),
            link_style: LinkStyle::default(),
            id_links: false,
            reference_style: None,
            default_embed_format: fig::Format::Yaml,
            embed_style: EmbedStyle::Delimited,
            fixity: Fixity::Payloads,
            history: History::Off,
            id_storage: IdStorage::Registry,
            workspace_id: String::new(),
        }
    }
}

/// The settings a [`WorkspaceConfig`] declares — the whole of what a workspace
/// says about itself in its own config document, in the form the builder takes.
///
/// This is the conversion the CLI used to spell out as ten builder calls. It
/// lives here rather than in [`config`](crate::config) because `workspace`
/// already depends on `config` (for [`Fixity`], [`History`], [`IdStorage`]) and
/// the reverse edge would be a cycle for no gain.
///
/// Two fields are deliberately not read. `identity` is a
/// [`Registration`](crate::identity::Registration), which the caller turns into
/// a policy *type* (`Minter::with(config.identity, seed)`) — it cannot be a
/// setting without giving up the bolt-on design. And `id_links` stays at its
/// default, because a config always yields an explicit `reference_style`, which
/// supersedes that legacy axis entirely.
///
/// [`WorkspaceConfig`]: crate::config::WorkspaceConfig
impl From<&crate::config::WorkspaceConfig> for Settings {
    fn from(config: &crate::config::WorkspaceConfig) -> Self {
        Self {
            relations: config.relation_set(),
            link_style: config.link_format(),
            reference_style: Some(config.reference_style()),
            default_embed_format: config.default_embed_format,
            embed_style: config.embed_style,
            fixity: config.fixity,
            history: config.history,
            id_storage: config.id_storage,
            workspace_id: config.workspace_id.clone(),
            ..Self::default()
        }
    }
}

/// A composed workspace: a filesystem, an identity policy, an index store, and
/// the [`Settings`] that say how it authors and reads documents.
#[derive(Debug)]
pub struct Workspace<FS, Id = NoIdentity, Ix = NoIndex> {
    /// The read core: the root, the filesystem, the id index, and the memo.
    /// Every traversal this workspace performs *is* a `prov-graph` traversal —
    /// the read methods below forward here rather than restating the walk, so
    /// the two can never drift into two answers for one workspace.
    graph: Graph<FS, Ix>,
    identity: Id,
    /// All ten authoring settings. The three the read core also needs are
    /// copied into `graph`'s own [`ReadSettings`] when the workspace is built;
    /// nothing mutates either afterwards, so the copies cannot drift.
    settings: Settings,
    /// Documents that earned an id this operation and, under a stamping mode,
    /// still need it written into their own frontmatter. Drained by
    /// [`commit`](Workspace::commit) into the operation's change set, so a
    /// document's id and the registry entry for it land in the same crash-atomic
    /// write — never one without the other.
    pending_stamps: Vec<(PathBuf, prov_graph::identity::Id)>,
    /// What this device remembers of the workspace's file digests. Absent until
    /// a host supplies one — see [`crate::fixity::FixityCache`], which is also
    /// where the rule about who may consult it is written down.
    fixity_cache: Mutex<Option<FixityCache>>,
}

/// Hand-written rather than derived, because the two memories carry different
/// answers to "what does a second handle on this workspace inherit?".
///
/// The **read memo** starts empty, with no scope open — a requirement, not a
/// preference. A [`ReadScope`] guard points at the memo it opened, and a clone
/// has no guard pointing at it; inheriting a nonzero depth would leave the copy
/// permanently scoped, remembering reads with nothing left to close it.
///
/// The **fixity cache** is copied, because it is a memory of the disk and both
/// handles are looking at the same disk. Two clones that both learn things do
/// diverge, and whichever is persisted last is the one that keeps what it
/// learned — which costs a re-hash and nothing else, since every entry is
/// validated against the file's own stat before it is believed.
impl<FS: Clone, Id: Clone, Ix: Clone> Clone for Workspace<FS, Id, Ix> {
    fn clone(&self) -> Self {
        Self {
            graph: self.graph.clone(),
            identity: self.identity.clone(),
            settings: self.settings.clone(),
            pending_stamps: self.pending_stamps.clone(),
            fixity_cache: Mutex::new(lock(&self.fixity_cache).clone()),
        }
    }
}

impl<FS> Workspace<FS, NoIdentity, NoIndex> {
    /// Start building a paths-only workspace over `fs`: root `"."`, identity
    /// off, and [`Settings::default`] — the [`RelationSet::diaryx`] vocabulary,
    /// the default [`LinkStyle`] (`MarkdownRoot`, matching diaryx), and
    /// [`IdStorage::Registry`], which is *not*
    /// [`WorkspaceConfig`](crate::config::WorkspaceConfig)'s `both` default, so a
    /// hand-built workspace keeps writing id-free documents unless it opts in.
    ///
    /// Consumers driving the builder from a config (the normal path) hand it the
    /// declared modes whole, with
    /// [`settings`](WorkspaceBuilder::settings)`(Settings::from(&config))`.
    pub fn builder(fs: FS) -> WorkspaceBuilder<FS, NoIdentity, NoIndex> {
        WorkspaceBuilder {
            fs,
            root: PathBuf::from("."),
            identity: NoIdentity,
            index: NoIndex,
            settings: Settings::default(),
            fixity_cache: None,
        }
    }
}

impl<FS, Id, Ix> Workspace<FS, Id, Ix> {
    /// Construct the history service for this workspace.
    ///
    /// Existing verb methods remain available below as compatibility
    /// forwarding methods while history moves behind this host boundary.
    pub fn history_store(&self) -> crate::history::HistoryStore<&Self> {
        crate::history::HistoryStore::new(self)
    }

    /// The history service over a *mutable* borrow — what the four mutating
    /// verbs need, since landing a change set is a mutation of the workspace.
    pub fn history_store_mut(&mut self) -> crate::history::HistoryStore<&mut Self> {
        crate::history::HistoryStore::new(self)
    }
}

impl<FS: Storage, Id, Ix: IndexStore> prov_history::HistoryReadHost for Workspace<FS, Id, Ix> {
    type Fs = FS;
    type Ix = Ix;

    fn graph(&self) -> &Graph<Self::Fs, Self::Ix> {
        self.graph()
    }

    fn embed_style(&self) -> EmbedStyle {
        self.embed_style()
    }

    fn default_embed_format(&self) -> fig::Format {
        self.default_embed_format()
    }

    fn history_captures(&self) -> bool {
        self.history().captures()
    }

    fn history_relation(&self) -> Option<&str> {
        self.relations().history_relation()
    }

    async fn history_path(&self, root_doc: &Path) -> Result<Option<PathBuf>> {
        self.history_path(root_doc).await
    }

    async fn reachable_files(&self, root_doc: &Path) -> Result<BTreeSet<PathBuf>> {
        self.reachable_files(root_doc).await
    }

    async fn history_exclusions(&self, root_doc: &Path) -> Result<Vec<PathBuf>> {
        // The two the store cannot know about: where the bin parks the bytes a
        // user has already consigned, and which page prov derives rather than the
        // author writing. Both are prefixes; a file names only itself.
        let mut excluded = Vec::new();
        if let Some(index) = self.recycle_bin_path(root_doc).await? {
            excluded.push(crate::history::store_dir(&index).join("items"));
        }
        if let Some(about) = self.about_path(root_doc).await? {
            excluded.push(about);
        }
        Ok(excluded)
    }

    fn registration_conflict(
        &self,
        id: &prov_graph::identity::Id,
        path: &Path,
    ) -> Option<Collision> {
        self.registration_conflict(id, path)
    }
}

impl<FS: Storage, Id, Ix: IndexStore> prov_history::HistoryWriteHost for Workspace<FS, Id, Ix> {
    fn change(&mut self) -> ChangeSet {
        self.change()
    }

    async fn commit(&mut self, cs: ChangeSet) -> Result<()> {
        self.commit(cs).await
    }

    fn fixity_cached(&self, path: &Path, meta: &Metadata) -> Option<String> {
        self.fixity_cached(path, meta)
    }

    fn fixity_remember(&self, path: &Path, meta: &Metadata, hash: &str) {
        self.fixity_remember(path, meta, hash)
    }
}

impl<FS, Id, Ix> Workspace<FS, Id, Ix> {
    /// The read core this workspace traverses through.
    ///
    /// Hand this to anything that only needs to *see* the workspace: it can
    /// read, resolve and walk, and it cannot write, because [`Graph`] exposes
    /// no method that does.
    pub fn graph(&self) -> &Graph<FS, Ix> {
        &self.graph
    }

    /// The workspace root.
    pub fn root(&self) -> &Path {
        self.graph.root()
    }

    /// Join a workspace-relative path — a [`Node::path`](prov_graph::graph::Node::path),
    /// or any other path this crate hands back — onto the workspace root,
    /// producing the fs-readable form a [`Storage`] read needs.
    ///
    /// The two path forms are deliberately kept apart: everything this crate
    /// returns (`Node::path`, [`Target::Path`], …) is workspace-relative and
    /// root-independent, so a workspace can be re-rooted to a different
    /// directory without touching a single stored path. `fs_path` is the one
    /// place that independence is given up, for the caller that actually needs
    /// to open the file.
    pub fn fs_path(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.graph.fs_path(rel)
    }

    /// The configured relation vocabulary.
    pub fn relations(&self) -> &RelationSet {
        &self.settings.relations
    }

    /// Open a **read scope**: for as long as the returned guard is held, a
    /// document this workspace reads is read once, and every later read of it
    /// this operation makes is answered from memory.
    ///
    /// This is for an operation composed of several passes over the same
    /// documents — [`check`](Self::check) is the archetype, and opens one for
    /// itself. Scopes nest, so an operation may open one and freely call
    /// another that opens its own; only the outermost exit drops what was
    /// remembered.
    ///
    /// Bounded by the operation on purpose. Anything prov writes forgets itself
    /// ([`commit`](Self::commit) drops what its change set touched), and the
    /// scope ends before control returns to a caller who might write behind
    /// prov's back — which is why this needs no invalidation policy and has
    /// none. See [`prov_graph::memo`].
    ///
    /// ```no_run
    /// # use prov::Workspace;
    /// # async fn demo<FS: prov::Storage, Id, Ix: prov::IndexStore>(ws: &Workspace<FS, Id, Ix>)
    /// #     -> prov::Result<()> {
    /// let _scope = ws.read_scope();
    /// let findings = ws.check("index.md").await?;
    /// # let _ = findings;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "the scope ends the moment its guard is dropped"]
    pub fn read_scope(&self) -> ReadScope<'_> {
        self.graph.read_scope()
    }

    /// Give this workspace a [`FixityCache`] to hash through, or `None` to take
    /// the one it has away.
    ///
    /// prov never reads or writes the cache's file — it has no notion of a
    /// location outside the workspace, and the cache belongs outside it. The
    /// host decodes the bytes, hands the cache over, and takes it back with
    /// [`take_fixity_cache`](Self::take_fixity_cache) to persist whatever was
    /// learned.
    pub fn set_fixity_cache(&mut self, cache: Option<FixityCache>) {
        *lock(&self.fixity_cache) = cache;
    }

    /// Take back the [`FixityCache`], with everything this workspace learned
    /// while it held it. Check [`FixityCache::is_dirty`] before writing it out:
    /// a run that learned nothing should not rewrite the file to say so.
    pub fn take_fixity_cache(&mut self) -> Option<FixityCache> {
        lock(&self.fixity_cache).take()
    }

    /// The remembered digest for the workspace-relative `path`, if the cache
    /// still describes the file `meta` stat'ed.
    pub(crate) fn fixity_cached(
        &self,
        path: &Path,
        meta: &prov_graph::fs::Metadata,
    ) -> Option<String> {
        lock(&self.fixity_cache)
            .as_ref()?
            .get(path, meta)
            .map(str::to_string)
    }

    /// Remember that `path` hashed to `hash` at the stat `meta` describes.
    /// Silently nothing when no cache is attached.
    pub(crate) fn fixity_remember(&self, path: &Path, meta: &prov_graph::fs::Metadata, hash: &str) {
        if let Some(cache) = lock(&self.fixity_cache).as_mut() {
            cache.put(path, meta, hash);
        }
    }

    /// Forget everything `cs` is about to change, in both the operation's read
    /// memo and the fixity cache.
    ///
    /// Called before the set lands rather than after, because forgetting is
    /// never the wrong answer: a set that then fails and rolls back has cost one
    /// re-read, where the other order would have left a memo describing bytes
    /// that no longer exist.
    ///
    /// For the fixity cache this is tidiness, not the safety mechanism. An entry
    /// is validated against the file's own stat, so *any* write — by prov, an
    /// editor, or a sync daemon — retires it whether or not prov thought to say
    /// so. The read memo has no such backstop, which is why it needs this.
    fn forget_written(&self, cs: &ChangeSet) {
        let mut memo = self.graph.memo_lock();
        let mut cache = lock(&self.fixity_cache);
        let mut forget = |path: &Path| {
            memo.forget(path);
            if let Some(cache) = cache.as_mut() {
                cache.forget(path);
            }
        };
        for op in cs.ops() {
            match op {
                FileOp::Write { path, .. }
                | FileOp::Remove { path }
                | FileOp::CopyFrom { path, .. } => forget(path),
                FileOp::Rename { from, to } => {
                    forget(from);
                    forget(to);
                }
            }
        }
    }

    /// The identity policy.
    pub fn identity(&self) -> &Id {
        &self.identity
    }

    /// The index store.
    pub fn index(&self) -> &Ix {
        self.graph.index()
    }

    /// The link style this workspace authors in (read from the root's
    /// `link_format`, or the default).
    pub fn link_style(&self) -> LinkStyle {
        self.settings.link_style
    }

    /// Whether this workspace authors durable structural links by id
    /// (registering the target) rather than a path — a convenience view over the
    /// effective default [`reference_style`](Self::reference_style).
    pub fn id_links(&self) -> bool {
        self.reference_style().registers()
    }

    /// How far this workspace records content checksums — attachments only (the
    /// default), attachments plus document bodies, or off. Consulted by the ops
    /// that *record* a hash (`attach`, `edit`); `check` honors any hash already
    /// recorded regardless.
    pub fn fixity(&self) -> Fixity {
        self.settings.fixity
    }

    /// Whether this workspace keeps a history store, and on what trigger.
    ///
    /// Gating *capture* is the CLI's job, but the axis has to reach the library
    /// for the opposite reason: a workspace that declares `manual` has said it
    /// wants a safety net, so `check` can tell "no store yet" from "a store is
    /// sitting there and the root has stopped pointing at it"
    /// ([`Finding::HistoryStoreUnlinked`](crate::validate::Finding::HistoryStoreUnlinked)).
    /// With the axis off there is nothing to be missing, and the pass stays
    /// silent.
    pub fn history(&self) -> History {
        self.settings.history
    }

    /// How this workspace embeds metadata — the family (`delimited`,
    /// `code-block`, `html-script`, …) that, with
    /// [`default_embed_format`](Self::default_embed_format), resolves to the
    /// concrete carrier a document prov authors gets.
    pub fn embed_style(&self) -> EmbedStyle {
        self.settings.embed_style
    }

    /// Where this workspace persists document ids (DESIGN §5). Consulted by the
    /// ops that *author* a document — under a stamping mode each one carries its
    /// own `id` — and by `check`, which reconciles the two homes against each
    /// other.
    pub fn id_storage(&self) -> IdStorage {
        self.settings.id_storage
    }

    /// What this workspace calls itself — the qualifier a cross-workspace
    /// reference names it by, or `""` when the workspace is anonymous.
    ///
    /// Its one operational use is recognizing a reference that names *this*
    /// workspace: `id:notes/abc` read inside the workspace called `notes` is a
    /// local reference that resolves through the registry, which is what lets a
    /// document keep working after being copied here from somewhere else.
    pub fn workspace_id(&self) -> &str {
        &self.settings.workspace_id
    }

    /// The workspace-default reference style — the fallback for any relation
    /// without its own `style` override. An explicit `reference_style` builder
    /// value wins; otherwise it is derived from the legacy `link_style`/`id_links`
    /// builder inputs so existing configurations behave exactly as before.
    pub fn reference_style(&self) -> ReferenceStyle {
        self.settings.reference_style.unwrap_or(ReferenceStyle {
            wrapper: Wrapper::Markdown,
            addressing: if self.settings.id_links {
                Addressing::Id
            } else {
                Addressing::Path
            },
            label: false,
            path_style: self.settings.link_style,
        })
    }

    /// The reference style prov authors `relation`'s links in: the
    /// relation's own override if it declares one, else the workspace default.
    pub fn reference_style_for(&self, relation: &str) -> ReferenceStyle {
        self.settings
            .relations
            .style_for(relation)
            .unwrap_or_else(|| self.reference_style())
    }

    /// The metadata format a new document gets when it inherits no parent block
    /// — a *default* for authoring, not a workspace constraint (existing
    /// documents keep their own format on write, §7).
    pub fn default_embed_format(&self) -> fig::Format {
        self.settings.default_embed_format
    }

    /// Mutable access to the index store (e.g. to persist it after mutations).
    pub fn index_mut(&mut self) -> &mut Ix {
        self.graph.index_mut()
    }
}

impl<FS, Id, Ix: IndexStore> Workspace<FS, Id, Ix> {
    /// Whether registering `id` at `path` would displace a registration the index
    /// already holds — the guard for any op that registers an id it did **not**
    /// just mint.
    ///
    /// A freshly minted id cannot collide, which is why most registrations need
    /// no check. The ones that do are the ops that carry an id in from somewhere
    /// else: a recycle-bin record re-registering a document's old id, and a
    /// history restore re-registering an id out of a captured manifest. In both,
    /// time has passed, and the workspace may have acquired that id — or that
    /// path — meanwhile.
    ///
    /// Checks **both directions**, because `id_storage` defaults to `both` and so
    /// the two fail independently: the target path can be free while the id is
    /// taken, and the id can be free while the path already carries another one.
    /// Returns `None` when the exact pair is already registered — re-registering
    /// what is already there displaces nothing.
    pub fn registration_conflict(
        &self,
        id: &prov_graph::identity::Id,
        path: &Path,
    ) -> Option<Collision> {
        if let Some(held_by) = self.graph.index().resolve(id)
            && held_by != path
        {
            return Some(Collision::Id {
                id: id.clone(),
                held_by,
            });
        }
        if let Some(held) = self.graph.index().id_for_path(path)
            && held != *id
        {
            return Some(Collision::Path {
                path: path.to_path_buf(),
                held,
            });
        }
        None
    }

    /// Whether moving `id` onto `dest` — via [`set_path`](IndexStore::set_path),
    /// not a fresh [`register`](IndexStore::register) — would displace a
    /// *different* id already registered there. The guard behind
    /// [`rename`](crate::mutate), [`separate`](crate::mutate), and
    /// [`combine`](crate::mutate): each relocates an id its document already
    /// holds, and none of their destinations is provably free of a live foreign
    /// registration (the same half-synced state [`registration_conflict`] exists
    /// for — a registry entry can name a path with no file behind it, or under a
    /// different id than the one now landing there).
    ///
    /// Deliberately **not** [`registration_conflict`]: `id` already resolves to
    /// wherever it is moving *from*, so that check's id-direction half would read
    /// as "already registered to a different document" on every ordinary move —
    /// the document it is leaving. Only the path direction is the risk a move
    /// introduces, so this checks just that half, and (matching
    /// [`registration_conflict`]'s own `held != id` discount) a `dest` that
    /// already carries this same `id` — a same-id no-op — is not a collision.
    ///
    /// [`registration_conflict`]: Self::registration_conflict
    pub(crate) fn move_conflict(
        &self,
        id: &prov_graph::identity::Id,
        dest: &Path,
    ) -> Option<Collision> {
        let held = self.graph.index().id_for_path(dest)?;
        (held != *id).then(|| Collision::Path {
            path: dest.to_path_buf(),
            held,
        })
    }
}

impl<FS: ReadStorage, Id, Ix: IdIndex> Workspace<FS, Id, Ix> {
    /// The registry document this workspace's root declares: the first target
    /// of the registry-pointer relation on `root_doc`, resolved. `None` when
    /// the vocabulary has no registry relation or the root does not declare
    /// one — the workspace simply has no (discoverable) registry.
    ///
    /// This is the anti-`.obsidian/` move: where the identity state lives is a
    /// fact *about the workspace*, declared in the root's own metadata like
    /// every other link — reachable, validatable, and tool-agnostic — rather
    /// than an app-private path convention.
    pub async fn registry_path(&self, root_doc: &Path) -> Result<Option<PathBuf>> {
        match self.relations().registry_relation() {
            Some(relation) => self.pointer_target(root_doc, relation).await,
            None => Ok(None),
        }
    }

    /// The workspace-config document this root declares via the config-pointer
    /// relation (§6, the registry's reachability move applied to policy). `None`
    /// when the vocabulary has no config relation or the root declares none —
    /// the workspace simply runs on defaults.
    pub async fn config_path(&self, root_doc: &Path) -> Result<Option<PathBuf>> {
        match self.relations().config_relation() {
            Some(relation) => self.pointer_target(root_doc, relation).await,
            None => Ok(None),
        }
    }

    /// The recycle-bin index document this root declares via the recycle-bin
    /// pointer relation (§6, the same reachability move as the registry). `None`
    /// when the vocabulary has no recycle relation or the root declares none —
    /// the workspace has no bin yet, so a deletion is a hard delete until one is
    /// bootstrapped.
    pub async fn recycle_bin_path(&self, root_doc: &Path) -> Result<Option<PathBuf>> {
        match self.relations().recycle_relation() {
            Some(relation) => self.pointer_target(root_doc, relation).await,
            None => Ok(None),
        }
    }

    /// The history-store index document this root declares via the history
    /// pointer relation (§6, the same reachability move as the registry). `None`
    /// when the vocabulary has no history relation or the root declares none —
    /// the workspace has no history store yet, so the first
    /// [`history_capture`](Self::history_capture) bootstraps one.
    pub async fn history_path(&self, root_doc: &Path) -> Result<Option<PathBuf>> {
        match self.relations().history_relation() {
            Some(relation) => self.pointer_target(root_doc, relation).await,
            None => Ok(None),
        }
    }

    /// The directories a **byte-parking store** keeps its contents under — the
    /// history store's interior and the recycle bin's `items/`.
    ///
    /// These are prov's own machinery, not the workspace's documents, and the
    /// distinction is the one this returns: a store's *index* is a document like
    /// any other (the root points at it, `check` validates it, a reader can open
    /// it and learn what the store holds), while everything beneath it is
    /// bookkeeping the workspace should be blind to.
    ///
    /// The line matters most for **names**. A shard index is titled
    /// `"{Month} {Year}"` and a binned document keeps the title it had, so a
    /// workspace that indexes these subtrees will resolve `[[January 2026]]` to a
    /// history shard, and `[[Some Note]]` to a copy of a note the author deleted
    /// — silently, since neither is anywhere the reader can see. Worse than a
    /// dead link, which at least reads as broken.
    ///
    /// Naming the directories rather than filtering paths afterwards is what
    /// keeps the *cost* out too: a scan that never descends does not read a
    /// thousand event documents in order to discard them.
    pub(crate) async fn parked_dirs(&self, root_doc: &Path) -> Result<Vec<PathBuf>> {
        let mut dirs = Vec::new();
        if let Some(index) = self.history_path(root_doc).await? {
            // The store's interior, not the store: the index document itself
            // stays reachable, so the `history` pointer is not a broken link and
            // the orphan sweep goes on ignoring what it never reached.
            dirs.push(crate::history::store_dir(&index).join(crate::history::EVENTS_DIR));
            dirs.push(crate::history::store_dir(&index).join(crate::history::BLOBS_DIR));
        }
        if let Some(index) = self.recycle_bin_path(root_doc).await? {
            dirs.push(crate::history::store_dir(&index).join("items"));
        }
        Ok(dirs)
    }

    /// The generated `about.md` this root declares via the about-pointer
    /// relation (§6, the same reachability move as the registry; spec §4's
    /// *generated prose* kind). `None` when the vocabulary has no about relation
    /// or the root declares none — the workspace has no generated page, which is
    /// what `about: off` looks like on disk.
    ///
    /// Note what this is *for*. Unlike the other pointers, nothing about reading
    /// the workspace depends on it: the page is written for a person, who finds
    /// it by its filename. The pointer is how **prov** locates the page to
    /// regenerate and to check for staleness, and how the file stays reachable
    /// instead of loose in the tree.
    pub async fn about_path(&self, root_doc: &Path) -> Result<Option<PathBuf>> {
        match self.relations().about_relation() {
            Some(relation) => self.pointer_target(root_doc, relation).await,
            None => Ok(None),
        }
    }

    /// Read a single workspace-config value by `key` from the linked config
    /// document. `None` when there is no config document or it lacks the key —
    /// the caller falls back to its default.
    pub async fn config_get(
        &self,
        root_doc: &Path,
        key: &str,
    ) -> Result<Option<prov_graph::meta::Value>> {
        let Some(config_doc) = self.config_path(root_doc).await? else {
            return Ok(None);
        };
        let (_, doc) = self.load(&config_doc).await?;
        Ok(doc.meta.get(key).cloned())
    }

    /// The effective [`WorkspaceConfig`] this root declares — the root's inline
    /// `prov:` block layered under the dedicated config document, over the
    /// defaults (the precedence `config document > root block > default`, the same
    /// layering [`config_findings`](crate::validate) lints and the CLI applies).
    /// This is how a library-level pass (validation's term-consistency check)
    /// reconstructs the `fields`/vocabulary declarations without the CLI's `Ctx`.
    ///
    /// [`WorkspaceConfig`]: crate::config::WorkspaceConfig
    pub async fn effective_config(
        &self,
        root_doc: &Path,
    ) -> Result<crate::config::WorkspaceConfig> {
        let mut config = crate::config::WorkspaceConfig::default();
        if let Ok((_, root)) = self.load(root_doc).await
            && let Some(block) = root.meta.get(crate::config::ROOT_CONFIG_KEY)
        {
            config.apply(block);
        }
        if let Some(config_doc) = self.config_path(root_doc).await? {
            let (_, doc) = self.load(&config_doc).await?;
            config.apply(&doc.meta);
        }
        Ok(config)
    }

    /// Resolve a `fields.<field>.vocabulary` pointer (a raw link string from
    /// config) to the vocabulary document's path, relative to `root_doc`. The
    /// same link machinery the structural pointers use ([`pointer_target`]), but
    /// the pointer is a config value rather than a relation on the root.
    ///
    /// [`pointer_target`]: Self::pointer_target
    pub fn vocabulary_path(&self, root_doc: &Path, pointer: &str) -> Option<PathBuf> {
        match self.resolve_link(&link::normalize(root_doc), &Link::parse(pointer)) {
            Target::Path(path) => Some(path),
            _ => None,
        }
    }

    /// Load and parse the controlled vocabulary a `fields` pointer names. `None`
    /// when the pointer does not resolve or the target is not a vocabulary store
    /// (no `vocabulary` marker). The store must be a whole-file config document
    /// (DESIGN §5); a markdown carrier is refused via [`require_whole_file`].
    ///
    /// [`require_whole_file`]: prov_graph::document::require_whole_file
    pub async fn load_vocabulary(
        &self,
        root_doc: &Path,
        pointer: &str,
    ) -> Result<Option<crate::vocabulary::Vocabulary>> {
        let Some(path) = self.vocabulary_path(root_doc, pointer) else {
            return Ok(None);
        };
        let (_, doc) = self.load(&path).await?;
        if let Some(carrier) = doc.carrier {
            prov_graph::document::require_whole_file(&path, carrier)?;
        }
        Ok(crate::vocabulary::Vocabulary::from_meta(&doc.meta))
    }
    /// Resolve the first target of `relation` declared on `root_doc` to a
    /// workspace path — the shared mechanic behind the registry and config
    /// pointers: a workspace resource named by a well-known relation on the root.
    async fn pointer_target(&self, root_doc: &Path, relation: &str) -> Result<Option<PathBuf>> {
        let root_doc = link::normalize(root_doc);
        let (_, doc) = self.load(&root_doc).await?;
        let Some(raw) = doc
            .meta
            .get(relation)
            .map(prov_graph::meta::Value::link_strings)
            .and_then(|targets| targets.into_iter().next())
        else {
            return Ok(None);
        };
        match self.resolve_link(&root_doc, &Link::parse(&raw)) {
            Target::Path(path) => Ok(Some(path)),
            _ => Ok(None),
        }
    }
}

impl<FS: Storage, Id: IdentityPolicy, Ix: IndexStore> Workspace<FS, Id, Ix> {
    /// Ensure the document at `path` has a registered stable ID, minting one if
    /// needed, and return it. Idempotent: an already-registered document
    /// returns its existing ID regardless of `event`.
    ///
    /// A fresh registration only happens when the identity policy's trigger
    /// set fires on `event` (DESIGN §4's registration lifecycle) — an inactive
    /// trigger is an error, so callers cannot silently grow the authoritative
    /// set beyond what the policy allows.
    pub async fn register(
        &mut self,
        path: &Path,
        event: Trigger,
    ) -> Result<prov_graph::identity::Id> {
        let path = link::normalize(path);
        if let Some(id) = self.graph.index().id_for_path(&path) {
            return Ok(id);
        }
        if !self.identity.registration().fires_on(event) {
            return Err(Error::Structure(format!(
                "identity policy does not register on {event:?}"
            )));
        }
        if !self.exists(&path).await? {
            return Err(Error::NotFound(path.to_path_buf()));
        }
        let id = self.mint_unique(&path);
        self.graph.index_mut().register(&id, &path);
        self.queue_stamp(&path, &id);
        Ok(id)
    }

    /// Mint until the ID is unknown to the index — including tombstones, so a
    /// deleted document's ID is never reissued to mean something else.
    pub(crate) fn mint_unique(&mut self, path: &Path) -> prov_graph::identity::Id {
        loop {
            let id = self.identity.mint(path);
            if !self.graph.index().is_known(&id) {
                return id;
            }
        }
    }

    /// The scalar prov writes for a durable link declared by `relation` from
    /// the document at `from` to `to` (titled `title`). The style is
    /// [`reference_style_for`](Self::reference_style_for)`(relation)`, so links
    /// going "down" (e.g. `contents`) and "up" (e.g. `part_of`) can differ. An
    /// `id`-addressing style registers `to` first (the link-by-id trigger) so the
    /// link survives a move untouched; if identity does not register on a link,
    /// [`format_reference`](link::format_reference) degrades it to a path.
    ///
    /// `target_exists` says whether `to` is already on disk: `true` registers it
    /// through the existence-checked [`register`](Self::register); `false` (a
    /// document being created in the same operation) mints and registers directly.
    /// The single seam through which create, rename repair, and autofix author a
    /// link.
    pub(crate) async fn authored_target(
        &mut self,
        relation: &str,
        from: &Path,
        to: &Path,
        title: &str,
        target_exists: bool,
    ) -> Result<String> {
        let style = self.reference_style_for(relation);
        let id = if style.registers() && self.identity.registration().fires_on(Trigger::Link) {
            Some(if target_exists {
                self.register(to, Trigger::Link).await?
            } else {
                self.register_for_authoring(to)
            })
        } else {
            None
        };
        Ok(link::format_reference(style, from, to, id.as_ref(), title))
    }

    /// Ensure `path` has an ID for the purpose of authoring a link *to* a
    /// document this same operation is creating — so the on-disk existence check
    /// in [`register`](Self::register) does not yet hold. Idempotent: returns any
    /// existing ID, else mints and registers one.
    pub(crate) fn register_for_authoring(&mut self, path: &Path) -> prov_graph::identity::Id {
        let path = link::normalize(path);
        if let Some(id) = self.graph.index().id_for_path(&path) {
            return id;
        }
        let id = self.mint_unique(&path);
        self.graph.index_mut().register(&id, &path);
        self.queue_stamp(&path, &id);
        id
    }

    /// Note that `path` should carry `id` in its own frontmatter, for
    /// [`commit`](Self::commit) to stage. A no-op unless the workspace stores ids
    /// in the document (DESIGN §5) — under registry-only storage a document never
    /// learns its own id.
    fn queue_stamp(&mut self, path: &Path, id: &prov_graph::identity::Id) {
        if self.settings.id_storage.stamps_frontmatter() {
            self.pending_stamps.push((path.to_path_buf(), id.clone()));
        }
    }
}

impl<FS, Id, Ix> Workspace<FS, Id, Ix> {
    /// The underlying filesystem.
    pub fn fs(&self) -> &FS {
        self.graph.fs()
    }
}

impl<FS: Storage, IdP, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Open a [`ChangeSet`] for a mutation: an empty set, and a checkpoint of
    /// the index so a failure can put it back.
    ///
    /// Pairs with exactly one [`commit`](Self::commit), and opens *before* the
    /// op's first index touch rather than at its writes — authoring an id-form
    /// link registers its target, so the registrations an op makes while
    /// computing its edits are part of what a failure has to unwind.
    ///
    /// An op can also fail *between* the two, by `?` on an edit it was still
    /// computing — a malformed parent block rejected by the editor, say. Its
    /// writes never happened, but its registrations did, and no `commit` ran to
    /// unwind them. The leak is not hypothetical: `create` mints an ID for the
    /// child *before* authoring the parent's entry, so a failure in between would
    /// leave the registry naming a document that was never written.
    ///
    /// So opening rolls back any checkpoint still outstanding before taking a new
    /// one. A store with nothing checkpointed ignores it, which is the ordinary
    /// case; the one that has something is the one that left it behind.
    pub(crate) fn change(&mut self) -> ChangeSet {
        self.graph.index_mut().rollback();
        self.graph.index_mut().checkpoint();
        ChangeSet::new()
    }

    /// [`load`](Self::load) a document, preferring what `cs` has already staged
    /// for it over what is on disk.
    ///
    /// For the op that edits the same document twice: the second edit has to see
    /// the first, and the first is in the set rather than on the filesystem.
    pub(crate) async fn load_staged(
        &self,
        cs: &ChangeSet,
        path: &Path,
    ) -> Result<(String, prov_graph::document::Document)> {
        let Some(bytes) = cs.staged(path) else {
            return self.load(path).await;
        };
        let text = String::from_utf8(bytes.to_vec())
            .map_err(|e| Error::Structure(format!("{} is not valid UTF-8: {e}", path.display())))?;
        let doc = prov_graph::document::Document::parse(path, &text)?;
        Ok((text, doc))
    }

    /// Land a staged [`ChangeSet`], together with the registry write when the op
    /// moved an ID — one unit, all of it or none of it.
    ///
    /// The registry is staged *last*, after the documents, for the same reason
    /// [`reparent`](crate::mutate) orders its writes the way it does: since the
    /// one failure this cannot rule out is a crash between ops, the window it
    /// leaves should be the diagnosable one. Documents-then-registry leaves at
    /// worst an ID resolving to a stale path, which `check` reports;
    /// registry-first would leave it resolving to a document that is not there.
    ///
    /// A dirty index with nowhere to persist (a workspace storing IDs in
    /// frontmatter only, or one whose registry document is not bootstrapped yet)
    /// stages nothing and stays dirty — the caller that knows the home writes it.
    /// It still *commits*, though: staging nothing is not failing, so its
    /// checkpoint is spent like anyone's. Conflating those two is how a
    /// successful op leaves a checkpoint behind for the next
    /// [`change`](Self::change) to mistake for a leak and unwind.
    pub(crate) async fn commit(&mut self, mut cs: ChangeSet) -> Result<()> {
        // Documents that earned an id this op write it down themselves, in the
        // same set as the registry entry — so the two homes for an id can never
        // disagree because of an interrupted write.
        if let Err(e) = self.stage_pending_stamps(&mut cs).await {
            self.pending_stamps.clear();
            self.graph.index_mut().rollback();
            return Err(e);
        }
        // The registry lives in a document, and the op may be moving or rewriting
        // that very document. Follow it before rendering — staged last, this write
        // would otherwise clobber the op's own edit to it.
        if let Err(e) = self.graph.index_mut().rebase(&cs) {
            self.graph.index_mut().rollback();
            return Err(e);
        }
        let staged_index = match self.graph.index_mut().pending_write() {
            Ok(Some((path, text))) => {
                cs.write(path, text);
                true
            }
            Ok(None) => false,
            Err(e) => {
                self.graph.index_mut().rollback();
                return Err(e);
            }
        };
        // Everything this set touches stops being something prov remembers —
        // before it lands, so a set that fails halfway leaves nothing behind
        // claiming to know what is on disk.
        self.forget_written(&cs);
        match cs.apply(self.graph.fs(), self.graph.root()).await {
            Ok(()) => {
                // Unconditional: the op succeeded, so its checkpoint is spent
                // either way. `staged_index` only says whether the store may now
                // call itself persisted — a store with no home stages nothing and
                // must stay dirty for whoever does write it, but its checkpoint is
                // just as finished as anyone's.
                self.graph.index_mut().committed(staged_index);
                Ok(())
            }
            Err(e) => {
                self.graph.index_mut().rollback();
                Err(e)
            }
        }
    }

    /// Drain [`pending_stamps`](Self::pending_stamps) into `cs`: for each
    /// document that earned an id, stage a copy of it carrying that id in its
    /// `id` field.
    ///
    /// Reads through the set rather than the filesystem
    /// ([`ChangeSet::staged`]), so a document this op is *already* rewriting is
    /// stamped on top of that edit instead of clobbering it. Three cases are
    /// deliberately skipped rather than guessed at:
    ///
    /// - a document the set **renames** — the stamp would land at a path the set
    ///   is emptying;
    /// - a document that is **not on disk** and not staged — nothing to edit
    ///   (`create` composes its new document's id inline, so this is the copy
    ///   that never needed a stamp, not a lost one);
    /// - a document that **already carries this exact id** — idempotent.
    ///
    /// A skip is never silent damage: the id is in the registry either way, and
    /// `check` raises [`Finding::UnstampedId`](crate::Finding::UnstampedId) for
    /// any document still missing its stamp.
    async fn stage_pending_stamps(&mut self, cs: &mut ChangeSet) -> Result<()> {
        for (path, id) in std::mem::take(&mut self.pending_stamps) {
            if cs.renamed_to(&path).is_some() {
                continue;
            }
            let text = match cs.staged(&path) {
                Some(bytes) => match std::str::from_utf8(bytes) {
                    Ok(text) => text.to_string(),
                    // An opaque payload (an attachment) is not a document and has
                    // no frontmatter to stamp.
                    Err(_) => continue,
                },
                None => match self.read_text(&path).await {
                    Ok(text) => text,
                    Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e),
                },
            };
            let doc = prov_graph::document::Document::parse(&path, &text)?;
            if doc.meta.get("id").and_then(Value::as_str) == Some(id.0.as_str()) {
                continue;
            }
            let updated = prov_store::edit::set_in_text(
                &text,
                doc.carrier,
                "id",
                fig::Value::Str(id.0.clone()),
            )?;
            cs.write(&path, updated);
        }
        Ok(())
    }

    /// Bootstrap a **linked sidecar**: create `sidecar` (a whole-file metadata
    /// document, seeded with `seed`, in `format`) beside the workspace and add
    /// `pointer` → `sidecar` to the root document's metadata — as one crash-safe
    /// [`ChangeSet`], so either both land or neither. Returns whether the sidecar
    /// was newly written (`false` when it already existed and only the pointer was
    /// (re-)added).
    ///
    /// This is the shared mechanic behind the registry and config documents the
    /// CLI declares on first use: a workspace resource is *reachable* precisely
    /// because the root points at it, so a sidecar written without the pointer —
    /// or a pointer added without the sidecar — is a torn half a scan can neither
    /// find nor trust. Bundling both into one change set is exactly why that torn
    /// state cannot occur. The seed and the pointer relation are the caller's
    /// policy (what the sidecar is *for*); the crash-safe two-file landing is the
    /// library's.
    pub async fn link_sidecar(
        &self,
        root_doc: &Path,
        pointer: &str,
        sidecar: &Path,
        seed: &prov_graph::meta::Mapping,
        format: fig::Format,
    ) -> Result<bool> {
        let mut cs = ChangeSet::new();
        let created = !self.exists(sidecar).await?;
        if created {
            cs.write(sidecar, prov_graph::meta::serialize_mapping(seed, format)?);
        }
        // The pointer value is the sidecar path as written (a bare filename when it
        // sits beside the root, which is the convention). Set it comment- and
        // format-preservingly, like any other metadata edit.
        let (text, doc) = self.load(root_doc).await?;
        let updated = prov_store::edit::set_in_text(
            &text,
            doc.carrier,
            pointer,
            prov_store::edit::infer_scalar(&sidecar.to_string_lossy()),
        )?;
        cs.write(root_doc, updated);
        cs.apply(self.graph.fs(), self.graph.root()).await?;
        Ok(created)
    }

    // TODO(port): scan/traverse from diaryx_core::workspace land here.
}

/// The read surface, forwarded to [`Graph`].
///
/// Every method here is one line. That is the point: `Workspace`'s traversal
/// *is* `prov-graph`'s traversal, not a second implementation that happens to
/// agree with it today. What the workspace adds is the two things the read core
/// deliberately does not know — where prov parks its own bytes (a history
/// store's interiors, the recycle bin), which the scoped walks must not index,
/// and the config layer those parked directories are declared in.
impl<FS: ReadStorage, Id, Ix: IdIndex> Workspace<FS, Id, Ix> {
    /// Read and split the document at a workspace-relative `path`, served from
    /// the read-scope memo when one is open.
    pub(crate) async fn load(&self, path: &Path) -> Result<(String, Document)> {
        self.graph.load(path).await
    }

    /// The parsed document at a workspace-relative `path`.
    pub async fn document(&self, path: impl AsRef<Path>) -> Result<Document> {
        self.graph.document(path).await
    }

    /// Whether `path` exists, unclamped and unmemoized.
    pub async fn exists(&self, path: &Path) -> Result<bool> {
        self.graph.exists(path).await
    }

    /// The raw bytes at `path` — for something that is not a document.
    pub async fn read_bytes(&self, path: &Path) -> Result<Vec<u8>> {
        self.graph.read_bytes(path).await
    }

    /// The raw text at `path` — for something that is not a document.
    pub async fn read_text(&self, path: &Path) -> Result<String> {
        self.graph.read_text(path).await
    }

    /// The entries of the directory at `path`.
    pub async fn listing(&self, path: &Path) -> Result<Vec<DirEntry>> {
        self.graph.listing(path).await
    }

    /// Metadata for the entry at `path`.
    pub async fn stat(&self, path: &Path) -> Result<Metadata> {
        self.graph.stat(path).await
    }

    /// Resolve `link`, written in the document at `doc`, to what it names.
    pub fn resolve_link(&self, doc: &Path, link: &Link) -> Target {
        self.graph.resolve_link(doc, link)
    }

    /// [`resolve_link`](Self::resolve_link), with a title index for nominal
    /// (`[[alias]]`) references.
    pub fn resolve_link_with(
        &self,
        doc: &Path,
        link: &Link,
        titles: Option<&TitleIndex>,
    ) -> Target {
        self.graph.resolve_link_with(doc, link, titles)
    }

    /// The census of every forward link reachable from `start`, with prov's own
    /// parked directories excluded from the nominal scan.
    pub async fn census(&self, start: impl AsRef<Path>) -> Result<Vec<CensusEntry>> {
        let start = start.as_ref();
        let parked = self.parked_dirs(start).await?;
        self.graph.census_within(start, &parked).await
    }

    /// The shared spanning-tree walk behind [`census`](Self::census) and the
    /// structural findings.
    pub(crate) async fn walk(&self, start: &Path) -> Result<Walk> {
        let parked = self.parked_dirs(start).await?;
        self.graph.walk(start, &parked).await
    }

    /// The backlink map for the workspace reachable from `start`: every resolved
    /// target to the inbound references that reach it. The census inverted, so
    /// it is always fresh — there is no stored index to drift.
    pub async fn backlinks(
        &self,
        start: impl AsRef<Path>,
    ) -> Result<BTreeMap<PathBuf, Vec<Backlink>>> {
        Ok(prov_graph::graph::invert(self.census(start).await?))
    }

    /// The inbound references to a single `target`, sorted by source.
    pub async fn backlinks_to(
        &self,
        start: impl AsRef<Path>,
        target: impl AsRef<Path>,
    ) -> Result<Vec<Backlink>> {
        Ok(prov_graph::graph::inbound(
            self.census(start).await?,
            target.as_ref(),
        ))
    }

    /// Every file the workspace reaches from `start` that is actually on disk.
    pub async fn reachable_files(&self, start: impl AsRef<Path>) -> Result<BTreeSet<PathBuf>> {
        let start = start.as_ref();
        let parked = self.parked_dirs(start).await?;
        self.graph.reachable_files_within(start, &parked).await
    }

    /// The documents among a walk's reachable set.
    pub async fn reachable_documents(
        &self,
        start: &Path,
        census: &[CensusEntry],
        content_bodies: &[PathBuf],
    ) -> Result<BTreeSet<PathBuf>> {
        self.graph
            .reachable_documents(start, census, content_bodies)
            .await
    }

    /// The materialized spanning tree rooted at `start`.
    pub async fn tree(&self, start: impl AsRef<Path>) -> Result<Node> {
        self.tree_with(start, TreeOptions::default()).await
    }

    /// [`tree`](Self::tree), with [`TreeOptions`].
    pub async fn tree_with(&self, start: impl AsRef<Path>, options: TreeOptions) -> Result<Node> {
        let start = start.as_ref();
        let parked = self.parked_dirs(start).await?;
        self.graph.tree_within(start, options, &parked).await
    }

    /// The full title index — every document under the root.
    pub async fn title_index(&self) -> Result<TitleIndex> {
        self.graph.title_index().await
    }

    /// The title index bounded to what the workspace reaches from `start`.
    pub async fn title_index_scoped(&self, start: &Path) -> Result<TitleIndex> {
        let parked = self.parked_dirs(start).await?;
        self.graph.title_index_scoped(start, &parked).await
    }

    /// Every `id` spelled in a document's own frontmatter, with its path.
    pub async fn scan_ids(&self) -> Result<Vec<(prov_graph::identity::Id, PathBuf)>> {
        self.graph.scan_ids().await
    }

    /// Every prose document under the root.
    pub async fn content_documents(&self) -> Result<Vec<PathBuf>> {
        self.graph.content_documents().await
    }

    /// The files directly inside `dirs` — the bounded listing the scans share.
    pub(crate) async fn direct_child_files(
        &self,
        dirs: &BTreeSet<PathBuf>,
    ) -> Result<Vec<PathBuf>> {
        self.graph.direct_child_files(dirs).await
    }

    /// The directories a reachable set occupies.
    pub(crate) fn reached_dirs(reachable: &BTreeSet<PathBuf>) -> BTreeSet<PathBuf> {
        Graph::<FS, Ix>::reached_dirs(reachable)
    }
}

/// exactly the layers requested — and none it does not.
#[derive(Debug, Clone)]
/// Builder for [`Workspace`]. Setting an identity policy or index store returns
/// a builder with a new type parameter, so the composed [`Workspace`] carries
pub struct WorkspaceBuilder<FS, Id, Ix> {
    fs: FS,
    root: PathBuf,
    identity: Id,
    index: Ix,
    settings: Settings,
    fixity_cache: Option<FixityCache>,
}

impl<FS, Id, Ix> WorkspaceBuilder<FS, Id, Ix> {
    /// Hash through a [`FixityCache`], so an operation that would otherwise read
    /// and hash every file in the workspace reads only the ones whose stat says
    /// they changed.
    ///
    /// Off by default: the cache is device-local state prov cannot locate for
    /// itself, so a workspace gets one only from a host that knows where it
    /// lives. Equivalent to [`Workspace::set_fixity_cache`] after the fact.
    pub fn fixity_cache(mut self, cache: FixityCache) -> Self {
        self.fixity_cache = Some(cache);
        self
    }
    /// Set the workspace root.
    pub fn root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = root.into();
        self
    }

    /// Set the relation vocabulary.
    pub fn relations(mut self, relations: RelationSet) -> Self {
        self.settings.relations = relations;
        self
    }

    /// Set the link style this workspace authors in (typically read from the
    /// root's `link_format`).
    pub fn link_style(mut self, link_style: LinkStyle) -> Self {
        self.settings.link_style = link_style;
        self
    }

    /// Author durable structural links by id (Obsidian-style) rather than paths.
    /// A convenience over [`reference_style`](Self::reference_style); effective
    /// only when identity registers on a link.
    pub fn id_links(mut self, id_links: bool) -> Self {
        self.settings.id_links = id_links;
        self
    }

    /// Set how far content checksums are recorded (attachments only by default).
    pub fn fixity(mut self, fixity: Fixity) -> Self {
        self.settings.fixity = fixity;
        self
    }

    /// Set whether this workspace keeps a history store. Off by default; see
    /// [`Workspace::history`] for what the library does with it (it does not gate
    /// capture — that is the caller's call).
    pub fn history(mut self, history: History) -> Self {
        self.settings.history = history;
        self
    }

    /// Set the metadata embedding family — the `(style, format)` half that
    /// resolves to a concrete carrier. Defaults to
    /// [`EmbedStyle::Delimited`], matching the config default.
    pub fn embed_style(mut self, embed_style: EmbedStyle) -> Self {
        self.settings.embed_style = embed_style;
        self
    }

    /// Set where a document's stable id is persisted (DESIGN §5). Under a
    /// frontmatter-stamping mode ([`IdStorage::stamps_frontmatter`]) every
    /// document prov authors carries its own `id`, so identity travels with the
    /// file and the registry becomes a rebuildable cache rather than the sole
    /// authority.
    pub fn id_storage(mut self, id_storage: IdStorage) -> Self {
        self.settings.id_storage = id_storage;
        self
    }

    /// Set what this workspace calls itself — the qualifier a cross-workspace
    /// reference (`id:<name>/<id>`) names it by. Empty (the default) leaves the
    /// workspace anonymous: it can hold foreign references, but a reference
    /// written *to* it can never be recognized here as local.
    pub fn workspace_id(mut self, name: impl Into<String>) -> Self {
        self.settings.workspace_id = name.into();
        self
    }

    /// Set the workspace-default reference style — the fallback for relations
    /// without their own override. Supersedes the `link_style`/`id_links`
    /// convenience inputs when set.
    pub fn reference_style(mut self, style: ReferenceStyle) -> Self {
        self.settings.reference_style = Some(style);
        self
    }

    /// Set the metadata format new documents get when they inherit no parent
    /// block (a default, not a constraint).
    pub fn default_embed_format(mut self, format: fig::Format) -> Self {
        self.settings.default_embed_format = format;
        self
    }

    /// Set every policy knob at once, from a [`Settings`] built elsewhere —
    /// typically `Settings::from(&config)`, which is the whole of what a
    /// workspace's config document declares.
    ///
    /// Supersedes anything the individual setters put there, so call it first if
    /// you mean to override one knob afterwards.
    pub fn settings(mut self, settings: Settings) -> Self {
        self.settings = settings;
        self
    }

    /// Attach an identity policy, turning identity on.
    pub fn identity<Id2>(self, identity: Id2) -> WorkspaceBuilder<FS, Id2, Ix> {
        WorkspaceBuilder {
            fs: self.fs,
            root: self.root,
            identity,
            index: self.index,
            settings: self.settings,
            fixity_cache: self.fixity_cache,
        }
    }

    /// Attach an index store (where IDs are persisted).
    pub fn index<Ix2>(self, index: Ix2) -> WorkspaceBuilder<FS, Id, Ix2> {
        WorkspaceBuilder {
            fs: self.fs,
            root: self.root,
            identity: self.identity,
            index,
            settings: self.settings,
            fixity_cache: self.fixity_cache,
        }
    }

    /// Finish building.
    ///
    /// This is where the ten authoring settings are split: the three the read
    /// core needs are copied into the [`Graph`]'s [`ReadSettings`], and the
    /// whole set is kept alongside for the verbs. Nothing mutates either
    /// afterwards, so the two copies of those three cannot drift.
    pub fn build(self) -> Workspace<FS, Id, Ix> {
        let read = ReadSettings {
            relations: self.settings.relations.clone(),
            workspace_id: self.settings.workspace_id.clone(),
            id_storage: self.settings.id_storage,
        };
        Workspace {
            graph: Graph::new(self.fs, self.root, self.index, read),
            identity: self.identity,
            settings: self.settings,
            pending_stamps: Vec::new(),
            fixity_cache: Mutex::new(self.fixity_cache),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{IdentityPolicy, Minter};
    use prov_store::index::InMemoryIndex;

    // A stand-in filesystem — the seam is exercised without a real backend.
    #[derive(Clone)]
    struct DummyFs;

    #[test]
    fn paths_only_by_default() {
        let ws = Workspace::builder(DummyFs).root("vault").build();
        assert_eq!(ws.root(), Path::new("vault"));
        assert_eq!(ws.relations().spanning_relation(), Some("contents"));
        // Identity off: the default policy fires no triggers.
        assert!(!ws.identity().registration().is_active());
    }

    #[test]
    fn fs_path_joins_a_workspace_relative_path_onto_the_root() {
        let ws = Workspace::builder(DummyFs).root("vault").build();
        assert_eq!(
            ws.fs_path(Path::new("notes/a.md")),
            Path::new("vault/notes/a.md")
        );
    }

    #[test]
    fn identity_opts_in_via_one_builder_line() {
        let ws = Workspace::builder(DummyFs)
            .root("vault")
            .identity(Minter::lazy(1))
            .index(InMemoryIndex::new())
            .build();
        assert!(ws.identity().registration().on_link);
        assert!(ws.index().is_empty());
    }

    /// Every knob, set away from its default, then carried across *both*
    /// type-parameter flips and out the other side.
    ///
    /// This is the property the [`Settings`] struct exists to make true by
    /// construction. It used to be four hand-copied field lists — `identity`,
    /// `index`, `build`, and `Clone`. An *omitted* field was always a compile
    /// error, so that was never the risk; the risk was a field written
    /// `workspace_id: String::new()` where it meant `self.workspace_id`, which
    /// type-checks, and which would revert one knob to its default for exactly
    /// the workspaces that called that one method. Nothing here can fail while
    /// the settings move whole — that is the point — so this test's real job is
    /// the day someone unpacks them again.
    #[test]
    fn every_setting_survives_the_builder_type_flips() {
        let settings = Settings {
            relations: RelationSet::diaryx(),
            link_style: LinkStyle::PlainRelative,
            id_links: true,
            reference_style: None,
            default_embed_format: fig::Format::Json,
            embed_style: EmbedStyle::CodeBlock,
            fixity: Fixity::Off,
            history: History::Manual,
            id_storage: IdStorage::Frontmatter,
            workspace_id: "notes".into(),
        };
        let ws = Workspace::builder(DummyFs)
            .root("vault")
            .settings(settings)
            // The two flips: each rebuilds the builder at a new type.
            .identity(Minter::lazy(1))
            .index(InMemoryIndex::new())
            .build();

        assert_eq!(ws.link_style(), LinkStyle::PlainRelative);
        assert_eq!(ws.default_embed_format(), fig::Format::Json);
        assert_eq!(ws.embed_style(), EmbedStyle::CodeBlock);
        assert_eq!(ws.fixity(), Fixity::Off);
        assert_eq!(ws.history(), History::Manual);
        assert_eq!(ws.id_storage(), IdStorage::Frontmatter);
        assert_eq!(ws.workspace_id(), "notes");
        assert_eq!(ws.relations().spanning_relation(), Some("contents"));
        // `id_links` has no field of its own on the far side — it is read back
        // through the reference style it feeds, which is the whole of what it
        // means.
        assert!(ws.id_links());
        assert_eq!(ws.reference_style().addressing, Addressing::Id);

        // And a clone is the same workspace, by the same mechanism.
        let copy = ws.clone();
        assert_eq!(copy.id_storage(), IdStorage::Frontmatter);
        assert_eq!(copy.workspace_id(), "notes");
    }

    /// A config document declares the workspace's policy, and all of it arrives.
    /// The CLI's whole workspace construction is this conversion plus a root, an
    /// identity policy, and an index.
    #[test]
    fn a_config_becomes_the_workspaces_settings() {
        let config = crate::config::WorkspaceConfig {
            id_storage: IdStorage::Frontmatter,
            history: History::Manual,
            fixity: Fixity::Off,
            embed_style: EmbedStyle::CodeBlock,
            default_embed_format: fig::Format::Json,
            workspace_id: "notes".into(),
            ..Default::default()
        };

        let ws = Workspace::builder(DummyFs)
            .root("vault")
            .settings(Settings::from(&config))
            .build();

        assert_eq!(ws.id_storage(), IdStorage::Frontmatter);
        assert_eq!(ws.history(), History::Manual);
        assert_eq!(ws.fixity(), Fixity::Off);
        assert_eq!(ws.embed_style(), EmbedStyle::CodeBlock);
        assert_eq!(ws.default_embed_format(), fig::Format::Json);
        assert_eq!(ws.workspace_id(), "notes");
        // A config always yields an explicit reference style, which is why the
        // legacy `id_links` axis stays at its default and is never consulted.
        assert_eq!(ws.reference_style(), config.reference_style());
    }
}
