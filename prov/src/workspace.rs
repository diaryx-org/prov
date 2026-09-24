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

use std::sync::Arc;

use prov_graph::bulk::{BulkReads, Hook};
use prov_graph::document::{Body, Document};
use prov_graph::field::FieldPath;
use prov_graph::fs::{DirEntry, Metadata};
use prov_graph::graph::{
    Backlink, CensusEntry, FrontmatterLink, Graph, Node, ReadSettings, TreeOptions, Walk,
};
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

mod fields;
mod ignore;
pub(crate) mod inbound;

pub use fields::{FieldScopes, Unresolved};
pub use ignore::{Ignore, IgnoreList, Reason};

/// A byte-parking store's directory — the parent of the index document that
/// names it. A retired recycle bin's `items/` hangs off its index this way, and
/// the retired history store's archive did too.
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
    /// The path-valued fields — see [`Workspace::references`].
    pub references: Vec<FieldPath>,
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
    /// Whether a delete records what it destroyed in the deletion log — see
    /// [`Workspace::record_deletions`].
    pub record_deletions: bool,
    /// Where a document's stable id is persisted — see
    /// [`Workspace::id_storage`].
    pub id_storage: IdStorage,
    /// What this workspace calls itself — the qualifier a cross-workspace
    /// reference names it by. Empty means anonymous, so no `id:<ws>/<id>`
    /// reference can ever be recognized as pointing back here. See
    /// [`WorkspaceConfig::workspace_id`](crate::config::WorkspaceConfig::workspace_id).
    pub workspace_id: String,
    /// The directories the workspace declares are not its content — see
    /// [`parked_dirs`](Workspace::parked_dirs) and
    /// [`WorkspaceConfig::out_of_scope`](crate::config::WorkspaceConfig::out_of_scope).
    /// Workspace-relative, `/`-separated. Empty declares nothing.
    pub out_of_scope: Vec<PathBuf>,
    /// The document the workspace node names as the root, if it names one — see
    /// [`WorkspaceConfig::root`](crate::config::WorkspaceConfig::root) and
    /// [`root_document`](Workspace::root_document). A bare file name in the root
    /// directory. `None` means the root is chosen by the candidate scan.
    pub root: Option<PathBuf>,
    /// The frontmatter field prov stamps when a document's content changes —
    /// see [`Workspace::updated_field`]. Empty means the workspace keeps no
    /// such field.
    pub updated: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            relations: RelationSet::diaryx(),
            references: Vec::new(),
            link_style: LinkStyle::default(),
            id_links: false,
            reference_style: None,
            default_embed_format: fig::Format::Yaml,
            embed_style: EmbedStyle::Delimited,
            fixity: Fixity::On,
            record_deletions: true,
            id_storage: IdStorage::Registry,
            workspace_id: String::new(),
            out_of_scope: Vec::new(),
            root: None,
            updated: String::new(),
        }
    }
}

/// The settings a [`WorkspaceConfig`] declares — the whole of what a workspace
/// says about itself in its own config document, in the form the builder takes.
///
/// This is the conversion the CLI used to spell out as ten builder calls. It
/// lives here rather than in [`config`](crate::config) because `workspace`
/// already depends on `config` (for [`Fixity`], [`IdStorage`]) and
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
            references: config.reference_fields(),
            link_style: config.link_format(),
            reference_style: Some(config.reference_style()),
            default_embed_format: config.default_embed_format,
            embed_style: config.embed_style,
            fixity: config.fixity,
            record_deletions: config.record_deletions,
            id_storage: config.id_storage,
            workspace_id: config.workspace_id.clone(),
            out_of_scope: config.out_of_scope.iter().map(PathBuf::from).collect(),
            root: config.root.as_deref().map(PathBuf::from),
            updated: config.updated.clone(),
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
    /// The inverse of the link graph, kept from one verb to the next so that
    /// a retitle or a rename asks "who links here?" of a lookup rather than a
    /// census. Empty until a verb first asks; stat-validated on every ask;
    /// updated or dropped by every set that lands. See [`inbound`].
    ///
    /// Interior mutability for the same reason the read memo has it: the
    /// ask is made from `&self`, and `apply_set` — which every write passes
    /// through — takes `&self` too.
    inbound: std::sync::Mutex<Option<inbound::InboundIndex>>,
    /// The directory this workspace's write-ahead journal is kept in, when it
    /// is not the root — see [`set_journal_home`](Workspace::set_journal_home).
    /// Held as given and checked at use ([`journal`](Workspace::journal)),
    /// because [`WorkspaceBuilder::build`] has no way to refuse.
    journal_home: Option<PathBuf>,
}

/// Hand-written rather than derived, because the read memo carries its own
/// answer to "what does a second handle on this workspace inherit?".
///
/// The **read memo** starts empty, with no scope open — a requirement, not a
/// preference. A [`ReadScope`] guard points at the memo it opened, and a clone
/// has no guard pointing at it; inheriting a nonzero depth would leave the copy
/// permanently scoped, remembering reads with nothing left to close it.
///
/// The **inbound index** starts empty for the same family of reason: what one
/// handle has remembered, the other would have to be told about on every
/// write, and a clone that begins with nothing simply censuses once.
///
/// The **journal home** is carried over. It is not a memory but a location,
/// and a second handle that journaled into the tree while the first kept its
/// journal elsewhere would be two writers disagreeing about where the crash
/// state lives — the one disagreement recovery cannot survive.
impl<FS: Clone, Id: Clone, Ix: Clone> Clone for Workspace<FS, Id, Ix> {
    fn clone(&self) -> Self {
        Self {
            graph: self.graph.clone(),
            identity: self.identity.clone(),
            settings: self.settings.clone(),
            pending_stamps: self.pending_stamps.clone(),
            inbound: inbound::empty(),
            journal_home: self.journal_home.clone(),
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
            bulk: None,
            journal_home: None,
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

    /// Tell the read core who to notify when a whole-tree read starts and
    /// ends — [`Graph::set_bulk_reads`], for a workspace already built. A
    /// backend priced per call installs itself here so that the census a
    /// `rename` or `retitle` runs costs it one scope rather than one round
    /// trip per document; see [`prov_graph::bulk`].
    pub fn set_bulk_reads(&mut self, hook: Arc<dyn BulkReads>) {
        self.graph.set_bulk_reads(hook);
    }

    /// The hook [`set_bulk_reads`](Self::set_bulk_reads) installed, if any —
    /// so a consumer that rebuilds its workspace can carry it over.
    pub fn bulk_reads(&self) -> Option<Arc<dyn BulkReads>> {
        self.graph.bulk_reads()
    }

    /// Keep this workspace's write-ahead journal in `home` instead of in the
    /// tree it applies to — or, given `None`, back in the root, which is the
    /// default.
    ///
    /// The default is right for a tree only this machine writes. It is wrong
    /// for a tree something **syncs**. The journal is one machine's crash
    /// state, and a sync service cannot tell it from content: it carries the
    /// file to machines that never crashed, where a recovery would replay
    /// *another* machine's intent against a tree that may have moved on, and
    /// where every apply would refuse the "stale" journal no local change
    /// left behind. Even with no crash at all, every change set of more than
    /// one op writes and then deletes a file in the root, and a sync service
    /// sees and ships both. With a home, the tree never holds a journal —
    /// not after a crash, and not for the length of an apply: a mutation
    /// writes the documents it changes and nothing else.
    ///
    /// `home` is a directory the caller owns and nothing syncs — an
    /// application-support or cache directory. The journal keeps its name,
    /// [`JOURNAL_NAME`](crate::journal::JOURNAL_NAME), inside it, and an
    /// apply makes the directory if it does not exist yet. Two obligations
    /// come with it, both the caller's:
    ///
    /// - **`home` must be absolute.** A relative one would resolve against the
    ///   process's current directory, which an apply and a later recovery have
    ///   no reason to share. It is not refused here — the builder's
    ///   [`journal_home`](WorkspaceBuilder::journal_home) cannot refuse, and
    ///   the two take it on the same terms — but every write is: the
    ///   workspace's [`journal`](Self::journal) is an error, so
    ///   [`apply_set`](Self::apply_set) fails before it writes anything, the
    ///   journal included. Call `journal()` once after setting a home to find
    ///   out at open rather than at the first save.
    /// - **One home, one root.** Nothing records which tree a journal was
    ///   for, so two workspaces sharing a home would each see the other's
    ///   interrupted change as a stale journal of their own — and recover it
    ///   into the wrong tree. Give each root its own directory.
    ///
    /// Setting a home moves where *this* workspace journals from now on; it
    /// does not move a journal already on disk. At open, recover the homed
    /// journal with [`recover_journal`](Self::recover_journal) (or
    /// [`recover_kept_in`](crate::journal::recover_kept_in)); whether also to
    /// finish a legacy journal left in the tree by an earlier, in-tree
    /// configuration is the caller's decision — see `recover_kept_in` for
    /// what turns on it.
    pub fn set_journal_home(&mut self, home: Option<PathBuf>) {
        self.journal_home = home;
    }

    /// The directory [`set_journal_home`](Self::set_journal_home) or
    /// [`WorkspaceBuilder::journal_home`] gave this workspace's journal, if
    /// any — so a consumer that rebuilds its workspace can carry it over.
    /// `None` means the journal lives in the root.
    pub fn journal_home(&self) -> Option<&Path> {
        self.journal_home.as_deref()
    }

    /// The write-ahead journal this workspace's writes go through: prov's
    /// [`workspace_journal`](crate::journal::workspace_journal), [kept
    /// in](crate::journal::Journal::kept_in) the
    /// [journal home](Self::set_journal_home) when there is one.
    ///
    /// Every write [`apply_set`](Self::apply_set) makes and every
    /// [`recover_journal`](Self::recover_journal) reads through this one
    /// value, which is what keeps the two naming the same file. A caller that
    /// lands a [`ChangeSet`] some other way — a bootstrap before the
    /// workspace exists — should apply it through this too.
    ///
    /// An error when the home is relative, which is the one way a home can be
    /// wrong that prov can see; nothing has been written when it says so.
    pub fn journal(&self) -> Result<crate::journal::Journal> {
        let journal = crate::journal::workspace_journal();
        Ok(match &self.journal_home {
            Some(home) => journal.kept_in(home.clone())?,
            None => journal,
        })
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

    /// The path-valued fields — the `fields` declarations of `type: ref`,
    /// each a path into the metadata whose every value is a link (spec §3).
    /// Read beside the relations wherever they are read.
    pub fn references(&self) -> &[FieldPath] {
        &self.settings.references
    }

    /// Every frontmatter link site in `meta` — relation entries and
    /// path-valued field values alike. See [`Graph::frontmatter_links`].
    pub fn frontmatter_links(&self, meta: &fig::Value) -> Vec<FrontmatterLink> {
        self.graph.frontmatter_links(meta)
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
                | FileOp::CopyFrom { path, .. }
                | FileOp::SetExecutable { path, .. }
                | FileOp::SetLink { path, .. } => forget(path),
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

    /// Whether [`delete`](Self::delete) records what it destroyed in the
    /// workspace's deletion log (`record_deletions`, on by default).
    ///
    /// The delete is a hard delete either way — prov does not keep the bytes.
    /// What the record adds is what
    /// [`restore`](Self::restore) repairs the graph from once they are back.
    pub fn record_deletions(&self) -> bool {
        self.settings.record_deletions
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

    /// The root **document** the workspace node names, if it names one.
    ///
    /// Distinct from [`root`](Self::root), which is the root *directory*. `None`
    /// means the node named none, or there is no node — the root is then the
    /// candidate scan's answer, which is every workspace that has never needed
    /// otherwise. See [`root_document`](Self::root_document), which prefers this
    /// and falls back to the scan.
    pub fn named_root(&self) -> Option<&Path> {
        self.settings.root.as_deref()
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

    /// The frontmatter field this workspace's own edits stamp with the instant
    /// of a content change (`updated: modified` in config), or `None` when it
    /// keeps no such field.
    ///
    /// The library never *writes* it on its own initiative — the caller decides
    /// an edit happened and supplies the instant
    /// ([`record_content_update`](Self::record_content_update)) — but it does
    /// *read* it: a confirmation ([`confirm`](Self::confirm)) is stale once this
    /// field says the document changed after it, and `check` reports that.
    pub fn updated_field(&self) -> Option<&str> {
        (!self.settings.updated.is_empty()).then_some(self.settings.updated.as_str())
    }

    /// The directories this workspace declares are not its content — another
    /// tool's store, a sync cache, a vendored checkout. Workspace-relative.
    ///
    /// Every walk is bounded by these ([`parked_dirs`](Self::parked_dirs)) and
    /// [`ignore_list`](Self::ignore_list) rules each one whole. Empty for a
    /// workspace that declares nothing, which is most of them.
    pub fn out_of_scope(&self) -> &[PathBuf] {
        &self.settings.out_of_scope
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
    /// else: a deletion record re-registering a document's old id, and a
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

    /// The **deletion log** this root declares via the deletions pointer
    /// relation (§6, the same reachability move as the registry). `None` when
    /// the vocabulary has no deletions relation or the root declares none — the
    /// workspace has no log yet, so the first recorded delete bootstraps one.
    ///
    /// A root written before the rename reaches its log through the legacy
    /// `recycle_bin` spelling, which resolves here too; see
    /// [`deletions_pointer`](Self::deletions_pointer) for which one answered.
    pub async fn deletions_path(&self, root_doc: &Path) -> Result<Option<PathBuf>> {
        Ok(self
            .deletions_pointer(root_doc)
            .await?
            .map(|(path, _)| path))
    }

    /// [`deletions_path`](Self::deletions_path), and the relation that found it.
    ///
    /// The two spellings are tried in order — `deletions` first, then the legacy
    /// `recycle_bin` — so a root that somehow carries both is read through the
    /// current one. `check` compares the answer against
    /// [`deletions_relation`](prov_graph::relation::RelationSet::deletions_relation)
    /// to report the old spelling as a rename to make, and `parked_dirs` uses it
    /// to know whether there may be bytes parked under the log.
    pub async fn deletions_pointer(&self, root_doc: &Path) -> Result<Option<(PathBuf, String)>> {
        for relation in [
            self.relations().deletions_relation(),
            self.relations().recycle_relation(),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(path) = self.pointer_target(root_doc, relation).await? {
                return Ok(Some((path, relation.to_string())));
            }
        }
        Ok(None)
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

    /// The directories a **byte-parking store** keeps its contents under — a
    /// retired recycle bin's `items/`, and a retired event store's archive.
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
    /// Another tool's store sitting beside the root — a version-control
    /// folder, a sync tool's cache, a vendored checkout — is here too, but
    /// only because the workspace **declared** it
    /// ([`out_of_scope`](Self::out_of_scope)). prov cannot recognize one:
    /// reachability says "nothing links this", which is equally true of a note
    /// its author forgot to link, so the difference is a statement only the
    /// workspace can make. Until it makes one, such a directory is ordinary
    /// unreached content — which is the honest answer, not an oversight.
    pub(crate) async fn parked_dirs(&self, root_doc: &Path) -> Result<Vec<PathBuf>> {
        // The declared scope first: it is the cheapest of the three (no read
        // at all) and the only one the workspace states rather than prov
        // deriving, so a walk bounded by it is bounded before any pointer is
        // followed.
        let mut dirs: Vec<PathBuf> = self.settings.out_of_scope.clone();
        // A retired prov event store the root still points at. The `history`
        // pointer and this parking survive the store's retirement so an
        // unmigrated workspace keeps its scans out of the event archive; both
        // go when nothing declares such a store any more.
        if let Some(index) = self.history_path(root_doc).await? {
            dirs.push(store_dir(&index).join("events"));
            dirs.push(store_dir(&index).join("blobs"));
        }
        // A recycle bin the root still points at. The deletion log that replaced
        // it parks nothing — a delete destroys the bytes and records that it
        // did — so this is only ever the bin of an unmigrated workspace, whose
        // parked items must stay out of every walk for exactly the reason above:
        // a binned document keeps its title, and indexing `items/` would resolve
        // `[[Some Note]]` to a copy of a note its author deleted. A `deletions/`
        // store has no `items/`, so the parking costs it nothing.
        if let Some((index, relation)) = self.deletions_pointer(root_doc).await?
            && Some(relation.as_str()) == self.relations().recycle_relation()
        {
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

    /// Load the controlled vocabulary a **reified** `fields` pointer names — one
    /// whose terms are nodes rather than rows (spec §3). The pointer's target is
    /// an ordinary content *index node* and its spanning-relation children are the
    /// terms, so this deliberately does not go through
    /// [`require_whole_file`](prov_graph::document::require_whole_file): the store
    /// is content, usually markdown-with-frontmatter, and that is the whole point
    /// of reifying. Any `vocabulary:` marker on the index node is ignored — what
    /// makes this a vocabulary is `reify: true` in the field declaration, not
    /// anything the target says about itself.
    ///
    /// `None` when the pointer does not resolve, or when the field declares no
    /// vocabulary at all.
    pub async fn load_reified_vocabulary(
        &self,
        root_doc: &Path,
        field: &str,
        spec: &crate::config::FieldSpec,
    ) -> Result<Option<crate::vocabulary::Vocabulary>> {
        let Some(terms) = self
            .reified_terms(root_doc, spec.vocabulary.as_deref())
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(crate::vocabulary::Vocabulary {
            field: field.to_string(),
            values: spec.values,
            terms: terms
                .into_iter()
                .map(|(key, (_, term))| (key, term))
                .collect(),
        }))
    }

    /// The **path of the term node** a reified vocabulary's `term` value names —
    /// the seam a consumer reads tier-3 payload through, since a reified term's
    /// gloss, body and per-term config live on the node rather than in a `terms:`
    /// row (spec §3).
    ///
    /// `None` when the pointer does not resolve or no child carries that term key.
    /// A *retired* term still returns its path: retirement says the value is no
    /// longer legal for new content, and the caller here is reading configuration
    /// off a node rather than judging membership — [`Vocabulary::accepts`] is what
    /// answers that question.
    ///
    /// Where two children claim one term key the **last** wins, matching the
    /// vocabulary [`load_reified_vocabulary`](Self::load_reified_vocabulary)
    /// builds, so the two never disagree about which node a value means.
    ///
    /// [`Vocabulary::accepts`]: crate::vocabulary::Vocabulary::accepts
    pub async fn reified_term_path(
        &self,
        root_doc: &Path,
        pointer: &str,
        term: &str,
    ) -> Result<Option<PathBuf>> {
        Ok(self
            .reified_terms(root_doc, Some(pointer))
            .await?
            .and_then(|mut terms| terms.remove(term))
            .map(|(path, _)| path))
    }

    /// The one walk behind both reified accessors: the index node's
    /// spanning-relation children, each resolved to a path and read as a term.
    /// `None` when the field declares no pointer or the pointer resolves to
    /// nothing.
    ///
    /// What is skipped, and why each skip belongs to somebody else: a child link
    /// that resolves to no path is the census's `BrokenLink`, a child that will
    /// not load is the walk's `Unreadable`, and a child with neither `term:` nor
    /// `title:` is simply not a term — none of the three is this loader's finding
    /// to raise, and raising it here would report it twice. Two children claiming
    /// one key collapse in `insert` order, the later winning; the tree is what
    /// says a term exists, so a duplicate is a structural mistake to see in
    /// `check`, not an error to fail a load on.
    async fn reified_terms(
        &self,
        root_doc: &Path,
        pointer: Option<&str>,
    ) -> Result<Option<BTreeMap<String, (PathBuf, crate::vocabulary::Term)>>> {
        let Some(pointer) = pointer else {
            return Ok(None);
        };
        let Some(index_path) = self.vocabulary_path(root_doc, pointer) else {
            return Ok(None);
        };
        // No spanning relation, no children to be terms.
        let Some(spanning) = self.relations().spanning_relation() else {
            return Ok(Some(BTreeMap::new()));
        };
        let (_, index) = self.load(&index_path).await?;
        let children = index
            .meta
            .get(spanning)
            .map(Value::link_strings)
            .unwrap_or_default();

        let mut terms = BTreeMap::new();
        for raw in children {
            let Target::Path(path) = self.resolve_link(&index_path, &Link::parse(&raw)) else {
                continue;
            };
            let Ok((_, child)) = self.load(&path).await else {
                continue;
            };
            // `term:` explicitly, `title:` by default — so a term can be retitled
            // for a reader without silently renaming the value every document
            // declares, while a node that never needed the distinction spells it
            // once.
            let Some(key) = child
                .meta
                .get("term")
                .and_then(Value::as_str)
                .or_else(|| child.meta.get("title").and_then(Value::as_str))
            else {
                continue;
            };
            let term = crate::vocabulary::Term {
                // The node's own prov id *is* the term's id — that is what
                // reifying buys — so the registry answers when frontmatter
                // storage is off and there is nothing to read off the document.
                id: child
                    .meta
                    .get("id")
                    .and_then(Value::as_str)
                    .map(|s| prov_graph::identity::Id(s.to_string()))
                    .or_else(|| self.index().id_for_path(&path)),
                // The prose body is the real gloss; prov does not read bodies
                // here, so only an explicit `means:` reaches a consumer.
                means: child
                    .meta
                    .get("means")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                retired: child
                    .meta
                    .get("retired")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            };
            terms.insert(key.to_string(), (path, term));
        }
        Ok(Some(terms))
    }

    /// The vocabulary governing `field`, loaded the way its declaration says to —
    /// [`load_reified_vocabulary`](Self::load_reified_vocabulary) under `reify:
    /// true`, [`load_vocabulary`](Self::load_vocabulary) otherwise. `None` for a
    /// type-only field: a field that names no vocabulary has nothing to be a
    /// member of.
    ///
    /// The single place the two forms are told apart, so a caller holding a
    /// [`FieldSpec`](crate::config::FieldSpec) never re-derives the choice.
    pub(crate) async fn load_field_vocabulary(
        &self,
        root_doc: &Path,
        field: &str,
        spec: &crate::config::FieldSpec,
    ) -> Result<Option<crate::vocabulary::Vocabulary>> {
        let Some(pointer) = spec.vocabulary.as_deref() else {
            return Ok(None);
        };
        if spec.reify {
            self.load_reified_vocabulary(root_doc, field, spec).await
        } else {
            self.load_vocabulary(root_doc, pointer).await
        }
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
        // A registry write means an id may now resolve elsewhere, which is a
        // change to the link graph no document's bytes show. The inbound index
        // is dropped whole rather than asked to work it out.
        if staged_index {
            self.forget_inbound();
        }
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
    /// write-ahead journal — this workspace's [`journal`](Self::journal), in
    /// its [home](Self::set_journal_home) when it has one.
    ///
    /// **Use this rather than [`ChangeSet::apply`] for anything that mutates a
    /// workspace.** `ChangeSet::apply` journals under `fs-transaction`'s own
    /// default name, in the root, which prov's recovery —
    /// [`crate::journal::recover`], the one `prov check` runs, or
    /// [`recover_journal`](Self::recover_journal) — does not look for. A
    /// crash mid-apply would then leave a journal nothing ever reads,
    /// stranding the change half-applied with no record of how to finish it.
    /// Routing every workspace write through here is what keeps the two ends
    /// naming the same file.
    ///
    /// Refused before anything is written when the journal home is relative
    /// (see [`journal`](Self::journal)). With a home, a journal left in the
    /// tree is not this apply's concern: only the homed journal can block it
    /// as stale.
    ///
    /// It is also the one place every write passes, which makes it where the
    /// inbound index learns what changed: decided from the staged bytes before
    /// the apply, settled against the disk after it, and dropped if the apply
    /// failed — see [`inbound`].
    pub async fn apply_set(&self, cs: &ChangeSet) -> Result<()> {
        let journal = self.journal()?;
        let plan = self.plan_inbound(cs);
        match journal.apply(cs, self.fs(), self.root()).await {
            Ok(()) => {
                self.settle_inbound(plan).await;
                Ok(())
            }
            Err(e) => {
                self.forget_inbound();
                Err(e.into())
            }
        }
    }

    /// Finish any change set a crash left in this workspace's
    /// [`journal`](Self::journal), rolling the tree forward to the
    /// fully-applied state, then remove the journal.
    ///
    /// The recovery that pairs with [`apply_set`](Self::apply_set): it reads
    /// the journal in the [home](Self::set_journal_home) when the workspace
    /// has one and in the root when it does not, through the same value the
    /// apply wrote, so the two cannot look in different places. Call it at
    /// open, before anything reads the tree. A no-op when there is no
    /// journal, so it is cheap to call unconditionally.
    ///
    /// With a home set, a journal in the tree is **not** read: a legacy one
    /// from before the home is recovered, if at all, by
    /// [`crate::journal::recover`] — the caller's decision, for the reasons
    /// [`recover_kept_in`](crate::journal::recover_kept_in) gives.
    pub async fn recover_journal(&self) -> Result<crate::journal::Recovered> {
        let recovered = self.journal()?.recover(self.fs(), self.root()).await?;
        if recovered != crate::journal::Recovered::Nothing {
            // Recovery wrote behind the inbound index's back; it is
            // stat-validated on every ask, but a dropped index is simply
            // honest about having missed the writes.
            self.forget_inbound();
        }
        Ok(recovered)
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
/// store's interiors, a retired recycle bin), which the scoped walks must not index,
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
    /// Nothing is skipped for being *parked* (a history store's interior, a
    /// retired recycle bin). Those bound a walk because a walk would descend into them;
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

    /// Execute a view: the documents `spec` covers, walking from `root_doc`.
    ///
    /// [`prov_views::select`] with the one thing that crate cannot supply for
    /// itself — a title index with the workspace's parked directories
    /// excluded, for an anchor written by title (`under: '[[Tasks]]'`). Built
    /// only when the anchor is a name; a path or an id anchor scans nothing.
    pub async fn select_view(
        &self,
        root_doc: &Path,
        spec: &prov_views::ViewSpec,
    ) -> std::result::Result<prov_views::Selection, prov_views::Error> {
        let nominal = spec.under.as_deref().is_some_and(|under| {
            let link = Link::parse(under);
            !link.is_external()
                && !link.is_same_document()
                && link.id_ref().is_none()
                && prov_graph::title::is_alias_shaped(link.addressed_target())
        });
        let titles = if nominal {
            Some(self.title_index_scoped(root_doc).await?)
        } else {
            None
        };
        prov_views::select_with(&self.graph, spec, root_doc, titles.as_ref()).await
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
    bulk: Option<Hook>,
    journal_home: Option<PathBuf>,
}

impl<FS, Id, Ix> WorkspaceBuilder<FS, Id, Ix> {
    /// Set the workspace root.
    pub fn root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = root.into();
        self
    }

    /// Who to tell when a whole-tree read starts and ends — see
    /// [`Workspace::set_bulk_reads`] and [`prov_graph::bulk`].
    pub fn bulk_reads(mut self, hook: Arc<dyn BulkReads>) -> Self {
        self.bulk = Some(Hook::new(hook));
        self
    }

    /// Keep the workspace's write-ahead journal in `home`, a directory
    /// outside the tree — for a tree something syncs. See
    /// [`Workspace::set_journal_home`] for why, and for the two obligations
    /// (an absolute home, one home per root) that come with it.
    ///
    /// A relative `home` is accepted here, since [`build`](Self::build)
    /// cannot fail, and refused at the workspace's first write instead —
    /// before anything is written. [`Workspace::journal`] reports it
    /// straight after `build` for a caller that would rather know at open.
    pub fn journal_home(mut self, home: impl Into<PathBuf>) -> Self {
        self.journal_home = Some(home.into());
        self
    }

    /// Set the relation vocabulary.
    pub fn relations(mut self, relations: RelationSet) -> Self {
        self.settings.relations = relations;
        self
    }

    /// Set the path-valued fields — see [`Workspace::references`].
    pub fn references(mut self, references: Vec<FieldPath>) -> Self {
        self.settings.references = references;
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

    /// Set whether a delete records what it destroyed (on by default) — see
    /// [`Workspace::record_deletions`].
    pub fn record_deletions(mut self, record_deletions: bool) -> Self {
        self.settings.record_deletions = record_deletions;
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

    /// Declare directories that are on disk beside the workspace but are not
    /// the workspace — see
    /// [`Workspace::out_of_scope`](Workspace::out_of_scope). Each is
    /// workspace-relative; anything absolute or reaching outside is the
    /// caller's error to avoid, since a builder has no config document to file
    /// a [`ConfigIssue`](crate::ConfigIssue) against.
    pub fn out_of_scope(mut self, dirs: impl IntoIterator<Item = PathBuf>) -> Self {
        self.settings.out_of_scope = dirs.into_iter().collect();
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
            bulk: self.bulk,
            journal_home: self.journal_home,
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
            bulk: self.bulk,
            journal_home: self.journal_home,
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
            references: self.settings.references.clone(),
            workspace_id: self.settings.workspace_id.clone(),
            id_storage: self.settings.id_storage,
        };
        let mut graph = Graph::new(self.fs, self.root, self.index, read);
        if let Some(hook) = self.bulk {
            graph.set_bulk_reads(hook.get());
        }
        Workspace {
            graph,
            identity: self.identity,
            settings: self.settings,
            pending_stamps: Vec::new(),
            inbound: inbound::empty(),
            journal_home: self.journal_home,
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
            references: vec![FieldPath::parse("sources[].resource")],
            link_style: LinkStyle::PlainRelative,
            id_links: true,
            reference_style: None,
            default_embed_format: fig::Format::Json,
            embed_style: EmbedStyle::CodeBlock,
            fixity: Fixity::Off,
            record_deletions: false,
            id_storage: IdStorage::Frontmatter,
            workspace_id: "notes".into(),
            out_of_scope: vec![PathBuf::from("history")],
            root: Some(PathBuf::from("home.md")),
            updated: "updated".into(),
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
        assert!(!ws.record_deletions());
        assert_eq!(ws.id_storage(), IdStorage::Frontmatter);
        assert_eq!(ws.workspace_id(), "notes");
        assert_eq!(ws.out_of_scope(), [PathBuf::from("history")]);
        assert_eq!(ws.relations().spanning_relation(), Some("contents"));
        assert_eq!(ws.references(), [FieldPath::parse("sources[].resource")]);
        // `id_links` has no field of its own on the far side — it is read back
        // through the reference style it feeds, which is the whole of what it
        // means.
        assert!(ws.id_links());
        assert_eq!(ws.reference_style().addressing, Addressing::Id);

        // And a clone is the same workspace, by the same mechanism.
        let copy = ws.clone();
        assert_eq!(copy.id_storage(), IdStorage::Frontmatter);
        assert_eq!(copy.workspace_id(), "notes");
        assert_eq!(copy.out_of_scope(), [PathBuf::from("history")]);
    }

    /// A config document declares the workspace's policy, and all of it arrives.
    /// The CLI's whole workspace construction is this conversion plus a root, an
    /// identity policy, and an index.
    #[test]
    fn a_config_becomes_the_workspaces_settings() {
        let config = crate::config::WorkspaceConfig {
            id_storage: IdStorage::Frontmatter,
            fixity: Fixity::Off,
            record_deletions: false,
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
        assert!(!ws.record_deletions());
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

    use prov_testkit::write;
    fn tempdir(tag: &str) -> PathBuf {
        prov_testkit::scratch("children", tag)
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

/// [`Workspace::load_reified_vocabulary`] and [`Workspace::reified_term_path`]
/// over a real filesystem. The fixtures are YAML frontmatter on markdown
/// carriers — deliberately the shape the *flat* loader refuses, since a reified
/// store being content is the whole distinction under test.
#[cfg(all(test, feature = "yaml"))]
mod reified_vocabulary_tests {
    use super::*;
    use crate::config::{FieldSpec, OpenClosed};
    use crate::identity::Minter;
    use prov_graph::exec::block_on;
    use prov_graph::fs::StdFs;
    use prov_graph::identity::Id as DocId;
    use prov_store::index::FileIndex;

    use prov_testkit::write;
    fn tempdir(tag: &str) -> PathBuf {
        prov_testkit::scratch("reified-vocab", tag)
    }

    fn spec(values: OpenClosed) -> FieldSpec {
        FieldSpec {
            ty: None,
            values,
            vocabulary: Some("vocab/index.md".into()),
            reify: true,
            default: None,
            under: None,
        }
    }

    /// One index node and every term shape the loader has to tell apart: a title
    /// carrying the key, an explicit `term:` overriding a reader-facing title, a
    /// retirement, a gloss, a self-declared id, and a child that is no term at
    /// all.
    fn a_vocabulary_of_audiences(tag: &str) -> PathBuf {
        let dir = tempdir(tag);
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- vocab/index.md\n---\n",
        );
        write(
            &dir,
            "vocab/index.md",
            "---\ntitle: Audiences\npart_of: /index.md\ncontents:\n\
             - public.md\n- friends.md\n- colleagues.md\n- readme.md\n- gone.md\n---\nWho may read what.\n",
        );
        write(
            &dir,
            "vocab/public.md",
            "---\ntitle: public\npart_of: index.md\nmeans: Anyone; safe to publish\n---\nThe gloss lives here.\n",
        );
        // A term retitled for a reader: the value every document declares is
        // `friends`, whatever the heading says.
        write(
            &dir,
            "vocab/friends.md",
            "---\ntitle: Friends and family\nterm: friends\nid: aud_k9fp\npart_of: index.md\n---\n",
        );
        write(
            &dir,
            "vocab/colleagues.md",
            "---\ntitle: colleagues\npart_of: index.md\nretired: true\n---\n",
        );
        // Neither `term:` nor `title:` — a node under the index that is not a term.
        write(
            &dir,
            "vocab/readme.md",
            "---\npart_of: index.md\n---\nHow to add a term.\n",
        );
        dir
    }

    fn load(dir: &Path, values: OpenClosed) -> crate::vocabulary::Vocabulary {
        let ws = Workspace::builder(StdFs).root(dir).build();
        block_on(ws.load_reified_vocabulary(Path::new("index.md"), "audience", &spec(values)))
            .unwrap()
            .expect("a reified vocabulary")
    }

    #[test]
    fn the_terms_are_the_index_nodes_spanning_children() {
        let dir = a_vocabulary_of_audiences("terms");
        let vocab = load(&dir, OpenClosed::Closed);
        assert_eq!(vocab.field, "audience");
        assert_eq!(vocab.values, OpenClosed::Closed);
        assert_eq!(
            vocab.terms.keys().cloned().collect::<Vec<_>>(),
            vec![
                "colleagues".to_string(),
                "friends".to_string(),
                "public".to_string()
            ],
            "a broken child link and a child that is no term are somebody else's finding"
        );
    }

    #[test]
    fn an_explicit_term_key_wins_over_the_title_it_is_read_under() {
        let dir = a_vocabulary_of_audiences("term-key");
        let vocab = load(&dir, OpenClosed::Closed);
        assert!(vocab.accepts("friends"));
        assert!(
            !vocab.terms.contains_key("Friends and family"),
            "retitling a term must not rename the value: {:?}",
            vocab.terms.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_term_node_without_an_explicit_key_is_read_under_its_title() {
        let dir = a_vocabulary_of_audiences("title-fallback");
        assert!(load(&dir, OpenClosed::Closed).accepts("public"));
    }

    #[test]
    fn retired_means_and_the_nodes_own_id_come_off_the_term_node() {
        let dir = a_vocabulary_of_audiences("fields");
        let vocab = load(&dir, OpenClosed::Closed);
        assert!(vocab.is_retired("colleagues"), "{:?}", vocab.terms);
        assert!(!vocab.accepts("colleagues"));
        assert_eq!(
            vocab.terms["public"].means.as_deref(),
            Some("Anyone; safe to publish"),
            "the prose body is the gloss prov does not read; `means:` is the one it does"
        );
        assert_eq!(vocab.terms["friends"].id, Some(DocId("aud_k9fp".into())));
        // Nothing invented for a term node that declares neither.
        assert_eq!(vocab.terms["public"].id, None);
        assert_eq!(vocab.terms["friends"].means, None);
    }

    /// The point of reifying: the term's id is the *node's* id. Frontmatter is
    /// only one of the two places that lives, so the registry answers for a
    /// workspace storing ids there instead.
    #[test]
    fn a_term_with_no_frontmatter_id_takes_the_one_the_registry_holds() {
        let dir = a_vocabulary_of_audiences("registry-id");
        let mut ws = Workspace::builder(StdFs)
            .root(&dir)
            .identity(Minter::lazy(9))
            .index(FileIndex::new(fig::Format::Yaml))
            .build();
        ws.index_mut()
            .register(&DocId("bcdfghj".into()), Path::new("vocab/public.md"));

        let vocab = block_on(ws.load_reified_vocabulary(
            Path::new("index.md"),
            "audience",
            &spec(OpenClosed::Closed),
        ))
        .unwrap()
        .expect("a reified vocabulary");
        assert_eq!(vocab.terms["public"].id, Some(DocId("bcdfghj".into())));
        // A self-declared id still wins — the document is the authority on itself.
        assert_eq!(vocab.terms["friends"].id, Some(DocId("aud_k9fp".into())));
    }

    /// The seam a consumer reads a term's tier-3 payload through: value in, the
    /// node that carries it out.
    #[test]
    fn a_term_value_resolves_to_the_node_that_declares_it() {
        let dir = a_vocabulary_of_audiences("term-path");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let path = |term: &str| {
            block_on(ws.reified_term_path(Path::new("index.md"), "vocab/index.md", term)).unwrap()
        };
        assert_eq!(path("public"), Some(PathBuf::from("vocab/public.md")));
        assert_eq!(path("friends"), Some(PathBuf::from("vocab/friends.md")));
        // Retirement is a membership judgment, not a reason to withhold the node.
        assert_eq!(
            path("colleagues"),
            Some(PathBuf::from("vocab/colleagues.md"))
        );
        assert_eq!(
            path("Friends and family"),
            None,
            "the key is the term, not the title"
        );
        assert_eq!(path("nobody"), None);
    }

    /// A field declaring no vocabulary has nothing to load, and a pointer at
    /// nothing resolves to nothing — neither is an error to raise here.
    #[test]
    fn a_field_with_no_pointer_and_a_pointer_at_nothing_both_load_nothing() {
        let dir = a_vocabulary_of_audiences("absent");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let none = FieldSpec {
            ty: None,
            values: OpenClosed::Closed,
            vocabulary: None,
            reify: true,
            default: None,
            under: None,
        };
        assert!(
            block_on(ws.load_reified_vocabulary(Path::new("index.md"), "audience", &none))
                .unwrap()
                .is_none()
        );
        assert!(
            block_on(ws.reified_term_path(
                Path::new("index.md"),
                "https://example.com/terms",
                "public"
            ))
            .unwrap()
            .is_none()
        );
    }
}

/// A journal kept outside the tree — [`Workspace::set_journal_home`] — over a
/// real filesystem, because the claims are about which files exist, and when.
#[cfg(test)]
mod journal_home_tests {
    use super::*;
    use crate::fs_faults::{FsEvent, RecordingFs};
    use crate::journal::{JOURNAL_NAME, Recovered};
    use prov_graph::exec::block_on;
    use prov_graph::fs::StdFs;
    use prov_testkit::{read, write};

    /// A workspace root with one parent document, and a separate journal home.
    fn fixture(tag: &str) -> (PathBuf, PathBuf) {
        let root = prov_testkit::scratch("journal-home", tag);
        write(&root, "index.md", "---\ntitle: Home\n---\n");
        let home = prov_testkit::scratch("journal-home", &format!("{tag}-home"));
        (root, home)
    }

    /// Is this a file the journal owns, by name — the journal itself or the
    /// staging sibling `write_atomic` publishes it through?
    fn is_journal(path: &Path) -> bool {
        path.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.contains(JOURNAL_NAME))
    }

    /// Every path an event touched.
    fn touched(event: &FsEvent) -> Vec<&Path> {
        match event {
            FsEvent::Write(p) | FsEvent::Remove(p) | FsEvent::Sync(p, _) => vec![p],
            FsEvent::Rename(from, to) => vec![from, to],
        }
    }

    /// **The synced-folder deployment.** A verb that journals — `create`
    /// writes the new document and edits its parent, a set of two — writes
    /// the documents it changes and nothing else in the tree, at any point
    /// of the apply. The recording backend sees every write, so this is
    /// checked as stated rather than inferred from the end state.
    #[test]
    fn a_homed_mutation_leaves_nothing_in_the_tree_but_its_documents() {
        let (root, home) = fixture("clean");
        let fs = RecordingFs::local();
        let mut ws = Workspace::builder(&fs)
            .root(&root)
            .journal_home(&home)
            .build();

        block_on(ws.create(Path::new("note.md"), Path::new("index.md"))).unwrap();

        let events = fs.events();
        // Not vacuous: the set *was* journaled — into the home.
        assert!(
            events
                .iter()
                .any(|e| matches!(e, FsEvent::Write(p) if p.starts_with(&home) && is_journal(p))),
            "the set was not journaled in the home: {events:?}"
        );
        // Every file the apply wrote, renamed or removed under the root is one
        // of the two documents it changes, or the staging sibling a
        // `write_atomic` of one of them goes through.
        let documents = ["index.md", "note.md"];
        for event in &events {
            if matches!(event, FsEvent::Sync(..)) {
                continue;
            }
            for path in touched(event) {
                let Ok(rel) = path.strip_prefix(&root) else {
                    continue;
                };
                let name = rel.to_str().unwrap();
                let staged = documents.iter().any(|d| name == format!(".{d}.fstx-tmp"));
                assert!(
                    documents.contains(&name) || staged,
                    "the apply touched {name} in the tree: {events:?}"
                );
            }
        }
        assert!(!root.join(JOURNAL_NAME).exists());
        assert!(!home.join(JOURNAL_NAME).exists(), "retired after the apply");
        assert!(read(&root, "index.md").contains("note.md"));
    }

    /// The same verb without a home journals in the root, as it always has —
    /// what the test above is the difference from.
    #[test]
    fn without_a_home_the_journal_is_in_the_root() {
        let (root, _) = fixture("in-tree");
        let fs = RecordingFs::local();
        let mut ws = Workspace::builder(&fs).root(&root).build();
        assert_eq!(ws.journal_home(), None);

        block_on(ws.create(Path::new("note.md"), Path::new("index.md"))).unwrap();

        assert!(
            fs.events()
                .iter()
                .any(|e| matches!(e, FsEvent::Write(p) if p.starts_with(&root) && is_journal(p)))
        );
    }

    /// An interrupted set journaled in the home is rolled forward by the
    /// homed recovery — both spellings of it — and not by the in-tree one,
    /// which looks where the journal is not.
    #[test]
    fn a_homed_journal_is_rolled_forward_by_the_homed_recovery() {
        let (root, home) = fixture("recover");
        let ws = Workspace::builder(StdFs)
            .root(&root)
            .journal_home(&home)
            .build();
        let mut cs = ChangeSet::new();
        cs.write("note.md", "---\ntitle: Note\npart_of: index.md\n---\n");
        cs.write("index.md", "---\ntitle: Home\ncontents:\n- note.md\n---\n");
        // The state a crash just after the commit point leaves: the whole
        // intent in the home, nothing yet in the tree.
        let journal = ws.journal().unwrap();
        assert_eq!(journal.path_in(&root), home.join(JOURNAL_NAME));
        std::fs::write(
            journal.path_in(&root),
            crate::journal::encode(cs.ops()).unwrap(),
        )
        .unwrap();

        assert_eq!(
            block_on(crate::journal::recover(&StdFs, &root)).unwrap(),
            Recovered::Nothing,
            "the in-tree recovery does not read a homed journal"
        );
        assert!(!root.join("note.md").exists());

        assert_eq!(
            block_on(ws.recover_journal()).unwrap(),
            Recovered::Applied(2)
        );
        assert!(read(&root, "index.md").contains("note.md"));
        assert!(root.join("note.md").exists());
        assert!(!home.join(JOURNAL_NAME).exists());

        // The free function finds the same file.
        std::fs::write(
            home.join(JOURNAL_NAME),
            crate::journal::encode(cs.ops()).unwrap(),
        )
        .unwrap();
        assert_eq!(
            block_on(crate::journal::recover_kept_in(&StdFs, &root, &home)).unwrap(),
            Recovered::Applied(2)
        );
        assert!(!home.join(JOURNAL_NAME).exists());
    }

    /// A `.prov-journal` in the tree — synced in from another machine, or left
    /// by an in-tree configuration before the home — is not a homed apply's
    /// concern: it neither blocks the write nor is touched by it. The same
    /// file does block a workspace that journals in the tree, which is what
    /// makes it a stale journal at all.
    #[test]
    fn a_journal_in_the_tree_does_not_block_a_homed_apply() {
        let (root, home) = fixture("stale-in-tree");
        write(&root, JOURNAL_NAME, "another machine's crash state");
        let mut cs = ChangeSet::new();
        cs.write("a.md", "a");
        cs.write("b.md", "b");

        let in_tree = Workspace::builder(StdFs).root(&root).build();
        let err = block_on(in_tree.apply_set(&cs)).unwrap_err();
        assert!(matches!(err, Error::StaleJournal(_)), "{err:?}");

        let homed = Workspace::builder(StdFs)
            .root(&root)
            .journal_home(&home)
            .build();
        block_on(homed.apply_set(&cs)).unwrap();
        assert_eq!(read(&root, "b.md"), "b");
        assert_eq!(
            read(&root, JOURNAL_NAME),
            "another machine's crash state",
            "a homed apply leaves the in-tree journal for the caller to decide on"
        );
        assert_eq!(
            block_on(homed.recover_journal()).unwrap(),
            Recovered::Nothing,
            "nor does the homed recovery read it"
        );
    }

    /// A relative home is refused at the first write, before anything —
    /// journal or document — is written, and is visible through `journal()`
    /// straight after `build` for a caller that wants to know at open.
    #[test]
    fn a_relative_home_is_refused_before_anything_is_written() {
        let (root, _) = fixture("relative");
        let ws = Workspace::builder(StdFs)
            .root(&root)
            .journal_home("not/absolute")
            .build();
        assert!(ws.journal().is_err());
        assert!(block_on(ws.recover_journal()).is_err());

        let mut cs = ChangeSet::new();
        cs.write("a.md", "a");
        cs.write("b.md", "b");
        let err = block_on(ws.apply_set(&cs)).unwrap_err();
        assert!(err.to_string().contains("absolute"), "{err}");
        assert!(!root.join("a.md").exists());
        assert!(!root.join(JOURNAL_NAME).exists());
        assert!(
            block_on(crate::journal::recover_kept_in(
                &StdFs,
                &root,
                "not/absolute"
            ))
            .is_err()
        );
    }

    /// A clone journals where its original does, and `None` puts the journal
    /// back in the root.
    #[test]
    fn the_home_is_carried_by_a_clone_and_cleared_by_none() {
        let (root, home) = fixture("clone");
        let mut ws = Workspace::builder(StdFs)
            .root(&root)
            .journal_home(&home)
            .build();
        let copy = ws.clone();
        assert_eq!(copy.journal_home(), Some(home.as_path()));
        assert_eq!(
            copy.journal().unwrap().path_in(&root),
            home.join(JOURNAL_NAME)
        );

        ws.set_journal_home(None);
        assert_eq!(ws.journal_home(), None);
        assert_eq!(
            ws.journal().unwrap().path_in(&root),
            root.join(JOURNAL_NAME)
        );
    }
}
