//! Crossing a workspace boundary — opening a peer, and descending into it.
//!
//! [`prov_graph::peer`] declares where the other workspaces are and stops
//! there, in so many words: "nothing here opens a workspace, and nothing here
//! can: reading the peer would need a second
//! [`ReadStorage`](prov_graph::fs::ReadStorage) and a second
//! [`IdIndex`](prov_graph::index::IdIndex), which only the host has." That is
//! true of `prov-graph`, and it stays true — it is why [`Graph`](prov_graph::Graph)
//! keeps two type parameters and why no method on it takes a resolver.
//!
//! It is not true one layer up. [`discovery::build`](crate::discovery) already
//! opens a second [`Workspace`] at an arbitrary directory out of the same
//! storage handle, because the storage seam is device-wide rather than
//! root-scoped: a crate that can walk *up* the filesystem to find a root it was
//! not given can also open a root a resolver hands it. So the descent lives
//! here, beside discovery, and `prov-graph` learns nothing.
//!
//! This keeps the promise [`peer`](prov_graph::peer) makes rather than breaking
//! it. Following a foreign reference is still "a *second step* after
//! resolution, taken by a caller that wants it"; all that changes is that prov
//! ships that second step once instead of each host writing it.
//!
//! ## What stays refused
//!
//! - **A [`PeerLocation::Url`] is never opened.** prov does no network I/O, and
//!   this is the one absolute in the module: a URL peer is an address to
//!   render, not a root to read. It is [`Refusal::Url`] under every trust
//!   level, including one a resolver has confirmed by its own means.
//! - **No writes across a boundary.** Everything here reads. Registration stays
//!   a publish-time contract — prov never reaches into another workspace to
//!   register on its behalf — so a reference to an unpublished foreign document
//!   can still dangle, and a `mv` still rewrites no peer's inbound references.
//! - **No peer table in the config.** A name is a fact about an archive, a
//!   location is a fact about a disk; being able to follow a map does not make
//!   the map the archive's business. Every location here comes from a
//!   [`PeerResolver`] the *host* supplied.
//! - **No resolver on [`Graph`](prov_graph::Graph)**, and no cost to a traversal
//!   that never crosses.
//! - **No following by default.** [`NoPeers`](prov_graph::NoPeers) is what a
//!   consumer that was given no resolver uses, and under it [`descend`] is
//!   exactly [`tree`](crate::workspace::Workspace::tree) with every foreign leaf
//!   carrying [`Refusal::Unknown`].
//!
//! ## Every refusal is an answer
//!
//! [`peer`](prov_graph::peer) has no error case, on purpose: "a peer that cannot
//! be found, cannot be read, or turns out to be someone else is never a failure,
//! because a foreign reference is carried whether or not it resolves." That does
//! not change because someone asked to follow it, so [`Refusal`] is never an
//! `Err` — it is the [`Boundary`] a leaf carries, and it is what lets a followed
//! tree say *why* ("the peer `notes` is on record at a directory that calls
//! itself `journal`") rather than silently showing what an unfollowed tree
//! shows.
//!
//! ## Termination
//!
//! Two workspaces may list each other, and an org root plus N repositories that
//! each link back is the expected shape rather than an exotic one. So [`descend`]
//! keeps a **trail** of the workspaces on the current path from the origin, and
//! refuses a peer already on it with [`Refusal::Cycle`] — a leaf, not an error.
//!
//! A trail rather than a global visited set, for the reason
//! [`tree`](prov_graph::graph::tree) gives for the same choice one level down:
//! revisiting a workspace from *another* branch is fine, because each branch
//! materializes its own subtree, and that is what makes this a tree rather than
//! a DAG rendered flat. An org root that names two documents of one repository —
//! its README and its `docs/tasks` index, say — renders both, exactly as
//! `Graph::tree` renders a document reached from two branches. Only a back-edge
//! to a workspace *on the current path* is a cycle. [`NodeKind::Cycle`] stays a
//! statement about one workspace's own spanning tree; this is the same shape one
//! level up.
//!
//! The trail is keyed on **both** halves, because both failures are reachable:
//! two names for one directory (a symlinked checkout) and two directories
//! claiming one name (the failure [`PeerLookup::confirm`] exists to catch,
//! arriving one hop later). The directory half is the *lexically normalized*
//! root directory — neither storage port offers a `canonicalize`, so a symlink
//! is not resolved away and it is the declared `workspace_id` that catches that
//! case. The origin is seeded onto the trail, so a peer that lists it back
//! stops there.
//!
//! Cost stays one *open* per peer whatever the trail does: opened peers are
//! memoized by name for the walk's duration, so an org root with many links into
//! one repository opens it once and walks it as many times as it is named.
//! [`Federation::workspaces`] therefore lists every peer opened, once, in
//! first-reached order.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use prov_graph::error::{Error, Result};
use prov_graph::identity::{Id, NoIdentity};
use prov_graph::index::{IdIndex, NoIndex};
use prov_graph::link;
use prov_graph::peer::{PeerLocation, PeerLookup, PeerResolver, Unconfirmed};
use prov_graph::{Node as GraphNode, NodeKind};
use prov_store::fs::Storage;
use prov_store::index::{FileIndex, IndexStore};

use crate::discovery::{Discovered, Discovery, discover};
use crate::workspace::{Settings, Workspace};

/// How many boundaries a [`descend`] crosses before it stops, when the caller
/// names no bound of its own. Crossings, not tree levels: a workspace's own
/// depth is bounded by its spanning tree and needs no help.
pub const DEFAULT_DEPTH: usize = 8;

/// A peer workspace the reader opened for reading — read-only, always.
///
/// [`NoIdentity`] rather than a minting policy, because there is no such thing
/// as writing across a boundary: identity is minted by the workspace that owns
/// the document, and a reader that could mint into a peer would be minting on
/// its behalf.
#[derive(Debug)]
pub struct Peer<FS> {
    /// The name that was asked for — the qualifier of the reference that led
    /// here, not necessarily what the workspace calls itself (under
    /// [`Trust::Unverified`] an anonymous peer answers to any name).
    pub name: String,
    /// Where the host said it was.
    pub location: PeerLocation,
    /// What discovery found there: the root directory, the root document, the
    /// registry pointer, the workspace node, and the effective config.
    pub discovered: Discovered,
    /// The opened workspace, over the same storage handle as the reader's.
    pub workspace: Workspace<FS, NoIdentity, FileIndex>,
}

impl<FS> Peer<FS> {
    /// What the peer calls itself — empty when it is anonymous.
    pub fn declares(&self) -> &str {
        &self.discovered.config.workspace_id
    }
}

/// The outcome of [`open_peer`]: a workspace, or the reason there is not one.
///
/// A single enum rather than a nested `Result`, so the refusal cannot be
/// mistaken for the error channel — an `Err` from [`open_peer`] means the
/// *reader's own* storage failed, never that the peer declined to open.
// `Opened` carries a whole `Workspace` and a whole `Discovered`, so it dwarfs
// the refusal — the same shape, and the same reasoning, as
// [`Discovery`](crate::discovery::Discovery): `open_peer` returns exactly one of
// these, once per boundary, and every caller destructures it immediately.
// Boxing to even the variants out would put an allocation in the signature to
// save a stack copy on a path taken once per crossing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Crossing<FS> {
    /// The peer was opened.
    Opened(Peer<FS>),
    /// It was not, and this is why.
    Refused(Refusal),
}

impl<FS> Crossing<FS> {
    /// The opened peer, or nothing.
    pub fn opened(self) -> Option<Peer<FS>> {
        match self {
            Self::Opened(peer) => Some(peer),
            Self::Refused(_) => None,
        }
    }

    /// The refusal, or nothing.
    pub fn refusal(&self) -> Option<&Refusal> {
        match self {
            Self::Refused(refusal) => Some(refusal),
            Self::Opened(_) => None,
        }
    }
}

/// How much doubt a reader will accept about *which* workspace it is opening.
///
/// The two halves of [`PeerLookup`]'s accessor pair, named. Neither reaches
/// [`Mismatched`](PeerLookup::Mismatched): the escape is for *absent* evidence,
/// never for evidence pointing the other way, and that is guaranteed by the
/// accessor rather than restated here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Trust {
    /// Only a peer that answers to the name asked for
    /// ([`PeerLookup::followable`]). The default, and the one a caller that has
    /// not thought about it gets.
    #[default]
    Confirmed,
    /// Also a peer whose name could not be checked — anonymous, or a location
    /// nothing local could read ([`PeerLookup::followable_unverified`]). The
    /// reader's escape to take, not prov's to take for it.
    Unverified,
}

/// Why a boundary was not crossed. Every variant is an answer, never an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No location on record. The ordinary state, and what every foreign leaf
    /// gets under [`NoPeers`](prov_graph::NoPeers).
    Unknown,
    /// A location whose occupant could not be checked against the name, under a
    /// trust level that requires it.
    Unconfirmed {
        /// Where the host said the workspace was.
        location: PeerLocation,
        /// Why the claim could not be checked.
        why: Unconfirmed,
    },
    /// A location occupied by a workspace that calls itself something else —
    /// the failure the whole design exists to prevent, caught. Refused under
    /// every trust level.
    Mismatched {
        /// Where the host said the workspace was.
        location: PeerLocation,
        /// The name the workspace found there actually declares.
        declares: String,
    },
    /// The location is a URL. prov does no network I/O, so this is refused
    /// however confidently the host asserted it.
    Url(String),
    /// The location is on record and followable, and discovery found no single
    /// workspace root there.
    Unopenable {
        /// Where the host said the workspace was.
        location: PeerLocation,
        /// What discovery said instead.
        reason: String,
    },
    /// The peer opened, and its own index has never heard of the id the
    /// reference named. The document may simply not be published yet —
    /// registration is a publish-time contract, and prov never registers on a
    /// peer's behalf.
    Unregistered {
        /// The peer the reference named.
        workspace: String,
        /// The id it named there.
        id: Id,
    },
    /// The peer is already on the path being descended — following it would go
    /// round. Two workspaces listing each other is the expected shape, not a
    /// mistake, so this is where the branch stops rather than a finding about
    /// either of them. Another *branch* may still enter the same workspace.
    Cycle {
        /// The peer the reference named.
        workspace: String,
        /// Its root directory, already on the path.
        root_dir: PathBuf,
    },
    /// The descent's crossing bound was reached. Says nothing about the peer.
    TooDeep,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown => f.write_str("no workspace of that name is on record"),
            Self::Unconfirmed { location, why } => write!(f, "{location} is unconfirmed: {why}"),
            Self::Mismatched { location, declares } => {
                write!(f, "{location} calls itself `{declares}`")
            }
            Self::Url(url) => write!(f, "{url} is a URL, and prov reads nothing over the network"),
            Self::Unopenable { location, reason } => {
                write!(f, "{location} could not be opened: {reason}")
            }
            Self::Unregistered { workspace, id } => {
                write!(f, "`{workspace}` has no document registered as `{id}`")
            }
            Self::Cycle {
                workspace,
                root_dir,
            } => write!(
                f,
                "`{workspace}` at {} is already on the path being descended",
                root_dir.display()
            ),
            Self::TooDeep => f.write_str("the descent's crossing bound was reached"),
        }
    }
}

/// Open the read-only [`Workspace`] a [`Discovered`] describes.
///
/// The library half of `prov-cli`'s `workspace()`, minus the identity policy —
/// which cannot come across, because it is a policy *type* there rather than a
/// value, and is what lets identity compile out entirely. The index half is
/// exactly the CLI's: parsed from the registry document the root declares, or,
/// under an [`IdStorage`](prov_graph::IdStorage) mode that keeps no registry,
/// rebuilt by scanning each document's own `id` field. A declared registry file
/// that is not on disk is an empty index rather than an error — a workspace that
/// has declared one and not yet written it is mid-bootstrap, not broken.
///
/// Public because it is the same opening a host doing anything else with a
/// [`Discovered`] needs; `prov-cli`'s `workspace()` could reasonably call it for
/// its index half and add the identity policy on top.
pub async fn open_discovered<FS: Storage + Clone>(
    fs: &FS,
    discovered: &Discovered,
) -> Result<Workspace<FS, NoIdentity, FileIndex>> {
    let index = if discovered.config.id_storage.keeps_registry() {
        match &discovered.registry {
            Some(rel) => {
                let text = match fs.read_to_string(&discovered.root_dir.join(rel)).await {
                    Ok(text) => text,
                    // A declared registry that is not on disk is an empty
                    // index, not a failure.
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                    Err(e) => return Err(Error::Io(e)),
                };
                FileIndex::parse(rel, &text)?
            }
            None => FileIndex::new(discovered.config.default_embed_format),
        }
    } else {
        // No registry document: rebuild the id→path map by scanning each file's
        // self-stored `id` field — a flat scan, independent of link resolution.
        let probe: Workspace<FS, NoIdentity, NoIndex> = Workspace::builder(fs.clone())
            .root(&discovered.root_dir)
            .build();
        let mut index = FileIndex::new(discovered.config.default_embed_format);
        for (id, path) in probe.scan_ids().await? {
            index.register(&id, &path);
        }
        // A scanned index reflects on-disk state, so it starts clean.
        index.mark_clean();
        index
    };
    Ok(Workspace::builder(fs.clone())
        .root(&discovered.root_dir)
        .settings(Settings::from(&discovered.config))
        .index(index)
        .build())
}

/// Open the workspace named `workspace`, if the host knows where it is and
/// `trust` allows following what it says.
///
/// The steps, in order, each of which can only refuse:
///
/// 1. Ask the resolver. [`Trust::Confirmed`] follows only
///    [`PeerLookup::followable`]; [`Trust::Unverified`] also follows
///    [`followable_unverified`](PeerLookup::followable_unverified). A
///    [`Mismatched`](PeerLookup::Mismatched) location is followed by neither.
/// 2. Refuse a [`PeerLocation::Url`] outright.
/// 3. Discover at the path. The location names a workspace **root**, so a
///    directory that merely sits inside one is [`Refusal::Unopenable`] rather
///    than a silent promotion to the enclosing workspace — a peer entry is a
///    claim about a root, and resolving it to something above it is how a
///    reference lands in real documents in the wrong archive.
/// 4. Check the name again. A resolver is *obliged* to have confirmed already,
///    but the evidence is now in hand for free, so this costs nothing and does
///    not depend on the host having kept its obligation. Under
///    [`Trust::Unverified`] an anonymous peer passes, as the reader asked; a peer
///    declaring a different name is [`Refusal::Mismatched`] under both.
pub async fn open_peer<FS: Storage + Clone>(
    fs: &FS,
    peers: &dyn PeerResolver,
    workspace: &str,
    trust: Trust,
) -> Result<Crossing<FS>> {
    let lookup = peers.locate(workspace);
    let followable = match trust {
        Trust::Confirmed => lookup.followable(),
        Trust::Unverified => lookup.followable_unverified(),
    };
    let Some(location) = followable.cloned() else {
        return Ok(Crossing::Refused(match lookup {
            PeerLookup::Unknown => Refusal::Unknown,
            PeerLookup::Unconfirmed { location, why } => Refusal::Unconfirmed { location, why },
            PeerLookup::Mismatched { location, declares } => {
                Refusal::Mismatched { location, declares }
            }
            // `Confirmed` is followable under both trust levels, so it never
            // reaches here; naming it beats an `unreachable!` on a value a host
            // supplied.
            PeerLookup::Confirmed(location) => Refusal::Unconfirmed {
                location,
                why: Unconfirmed::NotChecked,
            },
        }));
    };
    let root = match &location {
        PeerLocation::Url(url) => return Ok(Crossing::Refused(Refusal::Url(url.clone()))),
        PeerLocation::Path(root) => root.clone(),
    };

    let discovered = match discover(fs, &root).await {
        Ok(Discovery::Found(discovered)) => discovered,
        Ok(Discovery::Ambiguous { dir, candidates }) => {
            return Ok(unopenable(
                location,
                format!(
                    "ambiguous workspace root in {}: {}",
                    dir.display(),
                    candidates.join(", ")
                ),
            ));
        }
        Ok(Discovery::NotFound) => {
            return Ok(unopenable(location, "no workspace root there".to_string()));
        }
        Err(e) => return Ok(unopenable(location, e.to_string())),
    };
    if link::normalize(&discovered.root_dir) != link::normalize(&root) {
        return Ok(unopenable(
            location,
            format!(
                "that directory is not a workspace root (the nearest one is {})",
                discovered.root_dir.display()
            ),
        ));
    }

    let declares = discovered.config.workspace_id.clone();
    if !declares.is_empty() && declares != workspace {
        return Ok(Crossing::Refused(Refusal::Mismatched {
            location,
            declares,
        }));
    }
    if declares.is_empty() && trust == Trust::Confirmed {
        return Ok(Crossing::Refused(Refusal::Unconfirmed {
            location,
            why: Unconfirmed::Anonymous,
        }));
    }

    let opened = open_discovered(fs, &discovered).await?;
    Ok(Crossing::Opened(Peer {
        name: workspace.to_string(),
        location,
        discovered,
        workspace: opened,
    }))
}

fn unopenable<FS>(location: PeerLocation, reason: String) -> Crossing<FS> {
    Crossing::Refused(Refusal::Unopenable { location, reason })
}

/// How far a [`descend`] goes, and on whose say-so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Descent {
    /// How much doubt about a peer's identity the reader accepts.
    pub trust: Trust,
    /// How many boundaries may be crossed. Zero follows nothing;
    /// [`DEFAULT_DEPTH`] is the default.
    pub depth: usize,
}

impl Default for Descent {
    fn default() -> Self {
        Self {
            trust: Trust::default(),
            depth: DEFAULT_DEPTH,
        }
    }
}

/// One workspace a [`descend`] entered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reached {
    /// The name that was asked for. For the origin (index 0) this is what the
    /// workspace calls itself, and empty when it is anonymous — nobody asked.
    pub name: String,
    /// What the workspace calls itself — empty when it is anonymous.
    pub declares: String,
    /// Its root directory, as the resolver gave it.
    pub root_dir: PathBuf,
    /// Its root document, relative to `root_dir`.
    pub root_doc: PathBuf,
}

/// What a node's position says about the boundary it sits on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Boundary {
    /// A foreign reference that was followed. The node is the document it named
    /// *in the peer*, and its children are the peer's own subtree.
    Followed {
        /// The peer's index into [`Federation::workspaces`].
        into: usize,
    },
    /// A foreign reference that was not followed, and why. The node stays the
    /// [`NodeKind::Foreign`] leaf an unfollowed tree would show — with the
    /// reason attached, which is the whole difference.
    Refused(Refusal),
}

/// One node of a federated spanning tree.
///
/// [`prov_graph`]'s [`Node`](prov_graph::Node) with two fields added, because a
/// federated tree needs two facts its single-workspace counterpart does not: an
/// answer to "whose terms is this path in" — a relative path means nothing once
/// it has crossed a root — and, at a foreign reference, what happened there.
#[derive(Debug, Clone)]
pub struct Node {
    /// Which workspace this node belongs to: an index into
    /// [`Federation::workspaces`]. `path` is relative to *that* workspace's
    /// root, never to the origin's.
    pub workspace: usize,
    /// Workspace-relative, normalized path, in `workspace`'s terms. At an
    /// unfollowed foreign leaf this is the reference as written
    /// (`id:notes/ajp7eq`), exactly as [`prov_graph`]'s tree renders it.
    pub path: PathBuf,
    /// The document's `title` field, when present.
    pub title: Option<String>,
    /// The label the *parent's* link carried, when any — kept across a crossing,
    /// since the label belongs to the reference rather than to the target.
    pub label: Option<String>,
    /// How this node was resolved, in its own workspace's terms.
    pub kind: NodeKind,
    /// Spanning children, in declaration order.
    pub children: Vec<Node>,
    /// Set only at a foreign reference: whether it was followed, or refused and
    /// why. `None` everywhere else, so an ordinary node is silent about a
    /// boundary it does not sit on.
    pub boundary: Option<Boundary>,
}

/// A spanning tree that crossed one or more workspace boundaries, and the
/// workspaces it crossed into.
///
/// Paths are namespaced by workspace rather than merged into one list, for the
/// reason the boundary proposal gives: a relative path means nothing once it has
/// crossed a root, so every node says which workspace's terms it is in and a
/// consumer joins it onto that workspace's `root_dir` to reach a file.
#[derive(Debug, Clone)]
pub struct Federation {
    /// The federated tree, rooted at the document [`descend`] was started from.
    pub tree: Node,
    /// Every workspace reached, index 0 being the origin.
    pub workspaces: Vec<Reached>,
}

/// Materialize the spanning tree from `start`, following every **confirmed**
/// foreign spanning leaf into the peer it names and continuing there.
///
/// The single-workspace walk is [`Workspace::tree`]'s, unchanged and not
/// reimplemented — this composes it with [`open_peer`] and does nothing to a
/// tree that has no foreign leaves. Under
/// [`NoPeers`](prov_graph::NoPeers) the result is exactly
/// [`tree`](Workspace::tree) with every foreign leaf carrying
/// [`Refusal::Unknown`], which is why no existing caller changes behaviour.
///
/// Read-only throughout: nothing here writes, in the origin or in any peer.
pub async fn descend<FS, IdP, Ix>(
    ws: &Workspace<FS, IdP, Ix>,
    start: &Path,
    peers: &dyn PeerResolver,
    options: &Descent,
) -> Result<Federation>
where
    FS: Storage + Clone,
    Ix: IdIndex,
{
    let origin_root = ws.root().to_path_buf();
    let declares = ws.workspace_id().to_string();
    let root_doc = ws
        .root_document()
        .await?
        .unwrap_or_else(|| start.to_path_buf());

    let mut walk = Walk {
        fs: ws.fs(),
        peers,
        options,
        workspaces: vec![Reached {
            name: declares.clone(),
            declares: declares.clone(),
            root_dir: origin_root.clone(),
            root_doc,
        }],
        opened: vec![None],
        memo: BTreeMap::new(),
        // The origin is on the path before anything is walked, so a peer that
        // lists it back stops there.
        trail: vec![(link::normalize(&origin_root), declares)],
    };

    let tree = ws.tree(start).await?;
    let tree = walk.convert(tree, 0, 0).await?;
    Ok(Federation {
        tree,
        workspaces: walk.workspaces,
    })
}

/// What one descent carries: the storage handle every peer is opened over, the
/// resolver, the bound, the workspaces reached and the peers behind them, what
/// each name resolved to — and the trail, the one piece that is per-branch
/// rather than per-walk.
struct Walk<'a, FS> {
    fs: &'a FS,
    peers: &'a dyn PeerResolver,
    options: &'a Descent,
    workspaces: Vec<Reached>,
    /// Aligned with `workspaces`; `None` at index 0, which is the origin and was
    /// never opened as a peer.
    opened: Vec<Option<Peer<FS>>>,
    /// What each name asked for resolved to, so a name met a dozen times is
    /// located and opened once — and, on a second sighting, walked again rather
    /// than refused. Only outcomes that are facts *about the peer* are recorded:
    /// [`Refusal::TooDeep`] and [`Refusal::Cycle`] are facts about where in the
    /// walk the reference was met, and are decided at every sighting.
    memo: BTreeMap<String, std::result::Result<usize, Refusal>>,
    /// The workspaces on the current path from the origin — normalized root
    /// directory and declared name, the same pair
    /// [`open_peer`](crate::crossing::open_peer) checks identity with. Pushed on
    /// the way into a peer and popped on the way out, exactly as
    /// [`tree`](prov_graph::graph::tree) carries its own per-branch trail.
    trail: Vec<(PathBuf, String)>,
}

impl<'w, FS: Storage + Clone> Walk<'w, FS> {
    /// Rewrite one `prov-graph` node into a federated one, crossing at every
    /// foreign leaf. `ws` is the workspace whose terms `node`'s paths are in;
    /// `crossings` is how many boundaries have been crossed to get here.
    fn convert<'a>(
        &'a mut self,
        node: GraphNode,
        ws: usize,
        crossings: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Node>> + 'a>> {
        Box::pin(async move {
            if let NodeKind::Foreign { workspace, id } = &node.kind {
                let (name, id) = (workspace.clone(), id.clone());
                return self.cross(node, name, id, ws, crossings).await;
            }
            let GraphNode {
                path,
                title,
                label,
                kind,
                children: graph_children,
            } = node;
            let mut children = Vec::with_capacity(graph_children.len());
            for child in graph_children {
                children.push(self.convert(child, ws, crossings).await?);
            }
            Ok(Node {
                workspace: ws,
                path,
                title,
                label,
                kind,
                children,
                boundary: None,
            })
        })
    }

    /// Follow one foreign leaf, or leave it in place with the reason it stayed.
    fn cross<'a>(
        &'a mut self,
        node: GraphNode,
        name: String,
        id: Id,
        ws: usize,
        crossings: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Node>> + 'a>> {
        Box::pin(async move {
            if crossings >= self.options.depth {
                return Ok(refused(node, ws, Refusal::TooDeep));
            }
            let peer = match self.reach(&name).await? {
                Ok(peer) => peer,
                Err(refusal) => return Ok(refused(node, ws, refusal)),
            };
            // Already on the path from the origin: following it would go round.
            // Decided here rather than in `reach`, because it is a fact about
            // this branch and not about the peer — the same workspace reached
            // down another branch is walked, not refused.
            let key = (
                link::normalize(&self.workspaces[peer].root_dir),
                self.workspaces[peer].declares.clone(),
            );
            if self.on_trail(&key) {
                return Ok(refused(
                    node,
                    ws,
                    Refusal::Cycle {
                        workspace: name,
                        root_dir: self.workspaces[peer].root_dir.clone(),
                    },
                ));
            }
            // The peer's own index answers where the id lives — the registry
            // that issued it, which is the one thing `prov-graph` can never
            // reach from here.
            let Some(path) = self.opened[peer]
                .as_ref()
                .expect("a reached peer was opened")
                .workspace
                .index()
                .resolve(&id)
            else {
                return Ok(refused(
                    node,
                    ws,
                    Refusal::Unregistered {
                        workspace: name,
                        id,
                    },
                ));
            };
            let subtree = self.opened[peer]
                .as_ref()
                .expect("a reached peer was opened")
                .workspace
                .tree(&path)
                .await?;
            self.trail.push(key);
            let crossed = self.convert(subtree, peer, crossings + 1).await;
            self.trail.pop();
            let mut crossed = crossed?;
            // The node *is* the peer's document now — its path, title and kind
            // are the peer's — but the label came from the reference on this
            // side of the boundary, so it survives the crossing.
            crossed.label = node.label;
            crossed.boundary = Some(Boundary::Followed { into: peer });
            Ok(crossed)
        })
    }

    /// Whether a workspace is on the path from the origin to here.
    ///
    /// Either key settles it, because both failures are reachable: the
    /// directory catches two names for one workspace, and the declared name
    /// catches two directories that are one workspace — a symlinked checkout,
    /// which the lexical path comparison cannot see through. An anonymous
    /// workspace declares nothing, so only its directory speaks for it.
    fn on_trail(&self, (dir, declares): &(PathBuf, String)) -> bool {
        self.trail.iter().any(|(seen_dir, seen_declares)| {
            seen_dir == dir || (!declares.is_empty() && seen_declares == declares)
        })
    }

    /// The index into `workspaces` of the peer called `name`, opening it the
    /// first time this walk asks for it and answering from the memo afterwards.
    ///
    /// Says nothing about whether the peer may be *entered* from here — that is
    /// the trail's question, asked by the caller, because it depends on the
    /// branch rather than on the peer. What is decided here is decided once: a
    /// name that could not be located or opened is refused the same way every
    /// time it is met, and a name that opened is one `Workspace` for the whole
    /// walk however many branches reach it.
    ///
    /// Two names for one workspace collapse to one index, so
    /// [`Federation::workspaces`] is the set of workspaces reached rather than
    /// the list of names followed — and a peer that turns out to *be* the origin
    /// is index 0, which is what makes the trail recognize it.
    async fn reach(&mut self, name: &str) -> Result<std::result::Result<usize, Refusal>> {
        if let Some(outcome) = self.memo.get(name) {
            return Ok(outcome.clone());
        }
        let outcome = match open_peer(self.fs, self.peers, name, self.options.trust).await? {
            Crossing::Refused(refusal) => Err(refusal),
            Crossing::Opened(peer) => {
                let dir = link::normalize(&peer.discovered.root_dir);
                let declares = peer.declares().to_string();
                // A second name for a workspace already reached — the origin
                // among them, which is how a peer that lists the reader back is
                // recognized as the reader. `workspaces` is the set of
                // workspaces, so this gets the index it already has rather than
                // a second entry under the new name.
                let already = self.workspaces.iter().position(|reached| {
                    link::normalize(&reached.root_dir) == dir
                        || (!declares.is_empty() && reached.declares == declares)
                });
                match already {
                    Some(at) => {
                        // The origin is never opened as a peer, and now one has
                        // been opened at it, so the handle is worth keeping.
                        self.opened[at].get_or_insert(peer);
                        Ok(at)
                    }
                    None => {
                        self.workspaces.push(Reached {
                            name: name.to_string(),
                            declares,
                            root_dir: peer.discovered.root_dir.clone(),
                            root_doc: peer.discovered.root_doc.clone(),
                        });
                        self.opened.push(Some(peer));
                        Ok(self.workspaces.len() - 1)
                    }
                }
            }
        };
        self.memo.insert(name.to_string(), outcome.clone());
        Ok(outcome)
    }
}

/// A foreign leaf left where it was, carrying why. Every field but the boundary
/// is what an unfollowed tree would have shown, so a consumer that ignores
/// `boundary` renders exactly [`Workspace::tree`]'s answer.
fn refused(node: GraphNode, ws: usize, refusal: Refusal) -> Node {
    Node {
        // A refused leaf never left the workspace that declared it, so it stays
        // in that workspace's terms — the origin's only when the crossing
        // failed at the first hop, and a peer's wherever the walk had already
        // got to.
        workspace: ws,
        path: node.path,
        title: node.title,
        label: node.label,
        kind: node.kind,
        children: Vec::new(),
        boundary: Some(Boundary::Refused(refusal)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prov_graph::NoPeers;
    use prov_graph::exec::block_on;
    use prov_graph::fs::StdFs;
    use prov_testkit::write;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prov-crossing-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The outer root: a `contents` that is a manifest of foreign ids, which
    /// commits fine with none of the checkouts present because a name is not a
    /// path.
    fn org(dir: &Path, contents: &[&str]) {
        write(
            dir,
            "org/prov.yaml",
            "workspace_id: org\nroot: README.md\nid_storage: frontmatter\n",
        );
        let mut doc = String::from("---\nid: org1\ntitle: Org\ncontents:\n");
        for entry in contents {
            doc.push_str(&format!("- '{entry}'\n"));
        }
        doc.push_str("---\n");
        write(dir, "org/README.md", doc);
    }

    /// A sub-workspace in the phase-0 shape: a node naming its root, a root
    /// that says what contains it by foreign id, and — unless it is anonymous —
    /// a name to be referenced by.
    fn sub(dir: &Path, at: &str, declares: Option<&str>, id: &str, title: &str, contents: &[&str]) {
        let mut node = String::from("root: README.md\nid_storage: frontmatter\n");
        if let Some(name) = declares {
            node.push_str(&format!("workspace_id: {name}\n"));
        }
        write(dir, &format!("{at}/prov.yaml"), node);
        let mut doc = format!("---\nid: {id}\ntitle: {title}\npart_of: 'id:org/org1'\n");
        if !contents.is_empty() {
            doc.push_str("contents:\n");
            for entry in contents {
                doc.push_str(&format!("- '{entry}'\n"));
            }
        }
        doc.push_str("---\n");
        write(dir, &format!("{at}/README.md"), doc);
    }

    /// `org` beside `alpha` (which has a child) and `beta`. Siblings rather
    /// than nested, so no workspace scans another's ids into its own index.
    fn federation(tag: &str) -> PathBuf {
        let dir = tmp(tag);
        org(&dir, &["id:alpha/alpha1", "id:beta/beta1"]);
        sub(
            &dir,
            "alpha",
            Some("alpha"),
            "alpha1",
            "Alpha",
            &["notes.md"],
        );
        write(
            &dir,
            "alpha/notes.md",
            "---\nid: alpha2\ntitle: Alpha notes\npart_of: README.md\n---\n",
        );
        sub(&dir, "beta", Some("beta"), "beta1", "Beta", &[]);
        dir
    }

    fn open(dir: &Path) -> Workspace<StdFs, NoIdentity, FileIndex> {
        match block_on(discover(&StdFs, dir)).unwrap() {
            Discovery::Found(found) => block_on(open_discovered(&StdFs, &found)).unwrap(),
            other => panic!("expected a workspace at {}, got {other:?}", dir.display()),
        }
    }

    /// What the test map records for one name.
    enum At {
        /// A directory, confirmed the honest way.
        Dir(PathBuf),
        /// A directory the resolver vouches for without looking — a host that
        /// did not keep [`PeerResolver`]'s obligation, or a peer that was
        /// replaced between the lookup and the open.
        Claimed(PathBuf),
        /// A URL, and whether this host treats its own name resolution as the
        /// confirmation (diaryx's ARK) or leaves it unchecked.
        Url { url: String, confirmed: bool },
    }

    /// `prov-cli`'s `PeerMap` in a dozen lines: a path answer is confirmed by
    /// discovering the workspace there and reading what it calls itself.
    struct Peers(BTreeMap<String, At>);

    fn peers(entries: Vec<(&str, At)>) -> Peers {
        Peers(
            entries
                .into_iter()
                .map(|(name, at)| (name.to_string(), at))
                .collect(),
        )
    }

    impl PeerResolver for Peers {
        fn locate(&self, workspace: &str) -> PeerLookup {
            match self.0.get(workspace) {
                None => PeerLookup::Unknown,
                Some(At::Claimed(root)) => PeerLookup::Confirmed(PeerLocation::Path(root.clone())),
                Some(At::Url { url, confirmed }) => {
                    let location = PeerLocation::Url(url.clone());
                    if *confirmed {
                        PeerLookup::Confirmed(location)
                    } else {
                        PeerLookup::unchecked(location)
                    }
                }
                Some(At::Dir(root)) => {
                    let location = PeerLocation::Path(root.clone());
                    match block_on(discover(&StdFs, root)) {
                        Ok(Discovery::Found(found)) => {
                            PeerLookup::confirm(workspace, location, &found.config.workspace_id)
                        }
                        _ => PeerLookup::unreadable(location),
                    }
                }
            }
        }
    }

    fn names(federation: &Federation) -> Vec<&str> {
        federation
            .workspaces
            .iter()
            .map(|reached| reached.name.as_str())
            .collect()
    }

    /// Every node of a federated tree that was not crossed at is the node the
    /// single-workspace walk produced.
    fn same_shape(plain: &GraphNode, crossed: &Node) {
        assert_eq!(plain.path, crossed.path);
        assert_eq!(plain.title, crossed.title);
        assert_eq!(plain.label, crossed.label);
        assert_eq!(plain.kind, crossed.kind);
        assert_eq!(plain.children.len(), crossed.children.len());
        for (plain, crossed) in plain.children.iter().zip(&crossed.children) {
            same_shape(plain, crossed);
        }
    }

    #[test]
    fn no_peers_refuses_every_foreign_leaf_and_changes_nothing_else() {
        // The default everywhere, and the reason no existing caller changes
        // behaviour: the federated tree *is* the tree, with a reason attached
        // to each leaf that was already a leaf.
        let dir = federation("no-peers");
        let ws = open(&dir.join("org"));
        let plain = block_on(ws.tree("README.md")).unwrap();
        let federated = block_on(descend(
            &ws,
            Path::new("README.md"),
            &NoPeers,
            &Descent::default(),
        ))
        .unwrap();

        same_shape(&plain, &federated.tree);
        assert_eq!(names(&federated), ["org"]);
        assert_eq!(federated.workspaces[0].root_dir, dir.join("org"));
        assert_eq!(federated.tree.children.len(), 2);
        for child in &federated.tree.children {
            assert!(matches!(child.kind, NodeKind::Foreign { .. }));
            assert_eq!(child.workspace, 0);
            assert_eq!(child.boundary, Some(Boundary::Refused(Refusal::Unknown)));
        }
    }

    #[test]
    fn a_confirmed_peer_hangs_its_own_subtree_where_the_reference_was() {
        let dir = federation("follow");
        let ws = open(&dir.join("org"));
        let map = peers(vec![
            ("alpha", At::Dir(dir.join("alpha"))),
            ("beta", At::Dir(dir.join("beta"))),
        ]);
        let federated = block_on(descend(
            &ws,
            Path::new("README.md"),
            &map,
            &Descent::default(),
        ))
        .unwrap();

        assert_eq!(names(&federated), ["org", "alpha", "beta"]);
        assert_eq!(federated.workspaces[1].root_dir, dir.join("alpha"));
        assert_eq!(federated.workspaces[1].root_doc, Path::new("README.md"));
        assert_eq!(federated.workspaces[1].declares, "alpha");
        assert_eq!(federated.workspaces[2].root_dir, dir.join("beta"));

        let alpha = &federated.tree.children[0];
        assert_eq!(alpha.boundary, Some(Boundary::Followed { into: 1 }));
        // The node is the peer's document now — its path is in *alpha's* terms,
        // which is why every node has to say whose terms it is in.
        assert_eq!(alpha.workspace, 1);
        assert_eq!(alpha.path, Path::new("README.md"));
        assert_eq!(alpha.title.as_deref(), Some("Alpha"));
        assert_eq!(alpha.kind, NodeKind::Doc);
        assert_eq!(alpha.children.len(), 1);
        assert_eq!(alpha.children[0].path, Path::new("notes.md"));
        assert_eq!(alpha.children[0].workspace, 1);
        assert_eq!(alpha.children[0].boundary, None);

        let beta = &federated.tree.children[1];
        assert_eq!(beta.boundary, Some(Boundary::Followed { into: 2 }));
        assert_eq!(beta.workspace, 2);
        assert_eq!(beta.title.as_deref(), Some("Beta"));
        assert!(beta.children.is_empty());
    }

    #[test]
    fn an_org_that_names_two_documents_of_one_peer_renders_both() {
        // The motivating shape, and why the trail is per-branch: neither
        // reference is on the other's path from the origin, so both are
        // followed — exactly as `Graph::tree` renders a document reached from
        // two branches. The peer is opened once all the same.
        let dir = tmp("two-documents");
        org(&dir, &["id:alpha/alpha1", "id:alpha/alpha2"]);
        sub(
            &dir,
            "alpha",
            Some("alpha"),
            "alpha1",
            "Alpha",
            &["notes.md"],
        );
        write(
            &dir,
            "alpha/notes.md",
            "---\nid: alpha2\ntitle: Alpha notes\npart_of: README.md\n---\n",
        );
        let ws = open(&dir.join("org"));
        let map = peers(vec![("alpha", At::Dir(dir.join("alpha")))]);
        let federated = block_on(descend(
            &ws,
            Path::new("README.md"),
            &map,
            &Descent::default(),
        ))
        .unwrap();

        assert_eq!(names(&federated), ["org", "alpha"]);
        assert_eq!(federated.tree.children.len(), 2);

        let readme = &federated.tree.children[0];
        assert_eq!(readme.boundary, Some(Boundary::Followed { into: 1 }));
        assert_eq!(readme.path, Path::new("README.md"));
        assert_eq!(readme.children.len(), 1);
        assert_eq!(readme.children[0].path, Path::new("notes.md"));

        let notes = &federated.tree.children[1];
        assert_eq!(notes.boundary, Some(Boundary::Followed { into: 1 }));
        assert_eq!(notes.workspace, 1);
        assert_eq!(notes.path, Path::new("notes.md"));
        assert_eq!(notes.title.as_deref(), Some("Alpha notes"));
        assert!(notes.children.is_empty());
    }

    #[test]
    fn a_peer_that_lists_the_org_back_stops_there() {
        // The expected shape, not an exotic one — and the whole reason there is
        // a trail. The origin is on it before anything is walked, so the back
        // edge is a leaf with a reason rather than a second descent.
        let dir = tmp("links-back");
        org(&dir, &["id:alpha/alpha1"]);
        sub(
            &dir,
            "alpha",
            Some("alpha"),
            "alpha1",
            "Alpha",
            &["id:org/org1"],
        );
        let ws = open(&dir.join("org"));
        let map = peers(vec![
            ("alpha", At::Dir(dir.join("alpha"))),
            ("org", At::Dir(dir.join("org"))),
        ]);
        let federated = block_on(descend(
            &ws,
            Path::new("README.md"),
            &map,
            &Descent::default(),
        ))
        .unwrap();

        assert_eq!(names(&federated), ["org", "alpha"]);
        let back = &federated.tree.children[0].children[0];
        assert!(matches!(back.kind, NodeKind::Foreign { .. }));
        assert_eq!(back.workspace, 1);
        assert_eq!(
            back.boundary,
            Some(Boundary::Refused(Refusal::Cycle {
                workspace: "org".into(),
                root_dir: dir.join("org"),
            }))
        );
    }

    #[test]
    fn two_names_for_one_directory_are_refused_under_both_trust_levels() {
        // The map says `alpha2`, the archive says `alpha`, and following that
        // would land every `id:alpha2/…` reference on real documents in a
        // workspace that never answered to the name.
        let dir = federation("two-names");
        let map = peers(vec![("alpha2", At::Dir(dir.join("alpha")))]);
        for trust in [Trust::Confirmed, Trust::Unverified] {
            let outcome = block_on(open_peer(&StdFs, &map, "alpha2", trust)).unwrap();
            assert_eq!(
                outcome.refusal(),
                Some(&Refusal::Mismatched {
                    location: PeerLocation::Path(dir.join("alpha")),
                    declares: "alpha".into(),
                }),
                "under {trust:?}"
            );
        }
    }

    #[test]
    fn a_resolver_that_did_not_check_is_caught_at_the_open() {
        // Defence in depth: a resolver is obliged to confirm, and the evidence
        // is in hand for free once the peer has been read, so the check is made
        // again rather than taken on trust.
        let dir = federation("unchecked-claim");
        let map = peers(vec![("alpha", At::Claimed(dir.join("beta")))]);
        let outcome = block_on(open_peer(&StdFs, &map, "alpha", Trust::Confirmed)).unwrap();
        assert_eq!(
            outcome.refusal(),
            Some(&Refusal::Mismatched {
                location: PeerLocation::Path(dir.join("beta")),
                declares: "beta".into(),
            })
        );
    }

    #[test]
    fn an_anonymous_peer_is_refused_by_default_and_followed_on_insistence() {
        let dir = tmp("anonymous");
        org(&dir, &["id:gamma/gamma1"]);
        sub(&dir, "gamma", None, "gamma1", "Gamma", &[]);
        let ws = open(&dir.join("org"));
        let map = peers(vec![("gamma", At::Dir(dir.join("gamma")))]);

        let strict = block_on(descend(
            &ws,
            Path::new("README.md"),
            &map,
            &Descent::default(),
        ))
        .unwrap();
        assert_eq!(names(&strict), ["org"]);
        assert_eq!(
            strict.tree.children[0].boundary,
            Some(Boundary::Refused(Refusal::Unconfirmed {
                location: PeerLocation::Path(dir.join("gamma")),
                why: Unconfirmed::Anonymous,
            }))
        );

        let insistent = block_on(descend(
            &ws,
            Path::new("README.md"),
            &map,
            &Descent {
                trust: Trust::Unverified,
                ..Descent::default()
            },
        ))
        .unwrap();
        assert_eq!(names(&insistent), ["org", "gamma"]);
        assert_eq!(insistent.workspaces[1].declares, "");
        assert_eq!(
            insistent.tree.children[0].boundary,
            Some(Boundary::Followed { into: 1 })
        );
        assert_eq!(insistent.tree.children[0].title.as_deref(), Some("Gamma"));
    }

    #[test]
    fn an_anonymous_peer_that_is_the_reader_is_caught_by_the_directory() {
        // The half of the trail the name cannot carry: nothing is declared, so
        // only the root directory says these are one workspace.
        let dir = tmp("anonymous-self");
        write(
            &dir,
            "solo/prov.yaml",
            "root: README.md\nid_storage: frontmatter\n",
        );
        write(
            &dir,
            "solo/README.md",
            "---\nid: solo1\ntitle: Solo\ncontents:\n- 'id:mirror/solo1'\n---\n",
        );
        let ws = open(&dir.join("solo"));
        let map = peers(vec![("mirror", At::Dir(dir.join("solo")))]);
        let federated = block_on(descend(
            &ws,
            Path::new("README.md"),
            &map,
            &Descent {
                trust: Trust::Unverified,
                ..Descent::default()
            },
        ))
        .unwrap();

        assert_eq!(federated.workspaces.len(), 1);
        assert_eq!(federated.workspaces[0].name, "");
        assert_eq!(
            federated.tree.children[0].boundary,
            Some(Boundary::Refused(Refusal::Cycle {
                workspace: "mirror".into(),
                root_dir: dir.join("solo"),
            }))
        );
    }

    #[test]
    fn a_url_peer_is_never_opened() {
        // The one absolute in the module: prov reads nothing over the network,
        // however confidently the host asserted the address.
        let dir = tmp("url");
        org(&dir, &["id:ark/x", "id:web/x"]);
        let ws = open(&dir.join("org"));
        let map = peers(vec![
            (
                "ark",
                At::Url {
                    url: "https://diaryx.org/ark:/12345/x".into(),
                    confirmed: true,
                },
            ),
            (
                "web",
                At::Url {
                    url: "https://example.org/notes".into(),
                    confirmed: false,
                },
            ),
        ]);

        let strict = block_on(descend(
            &ws,
            Path::new("README.md"),
            &map,
            &Descent::default(),
        ))
        .unwrap();
        assert_eq!(
            strict.tree.children[0].boundary,
            Some(Boundary::Refused(Refusal::Url(
                "https://diaryx.org/ark:/12345/x".into()
            )))
        );
        // An unchecked URL never gets as far as the URL rule under the strict
        // trust — it is unconfirmed first — which is why the insistent pass
        // below is the one that proves the rule outranks the reader.
        assert_eq!(
            strict.tree.children[1].boundary,
            Some(Boundary::Refused(Refusal::Unconfirmed {
                location: PeerLocation::Url("https://example.org/notes".into()),
                why: Unconfirmed::NotChecked,
            }))
        );

        let insistent = block_on(descend(
            &ws,
            Path::new("README.md"),
            &map,
            &Descent {
                trust: Trust::Unverified,
                ..Descent::default()
            },
        ))
        .unwrap();
        assert_eq!(names(&insistent), ["org"]);
        for (child, url) in insistent.tree.children.iter().zip([
            "https://diaryx.org/ark:/12345/x",
            "https://example.org/notes",
        ]) {
            assert_eq!(
                child.boundary,
                Some(Boundary::Refused(Refusal::Url(url.into())))
            );
        }
    }

    #[test]
    fn an_id_the_peer_never_registered_is_a_leaf_with_the_id_named() {
        // Registration is a publish-time contract and prov never registers on a
        // peer's behalf, so a reference to an unpublished document dangles —
        // and says so.
        let dir = tmp("unregistered");
        org(&dir, &["id:alpha/nosuch"]);
        sub(&dir, "alpha", Some("alpha"), "alpha1", "Alpha", &[]);
        let ws = open(&dir.join("org"));
        let map = peers(vec![("alpha", At::Dir(dir.join("alpha")))]);
        let federated = block_on(descend(
            &ws,
            Path::new("README.md"),
            &map,
            &Descent::default(),
        ))
        .unwrap();

        assert_eq!(
            federated.tree.children[0].boundary,
            Some(Boundary::Refused(Refusal::Unregistered {
                workspace: "alpha".into(),
                id: Id("nosuch".into()),
            }))
        );
        // `workspaces` lists every peer opened, once, in first-reached order —
        // so alpha is there, whether or not a subtree of it rendered.
        assert_eq!(names(&federated), ["org", "alpha"]);
    }

    #[test]
    fn the_bound_counts_crossings_rather_than_tree_levels() {
        let dir = tmp("depth");
        org(&dir, &["id:alpha/alpha1"]);
        sub(
            &dir,
            "alpha",
            Some("alpha"),
            "alpha1",
            "Alpha",
            &["id:beta/beta1"],
        );
        sub(&dir, "beta", Some("beta"), "beta1", "Beta", &[]);
        let ws = open(&dir.join("org"));
        let map = peers(vec![
            ("alpha", At::Dir(dir.join("alpha"))),
            ("beta", At::Dir(dir.join("beta"))),
        ]);

        let none = block_on(descend(
            &ws,
            Path::new("README.md"),
            &map,
            &Descent {
                depth: 0,
                ..Descent::default()
            },
        ))
        .unwrap();
        assert_eq!(names(&none), ["org"]);
        assert_eq!(
            none.tree.children[0].boundary,
            Some(Boundary::Refused(Refusal::TooDeep))
        );

        let one = block_on(descend(
            &ws,
            Path::new("README.md"),
            &map,
            &Descent {
                depth: 1,
                ..Descent::default()
            },
        ))
        .unwrap();
        assert_eq!(names(&one), ["org", "alpha"]);
        assert_eq!(
            one.tree.children[0].boundary,
            Some(Boundary::Followed { into: 1 })
        );
        assert_eq!(
            one.tree.children[0].children[0].boundary,
            Some(Boundary::Refused(Refusal::TooDeep))
        );

        let two = block_on(descend(
            &ws,
            Path::new("README.md"),
            &map,
            &Descent {
                depth: 2,
                ..Descent::default()
            },
        ))
        .unwrap();
        assert_eq!(names(&two), ["org", "alpha", "beta"]);
    }

    #[test]
    fn a_directory_that_is_not_a_workspace_root_is_unopenable() {
        let dir = tmp("unopenable");
        std::fs::create_dir_all(dir.join("ghost")).unwrap();
        let map = peers(vec![("ghost", At::Dir(dir.join("ghost")))]);

        // The strict trust never gets there: the resolver could not read a
        // workspace, so the location is unconfirmed before it is opened.
        assert_eq!(
            block_on(open_peer(&StdFs, &map, "ghost", Trust::Confirmed))
                .unwrap()
                .refusal(),
            Some(&Refusal::Unconfirmed {
                location: PeerLocation::Path(dir.join("ghost")),
                why: Unconfirmed::Unreadable,
            })
        );
        let insistent = block_on(open_peer(&StdFs, &map, "ghost", Trust::Unverified)).unwrap();
        assert!(
            matches!(insistent.refusal(), Some(Refusal::Unopenable { .. })),
            "expected Unopenable, got {:?}",
            insistent.refusal()
        );
    }

    #[test]
    fn an_unknown_name_is_refused_without_being_looked_for() {
        let dir = federation("unknown");
        let map = peers(vec![]);
        assert_eq!(
            block_on(open_peer(&StdFs, &map, "alpha", Trust::Confirmed))
                .unwrap()
                .refusal(),
            Some(&Refusal::Unknown)
        );
        // And the directory really is there — it is the map that is empty.
        assert!(dir.join("alpha/README.md").exists());
    }

    #[test]
    fn an_opened_workspace_reads_the_registry_its_root_declares() {
        // The other half of `open_discovered`: a workspace that keeps a
        // registry document gets its index parsed from it, exactly as the CLI
        // builds one.
        let dir = tmp("registry");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\nregistry: registry.yaml\n---\n",
        );
        write(&dir, "registry.yaml", "registry:\n  r1: page.md\n");
        write(
            &dir,
            "page.md",
            "---\ntitle: Page\npart_of: index.md\n---\n",
        );
        let ws = open(&dir);
        assert_eq!(
            ws.index().resolve(&Id("r1".into())),
            Some(PathBuf::from("page.md"))
        );

        // And a declared registry that has not been written yet is an empty
        // index rather than a failure — a workspace mid-bootstrap is not broken.
        std::fs::remove_file(dir.join("registry.yaml")).unwrap();
        let ws = open(&dir);
        assert_eq!(ws.index().resolve(&Id("r1".into())), None);
    }
}
