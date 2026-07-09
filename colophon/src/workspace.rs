//! The workspace handle — where the filesystem, relation vocabulary, identity
//! policy, and index store are composed.
//!
//! The type parameters encode the "identity is a bolt-on" design: a
//! `Workspace<FS>` defaults to [`NoIdentity`] + [`NoIndex`] — paths only, with
//! the identity machinery compiled out. Opting in is one builder line that flips
//! a type parameter:
//!
//! ```no_run
//! use colophon::workspace::Workspace;
//! use colophon::relation::RelationSet;
//! # fn demo<FS>(fs: FS) {
//! // Paths only — no ID ever touches a document.
//! let ws = Workspace::builder(fs).root("vault").build();
//! # let _ = ws;
//! # }
//! ```
//!
//! The filesystem-driven `scan`/traverse/mutate engine ports from `diaryx_core`
//! next; the seams are in place so that port has somewhere to land.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::fs::Storage;
use crate::identity::{IdentityPolicy, NoIdentity, Trigger};
use crate::index::{IndexStore, NoIndex};
use crate::link::{self, Link, LinkStyle};
use crate::relation::RelationSet;

/// A composed workspace: a filesystem, a relation vocabulary, an identity
/// policy, an index store, and the link style it authors in.
#[derive(Debug, Clone)]
pub struct Workspace<FS, Id = NoIdentity, Ix = NoIndex> {
    fs: FS,
    root: PathBuf,
    relations: RelationSet,
    identity: Id,
    index: Ix,
    link_style: LinkStyle,
    id_links: bool,
    default_embed_format: fig::Format,
}

impl<FS> Workspace<FS, NoIdentity, NoIndex> {
    /// Start building a paths-only workspace over `fs`. Defaults: root `"."`,
    /// the [`RelationSet::diaryx`] vocabulary, identity off, and the default
    /// [`LinkStyle`] (`MarkdownRoot`, matching diaryx).
    pub fn builder(fs: FS) -> WorkspaceBuilder<FS, NoIdentity, NoIndex> {
        WorkspaceBuilder {
            fs,
            root: PathBuf::from("."),
            relations: RelationSet::diaryx(),
            identity: NoIdentity,
            index: NoIndex,
            link_style: LinkStyle::default(),
            id_links: false,
            default_embed_format: fig::Format::Yaml,
        }
    }
}

impl<FS, Id, Ix> Workspace<FS, Id, Ix> {
    /// The workspace root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The configured relation vocabulary.
    pub fn relations(&self) -> &RelationSet {
        &self.relations
    }

    /// The identity policy.
    pub fn identity(&self) -> &Id {
        &self.identity
    }

    /// The index store.
    pub fn index(&self) -> &Ix {
        &self.index
    }

    /// The link style this workspace authors in (read from the root's
    /// `link_format`, or the default).
    pub fn link_style(&self) -> LinkStyle {
        self.link_style
    }

    /// Whether this workspace authors durable structural links as
    /// `colophon:<id>` (registering the target) rather than a path.
    pub fn id_links(&self) -> bool {
        self.id_links
    }

    /// The metadata format a new document gets when it inherits no parent block
    /// — a *default* for authoring, not a workspace constraint (existing
    /// documents keep their own format on write, §7).
    pub fn default_embed_format(&self) -> fig::Format {
        self.default_embed_format
    }

    /// Mutable access to the index store (e.g. to persist it after mutations).
    pub fn index_mut(&mut self) -> &mut Ix {
        &mut self.index
    }
}

/// The resolution of one link target against a workspace: a path, an ID the
/// registry does not currently resolve, or an off-workspace reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A (normalized, workspace-relative) path.
    Path(PathBuf),
    /// A `colophon:<id>` reference with no live registry entry — unknown,
    /// tombstoned, or the workspace has no registry at all.
    UnresolvedId(crate::identity::Id),
    /// A URL or mail address — never resolved against the workspace and never
    /// rewritten by moves.
    External,
}

impl<FS, Id, Ix: IndexStore> Workspace<FS, Id, Ix> {
    /// Resolve `link` (declared in the document at `doc`) to a workspace
    /// target. Path targets resolve relative to `doc`'s directory; a
    /// `colophon:<id>` target resolves through the registry — the
    /// location-independent path that stays valid across moves.
    pub fn resolve_link(&self, doc: &Path, link: &Link) -> Target {
        if link.is_external() {
            return Target::External;
        }
        if let Some(id) = link.id_target() {
            return match self.index.resolve(&id) {
                Some(path) => Target::Path(link::normalize(path)),
                None => Target::UnresolvedId(id),
            };
        }
        Target::Path(link::resolve(doc, &link.target))
    }
}

impl<FS: Storage, Id, Ix: IndexStore> Workspace<FS, Id, Ix> {
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

    /// Read a single workspace-config value by `key` from the linked config
    /// document. `None` when there is no config document or it lacks the key —
    /// the caller falls back to its default.
    pub async fn config_get(
        &self,
        root_doc: &Path,
        key: &str,
    ) -> Result<Option<crate::meta::Value>> {
        let Some(config_doc) = self.config_path(root_doc).await? else {
            return Ok(None);
        };
        let (_, doc) = self.load(&config_doc).await?;
        Ok(doc.meta.get(key).cloned())
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
            .map(crate::meta::Value::link_strings)
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
    pub async fn register(&mut self, path: &Path, event: Trigger) -> Result<crate::identity::Id> {
        let path = link::normalize(path);
        if let Some(id) = self.index.id_for_path(&path) {
            return Ok(id);
        }
        if !self.identity.registration().fires_on(event) {
            return Err(Error::Structure(format!(
                "identity policy does not register on {event:?}"
            )));
        }
        if !self.fs.try_exists(&self.root.join(&path)).await? {
            return Err(Error::Structure(format!("{} does not exist", path.display())));
        }
        let id = self.mint_unique(&path);
        self.index.register(&id, &path);
        Ok(id)
    }

    /// Mint until the ID is unknown to the index — including tombstones, so a
    /// deleted document's ID is never reissued to mean something else.
    pub(crate) fn mint_unique(&mut self, path: &Path) -> crate::identity::Id {
        loop {
            let id = self.identity.mint(path);
            if !self.index.is_known(&id) {
                return id;
            }
        }
    }

    /// The target string colophon writes for a durable link from the document at
    /// `from` to `to` (titled `title`): a `colophon:<id>` when the workspace
    /// prefers id links and identity registers on a link — registering `to` — so
    /// the link survives a move untouched; otherwise a path rendered in the
    /// workspace's link style. The single seam through which create, rename
    /// repair, and autofix author a link.
    pub(crate) async fn authored_target(
        &mut self,
        from: &Path,
        to: &Path,
        title: &str,
    ) -> Result<String> {
        if self.id_links && self.identity.registration().fires_on(Trigger::Link) {
            let id = self.register(to, Trigger::Link).await?;
            Ok(link::id_target(&id))
        } else {
            Ok(link::format_link(self.link_style, from, to, title))
        }
    }
}

impl<FS: Storage, Id, Ix> Workspace<FS, Id, Ix> {
    /// The underlying filesystem.
    pub fn fs(&self) -> &FS {
        &self.fs
    }

    // TODO(port): scan/traverse/mutate from diaryx_core::workspace land here,
    // driving `fs` and maintaining `index` when `identity` triggers fire.
}

/// Builder for [`Workspace`]. Setting an identity policy or index store returns
/// a builder with a new type parameter, so the composed [`Workspace`] carries
/// exactly the layers requested — and none it does not.
#[derive(Debug, Clone)]
pub struct WorkspaceBuilder<FS, Id, Ix> {
    fs: FS,
    root: PathBuf,
    relations: RelationSet,
    identity: Id,
    index: Ix,
    link_style: LinkStyle,
    id_links: bool,
    default_embed_format: fig::Format,
}

impl<FS, Id, Ix> WorkspaceBuilder<FS, Id, Ix> {
    /// Set the workspace root.
    pub fn root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = root.into();
        self
    }

    /// Set the relation vocabulary.
    pub fn relations(mut self, relations: RelationSet) -> Self {
        self.relations = relations;
        self
    }

    /// Set the link style this workspace authors in (typically read from the
    /// root's `link_format`).
    pub fn link_style(mut self, link_style: LinkStyle) -> Self {
        self.link_style = link_style;
        self
    }

    /// Author durable structural links as `colophon:<id>` (Obsidian-style)
    /// rather than paths. Effective only when identity registers on a link.
    pub fn id_links(mut self, id_links: bool) -> Self {
        self.id_links = id_links;
        self
    }

    /// Set the metadata format new documents get when they inherit no parent
    /// block (a default, not a constraint).
    pub fn default_embed_format(mut self, format: fig::Format) -> Self {
        self.default_embed_format = format;
        self
    }

    /// Attach an identity policy, turning identity on.
    pub fn identity<Id2>(self, identity: Id2) -> WorkspaceBuilder<FS, Id2, Ix> {
        WorkspaceBuilder {
            fs: self.fs,
            root: self.root,
            relations: self.relations,
            identity,
            index: self.index,
            link_style: self.link_style,
            id_links: self.id_links,
            default_embed_format: self.default_embed_format,
        }
    }

    /// Attach an index store (where IDs are persisted).
    pub fn index<Ix2>(self, index: Ix2) -> WorkspaceBuilder<FS, Id, Ix2> {
        WorkspaceBuilder {
            fs: self.fs,
            root: self.root,
            relations: self.relations,
            identity: self.identity,
            index,
            link_style: self.link_style,
            id_links: self.id_links,
            default_embed_format: self.default_embed_format,
        }
    }

    /// Finish building.
    pub fn build(self) -> Workspace<FS, Id, Ix> {
        Workspace {
            fs: self.fs,
            root: self.root,
            relations: self.relations,
            identity: self.identity,
            index: self.index,
            link_style: self.link_style,
            id_links: self.id_links,
            default_embed_format: self.default_embed_format,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{IdentityPolicy, Minter};
    use crate::index::InMemoryIndex;

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
    fn identity_opts_in_via_one_builder_line() {
        let ws = Workspace::builder(DummyFs)
            .root("vault")
            .identity(Minter::lazy(1))
            .index(InMemoryIndex::new())
            .build();
        assert!(ws.identity().registration().on_link);
        assert!(ws.index().is_empty());
    }
}
