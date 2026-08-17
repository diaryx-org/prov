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
//! can fail. The consequence worth stating: a history capture parks the
//! *manifest*, not the photographs. Damage to them stays **detectable** — every
//! hash is on record — but not undoable from the history store, which is the
//! price of not duplicating an archive into `history/blobs/`.

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
}

impl ManifestUpdate {
    /// Whether the rebuild found the manifest already correct.
    pub fn is_clean(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
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
        let covered = link::resolve(manifest_doc, root);
        let mut files = Vec::new();
        for rel in self.graph().scan_covered(&covered).await? {
            let hash = if hash {
                let bytes = self
                    .read_bytes(&link::normalize(covered.join(&rel)))
                    .await?;
                Some(crate::fixity::digest(&bytes))
            } else {
                None
            };
            files.push(ManifestEntry { path: rel, hash });
        }
        let mut manifest = Manifest {
            root: root.to_string(),
            files,
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
        let on_disk = self.graph().scan_covered(&root).await?;
        let (missing, extra) = prov_graph::manifest::diff(&manifest.files, &on_disk);
        Ok(Some(ManifestStatus {
            node: node.to_path_buf(),
            manifest: manifest_doc,
            root,
            hashed: manifest.is_hashed(),
            listed: manifest.files.len(),
            missing,
            extra,
        }))
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
        let mut findings = Vec::new();
        for entry in &manifest.files {
            let Some(recorded) = &entry.hash else {
                continue;
            };
            if !crate::fixity::is_recognized(recorded) {
                continue;
            }
            let path = manifest.file_path(&manifest_doc, entry);
            let Ok(bytes) = self.read_bytes(&path).await else {
                continue; // absent: the drift pass's finding, not this one's
            };
            let actual = crate::fixity::digest(&bytes);
            if &actual != recorded {
                findings.push(Finding::ManifestMismatch {
                    node: node.to_path_buf(),
                    manifest: manifest_doc.clone(),
                    path,
                    recorded: recorded.clone(),
                    actual,
                });
            }
        }
        Ok(findings)
    }

    /// What rebuilding `node`'s manifest from the directory would change, and
    /// the writes that would do it — the manifest document, and the node whose
    /// `content_hash` pins it.
    ///
    /// Split out from [`update_manifest`](Workspace::update_manifest) so the
    /// autofix ([`Fix::RegenerateManifest`](crate::remedy::Fix::RegenerateManifest))
    /// stages exactly the same two writes into the change set it is already
    /// building, rather than committing a second one behind its back. The writes
    /// are empty when the manifest is already correct.
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
        // An empty manifest has no mode to preserve, so it takes the
        // workspace's: a directory that was empty when it was covered and has
        // files in it now should record them the way this workspace records
        // everything else.
        let hashed = if current.files.is_empty() {
            self.fixity().covers_payloads()
        } else {
            current.is_hashed()
        };
        let fresh = self
            .build_manifest(&manifest_doc, &current.root, hashed)
            .await?;

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
        if update.is_clean() {
            return Ok((update, Vec::new()));
        }

        let (_, manifest_parsed) = self.load(&manifest_doc).await?;
        let title = manifest_parsed
            .meta
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| link::path_to_title(&manifest_doc));
        let format = self.default_embed_format();
        let new_text = prov_graph::meta::serialize_mapping(&fresh.to_mapping(&title), format)?;

        let mut writes = vec![(manifest_doc, new_text.clone())];
        // The node pins the manifest, so a rewritten manifest is a stale pin
        // until this lands with it — one change set, never two.
        let (node_text, node_doc) = self.load(&node).await?;
        if node_doc.meta.get("content_hash").is_some() || self.fixity().covers_payloads() {
            let restamped = prov_store::edit::set_in_text(
                &node_text,
                node_doc.carrier,
                "content_hash",
                fig::Value::Str(crate::fixity::digest(new_text.as_bytes())),
            )?;
            writes.push((node, restamped));
        }
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
        let hash = self.fixity().covers_payloads();
        self.attach_manifest_titled(dir, parent, None, hash).await
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
        let dir = link::normalize(dir);
        let parent = link::normalize(parent);

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

        let (spanning, inverse) = self.spanning_pair()?;
        let format = self.default_embed_format();
        let node = crate::attach::sidecar_path(&dir, format);
        let manifest_doc = manifest_sibling(&node);
        for path in [&node, &manifest_doc] {
            if self.exists(path).await? {
                return Err(Error::AlreadyExists(path.to_path_buf()));
            }
        }

        let (parent_text, parent_doc) = self.load(&parent).await?;
        let title = title_override
            .map(str::to_owned)
            .unwrap_or_else(|| link::path_to_title(&dir));
        let parent_title = parent_doc
            .meta
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| link::path_to_title(&parent));

        // The manifest's `root`: the covered directory as seen from the manifest
        // document's own directory (they are siblings, so just its name), with a
        // trailing slash — it names a directory, and the file should say so to
        // someone reading it without prov.
        let root = format!(
            "{}/",
            link::relative(manifest_doc.parent().unwrap_or(Path::new("")), &dir)
        );
        let manifest = self.build_manifest(&manifest_doc, &root, hash).await?;
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
        // sidecar pins its payload's. Recorded whenever the workspace covers
        // payloads — and note it is decided by the *workspace*, not by `hash`:
        // pinning the list costs one hash of one small file, and it is what makes
        // the per-row hashes worth anything.
        if self.fixity().covers_payloads() {
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
    /// and a refresh is not the place to silently reverse it.
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
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::*;
    use prov_graph::exec::block_on;
    use prov_graph::fs::StdFs;

    fn write(dir: &Path, rel: &str, bytes: &[u8]) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }

    fn read(dir: &Path, rel: &str) -> String {
        std::fs::read_to_string(dir.join(rel)).unwrap()
    }

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prov-manifest-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ws(dir: &Path) -> Workspace<StdFs> {
        Workspace::builder(StdFs).root(dir).build()
    }

    /// A workspace with an index, a photo directory and nothing else.
    fn photos(tag: &str) -> PathBuf {
        let dir = tempdir(tag);
        write(&dir, "index.md", b"---\ntitle: Home\n---\n");
        write(&dir, "photos/a.jpg", &[0xff, 0xd8, 0x01]);
        write(&dir, "photos/2019/b.jpg", &[0xff, 0xd8, 0x02]);
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
            write(&dir, &format!("photos/{i:04}.jpg"), &[0xff, i as u8]);
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

        write(&dir, "photos/c.jpg", &[0xff, 0xd8, 0x03]); // added
        std::fs::remove_file(dir.join("photos/a.jpg")).unwrap(); // removed
        write(&dir, "photos/2019/b.jpg", &[0xff, 0xd8, 0x99]); // rewritten

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

        write(&dir, "photos/c.jpg", &[0xff, 0xd8, 0x03]);
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
        write(&dir, "photos/a.jpg", &[0xff, 0xd8, 0x77]);

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
        write(&dir, "photos/c.jpg", &[0xff, 0xd8, 0x03]);
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
        // What a history capture parks falls out of this one set, so the promise
        // "the manifest is captured, the photographs are not" is really a
        // statement about reachability — tested where it is decided rather than
        // through the store that consumes it.
        let dir = photos("reachable");
        block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();

        let reachable = block_on(ws(&dir).reachable_files("index.md")).unwrap();
        assert!(reachable.contains(Path::new("photos.manifest.yaml")));
        assert!(reachable.contains(Path::new("photos.yaml")));
        assert!(
            !reachable.contains(Path::new("photos/a.jpg")),
            "an archive must not be duplicated into the history store"
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
        write(&dir, "photos/a.jpg", &[0xff, 0xd8, 0x77]);
        assert_eq!(
            block_on(ws(&dir).verify_manifest(Path::new("photos.yaml"))).unwrap(),
            vec![]
        );
    }
}
