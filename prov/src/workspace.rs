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

use std::collections::BTreeSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use crate::change::ChangeSet;
use crate::config::{Fixity, History, IdStorage};
use crate::content::ContentFormat;
use crate::document::EmbedStyle;
use crate::error::{Error, Result};
use crate::fs::Storage;
use crate::identity::{IdentityPolicy, NoIdentity, Trigger};
use crate::index::{Collision, IndexStore, NoIndex};
use crate::link::{self, Addressing, Link, LinkStyle, ReferenceStyle, Wrapper};
use crate::meta::Value;
use crate::relation::RelationSet;
use crate::title::{self, TitleIndex, TitleMatch};

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
    reference_style: Option<ReferenceStyle>,
    default_embed_format: fig::Format,
    embed_style: EmbedStyle,
    fixity: Fixity,
    history: History,
    id_storage: IdStorage,
    /// Documents that earned an id this operation and, under a stamping mode,
    /// still need it written into their own frontmatter. Drained by
    /// [`commit`](Workspace::commit) into the operation's change set, so a
    /// document's id and the registry entry for it land in the same crash-atomic
    /// write — never one without the other.
    pending_stamps: Vec<(PathBuf, crate::identity::Id)>,
}

impl<FS> Workspace<FS, NoIdentity, NoIndex> {
    /// Start building a paths-only workspace over `fs`. Defaults: root `"."`,
    /// the [`RelationSet::diaryx`] vocabulary, identity off, and the default
    /// [`LinkStyle`] (`MarkdownRoot`, matching diaryx).
    ///
    /// Identity storage defaults to [`IdStorage::Registry`] — *not*
    /// [`WorkspaceConfig`](crate::config::WorkspaceConfig)'s `both` default — so a
    /// hand-built workspace keeps writing id-free documents unless it opts in.
    /// Consumers that drive the builder from a config (the normal path) pass
    /// [`id_storage`](WorkspaceBuilder::id_storage) and get the declared mode.
    pub fn builder(fs: FS) -> WorkspaceBuilder<FS, NoIdentity, NoIndex> {
        WorkspaceBuilder {
            fs,
            root: PathBuf::from("."),
            relations: RelationSet::diaryx(),
            identity: NoIdentity,
            index: NoIndex,
            link_style: LinkStyle::default(),
            id_links: false,
            reference_style: None,
            default_embed_format: fig::Format::Yaml,
            embed_style: EmbedStyle::Delimited,
            fixity: Fixity::Payloads,
            history: History::Off,
            id_storage: IdStorage::Registry,
        }
    }
}

impl<FS, Id, Ix> Workspace<FS, Id, Ix> {
    /// The workspace root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Join a workspace-relative path — a [`Node::path`](crate::tree::Node::path),
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
        self.root.join(rel)
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
        self.fixity
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
        self.history
    }

    /// How this workspace embeds metadata — the family (`delimited`,
    /// `code-block`, `html-script`, …) that, with
    /// [`default_embed_format`](Self::default_embed_format), resolves to the
    /// concrete carrier a document prov authors gets.
    pub fn embed_style(&self) -> EmbedStyle {
        self.embed_style
    }

    /// Where this workspace persists document ids (DESIGN §5). Consulted by the
    /// ops that *author* a document — under a stamping mode each one carries its
    /// own `id` — and by `check`, which reconciles the two homes against each
    /// other.
    pub fn id_storage(&self) -> IdStorage {
        self.id_storage
    }

    /// The workspace-default reference style — the fallback for any relation
    /// without its own `style` override. An explicit `reference_style` builder
    /// value wins; otherwise it is derived from the legacy `link_style`/`id_links`
    /// builder inputs so existing configurations behave exactly as before.
    pub fn reference_style(&self) -> ReferenceStyle {
        self.reference_style.unwrap_or(ReferenceStyle {
            wrapper: Wrapper::Markdown,
            addressing: if self.id_links {
                Addressing::Id
            } else {
                Addressing::Path
            },
            label: false,
            path_style: self.link_style,
        })
    }

    /// The reference style prov authors `relation`'s links in: the
    /// relation's own override if it declares one, else the workspace default.
    pub fn reference_style_for(&self, relation: &str) -> ReferenceStyle {
        self.relations
            .style_for(relation)
            .unwrap_or_else(|| self.reference_style())
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

/// Whether `path` names a document the title scan should read — one whose
/// extension is a recognized body format (Markdown/Djot/HTML) or a whole-file
/// metadata format (YAML/JSON/…). Non-document files (images, binaries) are
/// skipped so the scan neither reads nor mis-indexes them.
fn is_document_path(path: &Path) -> bool {
    !crate::document::is_opaque_payload(path)
}

/// The workspace-relative paths of the *files* among a directory's `entries`,
/// the listing a shadow check probes
/// ([`is_shadowed_payload`](Workspace::is_shadowed_payload)). Hidden entries are
/// skipped, matching the scans that build this.
fn file_listing(rel_dir: &Path, entries: &[crate::fs::DirEntry]) -> BTreeSet<PathBuf> {
    entries
        .iter()
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.file_name().and_then(|n| n.to_str()).map(str::to_owned))
        .filter(|name| !name.starts_with('.'))
        .map(|name| {
            if rel_dir.as_os_str().is_empty() {
                PathBuf::from(name)
            } else {
                rel_dir.join(name)
            }
        })
        .collect()
}

/// The resolution of one link target against a workspace: a path, an ID the
/// registry does not currently resolve, or an off-workspace reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A (normalized, workspace-relative) path.
    Path(PathBuf),
    /// An `id:<id>` reference with no live registry entry — unknown,
    /// tombstoned, or the workspace has no registry at all.
    UnresolvedId(crate::identity::Id),
    /// A nominal (alias) reference whose name several documents claim, so it
    /// cannot be resolved to one. The `String` is the name as written.
    AmbiguousAlias(String),
    /// A URL or mail address — never resolved against the workspace and never
    /// rewritten by moves.
    External,
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
        id: &crate::identity::Id,
        path: &Path,
    ) -> Option<Collision> {
        if let Some(held_by) = self.index.resolve(id)
            && held_by != path
        {
            return Some(Collision::Id {
                id: id.clone(),
                held_by,
            });
        }
        if let Some(held) = self.index.id_for_path(path)
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
    pub(crate) fn move_conflict(&self, id: &crate::identity::Id, dest: &Path) -> Option<Collision> {
        let held = self.index.id_for_path(dest)?;
        (held != *id).then(|| Collision::Path {
            path: dest.to_path_buf(),
            held,
        })
    }

    /// Resolve `link` (declared in the document at `doc`) to a workspace target,
    /// without nominal (alias) resolution — path and `id:` targets only. Use
    /// [`resolve_link_with`](Self::resolve_link_with) when a [`TitleIndex`] is
    /// available and `[[My File]]`-style aliases should resolve.
    pub fn resolve_link(&self, doc: &Path, link: &Link) -> Target {
        self.resolve_link_with(doc, link, None)
    }

    /// Resolve `link` to a workspace target. Path targets resolve relative to
    /// `doc`'s directory; an `id:<id>` target resolves through the registry (the
    /// location-independent path that stays valid across moves); an
    /// alias-shaped target (a bare name) resolves through `titles` when one is
    /// supplied — `Unique` to its path, `Ambiguous` to
    /// [`Target::AmbiguousAlias`], and `Unknown` falling through to a path (so a
    /// nominal link to nothing surfaces as a missing/broken path, exactly as
    /// before aliases existed). With `titles` `None`, alias resolution is off
    /// and this is the pure path/id resolver.
    pub fn resolve_link_with(
        &self,
        doc: &Path,
        link: &Link,
        titles: Option<&TitleIndex>,
    ) -> Target {
        if link.is_external() {
            return Target::External;
        }
        if let Some(id) = link.id_target() {
            return match self.index.resolve(&id) {
                Some(path) => Target::Path(link::normalize(path)),
                None => Target::UnresolvedId(id),
            };
        }
        if let Some(titles) = titles
            && title::is_alias_shaped(&link.target)
        {
            match titles.resolve(&link.target) {
                TitleMatch::Unique(path) => return Target::Path(link::normalize(path)),
                TitleMatch::Ambiguous(_) => return Target::AmbiguousAlias(link.target.clone()),
                // Unknown: fall through — a bare name with nothing behind it is
                // treated as a path, so it reads as missing like any dead link.
                TitleMatch::Unknown => {}
            }
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
    ) -> Result<Option<crate::meta::Value>> {
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
    /// [`require_whole_file`]: crate::document::require_whole_file
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
            crate::document::require_whole_file(&path, carrier)?;
        }
        Ok(crate::vocabulary::Vocabulary::from_meta(&doc.meta))
    }

    /// Build the workspace's [`TitleIndex`] by scanning every document under the
    /// root and registering it under its `title` and its file stem. This is a
    /// **derived cache** (DESIGN §5): rebuilt on demand, never persisted. It is
    /// what makes nominal (`[[My File]]`) references resolvable — a flat
    /// filesystem scan, deliberately independent of link resolution so that
    /// alias links can themselves be *spanning* (`contents: alias`) without a
    /// chicken-and-egg between "walk the tree" and "resolve the walk's links."
    pub async fn title_index(&self) -> Result<TitleIndex> {
        let mut index = TitleIndex::new();
        self.scan_titles(PathBuf::new(), &mut index).await?;
        Ok(index)
    }

    /// The title index bounded to the directories the workspace reaches from
    /// `start` (DESIGN §8) — the reachability-scoped counterpart to
    /// [`title_index`](Self::title_index). Only documents in a directory some
    /// link path/id-reaches are indexed, so a `[[alias]]` resolves within the
    /// workspace without scanning `target/`, a vendored tree, or a nested
    /// workspace at the repo root.
    ///
    /// Falls back to the full [`title_index`](Self::title_index) when the
    /// **spanning** relation is addressed by alias: descending the tree then needs
    /// every title up front, so the scan cannot be bounded (the chicken-and-egg
    /// the flat scan was written to avoid). An overlay alias to an *orphan* (a doc
    /// no path/id link reaches) likewise falls outside the scope and reads as
    /// broken — which it effectively is.
    pub async fn title_index_scoped(&self, start: &Path) -> Result<TitleIndex> {
        let (dirs, needs_full) = self.title_scope(start).await?;
        if needs_full {
            return self.title_index().await;
        }
        let mut index = TitleIndex::new();
        let files = self.direct_child_files(&dirs).await?;
        let listing: BTreeSet<PathBuf> = files.iter().cloned().collect();
        for rel in files {
            if !is_document_path(&rel) || self.is_shadowed_payload(&rel, &listing).await {
                continue;
            }
            if let Some(stem) = rel.file_stem().and_then(|s| s.to_str()) {
                index.insert(stem, rel.clone());
            }
            if let Ok((_, doc)) = self.load(&rel).await
                && let Some(title) = doc.meta.get("title").and_then(Value::as_str)
            {
                index.insert(title, rel.clone());
            }
        }
        Ok(index)
    }

    /// The directories the workspace occupies, reached from `start` by following
    /// path/id links — spanning links drive descent, and every relation's (and
    /// body wikilink's) path/id target contributes its directory, so an alias can
    /// resolve to anything the tree links. The scope [`title_index_scoped`] indexes.
    ///
    /// The returned flag is `true` when a **spanning** link is alias-shaped: it
    /// cannot be followed without the title index, so the scope would be
    /// incomplete and the caller must scan in full instead.
    async fn title_scope(&self, start: &Path) -> Result<(BTreeSet<PathBuf>, bool)> {
        let spanning = self.relations().spanning_relation().map(str::to_owned);
        let dir_of = |p: &Path| p.parent().unwrap_or(Path::new("")).to_path_buf();
        let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
        let mut visited: BTreeSet<PathBuf> = BTreeSet::new();
        let mut queue = vec![link::normalize(start)];
        let mut needs_full = false;
        while let Some(path) = queue.pop() {
            if !visited.insert(path.clone()) {
                continue;
            }
            dirs.insert(dir_of(&path));
            let Ok((_, doc)) = self.load(&path).await else {
                continue;
            };
            for edge in self.relations().edges(&doc.meta) {
                let link = Link::parse(&edge.target);
                let is_spanning = Some(edge.relation.as_str()) == spanning.as_deref();
                if link.is_external() {
                    continue;
                }
                if title::is_alias_shaped(&link.target) {
                    // Can't resolve without the index; a spanning alias defeats bounding.
                    needs_full = needs_full || is_spanning;
                    continue;
                }
                if let Target::Path(target) = self.resolve_link(&path, &link) {
                    dirs.insert(dir_of(&target));
                    if is_spanning {
                        queue.push(target);
                    }
                }
            }
            for body_link in link::scan_body_links(&path, &doc.body) {
                let link = body_link.link;
                if link.is_external() || title::is_alias_shaped(&link.target) {
                    continue;
                }
                if let Target::Path(target) = self.resolve_link(&path, &link) {
                    dirs.insert(dir_of(&target));
                }
            }
        }
        Ok((dirs, needs_full))
    }

    /// Scan every document under the root for a self-stored `id` frontmatter
    /// field, returning the `(id, path)` pairs — the rebuildable id→path map for
    /// the frontmatter-only identity storage mode ([`IdStorage::FrontmatterOnly`]).
    /// Like [`title_index`](Self::title_index) this is a flat filesystem scan,
    /// deliberately independent of link resolution (so it can bootstrap the very
    /// index that id links resolve through, with no chicken-and-egg).
    ///
    /// [`IdStorage::FrontmatterOnly`]: crate::config::IdStorage::FrontmatterOnly
    pub async fn scan_ids(&self) -> Result<Vec<(crate::identity::Id, PathBuf)>> {
        let mut ids = Vec::new();
        self.scan_ids_dir(PathBuf::new(), &mut ids).await?;
        Ok(ids)
    }

    /// Every content document (Markdown/Djot/HTML) under the root, as sorted
    /// workspace-relative paths — the on-disk population the orphan check diffs
    /// against what the spanning tree reaches (DESIGN §8). Deliberately restricted
    /// to *content* documents: whole-file metadata sidecars (a config or registry
    /// document, a stray `.yaml`) are not prose a user orphans, so they are not
    /// candidates. A flat filesystem scan (hidden entries skipped), independent of
    /// link resolution, like the title/id scans beside it.
    pub async fn content_documents(&self) -> Result<Vec<PathBuf>> {
        let mut docs = Vec::new();
        self.scan_content_dir(PathBuf::new(), &mut docs).await?;
        docs.sort();
        Ok(docs)
    }

    /// The workspace-relative direct-child files of each directory in `dirs`
    /// (non-recursive), skipping hidden entries and unreadable directories.
    ///
    /// The bounded-scan primitive behind reachability-scoped discovery (DESIGN
    /// §8): it opens only the directories it is handed and never descends into
    /// subdirectories, so an *unreached* directory — a vendored tree, a nested
    /// prov workspace — is neither read nor reported. Callers filter the
    /// result for the file kind they care about (content documents for the orphan
    /// check, opaque payloads for `attach --all`).
    pub(crate) async fn direct_child_files(
        &self,
        dirs: &BTreeSet<PathBuf>,
    ) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        for dir in dirs {
            let Ok(entries) = self.fs.read_dir(&self.root.join(dir)).await else {
                continue;
            };
            for entry in entries {
                let Some(name) = entry
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_owned)
                else {
                    continue;
                };
                if name.starts_with('.') || !entry.file_type().is_file() {
                    continue;
                }
                files.push(if dir.as_os_str().is_empty() {
                    PathBuf::from(&name)
                } else {
                    dir.join(&name)
                });
            }
        }
        Ok(files)
    }

    /// The directories the reachable set `reachable` occupies — each reached
    /// document's own directory (the workspace root's directory always among
    /// them, since the root document is reachable). The scope
    /// [`direct_child_files`](Self::direct_child_files) is bounded to: a directory
    /// is "known" precisely when a linked document lives directly in it.
    pub(crate) fn reached_dirs(reachable: &BTreeSet<PathBuf>) -> BTreeSet<PathBuf> {
        reachable
            .iter()
            .map(|p| p.parent().unwrap_or(Path::new("")).to_path_buf())
            .collect()
    }

    /// Recursively collect content-document paths under `rel_dir`. Same walk as
    /// [`scan_ids_dir`](Self::scan_ids_dir); unreadable/hidden entries are skipped.
    fn scan_content_dir<'a>(
        &'a self,
        rel_dir: PathBuf,
        docs: &'a mut Vec<PathBuf>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
            let Ok(entries) = self.fs.read_dir(&self.root.join(&rel_dir)).await else {
                return Ok(());
            };
            for entry in entries {
                let Some(name) = entry
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_owned)
                else {
                    continue;
                };
                if name.starts_with('.') {
                    continue;
                }
                let rel = if rel_dir.as_os_str().is_empty() {
                    PathBuf::from(&name)
                } else {
                    rel_dir.join(&name)
                };
                if entry.file_type().is_dir() {
                    self.scan_content_dir(rel, docs).await?;
                } else if entry.file_type().is_file()
                    && ContentFormat::from_extension(&rel).is_some()
                {
                    docs.push(rel);
                }
            }
            Ok(())
        })
    }

    /// Recursively collect self-stored `id` fields under `rel_dir`. Same walk as
    /// [`scan_titles`](Self::scan_titles); unreadable/hidden entries are skipped.
    fn scan_ids_dir<'a>(
        &'a self,
        rel_dir: PathBuf,
        ids: &'a mut Vec<(crate::identity::Id, PathBuf)>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
            let Ok(entries) = self.fs.read_dir(&self.root.join(&rel_dir)).await else {
                return Ok(());
            };
            let listing = file_listing(&rel_dir, &entries);
            for entry in entries {
                let Some(name) = entry
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_owned)
                else {
                    continue;
                };
                if name.starts_with('.') {
                    continue;
                }
                let rel = if rel_dir.as_os_str().is_empty() {
                    PathBuf::from(&name)
                } else {
                    rel_dir.join(&name)
                };
                if entry.file_type().is_dir() {
                    self.scan_ids_dir(rel, ids).await?;
                } else if entry.file_type().is_file()
                    && is_document_path(&rel)
                    // An `id:` inside a shadowed payload is an example, not a
                    // claim on the registry (see `attach_opaque`).
                    && !self.is_shadowed_payload(&rel, &listing).await
                    && let Ok((_, doc)) = self.load(&rel).await
                    && let Some(id) = doc.meta.get("id").and_then(Value::as_str)
                    && !id.trim().is_empty()
                {
                    ids.push((crate::identity::Id(id.trim().to_string()), rel));
                }
            }
            Ok(())
        })
    }

    /// Recursively index the documents under the workspace-relative `rel_dir`.
    /// Unreadable directories and files are skipped (a title index is a
    /// best-effort cache, not a validation pass); hidden entries (`.`-prefixed)
    /// are ignored.
    fn scan_titles<'a>(
        &'a self,
        rel_dir: PathBuf,
        index: &'a mut TitleIndex,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
            let Ok(entries) = self.fs.read_dir(&self.root.join(&rel_dir)).await else {
                return Ok(());
            };
            let listing = file_listing(&rel_dir, &entries);
            for entry in entries {
                let Some(name) = entry
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_owned)
                else {
                    continue;
                };
                if name.starts_with('.') {
                    continue;
                }
                let rel = if rel_dir.as_os_str().is_empty() {
                    PathBuf::from(&name)
                } else {
                    rel_dir.join(&name)
                };
                if entry.file_type().is_dir() {
                    self.scan_titles(rel, index).await?;
                } else if entry.file_type().is_file()
                    && is_document_path(&rel)
                    // A shadowed payload is bytes prov agreed not to read: its
                    // title is a specimen's, and must not answer `[[alias]]`.
                    && !self.is_shadowed_payload(&rel, &listing).await
                {
                    // Always index by stem (name-based resolution, Obsidian-style)…
                    if let Some(stem) = rel.file_stem().and_then(|s| s.to_str()) {
                        index.insert(stem, rel.clone());
                    }
                    // …and by the declared `title` when the document parses.
                    if let Ok((_, doc)) = self.load(&rel).await
                        && let Some(title) = doc.meta.get("title").and_then(Value::as_str)
                    {
                        index.insert(title, rel.clone());
                    }
                }
            }
            Ok(())
        })
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
            return Err(Error::NotFound(path.to_path_buf()));
        }
        let id = self.mint_unique(&path);
        self.index.register(&id, &path);
        self.queue_stamp(&path, &id);
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
    pub(crate) fn register_for_authoring(&mut self, path: &Path) -> crate::identity::Id {
        let path = link::normalize(path);
        if let Some(id) = self.index.id_for_path(&path) {
            return id;
        }
        let id = self.mint_unique(&path);
        self.index.register(&id, &path);
        self.queue_stamp(&path, &id);
        id
    }

    /// Note that `path` should carry `id` in its own frontmatter, for
    /// [`commit`](Self::commit) to stage. A no-op unless the workspace stores ids
    /// in the document (DESIGN §5) — under registry-only storage a document never
    /// learns its own id.
    fn queue_stamp(&mut self, path: &Path, id: &crate::identity::Id) {
        if self.id_storage.stamps_frontmatter() {
            self.pending_stamps.push((path.to_path_buf(), id.clone()));
        }
    }
}

impl<FS: Storage, Id, Ix> Workspace<FS, Id, Ix> {
    /// The underlying filesystem.
    pub fn fs(&self) -> &FS {
        &self.fs
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
        self.index.rollback();
        self.index.checkpoint();
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
    ) -> Result<(String, crate::document::Document)> {
        let Some(bytes) = cs.staged(path) else {
            return self.load(path).await;
        };
        let text = String::from_utf8(bytes.to_vec())
            .map_err(|e| Error::Structure(format!("{} is not valid UTF-8: {e}", path.display())))?;
        let doc = crate::document::Document::parse(path, &text)?;
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
            self.index.rollback();
            return Err(e);
        }
        // The registry lives in a document, and the op may be moving or rewriting
        // that very document. Follow it before rendering — staged last, this write
        // would otherwise clobber the op's own edit to it.
        if let Err(e) = self.index.rebase(&cs) {
            self.index.rollback();
            return Err(e);
        }
        let staged_index = match self.index.pending_write() {
            Ok(Some((path, text))) => {
                cs.write(path, text);
                true
            }
            Ok(None) => false,
            Err(e) => {
                self.index.rollback();
                return Err(e);
            }
        };
        match cs.apply(&self.fs, &self.root).await {
            Ok(()) => {
                // Unconditional: the op succeeded, so its checkpoint is spent
                // either way. `staged_index` only says whether the store may now
                // call itself persisted — a store with no home stages nothing and
                // must stay dirty for whoever does write it, but its checkpoint is
                // just as finished as anyone's.
                self.index.committed(staged_index);
                Ok(())
            }
            Err(e) => {
                self.index.rollback();
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
                None => match self.fs.read_to_string(&self.root.join(&path)).await {
                    Ok(text) => text,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e.into()),
                },
            };
            let doc = crate::document::Document::parse(&path, &text)?;
            if doc.meta.get("id").and_then(Value::as_str) == Some(id.0.as_str()) {
                continue;
            }
            let updated =
                crate::edit::set_in_text(&text, doc.carrier, "id", fig::Value::Str(id.0.clone()))?;
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
        seed: &crate::meta::Mapping,
        format: fig::Format,
    ) -> Result<bool> {
        let mut cs = ChangeSet::new();
        let created = !self.fs.try_exists(&self.root.join(sidecar)).await?;
        if created {
            cs.write(sidecar, crate::meta::serialize_mapping(seed, format)?);
        }
        // The pointer value is the sidecar path as written (a bare filename when it
        // sits beside the root, which is the convention). Set it comment- and
        // format-preservingly, like any other metadata edit.
        let (text, doc) = self.load(root_doc).await?;
        let updated = crate::edit::set_in_text(
            &text,
            doc.carrier,
            pointer,
            crate::edit::infer_scalar(&sidecar.to_string_lossy()),
        )?;
        cs.write(root_doc, updated);
        cs.apply(&self.fs, &self.root).await?;
        Ok(created)
    }

    /// Write the generated `about.md` and point the root at it, in one change
    /// set. Returns the path written.
    ///
    /// Unlike [`link_sidecar`](Self::link_sidecar) — which bootstraps a
    /// whole-file *record store* and leaves it alone thereafter — this rewrites
    /// the file **whole** every time, because the page is a pure function of
    /// configuration and there is nothing in it to preserve. Spec §4 calls this
    /// target kind *generated prose*: no inverse, no `part_of`, no id, not in
    /// the spanning tree, and never merged.
    ///
    /// The pointer is created if absent and left alone if present, so a
    /// workspace that has moved its page keeps it where it put it.
    pub async fn write_about(
        &self,
        root_doc: &Path,
        config: &crate::config::WorkspaceConfig,
        ctx: &crate::about::AboutContext,
    ) -> Result<PathBuf> {
        let page = crate::about::generate(config, self.relations(), ctx)?;
        let path = match self.about_path(root_doc).await? {
            Some(existing) => existing,
            None => PathBuf::from(default_about_name(config.content_format)),
        };

        let mut cs = ChangeSet::new();
        cs.write(&path, page);
        // Point the root at it only when it is not already pointed at, so the
        // root's own bytes are untouched on an ordinary regeneration.
        if self.about_path(root_doc).await?.is_none()
            && let Some(pointer) = self.relations().about_relation()
        {
            let (text, doc) = self.load(root_doc).await?;
            let updated = crate::edit::set_in_text(
                &text,
                doc.carrier,
                pointer,
                crate::edit::infer_scalar(&path.to_string_lossy()),
            )?;
            cs.write(root_doc, updated);
        }
        cs.apply(&self.fs, &self.root).await?;
        Ok(path)
    }

    /// Remove the generated page and the root's pointer to it — the
    /// `about: structure` → `off` transition.
    ///
    /// Deleting is safe here in a way it is not anywhere else in prov: the page
    /// is derived, so nothing user-authored can be lost (spec §4 — "a pure
    /// function of configuration, therefore discardable"). It is *not* routed to
    /// the recycle bin for the same reason; a bin entry would promise a recovery
    /// worth having, and regeneration is always available instead.
    ///
    /// Returns the path removed, or `None` when there was no page to remove.
    pub async fn remove_about(&self, root_doc: &Path) -> Result<Option<PathBuf>> {
        let Some(path) = self.about_path(root_doc).await? else {
            return Ok(None);
        };
        let mut cs = ChangeSet::new();
        if self.fs.try_exists(&self.root.join(&path)).await? {
            cs.remove(&path);
        }
        if let Some(pointer) = self.relations().about_relation() {
            let (text, doc) = self.load(root_doc).await?;
            let updated = crate::edit::unset_in_text(&text, doc.carrier, pointer)?;
            cs.write(root_doc, updated);
        }
        cs.apply(&self.fs, &self.root).await?;
        Ok(Some(path))
    }

    /// The page prov *would* generate, beside what is on disk — the staleness
    /// question, answered without writing anything.
    ///
    /// `Ok(None)` means the page is current. `Ok(Some(diff))` carries the
    /// expected page and what is actually there (`None` when the file is
    /// missing), which is what `check` reports and `prov about --check` prints.
    ///
    /// **The comparison is over the body only.** The metadata block is excluded
    /// deliberately, and that single choice does two jobs: a content-only page
    /// (`embed_style: separate`, where there is no block at all) has nothing
    /// missing from the comparison, and `generated_by: prov <version>` never
    /// makes a workspace stale merely because prov was upgraded. A byline that
    /// names an older version is harmless; a `check` that fires in every
    /// workspace on earth after a release is not.
    pub async fn about_diff(
        &self,
        root_doc: &Path,
        config: &crate::config::WorkspaceConfig,
        ctx: &crate::about::AboutContext,
    ) -> Result<Option<AboutDiff>> {
        let expected = crate::about::generate(config, self.relations(), ctx)?;
        let Some(path) = self.about_path(root_doc).await? else {
            return Ok(Some(AboutDiff {
                path: PathBuf::from(default_about_name(config.content_format)),
                expected,
                actual: None,
            }));
        };
        if !self.fs.try_exists(&self.root.join(&path)).await? {
            return Ok(Some(AboutDiff {
                path,
                expected,
                actual: None,
            }));
        }
        let actual = self.fs.read_to_string(&self.root.join(&path)).await?;
        if crate::about::same_body(&actual, &expected, config.content_format) {
            return Ok(None);
        }
        Ok(Some(AboutDiff {
            path,
            expected,
            actual: Some(actual),
        }))
    }

    /// The [`Finding::AboutStale`] this workspace's generated page warrants, if
    /// any — the `check` view over [`about_diff`](Self::about_diff).
    ///
    /// Silent when the workspace asks for no page (`about: off`) *and* declares
    /// no pointer: nothing was promised, so nothing is broken. A workspace that
    /// still declares a pointer is still checked, because the pointer is a
    /// promise regardless of what the axis now says.
    ///
    /// [`Finding::AboutStale`]: crate::validate::Finding::AboutStale
    pub async fn check_about(
        &self,
        root_doc: &Path,
        config: &crate::config::WorkspaceConfig,
        ctx: &crate::about::AboutContext,
    ) -> Result<Option<crate::validate::Finding>> {
        let declared = self.about_path(root_doc).await?.is_some();
        if !crate::about::enabled(config) && !declared {
            return Ok(None);
        }
        Ok(self.about_diff(root_doc, config, ctx).await?.map(|diff| {
            crate::validate::Finding::AboutStale {
                path: diff.path,
                missing: diff.actual.is_none(),
                expected: diff.expected,
            }
        }))
    }

    // TODO(port): scan/traverse from diaryx_core::workspace land here.
}

/// The default filename for the generated page, in the workspace's content
/// format.
///
/// Load-bearing, and the reason it is a constant rather than a setting the user
/// is asked about: a person opening the directory finds this file *by its name*,
/// with no pointer traversal and no convention beyond being able to read. The
/// pointer may name any path — placement is ergonomic (spec §5) — but the
/// default must be the most guessable name in the most guessable place.
pub fn default_about_name(format: crate::content::ContentFormat) -> String {
    format!("about.{}", format.extension())
}

/// What [`Workspace::about_diff`] found: the page prov would write, and what is
/// there instead.
#[derive(Debug, Clone)]
pub struct AboutDiff {
    /// Where the page lives (or would).
    pub path: PathBuf,
    /// The page prov would generate from the current configuration.
    pub expected: String,
    /// What is on disk, or `None` when the file is missing.
    pub actual: Option<String>,
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
    reference_style: Option<ReferenceStyle>,
    default_embed_format: fig::Format,
    embed_style: EmbedStyle,
    fixity: Fixity,
    history: History,
    id_storage: IdStorage,
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

    /// Author durable structural links by id (Obsidian-style) rather than paths.
    /// A convenience over [`reference_style`](Self::reference_style); effective
    /// only when identity registers on a link.
    pub fn id_links(mut self, id_links: bool) -> Self {
        self.id_links = id_links;
        self
    }

    /// Set how far content checksums are recorded (attachments only by default).
    pub fn fixity(mut self, fixity: Fixity) -> Self {
        self.fixity = fixity;
        self
    }

    /// Set whether this workspace keeps a history store. Off by default; see
    /// [`Workspace::history`] for what the library does with it (it does not gate
    /// capture — that is the caller's call).
    pub fn history(mut self, history: History) -> Self {
        self.history = history;
        self
    }

    /// Set the metadata embedding family — the `(style, format)` half that
    /// resolves to a concrete carrier. Defaults to
    /// [`EmbedStyle::Delimited`], matching the config default.
    pub fn embed_style(mut self, embed_style: EmbedStyle) -> Self {
        self.embed_style = embed_style;
        self
    }

    /// Set where a document's stable id is persisted (DESIGN §5). Under a
    /// frontmatter-stamping mode ([`IdStorage::stamps_frontmatter`]) every
    /// document prov authors carries its own `id`, so identity travels with the
    /// file and the registry becomes a rebuildable cache rather than the sole
    /// authority.
    pub fn id_storage(mut self, id_storage: IdStorage) -> Self {
        self.id_storage = id_storage;
        self
    }

    /// Set the workspace-default reference style — the fallback for relations
    /// without their own override. Supersedes the `link_style`/`id_links`
    /// convenience inputs when set.
    pub fn reference_style(mut self, style: ReferenceStyle) -> Self {
        self.reference_style = Some(style);
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
            reference_style: self.reference_style,
            default_embed_format: self.default_embed_format,
            embed_style: self.embed_style,
            fixity: self.fixity,
            history: self.history,
            id_storage: self.id_storage,
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
            reference_style: self.reference_style,
            default_embed_format: self.default_embed_format,
            embed_style: self.embed_style,
            fixity: self.fixity,
            history: self.history,
            id_storage: self.id_storage,
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
            reference_style: self.reference_style,
            default_embed_format: self.default_embed_format,
            embed_style: self.embed_style,
            fixity: self.fixity,
            history: self.history,
            id_storage: self.id_storage,
            pending_stamps: Vec::new(),
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
}
