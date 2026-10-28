//! Manifests — one node standing for a directory of files, instead of one
//! sidecar per file.
//!
//! [`attach`](crate::Workspace::attach) gives a single opaque file
//! workspace-linked metadata by minting a sidecar beside it. The trade is
//! excellent for a hundred files and absurd for ten thousand photographs: a
//! directory would double in size, in a way no person can read and no sync
//! transport can carry cheaply.
//!
//! [`attach_manifest`](Workspace::attach_manifest) is the bulk form. One node
//! (`photos.yaml`) and one manifest document (`photos.manifest.yaml`) stand for
//! the whole of `photos/`, listing every opaque file under it and — when the
//! workspace records fixity — the digest of each. The node hashes the *manifest*
//! the way an attachment sidecar hashes its payload, so the chain is:
//!
//! ```text
//! photos.yaml  --content_hash-->  photos.manifest.yaml  --hash per row-->  photos/**
//! ```
//!
//! and tampering with a row means tampering with a file the node has pinned.
//! The record shape is [`prov_graph::manifest`]; this module is the verbs.
//!
//! The verbs:
//! - [`attach_manifest`](Workspace::attach_manifest) — mint the pair and link
//!   the node under a parent (the bulk analogue of `attach`).
//! - [`update_manifest`](Workspace::update_manifest) — rebuild the rows from the
//!   directory as it is now, and re-stamp the node.
//! - [`extend_manifest`](Workspace::extend_manifest) — append rows for named
//!   files, hashing only those and leaving every other row as it was.
//! - [`manifest_status`](Workspace::manifest_status) — what the manifest says and
//!   whether the directory still agrees, reading no covered file.
//! - [`verify_manifest`](Workspace::verify_manifest) — the **deep** check: read
//!   every listed file and compare its digest. `check` deliberately does not do
//!   this; it is the pass you run on purpose, or on a schedule, over an archive.
//!
//! And the two reverse lookups, which are **not** interchangeable:
//! [`manifest_node_for`](Workspace::manifest_node_for) probes the `<dir>.<ext>`
//! convention and is cheap, while
//! [`manifest_node_covering`](Workspace::manifest_node_covering) asks every
//! reachable document and is right. A `rename` moves the node without moving the
//! archive, so the convention stops matching in ordinary use — which is why the
//! verb that would otherwise mint a *second* manifest over one archive pays for
//! the census.
//!
//! ## What a manifest does *not* do
//!
//! It never shadows a document. A manifest covers the opaque payloads under its
//! root; a `.md` note sitting among the photographs is a document like any
//! other — linked, censused, orphan-checked. Deliberate shadowing of a readable
//! file stays a single-file affair (`attach --opaque`), where the promise is
//! made one file at a time and can be seen in the sidecar beside it.
//!
//! Nor do the covered files enter the reachable set. They are opaque bytes: not
//! content documents, so never orphan candidates, and adding ten thousand paths
//! to every walk would make the archive pay on each one for a check none of them
//! can fail. The consequence worth stating: a tool that copies, syncs or
//! records what the workspace reaches takes the *manifest*, not the
//! photographs — [`ignore_list`](Workspace::ignore_list) rules the archive out
//! by naming it *claimed*. Damage to the files stays **detectable**, because
//! every hash is on record, and recovering them is that tool's business rather
//! than prov's. The price of not duplicating an archive is that prov never
//! holds a second copy to restore from.

use std::path::{Path, PathBuf};

use fig::Segment;

use crate::identity::{IdentityPolicy, Trigger};
use crate::validate::Finding;
use crate::workspace::Workspace;
use prov_graph::error::{Error, Result};
use prov_graph::fs::ReadStorage;
use prov_graph::index::IdIndex;
use prov_graph::link;
use prov_graph::manifest::{Manifest, ManifestEntry, manifest_sibling};
use prov_graph::meta::{Mapping, Value};
use prov_store::edit::MetaEditor;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

/// What a [`update_manifest`](Workspace::update_manifest) changed, in the terms
/// a person asked the question in: which files are new to the manifest, which it
/// listed and can no longer find, and which are still there with different bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManifestUpdate {
    /// The manifest document rewritten (or that would have been).
    pub manifest: PathBuf,
    /// Files on disk the old manifest did not list, relative to its root.
    pub added: Vec<PathBuf>,
    /// Files the old manifest listed that are no longer on disk.
    pub removed: Vec<PathBuf>,
    /// Files whose recorded digest disagreed with their current bytes. In an
    /// unhashed manifest this is always empty — there was nothing to disagree.
    pub changed: Vec<PathBuf>,
    /// Whether the rows already agreed with the directory but the manifest's
    /// bytes were not the ones its node pinned — a row's field edited by hand —
    /// so the pin was re-recorded over the manifest as it stands.
    pub repinned: bool,
}

impl ManifestUpdate {
    /// Whether the rebuild found the manifest already correct.
    pub fn is_clean(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.changed.is_empty()
            && !self.repinned
    }
}

/// What a manifest says about its directory, and how the directory currently
/// disagrees — the cheap report, computed without reading a covered file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestStatus {
    /// The node declaring the manifest.
    pub node: PathBuf,
    /// The manifest document itself.
    pub manifest: PathBuf,
    /// The covered directory, workspace-relative.
    pub root: PathBuf,
    /// Whether every row carries a digest — a fixity baseline rather than an
    /// inventory.
    pub hashed: bool,
    /// How many files the manifest lists.
    pub listed: usize,
    /// Rows whose file is not on disk, relative to `root`.
    pub missing: Vec<PathBuf>,
    /// Opaque files under `root` that no row claims, relative to `root`.
    pub extra: Vec<PathBuf>,
    /// Who keeps the files, when the manifest says ([`Held`](prov_graph::manifest::Held)).
    pub held: Option<prov_graph::manifest::Held>,
    /// Whether the files are held elsewhere and not here: the manifest is
    /// held and its directory is absent. Then nothing is missing or extra —
    /// there was nothing here to compare.
    pub elsewhere: bool,
}

impl ManifestStatus {
    /// Whether the directory and the manifest still describe the same file set.
    pub fn agrees(&self) -> bool {
        self.missing.is_empty() && self.extra.is_empty()
    }
}

impl<FS: ReadStorage, Id, Ix: IdIndex> Workspace<FS, Id, Ix> {
    /// The node covering the directory `dir`, or `None` when nothing does — the
    /// bulk counterpart of [`attachment_for`](Workspace::attachment_for).
    pub async fn manifest_node_for(&self, dir: &Path) -> Result<Option<PathBuf>> {
        self.graph().manifest_node_for(dir).await
    }

    /// The manifest `node` declares, loaded and parsed, with its path. `None`
    /// when `node` declares none.
    pub async fn manifest_of(&self, node: &Path) -> Result<Option<(PathBuf, Manifest)>> {
        self.graph().manifest_of(node).await
    }

    /// Build the manifest the directory under `manifest_doc`'s `root` would
    /// produce right now: every opaque file under it, sorted, hashed when
    /// `hash`.
    ///
    /// Hashing **re-reads every file**, deliberately. The device-local fixity
    /// cache could answer from a stat, and must not: a remembered digest may
    /// decide what to do, never establish a fixity baseline, and these rows are
    /// exactly that (DESIGN §, the fixity-cache rule). Bit-rot is by
    /// construction the change a stat cannot see.
    pub async fn build_manifest(
        &self,
        manifest_doc: &Path,
        root: &str,
        hash: bool,
    ) -> Result<Manifest> {
        self.build_manifest_claiming(manifest_doc, root, hash, false)
            .await
    }

    /// [`build_manifest`](Self::build_manifest) for a manifest that may be
    /// [`verbatim`](Manifest::verbatim): when `verbatim`, the documents under
    /// `root` are listed and hashed as well, and the manifest says so.
    pub async fn build_manifest_claiming(
        &self,
        manifest_doc: &Path,
        root: &str,
        hash: bool,
        verbatim: bool,
    ) -> Result<Manifest> {
        let claim = Manifest {
            root: root.to_string(),
            verbatim,
            ..Default::default()
        };
        self.build_manifest_like(manifest_doc, &claim, hash).await
    }

    /// [`build_manifest`](Self::build_manifest) for what `claim` claims: its
    /// `root`, whether it is [`verbatim`](Manifest::verbatim), and its
    /// [`exclude`](Manifest::exclude) patterns, which the result carries too.
    /// `claim`'s own rows are not consulted.
    pub async fn build_manifest_like(
        &self,
        manifest_doc: &Path,
        claim: &Manifest,
        hash: bool,
    ) -> Result<Manifest> {
        let covered = link::resolve(manifest_doc, &claim.root);
        let paths = self
            .graph()
            .scan_excluding(&covered, claim.verbatim, &claim.exclude)
            .await?;
        let files = if hash {
            let mut digests = crate::fixity::Digests::new();
            for rel in &paths {
                self.push_file_digest(&mut digests, &link::normalize(covered.join(rel)))
                    .await?;
            }
            paths
                .into_iter()
                .zip(digests.finish())
                .map(|(path, hash)| ManifestEntry {
                    path,
                    hash: Some(hash),
                    ..Default::default()
                })
                .collect()
        } else {
            paths
                .into_iter()
                .map(|path| ManifestEntry {
                    path,
                    ..Default::default()
                })
                .collect()
        };
        let mut manifest = Manifest {
            root: claim.root.clone(),
            files,
            verbatim: claim.verbatim,
            exclude: claim.exclude.clone(),
            held: claim.held.clone(),
        };
        manifest.sort();
        Ok(manifest)
    }

    /// The node covering `dir`, found by the convention *or*, failing that, by
    /// asking every reachable document what it covers.
    ///
    /// [`manifest_node_for`](Self::manifest_node_for) probes `<dir>.<ext>` and
    /// is the fast path; this is the authoritative one, and the two are not
    /// interchangeable. A `rename` moves the node without moving the covered
    /// directory (deliberately — see `mutate::rename`), so the conventional name
    /// stops matching as a matter of *ordinary use*, not of hand-editing. If the
    /// verbs trusted the probe, `attach --manifest` on an already-covered
    /// directory would mint a second manifest over it, and two records would
    /// claim one archive with no rule about which is right.
    ///
    /// That is what makes it worth a whole census here, where the attachment
    /// lookup settles for the probe: a payload's sidecar keeps the convention
    /// across a rename (the payload travels with it), and a manifest's node
    /// cannot.
    pub async fn manifest_node_covering(&self, dir: &Path) -> Result<Option<PathBuf>> {
        // The walk loads every reachable document, and the loop below asks each
        // of them for its manifest — which loads it again. One scope makes the
        // census a whole workspace pays for once, not twice.
        let _scope = self.read_scope();
        let dir = link::normalize(dir);
        if let Some(node) = self.manifest_node_for(&dir).await? {
            return Ok(Some(node));
        }
        let Some(root) = self.root_document().await? else {
            return Ok(None);
        };
        let walk = self.walk(&root).await?;
        let reachable = self
            .reachable_documents(&root, &walk.census, &walk.content_bodies)
            .await?;
        for doc in reachable {
            if let Ok(Some((manifest_doc, manifest))) = self.manifest_of(&doc).await
                && manifest.covered_root(&manifest_doc) == dir
            {
                return Ok(Some(doc));
            }
        }
        Ok(None)
    }

    /// What the manifest `node` declares says, and whether the directory still
    /// agrees with it — the cheap report, reading no covered file.
    ///
    /// `None` when `node` declares no manifest. The same completeness question
    /// `check`'s drift pass asks, answered through the same
    /// [`diff`](prov_graph::manifest::diff), so a host and a `check` can never
    /// disagree about what is in the directory.
    pub async fn manifest_status(&self, node: &Path) -> Result<Option<ManifestStatus>> {
        let Some((manifest_doc, manifest)) = self.manifest_of(node).await? else {
            return Ok(None);
        };
        let root = manifest.checked_root(&manifest_doc)?;
        let elsewhere = manifest.held.is_some() && !self.is_dir_here(&root).await;
        let (missing, extra) = if elsewhere {
            (Vec::new(), Vec::new())
        } else {
            let on_disk = self.graph().scan_manifest(&manifest_doc, &manifest).await?;
            prov_graph::manifest::diff(&manifest.files, &on_disk)
        };
        Ok(Some(ManifestStatus {
            node: node.to_path_buf(),
            manifest: manifest_doc,
            root,
            hashed: manifest.is_hashed(),
            listed: manifest.files.len(),
            missing,
            extra,
            held: manifest.held,
            elsewhere,
        }))
    }

    /// Whether `dir` is a directory on this device — following a link, as the
    /// scan does.
    pub(crate) async fn is_dir_here(&self, dir: &Path) -> bool {
        self.graph()
            .stat(dir)
            .await
            .map(|m| m.is_dir())
            .unwrap_or(false)
    }

    /// Refuse to rebuild or extend a held manifest's rows from a directory
    /// that is not here: the scan would find nothing, and an update would empty
    /// the record of an archive this device simply does not carry.
    async fn refuse_held_elsewhere(&self, manifest_doc: &Path, manifest: &Manifest) -> Result<()> {
        let Some(held) = &manifest.held else {
            return Ok(());
        };
        let root = manifest.checked_root(manifest_doc)?;
        if self.is_dir_here(&root).await {
            return Ok(());
        }
        Err(Error::Structure(format!(
            "{}: its files are held by {} and are not on this device; refresh it where they are",
            manifest_doc.display(),
            held.by
        )))
    }

    /// **Deep** verification of the manifest `node` declares: read every listed
    /// file and compare its bytes against the recorded digest, one
    /// [`Finding::ManifestMismatch`] per file that disagrees.
    ///
    /// This is the pass `check` leaves out. `check` verifies what is cheap —
    /// that the node's hash still covers the manifest, and that the rows and the
    /// directory still name the same files — because a `check` that re-reads an
    /// archive is a `check` people stop running. Corruption *inside* a file that
    /// is still present and still listed is what this answers, and it costs one
    /// full read of the archive.
    ///
    /// Rows with no digest are skipped: an unhashed manifest is an inventory,
    /// and it promised nothing about bytes. A missing file is reported by
    /// `check`'s drift pass, not here, so the two never double-report.
    pub async fn verify_manifest(&self, node: &Path) -> Result<Vec<Finding>> {
        let Some((manifest_doc, manifest)) = self.manifest_of(node).await? else {
            return Ok(Vec::new());
        };
        let mut read = Vec::new();
        let mut digests = crate::fixity::Digests::new();
        for entry in &manifest.files {
            let Some(recorded) = &entry.hash else {
                continue;
            };
            if !crate::fixity::is_recognized(recorded) {
                continue;
            }
            let path = manifest.file_path(&manifest_doc, entry);
            if self.push_file_digest(&mut digests, &path).await.is_err() {
                continue; // absent: the drift pass's finding, not this one's
            }
            read.push((path, recorded));
        }
        Ok(read
            .into_iter()
            .zip(digests.finish())
            .filter(|((_, recorded), actual)| actual != *recorded)
            .map(|((path, recorded), actual)| Finding::ManifestMismatch {
                node: node.to_path_buf(),
                manifest: manifest_doc.clone(),
                path,
                recorded: recorded.clone(),
                actual,
            })
            .collect())
    }

    /// What rebuilding `node`'s manifest from the directory would change, and
    /// the writes that would do it — the manifest document, and the node whose
    /// `content_hash` pins it.
    ///
    /// Split out from [`update_manifest`](Workspace::update_manifest) so the
    /// autofix ([`Fix::RegenerateManifest`](crate::remedy::Fix::RegenerateManifest))
    /// stages exactly the same two writes into the change set it is already
    /// building, rather than committing a second one behind its back. The writes
    /// are empty when the manifest is already correct, and the node alone when
    /// only its pin is behind.
    pub(crate) async fn plan_manifest_rebuild(
        &self,
        node: &Path,
    ) -> Result<(ManifestUpdate, Vec<(PathBuf, String)>)> {
        let node = link::normalize(node);
        let Some((manifest_doc, current)) = self.manifest_of(&node).await? else {
            return Err(Error::Structure(format!(
                "{} declares no manifest",
                node.display()
            )));
        };
        self.refuse_held_elsewhere(&manifest_doc, &current).await?;
        // An empty manifest has no mode to preserve, so it takes the
        // workspace's: a directory that was empty when it was covered and has
        // files in it now should record them the way this workspace records
        // everything else.
        let hashed = if current.files.is_empty() {
            self.fixity().is_on()
        } else {
            current.is_hashed()
        };
        // Whether it claims documents too, and what it leaves out, are
        // decisions made at attach or written since, and kept, as the hashing
        // mode is.
        let mut fresh = self
            .build_manifest_like(&manifest_doc, &current, hashed)
            .await?;
        // What a row says about its file is the one thing the directory cannot
        // say again: it stays on the row while the file does, bytes changed or
        // not, and goes with the row when the file has gone.
        let mut said: std::collections::BTreeMap<&Path, &Mapping> = current
            .files
            .iter()
            .map(|e| (e.path.as_path(), &e.fields))
            .collect();
        for entry in &mut fresh.files {
            if let Some(fields) = said.remove(entry.path.as_path()) {
                entry.fields = fields.clone();
            }
        }

        let old: std::collections::BTreeMap<&Path, Option<&String>> = current
            .files
            .iter()
            .map(|e| (e.path.as_path(), e.hash.as_ref()))
            .collect();
        let new: std::collections::BTreeMap<&Path, Option<&String>> = fresh
            .files
            .iter()
            .map(|e| (e.path.as_path(), e.hash.as_ref()))
            .collect();
        let mut update = ManifestUpdate {
            manifest: manifest_doc.clone(),
            ..Default::default()
        };
        for (path, hash) in &new {
            match old.get(path) {
                None => update.added.push(path.to_path_buf()),
                Some(before) if before != hash => update.changed.push(path.to_path_buf()),
                Some(_) => {}
            }
        }
        for path in old.keys() {
            if !new.contains_key(path) {
                update.removed.push(path.to_path_buf());
            }
        }
        let (node_text, node_doc) = self.load(&node).await?;
        if update.is_clean() {
            // The rows stand, so the manifest is not rewritten — a person's
            // edits to it are kept as they laid them out. Its node is
            // re-pinned when those edits left the pin behind, since saying so
            // is what asking for an update over an edited manifest means.
            let bytes = self.read_bytes(&manifest_doc).await?;
            let pinned = node_doc.meta.get("content_hash").and_then(Value::as_str);
            let actual = crate::fixity::digest(&bytes);
            if pinned.is_none_or(|pin| pin == actual) {
                return Ok((update, Vec::new()));
            }
            update.repinned = true;
            let restamped = prov_store::edit::set_in_text(
                &node_text,
                node_doc.carrier,
                "content_hash",
                fig::Value::Str(actual),
            )?;
            return Ok((update, vec![(node, restamped)]));
        }

        let writes = self.manifest_rewrite(&node, &manifest_doc, &fresh).await?;
        Ok((update, writes))
    }

    /// The writes that replace `node`'s manifest document with `rows`: the
    /// manifest under its own title, and the node re-pinned over the new bytes.
    async fn manifest_rewrite(
        &self,
        node: &Path,
        manifest_doc: &Path,
        rows: &Manifest,
    ) -> Result<Vec<(PathBuf, String)>> {
        let (_, manifest_parsed) = self.load(manifest_doc).await?;
        let title = manifest_parsed
            .meta
            .get("title")
            .and_then(prov_graph::title::title_text)
            .unwrap_or_else(|| link::path_to_title(manifest_doc));
        let format = self.default_embed_format();
        let new_text = prov_graph::meta::serialize_mapping(&rows.to_mapping(&title), format)?;

        let (node_text, node_doc) = self.load(node).await?;
        let mut writes = vec![(manifest_doc.to_path_buf(), new_text.clone())];
        // The node pins the manifest, so a rewritten manifest is a stale pin
        // until this lands with it — one change set, never two.
        if node_doc.meta.get("content_hash").is_some() || self.fixity().is_on() {
            let restamped = prov_store::edit::set_in_text(
                &node_text,
                node_doc.carrier,
                "content_hash",
                fig::Value::Str(crate::fixity::digest(new_text.as_bytes())),
            )?;
            writes.push((node.to_path_buf(), restamped));
        }
        Ok(writes)
    }

    /// What appending rows for `paths` to `node`'s manifest would record, and
    /// the writes that would do it — the read half of
    /// [`extend_manifest`](Workspace::extend_manifest).
    ///
    /// Each path is workspace-relative and must name a file the directory scan
    /// would list: under the covered root, opaque, not hidden, not inside a
    /// nested manifest's directory, and not already a row. Anything else is
    /// refused before a byte is read, and the whole call with it — a partial
    /// add would leave the caller to work out which of its files were taken.
    async fn plan_manifest_extend(
        &self,
        node: &Path,
        paths: &[PathBuf],
    ) -> Result<(ManifestUpdate, Vec<(PathBuf, String)>)> {
        let node = link::normalize(node);
        let Some((manifest_doc, mut current)) = self.manifest_of(&node).await? else {
            return Err(Error::Structure(format!(
                "{} declares no manifest",
                node.display()
            )));
        };
        let mut update = ManifestUpdate {
            manifest: manifest_doc.clone(),
            ..Default::default()
        };
        if paths.is_empty() {
            return Ok((update, Vec::new()));
        }
        self.refuse_held_elsewhere(&manifest_doc, &current).await?;
        let root = current.checked_root(&manifest_doc)?;
        let mut listed: std::collections::BTreeSet<PathBuf> =
            current.files.iter().map(|e| e.path.clone()).collect();
        let mut added = Vec::new();
        for path in paths {
            let path = link::normalize(path);
            let refuse = |why: &str| {
                Err(Error::Structure(format!(
                    "cannot add {} to {}: {why}",
                    path.display(),
                    manifest_doc.display()
                )))
            };
            let Some(rel) = path
                .strip_prefix(&root)
                .ok()
                .filter(|r| !r.as_os_str().is_empty() && !link::escapes_root(&path))
            else {
                return refuse(&format!("it is not under {}", root.display()));
            };
            let rel = rel.to_path_buf();
            if !current.claims(&rel) {
                return refuse(
                    "it is a document, which a manifest covers only when it is verbatim",
                );
            }
            if rel.iter().any(|c| c.to_string_lossy().starts_with('.')) {
                return refuse("a hidden file is never listed");
            }
            if current.excludes_file(&rel) {
                return refuse("the manifest excludes it");
            }
            if !self
                .graph()
                .stat(&path)
                .await
                .map(|m| m.is_file())
                .unwrap_or(false)
            {
                return refuse("no such file");
            }
            // A nested manifest owns its subtree, so a row here would claim a
            // file the scan never lists — reported missing while present.
            let mut dir = path.parent();
            while let Some(d) = dir.filter(|d| *d != root && d.starts_with(&root)) {
                if self.manifest_node_for(d).await?.is_some() {
                    return refuse(&format!("{} has a manifest of its own", d.display()));
                }
                dir = d.parent();
            }
            if !listed.insert(rel.clone()) {
                return refuse("it is already listed");
            }
            added.push((path, rel));
        }

        // The mode is the manifest's, as on a rebuild: an empty one takes the
        // workspace's, a hashed one hashes the new rows — and only those.
        let hashed = if current.files.is_empty() {
            self.fixity().is_on()
        } else {
            current.is_hashed()
        };
        let digests: Vec<Option<String>> = if hashed {
            let mut digests = crate::fixity::Digests::new();
            for (path, _) in &added {
                self.push_file_digest(&mut digests, path).await?;
            }
            digests.finish().into_iter().map(Some).collect()
        } else {
            vec![None; added.len()]
        };
        for ((_, rel), hash) in added.into_iter().zip(digests) {
            update.added.push(rel.clone());
            current.files.push(ManifestEntry {
                path: rel,
                hash,
                ..Default::default()
            });
        }
        current.sort();
        update
            .added
            .sort_by_key(|p| prov_graph::manifest::path_sort_key(p));

        let writes = self
            .manifest_rewrite(&node, &manifest_doc, &current)
            .await?;
        Ok((update, writes))
    }
}

impl<FS: Storage, IdP: IdentityPolicy, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Cover the directory `dir` with a manifest node linked under `parent`:
    /// mint `dir.yaml` (the node, carrying `title`, the inverse link up, a
    /// `manifest` pointer and — under a fixity setting that covers payloads —
    /// a `content_hash` of the manifest's bytes) and `dir.manifest.yaml` (the
    /// record store), and add the node to `parent`'s spanning field.
    ///
    /// Refuses a directory that is already covered, and refuses to overwrite
    /// either file. Files under `dir` are *not* moved, rewritten or read as
    /// documents — only, when hashing, read as bytes.
    pub async fn attach_manifest(&mut self, dir: &Path, parent: &Path) -> Result<PathBuf> {
        let hash = self.fixity().is_on();
        self.attach_manifest_titled(dir, parent, None, hash).await
    }

    /// Every refusal [`attach_manifest`](Self::attach_manifest) makes of the
    /// directory itself, made without writing: `dir` must be a directory, not
    /// already covered, and neither the node nor the manifest beside it may
    /// exist yet. For a caller that writes something first, as
    /// [`check_attach`](Self::check_attach) is.
    pub async fn check_attach_manifest(&self, dir: &Path) -> Result<()> {
        let dir = link::normalize(dir);
        if !self
            .graph()
            .stat(&dir)
            .await
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            return Err(Error::Structure(format!(
                "{} is not a directory — a manifest covers a directory of files; \
                 use `attach` for a single one",
                dir.display()
            )));
        }
        if let Some(existing) = self.manifest_node_covering(&dir).await? {
            return Err(Error::Structure(format!(
                "{} is already covered by the manifest node {}",
                dir.display(),
                existing.display()
            )));
        }

        let node = crate::attach::sidecar_path(&dir, self.default_embed_format());
        let manifest_doc = manifest_sibling(&node);
        for path in [&node, &manifest_doc] {
            if self.exists(path).await? {
                return Err(Error::AlreadyExists(path.to_path_buf()));
            }
        }
        Ok(())
    }

    /// [`attach_manifest`](Self::attach_manifest) with an explicit title and an
    /// explicit choice about hashing.
    ///
    /// `hash` is worth its own parameter rather than following the fixity axis
    /// alone: hashing is a per-directory cost (one full read of the archive, now
    /// and at every refresh), and a directory of a hundred thousand scans is a
    /// place someone may reasonably want the inventory without the baseline
    /// while still wanting checksums everywhere else.
    pub async fn attach_manifest_titled(
        &mut self,
        dir: &Path,
        parent: &Path,
        title_override: Option<&str>,
        hash: bool,
    ) -> Result<PathBuf> {
        self.attach_manifest_claiming(dir, parent, title_override, hash, false)
            .await
    }

    /// [`attach_manifest_titled`](Self::attach_manifest_titled), choosing
    /// whether the manifest is [`verbatim`](Manifest::verbatim): whether it
    /// claims the documents under `dir` too, so the directory is carried byte
    /// for byte — a built web app with its `index.html` — rather than its
    /// documents staying documents of the workspace.
    pub async fn attach_manifest_claiming(
        &mut self,
        dir: &Path,
        parent: &Path,
        title_override: Option<&str>,
        hash: bool,
        verbatim: bool,
    ) -> Result<PathBuf> {
        self.attach_manifest_excluding(dir, parent, title_override, hash, verbatim, &[])
            .await
    }

    /// [`attach_manifest_claiming`](Self::attach_manifest_claiming), leaving
    /// out of the manifest what the `exclude` patterns name and writing the
    /// patterns into it, so every later refresh and `check` leaves the same
    /// files out. For a directory something else also keeps files in — a
    /// NAS's thumbnail folders beside the photographs, a recycle bin — whose
    /// files are neither the archive nor drift. See
    /// [`Manifest::excludes`] for the syntax; a pattern it does not read is
    /// refused before anything is written.
    pub async fn attach_manifest_excluding(
        &mut self,
        dir: &Path,
        parent: &Path,
        title_override: Option<&str>,
        hash: bool,
        verbatim: bool,
        exclude: &[String],
    ) -> Result<PathBuf> {
        for pattern in exclude {
            prov_graph::manifest::check_pattern(pattern)?;
        }
        let dir = link::normalize(dir);
        let parent = link::normalize(parent);
        self.check_attach_manifest(&dir).await?;

        let (spanning, inverse) = self.spanning_pair()?;
        let format = self.default_embed_format();
        let node = crate::attach::sidecar_path(&dir, format);
        let manifest_doc = manifest_sibling(&node);

        let (parent_text, parent_doc) = self.load(&parent).await?;
        let title = title_override
            .map(str::to_owned)
            .unwrap_or_else(|| link::path_to_title(&dir));
        let parent_title = parent_doc
            .meta
            .get("title")
            .and_then(prov_graph::title::title_text)
            .unwrap_or_else(|| link::path_to_title(&parent));

        // The manifest's `root`: the covered directory as seen from the manifest
        // document's own directory (they are siblings, so just its name), with a
        // trailing slash — it names a directory, and the file should say so to
        // someone reading it without prov.
        let root = format!(
            "{}/",
            link::relative(manifest_doc.parent().unwrap_or(Path::new("")), &dir)
        );
        let claim = Manifest {
            root,
            verbatim,
            exclude: exclude.to_vec(),
            ..Default::default()
        };
        let manifest = self
            .build_manifest_like(&manifest_doc, &claim, hash)
            .await?;
        let manifest_text = prov_graph::meta::serialize_mapping(
            &manifest.to_mapping(&format!("{title} — manifest")),
            format,
        )?;

        // Opens before the first id-authoring call below, so the index
        // checkpoint covers the registrations those make (see `mutate::create`).
        let mut cs = self.change();

        let up = self
            .authored_target(&inverse, &node, &parent, &parent_title, true)
            .await?;
        let down = self
            .authored_target(&spanning, &parent, &node, &title, false)
            .await?;

        let mut map = Mapping::new();
        map.insert("title".into(), Value::String(title));
        map.insert(inverse.clone(), Value::String(up));
        map.insert(
            prov_graph::manifest::MANIFEST_KEY.into(),
            Value::String(
                manifest_doc
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string(),
            ),
        );
        // Fixity: the node pins the manifest's bytes, exactly as an attachment
        // sidecar pins its payload's. Recorded whenever the workspace records
        // checksums at all — and note it is decided by the *workspace*, not by
        // `hash`: pinning the list costs one hash of one small file, and it is
        // what makes the per-row hashes worth anything.
        if self.fixity().is_on() {
            map.insert(
                "content_hash".into(),
                Value::String(crate::fixity::digest(manifest_text.as_bytes())),
            );
        }
        let node_text = prov_graph::meta::serialize_mapping(&map, format)?;

        let mut parent_editor = MetaEditor::open_or_init(&parent_text, parent_doc.carrier)?;
        let span_path = [Segment::Key(&spanning)];
        if parent_editor
            .append_value(&span_path, fig::Value::Str(down.clone()))
            .is_err()
        {
            parent_editor.set_value(&span_path, fig::Value::Seq(vec![fig::Value::Str(down)]))?;
        }
        let parent_out = parent_editor.render()?;

        cs.write(&manifest_doc, manifest_text);
        cs.write(&node, node_text);
        cs.write(&parent, parent_out);

        if self.identity().registration().fires_on(Trigger::Create)
            && self.index().id_for_path(&node).is_none()
        {
            let id = self.mint_unique(&node);
            self.index_mut().register(&id, &node);
        }
        self.commit(cs).await?;
        Ok(node)
    }

    /// Rebuild the manifest `node` declares from the directory as it is now, and
    /// re-stamp the node's `content_hash` over the result. Returns what changed.
    ///
    /// The refresh verb: a directory of photographs gains and loses files
    /// without prov being told, and this is how the record catches up. It
    /// **preserves the manifest's hashing mode** — an inventory stays an
    /// inventory, a hashed manifest is re-hashed in full — because whether to
    /// carry a fixity baseline over this directory is a decision already made,
    /// and a refresh is not the place to silently reverse it. A row's own
    /// fields stay on it while its file is there; when they are all that
    /// changed, the manifest is left as written and only the node's pin is
    /// re-recorded ([`ManifestUpdate::repinned`]).
    ///
    /// Nothing is written when nothing changed, down to the byte: a manifest is
    /// a file some transport carries, and rewriting an identical one is a
    /// conflict waiting to happen for no gain.
    pub async fn update_manifest(&mut self, node: &Path) -> Result<ManifestUpdate> {
        let (update, writes) = self.plan_manifest_rebuild(node).await?;
        if writes.is_empty() {
            return Ok(update);
        }
        let mut cs = self.change();
        for (path, text) in writes {
            cs.write(&path, text);
        }
        self.commit(cs).await?;
        Ok(update)
    }

    /// Say who keeps `node`'s files — or, with `None`, that every device
    /// carrying the manifest has them — and re-pin the node over the result,
    /// in one change set. Answers whether anything was written: setting what
    /// is already set writes nothing.
    ///
    /// The rows are kept as they are. Marking a manifest held is what makes it
    /// safe to carry to a device without its directory, so it is done where
    /// the files are, before the manifest travels.
    pub async fn set_manifest_held(
        &mut self,
        node: &Path,
        held: Option<prov_graph::manifest::Held>,
    ) -> Result<bool> {
        let node = link::normalize(node);
        let Some((manifest_doc, mut manifest)) = self.manifest_of(&node).await? else {
            return Err(Error::Structure(format!(
                "{} declares no manifest",
                node.display()
            )));
        };
        if manifest.held == held {
            return Ok(false);
        }
        manifest.held = held;
        let writes = self
            .manifest_rewrite(&node, &manifest_doc, &manifest)
            .await?;
        let mut cs = self.change();
        for (path, text) in writes {
            cs.write(&path, text);
        }
        self.commit(cs).await?;
        Ok(true)
    }

    /// Append rows for `paths` — workspace-relative files under `node`'s
    /// covered root — to its manifest, and re-stamp the node's `content_hash`
    /// over the result in the same change set. Returns the rows added, in
    /// [`ManifestUpdate::added`].
    ///
    /// The add verb, for a host that has just put files into a covered
    /// directory and knows which. Unlike
    /// [`update_manifest`](Self::update_manifest) it does not look at the rest
    /// of the directory: every existing row stays as it was, its file present
    /// or not — a file missing here may be gone, or not yet synced to this
    /// device, and an add is no place to decide which — and only the named
    /// files are read, when the manifest is hashed. Adding two photographs to
    /// an archive of two thousand reads two.
    ///
    /// Refuses, writing nothing, a path outside the root, a readable document,
    /// a file the directory scan would never list, one that does not exist, or
    /// one already listed. Writes nothing when `paths` is empty.
    pub async fn extend_manifest(
        &mut self,
        node: &Path,
        paths: &[PathBuf],
    ) -> Result<ManifestUpdate> {
        let (update, writes) = self.plan_manifest_extend(node, paths).await?;
        if writes.is_empty() {
            return Ok(update);
        }
        let mut cs = self.change();
        for (path, text) in writes {
            cs.write(&path, text);
        }
        self.commit(cs).await?;
        Ok(update)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prov_graph::exec::block_on;
    use prov_graph::fs::StdFs;

    use prov_testkit::{read, write};
    fn tempdir(tag: &str) -> PathBuf {
        prov_testkit::scratch("manifest", tag)
    }

    fn ws(dir: &Path) -> Workspace<StdFs> {
        Workspace::builder(StdFs).root(dir).build()
    }

    /// A workspace with an index, a photo directory and nothing else.
    fn photos(tag: &str) -> PathBuf {
        let dir = tempdir(tag);
        write(&dir, "index.md", b"---\ntitle: Home\n---\n");
        write(&dir, "photos/a.jpg", [0xff, 0xd8, 0x01]);
        write(&dir, "photos/2019/b.jpg", [0xff, 0xd8, 0x02]);
        dir
    }

    #[test]
    fn one_node_and_one_manifest_stand_for_a_whole_directory() {
        let dir = photos("basic");
        let node =
            block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();
        assert_eq!(node, PathBuf::from("photos.yaml"));

        let node_text = read(&dir, "photos.yaml");
        assert!(
            node_text.contains("manifest: photos.manifest.yaml"),
            "{node_text}"
        );
        assert!(
            !node_text.contains("content:"),
            "a manifest node has no payload: {node_text}"
        );

        // Every opaque file under the directory, recursively, relative to the
        // root — and its digest, since the default fixity covers payloads.
        let manifest = read(&dir, "photos.manifest.yaml");
        assert!(manifest.contains("root: photos/"), "{manifest}");
        assert!(manifest.contains("path: a.jpg"), "{manifest}");
        assert!(manifest.contains("path: 2019/b.jpg"), "{manifest}");
        assert!(
            manifest.contains(&crate::fixity::digest(&[0xff, 0xd8, 0x02])),
            "{manifest}"
        );

        // The node pins the manifest's bytes — the chain the design rests on.
        assert!(
            node_text.contains(&crate::fixity::digest(manifest.as_bytes())),
            "{node_text}"
        );

        // The parent links the node, and the whole thing validates.
        assert!(read(&dir, "index.md").contains("photos.yaml"));
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn ten_thousand_photos_cost_two_files() {
        // The whole point, stated as a test: the sidecar-per-file design would
        // have written 500 documents here.
        let dir = tempdir("scale");
        write(&dir, "index.md", b"---\ntitle: Home\n---\n");
        for i in 0..500 {
            write(&dir, &format!("photos/{i:04}.jpg"), [0xff, i as u8]);
        }
        block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();

        let created = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().unwrap().is_file())
            .count();
        assert_eq!(created, 3, "index.md + the node + the manifest");
        assert_eq!(
            block_on(ws(&dir).manifest_of(Path::new("photos.yaml")))
                .unwrap()
                .unwrap()
                .1
                .files
                .len(),
            500
        );
    }

    #[test]
    fn a_document_among_the_photos_is_not_claimed() {
        // A manifest covers bytes; it never shadows a document. The note stays a
        // document — which is why it is then an ordinary orphan, exactly as it
        // would be in any other unreached directory prov has been pointed at.
        let dir = photos("readable");
        write(&dir, "photos/note.md", b"---\ntitle: Note\n---\nhi\n");
        block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();

        let manifest = read(&dir, "photos.manifest.yaml");
        assert!(!manifest.contains("note.md"), "{manifest}");
    }

    /// A built web app: its `index.html` is part of the build, not a page.
    fn app(tag: &str) -> PathBuf {
        let dir = tempdir(tag);
        write(&dir, "index.md", b"---\ntitle: Home\n---\n");
        write(
            &dir,
            "app/index.html",
            b"<!doctype html><title>Player</title><script src=\"/app/index-1.js\"></script>\n",
        );
        write(&dir, "app/index-1.js", b"console.log(1)\n");
        write(
            &dir,
            "app/assets/help.html",
            b"<!doctype html><title>Help</title>\n",
        );
        dir
    }

    fn attach_verbatim(dir: &Path) -> PathBuf {
        block_on(ws(dir).attach_manifest_claiming(
            Path::new("app"),
            Path::new("index.md"),
            None,
            true,
            true,
        ))
        .unwrap()
    }

    #[test]
    fn a_verbatim_manifest_claims_the_documents_too() {
        let dir = app("verbatim");
        let node = attach_verbatim(&dir);

        let manifest = read(&dir, "app.manifest.yaml");
        assert!(manifest.contains("verbatim: true"), "{manifest}");
        for path in ["index.html", "index-1.js", "assets/help.html"] {
            assert!(
                manifest.contains(&format!("path: {path}")),
                "{path}: {manifest}"
            );
        }
        let status = block_on(ws(&dir).manifest_status(&node)).unwrap().unwrap();
        assert!(status.agrees(), "{status:?}");
        assert_eq!(status.listed, 3);
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);

        // The same directory, covered without the flag, leaves its pages out.
        let plain = app("verbatim-plain");
        block_on(ws(&plain).attach_manifest(Path::new("app"), Path::new("index.md"))).unwrap();
        let manifest = read(&plain, "app.manifest.yaml");
        assert!(!manifest.contains("verbatim"), "{manifest}");
        assert!(!manifest.contains("index.html"), "{manifest}");
    }

    #[test]
    fn a_verbatim_manifest_stays_verbatim_and_reports_a_new_page_as_drift() {
        let dir = app("verbatim-update");
        let node = attach_verbatim(&dir);
        write(
            &dir,
            "app/about.html",
            b"<!doctype html><title>About</title>\n",
        );

        let status = block_on(ws(&dir).manifest_status(&node)).unwrap().unwrap();
        assert_eq!(status.extra, vec![PathBuf::from("about.html")]);

        let update = block_on(ws(&dir).update_manifest(&node)).unwrap();
        assert_eq!(update.added, vec![PathBuf::from("about.html")]);
        let manifest = read(&dir, "app.manifest.yaml");
        assert!(manifest.contains("verbatim: true"), "{manifest}");
        assert!(manifest.contains("path: about.html"), "{manifest}");
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn a_page_can_be_added_to_a_verbatim_manifest_only() {
        let dir = app("verbatim-add");
        let node = attach_verbatim(&dir);
        write(&dir, "app/about.html", b"<!doctype html>\n");
        let update =
            block_on(ws(&dir).extend_manifest(&node, &[PathBuf::from("app/about.html")])).unwrap();
        assert_eq!(update.added, vec![PathBuf::from("about.html")]);

        let plain = app("verbatim-add-plain");
        let node =
            block_on(ws(&plain).attach_manifest(Path::new("app"), Path::new("index.md"))).unwrap();
        let err = block_on(ws(&plain).extend_manifest(&node, &[PathBuf::from("app/index.html")]))
            .unwrap_err();
        assert!(err.to_string().contains("verbatim"), "{err}");
    }

    #[test]
    fn a_link_into_a_verbatim_directory_leaves_its_pages_out_of_the_workspace() {
        // Linking a file of the app puts its directory in reach. Its pages are
        // still the app's: no orphan, and no title of theirs answers an alias.
        let dir = app("verbatim-linked");
        attach_verbatim(&dir);
        let index = read(&dir, "index.md");
        std::fs::write(
            dir.join("index.md"),
            format!("{index}\n[script](app/index-1.js)\n"),
        )
        .unwrap();
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);

        let titles = block_on(ws(&dir).title_index_scoped(Path::new("index.md"))).unwrap();
        assert_eq!(
            titles.resolve("index"),
            prov_graph::title::TitleMatch::Unique(PathBuf::from("index.md"))
        );
    }

    #[test]
    fn a_verbatim_manifest_is_not_scattered() {
        let dir = app("verbatim-scatter");
        let node = attach_verbatim(&dir);
        let err = block_on(ws(&dir).plan_scatter(&node, None)).unwrap_err();
        assert!(err.to_string().contains("verbatim"), "{err}");
    }

    #[test]
    fn refusing_a_second_manifest_over_the_same_directory() {
        let dir = photos("twice");
        block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();
        let err = block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md")))
            .unwrap_err();
        assert!(err.to_string().contains("already covered"), "{err}");
    }

    #[test]
    fn a_single_file_is_not_a_manifest_subject() {
        let dir = photos("notadir");
        let err =
            block_on(ws(&dir).attach_manifest(Path::new("photos/a.jpg"), Path::new("index.md")))
                .unwrap_err();
        assert!(err.to_string().contains("not a directory"), "{err}");
    }

    #[test]
    fn update_catches_additions_removals_and_edits_and_restamps_the_node() {
        let dir = photos("update");
        block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();

        write(&dir, "photos/c.jpg", [0xff, 0xd8, 0x03]); // added
        std::fs::remove_file(dir.join("photos/a.jpg")).unwrap(); // removed
        write(&dir, "photos/2019/b.jpg", [0xff, 0xd8, 0x99]); // rewritten

        let mut w = ws(&dir);
        let update = block_on(w.update_manifest(Path::new("photos.yaml"))).unwrap();
        assert_eq!(update.added, vec![PathBuf::from("c.jpg")]);
        assert_eq!(update.removed, vec![PathBuf::from("a.jpg")]);
        assert_eq!(update.changed, vec![PathBuf::from("2019/b.jpg")]);

        // The node's pin followed the rewrite in the same change set, so the
        // workspace is clean immediately afterwards rather than one `check` behind.
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn an_unchanged_directory_rewrites_nothing() {
        let dir = photos("noop");
        block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();
        let before = read(&dir, "photos.manifest.yaml");

        let mut w = ws(&dir);
        let update = block_on(w.update_manifest(Path::new("photos.yaml"))).unwrap();
        assert!(update.is_clean());
        assert_eq!(read(&dir, "photos.manifest.yaml"), before);
    }

    #[test]
    fn a_refresh_keeps_what_a_row_says_while_its_file_is_there() {
        let dir = photos("row-fields");
        block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();
        let text = read(&dir, "photos.manifest.yaml")
            .replace("- path: a.jpg\n", "- path: a.jpg\n  caption: At the lake\n")
            .replace(
                "- path: 2019/b.jpg\n",
                "- path: 2019/b.jpg\n  caption: The dog\n",
            );
        write(&dir, "photos.manifest.yaml", text.clone());

        // Edited by hand, the rows still agree with the directory: the
        // manifest stands as written and the node's pin catches up.
        assert!(!block_on(ws(&dir).check("index.md")).unwrap().is_empty());
        let update = block_on(ws(&dir).update_manifest(Path::new("photos.yaml"))).unwrap();
        assert!(update.repinned && !update.is_clean(), "{update:?}");
        assert_eq!(read(&dir, "photos.manifest.yaml"), text);
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);

        write(&dir, "photos/a.jpg", [0xff, 0xd8, 0x99]); // rewritten
        std::fs::remove_file(dir.join("photos/2019/b.jpg")).unwrap(); // gone
        write(&dir, "photos/c.jpg", [0xff, 0xd8, 0x03]); // added
        block_on(ws(&dir).update_manifest(Path::new("photos.yaml"))).unwrap();

        let manifest = read(&dir, "photos.manifest.yaml");
        assert!(manifest.contains("caption: At the lake"), "{manifest}");
        assert!(
            !manifest.contains("The dog"),
            "went with its row: {manifest}"
        );
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn extend_appends_named_files_and_keeps_every_other_row() {
        let dir = photos("extend");
        block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();
        let text = read(&dir, "photos.manifest.yaml")
            .replace("- path: a.jpg\n", "- path: a.jpg\n  caption: At the lake\n");
        write(&dir, "photos.manifest.yaml", text);

        // A listed file goes missing (not yet synced here), another is
        // rewritten, an unnamed one appears: an add touches none of them.
        std::fs::remove_file(dir.join("photos/a.jpg")).unwrap();
        write(&dir, "photos/2019/b.jpg", [0xff, 0xd8, 0x99]);
        write(&dir, "photos/unnamed.jpg", [0xff, 0xd8, 0x05]);
        write(&dir, "photos/c.jpg", [0xff, 0xd8, 0x03]);
        write(&dir, "photos/2020/d.jpg", [0xff, 0xd8, 0x04]);

        let update = block_on(ws(&dir).extend_manifest(
            Path::new("photos.yaml"),
            &[
                PathBuf::from("photos/c.jpg"),
                PathBuf::from("photos/2020/d.jpg"),
            ],
        ))
        .unwrap();
        assert_eq!(
            update.added,
            vec![PathBuf::from("2020/d.jpg"), PathBuf::from("c.jpg")]
        );
        assert!(update.removed.is_empty() && update.changed.is_empty());

        let (_, manifest) = block_on(ws(&dir).manifest_of(Path::new("photos.yaml")))
            .unwrap()
            .unwrap();
        let row = |p: &str| {
            manifest
                .files
                .iter()
                .find(|e| e.path == Path::new(p))
                .unwrap_or_else(|| panic!("no row for {p}: {manifest:?}"))
        };
        assert_eq!(
            row("a.jpg").hash.as_deref(),
            Some(crate::fixity::digest(&[0xff, 0xd8, 0x01]).as_str()),
            "a missing file keeps its row"
        );
        assert!(row("a.jpg").fields.contains_key("caption"));
        assert_eq!(
            row("2019/b.jpg").hash.as_deref(),
            Some(crate::fixity::digest(&[0xff, 0xd8, 0x02]).as_str()),
            "an existing row is not re-hashed"
        );
        assert_eq!(
            row("c.jpg").hash.as_deref(),
            Some(crate::fixity::digest(&[0xff, 0xd8, 0x03]).as_str())
        );
        assert!(row("2020/d.jpg").hash.is_some());
        assert_eq!(manifest.files.len(), 4, "the unnamed file is not added");

        // The node was re-pinned in the same change set: no fixity finding,
        // only the drift the add deliberately left alone.
        let status = block_on(ws(&dir).manifest_status(Path::new("photos.yaml")))
            .unwrap()
            .unwrap();
        assert_eq!(status.missing, vec![PathBuf::from("a.jpg")]);
        assert_eq!(status.extra, vec![PathBuf::from("unnamed.jpg")]);
        let findings = block_on(ws(&dir).check("index.md")).unwrap();
        assert!(
            !findings
                .iter()
                .any(|f| matches!(f, Finding::FixityMismatch { .. })),
            "{findings:?}"
        );
    }

    #[test]
    fn extend_refuses_what_a_row_cannot_honestly_say() {
        let dir = photos("extend-refuse");
        write(&dir, "elsewhere.jpg", [0xff]);
        write(&dir, "photos/note.md", b"---\ntitle: Note\n---\n");
        write(&dir, "photos/.hidden.jpg", [0xff]);
        block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();
        write(&dir, "photos/new.jpg", [0xff]);
        let before = read(&dir, "photos.manifest.yaml");

        for (path, why) in [
            ("elsewhere.jpg", "not under"),
            ("photos/../elsewhere.jpg", "not under"),
            ("photos/note.md", "document"),
            ("photos/.hidden.jpg", "hidden"),
            ("photos/absent.jpg", "no such file"),
            ("photos/a.jpg", "already listed"),
        ] {
            let err = block_on(ws(&dir).extend_manifest(
                Path::new("photos.yaml"),
                &[PathBuf::from("photos/new.jpg"), PathBuf::from(path)],
            ))
            .unwrap_err();
            assert!(err.to_string().contains(why), "{path}: {err}");
        }
        // Naming one file twice is the same refusal.
        let err = block_on(ws(&dir).extend_manifest(
            Path::new("photos.yaml"),
            &[
                PathBuf::from("photos/new.jpg"),
                PathBuf::from("photos/new.jpg"),
            ],
        ))
        .unwrap_err();
        assert!(err.to_string().contains("already listed"), "{err}");
        assert_eq!(
            read(&dir, "photos.manifest.yaml"),
            before,
            "a refused add writes nothing, the good paths in it included"
        );
    }

    #[test]
    fn extend_with_nothing_writes_nothing() {
        let dir = photos("extend-empty");
        block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();
        // Laid out by hand, so a rewrite would show.
        let text = format!("{}\n", read(&dir, "photos.manifest.yaml"));
        write(&dir, "photos.manifest.yaml", text.clone());
        let node = read(&dir, "photos.yaml");

        let update = block_on(ws(&dir).extend_manifest(Path::new("photos.yaml"), &[])).unwrap();
        assert!(update.is_clean());
        assert_eq!(update.manifest, PathBuf::from("photos.manifest.yaml"));
        assert_eq!(read(&dir, "photos.manifest.yaml"), text);
        assert_eq!(read(&dir, "photos.yaml"), node);
    }

    #[test]
    fn extending_an_inventory_records_no_checksum() {
        let dir = photos("extend-unhashed");
        block_on(ws(&dir).attach_manifest_titled(
            Path::new("photos"),
            Path::new("index.md"),
            None,
            false,
        ))
        .unwrap();
        write(&dir, "photos/c.jpg", [0xff, 0xd8, 0x03]);
        block_on(
            ws(&dir).extend_manifest(Path::new("photos.yaml"), &[PathBuf::from("photos/c.jpg")]),
        )
        .unwrap();
        let manifest = read(&dir, "photos.manifest.yaml");
        assert!(manifest.contains("path: c.jpg"), "{manifest}");
        assert!(!manifest.contains("hash:"), "{manifest}");
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn an_inventory_stays_an_inventory_across_a_refresh() {
        let dir = photos("unhashed");
        block_on(ws(&dir).attach_manifest_titled(
            Path::new("photos"),
            Path::new("index.md"),
            None,
            false,
        ))
        .unwrap();
        assert!(!read(&dir, "photos.manifest.yaml").contains("hash:"));

        write(&dir, "photos/c.jpg", [0xff, 0xd8, 0x03]);
        let mut w = ws(&dir);
        block_on(w.update_manifest(Path::new("photos.yaml"))).unwrap();
        assert!(
            !read(&dir, "photos.manifest.yaml").contains("hash:"),
            "a refresh must not silently start recording a baseline"
        );
    }

    #[test]
    fn deep_verification_finds_a_rotted_photo_that_check_cannot() {
        let dir = photos("deep");
        block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();

        // A bit rots: the file is still there, still listed, and its bytes have
        // changed. Nothing shallow can see this.
        write(&dir, "photos/a.jpg", [0xff, 0xd8, 0x77]);

        assert_eq!(
            block_on(ws(&dir).check("index.md")).unwrap(),
            vec![],
            "the cheap pass sees a present, listed file"
        );
        let deep = block_on(ws(&dir).verify_manifest(Path::new("photos.yaml"))).unwrap();
        assert!(
            matches!(&deep[..], [Finding::ManifestMismatch { path, .. }]
                if path == Path::new("photos/a.jpg")),
            "{deep:?}"
        );
    }

    #[test]
    fn check_reports_drift_in_both_directions_and_the_fix_accepts_it() {
        let dir = photos("drift");
        block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();

        // A photo appears and a photo vanishes — neither is a document, so no
        // other pass in prov can see either one.
        write(&dir, "photos/c.jpg", [0xff, 0xd8, 0x03]);
        std::fs::remove_file(dir.join("photos/2019/b.jpg")).unwrap();

        let mut w = ws(&dir);
        let findings = block_on(w.check("index.md")).unwrap();
        let drift = findings
            .iter()
            .find_map(|f| match f {
                Finding::ManifestDrift { missing, extra, .. } => {
                    Some((missing.clone(), extra.clone()))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected drift, got {findings:?}"));
        assert_eq!(drift.0, vec![PathBuf::from("2019/b.jpg")]);
        assert_eq!(drift.1, vec![PathBuf::from("c.jpg")]);

        // The repair is offered but never taken unattended: accepting the
        // directory writes a loss into the record as though it were intended.
        let finding = findings
            .iter()
            .find(|f| matches!(f, Finding::ManifestDrift { .. }))
            .unwrap();
        let remedies = block_on(w.remedies(finding)).unwrap();
        assert_eq!(remedies[0].warrant, crate::remedy::Warrant::Judgment);

        block_on(w.apply_fix(&remedies[0].fix)).unwrap();
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
        assert!(read(&dir, "photos.manifest.yaml").contains("path: c.jpg"));
    }

    #[test]
    fn tampering_with_the_manifest_breaks_the_nodes_pin() {
        // The chain the whole design rests on: the node hashes the manifest, so
        // editing a row — the cheap way to make a corrupted archive look intact
        // — is a fixity mismatch on the node.
        let dir = photos("pin");
        block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();

        let manifest = read(&dir, "photos.manifest.yaml");
        std::fs::write(
            dir.join("photos.manifest.yaml"),
            manifest.replace("sha256:", "sha256:0"),
        )
        .unwrap();

        // And it is reported as covering *the manifest's* bytes — not as an
        // empty body, which a whole-file node would hash to and which would
        // "detect" tampering by mismatching against an intact manifest too.
        let tampered = read(&dir, "photos.manifest.yaml");
        let findings = block_on(ws(&dir).check("index.md")).unwrap();
        assert!(
            findings.iter().any(|f| matches!(
                f,
                Finding::FixityMismatch { doc, actual, .. }
                    if doc == Path::new("photos.yaml")
                        && actual == &crate::fixity::digest(tampered.as_bytes())
            )),
            "{findings:?}"
        );
    }

    #[test]
    fn a_manifest_that_will_not_parse_is_its_own_finding() {
        let dir = photos("malformed");
        block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();
        // A row loses its `path` — the file parses as a document and fails as a
        // record store, which is a different repair and a different risk.
        write(
            &dir,
            "photos.manifest.yaml",
            b"title: M\nroot: photos/\nfiles:\n- hash: sha256:x\n",
        );

        let findings = block_on(ws(&dir).check("index.md")).unwrap();
        assert!(
            findings
                .iter()
                .any(|f| matches!(f, Finding::ManifestMalformed { doc, .. }
                    if doc == Path::new("photos.manifest.yaml"))),
            "{findings:?}"
        );
        // And it does not also report every photo as unlisted: with no
        // trustworthy row set there is nothing to compare a listing against.
        assert!(
            !findings
                .iter()
                .any(|f| matches!(f, Finding::ManifestDrift { .. })),
            "{findings:?}"
        );
    }

    #[test]
    fn a_node_may_not_be_a_payloads_sidecar_and_a_directorys_at_once() {
        let dir = photos("conflict");
        write(
            &dir,
            "index.md",
            b"---\ntitle: Home\ncontents:\n- both.yaml\n---\n",
        );
        write(
            &dir,
            "both.yaml",
            b"title: Both\npart_of: index.md\ncontent: photos/a.jpg\nmanifest: both.manifest.yaml\n",
        );
        write(
            &dir,
            "both.manifest.yaml",
            b"title: M\nroot: photos/\nfiles:\n",
        );

        let findings = block_on(ws(&dir).check("index.md")).unwrap();
        assert!(
            findings.iter().any(
                |f| matches!(f, Finding::ManifestConflict { doc } if doc == Path::new("both.yaml"))
            ),
            "{findings:?}"
        );
    }

    #[test]
    fn a_covered_directory_is_invisible_to_the_loose_sweeps() {
        // The failure this prevents is the loud one: `attach --all --recursive`
        // over an archive would mint one sidecar per photograph — exactly what
        // the manifest was reached for.
        let dir = photos("loose");
        write(&dir, "loose.pdf", b"%PDF-1.7\n");
        block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();

        assert_eq!(
            block_on(ws(&dir).loose_attachments()).unwrap(),
            vec![PathBuf::from("loose.pdf")],
            "the recursive sweep walks into no covered directory"
        );
        assert_eq!(
            block_on(ws(&dir).loose_attachments_in(Path::new("index.md"))).unwrap(),
            vec![PathBuf::from("loose.pdf")]
        );

        // And a covered file named outright is refused rather than gaining a
        // second, rival record of its bytes.
        let err = block_on(ws(&dir).attach(Path::new("photos/a.jpg"), Path::new("index.md")))
            .unwrap_err();
        assert!(err.to_string().contains("already covered"), "{err}");
    }

    #[test]
    fn renaming_the_node_moves_its_manifest_and_leaves_the_archive_alone() {
        let dir = photos("rename");
        block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();

        block_on(ws(&dir).rename(Path::new("photos.yaml"), Path::new("albums/trip.yaml"))).unwrap();

        // The record store travelled; the ten thousand photographs did not.
        assert!(dir.join("albums/trip.manifest.yaml").exists());
        assert!(!dir.join("photos.manifest.yaml").exists());
        assert!(
            dir.join("photos/a.jpg").exists(),
            "a rename of the description must not move the archive"
        );

        let node = read(&dir, "albums/trip.yaml");
        assert!(node.contains("manifest: trip.manifest.yaml"), "{node}");
        let manifest = read(&dir, "albums/trip.manifest.yaml");
        assert!(
            manifest.contains("root: ../photos/"),
            "root re-spelled from where the manifest now sits: {manifest}"
        );

        // And the node's pin followed the manifest's new bytes, so prov's own
        // maintenance did not leave a fixity alarm behind it.
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn a_renamed_node_still_covers_its_directory() {
        // The hazard the convention alone cannot see. After a rename nothing
        // beside `photos/` names the node, so a probe reads the directory as
        // uncovered — and minting a second manifest would leave two records
        // claiming one archive with no rule about which is right.
        let dir = photos("renamed-cover");
        block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();
        block_on(ws(&dir).rename(Path::new("photos.yaml"), Path::new("albums/trip.yaml"))).unwrap();

        assert_eq!(
            block_on(ws(&dir).manifest_node_for(Path::new("photos"))).unwrap(),
            None,
            "the probe cannot see it — which is why the verbs do not use it"
        );
        assert_eq!(
            block_on(ws(&dir).manifest_node_covering(Path::new("photos"))).unwrap(),
            Some(PathBuf::from("albums/trip.yaml"))
        );
        let err = block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md")))
            .unwrap_err();
        assert!(err.to_string().contains("already covered"), "{err}");
    }

    #[test]
    fn deleting_the_node_deletes_the_manifest_and_keeps_the_photographs() {
        let dir = photos("delete");
        block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();

        block_on(ws(&dir).delete(Path::new("photos.yaml"), false)).unwrap();

        assert!(!dir.join("photos.yaml").exists());
        assert!(!dir.join("photos.manifest.yaml").exists());
        assert!(
            dir.join("photos/a.jpg").exists() && dir.join("photos/2019/b.jpg").exists(),
            "deleting a description is not deleting the archive"
        );
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn the_reachable_set_holds_the_manifest_and_not_the_archive() {
        // What a copying tool takes falls out of this one set, so the promise
        // "the manifest travels, the photographs do not" is really a statement
        // about reachability — tested where it is decided rather than through
        // the ignore list that renders it.
        let dir = photos("reachable");
        block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();

        let reachable = block_on(ws(&dir).reachable_files("index.md")).unwrap();
        assert!(reachable.contains(Path::new("photos.manifest.yaml")));
        assert!(reachable.contains(Path::new("photos.yaml")));
        assert!(
            !reachable.contains(Path::new("photos/a.jpg")),
            "an archive must not enter the reachable set"
        );
    }

    #[test]
    fn a_body_link_reaches_a_covered_file_without_reading_it() {
        // Captured shell output: a `.txt` payload whose bytes hold an HTML data
        // island that looks like a metadata block, `content_hash` and all.
        // Linking it from a body must not make it a document, or `check` reads
        // that hash as the payload's own and calls the payload corrupt.
        let dir = photos("body-link");
        write(
            &dir,
            "photos/out.txt",
            b"$ cat page.html\n\n<script type=\"application/yaml\">\ntitle: X\ncontent_hash: sha256:00\n</script>\n",
        );
        block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();
        assert!(read(&dir, "photos.manifest.yaml").contains("path: out.txt"));

        let index = read(&dir, "index.md");
        write(
            &dir,
            "index.md",
            format!("{index}\n[out](/photos/out.txt) and [gone](/photos/gone.txt)\n").as_bytes(),
        );
        let findings = block_on(ws(&dir).check("index.md")).unwrap();
        // The covered file is reached and not read; a link to one that is not
        // there is still broken.
        assert!(
            matches!(
                findings.as_slice(),
                [Finding::BrokenLink { target, .. }] if target == "/photos/gone.txt"
            ),
            "{findings:?}"
        );
    }

    #[test]
    fn a_manifest_node_has_no_copy() {
        let dir = photos("duplicate");
        block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();
        let err = block_on(ws(&dir).duplicate(Path::new("photos.yaml"))).unwrap_err();
        assert!(err.to_string().contains("no copy"), "{err}");
    }

    #[test]
    fn an_unhashed_manifest_promises_nothing_about_bytes() {
        let dir = photos("deep-unhashed");
        block_on(ws(&dir).attach_manifest_titled(
            Path::new("photos"),
            Path::new("index.md"),
            None,
            false,
        ))
        .unwrap();
        write(&dir, "photos/a.jpg", [0xff, 0xd8, 0x77]);
        assert_eq!(
            block_on(ws(&dir).verify_manifest(Path::new("photos.yaml"))).unwrap(),
            vec![]
        );
    }

    /// The patterns a Synology share needs: its thumbnail folders, its
    /// extended-attribute streams, and its recycle bin.
    fn synology() -> Vec<String> {
        ["@eaDir/", "*@SynoEAStream", "/#recycle/"]
            .map(String::from)
            .to_vec()
    }

    /// `photos("…")` with what a Synology keeps beside the photographs.
    fn nas_share(tag: &str) -> PathBuf {
        let dir = photos(tag);
        write(&dir, "photos/@eaDir/a.jpg/SYNOPHOTO_THUMB_XL.jpg", [1]);
        write(&dir, "photos/2019/@eaDir/b.jpg/SYNOPHOTO_THUMB_M.jpg", [2]);
        write(&dir, "photos/2019/b.jpg@SynoEAStream", [3]);
        write(&dir, "photos/#recycle/gone.jpg", [4]);
        dir
    }

    #[test]
    fn a_held_manifest_is_elsewhere_where_its_directory_is_not() {
        use prov_graph::manifest::Held;
        let dir = photos("held");
        let mut w = ws(&dir);
        let node = block_on(w.attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();
        let nas = Held {
            by: "Harris NAS".into(),
            at: Some("https://nas.example/".into()),
        };
        assert!(block_on(w.set_manifest_held(&node, Some(nas.clone()))).unwrap());
        assert!(!block_on(w.set_manifest_held(&node, Some(nas.clone()))).unwrap());
        let manifest = read(&dir, "photos.manifest.yaml");
        assert!(manifest.contains("by: Harris NAS"), "{manifest}");
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);

        // On the keeper, a held manifest is checked as any other.
        write(&dir, "photos/stray.jpg", [0xff, 0xd8, 0x09]);
        assert!(
            !block_on(w.manifest_status(&node))
                .unwrap()
                .unwrap()
                .agrees()
        );
        std::fs::remove_file(dir.join("photos/stray.jpg")).unwrap();

        // Carried to a device without the photographs: elsewhere, not missing.
        std::fs::rename(dir.join("photos"), dir.join("away")).unwrap();
        let status = block_on(w.manifest_status(&node)).unwrap().unwrap();
        assert!(status.elsewhere && status.agrees(), "{status:?}");
        assert_eq!(status.held, Some(nas));
        assert_eq!(status.listed, 2);
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);

        // And nothing here may rebuild the rows from a directory it lacks.
        let before = read(&dir, "photos.manifest.yaml");
        let refused = block_on(w.update_manifest(&node)).unwrap_err().to_string();
        assert!(refused.contains("held by Harris NAS"), "{refused}");
        assert!(block_on(w.extend_manifest(&node, &[PathBuf::from("photos/c.jpg")])).is_err());
        assert_eq!(read(&dir, "photos.manifest.yaml"), before);

        // Unheld, the same absence is a broken root again.
        assert!(block_on(w.set_manifest_held(&node, None)).unwrap());
        assert!(!block_on(w.check("index.md")).unwrap().is_empty());
    }

    #[test]
    fn excluded_files_are_neither_listed_nor_drift() {
        let dir = nas_share("exclude");
        let mut w = ws(&dir);
        let node = block_on(w.attach_manifest_excluding(
            Path::new("photos"),
            Path::new("index.md"),
            None,
            true,
            false,
            &synology(),
        ))
        .unwrap();

        let manifest = read(&dir, "photos.manifest.yaml");
        assert!(manifest.contains("exclude:"), "{manifest}");
        let status = block_on(w.manifest_status(&node)).unwrap().unwrap();
        assert_eq!(status.listed, 2, "{manifest}");
        assert!(status.agrees(), "{status:?}");
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);

        // Synology makes a thumbnail for a new photograph: still not drift.
        write(&dir, "photos/2019/@eaDir/c.jpg/SYNOPHOTO_THUMB_M.jpg", [5]);
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);

        // A refresh keeps the patterns, and what they leave out stays out.
        write(&dir, "photos/2019/c.jpg", [0xff, 0xd8, 0x03]);
        let update = block_on(w.update_manifest(&node)).unwrap();
        assert_eq!(update.added, vec![PathBuf::from("2019/c.jpg")]);
        let manifest = read(&dir, "photos.manifest.yaml");
        assert!(manifest.contains("'@eaDir/'"), "{manifest}");
        assert!(!manifest.contains("THUMB"), "{manifest}");

        // An add of an excluded file is refused, not listed.
        let refused = block_on(w.extend_manifest(
            &node,
            &[PathBuf::from("photos/@eaDir/a.jpg/SYNOPHOTO_THUMB_XL.jpg")],
        ))
        .unwrap_err();
        assert!(refused.to_string().contains("excludes it"), "{refused}");
    }

    #[test]
    fn a_pattern_the_manifest_cannot_read_is_refused_before_anything_is_written() {
        let dir = photos("exclude-refused");
        for pattern in ["!keep.jpg", "[ab].jpg", "../outside", "/"] {
            let refused = block_on(ws(&dir).attach_manifest_excluding(
                Path::new("photos"),
                Path::new("index.md"),
                None,
                true,
                false,
                &[pattern.to_string()],
            ))
            .unwrap_err();
            assert!(
                refused.to_string().contains(pattern),
                "{pattern}: {refused}"
            );
        }
        assert!(!dir.join("photos.yaml").exists());
        assert!(!dir.join("photos.manifest.yaml").exists());
    }

    #[test]
    fn a_large_file_hashes_to_the_digest_of_its_bytes() {
        // The streaming path, taken on purpose: the threshold only decides
        // which of two equal answers is computed.
        let dir = photos("stream");
        let bytes: Vec<u8> = (0..3 * crate::fixity::STREAM_CHUNK as u32 / 2)
            .map(|i| (i % 253) as u8)
            .collect();
        write(&dir, "photos/video.mov", &bytes);
        let w = ws(&dir);
        assert_eq!(
            block_on(w.graph().digest_file(Path::new("photos/video.mov"))).unwrap(),
            crate::fixity::digest(&bytes)
        );
        assert_eq!(
            block_on(w.file_digest(Path::new("photos/video.mov"))).unwrap(),
            crate::fixity::digest(&bytes)
        );
    }

    #[cfg(unix)]
    mod linked {
        use super::*;
        use std::os::unix::fs::symlink;

        /// A workspace whose `photos` is a link to an archive kept outside it,
        /// as a NAS share is linked into a workspace kept on another.
        fn linked_share(tag: &str) -> (PathBuf, PathBuf) {
            let dir = tempdir(tag);
            write(&dir, "ws/index.md", b"---\ntitle: Home\n---\n");
            write(&dir, "share/a.jpg", [0xff, 0xd8, 0x01]);
            write(&dir, "share/2019/b.jpg", [0xff, 0xd8, 0x02]);
            symlink(dir.join("share"), dir.join("ws/photos")).unwrap();
            (dir.join("ws"), dir.join("share"))
        }

        #[test]
        fn a_linked_root_is_covered_checked_and_ruled_out_as_claimed() {
            let (ws_dir, _) = linked_share("linked-root");
            let mut w = ws(&ws_dir);
            let node =
                block_on(w.attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();
            let status = block_on(w.manifest_status(&node)).unwrap().unwrap();
            assert_eq!(status.listed, 2);
            assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);
            assert_eq!(block_on(w.verify_manifest(&node)).unwrap(), vec![]);

            // A tool recording the folder meets a link, and must be told to
            // leave it — as the link it is, so gitignore can match it.
            let list = block_on(w.ignore_list(Path::new("index.md"))).unwrap();
            assert!(
                list.render().lines().any(|line| line == "/photos"),
                "{}",
                list.render()
            );
        }

        #[test]
        fn a_link_inside_a_covered_directory_is_followed_and_a_dangling_one_skipped() {
            let (ws_dir, share) = linked_share("linked-inside");
            write(&share, "../elsewhere/c.jpg", [0xff, 0xd8, 0x03]);
            symlink("../../elsewhere", share.join("2019/more")).unwrap();
            symlink("../a.jpg", share.join("2019/same.jpg")).unwrap();
            symlink("nowhere.jpg", share.join("gone.jpg")).unwrap();

            let mut w = ws(&ws_dir);
            let node =
                block_on(w.attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();
            let (_, manifest) = block_on(w.manifest_of(&node)).unwrap().unwrap();
            let paths: Vec<_> = manifest.files.iter().map(|e| e.path.clone()).collect();
            assert_eq!(
                paths,
                ["2019/b.jpg", "2019/more/c.jpg", "2019/same.jpg", "a.jpg"].map(PathBuf::from)
            );
            let same = &manifest.files[2];
            assert_eq!(
                same.hash.as_deref(),
                Some(crate::fixity::digest(&[0xff, 0xd8, 0x01]).as_str())
            );
            assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);
        }

        #[test]
        fn a_link_that_leads_back_to_itself_is_refused() {
            let (ws_dir, share) = linked_share("linked-loop");
            symlink("..", share.join("2019/again")).unwrap();
            let refused =
                block_on(ws(&ws_dir).attach_manifest(Path::new("photos"), Path::new("index.md")))
                    .unwrap_err();
            assert!(refused.to_string().contains("leads back"), "{refused}");
        }
    }
}
