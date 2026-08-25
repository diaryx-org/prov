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

use prov_graph::document::{Body, Document};
use prov_graph::fs::{DirEntry, Metadata};
use prov_graph::graph::{Backlink, CensusEntry, Graph, Node, ReadSettings, TreeOptions, Walk};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::change::{ChangeSet, FileOp};
use crate::config::{Fixity, IdStorage};
use crate::identity::{IdentityPolicy, NoIdentity, Trigger};
use prov_graph::document::EmbedStyle;
use prov_graph::error::{Error, Result};
use prov_graph::fs::ReadStorage;
use prov_graph::graph::Target;
use prov_graph::index::{Collision, IdIndex, NoIndex};
use prov_graph::link::{self, Addressing, Link, LinkStyle, ReferenceStyle, Wrapper};
use prov_graph::memo::ReadScope;
use prov_graph::meta::Value;
use prov_graph::relation::RelationSet;
use prov_graph::title::TitleIndex;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

mod ignore;

pub use ignore::{Ignore, IgnoreList, Reason};

/// A byte-parking store's directory — the parent of the index document that
/// names it. The recycle bin's `items/` hangs off its index this way, and the
/// retired history store's archive did too.
fn store_dir(store_index: &Path) -> PathBuf {
    store_index.parent().unwrap_or(Path::new("")).to_path_buf()
}

/// The workspace's **policy knobs**, as one value.
///
/// Every field here answers "how does this workspace author and read
/// documents?" — the vocabulary, the reference style, where ids live, how far
/// checksums go. What is *not* here is as deliberate: the filesystem and root
/// are a location rather than a policy; and the identity policy and index store
/// are type parameters, because [`Workspace`]'s whole "identity is a bolt-on"
/// design is that they can be compiled out.
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
}

/// Hand-written rather than derived, because the read memo carries its own
/// answer to "what does a second handle on this workspace inherit?".
///
/// The **read memo** starts empty, with no scope open — a requirement, not a
/// preference. A [`ReadScope`] guard points at the memo it opened, and a clone
/// has no guard pointing at it; inheriting a nonzero depth would leave the copy
/// permanently scoped, remembering reads with nothing left to close it.
impl<FS: Clone, Id: Clone, Ix: Clone> Clone for Workspace<FS, Id, Ix> {
    fn clone(&self) -> Self {
        Self {
            graph: self.graph.clone(),
            identity: self.identity.clone(),
            settings: self.settings.clone(),
            pending_stamps: self.pending_stamps.clone(),
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
        }
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
    pub fn read_scope(&self) -> ReadScope {
        self.graph.read_scope()
    }

    /// Forget everything `cs` is about to change in the operation's read memo.
    ///
    /// Called before the set lands rather than after, because forgetting is
    /// never the wrong answer: a set that then fails and rolls back has cost one
    /// re-read, where the other order would have left a memo describing bytes
    /// that no longer exist.
    fn forget_written(&self, cs: &ChangeSet) {
        let mut memo = self.graph.memo_lock();
        let mut forget = |path: &Path| {
            memo.forget(path);
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

    /// The retired event store's index document, if this root still declares
    /// one via the history pointer relation (§6). `None` when the vocabulary
    /// has no history relation or the root declares none — which every
    /// migrated workspace is: the pointer survives only so an unmigrated
    /// store stays parked ([`parked_dirs`](Self::parked_dirs)) and the about
    /// page can still name it.
    pub async fn history_path(&self, root_doc: &Path) -> Result<Option<PathBuf>> {
        match self.relations().history_relation() {
            Some(relation) => self.pointer_target(root_doc, relation).await,
            None => Ok(None),
        }
    }

    /// The directories a **byte-parking store** keeps its contents under — the
    /// recycle bin's `items/`, and a retired event store's archive.
    ///
    /// These are machinery, not the workspace's documents, and every walk is
    /// blind to them by decision. The line matters most for **names**: a
    /// binned document keeps the title it had, so a workspace that indexed
    /// these subtrees would resolve `[[Some Note]]` to a copy of a note the
    /// author deleted — silently, since it is nowhere the reader can see.
    /// Worse than a dead link, which at least reads as broken.
    ///
    /// Naming the directories rather than filtering paths afterwards is what
    /// keeps the *cost* out too: a scan that never descends does not read a
    /// thousand revision documents in order to discard them.
    ///
    /// What is *not* here is another tool's store sitting beside the root — a
    /// version-control folder, a sync tool's cache. prov has no way to be told
    /// about one, so every walk sees it as ordinary content the graph fails to
    /// reach: [`ignore_list`](Self::ignore_list) reports it, which is useful,
    /// and `check` reports its interior, which is noise. Scoping a walk to
    /// something the workspace declares is its own decision, unmade.
    pub(crate) async fn parked_dirs(&self, root_doc: &Path) -> Result<Vec<PathBuf>> {
        let mut dirs = Vec::new();
        // A retired prov event store the root still points at. The `history`
        // pointer and this parking survive the store's retirement so an
        // unmigrated workspace keeps its scans out of the event archive; both
        // go when nothing declares such a store any more.
        if let Some(index) = self.history_path(root_doc).await? {
            dirs.push(store_dir(&index).join("events"));
            dirs.push(store_dir(&index).join("blobs"));
        }
        if let Some(index) = self.recycle_bin_path(root_doc).await? {
            dirs.push(store_dir(&index).join("items"));
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
        match self.apply_set(&cs).await {
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

    /// Land `cs` against this workspace's tree, all-or-nothing, through prov's
    /// write-ahead journal.
    ///
    /// **Use this rather than [`ChangeSet::apply`] for anything that mutates a
    /// workspace.** `ChangeSet::apply` journals under `fs-transaction`'s own
    /// default name, which prov's recovery — [`crate::journal::recover`], the
    /// one `prov check` runs — does not look for. A crash mid-apply would then
    /// leave a journal nothing ever reads, stranding the change half-applied
    /// with no record of how to finish it. Routing every workspace write
    /// through here is what keeps the two ends naming the same file.
    pub async fn apply_set(&self, cs: &ChangeSet) -> Result<()> {
        Ok(crate::journal::workspace_journal()
            .apply(cs, self.fs(), self.root())
            .await?)
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
        self.apply_set(&cs).await?;
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

    /// The prose body of the document at `path`, wherever it physically lives —
    /// its own file when combined, its `content` target when separated. See
    /// [`Graph::body`].
    pub async fn body(&self, path: impl AsRef<Path>) -> Result<Body> {
        self.graph.body(path).await
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
    /// Scoped, like the two below it: locating the parked directories reads the
    /// root once per pointer it follows, and the pass that follows reads it
    /// again as the first node it visits. Three reads of the root document for
    /// one census, before the walk's own scope has a say.
    pub async fn census(&self, start: impl AsRef<Path>) -> Result<Vec<CensusEntry>> {
        let _scope = self.read_scope();
        let start = start.as_ref();
        let parked = self.parked_dirs(start).await?;
        self.graph.census_within(start, &parked).await
    }

    /// The documents the workspace reaches from `start` — the population
    /// [`check`](Self::check) validates, over a walk this performs itself.
    ///
    /// The convenience form of
    /// [`reachable_documents`](Self::reachable_documents), which takes a census
    /// and a content-body list a caller outside this crate has no way to
    /// assemble ([`walk`](Self::walk) is internal). Callers that want "every
    /// document, the way check counts them" would otherwise reach for
    /// [`reachable_files`](Self::reachable_files) — which is a *file* set, and
    /// so includes the shadowed payloads (`attach --opaque`) that this
    /// deliberately leaves out. Those are bytes prov is holding without
    /// interpreting: any `content_hash` inside one belongs to the exhibit, not
    /// to this workspace, and a sweep that parsed them would rewrite it.
    pub async fn reachable_documents_from(
        &self,
        start: impl AsRef<Path>,
    ) -> Result<BTreeSet<PathBuf>> {
        let start = start.as_ref();
        let walk = self.walk(start).await?;
        self.reachable_documents(start, &walk.census, &walk.content_bodies)
            .await
    }

    /// The shared spanning-tree walk behind [`census`](Self::census) and the
    /// structural findings. Scoped for the reason [`census`](Self::census) is.
    pub(crate) async fn walk(&self, start: &Path) -> Result<Walk> {
        let _scope = self.read_scope();
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
    /// Scoped for the reason [`census`](Self::census) is.
    pub async fn reachable_files(&self, start: impl AsRef<Path>) -> Result<BTreeSet<PathBuf>> {
        let _scope = self.read_scope();
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

    /// The documents `parent` directly contains: its spanning children,
    /// resolved and loaded, in declaration order.
    ///
    /// **The bounded counterpart to [`tree`](Self::tree)** — one generation, one
    /// read each, and never a walk. `tree` materializes everything reachable,
    /// which is what a caller rendering a sidebar or auditing a subtree wants;
    /// a caller looking for *one* child among a node's children wants this, and
    /// reaching for `tree` to get it pays the whole subtree to read one
    /// generation of it. That cost is invisible on a local disk and dominant on
    /// a synced or remote [`Storage`], where each read is a round trip — which
    /// is exactly why [`plan_route`](Self::plan_route) descends a segment at a
    /// time rather than walking.
    ///
    /// Each child arrives **with its parsed document**, because the read that
    /// resolves a child is the same read that answers whatever was being asked
    /// about it. Handing back paths alone would make every caller read the
    /// generation a second time, which is the cost this exists to remove.
    ///
    /// # What is left out
    ///
    /// A child that resolves off-workspace (an external URL), to an id nothing
    /// registers, or to an ambiguous alias, and one whose document will not load
    /// or parse, is **omitted**. This is the one real difference from
    /// [`tree`](Self::tree), which marks each of those as a
    /// [`NodeKind`](prov_graph::graph::NodeKind) so a walk can report it: a
    /// caller diagnosing a workspace wants that and should use `tree` or
    /// [`check`](Self::check); a caller resolving a name through the tree wants
    /// a broken sibling to be a reason to keep looking, not a reason to fail.
    ///
    /// Nothing is skipped for being *parked* (a history store's interior, the
    /// recycle bin). Those bound a walk because a walk would descend into them;
    /// one generation of declared children cannot wander in, and a document that
    /// genuinely declares a parked path as its child is stating something the
    /// caller asked to be told.
    pub async fn spanning_children(
        &self,
        parent: impl AsRef<Path>,
    ) -> Result<Vec<(PathBuf, Document)>> {
        let parent = parent.as_ref();
        let (_, doc) = self.load(parent).await?;
        let mut out = Vec::new();
        for raw in self.relations().children(&fig::Value::from(&doc.meta)) {
            let Target::Path(path) = self.resolve_link(parent, &Link::parse(&raw)) else {
                continue;
            };
            let Ok((_, child)) = self.load(&path).await else {
                continue;
            };
            out.push((path, child));
        }
        Ok(out)
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
}

impl<FS, Id, Ix> WorkspaceBuilder<FS, Id, Ix> {
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
        assert_eq!(ws.fixity(), Fixity::Off);
        assert_eq!(ws.embed_style(), EmbedStyle::CodeBlock);
        assert_eq!(ws.default_embed_format(), fig::Format::Json);
        assert_eq!(ws.workspace_id(), "notes");
        // A config always yields an explicit reference style, which is why the
        // legacy `id_links` axis stays at its default and is never consulted.
        assert_eq!(ws.reference_style(), config.reference_style());
    }
}

/// [`Workspace::spanning_children`] over a real filesystem — separate from the
/// builder tests above, which need no disk, and read-counting because the whole
/// claim is about what is *not* read.
#[cfg(test)]
mod spanning_children_tests {
    use super::*;
    use crate::fs_faults::CountingFs;
    use prov_graph::exec::block_on;

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prov-children-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, rel: &str, text: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// **One generation, and the documents that resolved it.**
    ///
    /// Declaration order is kept — a caller matching children against an
    /// ordered vocabulary, or reporting the first match, is entitled to the
    /// order the parent wrote. The grandchild is the point of the read count:
    /// `tree` would have materialized it to answer this, and on a synced
    /// backend that read is a round trip nobody asked for.
    #[test]
    fn reads_one_generation_and_hands_back_what_it_read() {
        let dir = tempdir("bounded");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- a.md\n- b.md\n- deep/index.md\n---\n",
        );
        write(&dir, "a.md", "---\ntitle: A\npart_of: index.md\n---\n");
        write(&dir, "b.md", "---\ntitle: B\npart_of: index.md\n---\n");
        write(
            &dir,
            "deep/index.md",
            "---\ntitle: Deep\npart_of: /index.md\ncontents:\n- child.md\n---\n",
        );
        write(
            &dir,
            "deep/child.md",
            "---\ntitle: Grandchild\npart_of: /deep/index.md\n---\n",
        );

        let fs = CountingFs::default();
        let ws = Workspace::builder(fs.clone()).root(&dir).build();
        let children = block_on(ws.spanning_children("index.md")).expect("children");

        let named: Vec<(String, String)> = children
            .iter()
            .map(|(path, doc)| {
                (
                    path.display().to_string(),
                    doc.meta
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                )
            })
            .collect();
        assert_eq!(
            named,
            vec![
                ("a.md".to_string(), "A".to_string()),
                ("b.md".to_string(), "B".to_string()),
                ("deep/index.md".to_string(), "Deep".to_string()),
            ],
            "the parent's declared children, in the order it declared them, each parsed"
        );

        assert_eq!(
            fs.doc_reads(&dir, "deep/child.md"),
            0,
            "a generation below the one asked for was read"
        );
        for rel in ["index.md", "a.md", "b.md", "deep/index.md"] {
            assert_eq!(fs.doc_reads(&dir, rel), 1, "{rel} was read more than once");
        }
    }

    /// A broken sibling is a reason to keep looking, never a reason to fail —
    /// the resilience the route walk had, now where every caller inherits it.
    /// `check` is what reports these; a caller resolving a name through the tree
    /// wants the children it *can* see.
    #[test]
    fn a_child_that_cannot_be_resolved_or_read_is_left_out_rather_than_raised() {
        let dir = tempdir("broken");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- gone.md\n- '[Elsewhere](https://example.com/)'\n- ok.md\n---\n",
        );
        write(&dir, "ok.md", "---\ntitle: OK\npart_of: index.md\n---\n");

        let ws = Workspace::builder(CountingFs::default()).root(&dir).build();
        let children =
            block_on(ws.spanning_children("index.md")).expect("a broken sibling is not an error");

        assert_eq!(children.len(), 1, "{children:?}");
        assert_eq!(children[0].0, Path::new("ok.md"));
    }

    /// A leaf has no children and costs one read to say so. Not a special case
    /// in the implementation — worth a test because it is the answer every
    /// descent terminates on.
    #[test]
    fn a_node_declaring_no_containment_has_no_children() {
        let dir = tempdir("leaf");
        write(&dir, "index.md", "---\ntitle: Home\n---\n");

        let fs = CountingFs::default();
        let ws = Workspace::builder(fs.clone()).root(&dir).build();
        assert!(
            block_on(ws.spanning_children("index.md"))
                .expect("children")
                .is_empty()
        );
        assert_eq!(fs.doc_reads(&dir, "index.md"), 1);
    }
}
