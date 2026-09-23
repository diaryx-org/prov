//! Opening a workspace to change it — the setup and the cleanup every writer
//! owes, whichever program it is.
//!
//! A [`Workspace`] built with nothing but a root can read, and can run every
//! mutation that mints no id. A writer that may mint needs three more things,
//! and they are the same three whether the writer is prov's CLI or an editor:
//!
//! - **The id registry, loaded.** The configured identity policy, over the
//!   registry the root declares — or, under `id_storage: frontmatter-only`,
//!   over an index rebuilt from each document's own `id`, since that mode
//!   keeps no registry document. [`Workspace::open_for_writing`].
//! - **A registry to write into, before anything mints.** A workspace that
//!   declares no registry yet gets one, linked from the root in the same
//!   crash-safe write. [`ensure_registry`].
//! - **The ids a mutation minted, landed where the config says they live.**
//!   Stamped into each document's `id` field, written to the registry, or both.
//!   [`Workspace::persist_identity`].
//!
//! These lived in the CLI until an editor needed them. A second writer that
//! restated them would restate them slightly differently, and a workspace one
//! program leaves consistent would be one the other leaves with unstamped ids —
//! so they live here, once.
//!
//! Nothing here reads a clock or draws entropy: the minter's seed is the
//! caller's, as the `updated` instant is.

use std::path::{Path, PathBuf};

use prov_graph::document::{Document, whole_file_extension};
use prov_graph::error::{Error, Result};
use prov_graph::identity::Id;
use prov_graph::meta::{Mapping, Value};
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

use crate::change::ChangeSet;
use crate::discovery::Discovered;
use crate::{FileIndex, Minter, Settings, Workspace, WorkspaceConfig};

/// The stem of the registry document [`ensure_registry`] creates, beside the
/// root: `registry.yaml`, `registry.json`, … in the workspace's metadata format.
///
/// Only a default. Where the registry lives is a fact the workspace declares,
/// through the root's `registry` pointer, and a registry can equally be a `.md`
/// file whose frontmatter carries the records — anything the pointer targets.
pub const REGISTRY_STEM: &str = "registry";

/// Where a workspace is and what it says about itself — the part of a
/// [`Discovered`] that opening one for writing reads.
///
/// Borrowed rather than owned so a caller that keeps these facts in a shape of
/// its own (the CLI's context, an editor's session) can lend them without
/// building a `Discovered` it does not otherwise hold.
#[derive(Debug, Clone, Copy)]
pub struct WorkspaceRoot<'a> {
    /// The workspace root directory.
    pub root_dir: &'a Path,
    /// The root document, relative to `root_dir`.
    pub root_doc: &'a Path,
    /// The registry document the root declares, relative to `root_dir`, if any.
    pub registry: Option<&'a Path>,
    /// The effective workspace configuration.
    pub config: &'a WorkspaceConfig,
}

impl Discovered {
    /// This workspace, as [`Workspace::open_for_writing`] and its companions
    /// read it.
    pub fn as_root(&self) -> WorkspaceRoot<'_> {
        WorkspaceRoot {
            root_dir: &self.root_dir,
            root_doc: &self.root_doc,
            registry: self.registry.as_deref(),
            config: &self.config,
        }
    }
}

/// What [`ensure_registry`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryBootstrap {
    /// The registry the root now declares, relative to the root directory. The
    /// caller's record of the workspace should say so from here on.
    pub path: PathBuf,
    /// Whether the registry document was newly written — `false` when it was
    /// already on disk and only the root's pointer to it was missing.
    pub created: bool,
}

/// Make sure the workspace declares a registry to mint into, bootstrapping one
/// when it does not.
///
/// `None` when there is nothing to do: the workspace already declares one, or
/// `id_storage: frontmatter-only` keeps ids in the documents and no registry
/// at all. *When* to ask is the caller's: an editor about to run a mutation
/// asks when [`WorkspaceConfig::mints_on_mutation`] says one may mint, and a
/// repair that has just minted asks regardless. Otherwise it writes
/// `registry.<ext>` beside the root — titled, in the workspace's metadata
/// format — and points the root's `registry` key at it, as one change set
/// ([`Workspace::link_sidecar`]).
///
/// Call it *before* [`Workspace::open_for_writing`], with the registry it
/// reports folded into the root that call is given: the workspace is built
/// over the registry it will write, and one built over none has nowhere to
/// stage an id a mutation mints.
pub async fn ensure_registry<FS: Storage>(
    fs: FS,
    at: WorkspaceRoot<'_>,
) -> Result<Option<RegistryBootstrap>> {
    let config = at.config;
    if at.registry.is_some() || !config.id_storage.keeps_registry() {
        return Ok(None);
    }
    let format = config.default_embed_format;
    let path = PathBuf::from(format!("{REGISTRY_STEM}.{}", whole_file_extension(format)));
    // Machinery is reached one-way, through the root's `registry` pointer, so
    // the seed carries a title and no `part_of`: a back-link would claim a
    // place in the spanning tree the registry does not have.
    let mut seed = Mapping::new();
    seed.insert("title".into(), Value::String("ID registry".into()));
    let probe: Workspace<FS> = Workspace::builder(fs).root(at.root_dir).build();
    let created = probe
        .link_sidecar(at.root_doc, "registry", &path, &seed, format)
        .await?;
    Ok(Some(RegistryBootstrap { path, created }))
}

impl<FS: Storage + Clone> Workspace<FS, Minter, FileIndex> {
    /// A workspace that can run every mutation, including the ones that mint:
    /// the configured identity policy, over the id index this workspace keeps.
    ///
    /// Every policy knob comes from `at.config`, whole ([`Settings`]). The
    /// index is the registry the root declares, parsed — an empty one in the
    /// workspace's metadata format when none is declared yet, or the declared
    /// file does not exist — except under `id_storage: frontmatter-only`,
    /// which keeps no registry and rebuilds the id → path map from each
    /// document's own `id` field.
    ///
    /// `seed` seeds the minter. Uniqueness is enforced against the index, so
    /// the seed only has to differ between runs; the library draws no entropy
    /// of its own.
    ///
    /// A mutation that may mint wants [`ensure_registry`] first, and every
    /// mutation wants [`persist_identity`](Self::persist_identity) after.
    pub async fn open_for_writing(fs: FS, at: WorkspaceRoot<'_>, seed: u64) -> Result<Self> {
        let config = at.config;
        let index = if config.id_storage.keeps_registry() {
            match at.registry {
                Some(rel) => {
                    let text = match fs.read_to_string(&at.root_dir.join(rel)).await {
                        Ok(text) => text,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                        Err(e) => return Err(e.into()),
                    };
                    FileIndex::parse(rel, &text)?
                }
                None => FileIndex::new(config.default_embed_format),
            }
        } else {
            let probe: Workspace<FS> = Workspace::builder(fs.clone()).root(at.root_dir).build();
            let mut index = FileIndex::new(config.default_embed_format);
            for (id, path) in probe.scan_ids().await? {
                index.register(&id, &path);
            }
            // Read off the disk just now, so it has nothing to write back.
            index.mark_clean();
            index
        };
        Ok(Workspace::builder(fs)
            .root(at.root_dir)
            .settings(Settings::from(config))
            .identity(Minter::with(config.identity, seed))
            .index(index)
            .build())
    }

    /// Land the identity changes a mutation made, where the workspace's
    /// `id_storage` says ids live.
    ///
    /// Under the modes that stamp frontmatter, every live id is written into
    /// its document's `id` field — idempotently, so a document already
    /// carrying its id is untouched, and a workspace that just switched to
    /// frontmatter storage is back-filled. Under the modes that keep a
    /// registry, a registry the mutation dirtied but could not stage itself is
    /// written: ordinarily the mutation stages the registry in its own change
    /// set and this finds nothing to do, but a workspace with no registry
    /// document *yet* — `check --fix` declines to bootstrap one until a fix has
    /// actually minted — leaves the index dirty with nowhere to go. Under
    /// `frontmatter-only`, which keeps no registry, the index is marked clean.
    ///
    /// Each of the two writes is one crash-safe change set.
    pub async fn persist_identity(&mut self, at: WorkspaceRoot<'_>) -> Result<()> {
        if at.config.id_storage.stamps_frontmatter() {
            self.stamp_ids().await?;
        }
        if at.config.id_storage.keeps_registry() {
            self.save_index(at.registry).await
        } else {
            self.index_mut().mark_clean();
            Ok(())
        }
    }

    /// Every live id, stamped into its document's `id` field. A tombstoned id
    /// has no live path and is skipped; so is a document that cannot be read
    /// or parsed, which `check` reports on its own terms.
    async fn stamp_ids(&mut self) -> Result<()> {
        let pairs: Vec<(Id, PathBuf)> = self
            .index()
            .iter()
            .map(|(id, path)| (id.clone(), path.clone()))
            .collect();
        let mut cs = ChangeSet::new();
        for (id, rel) in pairs {
            let Ok(text) = self.read_text(&rel).await else {
                continue;
            };
            let Ok(doc) = Document::parse(&rel, &text) else {
                continue;
            };
            if doc.meta.get("id").and_then(Value::as_str) == Some(id.0.as_str()) {
                continue;
            }
            // Always a string, never an inferred scalar: an id from the NOID
            // alphabet may be all digits, and inferring would stamp an integer
            // (dropping any leading zero) that `as_str` cannot read back.
            let stamped = crate::edit::set_in_text(
                &text,
                doc.carrier,
                "id",
                (&Value::String(id.0.clone())).into(),
            )?;
            cs.write(&rel, stamped);
        }
        if cs.is_empty() {
            return Ok(());
        }
        self.apply_set(&cs).await
    }

    /// Write a registry the index holds changes for and no mutation staged.
    async fn save_index(&mut self, registry: Option<&Path>) -> Result<()> {
        if !self.index().is_dirty() {
            return Ok(());
        }
        let Some(rel) = registry else {
            return Err(Error::Io(std::io::Error::other(
                "the registry changed but no registry document is declared",
            )));
        };
        let host_text = match self.read_text(rel).await {
            Ok(text) => text,
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        self.index_mut().set_host(rel, &host_text)?;
        let Some((path, rendered)) = self.index_mut().pending_write()? else {
            return Ok(());
        };
        let mut cs = ChangeSet::new();
        cs.write(path, rendered);
        self.apply_set(&cs).await?;
        self.index_mut().committed(true);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::IdStorage;
    use crate::identity::{Registration, Trigger};
    use crate::{StdFs, block_on};
    use prov_graph::index::IdIndex;
    use prov_testkit::{read, write};

    fn workspace(tag: &str) -> PathBuf {
        let dir = prov_testkit::scratch("writable", tag);
        write(
            &dir,
            "README.md",
            "---\ntitle: Home\ncontents:\n- note.md\n---\n",
        );
        write(
            &dir,
            "note.md",
            "---\ntitle: Note\npart_of: README.md\n---\n",
        );
        dir
    }

    fn config(id_storage: IdStorage) -> WorkspaceConfig {
        WorkspaceConfig {
            id_storage,
            identity: Registration::LAZY,
            ..WorkspaceConfig::default()
        }
    }

    fn root<'a>(
        dir: &'a Path,
        registry: Option<&'a Path>,
        config: &'a WorkspaceConfig,
    ) -> WorkspaceRoot<'a> {
        WorkspaceRoot {
            root_dir: dir,
            root_doc: Path::new("README.md"),
            registry,
            config,
        }
    }

    #[test]
    fn a_registry_is_bootstrapped_beside_the_root_and_the_root_points_at_it() {
        let dir = workspace("bootstrap");
        let config = config(IdStorage::Frontmatter);

        let boot = block_on(ensure_registry(StdFs, root(&dir, None, &config)))
            .unwrap()
            .expect("a workspace that keeps a registry and declares none gets one");
        assert_eq!(boot.path, Path::new("registry.yaml"));
        assert!(boot.created);
        assert!(read(&dir, "registry.yaml").contains("title: ID registry"));
        assert!(read(&dir, "README.md").contains("registry:"));

        // Declared now, so there is nothing left to do.
        let again = block_on(ensure_registry(
            StdFs,
            root(&dir, Some(&boot.path), &config),
        ))
        .unwrap();
        assert_eq!(again, None);
    }

    #[test]
    fn a_minted_id_lands_in_the_document_and_the_registry_and_is_read_back() {
        let dir = workspace("mint");
        let config = config(IdStorage::Frontmatter);
        let registry = block_on(ensure_registry(StdFs, root(&dir, None, &config)))
            .unwrap()
            .unwrap()
            .path;
        let at = root(&dir, Some(&registry), &config);

        let mut ws = block_on(Workspace::open_for_writing(StdFs, at, 1)).unwrap();
        let id = block_on(ws.register(Path::new("note.md"), Trigger::Link)).unwrap();
        block_on(ws.persist_identity(at)).unwrap();
        assert!(!ws.index().is_dirty(), "everything minted is on disk");

        assert!(
            read(&dir, "note.md").contains(&format!("id: {}", id.0)),
            "{}",
            read(&dir, "note.md")
        );
        assert!(read(&dir, "registry.yaml").contains(&id.0));

        // A second writer, opened over the same disk, knows the id.
        let reopened = block_on(Workspace::open_for_writing(StdFs, at, 2)).unwrap();
        assert_eq!(
            reopened.index().resolve(&id),
            Some(PathBuf::from("note.md"))
        );
    }

    #[test]
    fn frontmatter_only_keeps_no_registry_and_reads_ids_off_the_documents() {
        let dir = workspace("frontmatter-only");
        write(
            &dir,
            "note.md",
            "---\ntitle: Note\npart_of: README.md\nid: k7q2x9\n---\n",
        );
        let config = config(IdStorage::FrontmatterOnly);

        assert_eq!(
            block_on(ensure_registry(StdFs, root(&dir, None, &config))).unwrap(),
            None
        );
        assert!(!dir.join("registry.yaml").exists());

        let mut ws = block_on(Workspace::open_for_writing(
            StdFs,
            root(&dir, None, &config),
            1,
        ))
        .unwrap();
        assert_eq!(
            ws.index().resolve(&Id("k7q2x9".into())),
            Some(PathBuf::from("note.md"))
        );
        assert!(
            !ws.index().is_dirty(),
            "read off the disk, nothing to write"
        );
        block_on(ws.persist_identity(root(&dir, None, &config))).unwrap();
        assert!(!dir.join("registry.yaml").exists());
    }
}
